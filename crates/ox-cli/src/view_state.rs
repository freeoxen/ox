//! ViewState — per-frame snapshot of all state needed for rendering.
//!
//! `fetch_view_state` reads from the broker (UiStore, InboxStore) and
//! borrows from App to produce a `ViewState` that draw functions consume.
//! This decouples rendering from mutable App access and broker writes.

use ox_broker::ClientHandle;
use ox_types::{ApprovalRequest, ScreenSnapshot, UiSnapshot};
use structfs_core_store::{Value, path};

use crate::app::App;
use crate::types::{ChatMessage, CustomizeState};

pub use crate::parse::InboxThread;
use crate::parse::{parse_chat_messages, parse_inbox_threads};

// ---------------------------------------------------------------------------
// ViewState — per-frame rendering snapshot
// ---------------------------------------------------------------------------

/// Per-frame snapshot of all state the draw functions need.
///
/// Borrows from App where possible to avoid cloning large structures.
/// Broker-sourced data (ui state, inbox threads, committed messages)
/// is owned because it comes from async reads.
#[allow(dead_code)]
pub struct ViewState<'a> {
    // -- Broker-sourced (owned, typed) -----------------------------------
    pub ui: UiSnapshot,

    /// Inbox threads (only populated on inbox screen).
    pub inbox_threads: Vec<InboxThread>,
    /// Messages for the active thread.
    pub messages: Vec<ChatMessage>,
    /// Turn state for the active thread.
    pub turn: ox_history::TurnState,
    /// Pending approval for the active thread.
    pub approval_pending: Option<ApprovalRequest>,

    // -- Config ----------------------------------------------------------
    pub model: String,
    pub provider: String,

    // -- App-borrowed (references) ---------------------------------------
    pub input_history: &'a [String],
    pub approval_selected: usize,
    pub pending_customize: &'a Option<CustomizeState>,
    pub key_hints: Vec<(String, String)>,
    pub show_shortcuts: bool,
    pub editor_mode: crate::editor::EditorMode,
    pub editor_command_buffer: String,
}

// ---------------------------------------------------------------------------
// fetch_view_state — build a ViewState from broker + App
// ---------------------------------------------------------------------------

/// Read state from the broker and borrow from App to produce a ViewState.
///
/// Reads conditionally: inbox screen reads `inbox/threads`, thread screen
/// reads `threads/{id}/history/messages`. Does not read both.
pub async fn fetch_view_state<'a>(
    client: &ClientHandle,
    app: &'a App,
    dialog: &'a crate::event_loop::DialogState,
    editor_mode: crate::editor::EditorMode,
    editor_command_buffer: &str,
) -> ViewState<'a> {
    // Read UiSnapshot via typed deserialization
    let ui: UiSnapshot = client
        .read_typed::<UiSnapshot>(&path!("ui"))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();

    // Conditional reads based on screen variant
    let mut inbox_threads = Vec::new();
    let mut messages = Vec::new();
    let mut turn = ox_history::TurnState::new();
    let mut approval_pending: Option<ApprovalRequest> = None;

    match &ui.screen {
        ScreenSnapshot::Inbox(_) => {
            // Read inbox threads
            if let Ok(Some(record)) = client.read(&path!("inbox/threads")).await {
                if let Some(val) = record.as_value() {
                    inbox_threads = parse_inbox_threads(val);
                }
            }
        }
        ScreenSnapshot::Thread(snap) => {
            let tid = &snap.thread_id;
            // Read committed messages
            let msg_path = ox_path::oxpath!("threads", tid, "history", "messages");
            if let Ok(Some(record)) = client.read(&msg_path).await {
                if let Some(Value::Array(arr)) = record.as_value() {
                    messages = parse_chat_messages(arr);
                }
            }

            // Read turn state (typed)
            let turn_path = ox_path::oxpath!("threads", tid, "history", "turn");
            if let Ok(Some(t)) = client.read_typed::<ox_history::TurnState>(&turn_path).await {
                turn = t;
            }

            // Read approval/pending (typed)
            let approval_path = ox_path::oxpath!("threads", tid, "approval", "pending");
            if let Ok(Some(ap)) = client.read_typed::<ApprovalRequest>(&approval_path).await {
                // Only treat as pending if the tool_name is non-empty
                if !ap.tool_name.is_empty() {
                    approval_pending = Some(ap);
                }
            }
        }
        ScreenSnapshot::Settings(_) => {}
    }

    // Read model and default account from broker ConfigStore
    let model = client
        .read_typed::<String>(&path!("config/gate/defaults/model"))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let provider = client
        .read_typed::<String>(&path!("config/gate/defaults/account"))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();

    // Read bindings for current mode+screen to build key hints
    let (mode_str, screen_str) = match &ui.screen {
        ScreenSnapshot::Inbox(_) => {
            if ui.editor().is_some() {
                ("insert", "inbox")
            } else {
                ("normal", "inbox")
            }
        }
        ScreenSnapshot::Thread(_) => (
            if ui.editor().is_some() {
                "insert"
            } else {
                "normal"
            },
            "thread",
        ),
        ScreenSnapshot::Settings(_) => ("normal", "settings"),
    };
    let key_hints = read_key_hints(client, mode_str, screen_str).await;

    ViewState {
        ui,
        inbox_threads,
        messages,
        turn,
        approval_pending,
        input_history: &app.input_history,
        model,
        provider,
        approval_selected: dialog.approval_selected,
        pending_customize: &dialog.pending_customize,
        key_hints,
        show_shortcuts: dialog.show_shortcuts,
        editor_mode,
        editor_command_buffer: editor_command_buffer.to_string(),
    }
}

/// Read bindings for the current mode+screen and extract (key, description) pairs.
async fn read_key_hints(client: &ClientHandle, mode: &str, screen: &str) -> Vec<(String, String)> {
    let bindings_path =
        structfs_core_store::Path::parse(&format!("input/bindings/{mode}/{screen}"))
            .unwrap_or_else(|_| path!("input/bindings"));
    let bindings = match client.read(&bindings_path).await {
        Ok(Some(record)) => match record.as_value() {
            Some(Value::Array(arr)) => arr.clone(),
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };

    let mut hints = Vec::new();
    let mut seen_keys = std::collections::HashSet::new();
    for binding in &bindings {
        if let Value::Map(m) = binding {
            let key = match m.get("key") {
                Some(Value::String(s)) => s.clone(),
                _ => continue,
            };
            let desc = match m.get("description") {
                Some(Value::String(s)) => s.clone(),
                _ => continue,
            };
            // Skip duplicate keys (screen-specific already takes priority in the
            // binding list, but we may see both generic and specific)
            if seen_keys.insert(key.clone()) {
                hints.push((key, desc));
            }
        }
    }
    hints
}

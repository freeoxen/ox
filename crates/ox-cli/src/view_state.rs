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
    /// Raw StructFS message values for the history explorer.
    pub raw_messages: Vec<Value>,
    /// History entries with pretty-printed content.
    pub history_pretty: std::collections::HashSet<usize>,
    /// History entries showing full content.
    pub history_full: std::collections::HashSet<usize>,

    // -- Config ----------------------------------------------------------
    pub model: String,
    pub provider: String,
    pub pricing_overrides: std::collections::BTreeMap<String, ox_gate::pricing::ModelPricing>,

    // -- App-borrowed (references) ---------------------------------------
    pub input_history: &'a [String],
    pub approval_selected: usize,
    pub pending_customize: &'a Option<CustomizeState>,
    pub key_hints: Vec<ox_types::KeyHint>,
    pub show_shortcuts: bool,
    pub show_usage: bool,
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
    let mut raw_messages = Vec::new();
    let mut turn = ox_history::TurnState::new();
    let mut approval_pending: Option<ApprovalRequest> = None;
    let mut history_pretty = std::collections::HashSet::new();
    let mut history_full = std::collections::HashSet::new();

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
            if let Ok(tid) = ox_kernel::PathComponent::try_new(snap.thread_id.as_str()) {
                // Read committed messages
                let msg_path = ox_path::oxpath!("threads", tid.clone(), "history", "messages");
                if let Ok(Some(record)) = client.read(&msg_path).await {
                    if let Some(Value::Array(arr)) = record.as_value() {
                        messages = parse_chat_messages(arr);
                    }
                }

                // Read turn state (typed)
                let turn_path = ox_path::oxpath!("threads", tid.clone(), "history", "turn");
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
        }
        ScreenSnapshot::History(snap) => {
            if let Ok(tid) = ox_kernel::PathComponent::try_new(snap.thread_id.as_str()) {
                let log_path = ox_path::oxpath!("threads", tid.clone(), "log", "entries");
                if let Ok(Some(record)) = client.read(&log_path).await {
                    if let Some(Value::Array(arr)) = record.as_value() {
                        raw_messages = arr.clone();
                    }
                }
                let turn_path = ox_path::oxpath!("threads", tid, "history", "turn");
                if let Ok(Some(t)) = client.read_typed::<ox_history::TurnState>(&turn_path).await {
                    turn = t;
                }
            }
            // Read pretty/full sets from UiStore (not serialized in snapshot)
            if let Ok(Some(record)) = client.read(&path!("ui/pretty")).await {
                if let Some(Value::Array(arr)) = record.as_value() {
                    for v in arr {
                        if let Value::Integer(n) = v {
                            history_pretty.insert(*n as usize);
                        }
                    }
                }
            }
            if let Ok(Some(record)) = client.read(&path!("ui/full")).await {
                if let Some(Value::Array(arr)) = record.as_value() {
                    for v in arr {
                        if let Value::Integer(n) = v {
                            history_full.insert(*n as usize);
                        }
                    }
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
        ScreenSnapshot::History(_) => ("normal", "history"),
        ScreenSnapshot::Settings(_) => ("normal", "settings"),
    };
    let key_hints = read_key_hints(client, mode_str, screen_str).await;

    // Read pricing overrides from config (empty BTreeMap if none configured).
    let pricing_overrides = read_pricing_overrides(client).await;

    ViewState {
        ui,
        inbox_threads,
        messages,
        raw_messages,
        history_pretty,
        history_full,
        turn,
        approval_pending,
        input_history: &app.input_history,
        model,
        provider,
        pricing_overrides,
        approval_selected: dialog.approval_selected,
        pending_customize: &dialog.pending_customize,
        key_hints,
        show_shortcuts: dialog.show_shortcuts,
        show_usage: dialog.show_usage,
        editor_mode,
        editor_command_buffer: editor_command_buffer.to_string(),
    }
}

/// Read pricing overrides from config/pricing.
///
/// Each key under `config/pricing` is a model prefix (e.g. `claude-sonnet-4`).
/// The value should be a map with `input_per_mtok` and `output_per_mtok` fields,
/// plus optional `cache_creation_multiplier` and `cache_read_multiplier`.
async fn read_pricing_overrides(
    client: &ClientHandle,
) -> std::collections::BTreeMap<String, ox_gate::pricing::ModelPricing> {
    let mut overrides = std::collections::BTreeMap::new();
    let Ok(Some(record)) = client
        .read(&structfs_core_store::path!("config/pricing"))
        .await
    else {
        return overrides;
    };
    let Some(Value::Map(map)) = record.as_value() else {
        return overrides;
    };
    for (model_prefix, val) in map {
        if let Value::Map(fields) = val {
            let input = match fields.get("input_per_mtok") {
                Some(Value::Float(f)) => *f,
                Some(Value::Integer(n)) => *n as f64,
                _ => continue,
            };
            let output = match fields.get("output_per_mtok") {
                Some(Value::Float(f)) => *f,
                Some(Value::Integer(n)) => *n as f64,
                _ => continue,
            };
            let cc = match fields.get("cache_creation_multiplier") {
                Some(Value::Float(f)) => *f,
                _ => 1.0,
            };
            let cr = match fields.get("cache_read_multiplier") {
                Some(Value::Float(f)) => *f,
                _ => 1.0,
            };
            overrides.insert(
                model_prefix.clone(),
                ox_gate::pricing::ModelPricing {
                    input_per_mtok: input,
                    output_per_mtok: output,
                    cache_creation_multiplier: cc,
                    cache_read_multiplier: cr,
                },
            );
        }
    }
    overrides
}

/// Read bindings for the current mode+screen, deduplicated by key.
async fn read_key_hints(client: &ClientHandle, mode: &str, screen: &str) -> Vec<ox_types::KeyHint> {
    let bindings_path =
        structfs_core_store::Path::parse(&format!("input/bindings/{mode}/{screen}"))
            .unwrap_or_else(|_| path!("input/bindings"));
    let hints: Vec<ox_types::KeyHint> = client
        .read_typed(&bindings_path)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();

    let mut result = Vec::new();
    let mut seen_keys = std::collections::HashSet::new();
    for hint in hints {
        if seen_keys.insert(hint.key.clone()) {
            result.push(hint);
        }
    }
    result
}

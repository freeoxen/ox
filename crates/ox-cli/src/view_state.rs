//! ViewState — per-frame snapshot of all state needed for rendering.
//!
//! `fetch_view_state` reads from the broker (UiStore, InboxStore) and
//! borrows from App to produce a `ViewState` that draw functions consume.
//! This decouples rendering from mutable App access and broker writes.

use std::collections::BTreeMap;

use ox_broker::ClientHandle;
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
    // -- Broker-sourced (owned) ------------------------------------------
    /// Current screen: "inbox" or "thread".
    pub screen: String,
    /// UI mode from broker.
    pub mode: String,
    /// Active thread id (None = inbox).
    pub active_thread: Option<String>,
    /// Selected inbox row.
    pub selected_row: usize,
    /// Scroll offset.
    pub scroll: u16,
    /// Scroll maximum.
    pub scroll_max: u16,
    /// Viewport height from broker.
    pub viewport_height: u16,
    /// Input text.
    pub input: String,
    /// Cursor position.
    pub cursor: usize,
    /// Pending action from UiStore command dispatch.
    pub pending_action: Option<String>,

    /// Inbox threads (only populated on inbox screen).
    pub inbox_threads: Vec<InboxThread>,
    /// Messages for the active thread (committed + in-progress turn).
    pub messages: Vec<ChatMessage>,
    /// Whether the agent is currently thinking/streaming.
    pub thinking: bool,
    /// Current tool call status: (tool_name, status).
    pub tool_status: Option<(String, String)>,
    /// Turn token usage: (input_tokens, output_tokens).
    pub turn_tokens: (u32, u32),
    /// Pending approval: (tool_name, input_preview), or None.
    pub approval_pending: Option<(String, String)>,

    // -- Broker-sourced search state --------------------------------------
    pub search_chips: Vec<String>,
    pub search_live_query: String,
    pub search_active: bool,

    // -- App-borrowed (references) ---------------------------------------
    /// Input history.
    pub input_history: &'a [String],
    /// Model name (read from broker ConfigStore).
    pub model: String,
    /// Provider name (read from broker ConfigStore).
    pub provider: String,
    /// Approval dialog selection index.
    pub approval_selected: usize,
    /// Pending customize dialog.
    pub pending_customize: &'a Option<CustomizeState>,
    /// Insert context from broker (e.g. "compose", "reply", "search"), or None in normal mode.
    pub insert_context: Option<String>,
    /// Key hints for the status bar, derived from bindings for the current mode+screen.
    /// Each entry is (key_label, description).
    pub key_hints: Vec<(String, String)>,
    /// Whether the shortcuts modal is showing.
    pub show_shortcuts: bool,
    /// Editor sub-mode within compose/reply input (insert vs normal).
    pub editor_mode: crate::event_loop::EditorMode,
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
    editor_mode: crate::event_loop::EditorMode,
) -> ViewState<'a> {
    // Read UiStore state
    let ui_state = match client.read(&path!("ui")).await {
        Ok(Some(record)) => match record.as_value() {
            Some(Value::Map(m)) => m.clone(),
            _ => BTreeMap::new(),
        },
        _ => BTreeMap::new(),
    };

    let screen = match ui_state.get("screen") {
        Some(Value::String(s)) => s.clone(),
        _ => "inbox".to_string(),
    };
    let mode = match ui_state.get("mode") {
        Some(Value::String(s)) => s.clone(),
        _ => "normal".to_string(),
    };
    let active_thread = match ui_state.get("active_thread") {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    };
    let selected_row = match ui_state.get("selected_row") {
        Some(Value::Integer(n)) => *n as usize,
        _ => 0,
    };
    let scroll = match ui_state.get("scroll") {
        Some(Value::Integer(n)) => *n as u16,
        _ => 0,
    };
    let scroll_max = match ui_state.get("scroll_max") {
        Some(Value::Integer(n)) => *n as u16,
        _ => 0,
    };
    let viewport_height = match ui_state.get("viewport_height") {
        Some(Value::Integer(n)) => *n as u16,
        _ => 0,
    };
    let (input, cursor) = match ui_state.get("input") {
        Some(Value::Map(m)) => {
            let content = match m.get("content") {
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            };
            let cur = match m.get("cursor") {
                Some(Value::Integer(n)) => *n as usize,
                _ => 0,
            };
            (content, cur)
        }
        _ => (String::new(), 0),
    };
    let pending_action = match ui_state.get("pending_action") {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    };

    let insert_context = match ui_state.get("insert_context") {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    };

    let search_chips = match ui_state.get("search_chips") {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let search_live_query = match ui_state.get("search_live_query") {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    let search_active = match ui_state.get("search_active") {
        Some(Value::Bool(b)) => *b,
        _ => false,
    };

    // Conditional reads based on screen
    let mut inbox_threads = Vec::new();
    let mut messages = Vec::new();
    let mut thinking = false;
    let mut tool_status: Option<(String, String)> = None;
    let mut turn_tokens: (u32, u32) = (0, 0);
    let mut approval_pending: Option<(String, String)> = None;

    if screen == "inbox" {
        // Read inbox threads
        if let Ok(Some(record)) = client.read(&path!("inbox/threads")).await {
            if let Some(val) = record.as_value() {
                inbox_threads = parse_inbox_threads(val);
            }
        }
    } else if screen == "thread" {
        if let Some(tid) = &active_thread {
            // Read committed messages
            let msg_path = ox_path::oxpath!("threads", tid, "history", "messages");
            if let Ok(Some(record)) = client.read(&msg_path).await {
                if let Some(Value::Array(arr)) = record.as_value() {
                    messages = parse_chat_messages(arr);
                }
            }

            // Read turn/thinking
            let thinking_path = ox_path::oxpath!("threads", tid, "history", "turn", "thinking");
            if let Ok(Some(record)) = client.read(&thinking_path).await {
                if let Some(Value::Bool(b)) = record.as_value() {
                    thinking = *b;
                }
            }

            // Read turn/tool
            let tool_path = ox_path::oxpath!("threads", tid, "history", "turn", "tool");
            if let Ok(Some(record)) = client.read(&tool_path).await {
                if let Some(Value::Map(m)) = record.as_value() {
                    let name = m
                        .get("name")
                        .and_then(|v| match v {
                            Value::String(s) => Some(s.clone()),
                            _ => None,
                        })
                        .unwrap_or_default();
                    let status = m
                        .get("status")
                        .and_then(|v| match v {
                            Value::String(s) => Some(s.clone()),
                            _ => None,
                        })
                        .unwrap_or_default();
                    tool_status = Some((name, status));
                }
            }

            // Read turn/tokens
            let tokens_path = ox_path::oxpath!("threads", tid, "history", "turn", "tokens");
            if let Ok(Some(record)) = client.read(&tokens_path).await {
                if let Some(Value::Map(m)) = record.as_value() {
                    let in_t = m
                        .get("in")
                        .and_then(|v| match v {
                            Value::Integer(i) => Some(*i as u32),
                            _ => None,
                        })
                        .unwrap_or(0);
                    let out_t = m
                        .get("out")
                        .and_then(|v| match v {
                            Value::Integer(i) => Some(*i as u32),
                            _ => None,
                        })
                        .unwrap_or(0);
                    turn_tokens = (in_t, out_t);
                }
            }

            // Read approval/pending
            let approval_path = ox_path::oxpath!("threads", tid, "approval", "pending");
            if let Ok(Some(record)) = client.read(&approval_path).await {
                if let Some(Value::Map(m)) = record.as_value() {
                    let tool_name = m.get("tool_name").and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    });
                    let input_preview = m.get("input_preview").and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    });
                    if let Some(tn) = tool_name {
                        approval_pending = Some((tn, input_preview.unwrap_or_default()));
                    }
                }
            }
        }
    }

    // Read model and default account from broker ConfigStore
    let model = match client.read(&path!("config/gate/defaults/model")).await {
        Ok(Some(r)) => match r.as_value() {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    };
    let provider = match client.read(&path!("config/gate/defaults/account")).await {
        Ok(Some(r)) => match r.as_value() {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    };

    // Read bindings for current mode+screen to build key hints
    let key_hints = read_key_hints(client, &mode, &screen).await;

    ViewState {
        screen,
        mode,
        active_thread,
        selected_row,
        scroll,
        scroll_max,
        viewport_height,
        input,
        cursor,
        pending_action,
        inbox_threads,
        messages,
        thinking,
        tool_status,
        turn_tokens,
        approval_pending,
        search_chips,
        search_live_query,
        search_active,
        input_history: &app.input_history,
        model,
        provider,
        approval_selected: dialog.approval_selected,
        pending_customize: &dialog.pending_customize,
        insert_context,
        key_hints,
        show_shortcuts: dialog.show_shortcuts,
        editor_mode,
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

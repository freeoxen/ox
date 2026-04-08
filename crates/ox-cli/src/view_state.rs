//! ViewState — per-frame snapshot of all state needed for rendering.
//!
//! `fetch_view_state` reads from the broker (UiStore, InboxStore) and
//! borrows from App to produce a `ViewState` that draw functions consume.
//! This decouples rendering from mutable App access and broker writes.

use std::collections::BTreeMap;

use ox_broker::ClientHandle;
use structfs_core_store::{Value, path};

use crate::app::{App, ChatMessage, CustomizeState, InputMode};

// ---------------------------------------------------------------------------
// InboxThread — parsed thread metadata for inbox display
// ---------------------------------------------------------------------------

/// Parsed thread metadata for inbox display.
#[derive(Debug, Clone)]
pub struct InboxThread {
    pub id: String,
    pub title: String,
    pub thread_state: String,
    pub labels: Vec<String>,
    pub token_count: i64,
    pub last_seq: i64,
}

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
    /// Model name.
    pub model: &'a str,
    /// Provider name.
    pub provider: &'a str,
    /// Approval dialog selection index.
    pub approval_selected: usize,
    /// Pending customize dialog.
    pub pending_customize: &'a Option<CustomizeState>,
    /// App input mode (from App, not broker — used until full migration).
    pub input_mode: &'a InputMode,
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Convert a slice of StructFS Values (Anthropic wire-format messages) into
/// `ChatMessage`s suitable for display.
pub fn parse_chat_messages(values: &[Value]) -> Vec<ChatMessage> {
    let mut out = Vec::new();
    for val in values {
        parse_one_message(val, &mut out);
    }
    out
}

fn parse_one_message(val: &Value, out: &mut Vec<ChatMessage>) {
    let map = match val {
        Value::Map(m) => m,
        _ => return,
    };

    let role = match map.get("role") {
        Some(Value::String(s)) => s.as_str(),
        _ => return,
    };

    let content = match map.get("content") {
        Some(v) => v,
        None => return,
    };

    match role {
        "user" => parse_user_content(content, out),
        "assistant" => parse_assistant_content(content, out),
        _ => {}
    }
}

fn parse_user_content(content: &Value, out: &mut Vec<ChatMessage>) {
    // Plain string content
    if let Value::String(s) = content {
        out.push(ChatMessage::User(s.clone()));
        return;
    }

    // Array content — look for tool_result blocks
    if let Value::Array(arr) = content {
        for item in arr {
            if let Value::Map(block) = item {
                if block.get("type") == Some(&Value::String("tool_result".to_string())) {
                    let content_str = match block.get("content") {
                        Some(Value::String(s)) => s.clone(),
                        _ => String::new(),
                    };
                    out.push(ChatMessage::ToolResult {
                        name: "tool".to_string(),
                        output: content_str,
                    });
                }
            }
        }
    }
}

fn parse_assistant_content(content: &Value, out: &mut Vec<ChatMessage>) {
    // Plain string content
    if let Value::String(s) = content {
        out.push(ChatMessage::AssistantChunk(s.clone()));
        return;
    }

    // Array of content blocks
    if let Value::Array(blocks) = content {
        let mut text = String::new();
        for block in blocks {
            let Value::Map(block_map) = block else {
                continue;
            };
            let block_type = match block_map.get("type") {
                Some(Value::String(s)) => s.as_str(),
                _ => continue,
            };
            match block_type {
                "text" => {
                    if let Some(Value::String(s)) = block_map.get("text") {
                        text.push_str(s);
                    }
                }
                "tool_use" => {
                    // Flush accumulated text before tool_use
                    if !text.is_empty() {
                        out.push(ChatMessage::AssistantChunk(std::mem::take(&mut text)));
                    }
                    let name = match block_map.get("name") {
                        Some(Value::String(s)) => s.clone(),
                        _ => "unknown".to_string(),
                    };
                    out.push(ChatMessage::ToolCall { name });
                }
                _ => {}
            }
        }
        if !text.is_empty() {
            out.push(ChatMessage::AssistantChunk(text));
        }
    }
}

/// Parse a broker inbox Value into a list of `InboxThread`s.
pub fn parse_inbox_threads(value: &Value) -> Vec<InboxThread> {
    let arr = match value {
        Value::Array(a) => a,
        _ => return Vec::new(),
    };

    let mut threads = Vec::with_capacity(arr.len());
    for item in arr {
        let Value::Map(map) = item else { continue };
        let id = match map.get("id") {
            Some(Value::String(s)) => s.clone(),
            _ => continue,
        };
        let title = match map.get("title") {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        let thread_state = match map.get("thread_state") {
            Some(Value::String(s)) => s.clone(),
            _ => "running".to_string(),
        };
        let labels = match map.get("labels") {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let token_count = match map.get("token_count") {
            Some(Value::Integer(n)) => *n,
            _ => 0,
        };
        let last_seq = match map.get("last_seq") {
            Some(Value::Integer(n)) => *n,
            _ => -1,
        };
        threads.push(InboxThread {
            id,
            title,
            thread_state,
            labels,
            token_count,
            last_seq,
        });
    }
    threads
}

// ---------------------------------------------------------------------------
// fetch_view_state — build a ViewState from broker + App
// ---------------------------------------------------------------------------

/// Read state from the broker and borrow from App to produce a ViewState.
///
/// Reads conditionally: inbox screen reads `inbox/threads`, thread screen
/// reads `threads/{id}/history/messages`. Does not read both.
pub async fn fetch_view_state<'a>(client: &ClientHandle, app: &'a App) -> ViewState<'a> {
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
    let input = match ui_state.get("input") {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    let cursor = match ui_state.get("cursor") {
        Some(Value::Integer(n)) => *n as usize,
        _ => 0,
    };
    let pending_action = match ui_state.get("pending_action") {
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
            let prefix = format!("threads/{tid}");

            // Read committed messages
            let msg_path =
                structfs_core_store::Path::parse(&format!("{prefix}/history/messages")).unwrap();
            if let Ok(Some(record)) = client.read(&msg_path).await {
                if let Some(Value::Array(arr)) = record.as_value() {
                    messages = parse_chat_messages(arr);
                }
            }

            // Read turn/thinking
            let thinking_path =
                structfs_core_store::Path::parse(&format!("{prefix}/history/turn/thinking"))
                    .unwrap();
            if let Ok(Some(record)) = client.read(&thinking_path).await {
                if let Some(Value::Bool(b)) = record.as_value() {
                    thinking = *b;
                }
            }

            // Read turn/tool
            let tool_path =
                structfs_core_store::Path::parse(&format!("{prefix}/history/turn/tool")).unwrap();
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
            let tokens_path =
                structfs_core_store::Path::parse(&format!("{prefix}/history/turn/tokens")).unwrap();
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
            let approval_path =
                structfs_core_store::Path::parse(&format!("{prefix}/approval/pending")).unwrap();
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
        model: &app.model,
        provider: &app.provider,
        approval_selected: app.approval_selected,
        pending_customize: &app.pending_customize,
        input_mode: &app.mode,
    }
}

// ---------------------------------------------------------------------------
// Standalone search filter
// ---------------------------------------------------------------------------

/// Check whether a thread matches all search chips and the live query.
#[allow(dead_code)]
pub fn search_matches(
    chips: &[String],
    live_query: &str,
    title: &str,
    labels: &[String],
    state: &str,
) -> bool {
    let hay = format!(
        "{} {} {}",
        title.to_lowercase(),
        labels
            .iter()
            .map(|l| l.to_lowercase())
            .collect::<Vec<_>>()
            .join(" "),
        state.to_lowercase()
    );
    for chip in chips {
        if !hay.contains(&chip.to_lowercase()) {
            return false;
        }
    }
    if !live_query.is_empty() && !hay.contains(&live_query.to_lowercase()) {
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::Value;

    /// Helper to build a Value::Map from key-value pairs.
    fn map(pairs: Vec<(&str, Value)>) -> Value {
        let mut m = BTreeMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v);
        }
        Value::Map(m)
    }

    fn s(val: &str) -> Value {
        Value::String(val.to_string())
    }

    #[test]
    fn parse_user_string_message() {
        let msgs = vec![map(vec![("role", s("user")), ("content", s("hello"))])];
        let parsed = parse_chat_messages(&msgs);
        assert_eq!(parsed.len(), 1);
        match &parsed[0] {
            ChatMessage::User(text) => assert_eq!(text, "hello"),
            other => panic!("expected User, got {other:?}"),
        }
    }

    #[test]
    fn parse_assistant_text_block() {
        let text_block = map(vec![("type", s("text")), ("text", s("hi there"))]);
        let msgs = vec![map(vec![
            ("role", s("assistant")),
            ("content", Value::Array(vec![text_block])),
        ])];
        let parsed = parse_chat_messages(&msgs);
        assert_eq!(parsed.len(), 1);
        match &parsed[0] {
            ChatMessage::AssistantChunk(text) => assert_eq!(text, "hi there"),
            other => panic!("expected AssistantChunk, got {other:?}"),
        }
    }

    #[test]
    fn parse_assistant_tool_use() {
        let tool_block = map(vec![
            ("type", s("tool_use")),
            ("id", s("tu_123")),
            ("name", s("read_file")),
            ("input", map(vec![("path", s("/tmp/x"))])),
        ]);
        let msgs = vec![map(vec![
            ("role", s("assistant")),
            ("content", Value::Array(vec![tool_block])),
        ])];
        let parsed = parse_chat_messages(&msgs);
        assert_eq!(parsed.len(), 1);
        match &parsed[0] {
            ChatMessage::ToolCall { name } => assert_eq!(name, "read_file"),
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn parse_user_tool_result() {
        let result_block = map(vec![
            ("type", s("tool_result")),
            ("tool_use_id", s("tu_123")),
            ("content", s("file contents here")),
        ]);
        let msgs = vec![map(vec![
            ("role", s("user")),
            ("content", Value::Array(vec![result_block])),
        ])];
        let parsed = parse_chat_messages(&msgs);
        assert_eq!(parsed.len(), 1);
        match &parsed[0] {
            ChatMessage::ToolResult { name, output } => {
                assert_eq!(name, "tool");
                assert_eq!(output, "file contents here");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn parse_mixed_conversation() {
        let user_msg = map(vec![("role", s("user")), ("content", s("explain this"))]);
        let text_block = map(vec![("type", s("text")), ("text", s("Sure, "))]);
        let tool_block = map(vec![
            ("type", s("tool_use")),
            ("id", s("tu_1")),
            ("name", s("grep")),
            ("input", map(vec![])),
        ]);
        let text_block2 = map(vec![("type", s("text")), ("text", s("done."))]);
        let assistant_msg = map(vec![
            ("role", s("assistant")),
            (
                "content",
                Value::Array(vec![text_block, tool_block, text_block2]),
            ),
        ]);
        let result_block = map(vec![
            ("type", s("tool_result")),
            ("tool_use_id", s("tu_1")),
            ("content", s("match found")),
        ]);
        let user_result = map(vec![
            ("role", s("user")),
            ("content", Value::Array(vec![result_block])),
        ]);

        let msgs = vec![user_msg, assistant_msg, user_result];
        let parsed = parse_chat_messages(&msgs);

        // User, AssistantChunk("Sure, "), ToolCall(grep), AssistantChunk("done."), ToolResult
        assert_eq!(parsed.len(), 5);
        assert!(matches!(&parsed[0], ChatMessage::User(t) if t == "explain this"));
        assert!(matches!(&parsed[1], ChatMessage::AssistantChunk(t) if t == "Sure, "));
        assert!(matches!(&parsed[2], ChatMessage::ToolCall { name } if name == "grep"));
        assert!(matches!(&parsed[3], ChatMessage::AssistantChunk(t) if t == "done."));
        assert!(
            matches!(&parsed[4], ChatMessage::ToolResult { name, output } if name == "tool" && output == "match found")
        );
    }

    #[test]
    fn parse_inbox_threads_basic() {
        let thread = map(vec![
            ("id", s("t_abc")),
            ("title", s("My thread")),
            ("thread_state", s("running")),
            ("labels", Value::Array(vec![s("bug")])),
            ("token_count", Value::Integer(1500)),
            ("last_seq", Value::Integer(3)),
        ]);
        let threads = parse_inbox_threads(&Value::Array(vec![thread]));
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, "t_abc");
        assert_eq!(threads[0].title, "My thread");
        assert_eq!(threads[0].thread_state, "running");
        assert_eq!(threads[0].labels, vec!["bug".to_string()]);
        assert_eq!(threads[0].token_count, 1500);
        assert_eq!(threads[0].last_seq, 3);
    }

    #[test]
    fn parse_inbox_threads_non_array_returns_empty() {
        let result = parse_inbox_threads(&Value::String("not an array".to_string()));
        assert!(result.is_empty());
    }

    #[test]
    fn parse_chat_messages_empty() {
        let parsed = parse_chat_messages(&[]);
        assert!(parsed.is_empty());
    }

    #[test]
    fn parse_unknown_role_ignored() {
        let msgs = vec![map(vec![
            ("role", s("system")),
            ("content", s("you are helpful")),
        ])];
        let parsed = parse_chat_messages(&msgs);
        assert!(parsed.is_empty());
    }

    #[test]
    fn parse_assistant_plain_string_content() {
        let msgs = vec![map(vec![
            ("role", s("assistant")),
            ("content", s("plain text response")),
        ])];
        let parsed = parse_chat_messages(&msgs);
        assert_eq!(parsed.len(), 1);
        assert!(matches!(&parsed[0], ChatMessage::AssistantChunk(t) if t == "plain text response"));
    }
}

# Draw Rewrite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** TUI draw functions read from a per-frame `ViewState` snapshot fetched from the broker instead of from `App` fields. `state_sync.rs` is deleted, `App` shrinks.

**Architecture:** One async function `fetch_view_state()` reads all needed state from the broker + App's streaming cache into a `ViewState` struct each frame. Draw functions take `&ViewState` instead of `&App`/`&mut App`. The broker is the sole source of truth for committed data; `StreamingTurn` is the only cache (for in-progress agent turns not yet in the broker).

**Tech Stack:** Rust, ratatui, ox-broker (ClientHandle), structfs-core-store, serde_json

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/ox-cli/src/view_state.rs` | Create | ViewState struct, StreamingTurn, fetch_view_state(), parse_chat_messages() |
| `crates/ox-cli/src/tui.rs` | Modify | Event loop: fetch ViewState, pass to draw, drain events separately |
| `crates/ox-cli/src/app.rs` | Modify | Remove dead fields, replace thread_views with streaming_turns, simplify handle_event |
| `crates/ox-cli/src/inbox_view.rs` | Modify | draw_inbox takes &ViewState |
| `crates/ox-cli/src/tab_bar.rs` | Modify | draw_tabs takes &ViewState |
| `crates/ox-cli/src/thread_view.rs` | Minor | No signature change (already takes &ThreadView); content_height return unchanged |
| `crates/ox-cli/src/state_sync.rs` | Delete | Replaced entirely by ViewState |
| `crates/ox-cli/src/main.rs` | Modify | Remove state_sync module declaration |

---

### Task 1: ViewState struct + fetch_view_state

Create the ViewState module with the struct, the broker fetch function, and a parser that converts broker message Values into ChatMessage.

**Files:**
- Create: `crates/ox-cli/src/view_state.rs`
- Modify: `crates/ox-cli/src/main.rs` (add `pub(crate) mod view_state;`)

- [ ] **Step 1: Write test for parse_chat_messages**

Create `crates/ox-cli/src/view_state.rs` with the test at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_user_message() {
        let msg = serde_json::json!({"role": "user", "content": "hello"});
        let value = structfs_serde_store::json_to_value(msg);
        let result = parse_chat_messages(&[value]);
        assert_eq!(result.len(), 1);
        match &result[0] {
            ChatMessage::User(s) => assert_eq!(s, "hello"),
            other => panic!("expected User, got {:?}", other),
        }
    }

    #[test]
    fn parse_assistant_text_message() {
        let msg = serde_json::json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "hi there"}]
        });
        let value = structfs_serde_store::json_to_value(msg);
        let result = parse_chat_messages(&[value]);
        assert_eq!(result.len(), 1);
        match &result[0] {
            ChatMessage::AssistantChunk(s) => assert_eq!(s, "hi there"),
            other => panic!("expected AssistantChunk, got {:?}", other),
        }
    }

    #[test]
    fn parse_assistant_tool_use_message() {
        let msg = serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Let me check."},
                {"type": "tool_use", "id": "call_1", "name": "read_file", "input": {"path": "foo.rs"}}
            ]
        });
        let value = structfs_serde_store::json_to_value(msg);
        let result = parse_chat_messages(&[value]);
        assert_eq!(result.len(), 2);
        match &result[0] {
            ChatMessage::AssistantChunk(s) => assert_eq!(s, "Let me check."),
            other => panic!("expected AssistantChunk, got {:?}", other),
        }
        match &result[1] {
            ChatMessage::ToolCall { name } => assert_eq!(name, "read_file"),
            other => panic!("expected ToolCall, got {:?}", other),
        }
    }

    #[test]
    fn parse_tool_result_message() {
        let msg = serde_json::json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "call_1", "content": "file contents here"}
            ]
        });
        let value = structfs_serde_store::json_to_value(msg);
        let result = parse_chat_messages(&[value]);
        assert_eq!(result.len(), 1);
        match &result[0] {
            ChatMessage::ToolResult { name, output } => {
                assert_eq!(name, "call_1");
                assert_eq!(output, "file contents here");
            }
            other => panic!("expected ToolResult, got {:?}", other),
        }
    }

    #[test]
    fn parse_mixed_conversation() {
        let msgs = vec![
            serde_json::json!({"role": "user", "content": "hello"}),
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "hi"}]}),
        ];
        let values: Vec<_> = msgs
            .into_iter()
            .map(structfs_serde_store::json_to_value)
            .collect();
        let result = parse_chat_messages(&values);
        assert_eq!(result.len(), 2);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-cli parse_chat_messages`
Expected: FAIL — function doesn't exist yet.

- [ ] **Step 3: Write parse_chat_messages + ViewState struct**

Add to `crates/ox-cli/src/view_state.rs` above the tests:

```rust
//! ViewState — per-frame snapshot of all state needed for rendering.
//!
//! Assembled by `fetch_view_state()` each frame from broker reads + App's
//! streaming cache. Draw functions take `&ViewState` — pure, sync, testable.

use std::collections::HashMap;

use ox_broker::ClientHandle;
use structfs_core_store::{Value, path};
use structfs_serde_store::value_to_json;

use crate::app::{ApprovalState, ChatMessage, CustomizeState, SearchState, ThreadView};

/// Lightweight streaming state for in-progress agent turns.
///
/// Accumulated from AppEvent in the event loop. Cleared when the turn
/// commits (AppEvent::Done). This is the only cache — everything else
/// comes from the broker.
#[derive(Debug, Clone, Default)]
pub struct StreamingTurn {
    /// Accumulated text from TextDelta events.
    pub text: String,
    /// Current tool call (name) if one is in progress.
    pub tool_name: Option<String>,
    /// Whether the agent is mid-turn.
    pub thinking: bool,
    /// Token counts for this session.
    pub tokens_in: u32,
    pub tokens_out: u32,
}

/// Per-frame snapshot of all state needed for rendering.
pub struct ViewState<'a> {
    // -- UI state (from broker ui/* paths) --
    pub screen: String,
    pub mode: String,
    pub active_thread: Option<String>,
    pub selected_row: usize,
    pub scroll: usize,
    pub input: String,
    pub cursor: usize,
    pub pending_action: Option<String>,
    pub scroll_max: usize,
    pub viewport_height: usize,

    // -- Inbox (from broker inbox/threads) --
    pub inbox_threads: Vec<InboxThread>,

    // -- Active thread (from broker + streaming cache) --
    pub thread_view: Option<ThreadView>,

    // -- Streaming status per thread (for inbox live indicators) --
    pub streaming_turns: &'a HashMap<String, StreamingTurn>,

    // -- From App (not yet in broker) --
    pub search: &'a SearchState,
    pub input_history_len: usize,
    pub model: String,
    pub provider: String,

    // -- Dialogs --
    pub pending_approval: &'a Option<ApprovalState>,
    pub pending_customize: &'a Option<CustomizeState>,
}

/// Thread metadata for inbox display.
#[derive(Debug, Clone)]
pub struct InboxThread {
    pub id: String,
    pub title: String,
    pub state: String,
    pub labels: Vec<String>,
    pub token_count: i64,
    pub last_seq: i64,
}

/// Parse broker history messages (StructFS Value) into ChatMessage list.
///
/// Handles the Anthropic message format:
/// - User messages: `{"role": "user", "content": "text"}` or content array with tool_result
/// - Assistant messages: `{"role": "assistant", "content": [blocks...]}`
pub fn parse_chat_messages(values: &[Value]) -> Vec<ChatMessage> {
    let mut messages = Vec::new();
    for value in values {
        let json = value_to_json(value.clone());
        let role = json.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let content = &json["content"];

        match role {
            "user" => {
                if let Some(text) = content.as_str() {
                    messages.push(ChatMessage::User(text.to_string()));
                } else if let Some(arr) = content.as_array() {
                    // Tool results come as user messages with content array
                    for block in arr {
                        let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        if block_type == "tool_result" {
                            let tool_id = block
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown")
                                .to_string();
                            let output = block
                                .get("content")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            messages.push(ChatMessage::ToolResult {
                                name: tool_id,
                                output,
                            });
                        }
                    }
                }
            }
            "assistant" => {
                if let Some(arr) = content.as_array() {
                    for block in arr {
                        let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        match block_type {
                            "text" => {
                                let text = block
                                    .get("text")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                messages.push(ChatMessage::AssistantChunk(text));
                            }
                            "tool_use" => {
                                let name = block
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                messages.push(ChatMessage::ToolCall { name });
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    messages
}

/// Parse inbox thread list from broker Value into InboxThread structs.
fn parse_inbox_threads(value: &Value) -> Vec<InboxThread> {
    let arr = match value {
        Value::Array(a) => a,
        _ => return Vec::new(),
    };
    arr.iter()
        .filter_map(|item| {
            let map = match item {
                Value::Map(m) => m,
                _ => return None,
            };
            let id = match map.get("id") {
                Some(Value::String(s)) => s.clone(),
                _ => return None,
            };
            let title = match map.get("title") {
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            };
            let state = match map.get("thread_state") {
                Some(Value::String(s)) => s.clone(),
                _ => "running".to_string(),
            };
            let labels = match map.get("labels") {
                Some(Value::Array(a)) => a
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
            Some(InboxThread {
                id,
                title,
                state,
                labels,
                token_count,
                last_seq,
            })
        })
        .collect()
}

/// Fetch a complete ViewState from the broker + App's live state.
///
/// Reads UI state in one call, then conditionally reads inbox or thread
/// data based on the current screen.
pub async fn fetch_view_state<'a>(
    client: &ClientHandle,
    app: &'a crate::app::App,
) -> ViewState<'a> {
    // Read all UI state as a single Map
    let ui = client
        .read(&path!("ui"))
        .await
        .ok()
        .flatten()
        .and_then(|r| r.as_value().cloned())
        .and_then(|v| match v {
            Value::Map(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default();

    let get_str = |key: &str| -> String {
        match ui.get(key) {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        }
    };
    let get_usize = |key: &str| -> usize {
        match ui.get(key) {
            Some(Value::Integer(n)) => *n as usize,
            _ => 0,
        }
    };

    let screen = get_str("screen");
    let active_thread = match ui.get("active_thread") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    };
    let pending_action = match ui.get("pending_action") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    };

    // Conditional reads based on screen
    let mut inbox_threads = Vec::new();
    let mut thread_view = None;

    if screen == "thread" || active_thread.is_some() {
        // Read committed messages from broker
        if let Some(ref tid) = active_thread {
            let msg_path = structfs_core_store::Path::parse(&format!(
                "threads/{tid}/history/messages"
            ))
            .ok();
            let committed_values = if let Some(ref p) = msg_path {
                client
                    .read(p)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|r| r.as_value().cloned())
                    .and_then(|v| match v {
                        Value::Array(a) => Some(a),
                        _ => None,
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            let mut messages = parse_chat_messages(&committed_values);

            // Merge streaming turn if present
            if let Some(st) = app.streaming_turns.get(tid.as_str()) {
                if !st.text.is_empty() {
                    messages.push(ChatMessage::AssistantChunk(st.text.clone()));
                }
                if let Some(ref tool) = st.tool_name {
                    messages.push(ChatMessage::ToolCall {
                        name: tool.clone(),
                    });
                }
            }

            let thinking = app
                .streaming_turns
                .get(tid.as_str())
                .map(|st| st.thinking)
                .unwrap_or(false);

            thread_view = Some(ThreadView {
                messages,
                thinking,
                tokens_in: app
                    .streaming_turns
                    .get(tid.as_str())
                    .map(|st| st.tokens_in)
                    .unwrap_or(0),
                tokens_out: app
                    .streaming_turns
                    .get(tid.as_str())
                    .map(|st| st.tokens_out)
                    .unwrap_or(0),
                policy_stats: Default::default(),
            });
        }
    } else {
        // Inbox screen — read thread list
        let threads_value = client
            .read(&path!("inbox/threads"))
            .await
            .ok()
            .flatten()
            .and_then(|r| r.as_value().cloned())
            .unwrap_or(Value::Array(Vec::new()));
        inbox_threads = parse_inbox_threads(&threads_value);

        // Apply search filter if active
        if app.search.is_active() {
            let query = app.search.effective_query().to_lowercase();
            inbox_threads.retain(|t| {
                t.title.to_lowercase().contains(&query)
                    || t.labels.iter().any(|l| l.to_lowercase().contains(&query))
            });
        }
    }

    ViewState {
        screen,
        mode: get_str("mode"),
        active_thread,
        selected_row: get_usize("selected_row"),
        scroll: get_usize("scroll"),
        input: get_str("input"),
        cursor: get_usize("cursor"),
        pending_action,
        scroll_max: get_usize("scroll_max"),
        viewport_height: get_usize("viewport_height"),
        inbox_threads,
        thread_view,
        streaming_turns: &app.streaming_turns,
        search: &app.search,
        input_history_len: app.input_history.len(),
        model: app.model.clone(),
        provider: app.provider.clone(),
        pending_approval: &app.pending_approval,
        pending_customize: &app.pending_customize,
    }
}
```

- [ ] **Step 4: Register module**

In `crates/ox-cli/src/main.rs`, add:

```rust
pub(crate) mod view_state;
```

alongside the other module declarations.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ox-cli parse_chat_messages`
Expected: All 5 parse tests pass.

- [ ] **Step 6: Write fetch_view_state integration test**

Add to the test module in `view_state.rs`:

```rust
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fetch_view_state_reads_ui_state() {
        use ox_broker::BrokerStore;
        use ox_ui::UiStore;

        let broker = BrokerStore::default();
        broker.mount(path!("ui"), UiStore::new()).await;

        let client = broker.client();
        // UiStore defaults: screen="inbox", mode="normal", selected_row=0
        let app = make_test_app();
        let vs = fetch_view_state(&client, &app).await;

        assert_eq!(vs.screen, "inbox");
        assert_eq!(vs.mode, "normal");
        assert!(vs.active_thread.is_none());
        assert_eq!(vs.selected_row, 0);
    }
```

This test requires a `make_test_app()` helper that creates a minimal App. Add it:

```rust
    #[cfg(test)]
    fn make_test_app() -> crate::app::App {
        // App::new requires inbox + broker + rt_handle — use a minimal construction.
        // We only need the fields that fetch_view_state reads from App:
        // streaming_turns, search, input_history, model, provider,
        // pending_approval, pending_customize
        //
        // Create App with test values. This may need App to have a test constructor
        // or we construct it via App::new with a temp dir.
        let dir = tempfile::tempdir().unwrap();
        let inbox_root = dir.path().to_path_buf();
        let broker = ox_broker::BrokerStore::default();
        let rt_handle = tokio::runtime::Handle::current();
        crate::app::App::new(
            "anthropic".to_string(),
            "test-model".to_string(),
            1024,
            "sk-test".to_string(),
            dir.path().to_path_buf(),
            inbox_root,
            true, // no_policy
            broker,
            rt_handle,
        )
        .unwrap()
    }
```

Note: This test requires that App::new works with a temp directory. The agent.wasm must exist at `target/agent.wasm` for AgentPool initialization. If it doesn't exist, the test will fail. If that's the case, add a `#[cfg(test)]` constructor to App that skips AgentPool, or gate the test with an existence check similar to `engine.rs`.

If App::new fails without agent.wasm, wrap the test:

```rust
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fetch_view_state_reads_ui_state() {
        use ox_broker::BrokerStore;
        use ox_ui::UiStore;

        let app = match make_test_app() {
            Some(app) => app,
            None => {
                println!("SKIPPED: agent.wasm not found");
                return;
            }
        };

        let broker = BrokerStore::default();
        broker.mount(path!("ui"), UiStore::new()).await;

        let client = broker.client();
        let vs = fetch_view_state(&client, &app).await;

        assert_eq!(vs.screen, "inbox");
        assert_eq!(vs.mode, "normal");
    }
```

And make `make_test_app()` return `Option<App>`.

- [ ] **Step 7: Run all tests**

Run: `cargo test -p ox-cli view_state`
Expected: All pass.

- [ ] **Step 8: Commit**

```bash
git add crates/ox-cli/src/view_state.rs crates/ox-cli/src/main.rs
git commit -m "feat(ox-cli): add ViewState struct and fetch_view_state"
```

---

### Task 2: StreamingTurn + drain_agent_events

Add `streaming_turns: HashMap<String, StreamingTurn>` to App. Write `drain_agent_events()` that populates it from `event_rx`. This runs alongside the existing `thread_views` — both are populated for now. The draw rewrite (Task 3) will read from ViewState which uses `streaming_turns`; `thread_views` gets removed in Task 4.

**Files:**
- Modify: `crates/ox-cli/src/app.rs`
- Test: `crates/ox-cli/src/app.rs` or `crates/ox-cli/src/view_state.rs`

- [ ] **Step 1: Add streaming_turns field to App**

In `crates/ox-cli/src/app.rs`, add to the App struct after `thread_views`:

```rust
    /// In-progress streaming state per thread (the only cache).
    pub streaming_turns: HashMap<String, crate::view_state::StreamingTurn>,
```

Initialize it in `App::new`:

```rust
    streaming_turns: HashMap::new(),
```

- [ ] **Step 2: Write drain_agent_events function**

Add to `crates/ox-cli/src/app.rs`:

```rust
    /// Drain agent events from event_rx, updating streaming_turns and inbox.
    ///
    /// This replaces the streaming-related parts of handle_event for the
    /// ViewState path. Also handles Usage, PolicyStats, Done, SaveComplete
    /// for their side effects (inbox SQLite updates).
    pub fn drain_agent_events(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            // Still call handle_event for thread_views (backward compat until Task 4)
            self.handle_event_for_streaming(&event);
            self.handle_event(event);
        }
    }

    fn handle_event_for_streaming(&mut self, event: &AppEvent) {
        match event {
            AppEvent::Agent {
                ref thread_id,
                ref event,
            } => {
                let st = self
                    .streaming_turns
                    .entry(thread_id.clone())
                    .or_default();
                match event {
                    AgentEvent::TurnStart => {
                        st.thinking = true;
                        st.text.clear();
                        st.tool_name = None;
                    }
                    AgentEvent::TextDelta(text) => {
                        st.text.push_str(text);
                    }
                    AgentEvent::ToolCallStart { name } => {
                        st.tool_name = Some(name.clone());
                    }
                    AgentEvent::ToolCallResult { .. } => {
                        st.tool_name = None;
                    }
                    AgentEvent::TurnEnd => {
                        st.thinking = false;
                    }
                    AgentEvent::Error(_) => {
                        st.thinking = false;
                    }
                }
            }
            AppEvent::Usage {
                ref thread_id,
                input_tokens,
                output_tokens,
            } => {
                let st = self
                    .streaming_turns
                    .entry(thread_id.clone())
                    .or_default();
                st.tokens_in += input_tokens;
                st.tokens_out += output_tokens;
            }
            AppEvent::Done { ref thread_id, .. } => {
                // Clear streaming state — committed messages are in the broker now
                self.streaming_turns.remove(thread_id);
            }
            _ => {}
        }
    }
```

- [ ] **Step 3: Run compile check**

Run: `cargo check -p ox-cli`
Expected: Compiles (warnings about unused drain_agent_events are OK for now).

- [ ] **Step 4: Commit**

```bash
git add crates/ox-cli/src/app.rs
git commit -m "feat(ox-cli): add streaming_turns and drain_agent_events to App"
```

---

### Task 3: Rewrite draw functions to take &ViewState

Change draw_inbox, draw_tabs, draw_status_bar, and the main draw function to read from `&ViewState` instead of `&App` / `&mut App`. `draw_thread` is unchanged (already takes `&ThreadView`).

**Files:**
- Modify: `crates/ox-cli/src/inbox_view.rs`
- Modify: `crates/ox-cli/src/tab_bar.rs`
- Modify: `crates/ox-cli/src/tui.rs` (draw, draw_status_bar, draw_customize_dialog, draw_approval_dialog)

- [ ] **Step 1: Rewrite draw_inbox**

In `crates/ox-cli/src/inbox_view.rs`, change the signature from:

```rust
pub fn draw_inbox(frame: &mut Frame, app: &App, theme: &Theme, area: Rect)
```

to:

```rust
pub fn draw_inbox(
    frame: &mut Frame,
    vs: &crate::view_state::ViewState,
    theme: &Theme,
    area: Rect,
)
```

Replace all `app.` reads:
- `app.cached_threads` → `vs.inbox_threads` (but the type changes from tuple to `InboxThread` struct — update field access: `.0` → `.id`, `.1` → `.title`, `.2` → `.state`, `.3` → `.labels`, `.4` → `.token_count`, `.5` → `.last_seq`)
- `app.selected_row` → `vs.selected_row`
- `app.inbox_scroll` → `vs.scroll` (inbox scroll is now the unified scroll from UiStore)
- `app.thread_views.get(&id)` → `vs.streaming_turns.get(&id)` for live indicators (thinking, tokens)
- `app.search.is_active()` → `vs.search.is_active()` for empty state message

For streaming data in inbox rows, `StreamingTurn` has `thinking`, `tokens_in`, `tokens_out`, `text` (for activity detection). Adapt the per-row rendering to read from StreamingTurn instead of ThreadView.

Also change `draw_filter_bar`:

```rust
pub fn draw_filter_bar(
    frame: &mut Frame,
    vs: &crate::view_state::ViewState,
    theme: &Theme,
    area: Rect,
)
```

Replace `app.search.chips` → `vs.search.chips`, `app.search.live_query` → `vs.search.live_query`.

- [ ] **Step 2: Rewrite draw_tabs**

In `crates/ox-cli/src/tab_bar.rs`, change from:

```rust
pub fn draw_tabs(frame: &mut Frame, app: &App, theme: &Theme, area: Rect)
```

to:

```rust
pub fn draw_tabs(
    frame: &mut Frame,
    vs: &crate::view_state::ViewState,
    theme: &Theme,
    area: Rect,
)
```

Replace:
- `app.active_thread` → `vs.active_thread`
- `app.thread_views.get(tid)` → `vs.thread_view.as_ref()` (for title from first message)
- `app.cached_threads.len()` → `vs.inbox_threads.len()`
- `app.model` → `vs.model`
- `app.provider` → `vs.provider`

- [ ] **Step 3: Rewrite draw + draw_status_bar in tui.rs**

In `crates/ox-cli/src/tui.rs`, change the main `draw` function from:

```rust
fn draw(frame: &mut Frame, app: &mut App, theme: &Theme)
```

to:

```rust
fn draw(frame: &mut Frame, vs: &ViewState, theme: &Theme) -> Option<usize>
```

Returns `Some(content_height)` if a thread was drawn (for scroll_max feedback), None for inbox.

Replace all `app.` reads with `vs.` reads:
- `app.mode` → `vs.mode` (compare as string: `vs.mode.starts_with("insert")` or parse)
- `app.active_thread` → `vs.active_thread`
- `app.search.is_active()` → `vs.search.is_active()`
- `app.scroll` → `vs.scroll as u16`
- `app.input` → `vs.input`
- `app.cursor` → `vs.cursor`
- `app.pending_approval` → `vs.pending_approval`
- `app.pending_customize` → `vs.pending_customize`

For the thread content area, instead of:
```rust
let view = app.thread_views.entry(tid).or_default().clone();
app.last_content_height = draw_thread(frame, &view, app.scroll, theme, content_area);
```

Do:
```rust
if let Some(ref view) = vs.thread_view {
    let ch = draw_thread(frame, view, vs.scroll as u16, theme, content_area);
    content_height = Some(ch);
}
```

For inbox, remove `app.refresh_visible_threads()` and `app.ensure_selected_visible()` — inbox data comes from ViewState now. The scroll clamping logic should move into `fetch_view_state` or be handled by UiStore.

For `draw_status_bar`, change to:

```rust
fn draw_status_bar(frame: &mut Frame, vs: &ViewState, theme: &Theme, area: Rect)
```

Replace:
- `app.mode` → use `vs.mode` (string comparison)
- `app.active_thread` → `vs.active_thread`
- `app.thread_views.get(tid)` → `vs.thread_view.as_ref()` or `vs.streaming_turns.get(tid)`
- `app.cached_threads.len()` → `vs.inbox_threads.len()`

- [ ] **Step 4: Compile check**

Run: `cargo check -p ox-cli`
Expected: Errors in tui.rs because the event loop still calls `draw(frame, app, theme)`. That's expected — we fix it in Task 4.

Actually, we need to update the call site too. Change the `terminal.draw(|f| draw(f, app, theme))` call in the event loop to pass ViewState. But the event loop rewrite is Task 4. So for this task, **also update the draw call in run_async** to create a temporary ViewState. This makes it compile.

In `run_async`, replace:
```rust
terminal.draw(|f| draw(f, app, theme))?;
```
with:
```rust
let vs = crate::view_state::fetch_view_state(client, app).await;
let content_height = terminal.draw(|f| draw(f, &vs, theme))?.;
```

Wait — `terminal.draw` takes a closure that returns nothing. We need another approach for content_height. Use a Cell or just call draw and return content_height through a mutable reference:

```rust
let vs = crate::view_state::fetch_view_state(client, app).await;
let mut content_height: Option<usize> = None;
terminal.draw(|f| {
    content_height = draw(f, &vs, theme);
})?;
```

- [ ] **Step 5: Run compile + tests**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: Compiles. All existing tests pass (broker_setup tests don't call draw).

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/inbox_view.rs crates/ox-cli/src/tab_bar.rs crates/ox-cli/src/tui.rs
git commit -m "refactor(ox-cli): draw functions take &ViewState instead of &App"
```

---

### Task 4: Rewrite event loop

Replace the sync-based event loop in `run_async` with the ViewState-based loop. Remove all `sync_ui_to_app` / `sync_app_to_ui` calls. Use `drain_agent_events` instead of inline `handle_event`. Read pending_action from ViewState.

**Files:**
- Modify: `crates/ox-cli/src/tui.rs`

- [ ] **Step 1: Rewrite run_async**

Replace the body of `run_async` with the new event loop structure:

```rust
pub async fn run_async(
    app: &mut App,
    client: &ox_broker::ClientHandle,
    theme: &Theme,
    terminal: &mut ratatui::DefaultTerminal,
) -> std::io::Result<()> {
    loop {
        // 1. Drain agent events → update streaming_turns (+ thread_views for compat)
        app.drain_agent_events();

        // 2. Handle control events (approval requests)
        if app.pending_approval.is_none() && app.pending_customize.is_none() {
            if let Ok(crate::app::AppControl::PermissionRequest {
                thread_id,
                tool,
                input_preview,
                respond,
            }) = app.control_rx.try_recv()
            {
                app.pending_approval = Some(ApprovalState {
                    thread_id,
                    tool,
                    input_preview,
                    selected: 0,
                    respond,
                });
            }
        }

        // 3. Fetch ViewState from broker + streaming cache
        let vs = crate::view_state::fetch_view_state(client, app).await;

        // 4. Sync row count to UiStore (inbox screen only)
        if vs.active_thread.is_none() {
            let count = vs.inbox_threads.len() as i64;
            // ... write ui/set_row_count ...
        }

        // 5. Render (sync, pure)
        let mut content_height: Option<usize> = None;
        terminal.draw(|f| {
            content_height = draw(f, &vs, theme);
        })?;

        // 6. Update scroll_max from rendered content height
        if let Some(ch) = content_height {
            let vh = terminal.size()?.height as usize;
            // ... write ui/set_scroll_max and ui/set_viewport_height ...
        }

        // 7. Handle pending_action from ViewState
        if let Some(ref action) = vs.pending_action {
            match action.as_str() {
                "send_input" => {
                    app.send_input(&vs);
                    // Clear pending action
                    client
                        .write(&path!("ui/clear_pending_action"), Record::parsed(Value::Null))
                        .await
                        .ok();
                }
                "quit" => break,
                "open_selected" => {
                    // Read selected thread from ViewState
                    if let Some(thread) = vs.inbox_threads.get(vs.selected_row) {
                        app.open_thread_via_broker(client, &thread.id).await;
                    }
                    client
                        .write(&path!("ui/clear_pending_action"), Record::parsed(Value::Null))
                        .await
                        .ok();
                }
                "archive_selected" => {
                    if let Some(thread) = vs.inbox_threads.get(vs.selected_row) {
                        app.archive_thread_via_broker(client, &thread.id).await;
                    }
                    client
                        .write(&path!("ui/clear_pending_action"), Record::parsed(Value::Null))
                        .await
                        .ok();
                }
                _ => {
                    client
                        .write(&path!("ui/clear_pending_action"), Record::parsed(Value::Null))
                        .await
                        .ok();
                }
            }
        }

        // 8. Poll terminal event
        let event = tokio::task::block_in_place(|| {
            if crossterm::event::poll(std::time::Duration::from_millis(50))? {
                Ok(Some(crossterm::event::read()?))
            } else {
                Ok(None)
            }
        })?;

        if let Some(crossterm::event::Event::Key(key)) = event {
            // Key handling — same as current but reads vs.mode instead of app.mode
            handle_key_event(app, client, &vs, key).await;
        } else if let Some(crossterm::event::Event::Mouse(mouse)) = event {
            handle_mouse_event(app, client, &vs, mouse).await;
        }
    }
    Ok(())
}
```

Note: This is the structural skeleton. The actual key/mouse handling functions can be extracted from the current inline code. The key change is: no `sync_ui_to_app` or `sync_app_to_ui` calls anywhere.

**Important methods to add to App for broker-mediated actions:**

`send_input` needs to read `vs.input` instead of `app.input`. Change its signature to take `&ViewState`:

```rust
pub fn send_input(&mut self, vs: &ViewState) {
    let text = vs.input.clone();
    if text.is_empty() { return; }
    // Route based on vs.mode / vs.active_thread
    // ...
}
```

`open_thread_via_broker` writes to the broker instead of mutating App fields:

```rust
pub async fn open_thread_via_broker(&mut self, client: &ClientHandle, thread_id: &str) {
    // Ensure thread stores are mounted (via thread_mount)
    // The ThreadView will be populated from broker in next frame's fetch_view_state
    let mut map = std::collections::BTreeMap::new();
    map.insert(
        "thread_id".to_string(),
        Value::String(thread_id.to_string()),
    );
    client
        .write(&path!("ui/open"), Record::parsed(Value::Map(map)))
        .await
        .ok();
}
```

- [ ] **Step 2: Remove sync_ui_to_app / sync_app_to_ui imports and calls**

Remove all references to `crate::state_sync` from tui.rs.

- [ ] **Step 3: Compile + test**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: Compiles. Tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-cli/src/tui.rs crates/ox-cli/src/app.rs
git commit -m "feat(ox-cli): rewrite event loop with ViewState fetch"
```

---

### Task 5: Shrink App + delete state_sync

Remove fields from App that are now read from the broker via ViewState. Delete state_sync.rs. Remove thread_views (replaced by streaming_turns).

**Files:**
- Modify: `crates/ox-cli/src/app.rs`
- Delete: `crates/ox-cli/src/state_sync.rs`
- Modify: `crates/ox-cli/src/main.rs` (remove `mod state_sync`)

- [ ] **Step 1: Remove dead fields from App**

In `crates/ox-cli/src/app.rs`, remove these fields from the App struct:

```rust
    // REMOVE all of these:
    pub active_thread: Option<String>,
    pub mode: InputMode,
    pub selected_row: usize,
    pub inbox_scroll: usize,
    pub cached_threads: Vec<(String, String, String, Vec<String>, i64, i64)>,
    pub input: String,
    pub cursor: usize,
    pub scroll: u16,
    pub last_content_height: usize,
    pub last_viewport_height: usize,
    pub should_quit: bool,
```

Remove their initialization in `App::new`.

- [ ] **Step 2: Replace thread_views with streaming_turns**

Remove `pub thread_views: HashMap<String, ThreadView>` from App.

The `streaming_turns` field (added in Task 2) is already there. Remove the old `handle_event` method (replaced by `drain_agent_events` + `handle_event_for_streaming`). Or keep handle_event but remove the parts that write to thread_views.

Actually, simplify: rename `drain_agent_events` to just drain both streaming_turns and side effects (inbox SQLite updates). Remove the old `handle_event` entirely. Remove the `handle_event_for_streaming` intermediate — merge into one method.

- [ ] **Step 3: Remove methods that mutated dead fields**

Remove or simplify:
- `refresh_visible_threads()` — no longer needed (inbox data from broker)
- `ensure_selected_visible()` — no longer needed (scroll managed by UiStore)
- `open_thread()` — replaced by `open_thread_via_broker`
- `open_selected_thread()` — replaced by ViewState-based open in event loop
- `get_visible_threads()` — no longer needed

Keep:
- `send_input()` (updated to take &ViewState in Task 4)
- `do_compose()` / `do_reply()` — still route through AgentPool
- `archive_selected_thread()` → `archive_thread_via_broker`
- `update_thread_state()` — still writes to inbox SQLite
- `history_up()` / `history_down()` — still manage input_history
- `active_thinking()` — adapt to read from streaming_turns instead of thread_views

- [ ] **Step 4: Delete state_sync.rs**

```bash
rm crates/ox-cli/src/state_sync.rs
```

In `crates/ox-cli/src/main.rs`, remove:
```rust
mod state_sync;
```

- [ ] **Step 5: Compile + test**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: Compiles. Tests pass.

- [ ] **Step 6: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: 14/14 pass.

- [ ] **Step 7: Commit**

```bash
git add -u
git commit -m "refactor(ox-cli): shrink App, delete state_sync, ViewState is sole draw source"
```

---

### Task 6: Update status document

**Files:**
- Modify: `docs/design/rfc/structfs-tui-status.md`

- [ ] **Step 1: Add C5 section**

Add under Phase C:

```markdown
#### C5: Draw Rewrite (complete, N tests)
- `crates/ox-cli/src/view_state.rs` — ViewState struct, fetch_view_state(), parse_chat_messages(),
  StreamingTurn (the only cache), InboxThread
- `crates/ox-cli/src/tui.rs` — event loop fetches ViewState per frame, draw is pure
- `crates/ox-cli/src/inbox_view.rs` — draw_inbox takes &ViewState
- `crates/ox-cli/src/tab_bar.rs` — draw_tabs takes &ViewState
- `crates/ox-cli/src/app.rs` — 11 fields removed, thread_views replaced by streaming_turns,
  handle_event replaced by drain_agent_events
- `crates/ox-cli/src/state_sync.rs` — deleted (replaced by ViewState fetch)
```

Update "What's Next" — remove Draw Rewrite, promote Events-through-broker.

- [ ] **Step 2: Commit**

```bash
git add docs/design/rfc/structfs-tui-status.md
git commit -m "docs: update status/handoff for C5 completion"
```

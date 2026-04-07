# Events Through Broker Design

**Date:** 2026-04-07
**Status:** Draft
**Depends on:** C5 (Draw Rewrite — complete)

## Overview

Replace the `mpsc::Sender<AppEvent>` channel between agent workers and the
TUI with broker writes. CliEffects writes streaming events to
`threads/{id}/history/turn/*` through the broker. The TUI reads them via
`fetch_view_state`. AppEvent, event_rx, thread_views, streaming_turns,
handle_event, drain_agent_events — all deleted.

After this change, the broker is the sole communication path between agent
workers and the TUI. No mpsc channels remain for agent data.

## Write Side: CliEffects

CliEffects currently holds `event_tx: mpsc::Sender<AppEvent>` and
`control_tx: mpsc::Sender<AppControl>`. Replace `event_tx` with a
`ClientHandle` + `tokio::runtime::Handle` for broker writes.

`control_tx` stays — the approval flow uses it to send permission
requests with a response channel. This is a fundamentally different
pattern (request-response with blocking) that doesn't map to broker
writes.

### Event Mapping

| Current (mpsc) | New (broker path) | Value |
|---|---|---|
| `Agent { TurnStart }` | `history/turn/thinking` | `true` |
| `Agent { TextDelta(text) }` | `history/turn/streaming` | text (appends) |
| `Agent { ToolCallStart { name } }` | `history/turn/tool` | `{name, status:"running"}` |
| `Agent { ToolCallResult { .. } }` | `history/turn/tool` | `null` |
| `Agent { TurnEnd }` | `history/turn/thinking` | `false` |
| `Agent { Error(e) }` | `history/append` + `history/turn/thinking` | error message, then `false` |
| `Usage { in, out }` | `history/turn/tokens` | `{in, out}` |
| `Done { result }` | `history/commit` | finalizes turn |
| `PolicyStats { stats }` | `inbox/threads/{id}` (unscoped write) | policy fields |
| `SaveComplete { .. }` | `inbox/threads/{id}` (unscoped write) | last_seq, last_hash, updated_at |

All `history/*` writes go through the scoped adapter (`threads/{id}/`
prefix). PolicyStats and SaveComplete write to the InboxStore at
`inbox/threads/{id}` using an **unscoped** client (since the scoped client
resolves to `threads/{id}/inbox/threads/{id}` which is wrong).

### CliEffects Fields

Remove:
- `event_tx: mpsc::Sender<AppEvent>` — replaced by broker writes

Add:
- `broker_client: ClientHandle` — unscoped, for inbox writes
- `rt_handle: tokio::runtime::Handle` — for block_on

The scoped adapter is not inside CliEffects — it's the HostStore's backend.
For `turn/*` writes, CliEffects calls `emit_event` which needs to write
through the HostStore's backend. But CliEffects doesn't have access to the
backend (it's a sibling field in HostStore, not accessible from effects).

**Solution:** CliEffects holds its own scoped ClientHandle for turn writes.
The worker creates two clients from the same broker:
1. Scoped adapter → HostStore backend (for kernel reads/writes)
2. Scoped ClientHandle → CliEffects (for event writes)

Both are scoped to `threads/{id}`, both resolve to the same stores.
The ClientHandle is Clone, so creating a second one is free.

For inbox writes (PolicyStats, SaveComplete), CliEffects uses the
unscoped `broker_client`.

### Streaming During Complete

During LLM streaming, CliEffects::complete() calls a streaming callback
for each text delta. Currently this sends `AppEvent::Agent { TextDelta }`.
Now it writes to `history/turn/streaming` through the scoped client:

```
fn on_text_delta(&self, text: &str) {
    self.rt_handle.block_on(
        self.scoped_client.write(
            &path!("history/turn/streaming"),
            Record::parsed(Value::String(text.to_string())),
        )
    ).ok();
}
```

This happens on the worker's OS thread (not a tokio task), so
`handle.block_on()` is correct (same as SyncClientAdapter).

### Commit Flow

When a turn completes (all tool calls resolved, final response streamed),
the agent kernel writes `history/commit`. HistoryProvider's commit handler:
1. Takes accumulated streaming text → creates committed assistant message
2. Clears all turn state
3. The next `history/messages` read returns committed messages only

If the turn errored, the worker writes the error to `history/append`
as an error message before committing.

After commit, the worker writes SaveComplete data directly to the
InboxStore via the unscoped broker client.

## Read Side: fetch_view_state

### Current

```rust
pub committed_messages: Vec<ChatMessage>,
pub thread_views: &'a HashMap<String, ThreadView>,
```

Draw code prefers thread_views (has streaming data), falls back to
committed_messages.

### After

```rust
pub messages: Vec<ChatMessage>,   // from history/messages (includes in-progress turn)
pub thinking: bool,                // from history/turn/thinking
pub tool_status: Option<(String, String)>,  // from history/turn/tool
pub turn_tokens: (u32, u32),       // from history/turn/tokens
```

`messages` comes from `threads/{id}/history/messages` — HistoryProvider
already appends in-progress turn text to the messages array when
`turn.is_active()`. So one read gets everything: committed + streaming.

Turn metadata (thinking, tool, tokens) is read separately for the
status bar and indicators.

No more `thread_views` reference. No more streaming cache. The broker
is the sole source.

### Conditional Reads (Thread Screen)

When `screen == "thread"` and `active_thread` is Some:

```
read threads/{id}/history/messages  → messages (committed + in-progress)
read threads/{id}/history/turn/thinking  → thinking indicator
read threads/{id}/history/turn/tool  → tool status
read threads/{id}/history/turn/tokens  → token counts
```

When `screen == "inbox"`:
- No turn reads needed
- Inbox thread list comes from `inbox/threads` as before
- For per-thread activity indicators (thinking badge in inbox), we could
  read turn/thinking per visible thread, but that's O(N) broker reads.
  For now, the inbox doesn't show live streaming status. This is acceptable
  because the user can't see streaming content from the inbox anyway.

## What Gets Deleted

### Types
- `AppEvent` enum (app.rs) — all 5 variants gone
- `ThreadView` struct (app.rs) — gone
- `StreamingTurn` struct (app.rs) — gone
- `StreamingTurn` struct (view_state.rs) — gone

### App Fields
- `event_rx: mpsc::Receiver<AppEvent>` — gone
- `thread_views: HashMap<String, ThreadView>` — gone
- `streaming_turns: HashMap<String, StreamingTurn>` — gone

### App Methods
- `handle_event()` — gone (130 lines)
- `drain_agent_events()` — gone
- `update_streaming()` — gone

### AgentPool / agent_worker
- `event_tx: mpsc::Sender<AppEvent>` parameter — removed from AgentPool,
  spawn_worker, agent_worker

### ViewState Fields
- `thread_views: &'a HashMap<String, ThreadView>` — removed
- `committed_messages: Vec<ChatMessage>` — renamed to `messages`

### Event Loop
- `app.drain_agent_events()` call — removed (was step 1 of loop)

## What Stays

- `control_tx` / `control_rx` / `AppControl` — approval flow is
  request-response with a blocking channel, structurally different
  from event streaming
- `ChatMessage` enum — still used as the rendering type, built from
  broker data by parse_chat_messages
- `InboxThread` struct — still used for inbox display
- `parse_chat_messages()` — still converts broker Values to ChatMessage

## App After This Change

```rust
pub struct App {
    pub pool: AgentPool,
    pub active_thread: Option<String>,
    pub mode: InputMode,
    pub search: SearchState,
    pub input: String,
    pub cursor: usize,
    pub model: String,
    pub provider: String,
    pub control_rx: mpsc::Receiver<AppControl>,
    pub input_history: Vec<String>,
    history_cursor: usize,
    input_draft: String,
    pub pending_approval: Option<ApprovalState>,
    pub pending_customize: Option<CustomizeState>,
}
```

No caches. No parallel state. No dual-writes.

## ViewState After This Change

```rust
pub struct ViewState<'a> {
    // UI state (from broker)
    pub screen: String,
    pub mode: String,
    pub active_thread: Option<String>,
    pub selected_row: usize,
    pub scroll: u16,
    pub scroll_max: u16,
    pub viewport_height: u16,
    pub input: String,
    pub cursor: usize,
    pub pending_action: Option<String>,

    // Inbox (from broker)
    pub inbox_threads: Vec<InboxThread>,

    // Thread (from broker — committed + in-progress, single source)
    pub messages: Vec<ChatMessage>,
    pub thinking: bool,
    pub tool_status: Option<(String, String)>,
    pub turn_tokens: (u32, u32),

    // From App (not yet in broker)
    pub search: &'a SearchState,
    pub input_history: &'a [String],
    pub model: &'a str,
    pub provider: &'a str,
    pub pending_approval: &'a Option<ApprovalState>,
    pub pending_customize: &'a Option<CustomizeState>,
    pub input_mode: &'a InputMode,
}
```

## Testing

### HistoryProvider turn integration (already tested)
The existing 16 tests in ox-history cover turn/streaming, turn/thinking,
turn/tool, turn/tokens, commit, and messages-includes-turn. No new
store-level tests needed.

### fetch_view_state with turn data
Mount HistoryProvider in test broker, write turn data, call
fetch_view_state, assert messages include streaming text and thinking
is true.

### CliEffects broker writes
Unit test: create CliEffects with a test ClientHandle, call
emit_event/complete, verify broker received the expected writes.
(May be difficult to test in isolation — the CliEffects is tightly
coupled to HTTP transport. Integration testing via the full
agent_worker flow is more practical.)

### End-to-end
Existing ox-cli tests pass (they don't exercise the agent loop).
Manual testing: run ox, compose a thread, verify streaming text
appears, verify commit produces stable messages on reload.

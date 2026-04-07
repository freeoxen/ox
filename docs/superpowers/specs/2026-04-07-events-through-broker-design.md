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
workers and the TUI. No mpsc channels remain — not for agent data, not for
approval flow. The `control_tx`/`control_rx` channels are also eliminated
by routing approval through the broker with deferred replies.

## Deferred Replies (Broker Enhancement)

The broker's server loop currently wraps synchronous `Reader`/`Writer`
stores: call `store.write()`, send the result immediately. For the
approval flow, a store's write needs to return a future that resolves
later — when a second write from a different client provides the answer.

### Async Reader/Writer Traits

```rust
/// Async version of Reader. Returns a future that resolves when the
/// read result is available.
pub trait AsyncReader: Send + 'static {
    fn read(
        &mut self,
        from: &Path,
    ) -> impl Future<Output = Result<Option<Record>, StoreError>> + Send;
}

/// Async version of Writer. Returns a future that resolves when the
/// write result is available. The future may resolve immediately (like
/// a normal sync write) or defer until external input arrives (like
/// approval/request waiting for approval/response).
pub trait AsyncWriter: Send + 'static {
    fn write(
        &mut self,
        to: &Path,
        data: Record,
    ) -> impl Future<Output = Result<Path, StoreError>> + Send;
}
```

### Async Server Loop

The async server loop calls store methods sequentially (with `&mut self`),
but spawns deferred write futures as independent tasks. The store produces
the future synchronously, the future resolves asynchronously without
holding the store. No mutex needed.

```rust
async fn async_server_loop<S: AsyncReader + AsyncWriter>(
    mut store: S,
    mut rx: tokio::sync::mpsc::Receiver<Request>,
) {
    while let Some(request) = rx.recv().await {
        match request {
            Request::Read { path, reply } => {
                // Reads are fast — resolve inline.
                let result = store.read(&path).await;
                let _ = reply.send(result);
            }
            Request::Write { path, data, reply } => {
                // Store produces the future synchronously (&mut self),
                // setting up internal state (e.g., creating a oneshot).
                // The returned future is 'static + Send — it doesn't
                // borrow the store. Spawn it as an independent task.
                let fut = store.write(&path, data);
                tokio::spawn(async move {
                    let result = fut.await;
                    let _ = reply.send(result);
                });
            }
        }
    }
}
```

**Key pattern:** `store.write()` takes `&mut self` and returns a future
that is detached from the store. For fast writes, the future resolves
immediately (like a sync write wrapped in `async { Ok(path) }`). For
deferred writes (approval/request), the store creates a `oneshot::channel`,
stores the sender in its own state, and returns a future that awaits the
receiver. The future doesn't borrow the store — it owns the receiver.

This means:
- No mutex on the store
- The server loop stays sequential for store access
- Deferred futures run independently as spawned tasks
- Reads and other writes proceed normally while a deferred write is pending

### BrokerStore::mount_async

```rust
pub async fn mount_async<S: AsyncReader + AsyncWriter>(
    &self,
    prefix: Path,
    store: S,
) -> JoinHandle<()>
```

Mounts a store whose reads and writes are async. Used for stores that
need deferred replies (ApprovalStore). Regular sync stores use `mount()`
as before — the existing sync server loop is unchanged.

### ApprovalStore with Deferred Write

ApprovalStore implements `AsyncReader + AsyncWriter`. Its write to
`request` returns a future that doesn't resolve until a separate write
to `response` arrives.

The mechanism:

1. **`write("request", data)`** — store creates a `oneshot::channel()`.
   Stores the sender and request data in its own state. Returns a
   future that awaits the receiver. The future is `'static + Send` —
   it owns the receiver, doesn't borrow the store.

2. **`read("pending")`** — returns the stored request data (or null).
   Fast, non-blocking.

3. **`write("response", data)`** — reads the stored oneshot sender,
   calls `sender.send(decision)`. The spawned future from step 1
   resolves. Clears the pending request. Returns immediately.

```
Agent writes approval/request:
  → store.write("request") creates oneshot, stores sender, returns future
  → Server loop spawns the future as a task
  → Server loop continues (can handle reads and response writes)
  → Agent's client.write() blocks (SyncClientAdapter.block_on → future)

TUI reads approval/pending:
  → store.read("pending") returns request data (fast, inline)

TUI writes approval/response:
  → store.write("response") sends on oneshot sender, returns immediately
  → Spawned future from request resolves with the decision
  → Broker sends reply to agent's client
  → Agent's blocked write returns with the decision

Timeout:
  → Client-side timeout fires (broker default, configurable per-thread)
  → Agent treats timeout as denial
  → Oneshot receiver is dropped, sender.send() in future response
    would be a no-op
```

No mutex. The server loop calls store methods sequentially. The only
concurrency is between the server loop and the spawned future, which
communicates through the oneshot channel (no shared mutable state).

## Write Side: CliEffects

CliEffects currently holds `event_tx: mpsc::Sender<AppEvent>` and
`control_tx: mpsc::Sender<AppControl>`. Both are replaced by broker
writes through a `ClientHandle` + `tokio::runtime::Handle`.

For streaming events: write to `history/turn/*` through the scoped client.
For approval: write to `approval/request` through the scoped client —
the async ApprovalStore defers the reply until the TUI writes
`approval/response`, so the agent's write blocks until the user decides.

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
- `control_tx: mpsc::Sender<AppControl>` — replaced by approval through broker

Add:
- `broker_client: ClientHandle` — unscoped, for inbox writes
- `scoped_client: ClientHandle` — scoped to thread, for turn + approval writes
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
- `control_rx: mpsc::Receiver<AppControl>` — gone (approval through broker)
- `thread_views: HashMap<String, ThreadView>` — gone
- `streaming_turns: HashMap<String, StreamingTurn>` — gone
- `pending_approval: Option<ApprovalState>` — gone (read from broker)
- `pending_customize: Option<CustomizeState>` — gone (read from broker)

### App Methods
- `handle_event()` — gone (130 lines)
- `drain_agent_events()` — gone
- `update_streaming()` — gone

### AgentPool / agent_worker
- `event_tx: mpsc::Sender<AppEvent>` parameter — removed
- `control_tx: mpsc::Sender<AppControl>` parameter — removed

### Types
- `AppControl` enum — gone
- `ApprovalResponse` enum — replaced by broker Value writes

### ViewState Fields
- `thread_views: &'a HashMap<String, ThreadView>` — removed
- `committed_messages: Vec<ChatMessage>` — renamed to `messages`
- `pending_approval: &'a Option<ApprovalState>` — replaced by broker read
- `pending_customize: &'a Option<CustomizeState>` — replaced by broker read

### Event Loop
- `app.drain_agent_events()` call — removed
- `drain_control_rx()` — removed (approval read from broker via ViewState)

## What Stays

- `ChatMessage` enum — rendering type, built from broker data
- `InboxThread` struct — inbox display
- `parse_chat_messages()` — converts broker Values to ChatMessage
- `ApprovalState` / `CustomizeState` — still the rendering types for
  dialogs, but now built from broker data in fetch_view_state instead
  of stored in App

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
    pub input_history: Vec<String>,
    history_cursor: usize,
    input_draft: String,
}
```

11 fields. No mpsc channels. No caches. No dialog state. No parallel
state. No dual-writes.

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

    // Approval (from broker)
    pub approval_pending: Option<ApprovalRequest>,

    // From App (not yet in broker)
    pub search: &'a SearchState,
    pub input_history: &'a [String],
    pub model: &'a str,
    pub provider: &'a str,
    pub input_mode: &'a InputMode,
}
```

## Testing

### Async store support (broker)
Test that `mount_async` works: mount an async store, verify reads and
writes resolve correctly. Test deferred write: mount a store whose
write returns a future that resolves after a second write. Verify
the first client blocks and the second client's write unblocks it.

### ApprovalStore async integration
Mount ApprovalStore via `mount_async`. From one client, write
`approval/request`. From another client (in a separate task), write
`approval/response`. Verify the first write resolves with the
response data. Test timeout behavior.

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
Integration testing via the full agent_worker flow is more practical
for transport-coupled code.

### End-to-end
Existing ox-cli tests pass (they don't exercise the agent loop).
Manual testing: run ox, compose a thread, verify streaming text
appears, tool approval dialog works through broker, commit produces
stable messages on reload.

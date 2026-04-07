# Draw Rewrite Design

**Date:** 2026-04-07
**Status:** Draft
**Depends on:** C4 (Agent Worker Bridge — complete)

## Overview

Replace the TUI's draw-from-App-fields pattern with a per-frame `ViewState`
snapshot fetched from the broker. Draw functions become pure: `ViewState` in,
ratatui `Frame` out. The broker is the sole source of truth for all rendered
state. `state_sync.rs` is deleted. The `App` struct shrinks to agent pool,
event channels, streaming cache, and dialog state.

## ViewState

A plain struct assembled by one async function each frame. No caching, no
invalidation — a fresh read snapshot every frame.

### Fields

```
ViewState
├── screen: String              ← ui/screen
├── mode: String                ← ui/mode
├── active_thread: Option<String> ← ui/active_thread
├── selected_row: usize         ← ui/selected_row
├── scroll: usize               ← ui/scroll
├── input: String               ← ui/input
├── cursor: usize               ← ui/cursor
├── modal: Option<Value>        ← ui/modal
├── pending_action: Option<String> ← ui/pending_action
├── scroll_max: usize           ← ui/scroll_max
├── viewport_height: usize      ← ui/viewport_height
│
├── inbox_threads: Vec<Value>   ← inbox/threads (or inbox/search/{query})
│
├── thread_messages: Vec<Value> ← threads/{id}/history/messages (committed)
├── thread_message_count: i64   ← threads/{id}/history/count
├── streaming: Option<StreamingTurn> ← from App's streaming cache (not broker)
│
├── approval_pending: Option<Value> ← approval/pending
│
├── search: SearchSnapshot      ← from App's SearchState (not broker yet)
├── input_history: Vec<String>  ← from App (local, not in any store)
└── provider: String            ← from App (display metadata)
    model: String               ← from App (display metadata)
```

### Fetch function

```rust
pub async fn fetch_view_state(
    client: &ClientHandle,
    app: &App,  // for streaming cache, search, input_history, model/provider
) -> ViewState
```

Reads broker paths, merges with App's remaining live state (streaming cache,
search, input history, display metadata). Returns a complete snapshot.

The function reads UI state in a single `client.read(&path!("ui"))` call
(UiStore returns all fields as a Map), then conditionally reads thread and
inbox paths based on the screen value.

### Conditional reads

Not every frame needs every path:

- **Inbox screen**: read `inbox/threads` (or `inbox/search/{query}` if search
  is active). Skip thread paths.
- **Thread screen**: read `threads/{id}/history/messages` and
  `threads/{id}/history/count`. Skip inbox paths.
- **Compose screen**: read only UI state. Skip inbox and thread paths.

This keeps per-frame broker traffic proportional to what's visible.

## Draw Functions

### Current signatures

```rust
fn draw_inbox(f: &mut Frame, app: &App, theme: &Theme, area: Rect)
fn draw_thread(f: &mut Frame, app: &App, theme: &Theme, area: Rect)
fn draw_tab_bar(f: &mut Frame, app: &App, theme: &Theme, area: Rect)
```

### New signatures

```rust
fn draw_inbox(f: &mut Frame, vs: &ViewState, theme: &Theme, area: Rect)
fn draw_thread(f: &mut Frame, vs: &ViewState, theme: &Theme, area: Rect)
fn draw_tab_bar(f: &mut Frame, vs: &ViewState, theme: &Theme, area: Rect)
```

Pure functions. No `&mut`. No side effects. Testable with a constructed
ViewState — no broker needed for draw tests.

### Thread content rendering

For the active thread, ViewState holds committed messages from the broker
and an optional `StreamingTurn` from the cache. Draw merges them:

1. Render committed messages from `thread_messages`
2. If `streaming` is Some, append the in-progress turn at the bottom:
   - Accumulated text (partial assistant response)
   - Tool call status badge if active
   - Thinking indicator if mid-turn

This is the same visual result as today but with a clear authority boundary:
broker for committed, cache for in-flight.

## StreamingTurn (the honest cache)

A minimal struct that accumulates in-progress turn state from `AppEvent`:

```rust
pub struct StreamingTurn {
    pub text: String,           // accumulated TextDelta
    pub tool: Option<ToolStatus>, // current tool call name + status
    pub thinking: bool,         // agent is mid-turn
    pub input_tokens: u32,
    pub output_tokens: u32,
}
```

Populated by `drain_agent_events()` each frame from `app.event_rx`.
Cleared when `AppEvent::Done` arrives for the thread (the turn is committed
to history in the broker at that point).

This cache exists because agent streaming events still flow through mpsc,
not through the broker. It has an explicit expiration date: the
Events-through-broker phase moves streaming into `threads/{id}/history/turn/*`
paths, and this cache disappears.

## Event Loop

```
loop {
    // 1. Drain agent events → update streaming cache
    drain_agent_events(&mut app);

    // 2. Handle control events (approval requests)
    drain_control_events(&mut app);

    // 3. Fetch ViewState from broker + app's live state
    let vs = fetch_view_state(&client, &app).await;

    // 4. Render (sync, pure)
    terminal.draw(|f| draw(f, &vs, &theme))?;

    // 5. Update scroll_max from rendered content height
    //    (write to broker: ui/set_scroll_max, ui/set_viewport_height)
    update_scroll_bounds(&client, &vs, &terminal).await;

    // 6. Poll terminal event (with timeout)
    if let Some(event) = poll_event(Duration::from_millis(50))? {
        handle_terminal_event(&mut app, &client, &vs, event).await;
    }

    // 7. Handle pending_action from ViewState
    if let Some(action) = vs.pending_action {
        handle_action(&mut app, &client, &action).await;
    }

    // 8. Check quit
    if vs.pending_action.as_deref() == Some("quit") {
        break;
    }
}
```

### drain_agent_events

Replaces `app.handle_event()`. Reads from `app.event_rx`, updates:
- `app.streaming_turns[thread_id]` — accumulates text/tool/token state
- `app.pending_approval` — from AppControl channel
- SQLite write-through on SaveComplete

Does NOT update any UI fields (those are in the broker now).

### handle_action

Replaces the current pending_action handling in tui.rs. Actions from
UiStore (send_input, quit, open_selected, archive_selected) trigger
App methods that interact with AgentPool and broker:

- `send_input` → read input from vs, route to compose/reply via AgentPool
- `open_selected` → write to broker `ui/open` with thread_id
- `archive_selected` → write to broker `inbox/archive`
- `quit` → break the loop

## App Struct Changes

### Fields removed (read from broker via ViewState)

- `mode: InputMode`
- `active_thread: Option<String>`
- `selected_row: usize`
- `inbox_scroll: usize`
- `scroll: usize`
- `input: String`
- `cursor: usize`
- `cached_threads: Vec<...>`
- `last_content_height: usize`
- `last_viewport_height: usize`
- `should_quit: bool`

### Fields retained

- `pool: AgentPool` — agent lifecycle
- `event_rx: mpsc::Receiver<AppEvent>` — agent events (until Events-through-broker)
- `control_rx: mpsc::Receiver<AppControl>` — approval requests
- `streaming_turns: HashMap<String, StreamingTurn>` — in-progress turn cache
  (replaces `thread_views: HashMap<String, ThreadView>`)
- `pending_approval: Option<ApprovalState>` — dialog state machine
- `pending_customize: Option<CustomizeState>` — dialog state machine
- `search: SearchState` — filter state (until Search-in-UiStore)
- `input_history: Vec<String>` — input recall
- `history_cursor: usize` — input recall position
- `input_draft: String` — input recall draft
- `model: String` — display metadata
- `provider: String` — display metadata

### Fields converted

- `thread_views: HashMap<String, ThreadView>` →
  `streaming_turns: HashMap<String, StreamingTurn>`
  ThreadView held cached messages + streaming state. Messages now come from
  the broker. Only the streaming accumulator remains.

## Files Deleted

- `crates/ox-cli/src/state_sync.rs` — entirely replaced by ViewState fetch

## Files Created

- `crates/ox-cli/src/view_state.rs` — ViewState struct + `fetch_view_state()`

## Files Modified

- `crates/ox-cli/src/tui.rs` — event loop rewrite: fetch ViewState, pass to draw
- `crates/ox-cli/src/app.rs` — remove dead fields, replace thread_views with
  streaming_turns, simplify handle_event to drain_agent_events
- `crates/ox-cli/src/inbox_view.rs` — draw_inbox takes &ViewState
- `crates/ox-cli/src/thread_view.rs` — draw_thread takes &ViewState
- `crates/ox-cli/src/tab_bar.rs` — draw_tab_bar takes &ViewState
- `crates/ox-cli/src/main.rs` — minor wiring changes

## Testing

### ViewState fetch (integration)

Mount stores in a test broker, write known state, call `fetch_view_state`,
assert fields match. Test conditional reads: inbox screen skips thread paths,
thread screen skips inbox paths.

### Draw functions (unit)

Construct ViewState directly (no broker), call draw functions with a test
terminal backend, assert rendered content. This is the main benefit of the
ViewState pattern — draw is testable without infrastructure.

### Event loop (integration)

Existing broker_setup tests continue to verify store mounting and key
dispatch. The event loop changes are structural — if it compiles and the
existing tests pass, the wiring is correct.

## What This Enables

After the draw rewrite lands:

- **Events-through-broker** becomes a scoped change: CliEffects writes to
  `turn/*` paths, `fetch_view_state` reads them, StreamingTurn cache and
  `event_rx` disappear. Small diff, big architectural payoff.

- **Search-in-UiStore** becomes trivial: move SearchState fields into
  UiStore, `fetch_view_state` reads them, SearchState removed from App.

- **ConfigStore** slots in naturally: mount at `config/`, ViewState reads
  theme/provider/model from it.

Each subsequent phase removes one more field group from App and one more
conditional read path from ViewState. The architecture converges monotonically
toward "everything is a broker path."

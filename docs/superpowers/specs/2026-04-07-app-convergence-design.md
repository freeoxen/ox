# Phase 1: App Convergence

**Date:** 2026-04-07
**Status:** Draft
**Prereqs:** C1-C7 complete (BrokerStore, Core Stores, Broker Wiring, Async Event Loop, Agent Worker Bridge, Draw Rewrite, Events Through Broker, ThreadRegistry)

## Overview

Three bundled changes that eliminate the remaining state duplication
between App and UiStore, remove the last direct-mutation escape hatch
(search), and split the 1258-line tui.rs into focused modules.

After this, App holds only session-scoped concerns (AgentPool, input
history, model/provider, dialog state). All UI state lives in UiStore
and is read through the broker via ViewState.

## Step 1: Search State in UiStore

### Current State

`SearchState` in `app.rs` owns `chips: Vec<String>` and
`live_query: String`. The `handle_search_key` function in `tui.rs`
(lines 461-471) directly mutates `app.search`, bypassing the broker.
ViewState borrows `&app.search`. This is the last direct-mutation
escape hatch noted in the status doc.

### Changes

#### UiStore additions

New fields:
- `search_chips: Vec<String>` — committed filter terms
- `search_live_query: String` — in-progress search text

New read paths:
- `search_chips` -> `Value::Array` of `Value::String`
- `search_live_query` -> `Value::String`
- `search_active` -> `Value::Bool` (derived: chips non-empty OR live_query non-empty)

These fields are also included in the `all_fields_map()` response.

New write commands:
- `search_insert_char` — `{"char": "x"}` appends to live_query
- `search_delete_char` — backspace on live_query (pop last char)
- `search_clear` — clears live_query entirely
- `search_save_chip` — commits live_query as a chip (trims, skips empty)
- `search_dismiss_chip` — `{"index": N}` removes chip at index

#### ViewState changes

Replace `search: &'a SearchState` with owned fields read from broker:
- `search_chips: Vec<String>`
- `search_live_query: String`
- `search_active: bool`

`fetch_view_state` reads these from the `ui` map returned by the
broker (they're in `all_fields_map`).

#### Event loop changes

Delete `handle_search_key` from `tui.rs`.

Search text editing in insert/search mode routes through the broker:
- When InputStore returns an error (no binding) and insert_context is
  "search", dispatch to `ui/search_insert_char`, `ui/search_delete_char`,
  `ui/search_clear`, or `ui/search_save_chip` based on the key.
- Chip dismissal (1-9 in normal mode on inbox with search active)
  becomes a broker write to `ui/search_dismiss_chip`.

#### Deletions

- `SearchState` struct removed from `app.rs`
- `App.search` field removed
- `handle_search_key` function removed from `tui.rs`

#### Filter predicate

The `SearchState::matches()` logic becomes a standalone function in
`view_state.rs` operating on `&[String]` chips + `&str` live_query +
thread fields. It's a pure predicate used during inbox thread display
to filter the list.

## Step 2: Remove Duplicate App Fields

### Current State

App duplicates four fields that UiStore owns:
- `active_thread: Option<String>` — synced to broker via `ui/open`
- `mode: InputMode` — synced via `sync_mode_to_broker`
- `input: String` — synced via `ui/set_input`
- `cursor: usize` — synced via `ui/set_input`

These create bidirectional sync complexity and bugs when they diverge.

### Changes

#### Remove from App

Delete fields: `active_thread`, `mode`, `input`, `cursor`.

#### Refactor send_input_with_text

Current signature: `pub fn send_input_with_text(&mut self, text: String)`
reads `self.mode`, `self.input`, `self.active_thread`.

New signature:
```rust
pub fn send_input_with_text(
    &mut self,
    text: String,
    mode: &str,
    insert_context: Option<&str>,
    active_thread: Option<&str>,
)
```

All parameters come from ViewState, extracted before calling.

#### Refactor do_compose / do_reply

These methods currently read `self.input` and write `self.active_thread`.

- `do_compose` takes `input: String`, returns `Option<String>` (new
  thread_id on success). Does not set `self.active_thread` — caller
  writes `ui/open` through broker.
- `do_reply` takes `input: String, thread_id: &str`. Does not read
  `self.active_thread`.

Both still manage input_history (push, reset cursor, clear draft).

#### Delete open_thread

`App::open_thread` is deleted. Thread opening is handled entirely
through the broker via `ui/open` command to UiStore.

#### Delete sync_mode_to_broker

No App mode to sync. All mode transitions happen through UiStore
commands (`enter_insert`, `exit_insert`). The event loop writes
these directly after `send_input_with_text` returns.

#### Delete InputMode / InsertContext from app.rs

The `InputMode` and `InsertContext` enums in `app.rs` are no longer
needed by App. However, they're still used by `draw` and `draw_status_bar`
for pattern matching on rendering behavior. Two options:

**Chosen approach:** ViewState carries `mode: String` and
`insert_context: Option<String>` (already does). Draw functions
match on string values instead of enums. This avoids importing
UiStore enums into the rendering layer and keeps ViewState as the
only interface between broker and rendering.

Remove `InputMode`, `InsertContext` enums from `app.rs`. Remove
`input_mode: &'a InputMode` from ViewState. Add
`insert_context: Option<String>` to ViewState (string from broker).

#### Resulting App struct

```rust
pub struct App {
    pub pool: AgentPool,
    pub model: String,
    pub provider: String,
    pub input_history: Vec<String>,
    history_cursor: usize,
    input_draft: String,
    pub approval_selected: usize,
    pub pending_customize: Option<CustomizeState>,
}
```

8 fields. The approval/customize fields are dialog state machines
that don't duplicate broker state. They're candidates for a later
pass but not causing problems now.

## Step 3: Split tui.rs

### Current Structure (1258 lines)

| Lines | Content |
|-------|---------|
| 1-262 | `run_async` event loop, `send_approval_response`, `sync_mode_to_broker` |
| 263-400 | `dispatch_text_edit_owned`, `dispatch_mouse_owned` |
| 401-471 | `handle_search_key` (deleted in step 1) |
| 472-585 | `handle_approval_key` |
| 586-700 | `draw` (main composed view) |
| 700-733 | `draw_status_bar` |
| 734-800 | `draw_approval_dialog` |
| 800-1115 | customize dialog key handler + builders |
| 1116-1258 | `draw_customize_dialog` |

### New File Layout

**`event_loop.rs`** (~280 lines)
- `pub async fn run_async(...)` — the main loop
- `async fn send_approval_response(...)` — broker write helper
- `async fn dispatch_text_edit_owned(...)` — insert mode text editing
- `async fn dispatch_mouse_owned(...)` — mouse scroll/click routing

**`key_handlers.rs`** (~180 lines)
- `pub async fn handle_approval_key(...)` — approval dialog navigation
- `pub async fn handle_customize_key(...)` — customize dialog navigation
- `fn infer_args(...)` — decompose tool call into editable args

**`dialogs.rs`** (~280 lines)
- `pub fn draw_approval_dialog(...)` — approval overlay rendering
- `pub fn draw_customize_dialog(...)` — customize overlay rendering
- `fn build_node_from_customize(...)` — clash Node builder
- `fn build_sandbox_from_customize(...)` — sandbox builder
- Constants: `EFFECTS`, `SCOPES`, `NETWORKS`

**`tui.rs`** (~140 lines, retained as rendering entry point)
- `pub fn draw(...)` — main composed view (layout, dispatch to sub-views)
- `fn draw_status_bar(...)` — status line rendering

### Module Visibility

- `event_loop` is the public entry point (`run_async` called from `main.rs`)
- `tui` is `pub(crate)` (draw called from event_loop)
- `key_handlers` is `pub(crate)` (called from event_loop)
- `dialogs` is `pub(crate)` (called from tui::draw and key_handlers)

### Import Changes in lib.rs / main.rs

`main.rs` changes from `crate::tui::run_async` to
`crate::event_loop::run_async`. No other external interface changes.

## Execution Order

1. **Step 1** (search) first — eliminates the last `app.mode` dependency
   in the direct-mutation path.
2. **Step 2** (remove fields) second — requires step 1 complete because
   search was the last consumer of `app.mode` as `InputMode::Insert(Search)`.
3. **Step 3** (split) last — pure file reorganization, easier after
   logic changes land.

## Testing

- **Step 1:** New UiStore unit tests for all search commands. Existing
  ox-cli integration tests must still pass.
- **Step 2:** Verify `cargo check` and `cargo test` pass. No new tests
  needed — this is removing duplication, not adding behavior.
- **Step 3:** `cargo check` and `cargo test` — pure refactor, no
  behavior change.

Quality gate: `./scripts/quality_gates.sh` passes after each step.

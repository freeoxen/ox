# S-Tier Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate command boilerplate, move types to correct modules, extract dialog state from App, split view_state.rs — bringing implementation quality up to match the architecture.

**Architecture:** cmd! macro for one-line broker commands. types.rs for rendering/dialog types. DialogState local to event loop instead of App. parse.rs for message/inbox parsing extracted from view_state.rs.

**Tech Stack:** Rust, structfs-core-store, ox-broker, ratatui, crossterm

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/ox-cli/src/broker_cmd.rs` | Create | `cmd!` macro + `IntoValue` trait |
| `crates/ox-cli/src/types.rs` | Create | ChatMessage, ThreadView, APPROVAL_OPTIONS, CustomizeState, FsRuleState |
| `crates/ox-cli/src/parse.rs` | Create | parse_chat_messages, parse_inbox_threads, search_matches + tests |
| `crates/ox-cli/src/event_loop.rs` | Modify | Use cmd!, add DialogState, remove App dialog fields usage |
| `crates/ox-cli/src/key_handlers.rs` | Modify | Use cmd!, take &mut DialogState instead of &mut App |
| `crates/ox-cli/src/tui.rs` | Modify | Import from types.rs, use DialogState refs |
| `crates/ox-cli/src/app.rs` | Modify | Remove types + dialog fields, import from types.rs |
| `crates/ox-cli/src/view_state.rs` | Modify | Extract parsing, import from parse.rs + types.rs |
| `crates/ox-cli/src/main.rs` | Modify | Add module declarations |
| Various consumers | Modify | Update imports from app:: to types:: |

---

### Task 1: Create cmd! Macro

**Files:**
- Create: `crates/ox-cli/src/broker_cmd.rs`
- Modify: `crates/ox-cli/src/main.rs`

- [ ] **Step 1: Create broker_cmd.rs with the macro and trait**

Create `crates/ox-cli/src/broker_cmd.rs`:

```rust
//! Command construction helper for broker writes.
//!
//! The `cmd!` macro builds a `Record::parsed(Value::Map(...))` in one line,
//! eliminating the 3-5 line BTreeMap boilerplate at every broker write site.

use structfs_core_store::{Record, Value};

/// Trait for converting Rust values into StructFS Values.
pub trait IntoValue {
    fn into_value(self) -> Value;
}

impl IntoValue for &str {
    fn into_value(self) -> Value {
        Value::String(self.to_string())
    }
}

impl IntoValue for String {
    fn into_value(self) -> Value {
        Value::String(self)
    }
}

impl IntoValue for &String {
    fn into_value(self) -> Value {
        Value::String(self.clone())
    }
}

impl IntoValue for i64 {
    fn into_value(self) -> Value {
        Value::Integer(self)
    }
}

impl IntoValue for usize {
    fn into_value(self) -> Value {
        Value::Integer(self as i64)
    }
}

impl IntoValue for bool {
    fn into_value(self) -> Value {
        Value::Bool(self)
    }
}

impl IntoValue for Value {
    fn into_value(self) -> Value {
        self
    }
}

/// Helper function used by the cmd! macro to convert values.
pub fn into_value(v: impl IntoValue) -> Value {
    v.into_value()
}

/// Build a broker command Record from key-value pairs.
///
/// ```ignore
/// cmd!()                           // empty command
/// cmd!("key" => "value")           // single field
/// cmd!("a" => 1, "b" => "two")    // multiple fields
/// ```
#[macro_export]
macro_rules! cmd {
    () => {
        structfs_core_store::Record::parsed(
            structfs_core_store::Value::Map(std::collections::BTreeMap::new()),
        )
    };
    ($($key:expr => $val:expr),+ $(,)?) => {{
        let mut map = std::collections::BTreeMap::new();
        $(map.insert($key.to_string(), $crate::broker_cmd::into_value($val));)+
        structfs_core_store::Record::parsed(structfs_core_store::Value::Map(map))
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_empty() {
        let record = cmd!();
        let val = record.as_value().unwrap();
        assert!(matches!(val, Value::Map(m) if m.is_empty()));
    }

    #[test]
    fn cmd_single_string() {
        let record = cmd!("name" => "alice");
        let val = record.as_value().unwrap();
        match val {
            Value::Map(m) => {
                assert_eq!(m.get("name"), Some(&Value::String("alice".into())));
            }
            _ => panic!("expected Map"),
        }
    }

    #[test]
    fn cmd_multiple_types() {
        let record = cmd!("text" => "hello", "cursor" => 5_usize, "flag" => true);
        let val = record.as_value().unwrap();
        match val {
            Value::Map(m) => {
                assert_eq!(m.get("text"), Some(&Value::String("hello".into())));
                assert_eq!(m.get("cursor"), Some(&Value::Integer(5)));
                assert_eq!(m.get("flag"), Some(&Value::Bool(true)));
            }
            _ => panic!("expected Map"),
        }
    }

    #[test]
    fn cmd_owned_string() {
        let s = String::from("owned");
        let record = cmd!("key" => s);
        let val = record.as_value().unwrap();
        match val {
            Value::Map(m) => assert_eq!(m.get("key"), Some(&Value::String("owned".into()))),
            _ => panic!("expected Map"),
        }
    }
}
```

- [ ] **Step 2: Add module declaration to main.rs**

In `crates/ox-cli/src/main.rs`, add after `mod broker_setup;`:

```rust
#[macro_use]
mod broker_cmd;
```

The `#[macro_use]` makes the `cmd!` macro available to all other modules in the crate.

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-cli -- broker_cmd`
Expected: 4 tests pass.

- [ ] **Step 4: Apply cmd! to event_loop.rs**

Replace all BTreeMap command constructions in `crates/ox-cli/src/event_loop.rs`.

Remove `use std::collections::BTreeMap;` from the top-level import block inside `run_async` (line 22). Keep it only where still needed (the `event_map` for InputStore dispatch at line 225 still uses BTreeMap directly since it builds 3 fields dynamically — that's fine, or convert it too).

Key replacements in `run_async`:

```rust
// Line 54-58: set_row_count
let _ = client.write(&path!("ui/set_row_count"), cmd!("count" => row_count)).await;

// Line 71-75: set_scroll_max
let _ = client.write(&path!("ui/set_scroll_max"), cmd!("max" => scroll_max.max(0))).await;

// Line 77-84: set_viewport_height
let _ = client.write(&path!("ui/set_viewport_height"), cmd!("height" => viewport_height as i64)).await;

// Line 114-118: clear_input
let _ = client.write(&path!("ui/clear_input"), cmd!()).await;

// Line 120-124: exit_insert
let _ = client.write(&path!("ui/exit_insert"), cmd!()).await;

// Line 128-132: open thread
let _ = client.write(&path!("ui/open"), cmd!("thread_id" => tid)).await;

// Line 138-142: open_selected
let _ = client.write(&path!("ui/open"), cmd!("thread_id" => id.clone())).await;

// Line 151-155: archive
// (This one uses app.pool.inbox().write — different API, keep BTreeMap)

// Line 162-167: clear_pending_action
let _ = client.write(&path!("ui/clear_pending_action"), cmd!()).await;

// Line 213-219: search_dismiss_chip
let _ = client.write(&path!("ui/search_dismiss_chip"), cmd!("index" => idx as i64)).await;

// Line 225-228: InputStore event_map
let _ = client.write(&path!("input/key"), cmd!("mode" => mode, "key" => key_str.clone(), "screen" => screen)).await;

// Line 242-253: history_up set_input
let _ = client.write(&path!("ui/set_input"), cmd!("text" => text, "cursor" => cursor as i64)).await;

// Line 258-269: history_down set_input
let _ = client.write(&path!("ui/set_input"), cmd!("text" => text, "cursor" => cursor as i64)).await;
```

Apply to `dispatch_text_edit_owned`:

```rust
// Ctrl+A
let _ = client.write(&path!("ui/set_input"), cmd!("cursor" => 0_i64)).await;

// Ctrl+E
let _ = client.write(&path!("ui/set_input"), cmd!("cursor" => input_len as i64)).await;

// Char insert
let _ = client.write(&path!("ui/insert_char"), cmd!("char" => c.to_string(), "at" => cursor as i64)).await;

// Enter
let _ = client.write(&path!("ui/insert_char"), cmd!("char" => "\n", "at" => cursor as i64)).await;

// Backspace
let _ = client.write(&path!("ui/delete_char"), cmd!()).await;

// Left
let _ = client.write(&path!("ui/set_input"), cmd!("cursor" => pos as i64)).await;

// Right
let _ = client.write(&path!("ui/set_input"), cmd!("cursor" => pos as i64)).await;
```

Apply to `dispatch_mouse_owned`:

```rust
// All 4 empty commands:
let _ = client.write(&path!("ui/scroll_up"), cmd!()).await;
let _ = client.write(&path!("ui/select_prev"), cmd!()).await;
let _ = client.write(&path!("ui/scroll_down"), cmd!()).await;
let _ = client.write(&path!("ui/select_next"), cmd!()).await;
```

Apply to `dispatch_search_edit`:

```rust
let _ = client.write(&path!("ui/search_save_chip"), cmd!()).await;
let _ = client.write(&path!("ui/search_clear"), cmd!()).await;
let _ = client.write(&path!("ui/search_delete_char"), cmd!()).await;
let _ = client.write(&path!("ui/search_insert_char"), cmd!("char" => c.to_string())).await;
```

Remove all `use std::collections::BTreeMap;` and `use structfs_core_store::{Record, Value, path};` from inside the helper functions — they now only need `use structfs_core_store::path;` (the `path!` macro). The `cmd!` macro handles Record/Value internally.

- [ ] **Step 5: Verify compilation and tests**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: Compiles, 61+ tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/broker_cmd.rs crates/ox-cli/src/event_loop.rs crates/ox-cli/src/main.rs
git commit -m "feat(ox-cli): add cmd! macro, eliminate broker command boilerplate"
```

---

### Task 2: Extract Rendering Types to types.rs

**Files:**
- Create: `crates/ox-cli/src/types.rs`
- Modify: `crates/ox-cli/src/app.rs`
- Modify: `crates/ox-cli/src/main.rs`
- Modify: all files importing from `crate::app::` for these types

- [ ] **Step 1: Create types.rs**

Create `crates/ox-cli/src/types.rs` by moving from `app.rs`:
- `ThreadView` struct + derive
- `ChatMessage` enum + derive
- `APPROVAL_OPTIONS` const
- `CustomizeState` struct + impl block
- `FsRuleState` struct

The file should have no imports beyond standard library — these types are plain data.

- [ ] **Step 2: Update app.rs**

Remove the moved types from `app.rs`. Add import:

```rust
use crate::types::CustomizeState;
```

(App no longer references ChatMessage, ThreadView, APPROVAL_OPTIONS, or FsRuleState directly.)

- [ ] **Step 3: Add module declaration to main.rs**

Add `mod types;` to main.rs.

- [ ] **Step 4: Update all consumers**

Find and replace imports across the crate. Files that import from `crate::app::`:

- `event_loop.rs`: `APPROVAL_OPTIONS` → `crate::types::APPROVAL_OPTIONS`
- `key_handlers.rs`: `APPROVAL_OPTIONS, App` → `APPROVAL_OPTIONS` from `crate::types`, keep `App` from `crate::app`. Also `crate::app::CustomizeState` → `crate::types::CustomizeState`, `crate::app::FsRuleState` → `crate::types::FsRuleState`
- `view_state.rs`: `ChatMessage, CustomizeState` → from `crate::types`
- `tui.rs`: `crate::app::ThreadView` → `crate::types::ThreadView`
- `thread_view.rs`: `crate::app::{ChatMessage, ThreadView}` → `crate::types::{ChatMessage, ThreadView}`
- `tab_bar.rs`: `crate::app::ChatMessage` (if used) → `crate::types::ChatMessage`
- `dialogs.rs`: `crate::app::CustomizeState` → `crate::types::CustomizeState`

- [ ] **Step 5: Verify compilation and tests**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: Compiles, all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/types.rs crates/ox-cli/src/app.rs crates/ox-cli/src/main.rs crates/ox-cli/src/event_loop.rs crates/ox-cli/src/key_handlers.rs crates/ox-cli/src/view_state.rs crates/ox-cli/src/tui.rs crates/ox-cli/src/thread_view.rs crates/ox-cli/src/tab_bar.rs crates/ox-cli/src/dialogs.rs
git commit -m "refactor(ox-cli): extract rendering types to types.rs"
```

---

### Task 3: Move Dialog State Out of App

**Files:**
- Modify: `crates/ox-cli/src/event_loop.rs`
- Modify: `crates/ox-cli/src/key_handlers.rs`
- Modify: `crates/ox-cli/src/view_state.rs`
- Modify: `crates/ox-cli/src/app.rs`
- Modify: `crates/ox-cli/src/tui.rs`

- [ ] **Step 1: Define DialogState in event_loop.rs**

Add at the top of `crates/ox-cli/src/event_loop.rs` (after imports):

```rust
use crate::types::CustomizeState;

/// Dialog-local state, owned by the event loop (not App, not broker).
/// Approval and customize dialogs are ephemeral state machines that
/// don't need broker round-tripping.
pub(crate) struct DialogState {
    pub approval_selected: usize,
    pub pending_customize: Option<CustomizeState>,
}
```

- [ ] **Step 2: Create DialogState in run_async, pass to handlers**

At the top of `run_async`, after the `loop {`:

```rust
    let mut dialog = DialogState {
        approval_selected: 0,
        pending_customize: None,
    };
```

Actually — `dialog` must be created *before* the loop since it persists across frames. Move it before `loop {`.

Replace all `app.approval_selected` with `dialog.approval_selected` and `app.pending_customize` with `dialog.pending_customize` in `run_async`.

- [ ] **Step 3: Update ViewState to borrow from DialogState**

In `crates/ox-cli/src/view_state.rs`, change ViewState:

```rust
// Replace:
//     pub approval_selected: usize,
//     pub pending_customize: &'a Option<CustomizeState>,
// With:
    pub approval_selected: usize,
    pub pending_customize: &'a Option<CustomizeState>,
```

Actually these fields stay the same type — what changes is where `fetch_view_state` reads them from. Change `fetch_view_state` signature to also accept `&'a DialogState`:

Wait — `fetch_view_state` currently takes `&'a App`. We need to also pass dialog state. Two options:

**Option A:** Add a second parameter `dialog: &'a DialogState`.
**Option B:** Have the event loop set these fields after fetch_view_state.

Option A is cleaner. Change signature:

```rust
pub async fn fetch_view_state<'a>(
    client: &ClientHandle,
    app: &'a App,
    dialog: &'a crate::event_loop::DialogState,
) -> ViewState<'a> {
```

Update the ViewState construction at the bottom:

```rust
    // Replace:
    //     approval_selected: app.approval_selected,
    //     pending_customize: &app.pending_customize,
    // With:
        approval_selected: dialog.approval_selected,
        pending_customize: &dialog.pending_customize,
```

- [ ] **Step 4: Update key_handlers.rs signatures**

Change `handle_approval_key` and `handle_customize_key` to take `&mut DialogState` instead of `&mut App`:

```rust
pub(crate) async fn handle_approval_key(
    dialog: &mut crate::event_loop::DialogState,
    client: &ox_broker::ClientHandle,
    active_thread_id: &Option<String>,
    key: KeyCode,
    _modifiers: crossterm::event::KeyModifiers,
) {
```

Replace all `app.approval_selected` with `dialog.approval_selected` and `app.pending_customize` with `dialog.pending_customize` in both functions.

Also update `crate::app::CustomizeState` to `crate::types::CustomizeState` and `crate::app::FsRuleState` to `crate::types::FsRuleState` in the customize key handler.

- [ ] **Step 5: Update event_loop.rs call sites**

Update all calls to `handle_approval_key` and `handle_customize_key` to pass `&mut dialog` instead of `app`.

Update `fetch_view_state(client, app)` to `fetch_view_state(client, app, &dialog)`.

Update mouse click handler: `app.approval_selected = idx` → `dialog.approval_selected = idx`.

Update `dispatch_mouse_owned` call: `app.pending_customize.is_some()` → `dialog.pending_customize.is_some()`.

- [ ] **Step 6: Remove dialog fields from App**

In `crates/ox-cli/src/app.rs`, remove:
- `pub approval_selected: usize,`
- `pub pending_customize: Option<CustomizeState>,`
- `approval_selected: 0,` from `App::new`
- `pending_customize: None,` from `App::new`
- `use crate::types::CustomizeState;` if no longer needed

App now has 6 fields: pool, model, provider, input_history, history_cursor, input_draft.

- [ ] **Step 7: Verify compilation and tests**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: Compiles, all tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/ox-cli/src/event_loop.rs crates/ox-cli/src/key_handlers.rs crates/ox-cli/src/view_state.rs crates/ox-cli/src/app.rs crates/ox-cli/src/tui.rs
git commit -m "refactor(ox-cli): move dialog state out of App into event-loop-local DialogState"
```

---

### Task 4: Split view_state.rs into view_state.rs + parse.rs

**Files:**
- Create: `crates/ox-cli/src/parse.rs`
- Modify: `crates/ox-cli/src/view_state.rs`
- Modify: `crates/ox-cli/src/main.rs`

- [ ] **Step 1: Create parse.rs**

Create `crates/ox-cli/src/parse.rs` by moving from `view_state.rs`:

- `InboxThread` struct (currently in view_state.rs around line 20)
- `pub fn parse_chat_messages(values: &[Value]) -> Vec<ChatMessage>`
- `fn parse_one_message(val: &Value, out: &mut Vec<ChatMessage>)`
- `fn parse_user_content(content: &Value, out: &mut Vec<ChatMessage>)`
- `fn parse_assistant_content(content: &Value, out: &mut Vec<ChatMessage>)`
- `pub fn parse_inbox_threads(value: &Value) -> Vec<InboxThread>`
- `pub fn search_matches(chips, live_query, title, labels, state) -> bool`
- The entire `#[cfg(test)] mod tests` block

The file imports:

```rust
use structfs_core_store::Value;
use crate::types::ChatMessage;
```

All functions keep their existing visibility. `InboxThread` becomes `pub` since it's used by view_state.rs and inbox_view.rs.

- [ ] **Step 2: Update view_state.rs**

Remove all moved code. Add import:

```rust
use crate::parse::{InboxThread, parse_chat_messages, parse_inbox_threads};
```

Re-export `InboxThread` for consumers that currently import from `crate::view_state::InboxThread` (if any — check `inbox_view.rs` and `tab_bar.rs`):

```rust
pub use crate::parse::InboxThread;
```

view_state.rs should be down to ~250 lines: ViewState struct + fetch_view_state.

- [ ] **Step 3: Add module declaration to main.rs**

Add `mod parse;` to main.rs.

- [ ] **Step 4: Update any direct consumers of parse functions**

Check if any file imports `parse_chat_messages` or `parse_inbox_threads` directly from `view_state` — they shouldn't (they're only called inside `fetch_view_state`), but verify.

- [ ] **Step 5: Verify compilation and tests**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: Compiles, all tests pass (parse tests now run from parse.rs).

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/parse.rs crates/ox-cli/src/view_state.rs crates/ox-cli/src/main.rs
git commit -m "refactor(ox-cli): extract parsing functions to parse.rs"
```

---

### Task 5: Final Quality Gate and Status Update

**Files:**
- Modify: `docs/design/rfc/structfs-tui-status.md`

- [ ] **Step 1: Run formatter**

Run: `./scripts/fmt.sh`

- [ ] **Step 2: Commit formatting if needed**

- [ ] **Step 3: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: 14/14 pass.

- [ ] **Step 4: Verify final line counts**

Run: `wc -l crates/ox-cli/src/*.rs | sort -n`

Expected targets:
- app.rs: ~100 lines
- event_loop.rs: ~350 lines (shorter from cmd! macro)
- view_state.rs: ~250 lines
- No file over ~500 lines

- [ ] **Step 5: Verify App has 6 fields**

Grep App struct to confirm: pool, model, provider, input_history, history_cursor, input_draft.

- [ ] **Step 6: Update status document**

Add Phase 2 entry to `docs/design/rfc/structfs-tui-status.md`:

```markdown
#### Phase 2: S-Tier Polish (complete)
- `cmd!` macro eliminates broker command boilerplate (broker_cmd.rs)
- Rendering types extracted to types.rs (ChatMessage, ThreadView, CustomizeState, etc.)
- Dialog state (approval_selected, pending_customize) moved from App to event-loop-local DialogState
- Parsing functions extracted to parse.rs (parse_chat_messages, parse_inbox_threads, search_matches)
- App reduced to 6 fields: pool, model, provider, input_history, history_cursor, input_draft
- No file over 500 lines
- **Spec:** `docs/superpowers/specs/2026-04-07-s-tier-polish-design.md`
- **Plan:** `docs/superpowers/plans/2026-04-07-s-tier-polish.md`
```

- [ ] **Step 7: Commit**

```bash
git add docs/design/rfc/structfs-tui-status.md
git commit -m "docs: update status for Phase 2 S-Tier Polish completion"
```

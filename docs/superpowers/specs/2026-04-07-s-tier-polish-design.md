# Phase 2: S-Tier Polish

**Date:** 2026-04-07
**Status:** Draft
**Prereqs:** Phase 1 App Convergence complete

## Overview

Four cleanup changes that close the gap between "the architecture is
Elm Architecture" and "the code reads like it was designed this way."

After this, App has 6 fields (pool + input history + model/provider),
no file exceeds ~500 lines, rendering types live with rendering code,
and broker command construction is one-line.

## 1. Command Construction Macro

### Problem

15+ places in event_loop.rs build BTreeMap commands with 3-5 lines
of boilerplate each.

### Design

New module `crates/ox-cli/src/broker_cmd.rs` with a `cmd!` macro:

```rust
macro_rules! cmd {
    () => {
        structfs_core_store::Record::parsed(
            structfs_core_store::Value::Map(std::collections::BTreeMap::new())
        )
    };
    ($($key:expr => $val:expr),+ $(,)?) => {{
        let mut map = std::collections::BTreeMap::new();
        $(map.insert($key.to_string(), $crate::broker_cmd::into_value($val));)+
        structfs_core_store::Record::parsed(structfs_core_store::Value::Map(map))
    }};
}
```

Helper function for value conversion:

```rust
pub fn into_value(v: impl IntoValue) -> Value { v.into_value() }

pub trait IntoValue {
    fn into_value(self) -> Value;
}
// Impls for: &str, String, i64, usize, bool, Value
```

The macro returns `Record` directly. Usage:

```rust
// Before:
let mut cmd = BTreeMap::new();
cmd.insert("thread_id".to_string(), Value::String(tid));
let _ = client.write(&path!("ui/open"), Record::parsed(Value::Map(cmd))).await;

// After:
let _ = client.write(&path!("ui/open"), cmd!("thread_id" => tid)).await;
```

Applied to: event_loop.rs (all command constructions), key_handlers.rs
(send_approval_response, dismiss_chip), dispatch_search_edit,
dispatch_text_edit_owned, dispatch_mouse_owned.

## 2. Move Rendering Types Out of app.rs

### Problem

app.rs (274 lines) contains ~170 lines of rendering/dialog types
(ThreadView, ChatMessage, APPROVAL_OPTIONS, CustomizeState, FsRuleState)
that have nothing to do with App state.

### Design

Create `crates/ox-cli/src/types.rs` with:
- `ChatMessage` enum
- `ThreadView` struct
- `APPROVAL_OPTIONS` const
- `CustomizeState` struct + impl
- `FsRuleState` struct

All consumers update their import from `crate::app::X` to `crate::types::X`.
app.rs imports what it needs from types.rs.

After this, app.rs is ~100 lines: App struct + new/send_input/compose/reply/
history/update_thread_state + node_is_allow helper.

## 3. Move Dialog State Out of App

### Problem

`approval_selected` and `pending_customize` in App are UI state that
bypass the broker — the same anti-pattern Phase 1 eliminated elsewhere.

### Design

Create a `DialogState` struct local to the event loop:

```rust
struct DialogState {
    approval_selected: usize,
    pending_customize: Option<CustomizeState>,
}
```

This lives in `event_loop.rs`, constructed at the top of `run_async`,
passed to key handlers and draw functions via ViewState or direct ref.

Remove `approval_selected` and `pending_customize` from App.
Remove them from ViewState (they become direct refs from DialogState).

App drops to 6 fields: pool, model, provider, input_history,
history_cursor, input_draft.

ViewState changes: `approval_selected` and `pending_customize` become
references to DialogState fields instead of App fields. Since DialogState
is local to run_async and ViewState borrows from it, the lifetime works
the same way (ViewState already has lifetime 'a).

Key handlers change signature: take `&mut DialogState` instead of
`&mut App` for dialog mutations. `handle_approval_key` and
`handle_customize_key` only need DialogState + ClientHandle.

## 4. Split view_state.rs

### Problem

689 lines doing three jobs: ViewState struct + fetch, message parsing,
inbox parsing.

### Design

Extract parsing into `crates/ox-cli/src/parse.rs`:
- `pub fn parse_chat_messages(values: &[Value]) -> Vec<ChatMessage>`
- `fn parse_one_message(val: &Value, out: &mut Vec<ChatMessage>)`
- `fn parse_user_content(content: &Value, out: &mut Vec<ChatMessage>)`
- `fn parse_assistant_content(content: &Value, out: &mut Vec<ChatMessage>)`
- `pub fn parse_inbox_threads(value: &Value) -> Vec<InboxThread>`
- `pub fn search_matches(chips, live_query, title, labels, state) -> bool`
- All existing tests for these functions

`InboxThread` struct moves to parse.rs (it's a parse output type).

view_state.rs keeps: ViewState struct, fetch_view_state(). Down to ~250 lines.

## Execution Order

1. cmd! macro (no dependencies, enables cleaner code in steps 2-3)
2. types.rs extraction (independent, pure move)
3. Dialog state out of App (depends on types.rs for CustomizeState import)
4. view_state.rs split (independent, pure move)

## Testing

All changes are refactors — no new behavior. Each step:
- `cargo check -p ox-cli`
- `cargo test -p ox-cli`
- Quality gate: `./scripts/quality_gates.sh` after final step

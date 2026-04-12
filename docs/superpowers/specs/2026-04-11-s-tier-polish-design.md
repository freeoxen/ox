# S-Tier Polish: Command Protocol & Editor System

**Date:** 2026-04-11

## Problem

The command protocol and vim-style editor system work but have five
quality gaps that prevent S-tier status:

1. Editor logic is inlined in a 1700+ line event_loop.rs
2. `:w`/`:wq` duplicates the `send_input` pending_action logic
3. Editor modes have zero test coverage
4. All bindings use legacy `Action::Command` instead of `Action::Invoke`
5. New code uses raw `Path::parse()` instead of `oxpath!()` compile-time validation

## Design

### 1. Extract editor module

**Create:** `crates/ox-cli/src/editor.rs`

Move from `event_loop.rs`:
- `EditorMode` enum
- `InputSession` struct + all its methods
- `handle_editor_insert_key()` 
- `handle_editor_normal_key()`
- `handle_editor_command_key()`
- `EditSource`, `EditOp`, `Edit` types (used by InputSession)
- `now_ms()` helper

The event loop keeps the key dispatch logic (ESC intercept, mode
routing) but calls into `editor::*` functions. `InputSession` becomes
`pub(crate)` so the event loop can construct and use it.

`EditorMode` stays `pub(crate)` since ViewState and tui.rs reference it.

### 2. DRY up send_input

**Create in editor.rs:**

```rust
pub(crate) async fn submit_editor_content(
    session: &mut InputSession,
    app: &mut App,
    client: &ClientHandle,
) -> Option<String> // returns new thread_id if composed
```

This function does:
1. Flush pending edits
2. Read insert_context and active_thread from broker
3. Call `app.send_input_with_text()`
4. Clear input and exit insert mode via broker
5. Reset session
6. Return new thread_id (if compose created one)

Both the `send_input` pending_action handler and the `:w`/`:wq`
editor command handler call this single function.

### 3. Tests for editor modes

**Create:** `crates/ox-cli/src/editor.rs` test module

Tests operate on `InputSession` directly — no broker, no terminal,
no async. The functions take `&mut InputSession` + primitives.

**Editor-insert tests:**
- Character insertion at cursor
- Backspace deletes previous char
- Ctrl+a moves to start, Ctrl+e moves to end
- Arrow keys move cursor
- Enter inserts newline

**Editor-normal tests:**
- h/l move left/right
- j/k move up/down (with wrap_lines)
- w/b word forward/back
- 0/$ line start/end
- i enters insert mode
- a enters insert after cursor
- I/A enter insert at line start/end
- o/O open line below/above, enter insert
- x deletes char under cursor
- : enters command mode

**Editor-command tests:**
- Characters append to buffer
- Backspace removes from buffer
- Backspace on empty buffer → normal mode
- ESC → normal mode, buffer cleared
- Enter with "q" → sets a flag/returns an action (testable without broker)

For command execution tests that need a broker (`:w`, `:wq`), use
the same integration test pattern as `lib.rs`: mount stores in a
test broker, dispatch through it.

### 4. Action::Invoke migration

**Modify:** `crates/ox-cli/src/bindings.rs`

Replace every `Action::Command { target, fields }` with
`Action::Invoke { command, args }`. The command names come from the
builtin catalog.

Mapping:
- `cmd("ui/select_next")` → `Action::Invoke { command: "select_next", args: {} }`
- `cmd_with("ui/enter_insert", [static_field("context", "compose")])` → `Action::Invoke { command: "compose", args: {} }` (default applies)
- `cmd_with("approval/response", [static_field("decision", "allow_once")])` → `Action::Invoke { command: "approve", args: { "decision": "allow_once" } }`

After all bindings migrate, remove:
- `Action::Command` variant from the enum
- The `Command` arm in `InputStore::execute_action`
- `ActionField` enum (no longer needed)
- The `cmd()`, `cmd_with()`, `static_field()`, `p()` helpers in bindings.rs

**Modify:** `crates/ox-ui/src/input_store.rs`
- Remove `Action::Command` variant
- Remove `ActionField` enum
- Remove the legacy dispatch path in `execute_action`
- Update `binding_to_value` to only handle `Invoke` and `Macro`
- Update `handle_bind` to create `Invoke` actions

**Modify:** `crates/ox-ui/src/lib.rs`
- Remove `ActionField` from re-exports

### 5. oxpath! migration

**Modify:** All files that use `structfs_core_store::path!()` or
`Path::parse(&format!(...))` in new command protocol code:

- `crates/ox-cli/src/editor.rs` (after extraction)
- `crates/ox-cli/src/event_loop.rs`
- `crates/ox-ui/src/command_store.rs`
- `crates/ox-ui/src/command_registry.rs`

Replace with `ox_path::oxpath!()` for static paths. For dynamic paths
(like `command/commands/{name}`), continue using `Path::parse()` since
`oxpath!` requires literal components or `PathComponent` values.

Add `ox-path` as a dependency to `ox-ui/Cargo.toml` if not already present.

# S-Tier Event Loop + Full Typed Command System

**Date:** 2026-04-12
**Status:** Approved
**Scope:** Complete screen extraction, type remaining store protocols, delete cmd! macro

## Problem

The typed StructFS migration (2026-04-11) got us to A-tier. The remaining gaps:

1. **Screen extraction is incomplete.** `InputSession`, `TextInputView`, `prev_mode` still live in the event loop instead of `ThreadShell`. `SettingsState` and test connection polling still live in the event loop instead of `SettingsShell`. The `Outcome` enum is just `{Handled, Ignored}` — no `Quit`, no `Action`.

2. **Two command protocols coexist.** UI writes use typed `UiCommand` via `write_typed`. Everything else uses `cmd!` macro with string keys: `input/key`, config writes, inbox updates, approval responses. The `broker_cmd.rs` module and `cmd!` macro still exist.

3. **UiStore tests use the old string protocol.** ~100 uses of `cmd_map`/`empty_cmd` helpers writing string commands. They test the legacy path, not the typed path.

## Design

### 1. Screen Structs Own Their State

```rust
// shell.rs
pub(crate) enum Outcome {
    Ignored,
    Handled,
    Quit,
    Action(AppAction),
}

pub(crate) enum AppAction {
    Compose { text: String },
    Reply { thread_id: String, text: String },
    ArchiveThread { thread_id: String },
}

pub(crate) struct ShellState {
    pub inbox: InboxShell,
    pub thread: ThreadShell,
    pub settings: SettingsShell,
}
```

**ThreadShell** owns `InputSession`, `TextInputView`, `prev_mode: Mode`. Its `handle_key` handles ESC interception, editor sub-mode dispatch, and returns `Action(Compose{..})` or `Action(Reply{..})` on submit instead of the event loop calling `app.send_input_with_text` directly.

**SettingsShell** owns `SettingsState` (including wizard step, test connection rx, edit dialog, all of it). Its `poll()` method checks the test connection oneshot each frame. Its `handle_key` handles all settings navigation. `new_wizard()` constructor for first-run setup.

**InboxShell** is stateless — search state lives in UiStore (shared core). Handles search chip dismissal on digit keys.

### 2. Event Loop Becomes a Router

The event loop's responsibilities shrink to:
- Fetch ViewState from broker
- Call `shell.settings.poll()` if on settings screen
- Draw
- Poll terminal event
- Dispatch to screen handler: `shell.handle_key(screen, key, ...)` → match `Outcome`
- Handle `Outcome::Action` by calling `App` methods
- Handle `Outcome::Quit` by returning
- Mouse dispatch per-screen
- Flush pending edits after each event

Target: ~150 lines.

### 3. Type the Remaining Store Protocols

**InputKeyEvent** — new type in `ox-types`:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputKeyEvent {
    pub mode: Mode,
    pub key: String,
    pub screen: Screen,
}
```
InputStore's Writer accepts this via `from_value` on the `key` write path. Call site uses `write_typed`.

**ApprovalResponse** — new type in `ox-types`:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub decision: String,
}
```
Used by `key_handlers.rs` when writing to `threads/{id}/approval/response`.

**Config writes** — these are direct value writes to paths like `config/gate/defaults/model`. They already write typed Rust values (`String`, `i64`). Switch from `Record::parsed(Value::String(...))` to `write_typed(&path, &string_value)`. No new command type needed.

**Inbox writes** — `cmd!("inbox_state" => "done")` becomes a typed struct or direct `write_typed`.

### 4. Delete `cmd!` Macro

After all call sites migrate, delete `crates/ox-cli/src/broker_cmd.rs` entirely. Remove the `cmd!` macro export. Remove `mod broker_cmd` from `main.rs`.

### 5. Migrate UiStore Tests

Replace `cmd_map`/`empty_cmd` test helpers with typed `UiCommand` writes via `to_value`. Tests become:
```rust
let cmd = UiCommand::Open { thread_id: "t_001".to_string() };
store.write(&path!(""), Record::parsed(to_value(&cmd).unwrap())).unwrap();
```

The legacy string-based Writer path can be removed once all tests use typed commands and the `cmd!` macro is gone.

## Migration Path

1. Expand `Outcome` enum with `Quit` and `Action(AppAction)`
2. Create `ShellState`, `ThreadShell`, `SettingsShell` structs that own their state
3. Move `InputSession`/`TextInputView`/`prev_mode` into `ThreadShell`
4. Move `SettingsState` + test polling into `SettingsShell`
5. Wire event loop as router, handle `Outcome::Action` and `Outcome::Quit`
6. Move mouse handling per-screen
7. Add `InputKeyEvent` to ox-types, type the `input/key` write path
8. Add `ApprovalResponse` to ox-types, type the approval response write path
9. Convert config writes to `write_typed`
10. Convert inbox writes to `write_typed`
11. Delete `broker_cmd.rs` and `cmd!` macro
12. Migrate UiStore tests to typed commands
13. Remove legacy string-based Writer path from UiStore
14. Quality gates

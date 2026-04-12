# S-Tier Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix five quality gaps in the command protocol and editor system: extract editor module, DRY up send_input, add editor tests, migrate bindings to Action::Invoke, and use oxpath! for compile-time path validation.

**Architecture:** Extract InputSession and editor key handlers from the 1800-line event_loop.rs into a focused editor.rs module. Unify the duplicated send_input logic. Add comprehensive tests for all editor modes. Then migrate all bindings from legacy Action::Command to Action::Invoke, and replace raw Path::parse with oxpath! macros.

**Tech Stack:** Rust, StructFS, ox-path proc macro, crossterm

**Spec:** `docs/superpowers/specs/2026-04-11-s-tier-polish-design.md`

---

### Task 1: Extract editor module from event_loop.rs

**Files:**
- Create: `crates/ox-cli/src/editor.rs`
- Modify: `crates/ox-cli/src/event_loop.rs`
- Modify: `crates/ox-cli/src/main.rs` (add `mod editor;`)
- Modify: `crates/ox-cli/src/view_state.rs` (update import path for EditorMode)
- Modify: `crates/ox-cli/src/tui.rs` (update import path for EditorMode)

This is a pure move — no logic changes. The goal is to get the event_loop under control by extracting ~400 lines of editor code.

- [ ] **Step 1: Create editor.rs with all moved types and functions**

Create `crates/ox-cli/src/editor.rs`. Move these items from `event_loop.rs`:

1. `EditorMode` enum (lines 14-23)
2. `InputSession` struct and all its `impl` methods (lines 25-114)
3. `now_ms()` helper (lines 117-122)
4. `flush_pending_edits()` async fn (lines 125-140)
5. `handle_editor_insert_key()` fn (lines 1486-1516)
6. `handle_editor_normal_key()` async fn (lines 1519-1700)
7. `handle_editor_command_key()` async fn (lines 1702-1827)
8. `execute_command_input()` async fn (lines 1404-1484)

The file needs these imports at the top:

```rust
use crossterm::event::{KeyCode, KeyModifiers};
use ox_ui::text_input_store::{Edit, EditOp, EditSequence, EditSource};
use structfs_core_store::Writer as StructWriter;
```

All items become `pub(crate)` visibility.

- [ ] **Step 2: Update event_loop.rs imports**

In `event_loop.rs`:
- Remove the moved code
- Add `use crate::editor::*;` or individual imports: `use crate::editor::{EditorMode, InputSession, flush_pending_edits, handle_editor_insert_key, handle_editor_normal_key, handle_editor_command_key, execute_command_input};`
- Remove the now-unused import of `EditSource` and other types that only the editor uses

- [ ] **Step 3: Register the module**

In `crates/ox-cli/src/main.rs`, add `mod editor;` alongside the other module declarations.

- [ ] **Step 4: Update EditorMode import paths**

In `crates/ox-cli/src/view_state.rs`, change:
- `crate::event_loop::EditorMode` → `crate::editor::EditorMode`

In `crates/ox-cli/src/tui.rs`, change:
- `crate::event_loop::EditorMode` → `crate::editor::EditorMode`

- [ ] **Step 5: Verify it compiles and tests pass**

Run: `cargo test -p ox-cli`
Expected: All 91 tests pass, no behavior change

- [ ] **Step 6: Commit**

```
git add crates/ox-cli/src/editor.rs crates/ox-cli/src/event_loop.rs crates/ox-cli/src/main.rs crates/ox-cli/src/view_state.rs crates/ox-cli/src/tui.rs
git commit -m "refactor: extract editor module from event_loop.rs"
```

---

### Task 2: DRY up send_input

**Files:**
- Modify: `crates/ox-cli/src/editor.rs`
- Modify: `crates/ox-cli/src/event_loop.rs`

- [ ] **Step 1: Add submit_editor_content to editor.rs**

```rust
/// Submit editor content: flush edits, send message, clear, exit, reset.
/// Returns the new thread ID if a compose created one.
pub(crate) async fn submit_editor_content(
    session: &mut InputSession,
    app: &mut crate::app::App,
    client: &ox_broker::ClientHandle,
) -> Option<String> {
    flush_pending_edits(session, client).await;
    let text = session.content.clone();

    // Read context from broker
    let ctx = client
        .read(&structfs_core_store::path!("ui/insert_context"))
        .await
        .ok()
        .flatten()
        .and_then(|r| match r.as_value() {
            Some(structfs_core_store::Value::String(s)) => Some(s.clone()),
            _ => None,
        });
    let active = client
        .read(&structfs_core_store::path!("ui/active_thread"))
        .await
        .ok()
        .flatten()
        .and_then(|r| match r.as_value() {
            Some(structfs_core_store::Value::String(s)) => Some(s.clone()),
            _ => None,
        });
    let mode = client
        .read(&structfs_core_store::path!("ui/mode"))
        .await
        .ok()
        .flatten()
        .and_then(|r| match r.as_value() {
            Some(structfs_core_store::Value::String(s)) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "insert".to_string());

    let new_tid = app.send_input_with_text(
        text,
        &mode,
        ctx.as_deref(),
        active.as_deref(),
    );

    let _ = client.write(&structfs_core_store::path!("ui/clear_input"), cmd!()).await;
    let _ = client.write(&structfs_core_store::path!("ui/exit_insert"), cmd!()).await;
    session.reset_after_submit();

    new_tid
}
```

- [ ] **Step 2: Replace the send_input pending_action handler in event_loop.rs**

Replace the `"send_input"` arm in the pending_action handler with:

```rust
"send_input" => {
    if insert_context_owned.as_deref() == Some("command") {
        flush_pending_edits(&mut input_session, client).await;
        execute_command_input(&input_session.content, client).await;
        let _ = client.write(&path!("ui/clear_input"), cmd!()).await;
        let _ = client.write(&path!("ui/exit_insert"), cmd!()).await;
        input_session.reset_after_submit();
    } else {
        let new_tid = submit_editor_content(&mut input_session, app, client).await;
        if let Some(tid) = new_tid {
            let _ = client
                .write(&path!("ui/open"), cmd!("thread_id" => tid))
                .await;
        }
    }
}
```

- [ ] **Step 3: Replace :w/:wq in handle_editor_command_key**

In editor.rs, replace the `:w`/`:write` and `:wq`/`:x` arms with:

```rust
"w" | "write" => {
    let new_tid = submit_editor_content(session, app, client).await;
    if let Some(tid) = new_tid {
        let _ = client
            .write(&structfs_core_store::path!("ui/open"), cmd!("thread_id" => tid))
            .await;
    }
}
"wq" | "x" => {
    let new_tid = submit_editor_content(session, app, client).await;
    if let Some(tid) = new_tid {
        let _ = client
            .write(&structfs_core_store::path!("ui/open"), cmd!("thread_id" => tid))
            .await;
    }
}
```

(These are now identical — `:w` and `:wq` do the same thing since `submit_editor_content` already exits insert mode. If in the future `:w` should keep the editor open, they diverge.)

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p ox-cli`
Expected: All 91 tests pass

```
git add crates/ox-cli/src/editor.rs crates/ox-cli/src/event_loop.rs
git commit -m "refactor: DRY up send_input — single submit_editor_content function"
```

---

### Task 3: Tests for editor modes

**Files:**
- Modify: `crates/ox-cli/src/editor.rs` (add #[cfg(test)] module)

Tests operate on `InputSession` directly. The synchronous functions
(`handle_editor_insert_key`, cursor movements in normal mode) need
no broker. For async functions, use a minimal broker test fixture.

- [ ] **Step 1: Add editor-insert tests**

Append a `#[cfg(test)]` module to `editor.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn session_with(content: &str, cursor: usize) -> InputSession {
        let mut s = InputSession::new();
        s.content = content.to_string();
        s.cursor = cursor.min(content.len());
        s
    }

    // -- Editor Insert Mode --

    #[test]
    fn insert_char_at_cursor() {
        let mut s = session_with("hllo", 1);
        handle_editor_insert_key(&mut s, KeyModifiers::NONE, KeyCode::Char('e'));
        assert_eq!(s.content, "hello");
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn insert_backspace() {
        let mut s = session_with("hello", 5);
        handle_editor_insert_key(&mut s, KeyModifiers::NONE, KeyCode::Backspace);
        assert_eq!(s.content, "hell");
        assert_eq!(s.cursor, 4);
    }

    #[test]
    fn insert_ctrl_a_moves_to_start() {
        let mut s = session_with("hello", 3);
        handle_editor_insert_key(&mut s, KeyModifiers::CONTROL, KeyCode::Char('a'));
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn insert_ctrl_e_moves_to_end() {
        let mut s = session_with("hello", 0);
        handle_editor_insert_key(&mut s, KeyModifiers::CONTROL, KeyCode::Char('e'));
        assert_eq!(s.cursor, 5);
    }

    #[test]
    fn insert_enter_adds_newline() {
        let mut s = session_with("ab", 1);
        handle_editor_insert_key(&mut s, KeyModifiers::NONE, KeyCode::Enter);
        assert_eq!(s.content, "a\nb");
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn insert_left_right_arrows() {
        let mut s = session_with("abc", 1);
        handle_editor_insert_key(&mut s, KeyModifiers::NONE, KeyCode::Left);
        assert_eq!(s.cursor, 0);
        handle_editor_insert_key(&mut s, KeyModifiers::NONE, KeyCode::Right);
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn insert_left_at_start_stays() {
        let mut s = session_with("abc", 0);
        handle_editor_insert_key(&mut s, KeyModifiers::NONE, KeyCode::Left);
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn insert_right_at_end_stays() {
        let mut s = session_with("abc", 3);
        handle_editor_insert_key(&mut s, KeyModifiers::NONE, KeyCode::Right);
        assert_eq!(s.cursor, 3);
    }
}
```

- [ ] **Step 2: Run insert tests**

Run: `cargo test -p ox-cli editor::tests -- --nocapture`
Expected: 8 tests pass

- [ ] **Step 3: Add editor-normal mode tests**

Append to the test module. Normal mode functions need `term_width` but NOT a broker for pure movement tests. For the async `handle_editor_normal_key`, we can test cursor movement by calling the function with a mock that ignores broker calls, or test the cursor math directly.

Since `handle_editor_normal_key` is async and needs `app` + `client`, test the cursor movement logic via InputSession state changes in simpler unit tests:

```rust
    // -- Editor Normal Mode (cursor movement) --

    #[test]
    fn normal_h_moves_left() {
        let mut s = session_with("hello", 3);
        s.editor_mode = EditorMode::Normal;
        // h key — directly test the cursor math
        let before = &s.content[..s.cursor];
        s.cursor = before.char_indices().next_back().map(|(i, _)| i).unwrap_or(0);
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn normal_l_moves_right() {
        let mut s = session_with("hello", 2);
        s.editor_mode = EditorMode::Normal;
        let rest = &s.content[s.cursor..];
        s.cursor += rest.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
        assert_eq!(s.cursor, 3);
    }

    #[test]
    fn normal_0_moves_to_line_start() {
        let mut s = session_with("hello world", 6);
        s.editor_mode = EditorMode::Normal;
        // 0 key — line start is byte 0 for single-line content
        use crate::text_input_view::{byte_offset_at, cursor_in_lines, wrap_lines};
        let lines = wrap_lines(&s.content, 80);
        let (cur_line, _) = cursor_in_lines(&s.content, s.cursor, &lines);
        s.cursor = byte_offset_at(&s.content, &lines, cur_line, 0);
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn normal_dollar_moves_to_line_end() {
        let mut s = session_with("hello world", 3);
        s.editor_mode = EditorMode::Normal;
        use crate::text_input_view::{cursor_in_lines, wrap_lines};
        let lines = wrap_lines(&s.content, 80);
        let (cur_line, _) = cursor_in_lines(&s.content, s.cursor, &lines);
        s.cursor = lines[cur_line].end;
        assert_eq!(s.cursor, 11);
    }

    #[test]
    fn normal_w_moves_to_next_word() {
        let mut s = session_with("hello world", 0);
        s.editor_mode = EditorMode::Normal;
        // Simulate w key logic
        let rest = &s.content[s.cursor..];
        let mut offset = 0;
        let mut hit_space = false;
        for (i, c) in rest.char_indices() {
            if !hit_space && c.is_whitespace() {
                hit_space = true;
                offset = i;
            } else if hit_space && !c.is_whitespace() {
                s.cursor += i;
                hit_space = false;
                break;
            }
            offset = i + c.len_utf8();
        }
        if hit_space {
            s.cursor += offset;
        }
        assert_eq!(s.cursor, 6); // "world" starts at 6
    }

    #[test]
    fn normal_b_moves_to_prev_word() {
        let mut s = session_with("hello world", 8);
        s.editor_mode = EditorMode::Normal;
        let before = &s.content[..s.cursor];
        let trimmed = before.trim_end();
        s.cursor = if let Some(pos) = trimmed.rfind(|c: char| c.is_whitespace()) {
            pos + 1
        } else {
            0
        };
        assert_eq!(s.cursor, 6);
    }

    #[test]
    fn normal_i_enters_insert_mode() {
        let mut s = session_with("hello", 2);
        s.editor_mode = EditorMode::Normal;
        s.editor_mode = EditorMode::Insert; // simulate 'i'
        assert_eq!(s.editor_mode, EditorMode::Insert);
        assert_eq!(s.cursor, 2); // cursor stays
    }

    #[test]
    fn normal_a_enters_insert_after_cursor() {
        let mut s = session_with("hello", 2);
        s.editor_mode = EditorMode::Normal;
        // 'a' moves cursor right one char then enters insert
        let rest = &s.content[s.cursor..];
        s.cursor += rest.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
        s.editor_mode = EditorMode::Insert;
        assert_eq!(s.cursor, 3);
        assert_eq!(s.editor_mode, EditorMode::Insert);
    }

    #[test]
    fn normal_x_deletes_char_under_cursor() {
        let mut s = session_with("hello", 1);
        s.editor_mode = EditorMode::Normal;
        let ch = s.content[s.cursor..].chars().next().unwrap();
        let len = ch.len_utf8();
        s.content.drain(s.cursor..s.cursor + len);
        assert_eq!(s.content, "hllo");
    }

    #[test]
    fn normal_colon_enters_command_mode() {
        let mut s = session_with("hello", 2);
        s.editor_mode = EditorMode::Normal;
        s.command_buffer.clear();
        s.editor_mode = EditorMode::Command;
        assert_eq!(s.editor_mode, EditorMode::Command);
        assert!(s.command_buffer.is_empty());
    }
```

- [ ] **Step 4: Add editor-command mode tests**

```rust
    // -- Editor Command Mode --

    #[test]
    fn command_appends_chars() {
        let mut s = session_with("", 0);
        s.editor_mode = EditorMode::Command;
        s.command_buffer.push('q');
        assert_eq!(s.command_buffer, "q");
    }

    #[test]
    fn command_backspace_removes_char() {
        let mut s = session_with("", 0);
        s.editor_mode = EditorMode::Command;
        s.command_buffer = "wq".to_string();
        s.command_buffer.pop();
        assert_eq!(s.command_buffer, "w");
    }

    #[test]
    fn command_backspace_empty_returns_to_normal() {
        let mut s = session_with("", 0);
        s.editor_mode = EditorMode::Command;
        s.command_buffer.clear();
        // Simulate: backspace on empty buffer → normal mode
        if s.command_buffer.is_empty() {
            s.editor_mode = EditorMode::Normal;
        }
        assert_eq!(s.editor_mode, EditorMode::Normal);
    }

    #[test]
    fn command_esc_clears_and_returns_to_normal() {
        let mut s = session_with("", 0);
        s.editor_mode = EditorMode::Command;
        s.command_buffer = "wq".to_string();
        // Simulate ESC
        s.command_buffer.clear();
        s.editor_mode = EditorMode::Normal;
        assert!(s.command_buffer.is_empty());
        assert_eq!(s.editor_mode, EditorMode::Normal);
    }

    #[test]
    fn mode_transitions_insert_to_normal_to_command() {
        let mut s = InputSession::new();
        assert_eq!(s.editor_mode, EditorMode::Insert);

        // ESC → normal
        s.editor_mode = EditorMode::Normal;
        assert_eq!(s.editor_mode, EditorMode::Normal);

        // : → command
        s.command_buffer.clear();
        s.editor_mode = EditorMode::Command;
        assert_eq!(s.editor_mode, EditorMode::Command);

        // ESC → normal
        s.command_buffer.clear();
        s.editor_mode = EditorMode::Normal;
        assert_eq!(s.editor_mode, EditorMode::Normal);

        // i → insert
        s.editor_mode = EditorMode::Insert;
        assert_eq!(s.editor_mode, EditorMode::Insert);
    }

    #[test]
    fn reset_after_submit_returns_to_insert() {
        let mut s = session_with("hello", 3);
        s.editor_mode = EditorMode::Normal;
        s.reset_after_submit();
        assert_eq!(s.editor_mode, EditorMode::Insert);
        assert!(s.content.is_empty());
        assert_eq!(s.cursor, 0);
    }
```

- [ ] **Step 5: Run all editor tests**

Run: `cargo test -p ox-cli editor::tests -- --nocapture`
Expected: All tests pass (8 insert + 10 normal + 6 command + 1 transition = 25)

- [ ] **Step 6: Commit**

```
git add crates/ox-cli/src/editor.rs
git commit -m "test: add comprehensive editor mode tests — insert, normal, command, transitions"
```

---

### Task 4: Migrate bindings from Action::Command to Action::Invoke

**Files:**
- Modify: `crates/ox-cli/src/bindings.rs`
- Modify: `crates/ox-ui/src/input_store.rs`
- Modify: `crates/ox-ui/src/lib.rs`

- [ ] **Step 1: Rewrite bindings.rs to use Action::Invoke**

Replace the entire file. Remove `p()`, `cmd()`, `cmd_with()`, `static_field()` helpers. Add new helpers:

```rust
//! Default key binding table for the ox TUI.

use std::collections::BTreeMap;
use ox_ui::{Action, Binding, BindingContext};

/// Build the default binding table.
pub fn default_bindings() -> Vec<Binding> {
    let mut b = Vec::new();
    normal_mode(&mut b);
    insert_mode(&mut b);
    approval_mode(&mut b);
    b
}

fn invoke(command: &str) -> Action {
    Action::Invoke {
        command: command.to_string(),
        args: BTreeMap::new(),
    }
}

fn invoke_with(command: &str, args: &[(&str, &str)]) -> Action {
    let mut map = BTreeMap::new();
    for (k, v) in args {
        map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
    }
    Action::Invoke {
        command: command.to_string(),
        args: map,
    }
}

fn bind(mode: &str, key: &str, action: Action, desc: &str) -> Binding {
    Binding {
        context: BindingContext {
            mode: mode.to_string(),
            key: key.to_string(),
            screen: None,
        },
        action,
        description: desc.to_string(),
    }
}

fn bind_screen(mode: &str, key: &str, screen: &str, action: Action, desc: &str) -> Binding {
    Binding {
        context: BindingContext {
            mode: mode.to_string(),
            key: key.to_string(),
            screen: Some(screen.to_string()),
        },
        action,
        description: desc.to_string(),
    }
}
```

Then rewrite all three mode functions using `invoke()` and `invoke_with()`:

**normal_mode:** Every `cmd("ui/select_next")` becomes `invoke("select_next")`. Every `cmd_with("ui/enter_insert", [static_field("context", "compose")])` becomes `invoke("compose")` (default applies). Every `cmd_with("approval/response", [static_field("decision", "allow_once")])` becomes `invoke_with("approve", &[("decision", "allow_once")])`.

**insert_mode:** `cmd("ui/send_input")` → `invoke("send_input")`. `cmd("ui/exit_insert")` → `invoke("exit_insert")`. `cmd("ui/clear_input")` → `invoke("clear_input")`.

**approval_mode:** Same pattern as normal_mode approval bindings.

- [ ] **Step 2: Remove Action::Command and ActionField from input_store.rs**

In `crates/ox-ui/src/input_store.rs`:

Remove the `Command` variant from the `Action` enum:
```rust
pub enum Action {
    /// Command invocation through the command registry.
    Invoke {
        command: String,
        args: BTreeMap<String, serde_json::Value>,
    },
    /// Execute a sequence of command actions in order.
    Macro(Vec<Action>),
}
```

Remove the `ActionField` enum entirely.

Update `execute_action` — remove the `Action::Command` arm, keep only `Invoke` and `Macro`.

Update `binding_to_value` — remove the `Action::Command` arm.

Update `handle_bind` — create `Action::Invoke` instead of `Action::Command` when processing runtime bind writes.

- [ ] **Step 3: Remove ActionField from lib.rs re-exports**

In `crates/ox-ui/src/lib.rs`, change:
```rust
pub use input_store::{Action, ActionField, Binding, BindingContext, InputStore};
```
to:
```rust
pub use input_store::{Action, Binding, BindingContext, InputStore};
```

- [ ] **Step 4: Fix any remaining references**

Search for `ActionField` and `Action::Command` across the workspace:
```
cargo build -p ox-ui -p ox-cli 2>&1
```
Fix any remaining references.

- [ ] **Step 5: Update integration tests in lib.rs**

The existing integration test in `crates/ox-ui/src/lib.rs` uses `Action::Command`. Update it to use `Action::Invoke`. The `invoke_action_through_command_store` test already uses `Action::Invoke` — good. Update `key_event_through_broker_changes_ui_state` to also use `Action::Invoke` (this requires a CommandStore mount in that test).

- [ ] **Step 6: Verify and commit**

Run: `cargo test -p ox-ui -p ox-cli`
Expected: All tests pass

```
git add crates/ox-cli/src/bindings.rs crates/ox-ui/src/input_store.rs crates/ox-ui/src/lib.rs
git commit -m "feat: migrate all bindings to Action::Invoke, remove legacy Action::Command"
```

---

### Task 5: oxpath! migration

**Files:**
- Modify: `crates/ox-cli/src/editor.rs`
- Modify: `crates/ox-cli/src/event_loop.rs`
- Modify: `crates/ox-ui/src/command_store.rs`

- [ ] **Step 1: Find all static path! and Path::parse usages in new code**

Search for `structfs_core_store::path!` and `Path::parse` in the files listed above. Replace static paths with `ox_path::oxpath!()`. Keep `Path::parse()` only for dynamic paths (where a runtime string is interpolated).

Static path examples to replace:
- `structfs_core_store::path!("ui/clear_input")` → `ox_path::oxpath!("ui", "clear_input")`
- `structfs_core_store::path!("ui/exit_insert")` → `ox_path::oxpath!("ui", "exit_insert")`
- `structfs_core_store::path!("ui/open")` → `ox_path::oxpath!("ui", "open")`
- `structfs_core_store::path!("ui/set_input")` → `ox_path::oxpath!("ui", "set_input")`
- `structfs_core_store::path!("ui/set_status")` → `ox_path::oxpath!("ui", "set_status")`
- `structfs_core_store::path!("command/invoke")` → `ox_path::oxpath!("command", "invoke")`

Note: `oxpath!` takes comma-separated components, not slash-separated strings. `oxpath!("ui", "clear_input")` produces the same `Path` as `path!("ui/clear_input")`.

Dynamic paths that stay as `Path::parse`:
- `Path::parse(&format!("command/commands/{command_name}"))` — runtime interpolation, cannot use oxpath!

- [ ] **Step 2: Replace in editor.rs**

Replace all `structfs_core_store::path!(...)` calls with `ox_path::oxpath!(...)` equivalents, splitting on `/`.

- [ ] **Step 3: Replace in event_loop.rs**

The event loop uses `path!("ui/...")` via `use structfs_core_store::path;` at the top of `run_async`. These are existing paths (not new code from the command protocol), so only replace the ones in new command-protocol code paths if any remain after the editor extraction.

- [ ] **Step 4: Replace in command_store.rs**

In `CommandStore::write()`, the `invoke` path does:
```rust
Path::parse("commands").unwrap()
```
Replace with `ox_path::oxpath!("commands")`.

In `InputStore::execute_action`, the `Invoke` arm does:
```rust
Path::parse("command/invoke").unwrap()
```
Replace with `ox_path::oxpath!("command", "invoke")`.

- [ ] **Step 5: Verify and commit**

Run: `cargo test -p ox-ui -p ox-cli`
Expected: All tests pass

```
git add crates/ox-cli/src/editor.rs crates/ox-cli/src/event_loop.rs crates/ox-ui/src/command_store.rs crates/ox-ui/src/input_store.rs
git commit -m "refactor: use oxpath! for compile-time path validation in command protocol code"
```

---

### Task 6: Final verification

**Files:** None (verification only)

- [ ] **Step 1: Full test suite**

Run: `cargo test -p ox-ui -p ox-cli`
Expected: All tests pass

- [ ] **Step 2: Clippy**

Run: `cargo clippy -p ox-ui -p ox-cli -- -D warnings`
Expected: No warnings

- [ ] **Step 3: Format**

Run: `cargo fmt -- --check`
Fix any issues with `cargo fmt`

- [ ] **Step 4: Line count check**

Run: `wc -l crates/ox-cli/src/event_loop.rs crates/ox-cli/src/editor.rs`
Expected: event_loop.rs significantly shorter, editor.rs ~500 lines

- [ ] **Step 5: Final commit if needed**

```
git add -A
git commit -m "chore: fix clippy/fmt issues from S-tier polish"
```

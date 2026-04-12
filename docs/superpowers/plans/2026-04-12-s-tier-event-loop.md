# S-Tier Event Loop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the screen extraction so each screen owns its state, type all remaining store protocols, delete the `cmd!` macro, and migrate UiStore tests to typed commands.

**Architecture:** Screen structs (`ThreadShell`, `SettingsShell`, `InboxShell`) own their local state. The `Outcome` enum gains `Quit` and `Action(AppAction)` variants so the event loop is a pure router. All remaining `cmd!` / `Record::parsed(Value::...)` call sites migrate to `write_typed`. The `InputKeyEvent` and `ApprovalResponse` types join `ox-types`. UiStore tests switch from `cmd_map`/`empty_cmd` to typed `UiCommand`.

**Tech Stack:** Rust, serde, structfs-core-store, structfs-serde-store, ox-types, ox-broker, ox-ui, ox-cli

**Spec:** `docs/superpowers/specs/2026-04-12-s-tier-event-loop-design.md`

---

### Task 1: Expand `Outcome` and add `AppAction`

**Files:**
- Modify: `crates/ox-cli/src/shell.rs`

- [ ] **Step 1: Replace shell.rs with expanded types**

Replace the contents of `crates/ox-cli/src/shell.rs`:

```rust
//! Shell types — platform-local state and dispatch for the TUI.

/// What a screen handler returns.
pub(crate) enum Outcome {
    /// Key wasn't handled — fall through to global dispatch.
    Ignored,
    /// State was updated, continue to next frame.
    Handled,
    /// Quit the application.
    Quit,
    /// A cross-cutting action that needs App-level resources.
    Action(AppAction),
}

/// Cross-cutting actions that require App-level resources.
pub(crate) enum AppAction {
    Compose { text: String },
    Reply { thread_id: String, text: String },
    ArchiveThread { thread_id: String },
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p ox-cli`
Expected: compiles (existing code only uses `Outcome::Handled` and `Outcome::Ignored`).

- [ ] **Step 3: Commit**

```bash
git add crates/ox-cli/src/shell.rs
git commit -m "feat(shell): expand Outcome with Quit and Action(AppAction)"
```

---

### Task 2: Create `ThreadShell` struct that owns editor state

**Files:**
- Modify: `crates/ox-cli/src/thread_shell.rs`
- Modify: `crates/ox-cli/src/event_loop.rs`

- [ ] **Step 1: Rewrite thread_shell.rs with struct that owns state**

Replace `crates/ox-cli/src/thread_shell.rs`:

```rust
//! Thread screen — owns editor state and handles compose/reply key dispatch.

use crate::editor::{
    EditorMode, InputSession, execute_command_input, flush_pending_edits,
    handle_editor_command_key, handle_editor_insert_key, handle_editor_normal_key,
    submit_editor_content,
};
use crate::shell::{AppAction, Outcome};
use crossterm::event::{KeyCode, KeyModifiers};
use ox_path::oxpath;
use ox_types::{InsertContext, Mode, UiCommand};
use ox_ui::text_input_store::EditSource;

/// TUI-local state for the thread screen.
pub(crate) struct ThreadShell {
    pub input_session: InputSession,
    pub text_input_view: crate::text_input_view::TextInputView,
    pub prev_mode: Mode,
}

impl ThreadShell {
    pub fn new() -> Self {
        Self {
            input_session: InputSession::new(),
            text_input_view: crate::text_input_view::TextInputView::new(),
            prev_mode: Mode::Normal,
        }
    }

    /// Sync editor state when the broker's mode changes.
    pub fn sync_mode(&mut self, ui_mode: Mode, input_content: &str, input_cursor: usize) {
        if ui_mode != self.prev_mode {
            if ui_mode == Mode::Insert {
                self.input_session.init_from(input_content.to_string(), input_cursor);
                self.input_session.editor_mode = EditorMode::Insert;
            }
            self.prev_mode = ui_mode;
        }
    }

    /// Flush pending edits to the broker (call on mode exit or after events).
    pub async fn flush(&mut self, client: &ox_broker::ClientHandle) {
        flush_pending_edits(&mut self.input_session, client).await;
    }

    /// Handle ESC interception for editor sub-modes.
    pub fn handle_esc(&mut self, key_str: &str, insert_context: Option<InsertContext>) -> Outcome {
        if key_str == "Esc"
            && insert_context != Some(InsertContext::Search)
            && insert_context != Some(InsertContext::Command)
        {
            match self.input_session.editor_mode {
                EditorMode::Insert => {
                    self.input_session.editor_mode = EditorMode::Normal;
                    return Outcome::Handled;
                }
                EditorMode::Command => {
                    self.input_session.command_buffer.clear();
                    self.input_session.editor_mode = EditorMode::Normal;
                    return Outcome::Handled;
                }
                EditorMode::Normal => {}
            }
        }
        Outcome::Ignored
    }

    /// Handle unbound insert-mode keys (after InputStore dispatch fails).
    pub async fn handle_unbound_insert(
        &mut self,
        insert_context: Option<InsertContext>,
        app: &mut crate::app::App,
        client: &ox_broker::ClientHandle,
        terminal_width: u16,
        modifiers: KeyModifiers,
        code: KeyCode,
    ) -> Outcome {
        if insert_context == Some(InsertContext::Search) {
            dispatch_search_edit(client, modifiers, code).await;
        } else if insert_context == Some(InsertContext::Command) {
            handle_editor_insert_key(&mut self.input_session, modifiers, code);
        } else {
            match self.input_session.editor_mode {
                EditorMode::Insert => {
                    handle_editor_insert_key(&mut self.input_session, modifiers, code);
                }
                EditorMode::Normal => {
                    handle_editor_normal_key(
                        &mut self.input_session,
                        app,
                        client,
                        terminal_width,
                        code,
                    )
                    .await;
                }
                EditorMode::Command => {
                    handle_editor_command_key(&mut self.input_session, app, client, code).await;
                }
            }
        }
        Outcome::Handled
    }

    /// Handle send_input pending action — returns Outcome with AppAction if composing.
    pub async fn handle_send_input(
        &mut self,
        insert_context: Option<InsertContext>,
        app: &mut crate::app::App,
        client: &ox_broker::ClientHandle,
    ) -> Outcome {
        if insert_context == Some(InsertContext::Command) {
            self.flush(client).await;
            execute_command_input(&self.input_session.content, client).await;
            let _ = client
                .write_typed(&oxpath!("ui"), &UiCommand::ClearInput)
                .await;
            let _ = client
                .write_typed(&oxpath!("ui"), &UiCommand::ExitInsert)
                .await;
            self.input_session.reset_after_submit();
            Outcome::Handled
        } else {
            let new_tid =
                submit_editor_content(&mut self.input_session, app, client).await;
            if let Some(tid) = new_tid {
                let _ = client
                    .write_typed(&oxpath!("ui"), &UiCommand::Open { thread_id: tid })
                    .await;
            }
            Outcome::Handled
        }
    }

    /// Handle paste events.
    pub fn handle_paste(&mut self, text: &str, insert_context: Option<InsertContext>) {
        if insert_context != Some(InsertContext::Search) {
            self.input_session.insert(text, EditSource::Paste);
        }
    }
}

/// Search text editing — dispatched through UiStore via broker.
async fn dispatch_search_edit(
    client: &ox_broker::ClientHandle,
    modifiers: KeyModifiers,
    code: KeyCode,
) {
    match (modifiers, code) {
        (_, KeyCode::Enter) => {
            let _ = client
                .write_typed(&oxpath!("ui"), &UiCommand::SearchSaveChip)
                .await;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
            let _ = client
                .write_typed(&oxpath!("ui"), &UiCommand::SearchClear)
                .await;
        }
        (_, KeyCode::Backspace) => {
            let _ = client
                .write_typed(&oxpath!("ui"), &UiCommand::SearchDeleteChar)
                .await;
        }
        (_, KeyCode::Char(c)) => {
            let _ = client
                .write_typed(&oxpath!("ui"), &UiCommand::SearchInsertChar { char: c })
                .await;
        }
        _ => {}
    }
}
```

- [ ] **Step 2: Update event_loop.rs to use ThreadShell**

In `crates/ox-cli/src/event_loop.rs`:

1. Remove `let mut input_session = InputSession::new();`
2. Remove `let mut text_input_view = crate::text_input_view::TextInputView::new();`
3. Remove `let mut prev_mode = Mode::Normal;`
4. Add `let mut thread = crate::thread_shell::ThreadShell::new();`
5. Replace all `input_session` references with `thread.input_session`
6. Replace all `text_input_view` references with `thread.text_input_view`
7. Replace the mode-transition sync block (lines 123-134) with: `thread.sync_mode(vs.ui.mode, &vs.ui.input.content, vs.ui.input.cursor);` and call `thread.flush(client).await;` when exiting insert mode
8. Replace `text_input_view.set_state(...)` with `thread.text_input_view.set_state(...)`
9. Replace the ESC intercept call with `thread.handle_esc(&key_str, insert_context_owned)`
10. Replace `crate::thread_shell::handle_unbound_insert_key(...)` with `thread.handle_unbound_insert(...).await`
11. Replace the SendInput pending action block with `thread.handle_send_input(insert_context_owned, app, client).await`
12. Replace the Paste handler with `thread.handle_paste(&text, insert_context_owned)`
13. Move `dispatch_search_edit` function out of event_loop.rs (it's now in thread_shell.rs)
14. Replace `flush_pending_edits(&mut input_session, client).await` at the end with `thread.flush(client).await`

- [ ] **Step 3: Remove old standalone functions from thread_shell.rs**

Delete the old `handle_esc_intercept` and `handle_unbound_insert_key` standalone functions (they're now methods on `ThreadShell`).

- [ ] **Step 4: Verify it compiles and tests pass**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: compiles, all 148 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/thread_shell.rs crates/ox-cli/src/event_loop.rs
git commit -m "refactor: ThreadShell owns editor state, event loop delegates"
```

---

### Task 3: Create `SettingsShell` struct that owns settings state

**Files:**
- Modify: `crates/ox-cli/src/settings_shell.rs`
- Modify: `crates/ox-cli/src/event_loop.rs`

- [ ] **Step 1: Add SettingsShell struct wrapping existing functions**

At the top of `crates/ox-cli/src/settings_shell.rs`, add a struct and constructor that owns `SettingsState`, and add a `poll()` method for test connection checking:

```rust
use crate::settings_state::{DIALECTS, SettingsFocus, SettingsState, TestStatus, WizardStep};
use crate::shell::Outcome;
use ox_path::oxpath;
use ox_types::UiCommand;
use structfs_core_store::{Record, Value};

/// TUI-local state for the settings screen.
pub(crate) struct SettingsShell {
    pub state: SettingsState,
}

impl SettingsShell {
    pub fn new() -> Self {
        Self {
            state: SettingsState::new(),
        }
    }

    pub fn new_wizard() -> Self {
        Self {
            state: SettingsState::new_wizard(),
        }
    }

    /// Poll pending async test connection (call each frame).
    pub fn poll(&mut self) {
        if let Some(ref mut rx) = self.state.pending_test {
            match rx.try_recv() {
                Ok(result) => {
                    match result.test {
                        Ok((dialect, ms)) => {
                            self.state.test_status =
                                TestStatus::Success(format!("Connected ({dialect}, {ms}ms)"));
                        }
                        Err(e) => {
                            self.state.test_status = TestStatus::Failed(e);
                        }
                    }
                    match result.models {
                        Ok(models) => {
                            self.state.discovered_models = models;
                            self.state.model_picker_idx = None;
                        }
                        Err(_) => {
                            self.state.discovered_models.clear();
                        }
                    }
                    self.state.pending_test = None;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.state.test_status = TestStatus::Failed("Test cancelled".into());
                    self.state.pending_test = None;
                }
            }
        }
    }

    /// Populate accounts from config if empty (call when entering settings screen).
    pub fn ensure_accounts(&mut self, inbox_root: &std::path::Path) {
        if self.state.accounts.is_empty() {
            let config = crate::config::resolve_config(
                inbox_root,
                &crate::config::CliOverrides::default(),
            );
            self.state.refresh_accounts(&config, &inbox_root.join("keys"));
        }
    }
}
```

2. Update the existing `handle_key` function to take `&mut SettingsShell` instead of `&mut SettingsState`:

```rust
pub(crate) async fn handle_key(
    shell: &mut SettingsShell,
    key_str: &str,
    client: &ox_broker::ClientHandle,
    inbox_root: &std::path::Path,
) -> Outcome {
    let settings = &mut shell.state;
    // ... rest unchanged, just uses `settings` everywhere
```

And update the sub-functions (`handle_edit_dialog_key`, `handle_delete_confirm_key`, `handle_navigation_key`) similarly.

- [ ] **Step 2: Update event_loop.rs to use SettingsShell**

In `crates/ox-cli/src/event_loop.rs`:

1. Remove `use crate::settings_state::SettingsState;`
2. Replace `let mut settings = if needs_setup { ... }` with:
   ```rust
   let mut settings_shell = if needs_setup {
       client.write_typed(&oxpath!("ui"), &UiCommand::GoToSettings).await.ok();
       crate::settings_shell::SettingsShell::new_wizard()
   } else {
       crate::settings_shell::SettingsShell::new()
   };
   ```
3. Replace the test connection polling block (lines 60-94) with: `settings_shell.poll();`
4. Replace the accounts population block (lines 240-247) with: `if screen_owned == Screen::Settings { settings_shell.ensure_accounts(app.pool.inbox_root()); }`
5. Replace `crate::settings_shell::handle_key(&mut settings, ...)` with `crate::settings_shell::handle_key(&mut settings_shell, ...)`
6. Replace all `&settings` in the draw call with `&settings_shell.state`
7. Replace `settings.editing` references in mouse handling with `settings_shell.state.editing`

- [ ] **Step 3: Verify it compiles and tests pass**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: compiles, all 148 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-cli/src/settings_shell.rs crates/ox-cli/src/event_loop.rs
git commit -m "refactor: SettingsShell owns settings state, poll, and account init"
```

---

### Task 4: Event loop uses `Outcome::Quit` and `Outcome::Action`

**Files:**
- Modify: `crates/ox-cli/src/event_loop.rs`

- [ ] **Step 1: Replace pending_action dispatch with Outcome-based flow**

In the pending_action handling section of event_loop.rs, change `PendingAction::Quit => return Ok(())` to produce `Outcome::Quit`, and `PendingAction::ArchiveSelected` to produce `Outcome::Action(AppAction::ArchiveThread { ... })`.

The pending action block becomes:

```rust
        if let Some(action) = &pending_action {
            let outcome = match action {
                PendingAction::SendInput => {
                    thread.handle_send_input(insert_context_owned, app, client).await
                }
                PendingAction::Quit => Outcome::Quit,
                PendingAction::OpenSelected => {
                    if let Some(id) = &selected_thread_id {
                        let _ = client
                            .write_typed(&oxpath!("ui"), &UiCommand::Open { thread_id: id.clone() })
                            .await;
                    }
                    Outcome::Handled
                }
                PendingAction::ArchiveSelected => {
                    if let Some(id) = &selected_thread_id {
                        Outcome::Action(AppAction::ArchiveThread { thread_id: id.clone() })
                    } else {
                        Outcome::Handled
                    }
                }
            };
            let _ = client.write_typed(&oxpath!("ui"), &UiCommand::ClearPendingAction).await;
            match outcome {
                Outcome::Quit => return Ok(()),
                Outcome::Action(app_action) => {
                    execute_app_action(app, &app_action);
                }
                _ => {}
            }
        }
```

- [ ] **Step 2: Add `execute_app_action` function**

Add to event_loop.rs:

```rust
fn execute_app_action(app: &mut App, action: &crate::shell::AppAction) {
    use crate::shell::AppAction;
    match action {
        AppAction::ArchiveThread { thread_id } => {
            let update_path = ox_path::oxpath!("threads", thread_id);
            let mut map = std::collections::BTreeMap::new();
            map.insert(
                "inbox_state".to_string(),
                structfs_core_store::Value::String("done".to_string()),
            );
            app.pool
                .inbox()
                .write(
                    &update_path,
                    structfs_core_store::Record::parsed(structfs_core_store::Value::Map(map)),
                )
                .ok();
        }
        AppAction::Compose { text } => {
            // Future use — compose is currently handled via submit_editor_content
        }
        AppAction::Reply { thread_id, text } => {
            // Future use — reply is currently handled via submit_editor_content
        }
    }
}
```

- [ ] **Step 3: Remove the `cmd!` import from event_loop.rs**

Remove `use structfs_core_store::Writer as StructWriter;` if no longer used. Check if `cmd!` is still used anywhere in event_loop.rs — if not, the import is no longer needed.

- [ ] **Step 4: Verify it compiles and tests pass**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/event_loop.rs
git commit -m "refactor: event loop uses Outcome::Quit and Outcome::Action for dispatch"
```

---

### Task 5: Add `InputKeyEvent` to ox-types, type the input/key write

**Files:**
- Modify: `crates/ox-types/src/lib.rs`
- Create: `crates/ox-types/src/input.rs`
- Modify: `crates/ox-ui/src/input_store.rs`
- Modify: `crates/ox-cli/src/event_loop.rs`

- [ ] **Step 1: Add InputKeyEvent type to ox-types**

Create `crates/ox-types/src/input.rs`:

```rust
//! Input event types for key dispatch across the StructFS boundary.

use serde::{Deserialize, Serialize};

use crate::ui::{Mode, Screen};

/// A key event for InputStore dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputKeyEvent {
    pub mode: Mode,
    pub key: String,
    pub screen: Screen,
}
```

Update `crates/ox-types/src/lib.rs`:

```rust
pub mod approval;
pub mod command;
pub mod input;
pub mod snapshot;
pub mod turn;
pub mod ui;

pub use approval::*;
pub use command::*;
pub use input::*;
pub use snapshot::*;
pub use turn::*;
pub use ui::*;
```

- [ ] **Step 2: Update InputStore Writer to accept typed InputKeyEvent**

In `crates/ox-ui/src/input_store.rs`, in the Writer's `"key"` arm, add a typed path before the manual map parsing:

```rust
            "key" => {
                // Try typed InputKeyEvent first
                if let Ok(evt) = structfs_serde_store::from_value::<ox_types::InputKeyEvent>(value.clone()) {
                    let mode_str = match evt.mode {
                        ox_types::Mode::Normal => "normal",
                        ox_types::Mode::Insert => "insert",
                    };
                    let screen_str = match evt.screen {
                        ox_types::Screen::Inbox => "inbox",
                        ox_types::Screen::Thread => "thread",
                        ox_types::Screen::Settings => "settings",
                    };
                    let binding = self
                        .resolve(mode_str, &evt.key, Some(screen_str))
                        .ok_or_else(|| StoreError::store("input", "key", "no binding for key"))?;
                    let action = binding.action.clone();
                    // Build a context map for execute_action compatibility
                    let mut context = BTreeMap::new();
                    context.insert("mode".to_string(), Value::String(mode_str.to_string()));
                    context.insert("key".to_string(), Value::String(evt.key));
                    context.insert("screen".to_string(), Value::String(screen_str.to_string()));
                    return self.execute_action(&action, &context);
                }

                // Fallback: manual map parsing (legacy)
                let map = match value {
                    // ... existing code unchanged
```

- [ ] **Step 3: Update event_loop.rs call site**

In `crates/ox-cli/src/event_loop.rs`, replace the InputStore dispatch:

```rust
                        // Try InputStore dispatch
                        let result = client
                            .write_typed(
                                &oxpath!("input", "key"),
                                &ox_types::InputKeyEvent {
                                    mode: mode_owned,
                                    key: key_str.clone(),
                                    screen: screen_owned,
                                },
                            )
                            .await;
```

Remove the `mode_str` and `screen_str` local variables (no longer needed).

- [ ] **Step 4: Verify it compiles and tests pass**

Run: `cargo check -p ox-cli && cargo test -p ox-cli && cargo test -p ox-ui`

- [ ] **Step 5: Commit**

```bash
git add crates/ox-types/ crates/ox-ui/src/input_store.rs crates/ox-cli/src/event_loop.rs
git commit -m "feat: add InputKeyEvent type, type the input/key write path"
```

---

### Task 6: Add `ApprovalResponse` to ox-types, type approval writes

**Files:**
- Create: `crates/ox-types/src/approval.rs` (modify existing)
- Modify: `crates/ox-cli/src/key_handlers.rs`

- [ ] **Step 1: Add ApprovalResponse to ox-types**

In `crates/ox-types/src/approval.rs`, add:

```rust
/// A response to an approval request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub decision: String,
}
```

- [ ] **Step 2: Update send_approval_response in key_handlers.rs**

In `crates/ox-cli/src/key_handlers.rs`, replace `send_approval_response`:

```rust
pub(crate) async fn send_approval_response(
    client: &ox_broker::ClientHandle,
    active_thread_id: &Option<String>,
    response: &str,
) {
    if let Some(tid) = active_thread_id {
        let path = ox_path::oxpath!("threads", tid, "approval", "response");
        let _ = client
            .write_typed(&path, &ox_types::ApprovalResponse {
                decision: response.to_string(),
            })
            .await;
    }
}
```

Remove the `use structfs_core_store::{Record, Value};` import if no longer used in `send_approval_response`.

- [ ] **Step 3: Also update the approval pending read to use read_typed**

In `key_handlers.rs`, the `handle_approval_key` function reads `approval/pending` with manual `Value::Map` destructuring. Replace with `read_typed`:

```rust
                if let Ok(Some(req)) = client
                    .read_typed::<ox_types::ApprovalRequest>(&pending_path)
                    .await
                {
                    dialog.pending_customize = Some(CustomizeState::new(
                        &req.tool_name,
                        &req.input_preview,
                    ));
                }
```

- [ ] **Step 4: Verify it compiles and tests pass**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`

- [ ] **Step 5: Commit**

```bash
git add crates/ox-types/src/approval.rs crates/ox-cli/src/key_handlers.rs
git commit -m "feat: add ApprovalResponse type, type approval write path"
```

---

### Task 7: Type config writes in settings_shell.rs

**Files:**
- Modify: `crates/ox-cli/src/settings_shell.rs`

- [ ] **Step 1: Replace `Record::parsed(Value::String(...))` with `write_typed`**

In `crates/ox-cli/src/settings_shell.rs`, find all `Record::parsed(Value::String(...))` and `Record::parsed(Value::Integer(...))` and `Record::parsed(Value::Null)` patterns. Replace with `write_typed`:

For string writes:
```rust
// Before
client.write(&provider_path, Record::parsed(Value::String(provider))).await.ok();
// After
client.write_typed(&provider_path, &provider).await.ok();
```

For integer writes:
```rust
// Before
client.write(&path, Record::parsed(Value::Integer(max_tokens))).await.ok();
// After
client.write_typed(&path, &max_tokens).await.ok();
```

For null writes (delete/save commands): these are store-specific commands that write Null to signal deletion or save. Keep them as `Record::parsed(Value::Null)` since `write_typed(&path, &())` would serialize as `null` which is correct:
```rust
// Before
client.write(&oxpath!("config", "save"), Record::parsed(Value::Null)).await.ok();
// After
client.write_typed(&oxpath!("config", "save"), &serde_json::Value::Null).await.ok();
```

Actually, the simplest approach: `Value::Null` through write_typed won't work cleanly because `()` serializes differently. Keep `Record::parsed(Value::Null)` writes as-is — they're store protocol signals, not typed data.

Focus on replacing the `Value::String(...)` and `Value::Integer(...)` writes only.

- [ ] **Step 2: Remove `use structfs_core_store::{Record, Value};` if possible**

Check if `Record` and `Value` are still used (they will be for the Null writes). If still needed, keep the import.

- [ ] **Step 3: Verify it compiles and tests pass**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`

- [ ] **Step 4: Commit**

```bash
git add crates/ox-cli/src/settings_shell.rs
git commit -m "refactor: config writes use write_typed for string and integer values"
```

---

### Task 8: Delete `broker_cmd.rs` and `cmd!` macro

**Files:**
- Delete: `crates/ox-cli/src/broker_cmd.rs`
- Modify: `crates/ox-cli/src/main.rs`
- Modify: `crates/ox-cli/src/event_loop.rs` (if `cmd!` still used)

- [ ] **Step 1: Verify no remaining `cmd!` usage**

Search for `cmd!` in the entire ox-cli crate. If any remain, migrate them first.

Run: `grep -rn 'cmd!' crates/ox-cli/src/ --include='*.rs'`

The only remaining usage should be in `broker_cmd.rs` itself (the tests and macro definition). If other files still use `cmd!`, fix them first.

- [ ] **Step 2: Remove the module declaration**

In `crates/ox-cli/src/main.rs`, remove:
```rust
#[macro_use]
mod broker_cmd;
```
(or `mod broker_cmd;` — check the exact form)

- [ ] **Step 3: Delete the file**

```bash
rm crates/ox-cli/src/broker_cmd.rs
```

- [ ] **Step 4: Verify it compiles and tests pass**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`

- [ ] **Step 5: Commit**

```bash
git add -A crates/ox-cli/
git commit -m "refactor: delete broker_cmd.rs and cmd! macro — all writes are typed"
```

---

### Task 9: Migrate UiStore tests to typed UiCommand

**Files:**
- Modify: `crates/ox-ui/src/ui_store.rs` (test module only)

- [ ] **Step 1: Replace test helper with typed helper**

In the `#[cfg(test)] mod tests` block of `crates/ox-ui/src/ui_store.rs`, replace `cmd_map` and `empty_cmd` with a typed helper:

```rust
    fn typed_cmd(cmd: &ox_types::UiCommand) -> Record {
        Record::parsed(structfs_serde_store::to_value(cmd).unwrap())
    }
```

- [ ] **Step 2: Migrate all tests to use typed_cmd**

Replace each `empty_cmd()` and `cmd_map(...)` usage with `typed_cmd(&UiCommand::...)`. Examples:

```rust
// Before
store.write(&path!("select_next"), empty_cmd()).unwrap();
// After
store.write(&path!(""), typed_cmd(&UiCommand::SelectNext)).unwrap();

// Before
store.write(&path!("open"), cmd_map(&[("thread_id", Value::String("t_001".into()))])).unwrap();
// After
store.write(&path!(""), typed_cmd(&UiCommand::Open { thread_id: "t_001".into() })).unwrap();

// Before
store.write(&path!("enter_insert"), cmd_map(&[("context", Value::String("compose".into()))])).unwrap();
// After
store.write(&path!(""), typed_cmd(&UiCommand::EnterInsert { context: InsertContext::Compose })).unwrap();

// Before
store.write(&path!("set_row_count"), cmd_map(&[("count", Value::Integer(5))])).unwrap();
// After
store.write(&path!(""), typed_cmd(&UiCommand::SetRowCount { count: 5 })).unwrap();

// Before  
store.write(&path!("set_scroll_max"), cmd_map(&[("max", Value::Integer(5))])).unwrap();
// After
store.write(&path!(""), typed_cmd(&UiCommand::SetScrollMax { max: 5 })).unwrap();
```

Note: all typed commands write to `path!("")` (root), not to the command name as a path component. The command name is inside the serialized `UiCommand`.

Keep the `read_str` helper — it's still useful for reading back values.

EXCEPTION: The txn dedup test uses a `"txn"` field in the map. Typed `UiCommand` doesn't carry txn. Keep this test using the old `cmd_map` approach, or skip migrating it (txn dedup tests the legacy path).

- [ ] **Step 3: Remove `cmd_map` and `empty_cmd` helpers if no longer used**

If the txn test still needs them, keep them. Otherwise delete.

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-ui`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-ui/src/ui_store.rs
git commit -m "test(ox-ui): migrate UiStore tests to typed UiCommand"
```

---

### Task 10: Remove legacy string-based Writer path from UiStore

**Files:**
- Modify: `crates/ox-ui/src/ui_store.rs`

- [ ] **Step 1: Remove the string-based command dispatch**

In the `Writer::write` method of UiStore, the flow is currently:
1. Delegate `input/*` to TextInputStore
2. Try typed `UiCommand` via `from_value` on root path
3. Fall back to string-based `Command::parse` + match on command name

Remove step 3 — the entire `match command { "select_next" => ..., "open" => ..., ... }` block. The typed path is now the only path.

Update the Writer:

```rust
impl Writer for UiStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let command = if to.is_empty() {
            ""
        } else {
            to.components[0].as_str()
        };

        // Delegate input/* writes to TextInputStore
        if command == "input" {
            let sub = if to.components.len() > 1 {
                Path::parse(&to.components[1..].join("/")).unwrap_or_else(|_| path!(""))
            } else {
                path!("")
            };
            return self.text_input_store.write(&sub, data);
        }

        let value = data
            .as_value()
            .ok_or_else(|| StoreError::store("ui", "write", "write data must contain a value"))?;

        // Typed UiCommand dispatch
        let ui_cmd: UiCommand = structfs_serde_store::from_value(value.clone())
            .map_err(|e| StoreError::store("ui", "write", &format!("invalid command: {e}")))?;

        self.handle_ui_command(ui_cmd)
    }
}
```

- [ ] **Step 2: Remove `Command` import if no longer used**

Remove `use crate::command::{Command, TxnLog};` and the `txn_log` field from `UiStore` if txn dedup is no longer needed (typed commands don't carry txn). If txn dedup is still desired, keep it but integrate into `handle_ui_command`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-ui`
Expected: all tests pass (they were migrated in Task 9). The txn dedup test may need updating if the txn_log was removed.

- [ ] **Step 4: Run full workspace check**

Run: `cargo check && cargo test`

- [ ] **Step 5: Commit**

```bash
git add crates/ox-ui/src/ui_store.rs
git commit -m "refactor: remove legacy string-based command dispatch from UiStore Writer"
```

---

### Task 11: Quality gates

**Files:** None — verification only.

- [ ] **Step 1: Run the full quality gate script**

Run: `./scripts/quality_gates.sh`
Expected: 15/15 pass.

- [ ] **Step 2: Fix any failures and commit**

```bash
./scripts/fmt.sh
git add -A
git commit -m "fix: quality gate cleanup for S-tier event loop migration"
```

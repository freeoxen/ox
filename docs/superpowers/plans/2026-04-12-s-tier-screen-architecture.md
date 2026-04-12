# S-Tier Screen Architecture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the flat UiCommand/UiSnapshot/UiStore with screen-scoped commands, a screen-discriminated snapshot enum, and per-screen state — making invalid screen states unrepresentable. Type all remaining store protocols. Delete the `cmd!` macro.

**Architecture:** `UiCommand` becomes `UiCommand::Inbox(InboxCommand) | Thread(ThreadCommand) | Settings(SettingsCommand) | Global(GlobalCommand)`. `UiSnapshot` becomes a tagged enum whose variant carries only the active screen's state. UiStore holds an `ActiveScreen` enum and routes commands to the active screen's handler. The CLI shell matches on the snapshot variant and dispatches to per-screen handlers. Remaining untyped protocols (InputKeyEvent, ApprovalResponse, config writes) get typed. The `cmd!` macro is deleted.

**Tech Stack:** Rust, serde, structfs-core-store, structfs-serde-store, ox-types, ox-broker, ox-ui, ox-cli

**Spec:** `docs/superpowers/specs/2026-04-12-s-tier-event-loop-design.md`

---

## File Structure

**ox-types (types only, no behavior):**
- `src/ui.rs` — Screen, Mode, InsertContext, PendingAction, SettingsFocus, WizardStep, AccountEditFields
- `src/command.rs` — UiCommand (hierarchical), GlobalCommand, InboxCommand, ThreadCommand, SettingsCommand
- `src/snapshot.rs` — UiSnapshot (enum), InboxSnapshot, ThreadSnapshot, SettingsSnapshot, InputSnapshot, SearchSnapshot
- `src/input.rs` — InputKeyEvent (NEW)
- `src/approval.rs` — ApprovalRequest, ApprovalResponse (NEW)
- `src/turn.rs` — ToolStatus, TokenUsage (unchanged)

**ox-ui (shared core, behavior):**
- `src/ui_store.rs` — UiStore with ActiveScreen enum, per-screen handlers, router Writer

**ox-cli (TUI shell):**
- `src/shell.rs` — Outcome enum (Handled/Ignored)
- `src/thread_shell.rs` — ThreadShell struct owning InputSession, TextInputView, prev_mode
- `src/settings_shell.rs` — SettingsShell struct owning TUI-local state (test connection, discovered models, account summaries)
- `src/inbox_shell.rs` — InboxShell (stateless, delegates to broker)
- `src/event_loop.rs` — thin router
- `src/view_state.rs` — ViewState with screen-discriminated UiSnapshot
- `src/key_handlers.rs` — typed approval writes
- `src/broker_cmd.rs` — DELETED

---

### Task 1: Restructure ox-types with screen-scoped commands and snapshots

This is the foundation — all other tasks depend on it. Replace the flat `UiCommand` with hierarchical commands and flat `UiSnapshot` with a screen-discriminated enum.

**Files:**
- Modify: `crates/ox-types/src/ui.rs`
- Modify: `crates/ox-types/src/command.rs`
- Modify: `crates/ox-types/src/snapshot.rs`
- Modify: `crates/ox-types/src/approval.rs`
- Create: `crates/ox-types/src/input.rs`
- Modify: `crates/ox-types/src/lib.rs`
- Modify: `crates/ox-types/tests/serde_roundtrip.rs`

- [ ] **Step 1: Add settings-related types to ui.rs**

In `crates/ox-types/src/ui.rs`, add after the existing enums:

```rust
/// Which section of settings has focus.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsFocus {
    #[default]
    Accounts,
    Defaults,
}

/// Wizard step for guided setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WizardStep {
    AddAccount,
    SetDefaults,
    Done,
}

/// Fields for the account add/edit dialog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountEditFields {
    pub name: String,
    pub dialect: usize,
    pub endpoint: String,
    pub key: String,
    pub focus: usize,
    pub is_new: bool,
}
```

- [ ] **Step 2: Replace command.rs with hierarchical commands**

Replace the entire contents of `crates/ox-types/src/command.rs`:

```rust
//! Typed commands for writing to UiStore across the StructFS boundary.
//!
//! Commands are hierarchical — scoped to a screen or global.
//! UiStore routes screen-scoped commands to the active screen's handler
//! and rejects commands for inactive screens.

use serde::{Deserialize, Serialize};

use crate::ui::InsertContext;

/// Top-level command for UiStore writes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "scope", content = "command", rename_all = "snake_case")]
pub enum UiCommand {
    Global(GlobalCommand),
    Inbox(InboxCommand),
    Thread(ThreadCommand),
    Settings(SettingsCommand),
}

/// Commands that work regardless of which screen is active.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum GlobalCommand {
    Quit,
    Open { thread_id: String },
    Close,
    GoToSettings,
    GoToInbox,
    SetStatus { text: String },
    ClearPendingAction,
}

/// Commands scoped to the inbox screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum InboxCommand {
    SelectNext,
    SelectPrev,
    SelectFirst,
    SelectLast,
    SetRowCount { count: usize },
    OpenSelected,
    ArchiveSelected,
    SearchInsertChar { char: char },
    SearchDeleteChar,
    SearchClear,
    SearchSaveChip,
    SearchDismissChip { index: usize },
}

/// Commands scoped to the thread screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ThreadCommand {
    ScrollUp,
    ScrollDown,
    ScrollToTop,
    ScrollToBottom,
    ScrollPageUp,
    ScrollPageDown,
    ScrollHalfPageUp,
    ScrollHalfPageDown,
    SetScrollMax { max: usize },
    SetViewportHeight { height: usize },
    EnterInsert { context: InsertContext },
    ExitInsert,
    SetInput { content: String, cursor: usize },
    ClearInput,
    SendInput,
}

/// Commands scoped to the settings screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum SettingsCommand {
    // Navigation
    FocusAccounts,
    FocusDefaults,
    ToggleFocus,
    SelectNextAccount,
    SelectPrevAccount,
    SelectNextDefault,
    SelectPrevDefault,
    // Account CRUD
    StartAddAccount,
    StartEditAccount {
        name: String,
        dialect: usize,
        endpoint: String,
        key: String,
    },
    StartDeleteAccount,
    ConfirmDelete,
    CancelDelete,
    // Edit dialog
    EditFocusNext,
    EditFocusPrev,
    EditFocusField { field: usize },
    EditDialectNext,
    EditDialectPrev,
    EditInsertChar { char: char },
    EditBackspace,
    EditSave {
        name: String,
        provider: String,
        endpoint: Option<String>,
        key: String,
    },
    EditCancel,
    // Defaults
    DefaultAccountNext,
    DefaultAccountPrev,
    DefaultModelNext,
    DefaultModelPrev,
    DefaultModelInsertChar { char: char },
    DefaultModelBackspace,
    DefaultMaxTokensInsertChar { char: char },
    DefaultMaxTokensBackspace,
    SaveDefaults {
        account: String,
        model: String,
        max_tokens: i64,
    },
    // Wizard
    FinishWizard,
}
```

- [ ] **Step 3: Replace snapshot.rs with screen-discriminated enum**

Replace the entire contents of `crates/ox-types/src/snapshot.rs`:

```rust
//! Typed snapshots for reading store state across the StructFS boundary.
//!
//! UiSnapshot is a screen-discriminated enum — the variant carries only
//! the active screen's state. Thread fields don't exist on the inbox screen.

use serde::{Deserialize, Serialize};

use crate::ui::{AccountEditFields, InsertContext, Mode, PendingAction, SettingsFocus, WizardStep};

/// Screen-discriminated UI state snapshot — the read contract for UiStore.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "screen", rename_all = "snake_case")]
pub enum UiSnapshot {
    Inbox(InboxSnapshot),
    Thread(ThreadSnapshot),
    Settings(SettingsSnapshot),
}

impl Default for UiSnapshot {
    fn default() -> Self {
        UiSnapshot::Inbox(InboxSnapshot::default())
    }
}

/// Inbox screen state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InboxSnapshot {
    pub selected_row: usize,
    pub row_count: usize,
    pub search: SearchSnapshot,
    pub pending_action: Option<PendingAction>,
}

/// Thread screen state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSnapshot {
    pub thread_id: String,
    pub mode: Mode,
    pub insert_context: Option<InsertContext>,
    pub scroll: usize,
    pub scroll_max: usize,
    pub viewport_height: usize,
    pub input: InputSnapshot,
    pub pending_action: Option<PendingAction>,
}

/// Settings screen state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettingsSnapshot {
    pub focus: SettingsFocus,
    pub selected_account: usize,
    pub editing: Option<AccountEditFields>,
    pub delete_confirming: bool,
    pub wizard: Option<WizardStep>,
    pub defaults_focus: usize,
    pub default_account_idx: usize,
    pub default_model: String,
    pub default_max_tokens: String,
    pub pending_action: Option<PendingAction>,
}

/// Text input state snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InputSnapshot {
    pub content: String,
    pub cursor: usize,
}

/// Search state snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchSnapshot {
    pub chips: Vec<String>,
    pub live_query: String,
    pub active: bool,
}
```

- [ ] **Step 4: Add InputKeyEvent and ApprovalResponse**

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

In `crates/ox-types/src/approval.rs`, add `ApprovalResponse`:

```rust
//! Approval types for tool call policy enforcement.

use serde::{Deserialize, Serialize};

/// An approval request from the agent for a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub tool_name: String,
    pub input_preview: String,
}

/// A response to an approval request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub decision: String,
}
```

- [ ] **Step 5: Update lib.rs**

Replace `crates/ox-types/src/lib.rs`:

```rust
//! Shared types for the ox agent framework.
//!
//! Pure data — no behavior, no Store impls. Leaf of the dependency tree.

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

- [ ] **Step 6: Update serde round-trip tests**

Replace `crates/ox-types/tests/serde_roundtrip.rs` with tests covering the new hierarchical types. Key tests:

- `UiCommand::Global(GlobalCommand::Quit)` serializes with `"scope": "global"` and `"command": {"command": "quit"}`
- `UiCommand::Inbox(InboxCommand::SelectNext)` serializes with `"scope": "inbox"`
- `UiCommand::Thread(ThreadCommand::ScrollUp)` serializes with `"scope": "thread"`
- `UiCommand::Settings(SettingsCommand::ToggleFocus)` serializes with `"scope": "settings"`
- `UiSnapshot::Inbox(InboxSnapshot::default())` serializes with `"screen": "inbox"`
- `UiSnapshot::Thread(ThreadSnapshot { ... })` serializes with `"screen": "thread"`
- `UiSnapshot::Settings(SettingsSnapshot::default())` serializes with `"screen": "settings"`
- `InputKeyEvent` round-trips
- `ApprovalResponse` round-trips
- `SettingsFocus`, `WizardStep`, `AccountEditFields` round-trip

Remove old tests that reference the flat `UiCommand` variants directly.

- [ ] **Step 7: Verify it compiles**

Run: `cargo check -p ox-types && cargo test -p ox-types`
Expected: compiles. Note: `ox-ui` and `ox-cli` will NOT compile yet (they reference the old flat UiCommand). That's expected — Tasks 2-3 fix them.

- [ ] **Step 8: Commit**

```bash
git add crates/ox-types/
git commit -m "feat(ox-types): screen-scoped commands, screen-discriminated snapshots, InputKeyEvent, ApprovalResponse"
```

---

### Task 2: Restructure UiStore with ActiveScreen enum and router Writer

The biggest task — UiStore changes from flat state to a screen-discriminated `ActiveScreen` enum with per-screen handlers.

**Files:**
- Modify: `crates/ox-ui/src/ui_store.rs`
- Modify: `crates/ox-ui/src/lib.rs`

- [ ] **Step 1: Replace the UiStore struct with ActiveScreen-based design**

Read `crates/ox-ui/src/ui_store.rs` fully. Then rewrite it with:

**Struct:**
```rust
pub struct UiStore {
    screen: ActiveScreen,
    text_input_store: TextInputStore,
    pending_action: Option<PendingAction>,
    status: Option<String>,
}

enum ActiveScreen {
    Inbox(InboxState),
    Thread(ThreadState),
    Settings(SettingsState),
}

struct InboxState {
    selected_row: usize,
    row_count: usize,
    search_chips: Vec<String>,
    search_live_query: String,
}

struct ThreadState {
    thread_id: String,
    mode: Mode,
    insert_context: Option<InsertContext>,
    scroll: usize,
    scroll_max: usize,
    viewport_height: usize,
}

struct SettingsState {
    focus: SettingsFocus,
    selected_account: usize,
    editing: Option<AccountEditFields>,
    delete_confirming: bool,
    wizard: Option<WizardStep>,
    defaults_focus: usize,
    default_account_idx: usize,
    default_model: String,
    default_max_tokens: String,
}
```

**Reader:** The `snapshot()` method returns `UiSnapshot` enum:
- `ActiveScreen::Inbox(state)` → `UiSnapshot::Inbox(InboxSnapshot { ... })`
- `ActiveScreen::Thread(state)` → `UiSnapshot::Thread(ThreadSnapshot { ... })`
- `ActiveScreen::Settings(state)` → `UiSnapshot::Settings(SettingsSnapshot { ... })`

Reading `""` returns `to_value(&self.snapshot())`.

**Writer:** Deserialize `UiCommand`, route by variant:
- `UiCommand::Global(cmd)` → `handle_global(cmd)` — screen transitions, quit, status
- `UiCommand::Inbox(cmd)` → check `ActiveScreen::Inbox`, dispatch or error
- `UiCommand::Thread(cmd)` → check `ActiveScreen::Thread`, dispatch or error
- `UiCommand::Settings(cmd)` → check `ActiveScreen::Settings`, dispatch or error

**Screen transition handlers:**
- `GlobalCommand::Open { thread_id }` → `self.screen = ActiveScreen::Thread(ThreadState { thread_id, mode: Mode::Normal, ... })`
- `GlobalCommand::Close` → `self.screen = ActiveScreen::Inbox(InboxState::default())`
- `GlobalCommand::GoToSettings` → `self.screen = ActiveScreen::Settings(SettingsState::default())`
- `GlobalCommand::GoToInbox` → `self.screen = ActiveScreen::Inbox(InboxState::default())`

**Per-screen handlers:** Each command enum gets its own handler method:
- `handle_inbox(&mut InboxState, InboxCommand)` — selection, search, pending actions
- `handle_thread(&mut ThreadState, ThreadCommand, &mut TextInputStore)` — scroll, mode, input
- `handle_settings(&mut SettingsState, SettingsCommand)` — navigation, edit, defaults

**Important:** The legacy string-based command path and `Command::parse()` fallback should be REMOVED. All commands go through the typed `UiCommand` path. The `TxnLog` can be removed (typed commands are idempotent by design).

- [ ] **Step 2: Write new tests using typed commands**

Replace the existing test module with tests that use `UiCommand` variants. Test helpers:

```rust
fn typed_cmd(cmd: &UiCommand) -> Record {
    Record::parsed(structfs_serde_store::to_value(cmd).unwrap())
}

fn read_snapshot(store: &mut UiStore) -> UiSnapshot {
    let record = store.read(&path!("")).unwrap().unwrap();
    structfs_serde_store::from_value(record.as_value().unwrap().clone()).unwrap()
}
```

Key test cases:
- Initial state is `UiSnapshot::Inbox(...)` with `selected_row: 0`
- `GlobalCommand::Open` transitions to `UiSnapshot::Thread(...)`
- `GlobalCommand::Close` transitions back to `UiSnapshot::Inbox(...)`
- `InboxCommand::SelectNext` advances row on inbox screen
- `InboxCommand::SelectNext` errors when on thread screen
- `ThreadCommand::ScrollUp` scrolls on thread screen
- `ThreadCommand::EnterInsert` changes mode
- `SettingsCommand::ToggleFocus` toggles settings focus
- `GlobalCommand::Quit` sets pending_action

- [ ] **Step 3: Update lib.rs exports**

In `crates/ox-ui/src/lib.rs`, update re-exports to include the new types from ox-types (the hierarchical commands, screen snapshots, etc.).

- [ ] **Step 4: Verify it compiles and tests pass**

Run: `cargo check -p ox-ui && cargo test -p ox-ui`
Expected: ox-ui compiles and tests pass. ox-cli will NOT compile yet (it references the old flat ViewState/UiSnapshot).

- [ ] **Step 5: Commit**

```bash
git add crates/ox-ui/
git commit -m "feat(ox-ui): ActiveScreen enum, screen-scoped command routing, remove legacy Writer"
```

---

### Task 3: Update ox-cli ViewState and fetch_view_state for screen-discriminated UiSnapshot

**Files:**
- Modify: `crates/ox-cli/src/view_state.rs`
- Modify: `crates/ox-cli/src/tui.rs`
- Modify: `crates/ox-cli/src/tab_bar.rs`
- Modify: `crates/ox-cli/src/inbox_view.rs`
- Modify: `crates/ox-cli/src/thread_view.rs`
- Modify: `crates/ox-cli/src/dialogs.rs`

- [ ] **Step 1: Restructure ViewState to hold UiSnapshot enum**

`ViewState` now holds the `UiSnapshot` enum directly. Since the snapshot is screen-discriminated, screen-specific data access requires matching on the variant. The flat fields (`messages`, `inbox_threads`, `turn`, etc.) stay as separate fields — they come from different broker reads, not from UiStore.

```rust
pub struct ViewState<'a> {
    pub ui: ox_types::UiSnapshot,

    // Data from other stores (screen-conditional reads)
    pub inbox_threads: Vec<InboxThread>,
    pub messages: Vec<ChatMessage>,
    pub turn: ox_history::TurnState,
    pub approval_pending: Option<ox_types::ApprovalRequest>,

    // Config
    pub model: String,
    pub provider: String,

    // App-borrowed
    pub input_history: &'a [String],
    pub approval_selected: usize,
    pub pending_customize: &'a Option<CustomizeState>,
    pub key_hints: Vec<(String, String)>,
    pub show_shortcuts: bool,
    pub editor_mode: crate::editor::EditorMode,
    pub editor_command_buffer: String,
}
```

- [ ] **Step 2: Rewrite fetch_view_state**

The `UiSnapshot` is now a screen-discriminated enum. Conditional reads match on the variant:

```rust
let ui: UiSnapshot = client.read_typed(&oxpath!("ui")).await.ok().flatten().unwrap_or_default();

let (inbox_threads, messages, turn, approval_pending) = match &ui {
    UiSnapshot::Inbox(_) => {
        let threads = /* read inbox/threads */;
        (threads, Vec::new(), TurnState::default(), None)
    }
    UiSnapshot::Thread(snap) => {
        let messages = /* read threads/{snap.thread_id}/history/messages */;
        let turn = /* read turn state */;
        let approval = /* read approval */;
        (Vec::new(), messages, turn, approval)
    }
    UiSnapshot::Settings(_) => {
        (Vec::new(), Vec::new(), TurnState::default(), None)
    }
};
```

For key_hints, derive mode and screen strings from the snapshot variant:
```rust
let (mode_str, screen_str) = match &ui {
    UiSnapshot::Inbox(_) => ("normal", "inbox"),
    UiSnapshot::Thread(snap) => (
        match snap.mode { Mode::Normal => "normal", Mode::Insert => "insert" },
        "thread",
    ),
    UiSnapshot::Settings(_) => ("normal", "settings"),
};
```

- [ ] **Step 3: Update all draw functions**

Every draw function that accesses `vs.ui.screen`, `vs.ui.mode`, `vs.ui.scroll`, etc. needs to match on `vs.ui` instead. The pattern:

In `tui.rs`, the screen routing becomes:
```rust
match &vs.ui {
    UiSnapshot::Inbox(snap) => { /* render inbox */ }
    UiSnapshot::Thread(snap) => { /* render thread, access snap.scroll, snap.mode */ }
    UiSnapshot::Settings(snap) => { /* render settings */ }
}
```

In `tab_bar.rs`:
```rust
match &vs.ui {
    UiSnapshot::Thread(snap) => { /* show thread title */ }
    _ => { /* show app title */ }
}
```

In `inbox_view.rs`, access `snap.selected_row`, `snap.search.active`, etc. from the `InboxSnapshot`.

In `thread_view.rs`, access `snap.scroll`, `snap.mode`, etc. from the `ThreadSnapshot`.

In `dialogs.rs`, approval dialog access stays the same (reads from `vs.approval_pending`).

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p ox-cli`
Expected: compiles (event_loop.rs will need updating in the next task, but check should pass if types align).

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/
git commit -m "refactor(ox-cli): ViewState uses screen-discriminated UiSnapshot"
```

---

### Task 4: Update event loop and shell handlers for screen-scoped commands

**Files:**
- Modify: `crates/ox-cli/src/event_loop.rs`
- Modify: `crates/ox-cli/src/thread_shell.rs`
- Modify: `crates/ox-cli/src/inbox_shell.rs`
- Modify: `crates/ox-cli/src/settings_shell.rs`
- Modify: `crates/ox-cli/src/app.rs`

- [ ] **Step 1: Update all write_typed calls to use hierarchical commands**

Every `write_typed(&oxpath!("ui"), &UiCommand::ScrollUp)` becomes `write_typed(&oxpath!("ui"), &UiCommand::Thread(ThreadCommand::ScrollUp))`. Map all old flat variants to their new scoped location:

**Global:** `Quit`, `Open`, `Close`, `GoToSettings`, `GoToInbox`, `SetStatus`, `ClearPendingAction`
**Inbox:** `SelectNext`, `SelectPrev`, `SelectFirst`, `SelectLast`, `SetRowCount`, `OpenSelected`, `ArchiveSelected`, `SearchInsertChar`, `SearchDeleteChar`, `SearchClear`, `SearchSaveChip`, `SearchDismissChip`
**Thread:** `ScrollUp`, `ScrollDown`, `ScrollToTop`, `ScrollToBottom`, `ScrollPageUp`, `ScrollPageDown`, `ScrollHalfPageUp`, `ScrollHalfPageDown`, `SetScrollMax`, `SetViewportHeight`, `EnterInsert`, `ExitInsert`, `SetInput`, `ClearInput`, `SendInput`
**Settings:** All settings navigation and CRUD via `SettingsCommand` variants

Search for `UiCommand::` in all ox-cli files and update each occurrence.

- [ ] **Step 2: Update event loop's ViewState extraction**

The event loop extracts owned copies from ViewState. With the screen-discriminated snapshot, extraction changes:

```rust
let pending_action = match &vs.ui {
    UiSnapshot::Inbox(snap) => snap.pending_action,
    UiSnapshot::Thread(snap) => snap.pending_action,
    UiSnapshot::Settings(snap) => snap.pending_action,
};
let screen_owned = vs.ui.clone();
// No more separate mode_owned, insert_context_owned — they're inside the variant
```

The key dispatch section matches on `screen_owned`:
```rust
match &screen_owned {
    UiSnapshot::Inbox(snap) => { /* inbox key handling */ }
    UiSnapshot::Thread(snap) => { /* thread key handling, access snap.mode, snap.insert_context */ }
    UiSnapshot::Settings(snap) => { /* settings key handling */ }
}
```

- [ ] **Step 3: Update thread_shell.rs**

`handle_esc_intercept` takes `insert_context: Option<InsertContext>` — this now comes from `ThreadSnapshot.insert_context`.

`handle_unbound_insert_key` — same pattern, gets insert_context from the snapshot.

Search edit dispatch (dispatch_search_edit) should NOT be in thread_shell — it's inbox-scoped. Move it to inbox_shell or keep it in event_loop for now if it's only used when `insert_context == Search` which is thread/inbox ambiguous.

Actually, search is entered from the inbox screen via `EnterInsert { context: Search }`. But that puts us in Thread-like insert mode on the inbox screen... this is an area where the screen-scoping reveals a design tension. For now, keep search edit dispatch in the event loop as a special case, and document it as a TODO for future resolution.

- [ ] **Step 4: Update settings_shell.rs to use SettingsCommand**

The settings shell currently does all its work via direct broker writes (`Record::parsed(Value::String(...))`). With SettingsCommand, the navigation and UI state changes should go through the broker as typed commands:

```rust
// Before: direct mutation
client.write_typed(&oxpath!("ui"), &UiCommand::GoToInbox).await;
// After: scoped
client.write_typed(&oxpath!("ui"), &UiCommand::Global(GlobalCommand::GoToInbox)).await;
```

For settings-specific state changes (edit dialog, focus, selection), use `SettingsCommand`:
```rust
client.write_typed(&oxpath!("ui"), &UiCommand::Settings(SettingsCommand::ToggleFocus)).await;
client.write_typed(&oxpath!("ui"), &UiCommand::Settings(SettingsCommand::EditSave { name, provider, endpoint, key })).await;
```

The config writes (`config/gate/accounts/...`, `config/save`) stay as-is — they're not UI commands, they're config store writes.

- [ ] **Step 5: Update app.rs send_input_with_text**

This method takes `mode` and `insert_context` as parameters. With screen-scoped snapshots, the caller passes the ThreadSnapshot values directly:

```rust
pub fn send_input_with_text(
    &mut self,
    text: String,
    mode: ox_types::Mode,
    insert_context: Option<ox_types::InsertContext>,
    active_thread: Option<&str>,
) -> Option<String>
```

This signature doesn't need to change — the caller just extracts these from `ThreadSnapshot` instead of flat `UiSnapshot`.

- [ ] **Step 6: Type the input/key write**

In event_loop.rs, replace the `cmd!` InputStore dispatch:

```rust
// Before
client.write(&oxpath!("input", "key"), cmd!("mode" => mode_str, "key" => key_str, "screen" => screen_str)).await;

// After
client.write_typed(&oxpath!("input", "key"), &ox_types::InputKeyEvent {
    mode: /* from snapshot */,
    key: key_str.clone(),
    screen: /* from snapshot variant */,
}).await;
```

Update InputStore's Writer in `crates/ox-ui/src/input_store.rs` to accept `InputKeyEvent` via `from_value` (with fallback to the existing map parsing for backward compat with bindings tests).

- [ ] **Step 7: Type approval response writes**

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

Also update the approval pending read to use `read_typed`:
```rust
if let Ok(Some(req)) = client.read_typed::<ox_types::ApprovalRequest>(&pending_path).await {
    // use req.tool_name, req.input_preview
}
```

- [ ] **Step 8: Verify it compiles and tests pass**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: compiles, all tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/ox-cli/ crates/ox-ui/src/input_store.rs
git commit -m "refactor: screen-scoped commands in event loop, typed input/approval protocols"
```

---

### Task 5: Shell structs own their state

**Files:**
- Modify: `crates/ox-cli/src/shell.rs`
- Modify: `crates/ox-cli/src/thread_shell.rs`
- Modify: `crates/ox-cli/src/settings_shell.rs`
- Modify: `crates/ox-cli/src/event_loop.rs`

- [ ] **Step 1: ThreadShell struct owns editor state**

Rewrite `crates/ox-cli/src/thread_shell.rs` as a struct:

```rust
pub(crate) struct ThreadShell {
    pub input_session: InputSession,
    pub text_input_view: crate::text_input_view::TextInputView,
    pub prev_mode: Mode,
}

impl ThreadShell {
    pub fn new() -> Self { ... }
    pub fn sync_mode(&mut self, snap: &ThreadSnapshot) { ... }
    pub async fn flush(&mut self, client: &ClientHandle) { ... }
    pub fn handle_esc(&mut self, key_str: &str, snap: &ThreadSnapshot) -> Outcome { ... }
    pub async fn handle_unbound_insert(&mut self, snap: &ThreadSnapshot, app: &mut App, client: &ClientHandle, terminal_width: u16, modifiers: KeyModifiers, code: KeyCode) -> Outcome { ... }
    pub async fn handle_send_input(&mut self, snap: &ThreadSnapshot, app: &mut App, client: &ClientHandle) -> Outcome { ... }
    pub fn handle_paste(&mut self, text: &str, snap: &ThreadSnapshot) { ... }
}
```

- [ ] **Step 2: SettingsShell struct owns TUI-local state**

Add struct to `crates/ox-cli/src/settings_shell.rs`:

```rust
pub(crate) struct SettingsShell {
    // TUI-local state (not in shared core)
    pub test_status: TestStatus,
    pub pending_test: Option<oneshot::Receiver<TestResult>>,
    pub discovered_models: Vec<ox_kernel::ModelInfo>,
    pub model_picker_idx: Option<usize>,
    pub accounts: Vec<AccountSummary>,
    pub save_flash_until: Option<std::time::Instant>,
}

impl SettingsShell {
    pub fn new() -> Self { ... }
    pub fn poll(&mut self) { ... }  // check pending_test oneshot
    pub fn ensure_accounts(&mut self, inbox_root: &Path) { ... }
    pub async fn handle_key(&mut self, key_str: &str, snap: &SettingsSnapshot, client: &ClientHandle, inbox_root: &Path) -> Outcome { ... }
}
```

The shared-core settings state (focus, selection, editing, wizard, defaults) is read from `SettingsSnapshot` and mutated via `SettingsCommand` writes. The TUI-local state (test connections, discovered models, account summaries) lives in `SettingsShell`.

- [ ] **Step 3: Update event loop to use shell structs**

In event_loop.rs:
- Remove `let mut input_session`, `let mut text_input_view`, `let mut prev_mode`
- Replace with `let mut thread = ThreadShell::new();`
- Remove `let mut settings` (the SettingsState)
- Replace with `let mut settings_shell = SettingsShell::new();` (or `new_wizard()`)
- Replace all `input_session` → `thread.input_session`
- Replace all `text_input_view` → `thread.text_input_view`
- Replace mode sync with `thread.sync_mode(&snap)` on Thread screen
- Replace settings test polling with `settings_shell.poll()` on Settings screen
- Replace settings account init with `settings_shell.ensure_accounts(...)` on Settings screen
- Replace settings reference in draw with `&settings_shell`

- [ ] **Step 4: Update tui.rs draw to accept SettingsShell instead of SettingsState**

```rust
pub(crate) fn draw(
    frame: &mut Frame,
    vs: &ViewState,
    settings_shell: &crate::settings_shell::SettingsShell,
    theme: &Theme,
    text_input_view: &mut crate::text_input_view::TextInputView,
) -> (Option<usize>, usize)
```

Settings rendering reads from `SettingsSnapshot` (in `vs.ui`) for shared-core state and from `settings_shell` for TUI-local state (test_status, discovered_models, accounts for display).

- [ ] **Step 5: Verify it compiles and tests pass**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/
git commit -m "refactor: ThreadShell and SettingsShell own their state"
```

---

### Task 6: Delete `broker_cmd.rs` and `cmd!` macro

**Files:**
- Delete: `crates/ox-cli/src/broker_cmd.rs`
- Modify: `crates/ox-cli/src/main.rs`
- Modify: `crates/ox-cli/src/app.rs` (if cmd! still used there)

- [ ] **Step 1: Verify no remaining `cmd!` usage**

Run: `grep -rn 'cmd!' crates/ox-cli/src/ --include='*.rs'`

If any remain outside `broker_cmd.rs`, migrate them:
- `app.rs` `update_thread_state` uses `cmd!("inbox_state" => ...)` — replace with `Record::parsed(Value::Map(...))` or a typed write
- Any remaining config/inbox writes

- [ ] **Step 2: Remove module declaration and delete file**

In `crates/ox-cli/src/main.rs`, remove `#[macro_use] mod broker_cmd;`.

Delete `crates/ox-cli/src/broker_cmd.rs`.

- [ ] **Step 3: Verify it compiles and tests pass**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`

- [ ] **Step 4: Commit**

```bash
git add -A crates/ox-cli/
git commit -m "refactor: delete broker_cmd.rs and cmd! macro — all writes are typed"
```

---

### Task 7: Quality gates

- [ ] **Step 1: Run format**

Run: `./scripts/fmt.sh`

- [ ] **Step 2: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: 15/15 pass.

- [ ] **Step 3: Fix any failures and commit**

```bash
git add -A
git commit -m "fix: quality gate cleanup for S-tier screen architecture"
```

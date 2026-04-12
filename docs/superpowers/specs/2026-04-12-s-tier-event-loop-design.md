# S-Tier Screen Architecture via StructFS

**Date:** 2026-04-12
**Status:** Approved
**Scope:** Screen-scoped commands, screen-enum state, UiStore as router, CLI shell alignment, full typed protocol

## Problem

The typed StructFS migration (2026-04-11) replaced string comparisons with typed enums. But the architecture remains flat: `UiCommand` is a single 30+ variant enum where inbox, thread, and settings commands are all mixed together. `UiSnapshot` is a flat struct where `scroll` only matters on the thread screen and `selected_row` only matters on the inbox screen. `UiStore` is a flat state machine that handles all commands regardless of which screen is active.

This means:
- You can send `ScrollUp` while on the Inbox screen (no-op, but compiles)
- `UiSnapshot` always carries all fields even though half are irrelevant for the current screen
- UiStore's `handle_ui_command` is a monolith matching 30+ variants
- Adding a new screen requires touching the flat enum, flat snapshot, and monolith handler

The Chox TUI demonstrates the right pattern: each screen is its own Elm module with its own `Model`, `Msg`, `update`, `view`. Messages are hierarchical and screen-scoped.

## Architecture: Screens as StructFS Stores

Adopting the Chox pattern on StructFS, each screen becomes a sub-namespace of the `ui/` store:

```
ui/                          → UiStore (router)
  reads:  UiSnapshot (screen-discriminated enum)
  writes: UiCommand (hierarchical — Global | Inbox | Thread)

ui/inbox/                    → InboxState
  reads:  InboxSnapshot { selected_row, row_count, search }
  writes: InboxCommand { SelectNext, SelectPrev, ... }

ui/thread/                   → ThreadState  
  reads:  ThreadSnapshot { thread_id, mode, scroll, input, ... }
  writes: ThreadCommand { ScrollUp, EnterInsert, SendInput, ... }
```

The StructFS path encodes the routing. Writing `ui/inbox/select_next` or `write_typed(&oxpath!("ui"), &UiCommand::Inbox(InboxCommand::SelectNext))` — both route to the inbox state handler. Reading `ui/` returns a `UiSnapshot` enum whose variant tells you which screen you're on and carries only that screen's state.

### Core Types (ox-types)

**Screen-scoped commands:**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "scope", content = "command", rename_all = "snake_case")]
pub enum UiCommand {
    Global(GlobalCommand),
    Inbox(InboxCommand),
    Thread(ThreadCommand),
    Settings(SettingsCommand),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum GlobalCommand {
    Quit,
    Open { thread_id: String },
    Close,
    GoToSettings,
    GoToInbox,
    SetStatus { text: String },
}

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
```

**Screen-scoped snapshots:**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "screen", rename_all = "snake_case")]
pub enum UiSnapshot {
    Inbox(InboxSnapshot),
    Thread(ThreadSnapshot),
    Settings(SettingsSnapshot),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InboxSnapshot {
    pub selected_row: usize,
    pub row_count: usize,
    pub search: SearchSnapshot,
    pub pending_action: Option<PendingAction>,
}

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
```

**Settings commands and state:**

```rust
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
    StartEditAccount,
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
    EditSave,
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
    SaveDefaults,
    // Wizard
    FinishWizard,
}
```

Settings snapshot carries the shared-core navigation state. TUI-local state (async test connections, discovered models, save flash timers) stays in `SettingsShell`:

```rust
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsFocus { Accounts, Defaults }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WizardStep { AddAccount, SetDefaults, Done }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountEditFields {
    pub name: String,
    pub dialect: usize,
    pub endpoint: String,
    pub key: String,
    pub focus: usize,
    pub is_new: bool,
}
```

`PendingAction` appears in each screen snapshot (it's consumed by the event loop regardless of screen).

### UiStore: Router, Not Monolith

UiStore holds an `ActiveScreen` enum carrying per-screen state:

```rust
struct UiStore {
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

**Reader:** Returns `UiSnapshot` enum — the variant matches the active screen. Thread fields only exist when on Thread. Inbox fields only exist when on Inbox.

**Writer:** Deserializes `UiCommand`, routes by variant:
- `UiCommand::Global(cmd)` — always accepted, handles screen transitions
- `UiCommand::Inbox(cmd)` — only accepted when `screen == ActiveScreen::Inbox`; error otherwise
- `UiCommand::Thread(cmd)` — only accepted when `screen == ActiveScreen::Thread`; error otherwise
- `UiCommand::Settings(cmd)` — only accepted when `screen == ActiveScreen::Settings`; error otherwise

Screen transitions (`GlobalCommand::Open`, `GlobalCommand::Close`, `GlobalCommand::GoToSettings`, `GlobalCommand::GoToInbox`) construct the new `ActiveScreen` variant with default/initialized state.

### CLI Shell: Each Screen Gets Its Own Typed Slice

The TUI event loop reads `UiSnapshot`, matches on the variant, and dispatches to the right screen handler:

```rust
let ui: UiSnapshot = client.read_typed(&oxpath!("ui")).await?.unwrap_or_default();
match &ui {
    UiSnapshot::Inbox(snap) => shell.inbox.handle_key(key, snap, client).await,
    UiSnapshot::Thread(snap) => shell.thread.handle_key(key, snap, client).await,
    UiSnapshot::Settings(snap) => shell.settings.handle_key(key, snap, client).await,
}
```

Each shell handler produces only its own screen's commands:
```rust
// InboxShell::handle_key
client.write_typed(&oxpath!("ui"), &UiCommand::Inbox(InboxCommand::SelectNext)).await;
// Can't accidentally send ThreadCommand — wrong enum
```

### Remaining Typed Protocols

**InputKeyEvent** — typed input dispatch:
```rust
pub struct InputKeyEvent {
    pub mode: Mode,
    pub key: String,
    pub screen: Screen,
}
```

**ApprovalResponse** — typed approval writes:
```rust
pub struct ApprovalResponse {
    pub decision: String,
}
```

**Config writes** — use `write_typed` with raw typed values (String, i64). No new command type.

### Delete `cmd!` Macro

After all writes are typed, delete `broker_cmd.rs` and the `cmd!` macro entirely.

### Shell Structs Own State

- `ThreadShell` owns `InputSession`, `TextInputView`, `prev_mode` (TUI-local editor state)
- `SettingsShell` owns TUI-local settings state: `pending_test` (async oneshot), `discovered_models`, `test_status`, `save_flash_until`, `accounts` (display summaries). Shared-core settings state (focus, selection, editing, wizard) lives in `UiStore/SettingsState` and is read via `SettingsSnapshot`.
- `InboxShell` is stateless (inbox UI state lives in UiStore/InboxState)

## Migration Path

1. **Restructure ox-types** — replace flat `UiCommand` with hierarchical `GlobalCommand`/`InboxCommand`/`ThreadCommand`. Replace flat `UiSnapshot` with screen-discriminated enum. Add `InputKeyEvent`, `ApprovalResponse`.
2. **Restructure UiStore** — `ActiveScreen` enum with per-screen state. Router Writer. Screen-scoped Reader.
3. **Update CLI call sites** — all `write_typed` calls use hierarchical commands. `fetch_view_state` matches on `UiSnapshot` variant.
4. **Shell structs own state** — `ThreadShell`, `SettingsShell` hold their local state.
5. **Type remaining protocols** — InputKeyEvent for input/key, ApprovalResponse for approval writes, write_typed for config writes.
6. **Delete `cmd!` macro** — remove `broker_cmd.rs`.
7. **Migrate UiStore tests** — typed commands, remove legacy path.
8. **Quality gates**.

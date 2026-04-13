# Editor Widget Ownership

**Date:** 2026-04-12
**Status:** Approved
**Scope:** Move editor state into screen ownership, eliminate Mode as a separate concept

## Problem

Mode (`Normal`/`Insert`) and `insert_context` are duplicated in `InboxState` and `ThreadState`. Every consumer matches on both screen variants to extract mode, and every new consumer forgets one — causing bugs where compose, search, ESC, paste, send, and mode sync silently fail on the inbox screen.

The root cause: the text editor is a widget that screens compose, but it was modeled as flat state scattered across screen types instead of a component owned by the screen.

## Design

### Editor as a component owned by screens

```rust
struct UiStore {
    screen: ActiveScreen,
    pending_action: Option<PendingAction>,
    status: Option<String>,
    // No TextInputStore — screens own their editors
}

enum ActiveScreen {
    Inbox(InboxState),
    Thread(ThreadState),
    Settings(SettingsState),
}

struct InboxState {
    selected_row: usize,
    row_count: usize,
    search: SearchState,
    editor: Option<EditorState>,  // Some when composing or searching
}

struct ThreadState {
    thread_id: String,
    scroll: usize,
    scroll_max: usize,
    viewport_height: usize,
    editor: Option<EditorState>,  // Some when replying or in command mode
}

struct SettingsState {
    // ... settings-specific fields
    // Settings has its own editing mechanism (AccountEditFields),
    // NOT the general EditorState
}

/// The text editor widget's state — owned by the screen using it.
struct EditorState {
    context: InsertContext,  // NOT Option — if editor exists, context is known
    content: String,
    cursor: usize,
}
```

### Mode is eliminated as a separate concept

The editor's **presence** is the mode:
- `screen.editor.is_some()` → insert mode
- `screen.editor.is_none()` → normal mode

No `Mode` enum needed for this purpose. There's no impossible state where mode is Insert but no editor exists.

### Snapshots mirror the ownership

```rust
pub struct UiState {
    pub screen: ScreenSnapshot,
    pub pending_action: Option<PendingAction>,
}

pub enum ScreenSnapshot {
    Inbox(InboxSnapshot),
    Thread(ThreadSnapshot),
    Settings(SettingsSnapshot),
}

pub struct InboxSnapshot {
    pub selected_row: usize,
    pub row_count: usize,
    pub search: SearchSnapshot,
    pub editor: Option<EditorSnapshot>,
}

pub struct ThreadSnapshot {
    pub thread_id: String,
    pub scroll: usize,
    pub scroll_max: usize,
    pub viewport_height: usize,
    pub editor: Option<EditorSnapshot>,
}

pub struct EditorSnapshot {
    pub context: InsertContext,
    pub content: String,
    pub cursor: usize,
}
```

### Helper for cross-screen editor access

```rust
impl UiState {
    pub fn editor(&self) -> Option<&EditorSnapshot> {
        match &self.screen {
            ScreenSnapshot::Inbox(s) => s.editor.as_ref(),
            ScreenSnapshot::Thread(s) => s.editor.as_ref(),
            ScreenSnapshot::Settings(_) => None,
        }
    }
}
```

Consumers that need the editor regardless of screen call `ui.editor()`. One path, no screen matching, no variant to forget.

### Commands

`EnterInsert { context }` / `ExitInsert` are replaced by screen-specific commands that create or dismiss the editor widget:

```rust
pub enum InboxCommand {
    // ... selection, search ...
    Compose,         // editor = Some(EditorState { context: Compose, ... })
    Search,          // editor = Some(EditorState { context: Search, ... })
    DismissEditor,   // editor = None
    SubmitEditor,    // pending_action = SendInput, then editor = None
}

pub enum ThreadCommand {
    // ... scroll ...
    Reply,           // editor = Some(EditorState { context: Reply, ... })
    Command,         // editor = Some(EditorState { context: Command, ... })
    DismissEditor,   // editor = None
    SubmitEditor,    // pending_action = SendInput, then editor = None
}
```

### Editor text editing

Editor content mutations (insert char, delete, cursor movement) go through the `EditorState` owned by the active screen. The UiStore Writer routes `input/*` writes to whichever screen's editor is active:

```rust
// Writer: delegate input/* to the active editor
if path starts with "input" {
    let editor = self.active_editor_mut()?;  // finds the active screen's editor, errors if None
    // apply edit to editor.content / editor.cursor
}
```

### What changes in ox-types

- Remove `Mode` enum (no longer needed for editor mode — it's implicit in editor presence)
- Remove `InputSnapshot` as a standalone type (it's part of `EditorSnapshot`)
- `UiSnapshot` becomes `UiState` (struct with `screen: ScreenSnapshot`)
- Add `EditorSnapshot { context, content, cursor }`
- `InboxSnapshot` and `ThreadSnapshot` get `editor: Option<EditorSnapshot>`
- Commands: replace `EnterInsert`/`ExitInsert`/`SetInput`/`ClearInput`/`SendInput` with screen-specific `Compose`/`Reply`/`Search`/`Command`/`DismissEditor`/`SubmitEditor`

Note: `Mode` may still exist for other purposes (e.g. InputStore binding resolution uses mode strings for key dispatch). If so, it can be derived: `if ui.editor().is_some() { "insert" } else { "normal" }`. The Mode enum stays in ox-types for this purpose but is no longer stored as state — it's computed.

### What changes in ox-ui (UiStore)

- Remove `text_input_store: TextInputStore` from UiStore
- Remove `mode` and `insert_context` from InboxState and ThreadState
- Add `editor: Option<EditorState>` to InboxState and ThreadState
- `EditorState` holds content/cursor directly (replaces TextInputStore for this purpose)
- Edit sequences (the `EditOp`/`EditSequence` protocol) apply to the active editor
- `snapshot()` includes `EditorSnapshot` from whichever screen's editor is active

### What changes in ox-cli (event loop + shells)

- Mode checks: `if let Some(editor) = ui.editor()` replaces `if mode == Mode::Insert`
- Context checks: `editor.context == InsertContext::Compose` replaces `insert_context == Some(Compose)`
- ThreadShell's `sync_mode` becomes `sync_editor` — checks if editor appeared/disappeared
- `InputSession` init from `editor.content`/`editor.cursor` instead of from InputSnapshot

## Migration path

1. Add `EditorState`/`EditorSnapshot` types to ox-types
2. Restructure `UiState` as struct with `screen: ScreenSnapshot` + helpers
3. Move editor into screen states in UiStore, remove TextInputStore from UiStore top level
4. Update commands (Compose/Reply/Search/Command/DismissEditor/SubmitEditor)
5. Update all consumers to use `ui.editor()` helper
6. Update CLI shells
7. Quality gates

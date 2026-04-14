//! UiStore — in-memory state machine for TUI state.
//!
//! Reads return current field values. Writes are typed UiCommand enums
//! that transition state atomically.

use std::collections::BTreeMap;

use ox_types::{
    AccountEditFields, EditorSnapshot, GlobalCommand, HistoryCommand, HistorySnapshot,
    InboxCommand, InboxSnapshot, InsertContext, Mode, PendingAction, ScreenSnapshot,
    SearchSnapshot, SettingsCommand, SettingsFocus, SettingsSnapshot, ThreadCommand,
    ThreadSnapshot, UiCommand, UiSnapshot, WizardStep,
};
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer, path};

use crate::text_input_store::{Edit, EditOp, EditSequence};

// ---------------------------------------------------------------------------
// EditorState — owned by the screen using it
// ---------------------------------------------------------------------------

/// Internal editor widget state — owned by the screen.
struct EditorState {
    context: InsertContext,
    content: String,
    cursor: usize,
    generation: u64,
}

impl EditorState {
    fn new(context: InsertContext) -> Self {
        EditorState {
            context,
            content: String::new(),
            cursor: 0,
            generation: 0,
        }
    }

    /// Apply a single edit, clamping positions to valid bounds.
    fn apply_edit(&mut self, edit: &Edit) {
        let at = edit.at.min(self.content.len());
        match &edit.op {
            EditOp::Insert { text } => {
                self.content.insert_str(at, text);
                self.cursor = at + text.len();
            }
            EditOp::Delete { len } => {
                let end = (at + len).min(self.content.len());
                self.content.drain(at..end);
                self.cursor = at.min(self.content.len());
            }
        }
    }

    fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            context: self.context,
            content: self.content.clone(),
            cursor: self.cursor,
        }
    }
}

// ---------------------------------------------------------------------------
// Per-screen state
// ---------------------------------------------------------------------------

#[derive(Default)]
struct InboxState {
    selected_row: usize,
    row_count: usize,
    editor: Option<EditorState>,
    search_chips: Vec<String>,
    search_live_query: String,
}

impl InboxState {
    fn search_active(&self) -> bool {
        !self.search_chips.is_empty() || !self.search_live_query.is_empty()
    }
}

struct ThreadState {
    thread_id: String,
    editor: Option<EditorState>,
    scroll: usize,
    scroll_max: usize,
    viewport_height: usize,
}

impl ThreadState {
    fn new(thread_id: String) -> Self {
        ThreadState {
            thread_id,
            editor: None,
            scroll: 0,
            scroll_max: 0,
            viewport_height: 0,
        }
    }
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

impl Default for SettingsState {
    fn default() -> Self {
        SettingsState {
            focus: SettingsFocus::Accounts,
            selected_account: 0,
            editing: None,
            delete_confirming: false,
            wizard: None,
            defaults_focus: 0,
            default_account_idx: 0,
            default_model: String::new(),
            default_max_tokens: String::new(),
        }
    }
}

struct HistoryState {
    thread_id: String,
    selected_row: usize,
    row_count: usize,
    scroll: usize,
    scroll_max: usize,
    viewport_height: usize,
    expanded: std::collections::HashSet<usize>,
}

impl HistoryState {
    fn new(thread_id: String) -> Self {
        HistoryState {
            thread_id,
            selected_row: 0,
            row_count: 0,
            scroll: 0,
            scroll_max: 0,
            viewport_height: 0,
            expanded: std::collections::HashSet::new(),
        }
    }
}

enum ActiveScreen {
    Inbox(InboxState),
    Thread(ThreadState),
    Settings(SettingsState),
    History(HistoryState),
}

// ---------------------------------------------------------------------------
// UiStore
// ---------------------------------------------------------------------------

/// Holds all TUI state. Implements StructFS Reader and Writer.
pub struct UiStore {
    screen: ActiveScreen,
    pending_action: Option<PendingAction>,
    status: Option<String>,
}

impl UiStore {
    /// Create a new UiStore with default state.
    pub fn new() -> Self {
        UiStore {
            screen: ActiveScreen::Inbox(InboxState::default()),
            pending_action: None,
            status: None,
        }
    }

    // -- Screen guard helpers --

    fn inbox_state(&mut self) -> Result<&mut InboxState, StoreError> {
        match &mut self.screen {
            ActiveScreen::Inbox(s) => Ok(s),
            _ => Err(StoreError::store("ui", "inbox", "not on inbox screen")),
        }
    }

    fn thread_state(&mut self) -> Result<&mut ThreadState, StoreError> {
        match &mut self.screen {
            ActiveScreen::Thread(s) => Ok(s),
            _ => Err(StoreError::store("ui", "thread", "not on thread screen")),
        }
    }

    fn settings_state(&mut self) -> Result<&mut SettingsState, StoreError> {
        match &mut self.screen {
            ActiveScreen::Settings(s) => Ok(s),
            _ => Err(StoreError::store(
                "ui",
                "settings",
                "not on settings screen",
            )),
        }
    }

    fn history_state(&mut self) -> Result<&mut HistoryState, StoreError> {
        match &mut self.screen {
            ActiveScreen::History(s) => Ok(s),
            _ => Err(StoreError::store("ui", "history", "not on history screen")),
        }
    }

    /// Get a mutable reference to the active editor, if one exists.
    fn active_editor_mut(&mut self) -> Result<&mut EditorState, StoreError> {
        match &mut self.screen {
            ActiveScreen::Inbox(s) => s.editor.as_mut(),
            ActiveScreen::Thread(s) => s.editor.as_mut(),
            ActiveScreen::Settings(_) => None,
            ActiveScreen::History(_) => None,
        }
        .ok_or_else(|| StoreError::store("ui", "editor", "no active editor"))
    }

    // -- Snapshot --

    fn snapshot(&self) -> UiSnapshot {
        let screen = match &self.screen {
            ActiveScreen::Inbox(s) => ScreenSnapshot::Inbox(InboxSnapshot {
                selected_row: s.selected_row,
                row_count: s.row_count,
                editor: s.editor.as_ref().map(|e| e.snapshot()),
                search: SearchSnapshot {
                    chips: s.search_chips.clone(),
                    live_query: s.search_live_query.clone(),
                    active: s.search_active(),
                },
            }),
            ActiveScreen::Thread(s) => ScreenSnapshot::Thread(ThreadSnapshot {
                thread_id: s.thread_id.clone(),
                scroll: s.scroll,
                scroll_max: s.scroll_max,
                viewport_height: s.viewport_height,
                editor: s.editor.as_ref().map(|e| e.snapshot()),
            }),
            ActiveScreen::Settings(s) => ScreenSnapshot::Settings(SettingsSnapshot {
                focus: s.focus,
                selected_account: s.selected_account,
                editing: s.editing.clone(),
                delete_confirming: s.delete_confirming,
                wizard: s.wizard,
                defaults_focus: s.defaults_focus,
                default_account_idx: s.default_account_idx,
                default_model: s.default_model.clone(),
                default_max_tokens: s.default_max_tokens.clone(),
            }),
            ActiveScreen::History(s) => ScreenSnapshot::History(HistorySnapshot {
                thread_id: s.thread_id.clone(),
                selected_row: s.selected_row,
                scroll: s.scroll,
                scroll_max: s.scroll_max,
                viewport_height: s.viewport_height,
                expanded: s.expanded.iter().copied().collect(),
            }),
        };
        UiSnapshot {
            screen,
            pending_action: self.pending_action,
        }
    }

    // -- Backward compat helpers for individual field reads --

    fn screen_name(&self) -> &str {
        match &self.screen {
            ActiveScreen::Inbox(_) => "inbox",
            ActiveScreen::Thread(_) => "thread",
            ActiveScreen::Settings(_) => "settings",
            ActiveScreen::History(_) => "history",
        }
    }

    fn mode_value(&self) -> Value {
        let mode = match &self.screen {
            ActiveScreen::Inbox(s) => {
                if s.editor.is_some() {
                    Mode::Insert
                } else {
                    Mode::Normal
                }
            }
            ActiveScreen::Thread(s) => {
                if s.editor.is_some() {
                    Mode::Insert
                } else {
                    Mode::Normal
                }
            }
            ActiveScreen::Settings(_) => Mode::Normal,
            ActiveScreen::History(_) => Mode::Normal,
        };
        structfs_serde_store::to_value(&mode).unwrap_or(Value::Null)
    }

    fn insert_context_value(&self) -> Value {
        let ctx = match &self.screen {
            ActiveScreen::Inbox(s) => s.editor.as_ref().map(|e| e.context),
            ActiveScreen::Thread(s) => s.editor.as_ref().map(|e| e.context),
            ActiveScreen::Settings(_) => None,
            ActiveScreen::History(_) => None,
        };
        match ctx {
            Some(c) => structfs_serde_store::to_value(&c).unwrap_or(Value::Null),
            None => Value::Null,
        }
    }

    fn active_thread_value(&self) -> Value {
        match &self.screen {
            ActiveScreen::Thread(s) => Value::String(s.thread_id.clone()),
            ActiveScreen::History(s) => Value::String(s.thread_id.clone()),
            _ => Value::Null,
        }
    }

    fn status_value(&self) -> Value {
        match &self.status {
            Some(s) => Value::String(s.clone()),
            None => Value::Null,
        }
    }

    fn pending_action_value(&self) -> Value {
        match &self.pending_action {
            Some(action) => structfs_serde_store::to_value(action).unwrap_or(Value::Null),
            None => Value::Null,
        }
    }

    fn selected_row_value(&self) -> Value {
        match &self.screen {
            ActiveScreen::Inbox(s) => Value::Integer(s.selected_row as i64),
            ActiveScreen::History(s) => Value::Integer(s.selected_row as i64),
            _ => Value::Integer(0),
        }
    }

    fn row_count_value(&self) -> Value {
        match &self.screen {
            ActiveScreen::Inbox(s) => Value::Integer(s.row_count as i64),
            _ => Value::Integer(0),
        }
    }

    fn scroll_value(&self) -> Value {
        match &self.screen {
            ActiveScreen::Thread(s) => Value::Integer(s.scroll as i64),
            ActiveScreen::History(s) => Value::Integer(s.scroll as i64),
            _ => Value::Integer(0),
        }
    }

    fn scroll_max_value(&self) -> Value {
        match &self.screen {
            ActiveScreen::Thread(s) => Value::Integer(s.scroll_max as i64),
            ActiveScreen::History(s) => Value::Integer(s.scroll_max as i64),
            _ => Value::Integer(0),
        }
    }

    fn viewport_height_value(&self) -> Value {
        match &self.screen {
            ActiveScreen::Thread(s) => Value::Integer(s.viewport_height as i64),
            ActiveScreen::History(s) => Value::Integer(s.viewport_height as i64),
            _ => Value::Integer(0),
        }
    }

    fn search_chips_value(&self) -> Value {
        match &self.screen {
            ActiveScreen::Inbox(s) => Value::Array(
                s.search_chips
                    .iter()
                    .map(|c| Value::String(c.clone()))
                    .collect(),
            ),
            _ => Value::Array(vec![]),
        }
    }

    fn search_live_query_value(&self) -> Value {
        match &self.screen {
            ActiveScreen::Inbox(s) => Value::String(s.search_live_query.clone()),
            _ => Value::String(String::new()),
        }
    }

    fn search_active_value(&self) -> Value {
        match &self.screen {
            ActiveScreen::Inbox(s) => Value::Bool(s.search_active()),
            _ => Value::Bool(false),
        }
    }

    /// Read the active editor's content/cursor as an input snapshot value.
    fn input_value(&self, sub_key: &str) -> Value {
        let editor = match &self.screen {
            ActiveScreen::Inbox(s) => s.editor.as_ref(),
            ActiveScreen::Thread(s) => s.editor.as_ref(),
            ActiveScreen::Settings(_) => None,
            ActiveScreen::History(_) => None,
        };
        match editor {
            Some(e) => match sub_key {
                "" => {
                    let mut map = BTreeMap::new();
                    map.insert("content".to_string(), Value::String(e.content.clone()));
                    map.insert("cursor".to_string(), Value::Integer(e.cursor as i64));
                    Value::Map(map)
                }
                "content" => Value::String(e.content.clone()),
                "cursor" => Value::Integer(e.cursor as i64),
                _ => Value::Null,
            },
            None => match sub_key {
                "" => {
                    let mut map = BTreeMap::new();
                    map.insert("content".to_string(), Value::String(String::new()));
                    map.insert("cursor".to_string(), Value::Integer(0));
                    Value::Map(map)
                }
                "content" => Value::String(String::new()),
                "cursor" => Value::Integer(0),
                _ => Value::Null,
            },
        }
    }

    // -- Command handlers --

    fn handle_global(&mut self, cmd: GlobalCommand) -> Result<Path, StoreError> {
        match cmd {
            GlobalCommand::Quit => {
                self.pending_action = Some(PendingAction::Quit);
                Ok(path!("pending_action"))
            }
            GlobalCommand::Open { thread_id } => {
                self.screen = ActiveScreen::Thread(ThreadState::new(thread_id));
                Ok(path!("screen"))
            }
            GlobalCommand::Close => {
                self.screen = ActiveScreen::Inbox(InboxState::default());
                self.pending_action = None;
                Ok(path!("screen"))
            }
            GlobalCommand::GoToSettings => {
                self.screen = ActiveScreen::Settings(SettingsState::default());
                Ok(path!("screen"))
            }
            GlobalCommand::GoToInbox => {
                self.screen = ActiveScreen::Inbox(InboxState::default());
                Ok(path!("screen"))
            }
            GlobalCommand::OpenHistory { thread_id } => {
                self.screen = ActiveScreen::History(HistoryState::new(thread_id));
                Ok(path!("screen"))
            }
            GlobalCommand::BackToThread { thread_id } => {
                self.screen = ActiveScreen::Thread(ThreadState::new(thread_id));
                Ok(path!("screen"))
            }
            GlobalCommand::SetStatus { text } => {
                self.status = if text.is_empty() { None } else { Some(text) };
                Ok(path!("status"))
            }
            GlobalCommand::ClearPendingAction => {
                self.pending_action = None;
                Ok(path!("pending_action"))
            }
        }
    }

    fn handle_inbox(&mut self, cmd: InboxCommand) -> Result<Path, StoreError> {
        // Verify we're on the inbox screen
        let _ = self.inbox_state()?;

        match cmd {
            InboxCommand::SelectNext => {
                let s = self.inbox_state()?;
                if s.selected_row + 1 < s.row_count {
                    s.selected_row += 1;
                }
                Ok(path!("selected_row"))
            }
            InboxCommand::SelectPrev => {
                let s = self.inbox_state()?;
                if s.selected_row > 0 {
                    s.selected_row -= 1;
                }
                Ok(path!("selected_row"))
            }
            InboxCommand::SelectFirst => {
                let s = self.inbox_state()?;
                s.selected_row = 0;
                Ok(path!("selected_row"))
            }
            InboxCommand::SelectLast => {
                let s = self.inbox_state()?;
                if s.row_count > 0 {
                    s.selected_row = s.row_count - 1;
                }
                Ok(path!("selected_row"))
            }
            InboxCommand::SetRowCount { count } => {
                let s = self.inbox_state()?;
                s.row_count = count;
                if s.row_count > 0 && s.selected_row >= s.row_count {
                    s.selected_row = s.row_count - 1;
                } else if s.row_count == 0 {
                    s.selected_row = 0;
                }
                Ok(path!("row_count"))
            }
            InboxCommand::OpenSelected => {
                self.pending_action = Some(PendingAction::OpenSelected);
                Ok(path!("pending_action"))
            }
            InboxCommand::ArchiveSelected => {
                self.pending_action = Some(PendingAction::ArchiveSelected);
                Ok(path!("pending_action"))
            }
            InboxCommand::SearchInsertChar { char: ch } => {
                let s = self.inbox_state()?;
                s.search_live_query.push(ch);
                Ok(path!("search_live_query"))
            }
            InboxCommand::SearchDeleteChar => {
                let s = self.inbox_state()?;
                s.search_live_query.pop();
                Ok(path!("search_live_query"))
            }
            InboxCommand::SearchClear => {
                let s = self.inbox_state()?;
                s.search_live_query.clear();
                Ok(path!("search_live_query"))
            }
            InboxCommand::SearchSaveChip => {
                let s = self.inbox_state()?;
                let trimmed = s.search_live_query.trim().to_string();
                if !trimmed.is_empty() {
                    s.search_chips.push(trimmed);
                }
                s.search_live_query.clear();
                Ok(path!("search_chips"))
            }
            InboxCommand::SearchDismissChip { index } => {
                let s = self.inbox_state()?;
                if index < s.search_chips.len() {
                    s.search_chips.remove(index);
                }
                Ok(path!("search_chips"))
            }
            InboxCommand::Compose => {
                let s = self.inbox_state()?;
                s.editor = Some(EditorState::new(InsertContext::Compose));
                Ok(path!("editor"))
            }
            InboxCommand::Search => {
                let s = self.inbox_state()?;
                s.editor = Some(EditorState::new(InsertContext::Search));
                Ok(path!("editor"))
            }
            InboxCommand::DismissEditor => {
                let s = self.inbox_state()?;
                s.editor = None;
                Ok(path!("editor"))
            }
            InboxCommand::SubmitEditor => {
                self.pending_action = Some(PendingAction::SendInput);
                Ok(path!("pending_action"))
            }
        }
    }

    fn handle_thread(&mut self, cmd: ThreadCommand) -> Result<Path, StoreError> {
        // Verify we're on the thread screen
        let _ = self.thread_state()?;

        match cmd {
            ThreadCommand::ScrollUp => {
                let s = self.thread_state()?;
                if s.scroll < s.scroll_max {
                    s.scroll += 1;
                }
                Ok(path!("scroll"))
            }
            ThreadCommand::ScrollDown => {
                let s = self.thread_state()?;
                s.scroll = s.scroll.saturating_sub(1);
                Ok(path!("scroll"))
            }
            ThreadCommand::ScrollToTop => {
                let s = self.thread_state()?;
                s.scroll = s.scroll_max;
                Ok(path!("scroll"))
            }
            ThreadCommand::ScrollToBottom => {
                let s = self.thread_state()?;
                s.scroll = 0;
                Ok(path!("scroll"))
            }
            ThreadCommand::ScrollPageUp => {
                let s = self.thread_state()?;
                s.scroll = (s.scroll + s.viewport_height).min(s.scroll_max);
                Ok(path!("scroll"))
            }
            ThreadCommand::ScrollPageDown => {
                let s = self.thread_state()?;
                s.scroll = s.scroll.saturating_sub(s.viewport_height);
                Ok(path!("scroll"))
            }
            ThreadCommand::ScrollHalfPageUp => {
                let s = self.thread_state()?;
                let half = s.viewport_height / 2;
                s.scroll = (s.scroll + half).min(s.scroll_max);
                Ok(path!("scroll"))
            }
            ThreadCommand::ScrollHalfPageDown => {
                let s = self.thread_state()?;
                let half = s.viewport_height / 2;
                s.scroll = s.scroll.saturating_sub(half);
                Ok(path!("scroll"))
            }
            ThreadCommand::SetScrollMax { max } => {
                let s = self.thread_state()?;
                s.scroll_max = max;
                if s.scroll > s.scroll_max {
                    s.scroll = s.scroll_max;
                }
                Ok(path!("scroll_max"))
            }
            ThreadCommand::SetViewportHeight { height } => {
                let s = self.thread_state()?;
                s.viewport_height = height;
                Ok(path!("viewport_height"))
            }
            ThreadCommand::Reply => {
                let s = self.thread_state()?;
                s.editor = Some(EditorState::new(InsertContext::Reply));
                Ok(path!("editor"))
            }
            ThreadCommand::Command => {
                let s = self.thread_state()?;
                s.editor = Some(EditorState::new(InsertContext::Command));
                Ok(path!("editor"))
            }
            ThreadCommand::DismissEditor => {
                let s = self.thread_state()?;
                s.editor = None;
                Ok(path!("editor"))
            }
            ThreadCommand::SubmitEditor => {
                self.pending_action = Some(PendingAction::SendInput);
                Ok(path!("pending_action"))
            }
        }
    }

    fn handle_settings(&mut self, cmd: SettingsCommand) -> Result<Path, StoreError> {
        // Verify we're on the settings screen
        let _ = self.settings_state()?;

        match cmd {
            SettingsCommand::FocusAccounts => {
                let s = self.settings_state()?;
                s.focus = SettingsFocus::Accounts;
                Ok(path!("focus"))
            }
            SettingsCommand::FocusDefaults => {
                let s = self.settings_state()?;
                s.focus = SettingsFocus::Defaults;
                Ok(path!("focus"))
            }
            SettingsCommand::ToggleFocus => {
                let s = self.settings_state()?;
                s.focus = match s.focus {
                    SettingsFocus::Accounts => SettingsFocus::Defaults,
                    SettingsFocus::Defaults => SettingsFocus::Accounts,
                };
                Ok(path!("focus"))
            }
            SettingsCommand::SelectNextAccount => {
                let s = self.settings_state()?;
                s.selected_account += 1;
                Ok(path!("selected_account"))
            }
            SettingsCommand::SelectPrevAccount => {
                let s = self.settings_state()?;
                if s.selected_account > 0 {
                    s.selected_account -= 1;
                }
                Ok(path!("selected_account"))
            }
            SettingsCommand::SelectNextDefault => {
                let s = self.settings_state()?;
                s.defaults_focus += 1;
                Ok(path!("defaults_focus"))
            }
            SettingsCommand::SelectPrevDefault => {
                let s = self.settings_state()?;
                if s.defaults_focus > 0 {
                    s.defaults_focus -= 1;
                }
                Ok(path!("defaults_focus"))
            }
            SettingsCommand::StartAddAccount => {
                let s = self.settings_state()?;
                s.editing = Some(AccountEditFields {
                    is_new: true,
                    ..AccountEditFields::default()
                });
                Ok(path!("editing"))
            }
            SettingsCommand::StartEditAccount {
                name,
                dialect,
                endpoint,
                key,
            } => {
                let s = self.settings_state()?;
                s.editing = Some(AccountEditFields {
                    name,
                    dialect,
                    endpoint,
                    key,
                    focus: 0,
                    is_new: false,
                });
                Ok(path!("editing"))
            }
            SettingsCommand::StartDeleteAccount => {
                let s = self.settings_state()?;
                s.delete_confirming = true;
                Ok(path!("delete_confirming"))
            }
            SettingsCommand::ConfirmDelete => {
                let s = self.settings_state()?;
                s.delete_confirming = false;
                // Actual delete logic is in the app layer via pending_action
                Ok(path!("delete_confirming"))
            }
            SettingsCommand::CancelDelete => {
                let s = self.settings_state()?;
                s.delete_confirming = false;
                Ok(path!("delete_confirming"))
            }
            SettingsCommand::EditFocusNext => {
                let s = self.settings_state()?;
                if let Some(ref mut e) = s.editing {
                    e.focus += 1;
                }
                Ok(path!("editing"))
            }
            SettingsCommand::EditFocusPrev => {
                let s = self.settings_state()?;
                if let Some(ref mut e) = s.editing {
                    if e.focus > 0 {
                        e.focus -= 1;
                    }
                }
                Ok(path!("editing"))
            }
            SettingsCommand::EditFocusField { field } => {
                let s = self.settings_state()?;
                if let Some(ref mut e) = s.editing {
                    e.focus = field;
                }
                Ok(path!("editing"))
            }
            SettingsCommand::EditDialectNext => {
                let s = self.settings_state()?;
                if let Some(ref mut e) = s.editing {
                    e.dialect += 1;
                }
                Ok(path!("editing"))
            }
            SettingsCommand::EditDialectPrev => {
                let s = self.settings_state()?;
                if let Some(ref mut e) = s.editing {
                    if e.dialect > 0 {
                        e.dialect -= 1;
                    }
                }
                Ok(path!("editing"))
            }
            SettingsCommand::EditInsertChar { char: ch } => {
                let s = self.settings_state()?;
                if let Some(ref mut e) = s.editing {
                    match e.focus {
                        0 => e.name.push(ch),
                        // 1 is dialect (not a text field)
                        2 => e.endpoint.push(ch),
                        3 => e.key.push(ch),
                        _ => {}
                    }
                }
                Ok(path!("editing"))
            }
            SettingsCommand::EditBackspace => {
                let s = self.settings_state()?;
                if let Some(ref mut e) = s.editing {
                    match e.focus {
                        0 => {
                            e.name.pop();
                        }
                        2 => {
                            e.endpoint.pop();
                        }
                        3 => {
                            e.key.pop();
                        }
                        _ => {}
                    }
                }
                Ok(path!("editing"))
            }
            SettingsCommand::EditSave {
                name: _,
                provider: _,
                endpoint: _,
                key: _,
            } => {
                let s = self.settings_state()?;
                s.editing = None;
                Ok(path!("editing"))
            }
            SettingsCommand::EditCancel => {
                let s = self.settings_state()?;
                s.editing = None;
                Ok(path!("editing"))
            }
            SettingsCommand::DefaultAccountNext => {
                let s = self.settings_state()?;
                s.default_account_idx += 1;
                Ok(path!("default_account_idx"))
            }
            SettingsCommand::DefaultAccountPrev => {
                let s = self.settings_state()?;
                if s.default_account_idx > 0 {
                    s.default_account_idx -= 1;
                }
                Ok(path!("default_account_idx"))
            }
            SettingsCommand::DefaultModelNext | SettingsCommand::DefaultModelPrev => {
                // Model cycling is app-layer concern; store just acknowledges
                Ok(path!("default_model"))
            }
            SettingsCommand::DefaultModelInsertChar { char: ch } => {
                let s = self.settings_state()?;
                s.default_model.push(ch);
                Ok(path!("default_model"))
            }
            SettingsCommand::DefaultModelBackspace => {
                let s = self.settings_state()?;
                s.default_model.pop();
                Ok(path!("default_model"))
            }
            SettingsCommand::DefaultMaxTokensInsertChar { char: ch } => {
                let s = self.settings_state()?;
                s.default_max_tokens.push(ch);
                Ok(path!("default_max_tokens"))
            }
            SettingsCommand::DefaultMaxTokensBackspace => {
                let s = self.settings_state()?;
                s.default_max_tokens.pop();
                Ok(path!("default_max_tokens"))
            }
            SettingsCommand::SaveDefaults {
                account: _,
                model: _,
                max_tokens: _,
            } => {
                // Actual save is app-layer; store clears wizard if active
                let s = self.settings_state()?;
                if s.wizard.is_some() {
                    s.wizard = Some(WizardStep::Done);
                }
                Ok(path!("defaults_focus"))
            }
            SettingsCommand::FinishWizard => {
                let s = self.settings_state()?;
                s.wizard = None;
                Ok(path!("wizard"))
            }
        }
    }

    fn handle_history(&mut self, cmd: HistoryCommand) -> Result<Path, StoreError> {
        // Verify we're on the history screen
        let _ = self.history_state()?;

        match cmd {
            HistoryCommand::SelectNext => {
                let s = self.history_state()?;
                if s.row_count > 0 && s.selected_row + 1 < s.row_count {
                    s.selected_row += 1;
                }
                Ok(path!("selected_row"))
            }
            HistoryCommand::SelectPrev => {
                let s = self.history_state()?;
                s.selected_row = s.selected_row.saturating_sub(1);
                Ok(path!("selected_row"))
            }
            HistoryCommand::SelectFirst => {
                let s = self.history_state()?;
                s.selected_row = 0;
                Ok(path!("selected_row"))
            }
            HistoryCommand::SelectLast => {
                let s = self.history_state()?;
                if s.row_count > 0 {
                    s.selected_row = s.row_count - 1;
                }
                Ok(path!("selected_row"))
            }
            HistoryCommand::ToggleExpand => {
                let s = self.history_state()?;
                let row = s.selected_row;
                if !s.expanded.remove(&row) {
                    s.expanded.insert(row);
                }
                Ok(path!("expanded"))
            }
            HistoryCommand::ExpandAll => {
                let s = self.history_state()?;
                s.expanded.extend(0..1000);
                Ok(path!("expanded"))
            }
            HistoryCommand::CollapseAll => {
                let s = self.history_state()?;
                s.expanded.clear();
                Ok(path!("expanded"))
            }
            HistoryCommand::ScrollUp => {
                let s = self.history_state()?;
                if s.scroll < s.scroll_max {
                    s.scroll += 1;
                }
                Ok(path!("scroll"))
            }
            HistoryCommand::ScrollDown => {
                let s = self.history_state()?;
                s.scroll = s.scroll.saturating_sub(1);
                Ok(path!("scroll"))
            }
            HistoryCommand::ScrollToTop => {
                let s = self.history_state()?;
                s.scroll = s.scroll_max;
                Ok(path!("scroll"))
            }
            HistoryCommand::ScrollToBottom => {
                let s = self.history_state()?;
                s.scroll = 0;
                Ok(path!("scroll"))
            }
            HistoryCommand::ScrollPageUp => {
                let s = self.history_state()?;
                s.scroll = (s.scroll + s.viewport_height).min(s.scroll_max);
                Ok(path!("scroll"))
            }
            HistoryCommand::ScrollPageDown => {
                let s = self.history_state()?;
                s.scroll = s.scroll.saturating_sub(s.viewport_height);
                Ok(path!("scroll"))
            }
            HistoryCommand::ScrollHalfPageUp => {
                let s = self.history_state()?;
                let half = s.viewport_height / 2;
                s.scroll = (s.scroll + half).min(s.scroll_max);
                Ok(path!("scroll"))
            }
            HistoryCommand::ScrollHalfPageDown => {
                let s = self.history_state()?;
                let half = s.viewport_height / 2;
                s.scroll = s.scroll.saturating_sub(half);
                Ok(path!("scroll"))
            }
            HistoryCommand::SetRowCount { count } => {
                let s = self.history_state()?;
                s.row_count = count;
                if s.selected_row >= count && count > 0 {
                    s.selected_row = count - 1;
                }
                Ok(path!("row_count"))
            }
            HistoryCommand::SetScrollMax { max } => {
                let s = self.history_state()?;
                s.scroll_max = max;
                if s.scroll > s.scroll_max {
                    s.scroll = s.scroll_max;
                }
                Ok(path!("scroll_max"))
            }
            HistoryCommand::SetViewportHeight { height } => {
                let s = self.history_state()?;
                s.viewport_height = height;
                Ok(path!("viewport_height"))
            }
        }
    }

    fn dispatch_command(&mut self, cmd: UiCommand) -> Result<Path, StoreError> {
        match cmd {
            UiCommand::Global(g) => self.handle_global(g),
            UiCommand::Inbox(i) => self.handle_inbox(i),
            UiCommand::Thread(t) => self.handle_thread(t),
            UiCommand::Settings(s) => self.handle_settings(s),
            UiCommand::History(h) => self.handle_history(h),
        }
    }

    /// Map a legacy path-based command name + args to a UiCommand.
    ///
    /// The command dispatch chain (InputStore -> CommandStore -> UiStore) sends
    /// writes to paths like "select_next", "scroll_down", "open", etc. This
    /// method translates those into the typed UiCommand enum.
    fn resolve_path_command(&self, name: &str, value: &Value) -> Result<UiCommand, StoreError> {
        let map = match value {
            Value::Map(m) => m,
            _ => &BTreeMap::new(),
        };

        fn get_str(map: &BTreeMap<String, Value>, key: &str) -> Option<String> {
            match map.get(key) {
                Some(Value::String(s)) => Some(s.clone()),
                _ => None,
            }
        }
        fn get_usize(map: &BTreeMap<String, Value>, key: &str) -> Option<usize> {
            match map.get(key) {
                Some(Value::Integer(n)) => Some(*n as usize),
                _ => None,
            }
        }
        fn get_char(map: &BTreeMap<String, Value>, key: &str) -> Option<char> {
            get_str(map, key).and_then(|s| s.chars().next())
        }

        let err = |msg: &str| StoreError::store("ui", "path_command", msg.to_string());

        match name {
            // Global commands
            "quit" => Ok(UiCommand::Global(GlobalCommand::Quit)),
            "open" => Ok(UiCommand::Global(GlobalCommand::Open {
                thread_id: get_str(map, "thread_id").ok_or_else(|| err("missing thread_id"))?,
            })),
            "close" => Ok(UiCommand::Global(GlobalCommand::Close)),
            "go_to_settings" => Ok(UiCommand::Global(GlobalCommand::GoToSettings)),
            "go_to_inbox" => Ok(UiCommand::Global(GlobalCommand::GoToInbox)),
            "set_status" => Ok(UiCommand::Global(GlobalCommand::SetStatus {
                text: get_str(map, "text").unwrap_or_default(),
            })),
            "clear_pending_action" => Ok(UiCommand::Global(GlobalCommand::ClearPendingAction)),

            // Selection commands — route to History or Inbox based on screen
            "select_next" => match &self.screen {
                ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::SelectNext)),
                _ => Ok(UiCommand::Inbox(InboxCommand::SelectNext)),
            },
            "select_prev" => match &self.screen {
                ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::SelectPrev)),
                _ => Ok(UiCommand::Inbox(InboxCommand::SelectPrev)),
            },
            "select_first" => match &self.screen {
                ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::SelectFirst)),
                _ => Ok(UiCommand::Inbox(InboxCommand::SelectFirst)),
            },
            "select_last" => match &self.screen {
                ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::SelectLast)),
                _ => Ok(UiCommand::Inbox(InboxCommand::SelectLast)),
            },
            "set_row_count" => Ok(UiCommand::Inbox(InboxCommand::SetRowCount {
                count: get_usize(map, "count").ok_or_else(|| err("missing count"))?,
            })),
            "open_selected" => Ok(UiCommand::Inbox(InboxCommand::OpenSelected)),
            "archive_selected" => Ok(UiCommand::Inbox(InboxCommand::ArchiveSelected)),
            "search_insert_char" => Ok(UiCommand::Inbox(InboxCommand::SearchInsertChar {
                char: get_char(map, "char").ok_or_else(|| err("missing char"))?,
            })),
            "search_delete_char" => Ok(UiCommand::Inbox(InboxCommand::SearchDeleteChar)),
            "search_clear" => Ok(UiCommand::Inbox(InboxCommand::SearchClear)),
            "search_save_chip" => Ok(UiCommand::Inbox(InboxCommand::SearchSaveChip)),
            "search_dismiss_chip" => Ok(UiCommand::Inbox(InboxCommand::SearchDismissChip {
                index: get_usize(map, "index").ok_or_else(|| err("missing index"))?,
            })),

            // Scroll commands — route to History or Thread based on screen
            "scroll_up" => match &self.screen {
                ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::ScrollUp)),
                _ => Ok(UiCommand::Thread(ThreadCommand::ScrollUp)),
            },
            "scroll_down" => match &self.screen {
                ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::ScrollDown)),
                _ => Ok(UiCommand::Thread(ThreadCommand::ScrollDown)),
            },
            "scroll_to_top" => match &self.screen {
                ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::ScrollToTop)),
                _ => Ok(UiCommand::Thread(ThreadCommand::ScrollToTop)),
            },
            "scroll_to_bottom" => match &self.screen {
                ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::ScrollToBottom)),
                _ => Ok(UiCommand::Thread(ThreadCommand::ScrollToBottom)),
            },
            "scroll_page_up" => match &self.screen {
                ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::ScrollPageUp)),
                _ => Ok(UiCommand::Thread(ThreadCommand::ScrollPageUp)),
            },
            "scroll_page_down" => match &self.screen {
                ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::ScrollPageDown)),
                _ => Ok(UiCommand::Thread(ThreadCommand::ScrollPageDown)),
            },
            "scroll_half_page_up" => match &self.screen {
                ActiveScreen::History(_) => {
                    Ok(UiCommand::History(HistoryCommand::ScrollHalfPageUp))
                }
                _ => Ok(UiCommand::Thread(ThreadCommand::ScrollHalfPageUp)),
            },
            "scroll_half_page_down" => match &self.screen {
                ActiveScreen::History(_) => {
                    Ok(UiCommand::History(HistoryCommand::ScrollHalfPageDown))
                }
                _ => Ok(UiCommand::Thread(ThreadCommand::ScrollHalfPageDown)),
            },
            "set_scroll_max" => match &self.screen {
                ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::SetScrollMax {
                    max: get_usize(map, "max").ok_or_else(|| err("missing max"))?,
                })),
                _ => Ok(UiCommand::Thread(ThreadCommand::SetScrollMax {
                    max: get_usize(map, "max").ok_or_else(|| err("missing max"))?,
                })),
            },
            "set_viewport_height" => match &self.screen {
                ActiveScreen::History(_) => {
                    Ok(UiCommand::History(HistoryCommand::SetViewportHeight {
                        height: get_usize(map, "height").ok_or_else(|| err("missing height"))?,
                    }))
                }
                _ => Ok(UiCommand::Thread(ThreadCommand::SetViewportHeight {
                    height: get_usize(map, "height").ok_or_else(|| err("missing height"))?,
                })),
            },
            "enter_insert" => {
                let ctx_str = get_str(map, "context").ok_or_else(|| err("missing context"))?;
                let context: InsertContext =
                    serde_json::from_value(serde_json::Value::String(ctx_str)).map_err(|e| {
                        StoreError::store("ui", "path_command", format!("bad context: {e}"))
                    })?;
                // Route to the current screen's command
                match (&self.screen, context) {
                    (ActiveScreen::Inbox(_), InsertContext::Compose) => {
                        Ok(UiCommand::Inbox(InboxCommand::Compose))
                    }
                    (ActiveScreen::Inbox(_), InsertContext::Search) => {
                        Ok(UiCommand::Inbox(InboxCommand::Search))
                    }
                    (ActiveScreen::Thread(_), InsertContext::Reply) => {
                        Ok(UiCommand::Thread(ThreadCommand::Reply))
                    }
                    (ActiveScreen::Thread(_), InsertContext::Command) => {
                        Ok(UiCommand::Thread(ThreadCommand::Command))
                    }
                    (ActiveScreen::Inbox(_), InsertContext::Reply) => {
                        Ok(UiCommand::Inbox(InboxCommand::Compose))
                    }
                    (ActiveScreen::Inbox(_), InsertContext::Command) => {
                        Err(err("command context not supported on inbox screen"))
                    }
                    (ActiveScreen::Thread(_), InsertContext::Compose) => {
                        Ok(UiCommand::Thread(ThreadCommand::Reply))
                    }
                    (ActiveScreen::Thread(_), InsertContext::Search) => {
                        Err(err("search context not supported on thread screen"))
                    }
                    (ActiveScreen::Settings(_), _) => {
                        Err(err("enter_insert not supported on settings screen"))
                    }
                    (ActiveScreen::History(_), _) => {
                        Err(err("enter_insert not supported on history screen"))
                    }
                }
            }
            "exit_insert" => match &self.screen {
                ActiveScreen::Inbox(_) => Ok(UiCommand::Inbox(InboxCommand::DismissEditor)),
                ActiveScreen::Thread(_) => Ok(UiCommand::Thread(ThreadCommand::DismissEditor)),
                ActiveScreen::Settings(_) => Err(StoreError::store(
                    "ui",
                    "path_command",
                    "exit_insert not supported on settings screen",
                )),
                ActiveScreen::History(_) => Err(StoreError::store(
                    "ui",
                    "path_command",
                    "exit_insert not supported on history screen",
                )),
            },
            // set_input and clear_input handled via resolve_path_command_direct
            "send_input" => match &self.screen {
                ActiveScreen::Inbox(_) => Ok(UiCommand::Inbox(InboxCommand::SubmitEditor)),
                ActiveScreen::Thread(_) => Ok(UiCommand::Thread(ThreadCommand::SubmitEditor)),
                ActiveScreen::Settings(_) => Err(StoreError::store(
                    "ui",
                    "path_command",
                    "send_input not supported on settings screen",
                )),
                ActiveScreen::History(_) => Err(StoreError::store(
                    "ui",
                    "path_command",
                    "send_input not supported on history screen",
                )),
            },

            // History commands
            "open_history" => {
                let thread_id = match &self.screen {
                    ActiveScreen::Thread(s) => s.thread_id.clone(),
                    _ => {
                        return Err(err("open_history requires thread screen"));
                    }
                };
                Ok(UiCommand::Global(GlobalCommand::OpenHistory { thread_id }))
            }
            "back_to_thread" => {
                let thread_id = match &self.screen {
                    ActiveScreen::History(s) => s.thread_id.clone(),
                    _ => {
                        return Err(err("back_to_thread requires history screen"));
                    }
                };
                Ok(UiCommand::Global(GlobalCommand::BackToThread { thread_id }))
            }
            "toggle_expand" => Ok(UiCommand::History(HistoryCommand::ToggleExpand)),
            "expand_all" => Ok(UiCommand::History(HistoryCommand::ExpandAll)),
            "collapse_all" => Ok(UiCommand::History(HistoryCommand::CollapseAll)),

            // Modal commands (no-ops for now, kept for backward compat)
            "show_modal" | "dismiss_modal" => {
                Ok(UiCommand::Global(GlobalCommand::ClearPendingAction))
            }

            _ => Err(StoreError::store(
                "ui",
                "path_command",
                format!("unknown command path: {name}"),
            )),
        }
    }
}

impl Default for UiStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

impl Reader for UiStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = if from.is_empty() {
            ""
        } else {
            from.components[0].as_str()
        };
        let value = match key {
            "" => structfs_serde_store::to_value(&self.snapshot()).map_err(|e| {
                StoreError::store("ui", "read", format!("snapshot serialization failed: {e}"))
            })?,
            "screen" => Value::String(self.screen_name().to_string()),
            "active_thread" => self.active_thread_value(),
            "mode" => self.mode_value(),
            "insert_context" => self.insert_context_value(),
            "selected_row" => self.selected_row_value(),
            "row_count" => self.row_count_value(),
            "scroll" => self.scroll_value(),
            "scroll_max" => self.scroll_max_value(),
            "viewport_height" => self.viewport_height_value(),
            "input" => {
                let sub = if from.components.len() > 1 {
                    from.components[1].as_str()
                } else {
                    ""
                };
                self.input_value(sub)
            }
            "cursor" => self.input_value("cursor"),
            "status" => self.status_value(),
            "pending_action" => self.pending_action_value(),
            "search_chips" => self.search_chips_value(),
            "search_live_query" => self.search_live_query_value(),
            "search_active" => self.search_active_value(),
            _ => return Ok(None),
        };
        Ok(Some(Record::parsed(value)))
    }
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

impl Writer for UiStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        // Delegate input/* writes to the active editor
        if !to.is_empty() && to.components[0] == "input" {
            let action = if to.components.len() > 1 {
                to.components[1].as_str()
            } else {
                return Err(StoreError::store(
                    "ui",
                    "write",
                    "write to input root not supported",
                ));
            };

            match action {
                "edit" => {
                    let value = data.as_value().ok_or_else(|| {
                        StoreError::store("ui", "input/edit", "write data must contain a value")
                    })?;
                    let seq: EditSequence = structfs_serde_store::from_value(value.clone())
                        .map_err(|e| {
                            StoreError::store("ui", "input/edit", format!("bad edit sequence: {e}"))
                        })?;
                    let editor = self.active_editor_mut()?;
                    // Stale generation — ignore silently
                    if seq.generation < editor.generation {
                        return Ok(path!("input"));
                    }
                    for edit in &seq.edits {
                        editor.apply_edit(edit);
                    }
                    Ok(path!("input"))
                }
                "replace" => {
                    let value = data.as_value().ok_or_else(|| {
                        StoreError::store("ui", "input/replace", "write data must contain a value")
                    })?;
                    #[derive(serde::Deserialize)]
                    struct ReplacePayload {
                        content: String,
                        cursor: usize,
                    }
                    let payload: ReplacePayload = structfs_serde_store::from_value(value.clone())
                        .map_err(|e| {
                        StoreError::store(
                            "ui",
                            "input/replace",
                            format!("bad replace payload: {e}"),
                        )
                    })?;
                    let editor = self.active_editor_mut()?;
                    editor.content = payload.content;
                    editor.cursor = payload.cursor.min(editor.content.len());
                    editor.generation += 1;
                    Ok(path!("input"))
                }
                "clear" => {
                    let editor = self.active_editor_mut()?;
                    editor.content.clear();
                    editor.cursor = 0;
                    Ok(path!("input"))
                }
                _ => Err(StoreError::store(
                    "ui",
                    "write",
                    format!("unknown input path: {action}"),
                )),
            }
        }
        // Path-based routing: command dispatch chain sends writes to paths
        // like "select_next", "scroll_down", etc. Map these to UiCommand.
        else if !to.is_empty() {
            let cmd_name = &to.components[0];
            // set_input and clear_input are handled directly (they mutate editor state)
            if cmd_name == "set_input" || cmd_name == "clear_input" {
                let value = data.as_value().ok_or_else(|| {
                    StoreError::store("ui", "write", "write data must contain a value")
                })?;
                return self.resolve_path_command_direct(cmd_name, value);
            }
            let value = data.as_value().ok_or_else(|| {
                StoreError::store("ui", "write", "write data must contain a value")
            })?;
            let cmd = self.resolve_path_command(cmd_name, value)?;
            self.dispatch_command(cmd)
        } else {
            let value = data.as_value().ok_or_else(|| {
                StoreError::store("ui", "write", "write data must contain a value")
            })?;

            let cmd: UiCommand = structfs_serde_store::from_value(value.clone()).map_err(|e| {
                StoreError::store(
                    "ui",
                    "write",
                    format!("failed to deserialize UiCommand: {e}"),
                )
            })?;

            self.dispatch_command(cmd)
        }
    }
}

impl UiStore {
    /// Handle set_input and clear_input directly (they mutate editor, not via command dispatch).
    fn resolve_path_command_direct(
        &mut self,
        name: &str,
        value: &Value,
    ) -> Result<Path, StoreError> {
        let map = match value {
            Value::Map(m) => m,
            _ => &BTreeMap::new(),
        };

        fn get_str(map: &BTreeMap<String, Value>, key: &str) -> Option<String> {
            match map.get(key) {
                Some(Value::String(s)) => Some(s.clone()),
                _ => None,
            }
        }
        fn get_usize(map: &BTreeMap<String, Value>, key: &str) -> Option<usize> {
            match map.get(key) {
                Some(Value::Integer(n)) => Some(*n as usize),
                _ => None,
            }
        }

        match name {
            "set_input" => {
                let content = get_str(map, "text").unwrap_or_default();
                let cursor = get_usize(map, "cursor").unwrap_or(0);
                let editor = self.active_editor_mut().map_err(|_| {
                    StoreError::store("ui", "path_command", "no active editor for set_input")
                })?;
                editor.content = content;
                editor.cursor = cursor.min(editor.content.len());
                Ok(path!("input"))
            }
            "clear_input" => {
                let editor = self.active_editor_mut().map_err(|_| {
                    StoreError::store("ui", "path_command", "no active editor for clear_input")
                })?;
                editor.content.clear();
                editor.cursor = 0;
                Ok(path!("input"))
            }
            _ => Err(StoreError::store(
                "ui",
                "path_command",
                format!("unknown direct command: {name}"),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn typed_cmd(cmd: &UiCommand) -> Record {
        Record::parsed(structfs_serde_store::to_value(cmd).unwrap())
    }

    fn read_snapshot(store: &mut UiStore) -> UiSnapshot {
        let record = store.read(&path!("")).unwrap().unwrap();
        structfs_serde_store::from_value(record.as_value().unwrap().clone()).unwrap()
    }

    fn read_val(store: &mut UiStore, key: &str) -> Value {
        let p = path!(key);
        store.read(&p).unwrap().unwrap().as_value().unwrap().clone()
    }

    fn write_cmd(store: &mut UiStore, cmd: &UiCommand) {
        store.write(&path!(""), typed_cmd(cmd)).unwrap();
    }

    // -- History screen --

    #[test]
    fn history_screen_navigation() {
        let mut store = UiStore::new();
        // Open a thread, then open history
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::Open {
                thread_id: "t1".into(),
            }),
        );
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::OpenHistory {
                thread_id: "t1".into(),
            }),
        );
        assert_eq!(
            read_val(&mut store, "screen"),
            Value::String("history".into())
        );

        // Set row count
        write_cmd(
            &mut store,
            &UiCommand::History(HistoryCommand::SetRowCount { count: 5 }),
        );

        // SelectLast
        write_cmd(&mut store, &UiCommand::History(HistoryCommand::SelectLast));
        let snap = read_snapshot(&mut store);
        match &snap.screen {
            ScreenSnapshot::History(s) => assert_eq!(s.selected_row, 4),
            _ => panic!("expected History"),
        }

        // SelectFirst
        write_cmd(&mut store, &UiCommand::History(HistoryCommand::SelectFirst));
        let snap = read_snapshot(&mut store);
        match &snap.screen {
            ScreenSnapshot::History(s) => assert_eq!(s.selected_row, 0),
            _ => panic!("expected History"),
        }

        // SelectNext
        write_cmd(&mut store, &UiCommand::History(HistoryCommand::SelectNext));
        let snap = read_snapshot(&mut store);
        match &snap.screen {
            ScreenSnapshot::History(s) => assert_eq!(s.selected_row, 1),
            _ => panic!("expected History"),
        }

        // SelectNext at end stays put
        write_cmd(&mut store, &UiCommand::History(HistoryCommand::SelectLast));
        write_cmd(&mut store, &UiCommand::History(HistoryCommand::SelectNext));
        let snap = read_snapshot(&mut store);
        match &snap.screen {
            ScreenSnapshot::History(s) => assert_eq!(s.selected_row, 4),
            _ => panic!("expected History"),
        }

        // ToggleExpand
        write_cmd(&mut store, &UiCommand::History(HistoryCommand::SelectFirst));
        write_cmd(
            &mut store,
            &UiCommand::History(HistoryCommand::ToggleExpand),
        );
        let snap = read_snapshot(&mut store);
        match &snap.screen {
            ScreenSnapshot::History(s) => assert!(s.expanded.contains(&0)),
            _ => panic!("expected History"),
        }

        // BackToThread
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::BackToThread {
                thread_id: "t1".into(),
            }),
        );
        assert_eq!(
            read_val(&mut store, "screen"),
            Value::String("thread".into())
        );
    }

    // -- Initial state --

    #[test]
    fn initial_state_is_inbox() {
        let mut store = UiStore::new();
        assert_eq!(
            read_val(&mut store, "screen"),
            Value::String("inbox".into())
        );
        assert_eq!(read_val(&mut store, "mode"), Value::String("normal".into()));
        assert_eq!(read_val(&mut store, "selected_row"), Value::Integer(0));
    }

    #[test]
    fn initial_snapshot_is_inbox() {
        let mut store = UiStore::new();
        let snap = read_snapshot(&mut store);
        match snap.screen {
            ScreenSnapshot::Inbox(ref s) => {
                assert_eq!(s.selected_row, 0);
                assert_eq!(s.row_count, 0);
                assert!(snap.pending_action.is_none());
            }
            _ => panic!("expected Inbox snapshot"),
        }
    }

    #[test]
    fn read_unknown_returns_none() {
        let mut store = UiStore::new();
        let p = path!("nonexistent");
        assert!(store.read(&p).unwrap().is_none());
    }

    // -- Global commands --

    #[test]
    fn open_thread_transitions_to_thread_screen() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::Open {
                thread_id: "t_001".to_string(),
            }),
        );
        assert_eq!(
            read_val(&mut store, "screen"),
            Value::String("thread".into())
        );
        assert_eq!(
            read_val(&mut store, "active_thread"),
            Value::String("t_001".into())
        );
    }

    #[test]
    fn close_transitions_to_inbox() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::Open {
                thread_id: "t_001".to_string(),
            }),
        );
        write_cmd(&mut store, &UiCommand::Global(GlobalCommand::Close));
        assert_eq!(
            read_val(&mut store, "screen"),
            Value::String("inbox".into())
        );
        assert_eq!(read_val(&mut store, "active_thread"), Value::Null);
    }

    #[test]
    fn close_clears_pending_action() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::Open {
                thread_id: "t_001".to_string(),
            }),
        );
        write_cmd(&mut store, &UiCommand::Thread(ThreadCommand::SubmitEditor));
        assert!(read_val(&mut store, "pending_action") != Value::Null);
        write_cmd(&mut store, &UiCommand::Global(GlobalCommand::Close));
        assert_eq!(read_val(&mut store, "pending_action"), Value::Null);
    }

    #[test]
    fn go_to_settings() {
        let mut store = UiStore::new();
        write_cmd(&mut store, &UiCommand::Global(GlobalCommand::GoToSettings));
        assert_eq!(
            read_val(&mut store, "screen"),
            Value::String("settings".into())
        );
    }

    #[test]
    fn go_to_inbox() {
        let mut store = UiStore::new();
        write_cmd(&mut store, &UiCommand::Global(GlobalCommand::GoToSettings));
        write_cmd(&mut store, &UiCommand::Global(GlobalCommand::GoToInbox));
        assert_eq!(
            read_val(&mut store, "screen"),
            Value::String("inbox".into())
        );
    }

    #[test]
    fn quit_sets_pending_action() {
        let mut store = UiStore::new();
        write_cmd(&mut store, &UiCommand::Global(GlobalCommand::Quit));
        assert_eq!(
            read_val(&mut store, "pending_action"),
            Value::String("quit".into())
        );
    }

    #[test]
    fn clear_pending_action() {
        let mut store = UiStore::new();
        write_cmd(&mut store, &UiCommand::Global(GlobalCommand::Quit));
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::ClearPendingAction),
        );
        assert_eq!(read_val(&mut store, "pending_action"), Value::Null);
    }

    #[test]
    fn set_status() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::SetStatus {
                text: "hello".to_string(),
            }),
        );
        assert_eq!(
            read_val(&mut store, "status"),
            Value::String("hello".into())
        );
    }

    #[test]
    fn set_status_empty_clears() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::SetStatus {
                text: "hello".to_string(),
            }),
        );
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::SetStatus {
                text: "".to_string(),
            }),
        );
        assert_eq!(read_val(&mut store, "status"), Value::Null);
    }

    // -- Inbox commands --

    #[test]
    fn select_next_and_prev() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Inbox(InboxCommand::SetRowCount { count: 5 }),
        );
        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SelectNext));
        assert_eq!(read_val(&mut store, "selected_row"), Value::Integer(1));
        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SelectNext));
        assert_eq!(read_val(&mut store, "selected_row"), Value::Integer(2));
        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SelectPrev));
        assert_eq!(read_val(&mut store, "selected_row"), Value::Integer(1));
    }

    #[test]
    fn select_clamps_to_bounds() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Inbox(InboxCommand::SetRowCount { count: 3 }),
        );
        // Can't go below 0
        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SelectPrev));
        assert_eq!(read_val(&mut store, "selected_row"), Value::Integer(0));
        // Go to max
        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SelectNext));
        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SelectNext));
        assert_eq!(read_val(&mut store, "selected_row"), Value::Integer(2));
        // Can't go past row_count-1
        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SelectNext));
        assert_eq!(read_val(&mut store, "selected_row"), Value::Integer(2));
    }

    #[test]
    fn set_row_count_clamps_selection() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Inbox(InboxCommand::SetRowCount { count: 10 }),
        );
        for _ in 0..8 {
            write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SelectNext));
        }
        assert_eq!(read_val(&mut store, "selected_row"), Value::Integer(8));
        // Shrink to 5 rows
        write_cmd(
            &mut store,
            &UiCommand::Inbox(InboxCommand::SetRowCount { count: 5 }),
        );
        assert_eq!(read_val(&mut store, "selected_row"), Value::Integer(4));
    }

    #[test]
    fn select_first_and_last() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Inbox(InboxCommand::SetRowCount { count: 10 }),
        );
        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SelectLast));
        assert_eq!(read_val(&mut store, "selected_row"), Value::Integer(9));
        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SelectFirst));
        assert_eq!(read_val(&mut store, "selected_row"), Value::Integer(0));
    }

    #[test]
    fn open_selected_sets_pending_action() {
        let mut store = UiStore::new();
        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::OpenSelected));
        assert_eq!(
            read_val(&mut store, "pending_action"),
            Value::String("open_selected".into())
        );
    }

    #[test]
    fn archive_selected_sets_pending_action() {
        let mut store = UiStore::new();
        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::ArchiveSelected));
        assert_eq!(
            read_val(&mut store, "pending_action"),
            Value::String("archive_selected".into())
        );
    }

    #[test]
    fn inbox_commands_fail_on_thread_screen() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::Open {
                thread_id: "t_1".to_string(),
            }),
        );
        let result = store.write(
            &path!(""),
            typed_cmd(&UiCommand::Inbox(InboxCommand::SelectNext)),
        );
        assert!(result.is_err());
    }

    // -- Thread commands --

    #[test]
    fn scroll_commands() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::Open {
                thread_id: "t_1".to_string(),
            }),
        );
        write_cmd(
            &mut store,
            &UiCommand::Thread(ThreadCommand::SetScrollMax { max: 10 }),
        );
        write_cmd(&mut store, &UiCommand::Thread(ThreadCommand::ScrollUp));
        assert_eq!(read_val(&mut store, "scroll"), Value::Integer(1));
        write_cmd(&mut store, &UiCommand::Thread(ThreadCommand::ScrollDown));
        assert_eq!(read_val(&mut store, "scroll"), Value::Integer(0));
    }

    #[test]
    fn scroll_clamps_to_max() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::Open {
                thread_id: "t_1".to_string(),
            }),
        );
        write_cmd(
            &mut store,
            &UiCommand::Thread(ThreadCommand::SetScrollMax { max: 5 }),
        );
        for _ in 0..10 {
            write_cmd(&mut store, &UiCommand::Thread(ThreadCommand::ScrollUp));
        }
        assert_eq!(read_val(&mut store, "scroll"), Value::Integer(5));
    }

    #[test]
    fn set_scroll_max_clamps_current() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::Open {
                thread_id: "t_1".to_string(),
            }),
        );
        write_cmd(
            &mut store,
            &UiCommand::Thread(ThreadCommand::SetScrollMax { max: 10 }),
        );
        for _ in 0..8 {
            write_cmd(&mut store, &UiCommand::Thread(ThreadCommand::ScrollUp));
        }
        assert_eq!(read_val(&mut store, "scroll"), Value::Integer(8));
        write_cmd(
            &mut store,
            &UiCommand::Thread(ThreadCommand::SetScrollMax { max: 3 }),
        );
        assert_eq!(read_val(&mut store, "scroll"), Value::Integer(3));
    }

    #[test]
    fn scroll_to_top_and_bottom() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::Open {
                thread_id: "t_1".to_string(),
            }),
        );
        write_cmd(
            &mut store,
            &UiCommand::Thread(ThreadCommand::SetScrollMax { max: 50 }),
        );
        write_cmd(&mut store, &UiCommand::Thread(ThreadCommand::ScrollToTop));
        assert_eq!(read_val(&mut store, "scroll"), Value::Integer(50));
        write_cmd(
            &mut store,
            &UiCommand::Thread(ThreadCommand::ScrollToBottom),
        );
        assert_eq!(read_val(&mut store, "scroll"), Value::Integer(0));
    }

    #[test]
    fn page_and_half_page_scroll() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::Open {
                thread_id: "t_1".to_string(),
            }),
        );
        write_cmd(
            &mut store,
            &UiCommand::Thread(ThreadCommand::SetScrollMax { max: 100 }),
        );
        write_cmd(
            &mut store,
            &UiCommand::Thread(ThreadCommand::SetViewportHeight { height: 20 }),
        );

        write_cmd(&mut store, &UiCommand::Thread(ThreadCommand::ScrollPageUp));
        assert_eq!(read_val(&mut store, "scroll"), Value::Integer(20));

        write_cmd(
            &mut store,
            &UiCommand::Thread(ThreadCommand::ScrollHalfPageUp),
        );
        assert_eq!(read_val(&mut store, "scroll"), Value::Integer(30));

        write_cmd(
            &mut store,
            &UiCommand::Thread(ThreadCommand::ScrollHalfPageDown),
        );
        assert_eq!(read_val(&mut store, "scroll"), Value::Integer(20));

        write_cmd(
            &mut store,
            &UiCommand::Thread(ThreadCommand::ScrollPageDown),
        );
        assert_eq!(read_val(&mut store, "scroll"), Value::Integer(0));
    }

    #[test]
    fn page_scroll_clamps_to_max() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::Open {
                thread_id: "t_1".to_string(),
            }),
        );
        write_cmd(
            &mut store,
            &UiCommand::Thread(ThreadCommand::SetScrollMax { max: 10 }),
        );
        write_cmd(
            &mut store,
            &UiCommand::Thread(ThreadCommand::SetViewportHeight { height: 20 }),
        );
        write_cmd(&mut store, &UiCommand::Thread(ThreadCommand::ScrollPageUp));
        assert_eq!(read_val(&mut store, "scroll"), Value::Integer(10));
    }

    #[test]
    fn reply_and_dismiss_editor() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::Open {
                thread_id: "t_1".to_string(),
            }),
        );
        write_cmd(&mut store, &UiCommand::Thread(ThreadCommand::Reply));
        assert_eq!(read_val(&mut store, "mode"), Value::String("insert".into()));
        assert_eq!(
            read_val(&mut store, "insert_context"),
            Value::String("reply".into())
        );

        write_cmd(&mut store, &UiCommand::Thread(ThreadCommand::DismissEditor));
        assert_eq!(read_val(&mut store, "mode"), Value::String("normal".into()));
        assert_eq!(read_val(&mut store, "insert_context"), Value::Null);
    }

    #[test]
    fn reply_creates_clean_editor() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::Open {
                thread_id: "t_1".to_string(),
            }),
        );
        // Reply — should create editor with empty content
        write_cmd(&mut store, &UiCommand::Thread(ThreadCommand::Reply));
        assert_eq!(
            read_val(&mut store, "input/content"),
            Value::String("".into())
        );
        assert_eq!(read_val(&mut store, "cursor"), Value::Integer(0));
    }

    #[test]
    fn editor_content_via_input_edit() {
        use crate::text_input_store::{Edit, EditOp, EditSequence, EditSource};
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::Open {
                thread_id: "t_1".to_string(),
            }),
        );
        write_cmd(&mut store, &UiCommand::Thread(ThreadCommand::Reply));

        let seq = EditSequence {
            edits: vec![Edit {
                op: EditOp::Insert {
                    text: "hello".to_string(),
                },
                at: 0,
                source: EditSource::Key,
                ts_ms: 0,
            }],
            generation: 0,
        };
        let value = structfs_serde_store::to_value(&seq).unwrap();
        store
            .write(&path!("input/edit"), Record::parsed(value))
            .unwrap();

        let snap = read_val(&mut store, "input");
        match snap {
            Value::Map(m) => {
                assert_eq!(m.get("content"), Some(&Value::String("hello".to_string())));
                assert_eq!(m.get("cursor"), Some(&Value::Integer(5)));
            }
            _ => panic!("expected Map from input read"),
        }

        // Read via legacy "cursor" path
        assert_eq!(read_val(&mut store, "cursor"), Value::Integer(5));
    }

    #[test]
    fn submit_editor_sets_pending_action() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::Open {
                thread_id: "t_1".to_string(),
            }),
        );
        write_cmd(&mut store, &UiCommand::Thread(ThreadCommand::SubmitEditor));
        assert_eq!(
            read_val(&mut store, "pending_action"),
            Value::String("send_input".into())
        );
    }

    #[test]
    fn thread_commands_fail_on_inbox_screen() {
        let mut store = UiStore::new();
        let result = store.write(
            &path!(""),
            typed_cmd(&UiCommand::Thread(ThreadCommand::ScrollUp)),
        );
        assert!(result.is_err());
    }

    // -- Search commands --

    #[test]
    fn search_insert_char() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Inbox(InboxCommand::SearchInsertChar { char: 'h' }),
        );
        assert_eq!(
            read_val(&mut store, "search_live_query"),
            Value::String("h".into())
        );
        write_cmd(
            &mut store,
            &UiCommand::Inbox(InboxCommand::SearchInsertChar { char: 'i' }),
        );
        assert_eq!(
            read_val(&mut store, "search_live_query"),
            Value::String("hi".into())
        );
    }

    #[test]
    fn search_delete_char() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Inbox(InboxCommand::SearchInsertChar { char: 'a' }),
        );
        write_cmd(
            &mut store,
            &UiCommand::Inbox(InboxCommand::SearchInsertChar { char: 'b' }),
        );
        write_cmd(
            &mut store,
            &UiCommand::Inbox(InboxCommand::SearchDeleteChar),
        );
        assert_eq!(
            read_val(&mut store, "search_live_query"),
            Value::String("a".into())
        );
    }

    #[test]
    fn search_delete_char_empty_is_noop() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Inbox(InboxCommand::SearchDeleteChar),
        );
        assert_eq!(
            read_val(&mut store, "search_live_query"),
            Value::String("".into())
        );
    }

    #[test]
    fn search_clear() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Inbox(InboxCommand::SearchInsertChar { char: 'x' }),
        );
        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SearchClear));
        assert_eq!(
            read_val(&mut store, "search_live_query"),
            Value::String("".into())
        );
    }

    #[test]
    fn search_save_chip() {
        let mut store = UiStore::new();
        for ch in ['b', 'u', 'g'] {
            write_cmd(
                &mut store,
                &UiCommand::Inbox(InboxCommand::SearchInsertChar { char: ch }),
            );
        }
        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SearchSaveChip));
        assert_eq!(
            read_val(&mut store, "search_chips"),
            Value::Array(vec![Value::String("bug".into())])
        );
        assert_eq!(
            read_val(&mut store, "search_live_query"),
            Value::String("".into())
        );
    }

    #[test]
    fn search_save_chip_trims_whitespace() {
        let mut store = UiStore::new();
        for ch in [' ', 'a', ' '] {
            write_cmd(
                &mut store,
                &UiCommand::Inbox(InboxCommand::SearchInsertChar { char: ch }),
            );
        }
        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SearchSaveChip));
        assert_eq!(
            read_val(&mut store, "search_chips"),
            Value::Array(vec![Value::String("a".into())])
        );
    }

    #[test]
    fn search_save_chip_empty_is_noop() {
        let mut store = UiStore::new();
        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SearchSaveChip));
        assert_eq!(read_val(&mut store, "search_chips"), Value::Array(vec![]));
    }

    #[test]
    fn search_dismiss_chip() {
        let mut store = UiStore::new();
        for word in ["alpha", "beta"] {
            for ch in word.chars() {
                write_cmd(
                    &mut store,
                    &UiCommand::Inbox(InboxCommand::SearchInsertChar { char: ch }),
                );
            }
            write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SearchSaveChip));
        }
        write_cmd(
            &mut store,
            &UiCommand::Inbox(InboxCommand::SearchDismissChip { index: 0 }),
        );
        assert_eq!(
            read_val(&mut store, "search_chips"),
            Value::Array(vec![Value::String("beta".into())])
        );
    }

    #[test]
    fn search_dismiss_chip_out_of_bounds_is_noop() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Inbox(InboxCommand::SearchDismissChip { index: 99 }),
        );
        assert_eq!(read_val(&mut store, "search_chips"), Value::Array(vec![]));
    }

    #[test]
    fn search_active_derived() {
        let mut store = UiStore::new();
        assert_eq!(read_val(&mut store, "search_active"), Value::Bool(false));

        write_cmd(
            &mut store,
            &UiCommand::Inbox(InboxCommand::SearchInsertChar { char: 'x' }),
        );
        assert_eq!(read_val(&mut store, "search_active"), Value::Bool(true));

        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SearchClear));
        assert_eq!(read_val(&mut store, "search_active"), Value::Bool(false));

        write_cmd(
            &mut store,
            &UiCommand::Inbox(InboxCommand::SearchInsertChar { char: 'y' }),
        );
        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SearchSaveChip));
        assert_eq!(read_val(&mut store, "search_active"), Value::Bool(true));
    }

    // -- Snapshot round-trip --

    #[test]
    fn snapshot_inbox_round_trip() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Inbox(InboxCommand::SetRowCount { count: 5 }),
        );
        write_cmd(&mut store, &UiCommand::Inbox(InboxCommand::SelectNext));
        let snap = read_snapshot(&mut store);
        match snap.screen {
            ScreenSnapshot::Inbox(s) => {
                assert_eq!(s.selected_row, 1);
                assert_eq!(s.row_count, 5);
            }
            _ => panic!("expected Inbox snapshot"),
        }
    }

    #[test]
    fn snapshot_thread_round_trip() {
        let mut store = UiStore::new();
        write_cmd(
            &mut store,
            &UiCommand::Global(GlobalCommand::Open {
                thread_id: "t_42".to_string(),
            }),
        );
        let snap = read_snapshot(&mut store);
        match snap.screen {
            ScreenSnapshot::Thread(s) => {
                assert_eq!(s.thread_id, "t_42");
                assert!(s.editor.is_none());
            }
            _ => panic!("expected Thread snapshot"),
        }
    }

    #[test]
    fn snapshot_settings_round_trip() {
        let mut store = UiStore::new();
        write_cmd(&mut store, &UiCommand::Global(GlobalCommand::GoToSettings));
        let snap = read_snapshot(&mut store);
        match snap.screen {
            ScreenSnapshot::Settings(s) => {
                assert_eq!(s.focus, SettingsFocus::Accounts);
                assert!(s.editing.is_none());
            }
            _ => panic!("expected Settings snapshot"),
        }
    }

    // -- Settings commands --

    #[test]
    fn settings_toggle_focus() {
        let mut store = UiStore::new();
        write_cmd(&mut store, &UiCommand::Global(GlobalCommand::GoToSettings));
        write_cmd(
            &mut store,
            &UiCommand::Settings(SettingsCommand::ToggleFocus),
        );
        let snap = read_snapshot(&mut store);
        match snap.screen {
            ScreenSnapshot::Settings(s) => assert_eq!(s.focus, SettingsFocus::Defaults),
            _ => panic!("expected Settings"),
        }
        write_cmd(
            &mut store,
            &UiCommand::Settings(SettingsCommand::ToggleFocus),
        );
        let snap = read_snapshot(&mut store);
        match snap.screen {
            ScreenSnapshot::Settings(s) => assert_eq!(s.focus, SettingsFocus::Accounts),
            _ => panic!("expected Settings"),
        }
    }

    #[test]
    fn settings_commands_fail_on_inbox() {
        let mut store = UiStore::new();
        let result = store.write(
            &path!(""),
            typed_cmd(&UiCommand::Settings(SettingsCommand::ToggleFocus)),
        );
        assert!(result.is_err());
    }

    #[test]
    fn settings_start_add_account() {
        let mut store = UiStore::new();
        write_cmd(&mut store, &UiCommand::Global(GlobalCommand::GoToSettings));
        write_cmd(
            &mut store,
            &UiCommand::Settings(SettingsCommand::StartAddAccount),
        );
        let snap = read_snapshot(&mut store);
        match snap.screen {
            ScreenSnapshot::Settings(s) => {
                assert!(s.editing.is_some());
                assert!(s.editing.unwrap().is_new);
            }
            _ => panic!("expected Settings"),
        }
    }

    #[test]
    fn settings_edit_cancel() {
        let mut store = UiStore::new();
        write_cmd(&mut store, &UiCommand::Global(GlobalCommand::GoToSettings));
        write_cmd(
            &mut store,
            &UiCommand::Settings(SettingsCommand::StartAddAccount),
        );
        write_cmd(
            &mut store,
            &UiCommand::Settings(SettingsCommand::EditCancel),
        );
        let snap = read_snapshot(&mut store);
        match snap.screen {
            ScreenSnapshot::Settings(s) => assert!(s.editing.is_none()),
            _ => panic!("expected Settings"),
        }
    }

    // -- Error on wrong screen --

    #[test]
    fn unknown_write_returns_error() {
        let mut store = UiStore::new();
        let result = store.write(
            &path!(""),
            Record::parsed(Value::String("not a command".into())),
        );
        assert!(result.is_err());
    }

    // -- Search fields in snapshot --

    #[test]
    fn search_fields_in_inbox_snapshot() {
        let mut store = UiStore::new();
        let snap = read_snapshot(&mut store);
        match snap.screen {
            ScreenSnapshot::Inbox(s) => {
                assert!(s.search.chips.is_empty());
                assert!(s.search.live_query.is_empty());
                assert!(!s.search.active);
            }
            _ => panic!("expected Inbox"),
        }
    }
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "scope", content = "command", rename_all = "snake_case")]
pub enum UiCommand {
    Global(GlobalCommand),
    Inbox(InboxCommand),
    Thread(ThreadCommand),
    Settings(SettingsCommand),
    History(HistoryCommand),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum GlobalCommand {
    Quit,
    Open { thread_id: String },
    Close,
    GoToSettings,
    GoToInbox,
    OpenHistory { thread_id: String },
    BackToThread { thread_id: String },
    SetStatus { text: String },
    ClearPendingAction,
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
    Compose,
    Search,
    DismissEditor,
    SubmitEditor,
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
    Reply,
    Command,
    DismissEditor,
    SubmitEditor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum SettingsCommand {
    FocusAccounts,
    FocusDefaults,
    ToggleFocus,
    SelectNextAccount,
    SelectPrevAccount,
    SelectNextDefault,
    SelectPrevDefault,
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
    EditFocusNext,
    EditFocusPrev,
    EditFocusField {
        field: usize,
    },
    EditDialectNext,
    EditDialectPrev,
    EditInsertChar {
        char: char,
    },
    EditBackspace,
    EditSave {
        name: String,
        provider: String,
        endpoint: Option<String>,
        key: String,
    },
    EditCancel,
    DefaultAccountNext,
    DefaultAccountPrev,
    DefaultModelNext,
    DefaultModelPrev,
    DefaultModelInsertChar {
        char: char,
    },
    DefaultModelBackspace,
    DefaultMaxTokensInsertChar {
        char: char,
    },
    DefaultMaxTokensBackspace,
    SaveDefaults {
        account: String,
        model: String,
        max_tokens: i64,
    },
    FinishWizard,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum HistoryCommand {
    SelectNext,
    SelectPrev,
    SelectFirst,
    SelectLast,
    ToggleExpand,
    ExpandAll,
    CollapseAll,
    TogglePretty,
    ToggleFull,
    SelectPageUp,
    SelectPageDown,
    SelectHalfPageUp,
    SelectHalfPageDown,
    SetRowCount { count: usize },
}

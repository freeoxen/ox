use serde::{Deserialize, Serialize};

use crate::Decision;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Screen {
    #[default]
    Inbox,
    Thread,
    Settings,
    History,
}

impl Screen {
    pub fn as_str(self) -> &'static str {
        match self {
            Screen::Inbox => "inbox",
            Screen::Thread => "thread",
            Screen::Settings => "settings",
            Screen::History => "history",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "inbox" => Some(Screen::Inbox),
            "thread" => Some(Screen::Thread),
            "settings" => Some(Screen::Settings),
            "history" => Some(Screen::History),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Approval,
    HistorySearch,
    Shortcuts,
    Usage,
    Customize,
    /// The global `:` command line is open.
    Command,
    /// The inbox search input is open.
    Search,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::Insert => "insert",
            Mode::Approval => "approval",
            Mode::HistorySearch => "history_search",
            Mode::Shortcuts => "shortcuts",
            Mode::Usage => "usage",
            Mode::Customize => "customize",
            Mode::Command => "command",
            Mode::Search => "search",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "normal" => Some(Mode::Normal),
            "insert" => Some(Mode::Insert),
            "approval" => Some(Mode::Approval),
            "history_search" => Some(Mode::HistorySearch),
            "shortcuts" => Some(Mode::Shortcuts),
            "usage" => Some(Mode::Usage),
            "customize" => Some(Mode::Customize),
            "command" => Some(Mode::Command),
            "search" => Some(Mode::Search),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertContext {
    Compose,
    Reply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingAction {
    SendInput,
    Quit,
    OpenSelected,
    ArchiveSelected,
    ApprovalConfirm,
    Approve(Decision),
    ToggleShortcuts,
    DismissShortcuts,
    DismissUsage,
    ToggleUsage,
    EnterHistorySearch,
    HistorySearchCycle,
    AcceptHistorySearch,
    DismissHistorySearch,
    ToggleEditorMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsFocus {
    #[default]
    Accounts,
    Defaults,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WizardStep {
    AddAccount,
    SetDefaults,
    Done,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountEditFields {
    pub name: String,
    pub dialect: usize,
    pub endpoint: String,
    pub key: String,
    pub focus: usize,
    pub is_new: bool,
}

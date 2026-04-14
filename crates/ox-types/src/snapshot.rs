use serde::{Deserialize, Serialize};

use crate::ui::{AccountEditFields, InsertContext, PendingAction, SettingsFocus, WizardStep};

/// Top-level UI state snapshot — struct with screen variant and pending action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSnapshot {
    pub screen: ScreenSnapshot,
    pub pending_action: Option<PendingAction>,
}

impl UiSnapshot {
    /// Access the active editor regardless of which screen owns it.
    pub fn editor(&self) -> Option<&EditorSnapshot> {
        match &self.screen {
            ScreenSnapshot::Inbox(s) => s.editor.as_ref(),
            ScreenSnapshot::Thread(s) => s.editor.as_ref(),
            ScreenSnapshot::Settings(_) => None,
            ScreenSnapshot::History(_) => None,
        }
    }

    pub fn pending_action(&self) -> Option<PendingAction> {
        self.pending_action
    }
}

impl Default for UiSnapshot {
    fn default() -> Self {
        UiSnapshot {
            screen: ScreenSnapshot::Inbox(InboxSnapshot::default()),
            pending_action: None,
        }
    }
}

/// Which screen is active, with its snapshot data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "screen", rename_all = "snake_case")]
pub enum ScreenSnapshot {
    Inbox(InboxSnapshot),
    Thread(ThreadSnapshot),
    Settings(SettingsSnapshot),
    History(HistorySnapshot),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InboxSnapshot {
    pub selected_row: usize,
    pub row_count: usize,
    pub editor: Option<EditorSnapshot>,
    pub search: SearchSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSnapshot {
    pub thread_id: String,
    pub scroll: usize,
    pub scroll_max: usize,
    pub viewport_height: usize,
    pub editor: Option<EditorSnapshot>,
}

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySnapshot {
    pub thread_id: String,
    pub selected_row: usize,
    pub scroll: usize,
    pub scroll_max: usize,
    pub viewport_height: usize,
    /// Message indices currently expanded for detail view.
    pub expanded: Vec<usize>,
}

/// Snapshot of the text editor widget's state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorSnapshot {
    pub context: InsertContext,
    pub content: String,
    pub cursor: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchSnapshot {
    pub chips: Vec<String>,
    pub live_query: String,
    pub active: bool,
}

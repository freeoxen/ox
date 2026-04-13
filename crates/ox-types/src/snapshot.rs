use serde::{Deserialize, Serialize};

use crate::ui::{AccountEditFields, InsertContext, Mode, PendingAction, SettingsFocus, WizardStep};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "screen", rename_all = "snake_case")]
pub enum UiSnapshot {
    Inbox(InboxSnapshot),
    Thread(ThreadSnapshot),
    Settings(SettingsSnapshot),
}

impl UiSnapshot {
    pub fn pending_action(&self) -> Option<PendingAction> {
        match self {
            UiSnapshot::Inbox(s) => s.pending_action,
            UiSnapshot::Thread(s) => s.pending_action,
            UiSnapshot::Settings(s) => s.pending_action,
        }
    }
}

impl Default for UiSnapshot {
    fn default() -> Self {
        UiSnapshot::Inbox(InboxSnapshot::default())
    }
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InputSnapshot {
    pub content: String,
    pub cursor: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchSnapshot {
    pub chips: Vec<String>,
    pub live_query: String,
    pub active: bool,
}

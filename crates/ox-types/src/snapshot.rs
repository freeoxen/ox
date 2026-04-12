use serde::{Deserialize, Serialize};

use crate::ui::{InsertContext, Mode, PendingAction, Screen};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiSnapshot {
    pub screen: Screen,
    pub mode: Mode,
    pub active_thread: Option<String>,
    pub insert_context: Option<InsertContext>,
    pub selected_row: usize,
    pub scroll: usize,
    pub scroll_max: usize,
    pub viewport_height: usize,
    pub input: InputSnapshot,
    pub pending_action: Option<PendingAction>,
    pub search: SearchSnapshot,
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

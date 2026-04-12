use serde::{Deserialize, Serialize};

use crate::ui::{Mode, Screen};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputKeyEvent {
    pub mode: Mode,
    pub key: String,
    pub screen: Screen,
}

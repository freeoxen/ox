use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyHint {
    pub key: String,
    pub description: String,
    /// If true, this hint should appear in the status bar (curated subset).
    #[serde(default)]
    pub status_hint: bool,
}

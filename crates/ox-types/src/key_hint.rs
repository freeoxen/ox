use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyHint {
    pub key: String,
    pub description: String,
    /// Command name for grouping (e.g. "select_next", "compose").
    #[serde(default)]
    pub command: String,
    /// If true, this hint should appear in the status bar (curated subset).
    #[serde(default)]
    pub status_hint: bool,
}

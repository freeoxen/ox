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
    /// Curation priority carried from the source `BindingEntry`. Lower
    /// = more important. The status-bar renderer sorts by priority
    /// ascending and keeps the top N hints that fit available width.
    /// Defaults match `BindingEntry`'s default priority (200) when the
    /// source binding didn't set one.
    #[serde(default = "default_key_hint_priority")]
    pub priority: u8,
}

fn default_key_hint_priority() -> u8 {
    // Mirrors `horns_core::DEFAULT_BINDING_PRIORITY` without taking
    // the dep — ox-types is the lighter crate of the two.
    200
}

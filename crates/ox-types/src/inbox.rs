use serde::{Deserialize, Serialize};

/// Write to `threads` to create a new thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateThread {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

/// Write to `threads/{id}` to update thread metadata.
/// All fields are optional — only provided fields are updated by the inbox store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateThread {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inbox_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

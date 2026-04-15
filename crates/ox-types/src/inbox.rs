use serde::{Deserialize, Serialize};

/// Thread lifecycle state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadState {
    Running,
    WaitingForInput,
    BlockedOnApproval,
    Completed,
    Errored,
    Interrupted,
}

impl ThreadState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ThreadState::Running => "running",
            ThreadState::WaitingForInput => "waiting_for_input",
            ThreadState::BlockedOnApproval => "blocked_on_approval",
            ThreadState::Completed => "completed",
            ThreadState::Errored => "errored",
            ThreadState::Interrupted => "interrupted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "running" => Some(ThreadState::Running),
            "waiting_for_input" => Some(ThreadState::WaitingForInput),
            "blocked_on_approval" => Some(ThreadState::BlockedOnApproval),
            "completed" => Some(ThreadState::Completed),
            "errored" => Some(ThreadState::Errored),
            "interrupted" => Some(ThreadState::Interrupted),
            _ => None,
        }
    }
}

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
    pub thread_state: Option<ThreadState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inbox_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

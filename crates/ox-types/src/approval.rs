use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub tool_name: String,
    /// Raw tool input as JSON — the rendering layer decides how to present it.
    pub tool_input: serde_json::Value,
}

/// The outcome of a tool approval prompt.
///
/// `CancelTurn` is a third kind — neither allow nor deny. It is reserved
/// for the post-crash reconfirm flow (Task 3 of the durable-conversation
/// plan): the user explicitly aborts the in-flight turn rather than
/// retrying or skipping the interrupted tool. Kernel wiring that writes
/// `TurnAborted { reason: UserCanceledAfterCrash }` on this variant is
/// added in Task 3c; this variant itself exists so every `match` site
/// can be made exhaustive in one commit (Task 3a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    AllowOnce,
    AllowSession,
    AllowAlways,
    DenyOnce,
    DenySession,
    DenyAlways,
    CancelTurn,
}

impl Decision {
    /// `CancelTurn` is **not** an allow — it doesn't authorize the tool.
    pub fn is_allow(self) -> bool {
        matches!(
            self,
            Decision::AllowOnce | Decision::AllowSession | Decision::AllowAlways
        )
    }

    /// `CancelTurn` is **not** a deny — a deny feeds a denial result back
    /// to the model; a cancel aborts the whole turn. `is_allow` and
    /// `is_deny` both return `false` for `CancelTurn`; callers that need
    /// to handle all three kinds must match on the variant directly.
    pub fn is_deny(self) -> bool {
        matches!(
            self,
            Decision::DenyOnce | Decision::DenySession | Decision::DenyAlways
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Decision::AllowOnce => "allow_once",
            Decision::AllowSession => "allow_session",
            Decision::AllowAlways => "allow_always",
            Decision::DenyOnce => "deny_once",
            Decision::DenySession => "deny_session",
            Decision::DenyAlways => "deny_always",
            Decision::CancelTurn => "cancel_turn",
        }
    }
}

impl std::fmt::Display for Decision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub decision: Decision,
}

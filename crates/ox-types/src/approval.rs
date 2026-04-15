use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub tool_name: String,
    pub input_preview: String,
}

/// The outcome of a tool approval prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    AllowOnce,
    AllowSession,
    AllowAlways,
    DenyOnce,
    DenySession,
    DenyAlways,
}

impl Decision {
    pub fn is_allow(self) -> bool {
        matches!(
            self,
            Decision::AllowOnce | Decision::AllowSession | Decision::AllowAlways
        )
    }

    pub fn is_deny(self) -> bool {
        !self.is_allow()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Decision::AllowOnce => "allow_once",
            Decision::AllowSession => "allow_session",
            Decision::AllowAlways => "allow_always",
            Decision::DenyOnce => "deny_once",
            Decision::DenySession => "deny_session",
            Decision::DenyAlways => "deny_always",
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

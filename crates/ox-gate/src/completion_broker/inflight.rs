//! Per-request in-flight state for CompletionBrokerStore.
//!
//! `Inflight` is shared between the writer side (spawned dispatch task
//! pushes events + flips status) and the reader side (drains events,
//! awaits Notify for new events or terminal status). The struct owns
//! a `tokio::sync::Mutex<InflightState>` + a `Notify`.

use ox_kernel::CompletionRequest;
use ox_types::StreamEvent;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

use crate::codec::UsageInfo;

/// Lifecycle states for one in-flight completion. Serialized at
/// `outstanding/{N}` on the broker; consumers read this to know
/// whether more events are coming or the stream has terminated.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum CompletionStatus {
    Pending,
    Streaming {
        account: String,
        model_id: String,
        started_at_ms: u64,
    },
    Complete {
        account: String,
        model_id: String,
        completed_at_ms: u64,
    },
    Failed {
        account: String,
        model_id: String,
        reason: String,
        failed_at_ms: u64,
    },
}

impl CompletionStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete { .. } | Self::Failed { .. })
    }
}

pub struct Inflight {
    pub state: Mutex<InflightState>,
    pub notify: Notify,
}

impl Inflight {
    pub fn new(request: CompletionRequest) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(InflightState {
                request,
                events: Vec::new(),
                status: CompletionStatus::Pending,
                usage: None,
            }),
            notify: Notify::new(),
        })
    }
}

pub struct InflightState {
    pub request: CompletionRequest,
    pub events: Vec<StreamEvent>,
    pub status: CompletionStatus,
    pub usage: Option<UsageInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_terminal_only_for_complete_or_failed() {
        assert!(!CompletionStatus::Pending.is_terminal());
        assert!(
            !CompletionStatus::Streaming {
                account: "a".into(),
                model_id: "m".into(),
                started_at_ms: 0,
            }
            .is_terminal()
        );
        assert!(
            CompletionStatus::Complete {
                account: "a".into(),
                model_id: "m".into(),
                completed_at_ms: 1,
            }
            .is_terminal()
        );
        assert!(
            CompletionStatus::Failed {
                account: "a".into(),
                model_id: "m".into(),
                reason: "x".into(),
                failed_at_ms: 1,
            }
            .is_terminal()
        );
    }

    #[test]
    fn completion_status_serde_roundtrip() {
        let s = CompletionStatus::Streaming {
            account: "anthropic".into(),
            model_id: "claude-sonnet-4".into(),
            started_at_ms: 42,
        };
        let j = serde_json::to_string(&s).unwrap();
        let back: CompletionStatus = serde_json::from_str(&j).unwrap();
        assert_eq!(s, back);
    }
}

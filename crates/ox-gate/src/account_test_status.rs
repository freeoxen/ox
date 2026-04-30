//! Lifecycle status for "test connection" against an account.
//!
//! The four-state model (Idle → Testing → {Success, Failed}) is the same
//! shape used by `CatalogRefreshStatus`. Each transition records a
//! `*_at_ms` timestamp so the UI can show "started 3s ago" / "succeeded
//! 12s ago" without needing a separate clock channel.
//!
//! `Success` carries the dialect that responded and the round-trip
//! latency; the dialect is observed (not configured) because a misrouted
//! endpoint can answer with a different shape than the user expected and
//! we want that visible in the diagnostics UI.

use serde::{Deserialize, Serialize};

/// Status of an account-level "test connection" action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AccountTestStatus {
    /// No test in flight, no recorded result.
    Idle,
    /// Test in flight; started at the given epoch-ms timestamp.
    Testing { started_at_ms: u64 },
    /// Last test completed successfully.
    Success {
        /// Dialect observed in the response (e.g. `"anthropic"`,
        /// `"openai"`). Recorded so a misrouted endpoint surfaces as a
        /// dialect mismatch in the UI.
        dialect: String,
        /// Round-trip latency in milliseconds.
        latency_ms: u64,
        /// Epoch-ms when the test completed.
        completed_at_ms: u64,
    },
    /// Last test failed with the given reason.
    Failed {
        /// Human-readable failure reason for display in the UI.
        reason: String,
        /// Epoch-ms when the test completed.
        completed_at_ms: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(value: &AccountTestStatus) {
        let json = serde_json::to_string(value).expect("serialize");
        let parsed: AccountTestStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&parsed, value);
    }

    #[test]
    fn idle_roundtrip() {
        roundtrip(&AccountTestStatus::Idle);
    }

    #[test]
    fn testing_roundtrip() {
        roundtrip(&AccountTestStatus::Testing {
            started_at_ms: 1_700_000_000_000,
        });
    }

    #[test]
    fn success_roundtrip() {
        roundtrip(&AccountTestStatus::Success {
            dialect: "anthropic".to_string(),
            latency_ms: 142,
            completed_at_ms: 1_700_000_000_500,
        });
    }

    #[test]
    fn failed_roundtrip() {
        roundtrip(&AccountTestStatus::Failed {
            reason: "401 unauthorized: invalid api key".to_string(),
            completed_at_ms: 1_700_000_000_300,
        });
    }
}

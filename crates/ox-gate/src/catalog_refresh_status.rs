//! Lifecycle status for "refresh model catalog" against an account.
//!
//! Same four-state shape as `AccountTestStatus` (Idle → Refreshing →
//! {Success, Failed}). On success, the counters distinguish brand-new
//! entries from updated ones so the UI can render an honest summary
//! without re-diffing the catalog.

use serde::{Deserialize, Serialize};

/// Status of a catalog-refresh action for an account.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CatalogRefreshStatus {
    /// No refresh in flight, no recorded result.
    Idle,
    /// Refresh in flight; started at the given epoch-ms timestamp.
    Refreshing { started_at_ms: u64 },
    /// Last refresh completed successfully.
    Success {
        /// Models present in the new catalog that were not in the old one.
        models_added: u32,
        /// Models present in both catalogs whose metadata changed.
        models_updated: u32,
        /// Epoch-ms when the refresh completed.
        completed_at_ms: u64,
    },
    /// Last refresh failed with the given reason.
    Failed {
        /// Human-readable failure reason for display in the UI.
        reason: String,
        /// Epoch-ms when the refresh completed.
        completed_at_ms: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(value: &CatalogRefreshStatus) {
        let json = serde_json::to_string(value).expect("serialize");
        let parsed: CatalogRefreshStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&parsed, value);
    }

    #[test]
    fn idle_roundtrip() {
        roundtrip(&CatalogRefreshStatus::Idle);
    }

    #[test]
    fn refreshing_roundtrip() {
        roundtrip(&CatalogRefreshStatus::Refreshing {
            started_at_ms: 1_700_000_000_000,
        });
    }

    #[test]
    fn success_roundtrip() {
        roundtrip(&CatalogRefreshStatus::Success {
            models_added: 3,
            models_updated: 11,
            completed_at_ms: 1_700_000_000_750,
        });
    }

    #[test]
    fn failed_roundtrip() {
        roundtrip(&CatalogRefreshStatus::Failed {
            reason: "network unreachable".to_string(),
            completed_at_ms: 1_700_000_000_400,
        });
    }
}

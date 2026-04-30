//! Named completion role binding.
//!
//! A `CompletionRole` names a (account, model_id) pair under a stable
//! role name. Stored at `config/completions/{role_name}`; the day-one role
//! is `primary`. Replaces the older split `gate/defaults/{account, model}`
//! pair, where the two halves could drift apart silently.
//!
//! Field name `model_id` matches `ModelKey.model_id` in `ox-types` so a
//! Models-screen selection lifts directly into a role binding without any
//! field renaming on the way through.

use serde::{Deserialize, Serialize};

/// A named binding from role to (account, model_id).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompletionRole {
    /// Account name (key into `gate/accounts/{name}`).
    pub account: String,
    /// Model identifier as it appears in that account's catalog.
    pub model_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_serde_store::{from_value, to_value};

    #[test]
    fn json_roundtrip() {
        let original = CompletionRole {
            account: "anthropic".to_string(),
            model_id: "claude-sonnet-4-20250514".to_string(),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: CompletionRole = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, original);
    }

    #[test]
    fn structfs_value_roundtrip() {
        let original = CompletionRole {
            account: "openai".to_string(),
            model_id: "gpt-4o".to_string(),
        };
        let value = to_value(&original).expect("to_value");
        let parsed: CompletionRole = from_value(value).expect("from_value");
        assert_eq!(parsed, original);
    }
}

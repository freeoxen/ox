//! `ApiKey` — newtype wrapper around an API-key string.
//!
//! Keys live at `secret/keys/{account}` in StructFS. Wrapping them in a
//! newtype gives them a typed identity for `read_typed` / `write_typed` and
//! prevents accidental mixing with other strings in the namespace. Display
//! is intentionally not derived: leaking a key into a log via `{:?}` /
//! `{}` should require an explicit `.0` or `.expose()`.

use serde::{Deserialize, Serialize};

/// API key for an account, stored at `secret/keys/{account}` in StructFS.
///
/// The wrapped string is the raw key as the provider expects it. The
/// `Debug` impl elides the value to keep keys out of trace output.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct ApiKey(pub String);

impl ApiKey {
    /// Construct an `ApiKey` from a string.
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// Borrow the wrapped string. Use sparingly — every callsite is a
    /// place a key could leak into logs.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// `true` when the wrapped string is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Elide the key body. `len` is informative without leaking the value.
        f.debug_tuple("ApiKey")
            .field(&format_args!("<{} chars>", self.0.len()))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_round_trip_via_serde_json() {
        let key = ApiKey::new("sk-abc-123");
        let json = serde_json::to_string(&key).expect("serialize");
        // Transparent representation: just the underlying string literal.
        assert_eq!(json, "\"sk-abc-123\"");
        let back: ApiKey = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, key);
    }

    #[test]
    fn serde_round_trip_via_structfs_value() {
        use structfs_serde_store::{from_value, to_value};
        let key = ApiKey::new("sk-xyz");
        let v = to_value(&key).expect("to_value");
        let back: ApiKey = from_value(v).expect("from_value");
        assert_eq!(back, key);
    }

    #[test]
    fn debug_does_not_leak_key_body() {
        let key = ApiKey::new("sk-secret-xyz");
        let printed = format!("{key:?}");
        assert!(!printed.contains("sk-secret-xyz"), "Debug must elide key body, got: {printed}");
        assert!(printed.contains("13 chars"), "Debug should report length, got: {printed}");
    }

    #[test]
    fn is_empty_reflects_inner_string() {
        assert!(ApiKey::new("").is_empty());
        assert!(!ApiKey::new("k").is_empty());
    }
}

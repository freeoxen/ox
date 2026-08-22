//! Account configuration for LLM API access.

use serde::{Deserialize, Serialize};

/// An account binds a named provider to its configuration.
///
/// API keys are resolved separately from key files and environment
/// variables — they do not live on this type.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccountConfig {
    /// Name of the provider dialect (e.g. `"anthropic"`, `"openai"`).
    pub provider: String,
    /// User-typed display name (arbitrary Unicode). When `None`, renderers
    /// fall back to the path component. `#[serde(default)]` keeps old
    /// on-disk records loadable without migration.
    #[serde(default)]
    pub display_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_record_without_display_name_deserializes() {
        // Records written before `display_name` existed have a single
        // `provider` key; the new field must fall back to `None` via
        // `#[serde(default)]` so loads stay byte-compatible.
        let json = r#"{"provider":"anthropic"}"#;
        let parsed: AccountConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.provider, "anthropic");
        assert_eq!(parsed.display_name, None);
    }

    #[test]
    fn round_trip_preserves_display_name() {
        let cfg = AccountConfig {
            provider: "anthropic".to_string(),
            display_name: Some("My Personal".to_string()),
        };
        let s = serde_json::to_string(&cfg).unwrap();
        let back: AccountConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back.provider, "anthropic");
        assert_eq!(back.display_name.as_deref(), Some("My Personal"));
    }
}

//! Catalog metadata for a single model.
//!
//! `ModelInfo` lives in the gate domain — providers, accounts, and catalogs
//! are all gate concerns. The kernel reads only the primitives it needs at
//! request time (`model_id`, `max_output_tokens`) and never imports this
//! struct.
//!
//! Resolution order at request time is codec-fetch → known-family table →
//! user-override; `source` records which tier supplied the entry.
//!
//! - `max_context_size` — input ceiling. Operationally meaningful for an
//!   agent harness deciding when to compact / drop context.
//! - `max_output_tokens` — wire-required output cap; sent as the request's
//!   `max_tokens` field.

use serde::{Deserialize, Serialize};

/// A model entry in a provider's catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Model identifier (e.g. `"claude-sonnet-4-20250514"`).
    pub id: String,
    /// Human-readable name (e.g. `"Claude Sonnet 4"`).
    pub display_name: String,
    /// Maximum input context window in tokens (input ceiling), if known.
    pub max_context_size: Option<u32>,
    /// Wire-required output cap; sent as the request's `max_tokens`, if known.
    pub max_output_tokens: Option<u32>,
    /// Which tier of the resolution order produced this entry.
    pub source: ModelInfoSource,
}

/// Which tier of the resolution order produced a [`ModelInfo`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ModelInfoSource {
    /// Fetched live from the provider's models API.
    Server,
    /// Filled from a built-in known-family table (codec-fetch fallback).
    KnownTable,
    /// Set by the user via override.
    UserOverride,
    /// Added by hand because the connection can't enumerate models
    /// automatically (no /models endpoint, refresh failed, unsupported
    /// provider). Treated the same as Server everywhere except provenance
    /// display.
    UserEntered,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_info_roundtrip_all_some() {
        let original = ModelInfo {
            id: "claude-sonnet-4-20250514".to_string(),
            display_name: "Claude Sonnet 4".to_string(),
            max_context_size: Some(200_000),
            max_output_tokens: Some(8192),
            source: ModelInfoSource::Server,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: ModelInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.id, original.id);
        assert_eq!(parsed.display_name, original.display_name);
        assert_eq!(parsed.max_context_size, Some(200_000));
        assert_eq!(parsed.max_output_tokens, Some(8192));
        assert_eq!(parsed.source, ModelInfoSource::Server);
    }

    #[test]
    fn model_info_roundtrip_all_none() {
        let original = ModelInfo {
            id: "model-x".to_string(),
            display_name: "Model X".to_string(),
            max_context_size: None,
            max_output_tokens: None,
            source: ModelInfoSource::KnownTable,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: ModelInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.id, "model-x");
        assert_eq!(parsed.display_name, "Model X");
        assert!(parsed.max_context_size.is_none());
        assert!(parsed.max_output_tokens.is_none());
        assert_eq!(parsed.source, ModelInfoSource::KnownTable);
    }

    #[test]
    fn model_info_source_variants_roundtrip() {
        for variant in [
            ModelInfoSource::Server,
            ModelInfoSource::KnownTable,
            ModelInfoSource::UserOverride,
            ModelInfoSource::UserEntered,
        ] {
            let json = serde_json::to_string(&variant).expect("serialize");
            let parsed: ModelInfoSource = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn model_info_source_serializes_as_bare_pascal_case_string() {
        // Default serde for unit-only enums uses bare strings; pin that for
        // forward-compat with stored catalogs.
        assert_eq!(
            serde_json::to_string(&ModelInfoSource::Server).unwrap(),
            "\"Server\""
        );
        assert_eq!(
            serde_json::to_string(&ModelInfoSource::KnownTable).unwrap(),
            "\"KnownTable\""
        );
        assert_eq!(
            serde_json::to_string(&ModelInfoSource::UserOverride).unwrap(),
            "\"UserOverride\""
        );
        assert_eq!(
            serde_json::to_string(&ModelInfoSource::UserEntered).unwrap(),
            "\"UserEntered\""
        );
    }
}

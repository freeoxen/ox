//! Settings-screen UI records.
//!
//! Typed records consumed by both renderers (CLI) and subscription handlers
//! (broker). Field names match `ModelKey.model_id` / `account` in
//! `ox-gate::CompletionRole` so that a Models-screen selection lifts directly
//! into a role binding without any field renaming on the way through.
//!
//! Per spec §5.6 of the settings-screen redesign.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use structfs_core_store::Path;

use crate::path_serde;

/// Per-account validatable field. The `Copy` impl is load-bearing: this enum
/// is the key of `ValidationDiagnostics::field_errors` and is used pervasively
/// by the renderer to display field-by-field errors.
#[derive(Hash, Eq, PartialEq, Ord, PartialOrd, Copy, Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountField {
    Name,
    Protocol,
    Endpoint,
    Auth,
    Key,
}

/// Per-model overridable field on the Models screen.
#[derive(Hash, Eq, PartialEq, Ord, PartialOrd, Copy, Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelField {
    ContextSizeOverride,
    OutputTokensOverride,
}

/// The current stage of the manual-model entry form.
///
/// The form is a three-step state machine: the user types a model id,
/// then a context-window size, then a max-output-tokens size. Each
/// stage's commit advances to the next; the final stage's commit
/// finalizes the new `ModelInfo` into the account's catalog.
///
/// Wire format is PascalCase (`"Id"` / `"Ctx"` / `"Out"`) so it stays
/// distinguishable on read from the legacy stringly-typed values
/// (`"id"` / `"ctx"` / `"out"`) that older code paths still produce.
/// The dispatcher's mode-aware pass treats this typed shape as the
/// discriminator: typed → manual-model mode is active; legacy → fall
/// through to other passes. That dual-shape coexistence lets the new
/// command surface land before the old one retires.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum ManualModelStage {
    Id,
    Ctx,
    Out,
}

/// Identifier for a (account, model) pair. The field name `model_id` matches
/// `CompletionRole.model_id` in `ox-gate`.
#[derive(Hash, Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
pub struct ModelKey {
    pub account: String,
    pub model_id: String,
}

/// One row in the settings index (left pane).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsIndexEntry {
    pub id: String,
    pub label: String,
    pub description: String,
    #[serde(with = "path_serde")]
    pub target_cursor: Path,
    pub badge: BadgeSource,
}

/// How an index entry's right-hand badge is computed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BadgeSource {
    /// No badge.
    None,
    /// Literal text.
    Static(String),
    /// Count of immediate children at the given path.
    SubtreeCount(#[serde(with = "path_serde")] Path),
    /// Resolves to "{account} / {model}" from `config/gate/completions/primary`.
    /// Deprecated: prefer `BootstrapReference`. Retained for one release so
    /// stored SettingsIndexEntry records written under the old name still
    /// deserialize cleanly.
    PrimaryReference,
    /// Resolves to "{account} / {model}" from `config/gate/completions/bootstrap`,
    /// falling back to `config/gate/completions/primary` for migration.
    BootstrapReference,
}

/// Validation diagnostics for the currently-edited account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationDiagnostics {
    pub field_errors: BTreeMap<AccountField, String>,
    pub computed_at_ms: u64,
}

/// Cross-screen banner state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum GlobalBanner {
    None,
    Error { message: String, set_at_ms: u64 },
    Info { message: String, set_at_ms: u64 },
}

impl GlobalBanner {
    /// Build a `GlobalBanner::Error` value with `set_at_ms` set to the
    /// current epoch millis. Used by CLI command surfaces that surface
    /// validation errors to the user (e.g. invalid account names,
    /// reserved-prefix collisions).
    pub fn error(message: impl Into<String>) -> Self {
        GlobalBanner::Error {
            message: message.into(),
            set_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_roundtrip<
        T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    >(
        value: T,
    ) {
        let json = serde_json::to_string(&value).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, value);
    }

    #[test]
    fn account_field_name_roundtrip() {
        json_roundtrip(AccountField::Name);
    }

    #[test]
    fn account_field_protocol_roundtrip() {
        json_roundtrip(AccountField::Protocol);
    }

    #[test]
    fn account_field_endpoint_roundtrip() {
        json_roundtrip(AccountField::Endpoint);
    }

    #[test]
    fn account_field_auth_roundtrip() {
        json_roundtrip(AccountField::Auth);
    }

    #[test]
    fn account_field_key_roundtrip() {
        json_roundtrip(AccountField::Key);
    }

    #[test]
    fn model_field_context_size_override_roundtrip() {
        json_roundtrip(ModelField::ContextSizeOverride);
    }

    #[test]
    fn model_field_output_tokens_override_roundtrip() {
        json_roundtrip(ModelField::OutputTokensOverride);
    }

    #[test]
    fn manual_model_stage_id_roundtrip() {
        json_roundtrip(ManualModelStage::Id);
    }

    #[test]
    fn manual_model_stage_ctx_roundtrip() {
        json_roundtrip(ManualModelStage::Ctx);
    }

    #[test]
    fn manual_model_stage_out_roundtrip() {
        json_roundtrip(ManualModelStage::Out);
    }

    #[test]
    fn manual_model_stage_serializes_pascal_case() {
        // Wire format must be PascalCase so the new typed value is
        // distinguishable from the legacy stringly-typed "id"/"ctx"/"out"
        // values that older write sites still produce. The dispatcher
        // discriminates on shape; if these collided, the gating that
        // keeps Commit A dormant would fire prematurely.
        assert_eq!(serde_json::to_string(&ManualModelStage::Id).unwrap(), r#""Id""#);
        assert_eq!(serde_json::to_string(&ManualModelStage::Ctx).unwrap(), r#""Ctx""#);
        assert_eq!(serde_json::to_string(&ManualModelStage::Out).unwrap(), r#""Out""#);
    }

    #[test]
    fn manual_model_stage_rejects_legacy_lowercase() {
        // The legacy shape ("id", "ctx", "out") must NOT deserialize as
        // ManualModelStage — that's how the dispatcher distinguishes the
        // new typed value from the old stringly-typed one.
        assert!(serde_json::from_str::<ManualModelStage>(r#""id""#).is_err());
        assert!(serde_json::from_str::<ManualModelStage>(r#""ctx""#).is_err());
        assert!(serde_json::from_str::<ManualModelStage>(r#""out""#).is_err());
    }

    #[test]
    fn model_key_roundtrip() {
        json_roundtrip(ModelKey {
            account: "anthropic".to_string(),
            model_id: "claude-sonnet-4-20250514".to_string(),
        });
    }

    #[test]
    fn settings_index_entry_with_badge_none_roundtrip() {
        json_roundtrip(SettingsIndexEntry {
            id: "accounts".to_string(),
            label: "Accounts".to_string(),
            description: "Manage provider accounts".to_string(),
            target_cursor: Path::parse("settings/accounts").unwrap(),
            badge: BadgeSource::None,
        });
    }

    #[test]
    fn settings_index_entry_with_badge_static_roundtrip() {
        json_roundtrip(SettingsIndexEntry {
            id: "models".to_string(),
            label: "Models".to_string(),
            description: "Per-model overrides".to_string(),
            target_cursor: Path::parse("settings/models").unwrap(),
            badge: BadgeSource::Static("3 overrides".to_string()),
        });
    }

    #[test]
    fn settings_index_entry_with_badge_subtree_count_roundtrip() {
        json_roundtrip(SettingsIndexEntry {
            id: "accounts".to_string(),
            label: "Accounts".to_string(),
            description: "Provider accounts".to_string(),
            target_cursor: Path::parse("settings/accounts").unwrap(),
            badge: BadgeSource::SubtreeCount(Path::parse("config/gate/accounts").unwrap()),
        });
    }

    #[test]
    fn settings_index_entry_with_badge_primary_reference_roundtrip() {
        json_roundtrip(SettingsIndexEntry {
            id: "primary".to_string(),
            label: "Primary".to_string(),
            description: "Default account/model".to_string(),
            target_cursor: Path::parse("settings/primary").unwrap(),
            badge: BadgeSource::PrimaryReference,
        });
    }

    #[test]
    fn validation_diagnostics_empty_roundtrip() {
        json_roundtrip(ValidationDiagnostics {
            field_errors: BTreeMap::new(),
            computed_at_ms: 0,
        });
    }

    #[test]
    fn validation_diagnostics_populated_roundtrip() {
        let mut errors = BTreeMap::new();
        errors.insert(AccountField::Name, "must be unique".to_string());
        errors.insert(AccountField::Endpoint, "invalid URL".to_string());
        json_roundtrip(ValidationDiagnostics {
            field_errors: errors,
            computed_at_ms: 1_700_000_000_000,
        });
    }

    #[test]
    fn global_banner_none_roundtrip() {
        json_roundtrip(GlobalBanner::None);
    }

    #[test]
    fn global_banner_error_roundtrip() {
        json_roundtrip(GlobalBanner::Error {
            message: "save failed".to_string(),
            set_at_ms: 1_700_000_000_000,
        });
    }

    #[test]
    fn global_banner_info_roundtrip() {
        json_roundtrip(GlobalBanner::Info {
            message: "saved".to_string(),
            set_at_ms: 1_700_000_000_000,
        });
    }

}

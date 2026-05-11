//! Validation helper for `AccountConfig` — composes the existing per-field
//! validators (`validate_endpoint`, `AuthScheme::requires_key`) into a
//! single [`ValidationDiagnostics`] record so handlers can emit one
//! field-keyed diagnostic per write rather than five.
//!
//! Per spec §6.4: validation runs on every test/refresh trigger and
//! short-circuits the spawn when it fails. Returning `None` means "this
//! account is consistent enough to attempt the network call"; returning
//! `Some(diag)` means "stop here, write the diag, don't bill the user
//! for a doomed test".

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use ox_types::settings::{AccountField, ValidationDiagnostics};

use crate::{AccountConfig, ProviderConfig, validate_endpoint};

/// Validate an `AccountConfig` along with the resolved provider and key.
///
/// Returns `None` when every checked field is consistent. Returns
/// `Some(diagnostics)` with one entry per failing field otherwise. The
/// returned map's `BTreeMap` key ordering is deterministic, so two
/// equivalent accounts always produce equal diagnostics — useful for
/// snapshot tests.
///
/// The signature deliberately accepts borrowed `Option`s so callers can
/// pass partial state without first materializing dummy values for the
/// missing pieces; "no provider exists for this account" is a real
/// failure case the validator surfaces with `AccountField::Endpoint`.
pub fn validate_account(
    cfg: &AccountConfig,
    provider: Option<&ProviderConfig>,
    api_key: Option<&str>,
) -> Option<ValidationDiagnostics> {
    let _ = cfg;
    let mut errors: BTreeMap<AccountField, String> = BTreeMap::new();

    match provider {
        Some(p) => {
            if let Err(e) = validate_endpoint(&p.endpoint) {
                errors.insert(AccountField::Endpoint, e);
            }
            // Key requirement is a function of the provider's resolved
            // auth scheme, so the key check only runs when we have a
            // provider in hand.
            if p.resolved_auth().requires_key() && api_key.unwrap_or("").is_empty() {
                errors.insert(
                    AccountField::Key,
                    "API key required for this auth scheme".into(),
                );
            }
        }
        None => {
            errors.insert(AccountField::Endpoint, "provider not found".into());
        }
    }

    if errors.is_empty() {
        None
    } else {
        Some(ValidationDiagnostics {
            field_errors: errors,
            computed_at_ms: now_ms(),
        })
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AuthScheme;

    fn good_anthropic() -> ProviderConfig {
        ProviderConfig {
            dialect: "anthropic".into(),
            endpoint: "https://api.anthropic.com".into(),
            version: "2023-06-01".into(),
            auth: Some(AuthScheme::XApiKey),
        }
    }

    fn account() -> AccountConfig {
        AccountConfig {
            provider: "anthropic".into(),
            ..Default::default()
        }
    }

    #[test]
    fn valid_account_returns_none() {
        let diag = validate_account(&account(), Some(&good_anthropic()), Some("sk-test"));
        assert!(diag.is_none(), "expected None, got {diag:?}");
    }

    #[test]
    fn bad_endpoint_returns_endpoint_error() {
        let mut p = good_anthropic();
        p.endpoint = "no-scheme.example.com".into();
        let diag = validate_account(&account(), Some(&p), Some("sk-test")).expect("should be Some");
        assert!(
            diag.field_errors.contains_key(&AccountField::Endpoint),
            "expected Endpoint error, got: {:?}",
            diag.field_errors
        );
        assert!(!diag.field_errors.contains_key(&AccountField::Key));
    }

    #[test]
    fn missing_key_when_required_returns_key_error() {
        // Anthropic = XApiKey → key is required.
        let diag = validate_account(&account(), Some(&good_anthropic()), Some(""))
            .expect("should be Some");
        assert!(
            diag.field_errors.contains_key(&AccountField::Key),
            "expected Key error, got: {:?}",
            diag.field_errors
        );
    }

    #[test]
    fn missing_key_with_no_auth_is_fine() {
        // LM Studio shape: openai dialect + AuthScheme::None — empty key
        // is the *correct* value and must not fail validation.
        let p = ProviderConfig {
            dialect: "openai".into(),
            endpoint: "http://127.0.0.1:1234".into(),
            version: String::new(),
            auth: Some(AuthScheme::None),
        };
        let diag = validate_account(&account(), Some(&p), Some(""));
        assert!(
            diag.is_none(),
            "no-auth provider with empty key must validate"
        );
    }

    #[test]
    fn missing_provider_returns_endpoint_error() {
        let diag = validate_account(&account(), None, Some("sk-test")).expect("should be Some");
        assert!(
            diag.field_errors.contains_key(&AccountField::Endpoint),
            "expected Endpoint error, got: {:?}",
            diag.field_errors
        );
    }

    #[test]
    fn multiple_errors_compose() {
        // Bad endpoint AND missing key: both fields appear.
        let mut p = good_anthropic();
        p.endpoint = "ftp://nope".into();
        let diag = validate_account(&account(), Some(&p), Some("")).expect("should be Some");
        assert!(
            diag.field_errors.contains_key(&AccountField::Endpoint),
            "expected Endpoint, got {:?}",
            diag.field_errors
        );
        assert!(
            diag.field_errors.contains_key(&AccountField::Key),
            "expected Key, got {:?}",
            diag.field_errors
        );
        assert_eq!(diag.field_errors.len(), 2);
    }

    #[test]
    fn computed_at_ms_is_nonzero() {
        let diag = validate_account(&account(), None, None).expect("should be Some");
        // SystemTime in any modern test environment must be > 0; we just
        // pin it as a smoke test.
        assert!(diag.computed_at_ms > 0);
    }
}

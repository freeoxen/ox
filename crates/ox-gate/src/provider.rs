//! Provider configuration for LLM API endpoints.
//!
//! `endpoint` is the **base URL** the dialect dispatches against; the request
//! path (`/v1/messages`, `/v1/chat/completions`, `/v1/models`, …) is owned by
//! the dialect, not the user. See [`dialect_paths`].
//!
//! Authentication is modeled explicitly on `ProviderConfig` via [`AuthScheme`]
//! rather than inferred from the dialect or the host. That way the question
//! "does this provider need an API key?" has one answer (the field), not many
//! (heuristics scattered across UI / startup / transport that drift apart).

use serde::{Deserialize, Serialize};

/// How a provider authenticates.
///
/// Modeled as data on the provider so every code path that asks "does this
/// require a key?" (UI dialog, startup gate, request builder, test
/// connection) reads the same authoritative answer. Adding a new auth shape
/// (Azure's `api-key`, Vertex's bearer-with-prefix, …) is a new variant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuthScheme {
    /// `x-api-key: {key}` — Anthropic.
    XApiKey,
    /// `Authorization: Bearer {key}` — OpenAI and most cloud OpenAI-compat.
    BearerToken,
    /// No auth header — local servers (LM Studio, Ollama, vLLM by default).
    None,
}

impl AuthScheme {
    /// `true` if a key is required for this scheme. Used by validation
    /// at the Settings save boundary and at startup.
    pub fn requires_key(&self) -> bool {
        !matches!(self, AuthScheme::None)
    }

    /// Default for a dialect when no explicit scheme is set in config.
    /// Old configs missing the `auth` field deserialize to `None` and then
    /// resolve to this — keeps existing user data working without a write.
    pub fn default_for_dialect(dialect: &str) -> Self {
        match dialect {
            "openai" => AuthScheme::BearerToken,
            _ => AuthScheme::XApiKey,
        }
    }
}

/// Configuration for an LLM provider endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Wire format dialect: `"anthropic"` or `"openai"`.
    pub dialect: String,
    /// API base URL (no path suffix). Examples: `https://api.anthropic.com`,
    /// `http://127.0.0.1:1234`. The dialect appends its own path.
    pub endpoint: String,
    /// API version header (e.g. `"2023-06-01"` for Anthropic; empty for OpenAI).
    pub version: String,
    /// Auth scheme. `None` here means "not specified" → `resolved_auth()`
    /// derives from dialect. Concrete `Some(AuthScheme::None)` means the
    /// provider explicitly takes no auth (LM Studio, Ollama). The
    /// distinction matters for backwards-compatible deserialization of
    /// configs written before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthScheme>,
}

impl ProviderConfig {
    /// Default configuration for the Anthropic Messages API.
    pub fn anthropic() -> Self {
        Self {
            dialect: "anthropic".to_string(),
            endpoint: "https://api.anthropic.com".to_string(),
            version: "2023-06-01".to_string(),
            auth: Some(AuthScheme::XApiKey),
        }
    }

    /// Default configuration for the OpenAI Chat Completions API.
    pub fn openai() -> Self {
        Self {
            dialect: "openai".to_string(),
            endpoint: "https://api.openai.com".to_string(),
            version: String::new(),
            auth: Some(AuthScheme::BearerToken),
        }
    }

    /// The effective auth scheme, falling back to the dialect default when
    /// no explicit `auth` is set. Always prefer this over reading `auth`
    /// directly so legacy configs work uniformly.
    pub fn resolved_auth(&self) -> AuthScheme {
        self.auth
            .clone()
            .unwrap_or_else(|| AuthScheme::default_for_dialect(&self.dialect))
    }
}

/// A user-facing provider preset. Bundles the four knobs (dialect,
/// endpoint, version, auth) so a user picks one name and gets a
/// fully-formed configuration. `Custom` is the escape hatch — when chosen,
/// the Settings UI exposes all four fields for direct editing.
#[derive(Debug, Clone)]
pub struct Preset {
    /// User-facing label.
    pub label: &'static str,
    /// Canonical id used as the `gate.providers.{id}` key when this preset
    /// is the basis for a synthesized provider entry. Empty for `Custom`.
    pub id: &'static str,
    pub dialect: &'static str,
    pub endpoint: &'static str,
    pub version: &'static str,
    pub auth: AuthScheme,
    /// Whether selecting this preset should leave fields editable in the UI.
    /// `true` only for `Custom`; everything else is preset-locked unless the
    /// user explicitly picks `Custom`.
    pub custom: bool,
}

/// Built-in provider presets, in display order. Order matters: the first
/// is the default selection in the Add Account dialog.
///
/// A preset represents an **API shape** — dialect + endpoint pattern +
/// auth — not a runtime. Local model runtimes like LM Studio and Ollama
/// each expose multiple API shapes (Anthropic-compatible, OpenAI-compatible,
/// their own native), and the user picks which to use; the preset list
/// must not collapse those choices into a single "LM Studio" option.
/// Custom is the escape hatch for any URL the user wants to point at.
pub fn presets() -> &'static [Preset] {
    // Function rather than `const` because `AuthScheme` is not `Copy` for
    // forward-compat (e.g. a future `Custom { header, prefix }` variant
    // could carry owned strings).
    &[
        Preset {
            label: "Anthropic",
            id: "anthropic",
            dialect: "anthropic",
            endpoint: "https://api.anthropic.com",
            version: "2023-06-01",
            auth: AuthScheme::XApiKey,
            custom: false,
        },
        Preset {
            label: "OpenAI",
            id: "openai",
            dialect: "openai",
            endpoint: "https://api.openai.com",
            version: "",
            auth: AuthScheme::BearerToken,
            custom: false,
        },
        Preset {
            label: "Custom…",
            id: "",
            dialect: "openai",
            endpoint: "",
            version: "",
            auth: AuthScheme::None,
            custom: true,
        },
    ]
}

/// Per-dialect URL paths. `endpoint + completion_path` forms the completion
/// URL; `endpoint + models_path` forms the models-listing URL.
#[derive(Debug, Clone, Copy)]
pub struct DialectPaths {
    pub completion: &'static str,
    pub models: &'static str,
}

/// Look up the dialect's path suffixes. Falls back to the Anthropic shape for
/// unknown dialects so `endpoint + completion` still produces a URL — the
/// downstream HTTP error will surface dialect mismatches more clearly than a
/// silent fallback to `/`.
pub fn dialect_paths(dialect: &str) -> DialectPaths {
    match dialect {
        "openai" => DialectPaths {
            completion: "/v1/chat/completions",
            models: "/v1/models",
        },
        _ => DialectPaths {
            completion: "/v1/messages",
            models: "/v1/models",
        },
    }
}

/// Compose the request URL for the given dialect.
///
/// Normalization:
/// - A single trailing slash is trimmed.
/// - A legacy completion suffix (e.g. `/v1/chat/completions` left over from
///   pre-split configs) is dropped with a warning.
///
/// Endpoints **must** include a scheme (`http://` or `https://`). Schemes
/// are not inferred — guessing from host or port is wrong for too many real
/// configs. Use [`validate_endpoint`] at write time to surface this to the
/// user with a friendly error.
pub fn completion_url(config: &ProviderConfig) -> String {
    let paths = dialect_paths(&config.dialect);
    compose_url(&config.endpoint, paths.completion, &config.dialect)
}

/// Compose the models-listing URL for the given dialect.
pub fn models_url(config: &ProviderConfig) -> String {
    let paths = dialect_paths(&config.dialect);
    compose_url(&config.endpoint, paths.models, &config.dialect)
}

fn compose_url(endpoint: &str, suffix: &str, dialect: &str) -> String {
    let trimmed = trim_trailing_slash(endpoint);
    let stripped = strip_known_completion_suffix(trimmed, dialect);
    format!("{stripped}{suffix}")
}

fn trim_trailing_slash(s: &str) -> &str {
    s.strip_suffix('/').unwrap_or(s)
}

fn strip_known_completion_suffix<'a>(endpoint: &'a str, dialect: &str) -> &'a str {
    let suffix = dialect_paths(dialect).completion;
    if let Some(stripped) = endpoint.strip_suffix(suffix) {
        tracing::warn!(
            endpoint,
            suffix,
            "endpoint includes the dialect's completion path; \
             trimming it — please drop the suffix from your config"
        );
        return stripped;
    }
    endpoint
}

/// Validate a user-supplied endpoint string. Returns `Ok(())` if usable,
/// `Err(message)` with a one-line, user-facing reason otherwise.
///
/// Required: a scheme of `http://` or `https://`. Schemes are explicit so
/// that mixed-environment configs (HTTPS on a non-standard port, HTTP on a
/// public host) don't silently route the wrong way.
pub fn validate_endpoint(endpoint: &str) -> Result<(), String> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        return Err("endpoint is empty".into());
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(format!(
            "endpoint must start with http:// or https:// (got `{trimmed}`)"
        ));
    }
    // After the scheme, there has to be a host.
    let after_scheme = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .unwrap_or("");
    let host = after_scheme.split('/').next().unwrap_or("");
    if host.is_empty() {
        return Err("endpoint is missing a host after the scheme".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_url_appends_dialect_path() {
        let pc = ProviderConfig::anthropic();
        assert_eq!(completion_url(&pc), "https://api.anthropic.com/v1/messages");
        let pc = ProviderConfig::openai();
        assert_eq!(
            completion_url(&pc),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn completion_url_handles_trailing_slash() {
        let pc = ProviderConfig {
            dialect: "openai".into(),
            endpoint: "http://127.0.0.1:1234/".into(),
            version: String::new(),
            auth: None,
        };
        assert_eq!(
            completion_url(&pc),
            "http://127.0.0.1:1234/v1/chat/completions"
        );
    }

    #[test]
    fn completion_url_strips_legacy_suffix() {
        // User wrote the full URL in their TOML before this refactor.
        let pc = ProviderConfig {
            dialect: "openai".into(),
            endpoint: "http://127.0.0.1:1234/v1/chat/completions".into(),
            version: String::new(),
            auth: None,
        };
        assert_eq!(
            completion_url(&pc),
            "http://127.0.0.1:1234/v1/chat/completions"
        );
    }

    #[test]
    fn models_url_uses_models_path() {
        let pc = ProviderConfig::anthropic();
        assert_eq!(models_url(&pc), "https://api.anthropic.com/v1/models");
        let pc = ProviderConfig::openai();
        assert_eq!(models_url(&pc), "https://api.openai.com/v1/models");
    }

    #[test]
    fn models_url_strips_legacy_completion_suffix() {
        let pc = ProviderConfig {
            dialect: "openai".into(),
            endpoint: "http://127.0.0.1:1234/v1/chat/completions".into(),
            version: String::new(),
            auth: None,
        };
        assert_eq!(models_url(&pc), "http://127.0.0.1:1234/v1/models");
    }

    #[test]
    fn validate_accepts_http_and_https() {
        assert!(validate_endpoint("http://127.0.0.1:1234").is_ok());
        assert!(validate_endpoint("https://api.anthropic.com").is_ok());
        assert!(validate_endpoint("http://corp-proxy.example.com:8080").is_ok());
    }

    #[test]
    fn validate_rejects_missing_scheme() {
        let err = validate_endpoint("127.0.0.1:1234").unwrap_err();
        assert!(err.contains("http:// or https://"), "{err}");
    }

    #[test]
    fn validate_rejects_empty() {
        assert!(validate_endpoint("").is_err());
        assert!(validate_endpoint("   ").is_err());
    }

    #[test]
    fn validate_rejects_missing_host() {
        assert!(validate_endpoint("http://").is_err());
        assert!(validate_endpoint("https:///path").is_err());
    }

    // -- AuthScheme ----------------------------------------------------------

    #[test]
    fn auth_scheme_requires_key_matches_intent() {
        assert!(AuthScheme::XApiKey.requires_key());
        assert!(AuthScheme::BearerToken.requires_key());
        assert!(!AuthScheme::None.requires_key());
    }

    #[test]
    fn auth_scheme_default_by_dialect() {
        assert_eq!(
            AuthScheme::default_for_dialect("anthropic"),
            AuthScheme::XApiKey
        );
        assert_eq!(
            AuthScheme::default_for_dialect("openai"),
            AuthScheme::BearerToken
        );
        // Unknown dialects fall back to anthropic — same shape as the rest
        // of the codebase. Better to fail loud at request time than to
        // silently default to "no auth".
        assert_eq!(
            AuthScheme::default_for_dialect("mystery"),
            AuthScheme::XApiKey
        );
    }

    #[test]
    fn provider_resolved_auth_uses_explicit_when_set() {
        let pc = ProviderConfig {
            dialect: "openai".into(),
            endpoint: "http://localhost:1234".into(),
            version: String::new(),
            auth: Some(AuthScheme::None),
        };
        assert_eq!(pc.resolved_auth(), AuthScheme::None);
    }

    #[test]
    fn provider_resolved_auth_falls_back_to_dialect_default() {
        // Legacy config: no `auth` field. resolved_auth derives from dialect.
        let pc = ProviderConfig {
            dialect: "anthropic".into(),
            endpoint: "https://api.anthropic.com".into(),
            version: "2023-06-01".into(),
            auth: None,
        };
        assert_eq!(pc.resolved_auth(), AuthScheme::XApiKey);

        let pc = ProviderConfig {
            dialect: "openai".into(),
            endpoint: "https://api.openai.com".into(),
            version: String::new(),
            auth: None,
        };
        assert_eq!(pc.resolved_auth(), AuthScheme::BearerToken);
    }

    #[test]
    fn provider_config_deserializes_legacy_without_auth_field() {
        // A TOML/JSON config written before AuthScheme existed must still
        // deserialize cleanly and resolve to a sensible default.
        let json = serde_json::json!({
            "dialect": "anthropic",
            "endpoint": "https://api.anthropic.com",
            "version": "2023-06-01",
        });
        let pc: ProviderConfig = serde_json::from_value(json).unwrap();
        assert_eq!(pc.auth, None);
        assert_eq!(pc.resolved_auth(), AuthScheme::XApiKey);
    }

    // -- Presets -------------------------------------------------------------

    #[test]
    fn presets_lists_real_apis_and_a_custom_escape_hatch() {
        let labels: Vec<&str> = presets().iter().map(|p| p.label).collect();
        assert!(labels.contains(&"Anthropic"));
        assert!(labels.contains(&"OpenAI"));
        assert!(labels.iter().any(|l| l.starts_with("Custom")));
        // Local runtimes (LM Studio, Ollama, …) expose multiple API shapes;
        // "the runtime" is not a preset. Pinning the absence so a future
        // change has to make a deliberate choice.
        assert!(!labels.iter().any(|l| l.contains("LM Studio")));
        assert!(!labels.iter().any(|l| l.contains("Ollama")));
    }

    #[test]
    fn cloud_presets_require_auth() {
        for p in presets() {
            if matches!(p.id, "anthropic" | "openai") {
                assert!(p.auth.requires_key(), "{} must require a key", p.label);
            }
        }
    }

    #[test]
    fn every_preset_id_is_a_valid_path_component() {
        // Preset ids become StructFS path components at save time
        // (gate/providers/{id}). PathComponent's identifier rules are
        // stricter than what reads naturally as kebab-case, so this
        // pins every preset against the same validator the runtime uses.
        // `Custom` carries an empty id (it synthesizes a per-account
        // provider named after the account) and is exempt.
        for p in presets() {
            if p.custom {
                continue;
            }
            ox_kernel::PathComponent::try_new(p.id).unwrap_or_else(|e| {
                panic!(
                    "preset {:?} has id {:?} that is not a valid PathComponent: {e}",
                    p.label, p.id
                )
            });
        }
    }
}

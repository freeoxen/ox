//! Provider configuration for LLM API endpoints.
//!
//! `endpoint` is the **base URL** the dialect dispatches against; the request
//! path (`/v1/messages`, `/v1/chat/completions`, `/v1/models`, …) is owned by
//! the dialect, not the user. See [`dialect_paths`].

use serde::{Deserialize, Serialize};

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
}

impl ProviderConfig {
    /// Default configuration for the Anthropic Messages API.
    pub fn anthropic() -> Self {
        Self {
            dialect: "anthropic".to_string(),
            endpoint: "https://api.anthropic.com".to_string(),
            version: "2023-06-01".to_string(),
        }
    }

    /// Default configuration for the OpenAI Chat Completions API.
    pub fn openai() -> Self {
        Self {
            dialect: "openai".to_string(),
            endpoint: "https://api.openai.com".to_string(),
            version: String::new(),
        }
    }
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
}

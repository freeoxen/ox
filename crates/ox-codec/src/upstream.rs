//! Upstream request construction — shared by the native dispatch path and
//! the broker Block. One implementation means the two paths cannot drift:
//! the Block emits the same JSON the native path deserializes into an
//! `HttpRequest`, and the parity suite holds them together.

use ox_kernel::CompletionRequest;
use ox_types::api_key::ApiKey;
use ox_types::provider::{AuthScheme, ProviderConfig, completion_url};
use std::collections::BTreeMap;

/// Build the full upstream request as JSON in `UpstreamRequest` shape:
/// `{"dialect": ..., "request": {method, path, headers, body}}` — the
/// serde form of `structfs_http::types::HttpRequest`, constructed without
/// that crate so the builder stays wasm-clean.
pub fn build_upstream_request_json(
    provider: &ProviderConfig,
    api_key: &ApiKey,
    request: &CompletionRequest,
    upstream_model_id: &str,
) -> serde_json::Value {
    // Rebuild with the upstream model id (the inbound id may be a named
    // role or slash-form). The upstream call always streams — the SSE
    // executor can only consume event streams; the client's stream flag
    // governs only how the gateway shapes its own response.
    let mut rebuilt = CompletionRequest {
        model: upstream_model_id.to_string(),
        stream: true,
        ..request.clone()
    };
    // max_tokens == 0 means the (OpenAI-dialect) client omitted it. The
    // openai codec omits the field for that sentinel, but Anthropic-dialect
    // upstreams require max_tokens, so give the cross-dialect case a cap.
    if rebuilt.max_tokens == 0 && provider.dialect != "openai" {
        rebuilt.max_tokens = 4096;
    }

    // Both arms translate via a passthrough whitelist — a raw struct
    // serialization would splat every extras key into the body, and both
    // upstream APIs reject unknown fields.
    let body: serde_json::Value = match provider.dialect.as_str() {
        "openai" => crate::openai::translate_request(&rebuilt),
        _ => crate::anthropic::translate_request(&rebuilt),
    };

    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    headers.insert("Content-Type".into(), "application/json".into());
    match provider.resolved_auth() {
        AuthScheme::BearerToken => {
            headers.insert(
                "Authorization".into(),
                format!("Bearer {}", api_key.expose()),
            );
        }
        AuthScheme::XApiKey => {
            headers.insert("x-api-key".into(), api_key.expose().to_string());
        }
        AuthScheme::None => {}
    }
    if provider.dialect == "anthropic" && !provider.version.is_empty() {
        headers.insert("anthropic-version".into(), provider.version.clone());
    }

    serde_json::json!({
        "dialect": provider.dialect,
        "request": {
            "method": "POST",
            "path": completion_url(provider),
            "headers": headers,
            "body": body,
        },
    })
}

/// Classify the inbound model string's likely dialect for usage records.
pub fn detect_inbound_dialect(model: &str) -> String {
    let lower = model.to_lowercase();
    if lower.contains("gpt")
        || lower.contains("/o1")
        || lower.starts_with("o1")
        || lower.contains("/o3")
        || lower.starts_with("o3")
    {
        "openai".into()
    } else if lower.contains("claude") {
        "anthropic".into()
    } else {
        "unknown".into()
    }
}

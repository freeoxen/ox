//! Wire-level helpers shared by the native HTTP edge and the codec Block:
//! dialect-shaped error envelopes and status-kind mapping. One
//! implementation so the two paths cannot drift.

/// Anthropic-shaped error envelope body for an HTTP status.
pub fn anthropic_error_body(status: u16, message: &str) -> serde_json::Value {
    let kind = match status {
        400 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        _ => "api_error",
    };
    serde_json::json!({
        "type": "error",
        "error": { "type": kind, "message": message }
    })
}

/// OpenAI-shaped error envelope body for an HTTP status.
pub fn openai_error_body(status: u16, message: &str, code: Option<&str>) -> serde_json::Value {
    let kind = match status {
        400 | 401 | 403 | 404 => "invalid_request_error",
        429 => "rate_limit_exceeded",
        _ => "api_error",
    };
    serde_json::json!({
        "error": { "message": message, "type": kind, "code": code }
    })
}

/// Dialect dispatch for the Block's wire mode.
pub fn error_body(dialect: &str, status: u16, message: &str) -> serde_json::Value {
    match dialect {
        "openai" => openai_error_body(status, message, None),
        _ => anthropic_error_body(status, message),
    }
}

//! HTTP error envelopes shaped per dialect (Anthropic / OpenAI).
//!
//! The two functions return `(StatusCode, axum::Json<Value>)` so handlers
//! can `.into_response()` them. Map the StoreError / CodecError /
//! resolution failure messages into the appropriate envelope before
//! returning.

use axum::http::StatusCode;
use serde_json::Value;

pub fn anthropic_error(
    status: StatusCode,
    message: impl Into<String>,
) -> (StatusCode, axum::Json<Value>) {
    let body = ox_codec::wire::anthropic_error_body(status.as_u16(), &message.into());
    (status, axum::Json(body))
}

pub fn openai_error(
    status: StatusCode,
    message: impl Into<String>,
    code: Option<&str>,
) -> (StatusCode, axum::Json<Value>) {
    let body = ox_codec::wire::openai_error_body(status.as_u16(), &message.into(), code);
    (status, axum::Json(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_400_shape() {
        let (s, body) = anthropic_error(StatusCode::BAD_REQUEST, "bad");
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(body.0["type"], "error");
        assert_eq!(body.0["error"]["type"], "invalid_request_error");
        assert_eq!(body.0["error"]["message"], "bad");
    }

    #[test]
    fn anthropic_401_shape() {
        let (s, body) = anthropic_error(StatusCode::UNAUTHORIZED, "no key");
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert_eq!(body.0["error"]["type"], "authentication_error");
    }

    #[test]
    fn anthropic_500_shape() {
        let (s, body) = anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, "boom");
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.0["error"]["type"], "api_error");
    }

    #[test]
    fn openai_400_shape() {
        let (s, body) = openai_error(StatusCode::BAD_REQUEST, "bad", None);
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(body.0["error"]["type"], "invalid_request_error");
        assert_eq!(body.0["error"]["message"], "bad");
    }

    #[test]
    fn openai_401_shape() {
        let (s, body) = openai_error(StatusCode::UNAUTHORIZED, "no key", None);
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert_eq!(body.0["error"]["type"], "invalid_request_error");
    }

    #[test]
    fn openai_429_shape() {
        let (s, body) = openai_error(StatusCode::TOO_MANY_REQUESTS, "slow down", Some("rate_limited"));
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body.0["error"]["type"], "rate_limit_exceeded");
        assert_eq!(body.0["error"]["code"], "rate_limited");
    }
}

use ox_gate::codec::{anthropic as anthropic_codec, openai as openai_codec};
use ox_kernel::{CompletionRequest, StreamEvent};

/// Build the request URL, headers, and body for a given provider.
pub fn build_request(
    provider: &str,
    api_key: &str,
    request: &CompletionRequest,
) -> Result<(String, Vec<(String, String)>, String), String> {
    match provider {
        "openai" => {
            let body = openai_codec::translate_request(request);
            let body_str = serde_json::to_string(&body).map_err(|e| e.to_string())?;
            Ok((
                "https://api.openai.com/v1/chat/completions".to_string(),
                vec![
                    ("Content-Type".into(), "application/json".into()),
                    ("Authorization".into(), format!("Bearer {api_key}")),
                ],
                body_str,
            ))
        }
        _ => {
            let body_str = serde_json::to_string(request).map_err(|e| e.to_string())?;
            Ok((
                "https://api.anthropic.com/v1/messages".to_string(),
                vec![
                    ("Content-Type".into(), "application/json".into()),
                    ("x-api-key".into(), api_key.to_string()),
                    ("anthropic-version".into(), "2023-06-01".into()),
                ],
                body_str,
            ))
        }
    }
}

/// Parse an SSE response body using the appropriate provider codec.
pub fn parse_response(provider: &str, body: &str) -> Vec<StreamEvent> {
    match provider {
        "openai" => {
            let (events, _usage) = openai_codec::parse_sse_events(body);
            events
        }
        _ => anthropic_codec::parse_sse_events(body),
    }
}

/// Create a send function that dispatches HTTP requests via reqwest::blocking.
pub fn make_send_fn(
    provider: String,
    api_key: String,
) -> impl Fn(&CompletionRequest) -> Result<Vec<StreamEvent>, String> + Send + Sync {
    let client = reqwest::blocking::Client::new();
    move |request: &CompletionRequest| {
        let (url, headers, body) = build_request(&provider, &api_key, request)?;
        let mut req = client.post(&url).body(body);
        for (key, value) in &headers {
            req = req.header(key, value);
        }
        let resp = req.send().map_err(|e| format!("HTTP request failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            return Err(format!("HTTP {status}: {text}"));
        }
        let text = resp
            .text()
            .map_err(|e| format!("failed to read response: {e}"))?;
        Ok(parse_response(&provider, &text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_kernel::CompletionRequest;

    fn sample_request() -> CompletionRequest {
        CompletionRequest {
            model: "claude-sonnet-4-20250514".into(),
            max_tokens: 1024,
            system: "You are helpful.".into(),
            messages: vec![serde_json::json!({"role": "user", "content": "Hi"})],
            tools: vec![],
            stream: true,
        }
    }

    #[test]
    fn build_anthropic_request_url_and_headers() {
        let (url, headers, _body) =
            build_request("anthropic", "sk-test", &sample_request()).unwrap();
        assert_eq!(url, "https://api.anthropic.com/v1/messages");
        assert!(headers
            .iter()
            .any(|(k, v)| k == "x-api-key" && v == "sk-test"));
        assert!(headers
            .iter()
            .any(|(k, v)| k == "anthropic-version" && v == "2023-06-01"));
    }

    #[test]
    fn build_openai_request_url_and_headers() {
        let (url, headers, _body) =
            build_request("openai", "sk-oai", &sample_request()).unwrap();
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
        assert!(headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer sk-oai"));
    }

    #[test]
    fn parse_anthropic_response() {
        let body = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\ndata: {\"type\":\"message_stop\"}\n";
        let events = parse_response("anthropic", body);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], StreamEvent::TextDelta(t) if t == "Hello"));
        assert!(matches!(&events[1], StreamEvent::MessageStop));
    }

    #[test]
    fn parse_openai_response() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\ndata: [DONE]\n";
        let events = parse_response("openai", body);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], StreamEvent::TextDelta(t) if t == "Hi"));
        assert!(matches!(&events[1], StreamEvent::MessageStop));
    }
}

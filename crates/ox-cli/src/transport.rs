use ox_gate::codec::{anthropic as anthropic_codec, openai as openai_codec};
use ox_kernel::{CompletionRequest, StreamEvent};
use std::collections::HashSet;
use std::io::BufRead;

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
/// Used by CompletionTool for sub-completions (non-streaming).
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

// ---------------------------------------------------------------------------
// Incremental SSE parser — line-by-line for streaming
// ---------------------------------------------------------------------------

/// Stateful SSE line parser. Feed one line at a time, get events back.
pub struct SseParser {
    provider: String,
    openai_tool_started: HashSet<u64>,
}

impl SseParser {
    pub fn new(provider: &str) -> Self {
        Self {
            provider: provider.to_string(),
            openai_tool_started: HashSet::new(),
        }
    }

    /// Parse a single SSE line into zero or more stream events.
    pub fn feed(&mut self, line: &str) -> Vec<StreamEvent> {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data: ") else {
            return vec![];
        };
        if data == "[DONE]" {
            return vec![StreamEvent::MessageStop];
        }
        let Ok(json) = serde_json::from_str::<serde_json::Value>(data) else {
            return vec![];
        };
        match self.provider.as_str() {
            "openai" => self.parse_openai(&json),
            _ => Self::parse_anthropic(&json),
        }
    }

    fn parse_anthropic(json: &serde_json::Value) -> Vec<StreamEvent> {
        let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match event_type {
            "content_block_start" => {
                if let Some(cb) = json.get("content_block") {
                    if cb.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        let id = cb
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = cb
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        return vec![StreamEvent::ToolUseStart { id, name }];
                    }
                }
                vec![]
            }
            "content_block_delta" => {
                if let Some(delta) = json.get("delta") {
                    match delta.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                        "text_delta" => {
                            if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                return vec![StreamEvent::TextDelta(text.to_string())];
                            }
                        }
                        "input_json_delta" => {
                            if let Some(p) = delta.get("partial_json").and_then(|t| t.as_str()) {
                                return vec![StreamEvent::ToolUseInputDelta(p.to_string())];
                            }
                        }
                        _ => {}
                    }
                }
                vec![]
            }
            "message_stop" => vec![StreamEvent::MessageStop],
            "error" => {
                let msg = json
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error");
                vec![StreamEvent::Error(msg.to_string())]
            }
            _ => vec![],
        }
    }

    fn parse_openai(&mut self, json: &serde_json::Value) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        let Some(choices) = json.get("choices").and_then(|c| c.as_array()) else {
            return events;
        };
        for choice in choices {
            let Some(delta) = choice.get("delta") else {
                continue;
            };
            if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                if !content.is_empty() {
                    events.push(StreamEvent::TextDelta(content.to_string()));
                }
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                for tc in tool_calls {
                    let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                    let function = tc.get("function");
                    if self.openai_tool_started.insert(index) {
                        let id =
                            tc.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let name = function
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        events.push(StreamEvent::ToolUseStart { id, name });
                    }
                    if let Some(args) = function
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                    {
                        if !args.is_empty() {
                            events.push(StreamEvent::ToolUseInputDelta(args.to_string()));
                        }
                    }
                }
            }
        }
        events
    }
}

/// Stream an HTTP completion request, emitting events in real-time via callback.
/// Returns all events for kernel processing.
pub fn streaming_fetch(
    client: &reqwest::blocking::Client,
    provider: &str,
    api_key: &str,
    request: &CompletionRequest,
    on_event: &dyn Fn(&StreamEvent),
) -> Result<Vec<StreamEvent>, String> {
    let (url, headers, body) = build_request(provider, api_key, request)?;
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

    let reader = std::io::BufReader::new(resp);
    let mut parser = SseParser::new(provider);
    let mut all_events = Vec::new();

    for line in reader.lines() {
        let line = line.map_err(|e| format!("read error: {e}"))?;
        for event in parser.feed(&line) {
            on_event(&event);
            all_events.push(event);
        }
    }

    Ok(all_events)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // --- Incremental parser tests ---

    #[test]
    fn sse_parser_anthropic_text_delta() {
        let mut parser = SseParser::new("anthropic");
        let events = parser.feed("data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::TextDelta(t) if t == "Hi"));
    }

    #[test]
    fn sse_parser_anthropic_tool_start() {
        let mut parser = SseParser::new("anthropic");
        let events = parser.feed("data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"read_file\"}}");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::ToolUseStart { id, name } if id == "t1" && name == "read_file"));
    }

    #[test]
    fn sse_parser_anthropic_ignores_non_data() {
        let mut parser = SseParser::new("anthropic");
        assert!(parser.feed("event: ping").is_empty());
        assert!(parser.feed("").is_empty());
        assert!(parser.feed(": comment").is_empty());
    }

    #[test]
    fn sse_parser_anthropic_done() {
        let mut parser = SseParser::new("anthropic");
        let events = parser.feed("data: [DONE]");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::MessageStop));
    }

    #[test]
    fn sse_parser_openai_text_delta() {
        let mut parser = SseParser::new("openai");
        let events = parser.feed("data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::TextDelta(t) if t == "Hello"));
    }

    #[test]
    fn sse_parser_openai_tool_call_tracking() {
        let mut parser = SseParser::new("openai");
        // First chunk — starts tool
        let e1 = parser.feed("data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"tc1\",\"function\":{\"name\":\"shell\",\"arguments\":\"\"}}]}}]}");
        assert_eq!(e1.len(), 1);
        assert!(matches!(&e1[0], StreamEvent::ToolUseStart { name, .. } if name == "shell"));

        // Second chunk — same index, no duplicate start
        let e2 = parser.feed("data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"cmd\\\"\"}}]}}]}");
        assert_eq!(e2.len(), 1);
        assert!(matches!(&e2[0], StreamEvent::ToolUseInputDelta(_)));
    }

    #[test]
    fn streaming_fetch_callback_receives_events() {
        // This test verifies the callback wiring, not actual HTTP
        let mut parser = SseParser::new("anthropic");
        let mut received = Vec::new();

        let lines = [
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"A\"}}",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"B\"}}",
            "data: {\"type\":\"message_stop\"}",
        ];

        for line in &lines {
            for event in parser.feed(line) {
                received.push(event);
            }
        }

        assert_eq!(received.len(), 3);
        assert!(matches!(&received[0], StreamEvent::TextDelta(t) if t == "A"));
        assert!(matches!(&received[1], StreamEvent::TextDelta(t) if t == "B"));
        assert!(matches!(&received[2], StreamEvent::MessageStop));
    }
}

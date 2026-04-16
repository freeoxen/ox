use ox_gate::ProviderConfig;
#[cfg(test)]
use ox_gate::codec::anthropic as anthropic_codec;
use ox_gate::codec::{UsageInfo, openai as openai_codec};
use ox_kernel::{CompletionRequest, StreamEvent};
use std::collections::HashSet;
use std::io::BufRead;

/// URL, headers, body.
pub type RequestParts = (String, Vec<(String, String)>, String);

/// Build the request URL, headers, and body from a ProviderConfig.
pub fn build_request(
    config: &ProviderConfig,
    api_key: &str,
    request: &CompletionRequest,
) -> Result<RequestParts, String> {
    match config.dialect.as_str() {
        "openai" => {
            let body = openai_codec::translate_request(request);
            let body_str = serde_json::to_string(&body).map_err(|e| e.to_string())?;
            Ok((
                config.endpoint.clone(),
                vec![
                    ("Content-Type".into(), "application/json".into()),
                    ("Authorization".into(), format!("Bearer {api_key}")),
                ],
                body_str,
            ))
        }
        _ => {
            let body_str = serde_json::to_string(request).map_err(|e| e.to_string())?;
            let mut headers = vec![
                ("Content-Type".into(), "application/json".into()),
                ("x-api-key".into(), api_key.to_string()),
            ];
            if !config.version.is_empty() {
                headers.push(("anthropic-version".into(), config.version.clone()));
            }
            Ok((config.endpoint.clone(), headers, body_str))
        }
    }
}

/// Parse an SSE response body using the appropriate dialect codec.
#[cfg(test)]
fn parse_response(dialect: &str, body: &str) -> Vec<StreamEvent> {
    match dialect {
        "openai" => {
            let (events, _usage) = openai_codec::parse_sse_events(body);
            events
        }
        _ => anthropic_codec::parse_sse_events(body),
    }
}

// ---------------------------------------------------------------------------
// Incremental SSE parser — line-by-line for streaming, with usage tracking
// ---------------------------------------------------------------------------

/// Stateful SSE line parser. Feed one line at a time, get events back.
pub struct SseParser {
    dialect: String,
    openai_tool_started: HashSet<u64>,
    pub usage: UsageInfo,
}

impl SseParser {
    pub fn new(dialect: &str) -> Self {
        Self {
            dialect: dialect.to_string(),
            openai_tool_started: HashSet::new(),
            usage: UsageInfo::default(),
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
        match self.dialect.as_str() {
            "openai" => self.parse_openai(&json),
            _ => self.parse_anthropic(&json),
        }
    }

    fn parse_anthropic(&mut self, json: &serde_json::Value) -> Vec<StreamEvent> {
        let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match event_type {
            "message_start" => {
                if let Some(usage) = json.get("message").and_then(|m| m.get("usage")) {
                    if let Some(it) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                        self.usage.input_tokens = it as u32;
                    }
                    if let Some(ct) = usage
                        .get("cache_creation_input_tokens")
                        .and_then(|v| v.as_u64())
                    {
                        self.usage.cache_creation_input_tokens = ct as u32;
                    }
                    if let Some(cr) = usage
                        .get("cache_read_input_tokens")
                        .and_then(|v| v.as_u64())
                    {
                        self.usage.cache_read_input_tokens = cr as u32;
                    }
                }
                vec![]
            }
            "message_delta" => {
                if let Some(usage) = json.get("usage") {
                    if let Some(ot) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                        self.usage.output_tokens = ot as u32;
                    }
                }
                vec![]
            }
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
        // Track usage if present
        if let Some(usage_obj) = json.get("usage") {
            if let Some(pt) = usage_obj.get("prompt_tokens").and_then(|v| v.as_u64()) {
                self.usage.input_tokens = pt as u32;
            }
            if let Some(ct) = usage_obj.get("completion_tokens").and_then(|v| v.as_u64()) {
                self.usage.output_tokens = ct as u32;
            }
        }

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
                        let id = tc
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
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

/// Stream an HTTP completion request with retry on transient errors.
/// Emits events in real-time via callback. Returns all events + usage for kernel processing.
pub fn streaming_fetch(
    client: &reqwest::blocking::Client,
    config: &ProviderConfig,
    api_key: &str,
    request: &CompletionRequest,
    on_event: &dyn Fn(&StreamEvent),
) -> Result<(Vec<StreamEvent>, UsageInfo), String> {
    let (url, headers, body) = build_request(config, api_key, request)?;

    tracing::debug!(
        url = %url,
        dialect = %config.dialect,
        model = %request.model,
        messages = request.messages.len(),
        tools = request.tools.len(),
        "streaming fetch start"
    );

    let mut last_err = String::new();
    for attempt in 0..3u32 {
        if attempt > 0 {
            tracing::warn!(attempt, last_err = %last_err, "retrying streaming fetch");
            std::thread::sleep(std::time::Duration::from_secs(2u64.pow(attempt)));
        }

        let mut req = client.post(&url).body(body.clone());
        for (key, value) in &headers {
            req = req.header(key, value);
        }

        let resp = match req.send() {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("network error: {e}");
                continue;
            }
        };

        let status = resp.status();
        if status.as_u16() == 429 || status.is_server_error() {
            last_err = format!("HTTP {status}");
            tracing::warn!(status = %status, "transient HTTP error");
            continue;
        }
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            tracing::error!(status = %status, body = %text, "HTTP request failed");
            return Err(format!("HTTP {status}: {text}"));
        }

        // Success — stream line-by-line
        let reader = std::io::BufReader::new(resp);
        let mut parser = SseParser::new(&config.dialect);
        let mut all_events = Vec::new();

        for line in reader.lines() {
            let line = line.map_err(|e| format!("read error: {e}"))?;
            for event in parser.feed(&line) {
                on_event(&event);
                all_events.push(event);
            }
        }

        tracing::debug!(
            events = all_events.len(),
            input_tokens = parser.usage.input_tokens,
            output_tokens = parser.usage.output_tokens,
            "streaming fetch complete"
        );
        return Ok((all_events, parser.usage));
    }

    tracing::error!(last_err = %last_err, "streaming fetch exhausted retries");
    Err(format!("{last_err} (after 3 attempts)"))
}

/// Test an API connection with a minimal completion request (async).
/// Returns (dialect, elapsed_ms).
pub async fn test_connection_async(
    config: &ProviderConfig,
    api_key: &str,
) -> Result<(String, u128), String> {
    let client = reqwest::Client::new();
    let dialect = config.dialect.clone();

    let model = match dialect.as_str() {
        "openai" => "gpt-4o-mini",
        _ => "claude-haiku-4-5-20251001",
    };

    let request = ox_kernel::CompletionRequest {
        model: model.to_string(),
        max_tokens: 1,
        system: String::new(),
        messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
        tools: vec![],
        stream: true,
    };

    let (url, headers, body) = build_request(config, api_key, &request)?;

    let start = std::time::Instant::now();
    let mut req = client.post(&url).body(body);
    for (k, v) in &headers {
        req = req.header(k, v);
    }

    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {status}: {text}"));
    }
    // Consume body to complete the request
    let _ = resp.text().await;
    let elapsed = start.elapsed().as_millis();

    Ok((dialect, elapsed))
}

/// Fetch available models from a provider's API (async).
///
/// Derives the models endpoint from the completion endpoint:
/// - "https://api.anthropic.com/v1/messages" → "https://api.anthropic.com/v1/models"
/// - "https://api.openai.com/v1/chat/completions" → "https://api.openai.com/v1/models"
pub async fn fetch_model_catalog_async(
    config: &ProviderConfig,
    api_key: &str,
) -> Result<Vec<ox_kernel::ModelInfo>, String> {
    let client = reqwest::Client::new();

    let base = config
        .endpoint
        .rfind("/v1/")
        .map(|i| &config.endpoint[..i + 4])
        .unwrap_or(&config.endpoint);
    let models_base = format!("{base}models");

    let mut all_models: Vec<ox_kernel::ModelInfo> = Vec::new();
    let mut after_id: Option<String> = None;

    loop {
        let url = match (&after_id, config.dialect.as_str()) {
            (Some(cursor), "anthropic") => {
                format!("{models_base}?limit=1000&after_id={cursor}")
            }
            (None, "anthropic") => format!("{models_base}?limit=1000"),
            _ => models_base.clone(),
        };

        let mut req = client.get(&url);
        match config.dialect.as_str() {
            "openai" => {
                req = req.header("Authorization", format!("Bearer {api_key}"));
            }
            _ => {
                req = req.header("x-api-key", api_key);
                if !config.version.is_empty() {
                    req = req.header("anthropic-version", &config.version);
                }
            }
        }

        let resp = req.send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("HTTP {status}: {body}"));
        }

        let body = resp.text().await.map_err(|e| e.to_string())?;
        let page: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;

        if let Some(data) = page.get("data").and_then(|d| d.as_array()) {
            for entry in data {
                let id = entry
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let display_name = entry
                    .get("display_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&id)
                    .to_string();

                // OpenAI returns everything — filter to chat models
                if config.dialect == "openai"
                    && !(id.contains("gpt")
                        || id.contains("o1")
                        || id.contains("o3")
                        || id.contains("o4"))
                {
                    continue;
                }

                all_models.push(ox_kernel::ModelInfo { id, display_name });
            }
        }

        // Only Anthropic paginates with has_more/last_id
        let has_more = page
            .get("has_more")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !has_more {
            break;
        }
        after_id = page
            .get("last_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
    }

    all_models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(all_models)
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
    fn build_anthropic_request_from_config() {
        let config = ProviderConfig::anthropic();
        let (url, headers, _body) = build_request(&config, "sk-test", &sample_request()).unwrap();
        assert_eq!(url, "https://api.anthropic.com/v1/messages");
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "x-api-key" && v == "sk-test")
        );
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "anthropic-version" && v == "2023-06-01")
        );
    }

    #[test]
    fn build_openai_request_from_config() {
        let config = ProviderConfig::openai();
        let (url, headers, _body) = build_request(&config, "sk-oai", &sample_request()).unwrap();
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "Authorization" && v == "Bearer sk-oai")
        );
    }

    #[test]
    fn build_custom_endpoint() {
        let config = ProviderConfig {
            dialect: "openai".into(),
            endpoint: "http://localhost:8080/v1/chat/completions".into(),
            version: String::new(),
        };
        let (url, _, _) = build_request(&config, "key", &sample_request()).unwrap();
        assert_eq!(url, "http://localhost:8080/v1/chat/completions");
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
        assert!(
            matches!(&events[0], StreamEvent::ToolUseStart { id, name } if id == "t1" && name == "read_file")
        );
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
    fn sse_parser_anthropic_usage_tracking() {
        let mut parser = SseParser::new("anthropic");
        parser.feed(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":150}}}",
        );
        assert_eq!(parser.usage.input_tokens, 150);
        parser.feed("data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":42}}");
        assert_eq!(parser.usage.output_tokens, 42);
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
        let e1 = parser.feed("data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"tc1\",\"function\":{\"name\":\"shell\",\"arguments\":\"\"}}]}}]}");
        assert_eq!(e1.len(), 1);
        assert!(matches!(&e1[0], StreamEvent::ToolUseStart { name, .. } if name == "shell"));

        let e2 = parser.feed("data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"cmd\\\"\"}}]}}]}");
        assert_eq!(e2.len(), 1);
        assert!(matches!(&e2[0], StreamEvent::ToolUseInputDelta(_)));
    }

    #[test]
    fn sse_parser_openai_usage_tracking() {
        let mut parser = SseParser::new("openai");
        parser.feed("data: {\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":50}}");
        assert_eq!(parser.usage.input_tokens, 100);
        assert_eq!(parser.usage.output_tokens, 50);
    }
}

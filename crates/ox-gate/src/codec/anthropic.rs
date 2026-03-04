//! Anthropic SSE parsing and usage extraction.

use ox_kernel::StreamEvent;

use super::UsageInfo;

/// Parse an Anthropic SSE response body into a sequence of [`StreamEvent`]s.
pub fn parse_sse_events(body: &str) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    for line in body.lines() {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        if data == "[DONE]" {
            events.push(StreamEvent::MessageStop);
            continue;
        }
        let Ok(json) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match event_type {
            "content_block_start" => {
                if let Some(cb) = json.get("content_block") {
                    let cb_type = cb.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if cb_type == "tool_use" {
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
                        events.push(StreamEvent::ToolUseStart { id, name });
                    }
                }
            }
            "content_block_delta" => {
                if let Some(delta) = json.get("delta") {
                    let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match delta_type {
                        "text_delta" => {
                            if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                events.push(StreamEvent::TextDelta(text.to_string()));
                            }
                        }
                        "input_json_delta" => {
                            if let Some(partial) =
                                delta.get("partial_json").and_then(|t| t.as_str())
                            {
                                events.push(StreamEvent::ToolUseInputDelta(partial.to_string()));
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_stop" => {
                events.push(StreamEvent::MessageStop);
            }
            "error" => {
                let msg = json
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error");
                events.push(StreamEvent::Error(msg.to_string()));
            }
            _ => {
                // ping, message_start, content_block_stop, message_delta — ignore
            }
        }
    }

    events
}

/// Extract token usage from an Anthropic SSE response body.
///
/// Scans for `message_start` (input_tokens) and `message_delta` (output_tokens)
/// events in the SSE stream.
pub fn extract_usage(body: &str) -> UsageInfo {
    let mut info = UsageInfo::default();

    for line in body.lines() {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match event_type {
            "message_start" => {
                if let Some(usage) = json.get("message").and_then(|m| m.get("usage")) {
                    if let Some(it) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                        info.input_tokens = it as u32;
                    }
                }
            }
            "message_delta" => {
                if let Some(usage) = json.get("usage") {
                    if let Some(ot) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                        info.output_tokens = ot as u32;
                    }
                }
            }
            _ => {}
        }
    }

    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_text_delta() {
        let body = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n";
        let events = parse_sse_events(body);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::TextDelta(t) if t == "Hello"));
    }

    #[test]
    fn parse_tool_use_start() {
        let body = "data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"get_weather\"}}\n";
        let events = parse_sse_events(body);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], StreamEvent::ToolUseStart { id, name } if id == "t1" && name == "get_weather")
        );
    }

    #[test]
    fn parse_tool_input_delta() {
        let body = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"loc\\\"\"}}\n";
        let events = parse_sse_events(body);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::ToolUseInputDelta(s) if s == "{\"loc\""));
    }

    #[test]
    fn parse_message_stop() {
        let body = "data: {\"type\":\"message_stop\"}\n";
        let events = parse_sse_events(body);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::MessageStop));
    }

    #[test]
    fn parse_done_marker() {
        let body = "data: [DONE]\n";
        let events = parse_sse_events(body);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::MessageStop));
    }

    #[test]
    fn parse_error_event() {
        let body = "data: {\"type\":\"error\",\"error\":{\"message\":\"rate limit exceeded\"}}\n";
        let events = parse_sse_events(body);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Error(msg) if msg == "rate limit exceeded"));
    }

    #[test]
    fn parse_ignores_non_data_lines() {
        let body = "event: ping\ndata: {\"type\":\"message_start\",\"message\":{}}\n\ndata: {\"type\":\"message_stop\"}\n";
        let events = parse_sse_events(body);
        // message_start is ignored, message_stop is captured
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::MessageStop));
    }

    #[test]
    fn parse_mixed_events() {
        let body = "\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10}}}\n\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\
data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"echo\"}}\n\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\
data: {\"type\":\"message_stop\"}\n";
        let events = parse_sse_events(body);
        assert_eq!(events.len(), 4);
        assert!(matches!(&events[0], StreamEvent::TextDelta(t) if t == "Hi"));
        assert!(matches!(&events[1], StreamEvent::ToolUseStart { .. }));
        assert!(matches!(&events[2], StreamEvent::ToolUseInputDelta(s) if s == "{}"));
        assert!(matches!(&events[3], StreamEvent::MessageStop));
    }

    #[test]
    fn extract_usage_from_sse() {
        let body = "\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":150}}}\n\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\
data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":42}}\n\
data: {\"type\":\"message_stop\"}\n";
        let usage = extract_usage(body);
        assert_eq!(usage.input_tokens, 150);
        assert_eq!(usage.output_tokens, 42);
    }

    #[test]
    fn extract_usage_empty_body() {
        let usage = extract_usage("");
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
    }

    #[test]
    fn extract_usage_no_usage_events() {
        let body = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n";
        let usage = extract_usage(body);
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
    }
}

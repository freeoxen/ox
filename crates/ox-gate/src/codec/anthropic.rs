//! Anthropic SSE parsing and usage extraction.

use ox_kernel::{CompletionRequest, StreamEvent, ToolSchema};

use super::{CodecError, UsageInfo};

/// Decode an Anthropic Messages API request body into a [`CompletionRequest`].
///
/// `system` may be a plain string or an array of `{type:"text", text:"..."}` blocks;
/// the latter are joined with `\n\n` to produce a flat string.
pub fn decode_request(body: &serde_json::Value) -> Result<CompletionRequest, CodecError> {
    let obj = body.as_object().ok_or_else(|| {
        CodecError::InvalidShape("body must be a JSON object".into())
    })?;

    let model = obj.get("model").and_then(|v| v.as_str())
        .ok_or(CodecError::MissingField("model"))?
        .to_string();

    let max_tokens = obj.get("max_tokens").and_then(|v| v.as_u64())
        .ok_or(CodecError::MissingField("max_tokens"))? as u32;

    let system = match obj.get("system") {
        None => String::new(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => arr.iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => return Err(CodecError::InvalidShape(
            "system must be string or array of text blocks".into()
        )),
    };

    let messages = obj.get("messages").and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let tools: Vec<ToolSchema> = obj.get("tools").and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let name = t.get("name")?.as_str()?.to_string();
                    let description = t.get("description")
                        .and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let input_schema = t.get("input_schema")
                        .cloned().unwrap_or(serde_json::Value::Null);
                    Some(ToolSchema { name, description, input_schema })
                })
                .collect()
        })
        .unwrap_or_default();

    let stream = obj.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    Ok(CompletionRequest { model, max_tokens, system, messages, tools, stream })
}

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
                                events.push(StreamEvent::TextDelta { text: text.to_string() });
                            }
                        }
                        "input_json_delta" => {
                            if let Some(partial) =
                                delta.get("partial_json").and_then(|t| t.as_str())
                            {
                                events.push(StreamEvent::ToolUseInputDelta {
                                    delta: partial.to_string(),
                                });
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
                events.push(StreamEvent::Error { message: msg.to_string() });
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
                    if let Some(ct) = usage
                        .get("cache_creation_input_tokens")
                        .and_then(|v| v.as_u64())
                    {
                        info.cache_creation_input_tokens = ct as u32;
                    }
                    if let Some(cr) = usage
                        .get("cache_read_input_tokens")
                        .and_then(|v| v.as_u64())
                    {
                        info.cache_read_input_tokens = cr as u32;
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

/// Encode a buffered slice of [`StreamEvent`]s into an Anthropic Messages response shape.
///
/// Used when the client sends `stream: false`; the gateway collects all events and calls
/// this function to produce a single JSON object in place of an SSE stream.
pub fn encode_response(events: &[StreamEvent]) -> serde_json::Value {
    let mut content_blocks: Vec<serde_json::Value> = Vec::new();
    let mut current_text = String::new();
    let mut current_tool: Option<(String, String, String)> = None; // (id, name, input_json)
    let mut input_tokens = 0u32;
    let mut cache_creation = 0u32;
    let mut cache_read = 0u32;
    let mut output_tokens = 0u32;

    fn flush_text(blocks: &mut Vec<serde_json::Value>, text: &mut String) {
        if !text.is_empty() {
            blocks.push(serde_json::json!({ "type": "text", "text": text.clone() }));
            text.clear();
        }
    }
    fn flush_tool(blocks: &mut Vec<serde_json::Value>, tool: &mut Option<(String, String, String)>) {
        if let Some((id, name, input_json)) = tool.take() {
            let input = serde_json::from_str::<serde_json::Value>(&input_json)
                .unwrap_or(serde_json::Value::Object(Default::default()));
            blocks.push(serde_json::json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            }));
        }
    }

    for ev in events {
        match ev {
            StreamEvent::TextDelta { text } => {
                flush_tool(&mut content_blocks, &mut current_tool);
                current_text.push_str(text);
            }
            StreamEvent::ToolUseStart { id, name } => {
                flush_text(&mut content_blocks, &mut current_text);
                flush_tool(&mut content_blocks, &mut current_tool);
                current_tool = Some((id.clone(), name.clone(), String::new()));
            }
            StreamEvent::ToolUseInputDelta { delta } => {
                if let Some((_, _, ref mut input_json)) = current_tool {
                    input_json.push_str(delta);
                }
            }
            StreamEvent::InputUsage { input_tokens: it, cache_creation: cc, cache_read: cr } => {
                input_tokens = *it;
                cache_creation = *cc;
                cache_read = *cr;
            }
            StreamEvent::OutputUsage { output_tokens: ot } => {
                output_tokens = *ot;
            }
            StreamEvent::MessageStop | StreamEvent::Error { .. } => {}
        }
    }
    flush_text(&mut content_blocks, &mut current_text);
    flush_tool(&mut content_blocks, &mut current_tool);

    serde_json::json!({
        "type": "message",
        "role": "assistant",
        "model": "",
        "content": content_blocks,
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": input_tokens,
            "cache_creation_input_tokens": cache_creation,
            "cache_read_input_tokens": cache_read,
            "output_tokens": output_tokens,
        }
    })
}

#[cfg(test)]
mod encode_response_tests {
    use super::*;

    #[test]
    fn encode_text_only_response() {
        let events = vec![
            StreamEvent::InputUsage { input_tokens: 10, cache_creation: 0, cache_read: 0 },
            StreamEvent::TextDelta { text: "Hello".into() },
            StreamEvent::TextDelta { text: " world".into() },
            StreamEvent::OutputUsage { output_tokens: 2 },
            StreamEvent::MessageStop,
        ];
        let resp = encode_response(&events);
        assert_eq!(resp["type"], "message");
        assert_eq!(resp["role"], "assistant");
        assert_eq!(resp["content"][0]["type"], "text");
        assert_eq!(resp["content"][0]["text"], "Hello world");
        assert_eq!(resp["usage"]["input_tokens"], 10);
        assert_eq!(resp["usage"]["output_tokens"], 2);
    }

    #[test]
    fn encode_response_with_tool_use() {
        let events = vec![
            StreamEvent::ToolUseStart { id: "t1".into(), name: "read_file".into() },
            StreamEvent::ToolUseInputDelta { delta: r#"{"path":"#.into() },
            StreamEvent::ToolUseInputDelta { delta: r#""/etc/hosts"}"#.into() },
            StreamEvent::OutputUsage { output_tokens: 5 },
            StreamEvent::MessageStop,
        ];
        let resp = encode_response(&events);
        let blocks = resp["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_use");
        assert_eq!(blocks[0]["id"], "t1");
        assert_eq!(blocks[0]["name"], "read_file");
        assert_eq!(blocks[0]["input"]["path"], "/etc/hosts");
    }

    #[test]
    fn encode_mixed_text_and_tool_use_preserves_order() {
        let events = vec![
            StreamEvent::TextDelta { text: "I'll read it.".into() },
            StreamEvent::ToolUseStart { id: "t1".into(), name: "read_file".into() },
            StreamEvent::ToolUseInputDelta { delta: r#"{"p":"/a"}"#.into() },
            StreamEvent::MessageStop,
        ];
        let resp = encode_response(&events);
        let blocks = resp["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "I'll read it.");
        assert_eq!(blocks[1]["type"], "tool_use");
    }

    #[test]
    fn encode_empty_events_yields_empty_content() {
        let resp = encode_response(&[]);
        assert_eq!(resp["content"].as_array().unwrap().len(), 0);
        assert_eq!(resp["usage"]["input_tokens"], 0);
    }
}

#[cfg(test)]
mod decode_tests {
    use super::*;
    use crate::codec::CodecError;

    #[test]
    fn decode_minimal_request() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "hi"}]
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(req.model, "claude-sonnet-4-20250514");
        assert_eq!(req.max_tokens, 1024);
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.system, "");
        assert!(req.tools.is_empty());
        assert!(!req.stream);
    }

    #[test]
    fn decode_with_system_string() {
        let body = serde_json::json!({
            "model": "m",
            "max_tokens": 1,
            "system": "you are helpful",
            "messages": []
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(req.system, "you are helpful");
    }

    #[test]
    fn decode_with_system_block_array() {
        // Anthropic also accepts `system: [{type:"text", text:"..."}]`.
        // Flatten to a single string for the internal CompletionRequest.
        let body = serde_json::json!({
            "model": "m",
            "max_tokens": 1,
            "system": [{"type": "text", "text": "first"}, {"type": "text", "text": "second"}],
            "messages": []
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(req.system, "first\n\nsecond");
    }

    #[test]
    fn missing_model_errors() {
        let body = serde_json::json!({"max_tokens": 1, "messages": []});
        assert_eq!(decode_request(&body).unwrap_err(), CodecError::MissingField("model"));
    }

    #[test]
    fn missing_max_tokens_errors() {
        let body = serde_json::json!({"model": "m", "messages": []});
        assert_eq!(decode_request(&body).unwrap_err(), CodecError::MissingField("max_tokens"));
    }

    #[test]
    fn decode_includes_tools() {
        let body = serde_json::json!({
            "model": "m",
            "max_tokens": 1,
            "messages": [],
            "tools": [{
                "name": "read_file",
                "description": "Read a file",
                "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}}
            }]
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "read_file");
        assert_eq!(req.tools[0].description, "Read a file");
    }

    #[test]
    fn decode_honors_stream_flag() {
        let body = serde_json::json!({
            "model": "m",
            "max_tokens": 1,
            "stream": true,
            "messages": []
        });
        let req = decode_request(&body).unwrap();
        assert!(req.stream);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_text_delta() {
        let body = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n";
        let events = parse_sse_events(body);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::TextDelta { text } if text == "Hello"));
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
        assert!(matches!(&events[0], StreamEvent::ToolUseInputDelta { delta } if delta == "{\"loc\""));
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
        assert!(matches!(&events[0], StreamEvent::Error { message } if message == "rate limit exceeded"));
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
        assert!(matches!(&events[0], StreamEvent::TextDelta { text } if text == "Hi"));
        assert!(matches!(&events[1], StreamEvent::ToolUseStart { .. }));
        assert!(matches!(&events[2], StreamEvent::ToolUseInputDelta { delta } if delta == "{}"));
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

    #[test]
    fn extract_usage_with_cache_tokens() {
        let body = "\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":2095,\"cache_creation_input_tokens\":1800,\"cache_read_input_tokens\":200}}}\n\
data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":50}}\n\
data: {\"type\":\"message_stop\"}\n";
        let usage = extract_usage(body);
        assert_eq!(usage.input_tokens, 2095);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_creation_input_tokens, 1800);
        assert_eq!(usage.cache_read_input_tokens, 200);
    }

    #[test]
    fn extract_usage_without_cache_fields_defaults_to_zero() {
        let body = "\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":100}}}\n\
data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":25}}\n";
        let usage = extract_usage(body);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.cache_creation_input_tokens, 0);
        assert_eq!(usage.cache_read_input_tokens, 0);
    }
}

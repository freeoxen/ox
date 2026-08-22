//! OpenAI request translation and SSE parsing.

use ox_kernel::{CompletionRequest, StreamEvent};

use super::UsageInfo;

/// Translate an Anthropic-format [`CompletionRequest`] into an OpenAI request body.
pub fn translate_request(request: &CompletionRequest) -> serde_json::Value {
    let mut messages = Vec::<serde_json::Value>::new();

    // System prompt → system message
    if !request.system.is_empty() {
        messages.push(serde_json::json!({
            "role": "system",
            "content": request.system,
        }));
    }

    // Translate history messages
    for msg in &request.messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        match role {
            "user" => {
                // Check if content contains tool_result blocks
                if let Some(content_arr) = msg.get("content").and_then(|c| c.as_array()) {
                    for block in content_arr {
                        let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        if block_type == "tool_result" {
                            let tool_call_id = block
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let content = block
                                .get("content")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            messages.push(serde_json::json!({
                                "role": "tool",
                                "tool_call_id": tool_call_id,
                                "content": content,
                            }));
                        } else if block_type == "text" {
                            let text = block
                                .get("text")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            messages.push(serde_json::json!({
                                "role": "user",
                                "content": text,
                            }));
                        }
                    }
                } else {
                    // Plain string content
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": msg.get("content"),
                    }));
                }
            }
            "assistant" => {
                let mut oai_msg = serde_json::json!({"role": "assistant"});
                let mut text_parts = Vec::<String>::new();
                let mut tool_calls = Vec::<serde_json::Value>::new();

                if let Some(content_arr) = msg.get("content").and_then(|c| c.as_array()) {
                    for block in content_arr {
                        let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        match block_type {
                            "text" => {
                                if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                                    text_parts.push(t.to_string());
                                }
                            }
                            "tool_use" => {
                                let id = block
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let name = block
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let input =
                                    block.get("input").cloned().unwrap_or(serde_json::json!({}));
                                let args_str = serde_json::to_string(&input).unwrap_or_default();
                                tool_calls.push(serde_json::json!({
                                    "id": id,
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "arguments": args_str,
                                    }
                                }));
                            }
                            _ => {}
                        }
                    }
                } else if let Some(s) = msg.get("content").and_then(|c| c.as_str()) {
                    text_parts.push(s.to_string());
                }

                if !text_parts.is_empty() {
                    oai_msg["content"] = serde_json::Value::String(text_parts.join(""));
                }
                if !tool_calls.is_empty() {
                    oai_msg["tool_calls"] = serde_json::Value::Array(tool_calls);
                }
                messages.push(oai_msg);
            }
            _ => {
                messages.push(msg.clone());
            }
        }
    }

    // Translate tools
    let tools: Vec<serde_json::Value> = request
        .tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                }
            })
        })
        .collect();

    let mut body = serde_json::json!({
        "model": request.model,
        "messages": messages,
        "stream": true,
        // Without this OpenAI-dialect upstreams never report token usage on
        // streams, which leaves the gateway's ledger and the client-visible
        // usage empty for those providers.
        "stream_options": { "include_usage": true },
    });
    // max_tokens == 0 is the "client omitted it" sentinel from
    // decode_request; forward nothing rather than fabricating a cap the
    // client never asked for.
    if request.max_tokens > 0 {
        body["max_tokens"] = serde_json::json!(request.max_tokens);
    }

    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(tools);
    }

    // Generation params from the canonical extras map. Whitelisted —
    // OpenAI rejects unrecognized top-level arguments, so nothing else
    // from the map reaches the wire.
    const OPENAI_PASSTHROUGH: [&str; 10] = [
        "temperature",
        "top_p",
        "seed",
        "presence_penalty",
        "frequency_penalty",
        "logit_bias",
        "logprobs",
        "top_logprobs",
        "parallel_tool_calls",
        "user",
    ];
    for key in OPENAI_PASSTHROUGH {
        if let Some(v) = request.extra.get(key) {
            body[key] = v.clone();
        }
    }
    if let Some(stop) = request.extra.get("stop_sequences") {
        body["stop"] = stop.clone();
    }
    if let Some(tc) = request.extra.get("tool_choice") {
        if let Some(openai_tc) = tool_choice_to_openai(tc) {
            body["tool_choice"] = openai_tc;
        }
    }

    body
}

/// Parse OpenAI SSE events into [`StreamEvent`]s with usage information.
pub fn parse_sse_events(body: &str) -> (Vec<StreamEvent>, UsageInfo) {
    let mut events = Vec::new();
    let mut usage = UsageInfo::default();
    // Track active tool calls by index
    let mut tool_call_started: std::collections::HashSet<u64> = std::collections::HashSet::new();

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

        // Extract usage if present
        if let Some(usage_obj) = json.get("usage") {
            if let Some(pt) = usage_obj.get("prompt_tokens").and_then(|v| v.as_u64()) {
                usage.input_tokens = pt as u32;
            }
            if let Some(ct) = usage_obj.get("completion_tokens").and_then(|v| v.as_u64()) {
                usage.output_tokens = ct as u32;
            }
            // OpenAI reports cached prompt tokens under prompt_tokens_details.cached_tokens
            if let Some(cached) = usage_obj
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_u64())
            {
                usage.cache_read_input_tokens = cached as u32;
            }
        }

        // Process choices
        if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
            for choice in choices {
                let Some(delta) = choice.get("delta") else {
                    continue;
                };

                // Text content
                if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                    if !content.is_empty() {
                        events.push(StreamEvent::TextDelta { text: content.to_string() });
                    }
                }

                // Tool calls
                if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                    for tc in tool_calls {
                        let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                        let function = tc.get("function");

                        if tool_call_started.insert(index) {
                            // First chunk for this tool call — emit start
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

                        // Arguments delta
                        if let Some(args) = function
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                        {
                            if !args.is_empty() {
                                events.push(StreamEvent::ToolUseInputDelta {
                                    delta: args.to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    (events, usage)
}

/// Encode a buffered slice of [`StreamEvent`]s into an OpenAI chat.completion response shape.
pub fn encode_response(events: &[StreamEvent], meta: &crate::ResponseMeta) -> serde_json::Value {
    let mut content = String::new();
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
    let mut current_tool: Option<(String, String, String)> = None; // (id, name, arguments)
    let mut input_tokens = 0u32;
    let mut output_tokens = 0u32;
    let mut cache_read = 0u32;

    for ev in events {
        match ev {
            StreamEvent::TextDelta { text } => content.push_str(text),
            StreamEvent::ToolUseStart { id, name } => {
                if let Some((tid, tname, args)) = current_tool.take() {
                    tool_calls.push(serde_json::json!({
                        "id": tid, "type": "function",
                        "function": { "name": tname, "arguments": args },
                    }));
                }
                current_tool = Some((id.clone(), name.clone(), String::new()));
            }
            StreamEvent::ToolUseInputDelta { delta } => {
                if let Some((_, _, ref mut args)) = current_tool {
                    args.push_str(delta);
                }
            }
            StreamEvent::InputUsage { input_tokens: it, cache_read: cr, .. } => {
                input_tokens = *it;
                cache_read = *cr;
            }
            StreamEvent::OutputUsage { output_tokens: ot } => output_tokens = *ot,
            StreamEvent::MessageStop | StreamEvent::Error { .. } => {}
        }
    }
    if let Some((id, name, args)) = current_tool.take() {
        tool_calls.push(serde_json::json!({
            "id": id, "type": "function",
            "function": { "name": name, "arguments": args },
        }));
    }

    let mut message = serde_json::json!({ "role": "assistant" });
    if !content.is_empty() {
        message["content"] = serde_json::Value::String(content);
    }
    if !tool_calls.is_empty() {
        message["tool_calls"] = serde_json::Value::Array(tool_calls);
    }

    let finish_reason = if message.get("tool_calls").is_some() { "tool_calls" } else { "stop" };

    let mut usage = serde_json::json!({
        "prompt_tokens": input_tokens,
        "completion_tokens": output_tokens,
        "total_tokens": input_tokens + output_tokens,
    });
    if cache_read > 0 {
        usage["prompt_tokens_details"] = serde_json::json!({ "cached_tokens": cache_read });
    }

    serde_json::json!({
        "id": meta.id,
        "object": "chat.completion",
        "created": meta.created,
        "model": meta.model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": usage,
    })
}

#[cfg(test)]
mod encode_response_tests {
    use super::*;
    use ox_kernel::StreamEvent;

    fn meta() -> crate::ResponseMeta {
        crate::ResponseMeta {
            id: "chatcmpl-test01".into(),
            model: "gpt-test".into(),
            created: 1_700_000_000,
        }
    }

    #[test]
    fn encode_text_only_chat_completion() {
        let events = vec![
            StreamEvent::TextDelta { text: "Hello".into() },
            StreamEvent::TextDelta { text: " world".into() },
            StreamEvent::InputUsage { input_tokens: 10, cache_creation: 0, cache_read: 0 },
            StreamEvent::OutputUsage { output_tokens: 2 },
            StreamEvent::MessageStop,
        ];
        let resp = encode_response(&events, &meta());
        assert_eq!(resp["object"], "chat.completion");
        assert_eq!(resp["id"], "chatcmpl-test01");
        assert_eq!(resp["model"], "gpt-test");
        assert_eq!(resp["created"], 1_700_000_000);
        let choices = resp["choices"].as_array().unwrap();
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0]["message"]["role"], "assistant");
        assert_eq!(choices[0]["message"]["content"], "Hello world");
        assert_eq!(choices[0]["finish_reason"], "stop");
        assert_eq!(resp["usage"]["prompt_tokens"], 10);
        assert_eq!(resp["usage"]["completion_tokens"], 2);
        assert_eq!(resp["usage"]["total_tokens"], 12);
    }

    #[test]
    fn encode_tool_call_finish_reason() {
        let events = vec![
            StreamEvent::ToolUseStart { id: "t1".into(), name: "read_file".into() },
            StreamEvent::ToolUseInputDelta { delta: r#"{"path":"/etc/hosts"}"#.into() },
            StreamEvent::MessageStop,
        ];
        let resp = encode_response(&events, &meta());
        assert_eq!(resp["choices"][0]["finish_reason"], "tool_calls");
        let tool_calls = resp["choices"][0]["message"]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "t1");
        assert_eq!(tool_calls[0]["type"], "function");
        assert_eq!(tool_calls[0]["function"]["name"], "read_file");
        assert_eq!(tool_calls[0]["function"]["arguments"], r#"{"path":"/etc/hosts"}"#);
    }

    #[test]
    fn encode_cached_tokens_emit_details() {
        let events = vec![
            StreamEvent::InputUsage { input_tokens: 100, cache_creation: 0, cache_read: 80 },
            StreamEvent::OutputUsage { output_tokens: 10 },
            StreamEvent::MessageStop,
        ];
        let resp = encode_response(&events, &meta());
        assert_eq!(resp["usage"]["prompt_tokens_details"]["cached_tokens"], 80);
    }

    #[test]
    fn encode_empty_events_has_no_content() {
        let resp = encode_response(&[], &meta());
        assert_eq!(resp["object"], "chat.completion");
        assert_eq!(resp["id"], "chatcmpl-test01");
        assert_eq!(resp["model"], "gpt-test");
        assert_eq!(resp["created"], 1_700_000_000);
        assert!(resp["choices"][0]["message"].get("content").is_none());
        assert!(resp["choices"][0]["message"].get("tool_calls").is_none());
    }
}

use crate::CodecError;

pub fn decode_request(body: &serde_json::Value) -> Result<ox_kernel::CompletionRequest, CodecError> {
    let obj = body.as_object().ok_or_else(|| {
        CodecError::InvalidShape("body must be a JSON object".into())
    })?;

    let model = obj.get("model").and_then(|v| v.as_str())
        .ok_or(CodecError::MissingField("model"))?
        .to_string();

    // 0 is the "omitted" sentinel: OpenAI treats max_tokens as optional, so
    // translate_request forwards nothing rather than fabricating a cap that
    // silently truncates long generations. max_completion_tokens is the
    // current OpenAI SDK spelling; accept both.
    let max_tokens = obj
        .get("max_tokens")
        .or_else(|| obj.get("max_completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    let stream = obj.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    let raw_messages = obj.get("messages").and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut system = String::new();
    let mut messages = Vec::new();
    for m in raw_messages {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role == "system" {
            let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if !system.is_empty() {
                system.push_str("\n\n");
            }
            system.push_str(content);
        } else {
            messages.push(m);
        }
    }

    let mut tools: Vec<ox_kernel::ToolSchema> = Vec::new();
    if let Some(arr) = obj.get("tools").and_then(|v| v.as_array()) {
        for t in arr {
            // A malformed entry is a client error, not something to silently
            // drop — the model would run without a tool the client sent.
            let Some(f) = t.get("function") else {
                return Err(CodecError::InvalidShape(
                    "tools entries require a `function` object".into(),
                ));
            };
            let Some(name) = f.get("name").and_then(|v| v.as_str()) else {
                return Err(CodecError::InvalidShape(
                    "tools entries require function.name".into(),
                ));
            };
            let description = f.get("description")
                .and_then(|v| v.as_str()).unwrap_or("").to_string();
            let input_schema = f.get("parameters")
                .cloned().unwrap_or(serde_json::Value::Null);
            tools.push(ox_kernel::ToolSchema {
                name: name.to_string(),
                description,
                input_schema,
            });
        }
    }

    // Carry generation params. `stop` and `tool_choice` are renamed/reshaped
    // to the canonical Anthropic-style form the extras map uses; everything
    // else is captured verbatim (openai-only keys like seed or
    // presence_penalty keep their names and are only re-emitted by the
    // openai translate path).
    const HANDLED: [&str; 8] = [
        "model",
        "max_tokens",
        "max_completion_tokens",
        "messages",
        "tools",
        "stream",
        "stream_options",
        "tool_choice",
    ];
    let mut extra: std::collections::BTreeMap<String, serde_json::Value> = obj
        .iter()
        .filter(|(k, _)| !HANDLED.contains(&k.as_str()) && k.as_str() != "stop")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    match obj.get("stop") {
        Some(serde_json::Value::String(s)) => {
            extra.insert("stop_sequences".into(), serde_json::json!([s]));
        }
        Some(serde_json::Value::Array(a)) => {
            extra.insert("stop_sequences".into(), serde_json::Value::Array(a.clone()));
        }
        _ => {}
    }
    if let Some(tc) = obj.get("tool_choice") {
        if let Some(canonical) = tool_choice_to_canonical(tc) {
            extra.insert("tool_choice".into(), canonical);
        }
    }

    Ok(ox_kernel::CompletionRequest {
        model, max_tokens, system, messages, tools, stream, extra,
    })
}

/// OpenAI `tool_choice` → the canonical (Anthropic-shaped) form.
/// Unknown shapes translate to None and are dropped rather than guessed.
fn tool_choice_to_canonical(tc: &serde_json::Value) -> Option<serde_json::Value> {
    match tc {
        serde_json::Value::String(s) => match s.as_str() {
            "auto" => Some(serde_json::json!({"type": "auto"})),
            "none" => Some(serde_json::json!({"type": "none"})),
            "required" => Some(serde_json::json!({"type": "any"})),
            _ => None,
        },
        serde_json::Value::Object(o) => {
            let name = o.get("function")?.get("name")?.as_str()?;
            Some(serde_json::json!({"type": "tool", "name": name}))
        }
        _ => None,
    }
}

/// Canonical `tool_choice` → the OpenAI wire shape.
fn tool_choice_to_openai(tc: &serde_json::Value) -> Option<serde_json::Value> {
    match tc.get("type")?.as_str()? {
        "auto" => Some(serde_json::json!("auto")),
        "none" => Some(serde_json::json!("none")),
        "any" => Some(serde_json::json!("required")),
        "tool" => {
            let name = tc.get("name")?.as_str()?;
            Some(serde_json::json!({"type": "function", "function": {"name": name}}))
        }
        _ => None,
    }
}

#[cfg(test)]
mod passthrough_tests {
    use super::*;

    #[test]
    fn decode_canonicalizes_stop_and_tool_choice() {
        let body = serde_json::json!({
            "model": "m",
            "messages": [],
            "temperature": 1.5,
            "stop": "DONE",
            "tool_choice": "required",
            "seed": 7,
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(req.extra["temperature"], serde_json::json!(1.5));
        assert_eq!(req.extra["stop_sequences"], serde_json::json!(["DONE"]));
        assert_eq!(req.extra["tool_choice"], serde_json::json!({"type": "any"}));
        assert_eq!(req.extra["seed"], serde_json::json!(7));
    }

    #[test]
    fn decode_maps_function_tool_choice() {
        let body = serde_json::json!({
            "model": "m",
            "messages": [],
            "tool_choice": {"type": "function", "function": {"name": "read_file"}},
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(
            req.extra["tool_choice"],
            serde_json::json!({"type": "tool", "name": "read_file"})
        );
    }

    #[test]
    fn decode_accepts_max_completion_tokens() {
        let body = serde_json::json!({
            "model": "m",
            "messages": [],
            "max_completion_tokens": 256,
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(req.max_tokens, 256);
    }

    #[test]
    fn translate_emits_openai_shapes_and_skips_anthropic_only_keys() {
        let body = serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.3,
            "stop": ["A", "B"],
            "tool_choice": "auto",
            "seed": 42,
        });
        let req = decode_request(&body).unwrap();
        let mut req = req;
        // simulate an anthropic-only canonical key arriving cross-dialect
        req.extra.insert("top_k".into(), serde_json::json!(40));
        req.extra.insert("thinking".into(), serde_json::json!({"type": "enabled"}));
        let wire = translate_request(&req);
        assert_eq!(wire["temperature"], serde_json::json!(0.3));
        assert_eq!(wire["stop"], serde_json::json!(["A", "B"]));
        assert_eq!(wire["tool_choice"], serde_json::json!("auto"));
        assert_eq!(wire["seed"], serde_json::json!(42));
        assert!(wire.get("top_k").is_none(), "top_k must not reach an openai upstream");
        assert!(wire.get("thinking").is_none());
        assert!(wire.get("stop_sequences").is_none());
    }

    #[test]
    fn cross_dialect_anthropic_client_to_openai_upstream() {
        // Anthropic-shaped inbound params, openai upstream body.
        let inbound = serde_json::json!({
            "model": "m",
            "max_tokens": 50,
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.1,
            "stop_sequences": ["HALT"],
            "tool_choice": {"type": "any"},
        });
        let req = crate::anthropic::decode_request(&inbound).unwrap();
        let wire = translate_request(&req);
        assert_eq!(wire["temperature"], serde_json::json!(0.1));
        assert_eq!(wire["stop"], serde_json::json!(["HALT"]));
        assert_eq!(wire["tool_choice"], serde_json::json!("required"));
    }
}

#[cfg(test)]
mod decode_tests {
    use super::*;
    use crate::CodecError;

    #[test]
    fn decode_minimal_request() {
        let body = serde_json::json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(req.model, "gpt-4o-mini");
        assert_eq!(req.messages.len(), 1);
        // 0 = omitted sentinel; translate_request forwards no cap.
        assert_eq!(req.max_tokens, 0);
        let wire = translate_request(&req);
        assert!(wire.get("max_tokens").is_none());
        assert_eq!(req.system, "");
        assert!(!req.stream);
    }

    #[test]
    fn decode_extracts_system_from_messages() {
        let body = serde_json::json!({
            "model": "m",
            "messages": [
                {"role": "system", "content": "be helpful"},
                {"role": "user", "content": "hi"}
            ]
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(req.system, "be helpful");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0]["role"], "user");
    }

    #[test]
    fn decode_concatenates_multiple_system_messages() {
        let body = serde_json::json!({
            "model": "m",
            "messages": [
                {"role": "system", "content": "first"},
                {"role": "system", "content": "second"},
                {"role": "user", "content": "hi"}
            ]
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(req.system, "first\n\nsecond");
        assert_eq!(req.messages.len(), 1);
    }

    #[test]
    fn decode_max_tokens_when_provided() {
        let body = serde_json::json!({
            "model": "m",
            "max_tokens": 512,
            "messages": []
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(req.max_tokens, 512);
    }

    #[test]
    fn missing_model_errors() {
        let body = serde_json::json!({"messages": []});
        assert_eq!(decode_request(&body).unwrap_err(), CodecError::MissingField("model"));
    }

    #[test]
    fn decode_translates_tools() {
        let body = serde_json::json!({
            "model": "m",
            "messages": [],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "Read a file",
                    "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}
                }
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
    use ox_kernel::ToolSchema;

    #[test]
    fn translate_system_prompt() {
        let request = CompletionRequest {
            model: "gpt-4o".into(),
            max_tokens: 1024,
            system: "You are helpful.".into(),
            messages: vec![],
            tools: vec![],
            stream: true,
            extra: Default::default(),
        };
        let body = translate_request(&request);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are helpful.");
    }

    #[test]
    fn translate_user_message() {
        let request = CompletionRequest {
            model: "gpt-4o".into(),
            max_tokens: 1024,
            system: String::new(),
            messages: vec![serde_json::json!({"role": "user", "content": "Hello"})],
            tools: vec![],
            stream: true,
            extra: Default::default(),
        };
        let body = translate_request(&request);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "Hello");
    }

    #[test]
    fn translate_assistant_with_tool_use() {
        let request = CompletionRequest {
            model: "gpt-4o".into(),
            max_tokens: 1024,
            system: String::new(),
            messages: vec![serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "Let me check."},
                    {"type": "tool_use", "id": "t1", "name": "search", "input": {"q": "test"}}
                ]
            })],
            tools: vec![],
            stream: true,
            extra: Default::default(),
        };
        let body = translate_request(&request);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "assistant");
        assert_eq!(msgs[0]["content"], "Let me check.");
        let tcs = msgs[0]["tool_calls"].as_array().unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0]["id"], "t1");
        assert_eq!(tcs[0]["function"]["name"], "search");
    }

    #[test]
    fn translate_tool_result() {
        let request = CompletionRequest {
            model: "gpt-4o".into(),
            max_tokens: 1024,
            system: String::new(),
            messages: vec![serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "result data"}
                ]
            })],
            tools: vec![],
            stream: true,
            extra: Default::default(),
        };
        let body = translate_request(&request);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "tool");
        assert_eq!(msgs[0]["tool_call_id"], "t1");
        assert_eq!(msgs[0]["content"], "result data");
    }

    #[test]
    fn translate_tool_schemas() {
        let request = CompletionRequest {
            model: "gpt-4o".into(),
            max_tokens: 1024,
            system: String::new(),
            messages: vec![],
            tools: vec![ToolSchema {
                name: "get_weather".into(),
                description: "Get weather".into(),
                input_schema: serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            }],
            stream: true,
            extra: Default::default(),
        };
        let body = translate_request(&request);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
        assert_eq!(tools[0]["function"]["description"], "Get weather");
    }

    #[test]
    fn translate_no_tools_omits_field() {
        let request = CompletionRequest {
            model: "gpt-4o".into(),
            max_tokens: 1024,
            system: String::new(),
            messages: vec![],
            tools: vec![],
            stream: true,
            extra: Default::default(),
        };
        let body = translate_request(&request);
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn parse_text_delta() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n";
        let (events, usage) = parse_sse_events(body);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::TextDelta { text } if text == "Hello"));
        assert_eq!(usage.input_tokens, 0);
    }

    #[test]
    fn parse_tool_call() {
        let body = "\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"tc1\",\"function\":{\"name\":\"echo\",\"arguments\":\"\"}}]}}]}\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"text\\\"\"}}]}}]}\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\": \\\"hi\\\"}\"}}]}}]}\n\
data: [DONE]\n";
        let (events, _usage) = parse_sse_events(body);
        assert_eq!(events.len(), 4);
        assert!(
            matches!(&events[0], StreamEvent::ToolUseStart { id, name } if id == "tc1" && name == "echo")
        );
        assert!(matches!(&events[1], StreamEvent::ToolUseInputDelta { delta } if delta == "{\"text\""));
        assert!(matches!(&events[2], StreamEvent::ToolUseInputDelta { delta } if delta == ": \"hi\"}"));
        assert!(matches!(&events[3], StreamEvent::MessageStop));
    }

    #[test]
    fn parse_usage_extraction() {
        let body =
            "data: {\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":50}}\ndata: [DONE]\n";
        let (events, usage) = parse_sse_events(body);
        assert_eq!(events.len(), 1); // just MessageStop
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
    }

    #[test]
    fn parse_usage_with_cached_tokens() {
        let body = "data: {\"usage\":{\"prompt_tokens\":500,\"completion_tokens\":80,\"prompt_tokens_details\":{\"cached_tokens\":400}}}\ndata: [DONE]\n";
        let (_events, usage) = parse_sse_events(body);
        assert_eq!(usage.input_tokens, 500);
        assert_eq!(usage.output_tokens, 80);
        assert_eq!(usage.cache_read_input_tokens, 400);
        assert_eq!(usage.cache_creation_input_tokens, 0);
    }

    #[test]
    fn parse_usage_without_cached_tokens_defaults_to_zero() {
        let body =
            "data: {\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":50}}\ndata: [DONE]\n";
        let (_events, usage) = parse_sse_events(body);
        assert_eq!(usage.cache_read_input_tokens, 0);
    }

    #[test]
    fn parse_done_marker() {
        let body = "data: [DONE]\n";
        let (events, _usage) = parse_sse_events(body);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::MessageStop));
    }

    #[test]
    fn parse_ignores_non_data_lines() {
        let body =
            ": comment\nevent: ping\ndata: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n";
        let (events, _usage) = parse_sse_events(body);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::TextDelta { text } if text == "Hi"));
    }

    #[test]
    fn parse_multiple_tool_calls() {
        let body = "\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"tc1\",\"function\":{\"name\":\"a\",\"arguments\":\"{}\"}}]}}]}\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"tc2\",\"function\":{\"name\":\"b\",\"arguments\":\"{}\"}}]}}]}\n\
data: [DONE]\n";
        let (events, _usage) = parse_sse_events(body);
        assert_eq!(events.len(), 5); // start+args, start+args, MessageStop
        assert!(matches!(&events[0], StreamEvent::ToolUseStart { name, .. } if name == "a"));
        assert!(matches!(&events[1], StreamEvent::ToolUseInputDelta { delta } if delta == "{}"));
        assert!(matches!(&events[2], StreamEvent::ToolUseStart { name, .. } if name == "b"));
        assert!(matches!(&events[3], StreamEvent::ToolUseInputDelta { delta } if delta == "{}"));
        assert!(matches!(&events[4], StreamEvent::MessageStop));
    }
}

//! Stateless free functions that implement the agentic loop.
//!
//! These are composable building blocks that operate on StructFS stores
//! directly — no struct, no mutable state between calls.

use crate::{
    AgentEvent, CompletionRequest, ContentBlock, StreamEvent, ToolCall, ToolResult, ToolSchema,
    serialize_assistant_message, serialize_tool_results,
};
use serde::{Deserialize, Serialize};
use structfs_core_store::{Path, Reader, Record, Store, Value, Writer, path};

// ---------------------------------------------------------------------------
// ContextRef types
// ---------------------------------------------------------------------------

/// A typed reference to context that should be included in a completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContextRef {
    #[serde(rename = "system")]
    System { path: String },

    #[serde(rename = "history")]
    History {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        last: Option<usize>,
    },

    #[serde(rename = "tools")]
    Tools {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        only: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        except: Option<Vec<String>>,
    },

    #[serde(rename = "raw")]
    Raw { content: String },
}

/// The result of resolving a set of context references.
#[derive(Debug, Clone, Default)]
pub struct ResolvedContext {
    pub system: String,
    pub messages: Vec<serde_json::Value>,
    pub tools: Vec<ToolSchema>,
    pub extra_content: Vec<String>,
}

/// Resolve context references by reading from the namespace.
pub fn resolve_refs(
    context: &mut dyn Reader,
    refs: &[ContextRef],
) -> Result<ResolvedContext, String> {
    let mut resolved = ResolvedContext::default();

    for r in refs {
        match r {
            ContextRef::System { path } => {
                let p = Path::parse(path).map_err(|e| e.to_string())?;
                match context.read(&p).map_err(|e| e.to_string())? {
                    Some(Record::Parsed(Value::String(s))) => resolved.system = s,
                    Some(_) => return Err(format!("expected string at {path}")),
                    None => return Err(format!("nothing at {path}")),
                }
            }
            ContextRef::History { path, last } => {
                let p = Path::parse(path).map_err(|e| e.to_string())?;
                let json = match context.read(&p).map_err(|e| e.to_string())? {
                    Some(Record::Parsed(v)) => structfs_serde_store::value_to_json(v),
                    _ => return Err(format!("expected parsed record at {path}")),
                };
                let mut messages: Vec<serde_json::Value> =
                    serde_json::from_value(json).map_err(|e| e.to_string())?;
                if let Some(n) = last {
                    let start = messages.len().saturating_sub(*n);
                    messages = messages[start..].to_vec();
                }
                resolved.messages = messages;
            }
            ContextRef::Tools { path, only, except } => {
                let p = Path::parse(path).map_err(|e| e.to_string())?;
                let json = match context.read(&p).map_err(|e| e.to_string())? {
                    Some(Record::Parsed(v)) => structfs_serde_store::value_to_json(v),
                    _ => return Err(format!("expected parsed record at {path}")),
                };
                let mut tools: Vec<ToolSchema> =
                    serde_json::from_value(json).map_err(|e| e.to_string())?;
                if let Some(only) = only {
                    tools.retain(|t| only.contains(&t.name));
                }
                if let Some(except) = except {
                    tools.retain(|t| !except.contains(&t.name));
                }
                resolved.tools = tools;
            }
            ContextRef::Raw { content } => {
                resolved.extra_content.push(content.clone());
            }
        }
    }

    Ok(resolved)
}

/// The default bootstrap refs: system prompt, full history, all tools.
pub fn default_refs() -> Vec<ContextRef> {
    vec![
        ContextRef::System {
            path: "system".into(),
        },
        ContextRef::History {
            path: "history/messages".into(),
            last: None,
        },
        ContextRef::Tools {
            path: "tools/schemas".into(),
            only: None,
            except: None,
        },
    ]
}

// ---------------------------------------------------------------------------
// Stream event codec
// ---------------------------------------------------------------------------

/// Serialize a [`StreamEvent`] to JSON.
pub fn stream_event_to_json(event: &StreamEvent) -> serde_json::Value {
    match event {
        StreamEvent::TextDelta(text) => serde_json::json!({
            "type": "text_delta",
            "text": text,
        }),
        StreamEvent::ToolUseStart { id, name } => serde_json::json!({
            "type": "tool_use_start",
            "id": id,
            "name": name,
        }),
        StreamEvent::ToolUseInputDelta(delta) => serde_json::json!({
            "type": "tool_use_input_delta",
            "delta": delta,
        }),
        StreamEvent::MessageStop => serde_json::json!({
            "type": "message_stop",
        }),
        StreamEvent::Error(msg) => serde_json::json!({
            "type": "error",
            "message": msg,
        }),
    }
}

/// Deserialize a [`StreamEvent`] from JSON.
pub fn json_to_stream_event(json: &serde_json::Value) -> Result<StreamEvent, String> {
    let typ = json
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or("missing or non-string 'type' field")?;

    match typ {
        "text_delta" => {
            let text = json
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or("missing 'text' field")?;
            Ok(StreamEvent::TextDelta(text.to_string()))
        }
        "tool_use_start" => {
            let id = json
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or("missing 'id' field")?;
            let name = json
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("missing 'name' field")?;
            Ok(StreamEvent::ToolUseStart {
                id: id.to_string(),
                name: name.to_string(),
            })
        }
        "tool_use_input_delta" => {
            let delta = json
                .get("delta")
                .and_then(|v| v.as_str())
                .ok_or("missing 'delta' field")?;
            Ok(StreamEvent::ToolUseInputDelta(delta.to_string()))
        }
        "message_stop" => Ok(StreamEvent::MessageStop),
        "error" => {
            let msg = json
                .get("message")
                .and_then(|v| v.as_str())
                .ok_or("missing 'message' field")?;
            Ok(StreamEvent::Error(msg.to_string()))
        }
        other => Err(format!("unknown stream event type: {other}")),
    }
}

/// Serialize an [`AgentEvent`] to JSON.
pub fn agent_event_to_json(event: &AgentEvent) -> serde_json::Value {
    match event {
        AgentEvent::TurnStart => serde_json::json!({ "type": "turn_start" }),
        AgentEvent::TextDelta(text) => serde_json::json!({
            "type": "text_delta",
            "text": text,
        }),
        AgentEvent::ToolCallStart { name } => serde_json::json!({
            "type": "tool_call_start",
            "name": name,
        }),
        AgentEvent::ToolCallResult { name, result } => serde_json::json!({
            "type": "tool_call_result",
            "name": name,
            "result": result,
        }),
        AgentEvent::TurnEnd => serde_json::json!({ "type": "turn_end" }),
        AgentEvent::Error(msg) => serde_json::json!({
            "type": "error",
            "message": msg,
        }),
    }
}

/// Extract a list of [`StreamEvent`]s from a StructFS [`Record`].
///
/// Expects a `Record::Parsed` containing a JSON array of serialized events.
pub fn deserialize_events(record: Record) -> Result<Vec<StreamEvent>, String> {
    let value = match record {
        Record::Parsed(v) => v,
        _ => return Err("expected parsed record".into()),
    };
    let json = structfs_serde_store::value_to_json(value);
    let arr = json.as_array().ok_or("expected JSON array of events")?;
    arr.iter().map(json_to_stream_event).collect()
}

// ---------------------------------------------------------------------------
// Building blocks
// ---------------------------------------------------------------------------

/// Fire a completion with resolved context references.
///
/// 1. Resolves refs by reading from the namespace
/// 2. Reads model config from `gate/defaults/`
/// 3. Assembles a CompletionRequest (internal detail)
/// 4. Sends via `tools/completions/complete/{account}`
/// 5. Returns stream events
///
/// This is the kernel's completion primitive. `run_turn` calls it with
/// bootstrap refs. The LLM can call it as a tool (with custom refs).
pub fn complete(
    context: &mut dyn Store,
    account: &str,
    refs: &[ContextRef],
) -> Result<Vec<StreamEvent>, String> {
    let resolved = resolve_refs(context, refs)?;
    let (model, max_tokens) = read_model_config(context)?;

    let system = if resolved.extra_content.is_empty() {
        resolved.system
    } else {
        format!(
            "{}\n\n{}",
            resolved.system,
            resolved.extra_content.join("\n\n")
        )
    };

    let request = CompletionRequest {
        model,
        max_tokens,
        system,
        messages: resolved.messages,
        tools: resolved.tools,
        stream: true,
    };

    send_completion(context, account, &request)
}

/// Read prompt components from context and assemble a [`CompletionRequest`].
///
/// Convenience wrapper for async consumers (like ox-web) that need the
/// request for their own transport. Sync consumers should prefer
/// [`complete`] which handles transport internally.
pub fn synthesize(context: &mut dyn Reader) -> Result<CompletionRequest, String> {
    let refs = default_refs();
    let resolved = resolve_refs(context, &refs)?;
    let (model, max_tokens) = read_model_config(context)?;

    Ok(CompletionRequest {
        model,
        max_tokens,
        system: resolved.system,
        messages: resolved.messages,
        tools: resolved.tools,
        stream: true,
    })
}

/// Process stream events into content blocks, emitting [`AgentEvent`]s.
///
/// This is a pure function — no store access. The caller provides an emit
/// callback for observability.
pub fn accumulate_response(
    events: Vec<StreamEvent>,
    emit: &mut dyn FnMut(AgentEvent),
) -> Result<Vec<ContentBlock>, String> {
    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut current_text = String::new();
    let mut current_tool: Option<(String, String, String)> = None; // (id, name, input_json)

    for event in events {
        match event {
            StreamEvent::TextDelta(text) => {
                // Flush any pending tool
                flush_tool(&mut blocks, &mut current_tool);
                current_text.push_str(&text);
                emit(AgentEvent::TextDelta(text));
            }
            StreamEvent::ToolUseStart { id, name } => {
                // Flush any pending text
                flush_text(&mut blocks, &mut current_text);
                // Flush any prior tool
                flush_tool(&mut blocks, &mut current_tool);
                current_tool = Some((id, name, String::new()));
            }
            StreamEvent::ToolUseInputDelta(delta) => {
                if let Some((_, _, ref mut input_json)) = current_tool {
                    input_json.push_str(&delta);
                }
            }
            StreamEvent::MessageStop => {
                break;
            }
            StreamEvent::Error(e) => {
                flush_text(&mut blocks, &mut current_text);
                flush_tool(&mut blocks, &mut current_tool);
                emit(AgentEvent::Error(e.clone()));
                return Err(e);
            }
        }
    }

    // Flush remaining
    flush_text(&mut blocks, &mut current_text);
    flush_tool(&mut blocks, &mut current_tool);

    Ok(blocks)
}

/// Write the assistant message to history and extract tool calls.
///
/// Writes the serialized assistant message to `history/append` and returns
/// the tool calls the model requested. If empty, the turn is done.
pub fn record_turn(
    context: &mut dyn Writer,
    content: &[ContentBlock],
) -> Result<Vec<ToolCall>, String> {
    let assistant_json = serialize_assistant_message(content);
    let record = Record::parsed(structfs_serde_store::json_to_value(assistant_json));
    context
        .write(&path!("history/append"), record)
        .map_err(|e| e.to_string())?;

    let tool_calls: Vec<ToolCall> = content
        .iter()
        .filter_map(|block| {
            if let ContentBlock::ToolUse(tc) = block {
                Some(tc.clone())
            } else {
                None
            }
        })
        .collect();

    Ok(tool_calls)
}

/// Execute tool calls via the context store.
///
/// For each tool call: emits `ToolCallStart`, writes the input to
/// `tools/{wire_name}`, reads the result from `tools/{wire_name}/result`,
/// and emits `ToolCallResult`.
pub fn execute_tools(
    context: &mut dyn Store,
    tool_calls: &[ToolCall],
    emit: &mut dyn FnMut(AgentEvent),
) -> Result<Vec<ToolResult>, String> {
    let mut results = Vec::new();

    for tc in tool_calls {
        emit(AgentEvent::ToolCallStart {
            name: tc.name.clone(),
        });

        // Write tool input
        let tool_path = Path::parse(&format!("tools/{}", tc.name)).map_err(|e| e.to_string())?;
        let input_value = structfs_serde_store::json_to_value(tc.input.clone());
        context
            .write(&tool_path, Record::parsed(input_value))
            .map_err(|e| e.to_string())?;

        // Read tool result
        let result_path =
            Path::parse(&format!("tools/{}/result", tc.name)).map_err(|e| e.to_string())?;
        let result_record = context
            .read(&result_path)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("tool '{}' returned no result", tc.name))?;

        let result_value = match result_record {
            Record::Parsed(v) => structfs_serde_store::value_to_json(v),
            _ => return Err(format!("expected parsed result from tool '{}'", tc.name)),
        };

        let result_str = match &result_value {
            serde_json::Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };

        emit(AgentEvent::ToolCallResult {
            name: tc.name.clone(),
            result: result_str,
        });

        results.push(ToolResult {
            tool_use_id: tc.id.clone(),
            content: result_value,
        });
    }

    Ok(results)
}

/// Write tool results to history.
pub fn record_tool_results(context: &mut dyn Writer, results: &[ToolResult]) -> Result<(), String> {
    let results_json = serialize_tool_results(results);
    let record = Record::parsed(structfs_serde_store::json_to_value(results_json));
    context
        .write(&path!("history/append"), record)
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Send a completion request via the context store and return parsed events.
fn send_completion(
    context: &mut dyn Store,
    account: &str,
    request: &CompletionRequest,
) -> Result<Vec<StreamEvent>, String> {
    let request_path =
        Path::parse(&format!("tools/completions/complete/{account}")).map_err(|e| e.to_string())?;
    let request_json = serde_json::to_value(request).map_err(|e| e.to_string())?;
    let request_value = structfs_serde_store::json_to_value(request_json);
    context
        .write(&request_path, Record::parsed(request_value))
        .map_err(|e| e.to_string())?;

    let response_path = Path::parse(&format!("tools/completions/complete/{account}/response"))
        .map_err(|e| e.to_string())?;
    let response_record = context
        .read(&response_path)
        .map_err(|e| e.to_string())?
        .ok_or("completion returned no response")?;

    deserialize_events(response_record)
}

/// Read model ID and max_tokens from gate defaults.
fn read_model_config(context: &mut dyn Reader) -> Result<(String, u32), String> {
    let model = match context
        .read(&path!("gate/defaults/model"))
        .map_err(|e| e.to_string())?
    {
        Some(Record::Parsed(Value::String(s))) => s,
        _ => return Err("expected string from gate/defaults/model".into()),
    };
    let max_tokens = match context
        .read(&path!("gate/defaults/max_tokens"))
        .map_err(|e| e.to_string())?
    {
        Some(Record::Parsed(Value::Integer(n))) => n as u32,
        _ => return Err("expected integer from gate/defaults/max_tokens".into()),
    };
    Ok((model, max_tokens))
}

/// Read the default account from the context, defaulting to `"anthropic"`.
fn read_default_account(context: &mut dyn Reader) -> Result<String, String> {
    let record = context
        .read(&path!("gate/defaults/account"))
        .map_err(|e| e.to_string())?;

    match record {
        Some(Record::Parsed(Value::String(s))) => Ok(s),
        Some(_) => Err("expected string for gate/defaults/account".into()),
        None => Ok("anthropic".to_string()),
    }
}

/// Best-effort append to the structured log. Ignores errors.
fn log_entry(context: &mut dyn Writer, entry: serde_json::Value) {
    let value = structfs_serde_store::json_to_value(entry);
    let _ = context.write(&path!("log/append"), Record::parsed(value));
}

// ---------------------------------------------------------------------------
// Full agentic loop
// ---------------------------------------------------------------------------

/// Run a complete agentic turn loop.
///
/// 1. Read default account
/// 2. Loop: emit TurnStart → synthesize → send_completion → accumulate_response
///    → log assistant entry → record_turn → if no tools: emit TurnEnd, return
///    → log tool calls → execute_tools → log tool results → record_tool_results → loop
pub fn run_turn(context: &mut dyn Store, emit: &mut dyn FnMut(AgentEvent)) -> Result<(), String> {
    let account = read_default_account(context)?;
    let refs = default_refs();

    loop {
        emit(AgentEvent::TurnStart);

        let events = complete(context, &account, &refs)?;
        let content = accumulate_response(events, emit)?;

        // Log assistant entry
        log_entry(
            context,
            serde_json::json!({
                "type": "assistant",
                "content": serde_json::to_value(&content).unwrap_or(serde_json::Value::Null),
                "source": { "account": &account }
            }),
        );

        let tool_calls = record_turn(context, &content)?;

        if tool_calls.is_empty() {
            emit(AgentEvent::TurnEnd);
            return Ok(());
        }

        // Log tool calls
        for tc in &tool_calls {
            log_entry(
                context,
                serde_json::json!({
                    "type": "tool_call",
                    "id": tc.id,
                    "name": tc.name,
                    "input": tc.input,
                }),
            );
        }

        let results = execute_tools(context, &tool_calls, emit)?;

        // Log tool results
        for r in &results {
            log_entry(
                context,
                serde_json::json!({
                    "type": "tool_result",
                    "id": r.tool_use_id,
                    "output": r.content,
                }),
            );
        }

        record_tool_results(context, &results)?;
    }
}

// ---------------------------------------------------------------------------
// Private helpers for accumulate_response
// ---------------------------------------------------------------------------

fn flush_text(blocks: &mut Vec<ContentBlock>, text: &mut String) {
    if !text.is_empty() {
        blocks.push(ContentBlock::Text {
            text: std::mem::take(text),
        });
    }
}

fn flush_tool(blocks: &mut Vec<ContentBlock>, tool: &mut Option<(String, String, String)>) {
    if let Some((id, name, input_json)) = tool.take() {
        let input: serde_json::Value =
            serde_json::from_str(&input_json).unwrap_or(serde_json::Value::Null);
        blocks.push(ContentBlock::ToolUse(ToolCall { id, name, input }));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use structfs_core_store::Error as StoreError;

    // -----------------------------------------------------------------------
    // MockStore
    // -----------------------------------------------------------------------

    struct MockStore {
        data: BTreeMap<String, Value>,
        appended: Vec<(String, Value)>,
        completion_responses: std::sync::Mutex<std::collections::VecDeque<Value>>,
    }

    impl MockStore {
        fn new() -> Self {
            Self {
                data: BTreeMap::new(),
                appended: Vec::new(),
                completion_responses: std::sync::Mutex::new(std::collections::VecDeque::new()),
            }
        }

        fn set(&mut self, path: &str, value: Value) {
            self.data.insert(path.to_string(), value);
        }

        fn push_completion_response(&mut self, events_json: serde_json::Value) {
            self.completion_responses
                .lock()
                .unwrap()
                .push_back(structfs_serde_store::json_to_value(events_json));
        }
    }

    impl Reader for MockStore {
        fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
            let key = from.to_string();
            // Completion response reads are served from the queue when available.
            if key.contains("completions/complete") && key.ends_with("/response") {
                if let Some(value) = self.completion_responses.lock().unwrap().pop_front() {
                    return Ok(Some(Record::parsed(value)));
                }
            }
            Ok(self.data.get(&key).map(|v| Record::parsed(v.clone())))
        }
    }

    impl Writer for MockStore {
        fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
            if let Record::Parsed(v) = &data {
                self.appended.push((to.to_string(), v.clone()));
            }
            let value = match data {
                Record::Parsed(v) => v,
                _ => return Err(StoreError::store("mock", "write", "expected parsed")),
            };
            let key = to.to_string();
            self.data.insert(key.clone(), value.clone());
            // Simulate tool execution: writing to tools/X (not tools/X/result) also stores
            // a result at tools/X/result so execute_tools can read it back.
            if key.starts_with("tools/") && !key.contains("/result") {
                self.data.insert(format!("{key}/result"), value);
            }
            Ok(to.clone())
        }
    }

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn setup_synthesize_store() -> MockStore {
        let mut store = MockStore::new();
        store.set("system", Value::String("You are helpful.".into()));
        store.set(
            "history/messages",
            structfs_serde_store::json_to_value(serde_json::json!([
                {"role": "user", "content": "Hello"}
            ])),
        );
        store.set(
            "tools/schemas",
            structfs_serde_store::json_to_value(serde_json::json!([
                {
                    "name": "get_weather",
                    "description": "Gets weather",
                    "input_schema": {"type": "object", "properties": {}}
                }
            ])),
        );
        store.set("gate/defaults/model", Value::String("claude-test".into()));
        store.set("gate/defaults/max_tokens", Value::Integer(2048));
        store
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[test]
    fn stream_event_json_roundtrip() {
        let events = vec![
            StreamEvent::TextDelta("hello".into()),
            StreamEvent::ToolUseStart {
                id: "t1".into(),
                name: "read".into(),
            },
            StreamEvent::ToolUseInputDelta("{\"a\":1}".into()),
            StreamEvent::MessageStop,
            StreamEvent::Error("boom".into()),
        ];

        for event in &events {
            let json = stream_event_to_json(event);
            let roundtripped = json_to_stream_event(&json).unwrap();
            // Compare via JSON roundtrip since StreamEvent doesn't derive PartialEq
            let json2 = stream_event_to_json(&roundtripped);
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn synthesize_assembles_request() {
        let mut store = setup_synthesize_store();
        let request = synthesize(&mut store).unwrap();

        assert_eq!(request.model, "claude-test");
        assert_eq!(request.max_tokens, 2048);
        assert_eq!(request.system, "You are helpful.");
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].name, "get_weather");
        assert!(request.stream);
    }

    #[test]
    fn synthesize_fails_on_missing_system() {
        let mut store = MockStore::new();
        let result = synthesize(&mut store);
        assert!(result.is_err());
    }

    #[test]
    fn accumulate_text_only() {
        let events = vec![
            StreamEvent::TextDelta("Hello ".into()),
            StreamEvent::TextDelta("world".into()),
            StreamEvent::MessageStop,
        ];
        let mut emitted = Vec::new();
        let blocks = accumulate_response(events, &mut |e| emitted.push(e)).unwrap();

        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello world"),
            _ => panic!("expected Text block"),
        }
        // Should have emitted two TextDelta events
        assert_eq!(
            emitted
                .iter()
                .filter(|e| matches!(e, AgentEvent::TextDelta(_)))
                .count(),
            2
        );
    }

    #[test]
    fn accumulate_tool_use() {
        let events = vec![
            StreamEvent::ToolUseStart {
                id: "t1".into(),
                name: "get_weather".into(),
            },
            StreamEvent::ToolUseInputDelta(r#"{"city":"NYC"}"#.into()),
            StreamEvent::MessageStop,
        ];
        let mut emitted = Vec::new();
        let blocks = accumulate_response(events, &mut |e| emitted.push(e)).unwrap();

        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::ToolUse(tc) => {
                assert_eq!(tc.id, "t1");
                assert_eq!(tc.name, "get_weather");
                assert_eq!(tc.input, serde_json::json!({"city": "NYC"}));
            }
            _ => panic!("expected ToolUse block"),
        }
    }

    #[test]
    fn accumulate_mixed_text_and_tools() {
        let events = vec![
            StreamEvent::TextDelta("Let me check.".into()),
            StreamEvent::ToolUseStart {
                id: "t1".into(),
                name: "get_weather".into(),
            },
            StreamEvent::ToolUseInputDelta("{}".into()),
            StreamEvent::MessageStop,
        ];
        let mut emitted = Vec::new();
        let blocks = accumulate_response(events, &mut |e| emitted.push(e)).unwrap();

        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], ContentBlock::Text { .. }));
        assert!(matches!(&blocks[1], ContentBlock::ToolUse(_)));
    }

    #[test]
    fn record_turn_writes_history_and_extracts_tools() {
        let content = vec![
            ContentBlock::Text {
                text: "I'll check.".into(),
            },
            ContentBlock::ToolUse(ToolCall {
                id: "t1".into(),
                name: "get_weather".into(),
                input: serde_json::json!({"city": "NYC"}),
            }),
        ];

        let mut store = MockStore::new();
        let tool_calls = record_turn(&mut store, &content).unwrap();

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "get_weather");
        // Verify history/append was written
        assert!(
            store
                .appended
                .iter()
                .any(|(path, _)| path == "history/append")
        );
    }

    #[test]
    fn record_turn_no_tools() {
        let content = vec![ContentBlock::Text {
            text: "Hello!".into(),
        }];

        let mut store = MockStore::new();
        let tool_calls = record_turn(&mut store, &content).unwrap();

        assert!(tool_calls.is_empty());
        assert!(
            store
                .appended
                .iter()
                .any(|(path, _)| path == "history/append")
        );
    }

    #[test]
    fn record_tool_results_writes_history() {
        let results = vec![ToolResult {
            tool_use_id: "t1".into(),
            content: serde_json::json!("sunny, 72F"),
        }];

        let mut store = MockStore::new();
        record_tool_results(&mut store, &results).unwrap();

        assert!(
            store
                .appended
                .iter()
                .any(|(path, _)| path == "history/append")
        );
    }

    #[test]
    fn agent_event_to_json_all_variants() {
        let events = vec![
            AgentEvent::TurnStart,
            AgentEvent::TextDelta("hi".into()),
            AgentEvent::ToolCallStart {
                name: "read_file".into(),
            },
            AgentEvent::ToolCallResult {
                name: "read_file".into(),
                result: "ok".into(),
            },
            AgentEvent::TurnEnd,
            AgentEvent::Error("oops".into()),
        ];
        for event in &events {
            let json = agent_event_to_json(event);
            assert!(
                json.get("type").is_some(),
                "missing 'type' field in {json:?}"
            );
        }
    }

    #[test]
    fn deserialize_events_from_record() {
        let json = serde_json::json!([
            {"type": "text_delta", "text": "hello"},
            {"type": "message_stop"}
        ]);
        let record = Record::parsed(structfs_serde_store::json_to_value(json));
        let events = deserialize_events(record).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], StreamEvent::TextDelta(t) if t == "hello"));
        assert!(matches!(&events[1], StreamEvent::MessageStop));
    }

    #[test]
    fn read_default_account_returns_stored_value() {
        let mut store = MockStore::new();
        store.set("gate/defaults/account", Value::String("openai".into()));
        let account = read_default_account(&mut store).unwrap();
        assert_eq!(account, "openai");
    }

    #[test]
    fn read_default_account_defaults_to_anthropic() {
        let mut store = MockStore::new();
        let account = read_default_account(&mut store).unwrap();
        assert_eq!(account, "anthropic");
    }

    #[test]
    fn execute_tools_writes_and_reads_results() {
        let mut store = MockStore::new();
        let tool_calls = vec![ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            input: serde_json::json!({"text": "hello"}),
        }];
        let mut events = vec![];
        let results = execute_tools(&mut store, &tool_calls, &mut |e| {
            events.push(format!("{e:?}"))
        })
        .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tool_use_id, "tc1");
        assert!(
            events.iter().any(|e| e.contains("ToolCallStart")),
            "expected ToolCallStart event"
        );
        assert!(
            events.iter().any(|e| e.contains("ToolCallResult")),
            "expected ToolCallResult event"
        );
    }

    #[test]
    fn send_completion_writes_request_and_reads_response() {
        let mut store = MockStore::new();
        // Pre-populate the response path that send_completion will read from.
        let events_json = serde_json::json!([
            {"type": "text_delta", "text": "hello"},
            {"type": "message_stop"}
        ]);
        store.set(
            "tools/completions/complete/test/response",
            structfs_serde_store::json_to_value(events_json),
        );

        let request = CompletionRequest {
            model: "test".into(),
            max_tokens: 100,
            system: "test".into(),
            messages: vec![],
            tools: vec![],
            stream: true,
        };
        let events = send_completion(&mut store, "test", &request).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], StreamEvent::TextDelta(t) if t == "hello"));
        assert!(matches!(&events[1], StreamEvent::MessageStop));
    }

    #[test]
    fn log_entry_writes_to_log_append() {
        let mut store = MockStore::new();
        log_entry(
            &mut store,
            serde_json::json!({"type": "meta", "data": "test"}),
        );
        assert!(
            store.appended.iter().any(|(p, _)| p == "log/append"),
            "expected log/append write"
        );
    }

    #[test]
    fn run_turn_text_only() {
        let mut store = MockStore::new();
        // Context paths read by synthesize + read_default_account
        store.set("gate/defaults/account", Value::String("test".into()));
        store.set("system", Value::String("You are helpful.".into()));
        store.set(
            "history/messages",
            structfs_serde_store::json_to_value(
                serde_json::json!([{"role": "user", "content": "hi"}]),
            ),
        );
        store.set(
            "tools/schemas",
            structfs_serde_store::json_to_value(serde_json::json!([])),
        );
        store.set("gate/defaults/model", Value::String("test-model".into()));
        store.set("gate/defaults/max_tokens", Value::Integer(100));
        // Pre-populate completion response (text only — no tool calls → loop exits)
        store.set(
            "tools/completions/complete/test/response",
            structfs_serde_store::json_to_value(serde_json::json!([
                {"type": "text_delta", "text": "Hello!"},
                {"type": "message_stop"}
            ])),
        );

        let mut events = vec![];
        run_turn(&mut store, &mut |e| events.push(format!("{e:?}"))).unwrap();
        assert!(
            events.iter().any(|e| e.contains("TurnStart")),
            "expected TurnStart"
        );
        assert!(
            events.iter().any(|e| e.contains("TurnEnd")),
            "expected TurnEnd"
        );
        assert!(
            events.iter().any(|e| e.contains("TextDelta")),
            "expected TextDelta"
        );
    }

    #[test]
    fn json_to_stream_event_unknown_type() {
        let json = serde_json::json!({"type": "unknown_event"});
        let result = json_to_stream_event(&json);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown"));
    }

    #[test]
    fn accumulate_response_error_event() {
        let events = vec![
            StreamEvent::TextDelta("partial".into()),
            StreamEvent::Error("something broke".into()),
        ];
        let mut emitted = vec![];
        let result = accumulate_response(events, &mut |e| emitted.push(format!("{e:?}")));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("something broke"));
        assert!(emitted.iter().any(|e| e.contains("Error")));
    }

    #[test]
    fn deserialize_events_raw_record_returns_err() {
        // Record::Raw is the non-Parsed branch — deserialize_events must reject it.
        use structfs_core_store::{Bytes, Format};
        let record = Record::raw(Bytes::from_static(b"not parsed"), Format::OCTET_STREAM);
        let result = deserialize_events(record);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("parsed"));
    }

    #[test]
    fn synthesize_wrong_type_for_system() {
        let mut store = MockStore::new();
        store.set("system", Value::Integer(42)); // wrong type
        store.set(
            "history/messages",
            structfs_serde_store::json_to_value(serde_json::json!([])),
        );
        store.set(
            "tools/schemas",
            structfs_serde_store::json_to_value(serde_json::json!([])),
        );
        store.set("gate/defaults/model", Value::String("test".into()));
        store.set("gate/defaults/max_tokens", Value::Integer(100));
        let result = synthesize(&mut store);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("system"));
    }

    #[test]
    fn run_turn_with_tool_calls() {
        let mut store = MockStore::new();
        // Context paths
        store.set("gate/defaults/account", Value::String("test".into()));
        store.set("system", Value::String("You are helpful.".into()));
        store.set(
            "history/messages",
            structfs_serde_store::json_to_value(
                serde_json::json!([{"role": "user", "content": "hi"}]),
            ),
        );
        store.set(
            "tools/schemas",
            structfs_serde_store::json_to_value(serde_json::json!([])),
        );
        store.set("gate/defaults/model", Value::String("test".into()));
        store.set("gate/defaults/max_tokens", Value::Integer(100));

        // First completion: returns a tool call
        store.push_completion_response(serde_json::json!([
            {"type": "tool_use_start", "id": "tc1", "name": "echo"},
            {"type": "tool_use_input_delta", "delta": "{\"x\":1}"},
            {"type": "message_stop"}
        ]));
        // Second completion: returns plain text so the loop exits
        store.push_completion_response(serde_json::json!([
            {"type": "text_delta", "text": "Done!"},
            {"type": "message_stop"}
        ]));

        let mut events = vec![];
        run_turn(&mut store, &mut |e| events.push(format!("{e:?}"))).unwrap();

        // Two loop iterations → two TurnStart events
        let turn_starts = events.iter().filter(|e| e.contains("TurnStart")).count();
        assert_eq!(
            turn_starts, 2,
            "expected two TurnStart events (one per iteration)"
        );

        assert!(
            events.iter().any(|e| e.contains("ToolCallStart")),
            "expected ToolCallStart event"
        );
        assert!(
            events.iter().any(|e| e.contains("ToolCallResult")),
            "expected ToolCallResult event"
        );
        assert!(
            events.iter().any(|e| e.contains("TurnEnd")),
            "expected TurnEnd event"
        );
    }

    #[test]
    fn resolve_system_ref() {
        let mut store = MockStore::new();
        store.set("system", Value::String("You are helpful.".into()));
        let refs = vec![ContextRef::System { path: "system".into() }];
        let resolved = resolve_refs(&mut store, &refs).unwrap();
        assert_eq!(resolved.system, "You are helpful.");
    }

    #[test]
    fn resolve_history_ref_with_window() {
        let mut store = MockStore::new();
        store.set(
            "history/messages",
            structfs_serde_store::json_to_value(serde_json::json!([
                {"role": "user", "content": "a"},
                {"role": "assistant", "content": [{"type": "text", "text": "b"}]},
                {"role": "user", "content": "c"},
            ])),
        );
        let refs = vec![ContextRef::History { path: "history/messages".into(), last: Some(2) }];
        let resolved = resolve_refs(&mut store, &refs).unwrap();
        assert_eq!(resolved.messages.len(), 2);
    }

    #[test]
    fn resolve_history_ref_no_window() {
        let mut store = MockStore::new();
        store.set(
            "history/messages",
            structfs_serde_store::json_to_value(serde_json::json!([{"role": "user", "content": "a"}])),
        );
        let refs = vec![ContextRef::History { path: "history/messages".into(), last: None }];
        let resolved = resolve_refs(&mut store, &refs).unwrap();
        assert_eq!(resolved.messages.len(), 1);
    }

    #[test]
    fn resolve_tools_ref_with_only() {
        let mut store = MockStore::new();
        store.set(
            "tools/schemas",
            structfs_serde_store::json_to_value(serde_json::json!([
                {"name": "read_file", "description": "read", "input_schema": {}},
                {"name": "shell", "description": "run", "input_schema": {}},
                {"name": "write_file", "description": "write", "input_schema": {}},
            ])),
        );
        let refs = vec![ContextRef::Tools {
            path: "tools/schemas".into(),
            only: Some(vec!["read_file".into(), "shell".into()]),
            except: None,
        }];
        let resolved = resolve_refs(&mut store, &refs).unwrap();
        assert_eq!(resolved.tools.len(), 2);
    }

    #[test]
    fn resolve_tools_ref_with_except() {
        let mut store = MockStore::new();
        store.set(
            "tools/schemas",
            structfs_serde_store::json_to_value(serde_json::json!([
                {"name": "read_file", "description": "read", "input_schema": {}},
                {"name": "shell", "description": "run", "input_schema": {}},
            ])),
        );
        let refs = vec![ContextRef::Tools {
            path: "tools/schemas".into(),
            only: None,
            except: Some(vec!["shell".into()]),
        }];
        let resolved = resolve_refs(&mut store, &refs).unwrap();
        assert_eq!(resolved.tools.len(), 1);
        assert_eq!(resolved.tools[0].name, "read_file");
    }

    #[test]
    fn resolve_raw_ref() {
        let mut store = MockStore::new();
        let refs = vec![ContextRef::Raw { content: "Extra instructions.".into() }];
        let resolved = resolve_refs(&mut store, &refs).unwrap();
        assert_eq!(resolved.extra_content, vec!["Extra instructions."]);
    }

    #[test]
    fn resolve_multiple_refs() {
        let mut store = MockStore::new();
        store.set("system", Value::String("sys".into()));
        store.set("history/messages", structfs_serde_store::json_to_value(serde_json::json!([])));
        store.set("tools/schemas", structfs_serde_store::json_to_value(serde_json::json!([])));
        let refs = vec![
            ContextRef::System { path: "system".into() },
            ContextRef::History { path: "history/messages".into(), last: None },
            ContextRef::Tools { path: "tools/schemas".into(), only: None, except: None },
            ContextRef::Raw { content: "bonus".into() },
        ];
        let resolved = resolve_refs(&mut store, &refs).unwrap();
        assert_eq!(resolved.system, "sys");
        assert!(resolved.messages.is_empty());
        assert!(resolved.tools.is_empty());
        assert_eq!(resolved.extra_content, vec!["bonus"]);
    }

    #[test]
    fn context_ref_serde_roundtrip() {
        let refs = vec![
            ContextRef::System { path: "system".into() },
            ContextRef::History { path: "history/messages".into(), last: Some(20) },
            ContextRef::Tools { path: "tools/schemas".into(), only: Some(vec!["read_file".into()]), except: None },
            ContextRef::Raw { content: "hello".into() },
        ];
        let json = serde_json::to_value(&refs).unwrap();
        let roundtripped: Vec<ContextRef> = serde_json::from_value(json).unwrap();
        assert_eq!(roundtripped.len(), 4);
    }

    #[test]
    fn default_refs_has_three_entries() {
        let refs = default_refs();
        assert_eq!(refs.len(), 3);
    }

    #[test]
    fn complete_resolves_refs_and_sends() {
        let mut store = MockStore::new();
        store.set("system", Value::String("You are helpful.".into()));
        store.set(
            "history/messages",
            structfs_serde_store::json_to_value(serde_json::json!([
                {"role": "user", "content": "hi"}
            ])),
        );
        store.set(
            "tools/schemas",
            structfs_serde_store::json_to_value(serde_json::json!([])),
        );
        store.set("gate/defaults/model", Value::String("test-model".into()));
        store.set("gate/defaults/max_tokens", Value::Integer(100));
        store.push_completion_response(serde_json::json!([
            {"type": "text_delta", "text": "Hello!"},
            {"type": "message_stop"}
        ]));

        let refs = default_refs();
        let events = complete(&mut store, "test", &refs).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], StreamEvent::TextDelta(t) if t == "Hello!"));
    }

    #[test]
    fn complete_with_raw_ref_appends_to_system() {
        let mut store = MockStore::new();
        store.set("system", Value::String("Base prompt.".into()));
        store.set(
            "history/messages",
            structfs_serde_store::json_to_value(serde_json::json!([])),
        );
        store.set(
            "tools/schemas",
            structfs_serde_store::json_to_value(serde_json::json!([])),
        );
        store.set("gate/defaults/model", Value::String("test".into()));
        store.set("gate/defaults/max_tokens", Value::Integer(100));
        store.push_completion_response(serde_json::json!([
            {"type": "text_delta", "text": "ok"},
            {"type": "message_stop"}
        ]));

        let refs = vec![
            ContextRef::System {
                path: "system".into(),
            },
            ContextRef::History {
                path: "history/messages".into(),
                last: None,
            },
            ContextRef::Tools {
                path: "tools/schemas".into(),
                only: None,
                except: None,
            },
            ContextRef::Raw {
                content: "Extra instruction.".into(),
            },
        ];
        let events = complete(&mut store, "test", &refs).unwrap();
        assert!(!events.is_empty());

        // Verify the written request included extra content in system
        let written = store
            .appended
            .iter()
            .find(|(p, _)| p.contains("completions/complete"));
        assert!(written.is_some());
        let request_json = structfs_serde_store::value_to_json(written.unwrap().1.clone());
        let system = request_json.get("system").and_then(|v| v.as_str()).unwrap();
        assert!(system.contains("Base prompt."));
        assert!(system.contains("Extra instruction."));
    }
}

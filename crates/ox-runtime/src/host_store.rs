//! HostStore middleware — wraps a Namespace with effect interception.
//!
//! The HostStore sits between the Wasm agent and the StructFS namespace,
//! intercepting effectful paths (LLM completion, tool execution, event
//! emission) and delegating them to a [`HostEffects`] implementation
//! provided by the host (e.g. ox-cli).

use ox_context::Namespace;
use ox_kernel::{AgentEvent, CompletionRequest, StreamEvent, ToolCall};
use std::collections::BTreeMap;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer, path};

/// Callback trait for operations requiring host-side effects.
///
/// The host (ox-cli) implements this to provide LLM transport,
/// tool execution, and event emission.
pub trait HostEffects: Send {
    /// Perform LLM completion.
    /// Returns (stream_events, input_tokens, output_tokens).
    fn complete(
        &mut self,
        request: &CompletionRequest,
    ) -> Result<(Vec<StreamEvent>, u32, u32), String>;

    /// Execute a tool call. The host handles policy enforcement.
    fn execute_tool(&mut self, call: &ToolCall) -> Result<String, String>;

    /// Emit an agent event to the TUI or other observer.
    fn emit_event(&mut self, event: AgentEvent);
}

/// A minimal in-memory key-value store for internal use.
///
/// Used by HostStore to store tool results at `tool_results/{id}`.
struct SimpleStore {
    data: BTreeMap<String, Value>,
}

impl SimpleStore {
    fn new() -> Self {
        Self {
            data: BTreeMap::new(),
        }
    }
}

impl Reader for SimpleStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = from.to_string();
        Ok(self.data.get(&key).map(|v| Record::parsed(v.clone())))
    }
}

impl Writer for SimpleStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let value = match data {
            Record::Parsed(v) => v,
            _ => {
                return Err(StoreError::store(
                    "simple_store",
                    "write",
                    "expected parsed record",
                ));
            }
        };
        self.data.insert(to.to_string(), value);
        Ok(to.clone())
    }
}

/// StructFS middleware wrapping Namespace with effect interception.
///
/// Reads and writes to certain paths are intercepted and routed to
/// the [`HostEffects`] handler instead of the underlying namespace:
///
/// - **`gate/complete`** (write) — triggers LLM completion
/// - **`gate/response`** (read) — returns pending stream events
/// - **`tools/execute`** (write) — executes a tool call
/// - **`events/emit`** (write) — emits an agent event
pub struct HostStore<E: HostEffects> {
    /// The underlying namespace for non-effectful operations.
    pub namespace: Namespace,
    /// The effects handler.
    pub effects: E,
    /// Pending stream events from the most recent completion.
    pending_events: Option<Vec<StreamEvent>>,
}

impl<E: HostEffects> HostStore<E> {
    /// Create a new HostStore wrapping the given namespace and effects handler.
    ///
    /// Automatically mounts a `tool_results` store for storing tool outputs.
    pub fn new(mut namespace: Namespace, effects: E) -> Self {
        namespace.mount("tool_results", Box::new(SimpleStore::new()));
        Self {
            namespace,
            effects,
            pending_events: None,
        }
    }

    /// Handle a read operation, intercepting effectful paths.
    pub fn handle_read(&mut self, path: &Path) -> Result<Option<Record>, StoreError> {
        if path == &path!("gate/response") {
            return self.read_gate_response();
        }

        // Delegate everything else to the namespace.
        self.namespace.read(path)
    }

    /// Handle a write operation, intercepting effectful paths.
    pub fn handle_write(&mut self, path: &Path, data: Record) -> Result<Path, StoreError> {
        let prefix = if path.is_empty() {
            ""
        } else {
            path.components[0].as_str()
        };

        match prefix {
            "gate" if path == &path!("gate/complete") => self.write_gate_complete(data),
            "tools" if path == &path!("tools/execute") => self.write_tools_execute(data),
            "events" if path == &path!("events/emit") => self.write_events_emit(data),
            _ => self.namespace.write(path, data),
        }
    }

    // -- Effectful path handlers -----------------------------------------------

    fn read_gate_response(&mut self) -> Result<Option<Record>, StoreError> {
        let events = match self.pending_events.take() {
            Some(events) => events,
            None => return Ok(None),
        };

        let json_events: Vec<serde_json::Value> = events.iter().map(stream_event_to_json).collect();
        let json_value = serde_json::Value::Array(json_events);
        let value = structfs_serde_store::json_to_value(json_value);
        Ok(Some(Record::parsed(value)))
    }

    fn write_gate_complete(&mut self, data: Record) -> Result<Path, StoreError> {
        let request: CompletionRequest = record_to_typed(data, "gate", "complete")?;

        let (events, _input_tokens, _output_tokens) = self
            .effects
            .complete(&request)
            .map_err(|e| StoreError::store("gate", "complete", e))?;

        self.effects.emit_event(AgentEvent::TurnStart);
        self.pending_events = Some(events);

        Ok(path!("gate/response"))
    }

    fn write_tools_execute(&mut self, data: Record) -> Result<Path, StoreError> {
        let call: ToolCall = record_to_typed(data, "tools", "execute")?;

        self.effects.emit_event(AgentEvent::ToolCallStart {
            name: call.name.clone(),
        });

        let result = self
            .effects
            .execute_tool(&call)
            .map_err(|e| StoreError::store("tools", "execute", e))?;

        self.effects.emit_event(AgentEvent::ToolCallResult {
            name: call.name.clone(),
            result: result.clone(),
        });

        // Write the result into the namespace at tool_results/{call.id}
        let result_path = Path::parse(&format!("tool_results/{}", call.id))
            .map_err(|e| StoreError::store("tools", "execute", e.to_string()))?;
        let result_record = Record::parsed(Value::String(result));
        self.namespace.write(&result_path, result_record)?;

        Ok(result_path)
    }

    fn write_events_emit(&mut self, data: Record) -> Result<Path, StoreError> {
        let value = data
            .as_value()
            .ok_or_else(|| StoreError::store("events", "emit", "expected parsed record"))?
            .clone();
        let json = structfs_serde_store::value_to_json(value);
        let event =
            json_to_agent_event(json).map_err(|e| StoreError::store("events", "emit", e))?;

        self.effects.emit_event(event);
        Ok(path!("events/emit"))
    }
}

// -- Helper: deserialize a Record into a typed value --------------------------

fn record_to_typed<T: serde::de::DeserializeOwned>(
    data: Record,
    store: &'static str,
    op: &'static str,
) -> Result<T, StoreError> {
    let value = match data {
        Record::Parsed(v) => v,
        _ => return Err(StoreError::store(store, op, "expected parsed record")),
    };
    structfs_serde_store::from_value(value).map_err(|e| StoreError::store(store, op, e.to_string()))
}

// -- Manual StreamEvent <-> JSON (no serde derives on StreamEvent) ------------

fn stream_event_to_json(event: &StreamEvent) -> serde_json::Value {
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

// -- Manual JSON -> AgentEvent (no serde derives on AgentEvent) ---------------

fn json_to_agent_event(json: serde_json::Value) -> Result<AgentEvent, String> {
    let obj = json
        .as_object()
        .ok_or_else(|| "expected JSON object for AgentEvent".to_string())?;
    let event_type = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'type' field in AgentEvent".to_string())?;

    match event_type {
        "turn_start" => Ok(AgentEvent::TurnStart),
        "text_delta" => {
            let text = obj
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or("missing 'text' for text_delta")?;
            Ok(AgentEvent::TextDelta(text.to_string()))
        }
        "tool_call_start" => {
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("missing 'name' for tool_call_start")?;
            Ok(AgentEvent::ToolCallStart {
                name: name.to_string(),
            })
        }
        "tool_call_result" => {
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("missing 'name' for tool_call_result")?;
            let result = obj
                .get("result")
                .and_then(|v| v.as_str())
                .ok_or("missing 'result' for tool_call_result")?;
            Ok(AgentEvent::ToolCallResult {
                name: name.to_string(),
                result: result.to_string(),
            })
        }
        "turn_end" => Ok(AgentEvent::TurnEnd),
        "error" => {
            let msg = obj
                .get("message")
                .and_then(|v| v.as_str())
                .ok_or("missing 'message' for error")?;
            Ok(AgentEvent::Error(msg.to_string()))
        }
        other => Err(format!("unknown AgentEvent type: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_context::{ModelProvider, SystemProvider, ToolsProvider};
    use ox_history::HistoryProvider;
    struct MockEffects {
        complete_calls: usize,
        tool_calls: Vec<String>,
        events: Vec<String>,
    }

    impl MockEffects {
        fn new() -> Self {
            Self {
                complete_calls: 0,
                tool_calls: vec![],
                events: vec![],
            }
        }
    }

    impl HostEffects for MockEffects {
        fn complete(
            &mut self,
            _request: &CompletionRequest,
        ) -> Result<(Vec<StreamEvent>, u32, u32), String> {
            self.complete_calls += 1;
            Ok((
                vec![
                    StreamEvent::TextDelta("Hello!".into()),
                    StreamEvent::MessageStop,
                ],
                10,
                5,
            ))
        }

        fn execute_tool(&mut self, call: &ToolCall) -> Result<String, String> {
            self.tool_calls.push(call.name.clone());
            Ok(format!("result for {}", call.name))
        }

        fn emit_event(&mut self, event: AgentEvent) {
            self.events.push(format!("{:?}", event));
        }
    }

    fn make_namespace() -> Namespace {
        let mut ns = Namespace::new();
        ns.mount(
            "system",
            Box::new(SystemProvider::new("You are a test agent.".into())),
        );
        ns.mount("history", Box::new(HistoryProvider::new()));
        ns.mount("tools", Box::new(ToolsProvider::new(vec![])));
        ns.mount(
            "model",
            Box::new(ModelProvider::new("test-model".into(), 1024)),
        );
        ns
    }

    #[test]
    fn read_delegates_to_namespace() {
        let ns = make_namespace();
        let mut store = HostStore::new(ns, MockEffects::new());

        // Reading system prompt should delegate to namespace
        let result = store.handle_read(&path!("system")).unwrap();
        assert!(result.is_some());
        let value = result.unwrap().as_value().cloned().unwrap();
        assert_eq!(value, Value::String("You are a test agent.".into()));
    }

    #[test]
    fn write_gate_complete_calls_effects() {
        let ns = make_namespace();
        let mut store = HostStore::new(ns, MockEffects::new());

        let request = CompletionRequest {
            model: "test".into(),
            max_tokens: 100,
            system: "test".into(),
            messages: vec![],
            tools: vec![],
            stream: false,
        };
        let value = structfs_serde_store::to_value(&request).unwrap();
        let record = Record::parsed(value);

        let result_path = store.handle_write(&path!("gate/complete"), record).unwrap();
        assert_eq!(result_path.to_string(), "gate/response");
        assert_eq!(store.effects.complete_calls, 1);
        // Should have emitted TurnStart
        assert!(store.effects.events.iter().any(|e| e.contains("TurnStart")));
    }

    #[test]
    fn read_gate_response_returns_pending_events() {
        let ns = make_namespace();
        let mut store = HostStore::new(ns, MockEffects::new());

        // First, trigger a completion to populate pending_events
        let request = CompletionRequest {
            model: "test".into(),
            max_tokens: 100,
            system: "test".into(),
            messages: vec![],
            tools: vec![],
            stream: false,
        };
        let value = structfs_serde_store::to_value(&request).unwrap();
        store
            .handle_write(&path!("gate/complete"), Record::parsed(value))
            .unwrap();

        // Now read the response
        let result = store.handle_read(&path!("gate/response")).unwrap();
        assert!(result.is_some());

        // Second read should return None (events consumed)
        let result2 = store.handle_read(&path!("gate/response")).unwrap();
        assert!(result2.is_none());
    }

    #[test]
    fn write_tools_execute_calls_effects() {
        let ns = make_namespace();
        let mut store = HostStore::new(ns, MockEffects::new());

        let call = ToolCall {
            id: "call_001".into(),
            name: "echo".into(),
            input: serde_json::json!({"text": "hello"}),
        };
        let value = structfs_serde_store::to_value(&call).unwrap();
        let record = Record::parsed(value);

        let result_path = store.handle_write(&path!("tools/execute"), record).unwrap();

        // Should have called the tool
        assert_eq!(store.effects.tool_calls, vec!["echo"]);
        // Should have emitted ToolCallStart + ToolCallResult
        assert!(
            store
                .effects
                .events
                .iter()
                .any(|e| e.contains("ToolCallStart"))
        );
        assert!(
            store
                .effects
                .events
                .iter()
                .any(|e| e.contains("ToolCallResult"))
        );

        // Result should be written to namespace
        let stored = store.handle_read(&result_path).unwrap();
        assert!(stored.is_some());
        assert_eq!(
            stored.unwrap().as_value(),
            Some(&Value::String("result for echo".into()))
        );
    }

    #[test]
    fn write_events_emit_calls_effects() {
        let ns = make_namespace();
        let mut store = HostStore::new(ns, MockEffects::new());

        let event_json = serde_json::json!({
            "type": "text_delta",
            "text": "streaming chunk",
        });
        let value = structfs_serde_store::json_to_value(event_json);
        let record = Record::parsed(value);

        let result_path = store.handle_write(&path!("events/emit"), record).unwrap();
        assert_eq!(result_path.to_string(), "events/emit");
        assert!(store.effects.events.iter().any(|e| e.contains("TextDelta")));
    }

    #[test]
    fn write_non_effectful_delegates_to_namespace() {
        let ns = make_namespace();
        let mut store = HostStore::new(ns, MockEffects::new());

        // Writing to history/append should delegate to namespace
        let msg = serde_json::json!({
            "role": "user",
            "content": "hello",
        });
        let value = structfs_serde_store::json_to_value(msg);
        let record = Record::parsed(value);
        let result = store.handle_write(&path!("history/append"), record);
        assert!(result.is_ok());
    }
}

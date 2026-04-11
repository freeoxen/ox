//! CompletionModule — wraps [`GateStore`] for StructFS-native LLM transport.
//!
//! Preserves all ox-gate infrastructure: providers, accounts, codecs, config
//! handle, catalogs, snapshots, and usage tracking.  Delegates Reader/Writer
//! to the inner GateStore.
//!
//! When a [`CompletionTransport`] is injected via [`CompletionModule::set_transport`],
//! writes to `complete/{account}` execute an end-to-end LLM completion and store
//! the result for subsequent reads at `complete/{account}/response`.

use crate::ToolSchemaEntry;
use ox_gate::GateStore;
use ox_kernel::{CompletionRequest, StreamEvent};
use std::collections::BTreeMap;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

/// Transport for sending completion requests to an LLM.
///
/// The host injects this at construction. On native: reqwest HTTP.
/// On wasm: fetch API. The CompletionModule doesn't know or care.
pub trait CompletionTransport: Send + Sync {
    /// Send a completion request, return stream events.
    /// The callback receives events as they stream in (for real-time TUI updates).
    fn send(
        &self,
        request: &CompletionRequest,
        on_event: &dyn Fn(&StreamEvent),
    ) -> Result<(Vec<StreamEvent>, u32, u32), String>;
}

/// Stored result of a completion execution.
#[derive(Debug, Clone)]
pub struct CompletionResult {
    /// All stream events from the completion.
    pub events: Vec<StreamEvent>,
    /// Input tokens consumed.
    pub input_tokens: u32,
    /// Output tokens generated.
    pub output_tokens: u32,
}

/// Thin wrapper around [`GateStore`] that exposes it as a StructFS store
/// while adding tool-schema generation hooks for the unified ToolStore.
///
/// With a transport injected, writes to `complete/{account}` trigger end-to-end
/// LLM completions. Results are readable at `complete/{account}/response`.
pub struct CompletionModule {
    gate: GateStore,
    transport: Option<Box<dyn CompletionTransport>>,
    /// Stored results keyed by account name.
    results: BTreeMap<String, CompletionResult>,
}

impl CompletionModule {
    pub fn new(gate: GateStore) -> Self {
        Self {
            gate,
            transport: None,
            results: BTreeMap::new(),
        }
    }

    /// Inject a completion transport for end-to-end LLM execution.
    pub fn set_transport(&mut self, transport: Box<dyn CompletionTransport>) {
        self.transport = Some(transport);
    }

    /// Builder-style transport injection.
    pub fn with_transport(mut self, transport: Box<dyn CompletionTransport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Returns `true` if a transport has been injected.
    pub fn has_transport(&self) -> bool {
        self.transport.is_some()
    }

    /// Execute a completion via the injected transport.
    ///
    /// `account` identifies which stored result slot to use.
    /// `request` is the full completion request.
    /// `on_event` receives streaming events as they arrive.
    ///
    /// Stores the result for later retrieval via `result()`.
    pub fn execute(
        &mut self,
        account: &str,
        request: &CompletionRequest,
        on_event: &dyn Fn(&StreamEvent),
    ) -> Result<&CompletionResult, String> {
        let transport = self
            .transport
            .as_ref()
            .ok_or_else(|| "no CompletionTransport injected".to_string())?;
        let (events, input_tokens, output_tokens) = transport.send(request, on_event)?;
        let result = CompletionResult {
            events,
            input_tokens,
            output_tokens,
        };
        self.results.insert(account.to_string(), result);
        Ok(self.results.get(account).unwrap())
    }

    /// Retrieve the last completion result for an account.
    pub fn result(&self, account: &str) -> Option<&CompletionResult> {
        self.results.get(account)
    }

    /// Read a sub-path from the underlying GateStore.
    ///
    /// `sub` is a `/`-separated path string (e.g. `"defaults/account"`).
    pub fn read_gate(&mut self, sub: &str) -> Option<Value> {
        let path = Path::parse(sub).ok()?;
        let record = self.gate.read(&path).ok()??;
        record.as_value().cloned()
    }

    /// Write a sub-path to the underlying GateStore.
    ///
    /// `sub` is a `/`-separated path string (e.g. `"defaults/model"`).
    pub fn write_gate(&mut self, sub: &str, value: Value) -> Result<(), String> {
        let path = Path::parse(sub).map_err(|e| e.to_string())?;
        self.gate
            .write(&path, Record::Parsed(value))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Tool schemas for the completion module.
    ///
    /// Returns a schema for the `complete` tool — an LLM-callable tool that
    /// fires a completion with context references.
    pub fn schemas(&self) -> Vec<ToolSchemaEntry> {
        vec![ToolSchemaEntry {
            wire_name: "complete".to_string(),
            internal_path: "completions/complete".to_string(),
            description: "Fire an LLM completion with specified context references. \
                Use this to delegate sub-tasks to a model with custom context."
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "account": {
                        "type": "string",
                        "description": "Account name for the completion (e.g. 'anthropic', 'openai')"
                    },
                    "refs": {
                        "type": "array",
                        "description": "Context references to include in the prompt",
                        "items": {
                            "type": "object",
                            "properties": {
                                "type": {
                                    "type": "string",
                                    "enum": ["system", "history", "tools", "raw"],
                                    "description": "Reference type"
                                },
                                "path": {
                                    "type": "string",
                                    "description": "Namespace path to read from (for system, history, tools)"
                                },
                                "last": {
                                    "type": "integer",
                                    "description": "For history: only include the last N messages"
                                },
                                "only": {
                                    "type": "array",
                                    "items": {"type": "string"},
                                    "description": "For tools: only include these tool names"
                                },
                                "except": {
                                    "type": "array",
                                    "items": {"type": "string"},
                                    "description": "For tools: exclude these tool names"
                                },
                                "content": {
                                    "type": "string",
                                    "description": "For raw: literal content to include"
                                }
                            },
                            "required": ["type"]
                        }
                    }
                },
                "required": ["account", "refs"]
            }),
        }]
    }

    /// Mutable access to the inner GateStore.
    pub fn gate_mut(&mut self) -> &mut GateStore {
        &mut self.gate
    }

    /// Shared access to the inner GateStore.
    pub fn gate(&self) -> &GateStore {
        &self.gate
    }
}

/// StructFS Reader — handles `complete/{account}/response` for completion results,
/// delegates everything else to GateStore.
impl Reader for CompletionModule {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        // complete/{account}/response → return stored stream events as JSON array
        if from.len() >= 2 && from.components[0] == "complete" {
            let account = from.components[1].as_str();
            if from.len() >= 3 && from.components[2] == "response" {
                return Ok(self.results.get(account).map(|r| {
                    let json_events: Vec<serde_json::Value> =
                        r.events.iter().map(stream_event_to_json).collect();
                    let json_value = serde_json::Value::Array(json_events);
                    let value = structfs_serde_store::json_to_value(json_value);
                    Record::parsed(value)
                }));
            }
            // complete/{account} — return metadata about the result
            return Ok(self.results.get(account).map(|r| {
                let mut map = BTreeMap::new();
                map.insert(
                    "input_tokens".to_string(),
                    Value::Integer(r.input_tokens as i64),
                );
                map.insert(
                    "output_tokens".to_string(),
                    Value::Integer(r.output_tokens as i64),
                );
                map.insert(
                    "event_count".to_string(),
                    Value::Integer(r.events.len() as i64),
                );
                Record::parsed(Value::Map(map))
            }));
        }
        self.gate.read(from)
    }
}

/// StructFS Writer — handles `complete/{account}` for triggering completions,
/// delegates everything else to GateStore.
impl Writer for CompletionModule {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        // complete/{account} → execute completion via transport
        if to.len() >= 2 && to.components[0] == "complete" {
            let account = to.components[1].clone();

            // The written data must be a CompletionRequest serialized as a Value
            let value = data.as_value().ok_or_else(|| {
                StoreError::store(
                    "CompletionModule",
                    "write",
                    "expected Parsed record for completion request",
                )
            })?;
            let json = structfs_serde_store::value_to_json(value.clone());
            let request: CompletionRequest = serde_json::from_value(json).map_err(|e| {
                StoreError::store(
                    "CompletionModule",
                    "write",
                    format!("invalid CompletionRequest: {e}"),
                )
            })?;

            self.execute(&account, &request, &|_| {})
                .map_err(|e| StoreError::store("CompletionModule", "write", e))?;

            return Ok(to.clone());
        }
        self.gate.write(to, data)
    }
}

// -- Manual StreamEvent -> JSON (no serde derives on StreamEvent) -------------

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Mock transport that records calls and returns canned responses.
    struct MockTransport {
        response: Mutex<(Vec<StreamEvent>, u32, u32)>,
        calls: Arc<Mutex<Vec<CompletionRequest>>>,
    }

    impl MockTransport {
        fn new(events: Vec<StreamEvent>, input_tokens: u32, output_tokens: u32) -> Self {
            Self {
                response: Mutex::new((events, input_tokens, output_tokens)),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        #[allow(dead_code)]
        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl CompletionTransport for MockTransport {
        fn send(
            &self,
            request: &CompletionRequest,
            on_event: &dyn Fn(&StreamEvent),
        ) -> Result<(Vec<StreamEvent>, u32, u32), String> {
            self.calls.lock().unwrap().push(request.clone());
            let resp = self.response.lock().unwrap().clone();
            for event in &resp.0 {
                on_event(event);
            }
            Ok(resp)
        }
    }

    fn sample_request() -> CompletionRequest {
        CompletionRequest {
            model: "test-model".into(),
            max_tokens: 100,
            system: "test".into(),
            messages: vec![serde_json::json!({"role": "user", "content": "hello"})],
            tools: vec![],
            stream: true,
        }
    }

    #[test]
    fn schemas_returns_complete_tool() {
        let gate = GateStore::new();
        let module = CompletionModule::new(gate);
        let schemas = module.schemas();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].wire_name, "complete");
    }

    #[test]
    fn complete_schema_has_account_and_refs() {
        let gate = GateStore::new();
        let module = CompletionModule::new(gate);
        let schemas = module.schemas();
        let input = &schemas[0].input_schema;
        let props = input.get("properties").unwrap();
        assert!(props.get("account").is_some());
        assert!(props.get("refs").is_some());
        let required = input.get("required").unwrap().as_array().unwrap();
        assert!(required.contains(&serde_json::json!("account")));
        assert!(required.contains(&serde_json::json!("refs")));
    }

    #[test]
    fn gate_store_accessible_by_path() {
        let gate = GateStore::new();
        let mut module = CompletionModule::new(gate);
        let result = module.read_gate("defaults/account");
        assert!(result.is_some());
    }

    #[test]
    fn reader_delegates_to_gate() {
        let gate = GateStore::new();
        let mut module = CompletionModule::new(gate);
        let result = module.read(&structfs_core_store::path!("defaults/account"));
        assert!(result.is_ok());
    }

    #[test]
    fn has_transport_false_by_default() {
        let module = CompletionModule::new(GateStore::new());
        assert!(!module.has_transport());
    }

    #[test]
    fn set_transport_makes_it_available() {
        let mut module = CompletionModule::new(GateStore::new());
        let transport = MockTransport::new(vec![], 0, 0);
        module.set_transport(Box::new(transport));
        assert!(module.has_transport());
    }

    #[test]
    fn with_transport_builder() {
        let transport = MockTransport::new(vec![], 0, 0);
        let module = CompletionModule::new(GateStore::new()).with_transport(Box::new(transport));
        assert!(module.has_transport());
    }

    #[test]
    fn execute_calls_transport_and_stores_result() {
        let events = vec![
            StreamEvent::TextDelta("Hi".into()),
            StreamEvent::MessageStop,
        ];
        let transport = MockTransport::new(events.clone(), 10, 5);
        let mut module =
            CompletionModule::new(GateStore::new()).with_transport(Box::new(transport));

        let result = module.execute("anthropic", &sample_request(), &|_| {});
        assert!(result.is_ok());
        let r = result.unwrap();
        assert_eq!(r.events.len(), 2);
        assert_eq!(r.input_tokens, 10);
        assert_eq!(r.output_tokens, 5);
    }

    #[test]
    fn execute_without_transport_errors() {
        let mut module = CompletionModule::new(GateStore::new());
        let result = module.execute("anthropic", &sample_request(), &|_| {});
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no CompletionTransport"));
    }

    #[test]
    fn result_returns_stored_completion() {
        let events = vec![StreamEvent::TextDelta("response".into())];
        let transport = MockTransport::new(events, 42, 17);
        let mut module =
            CompletionModule::new(GateStore::new()).with_transport(Box::new(transport));

        assert!(module.result("anthropic").is_none());
        module
            .execute("anthropic", &sample_request(), &|_| {})
            .unwrap();
        let r = module.result("anthropic").unwrap();
        assert_eq!(r.input_tokens, 42);
        assert_eq!(r.output_tokens, 17);
    }

    #[test]
    fn on_event_callback_invoked_during_execute() {
        let events = vec![
            StreamEvent::TextDelta("a".into()),
            StreamEvent::TextDelta("b".into()),
        ];
        let transport = MockTransport::new(events, 0, 0);
        let mut module =
            CompletionModule::new(GateStore::new()).with_transport(Box::new(transport));

        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = received.clone();
        module
            .execute("test", &sample_request(), &move |evt| {
                received_clone.lock().unwrap().push(format!("{:?}", evt));
            })
            .unwrap();

        assert_eq!(received.lock().unwrap().len(), 2);
    }

    #[test]
    fn writer_triggers_completion_on_complete_path() {
        let events = vec![StreamEvent::TextDelta("done".into())];
        let transport = MockTransport::new(events, 8, 3);
        let mut module =
            CompletionModule::new(GateStore::new()).with_transport(Box::new(transport));

        let request = sample_request();
        let json = serde_json::to_value(&request).unwrap();
        let value = structfs_serde_store::json_to_value(json);

        let path = structfs_core_store::path!("complete/anthropic");
        let result = module.write(&path, Record::parsed(value));
        assert!(result.is_ok());

        // Result should now be stored
        let r = module.result("anthropic").unwrap();
        assert_eq!(r.input_tokens, 8);
        assert_eq!(r.output_tokens, 3);
    }

    #[test]
    fn reader_returns_response_for_complete_path() {
        let events = vec![StreamEvent::TextDelta("x".into())];
        let transport = MockTransport::new(events, 20, 10);
        let mut module =
            CompletionModule::new(GateStore::new()).with_transport(Box::new(transport));

        // Execute a completion first
        module
            .execute("myaccount", &sample_request(), &|_| {})
            .unwrap();

        // Read the response — should return serialized stream events array
        let path = structfs_core_store::path!("complete/myaccount/response");
        let record = module.read(&path).unwrap().unwrap();
        let value = record.as_value().unwrap();
        let json = structfs_serde_store::value_to_json(value.clone());
        let arr = json.as_array().expect("expected JSON array of events");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "text_delta");
        assert_eq!(arr[0]["text"], "x");
    }

    #[test]
    fn reader_returns_metadata_for_complete_account_path() {
        let events = vec![StreamEvent::TextDelta("x".into())];
        let transport = MockTransport::new(events, 20, 10);
        let mut module =
            CompletionModule::new(GateStore::new()).with_transport(Box::new(transport));

        // Execute a completion first
        module
            .execute("myaccount", &sample_request(), &|_| {})
            .unwrap();

        // Read complete/{account} (without /response) — returns metadata
        let path = structfs_core_store::path!("complete/myaccount");
        let record = module.read(&path).unwrap().unwrap();
        let value = record.as_value().unwrap();
        match value {
            Value::Map(map) => {
                assert_eq!(map.get("input_tokens"), Some(&Value::Integer(20)));
                assert_eq!(map.get("output_tokens"), Some(&Value::Integer(10)));
                assert_eq!(map.get("event_count"), Some(&Value::Integer(1)));
            }
            other => panic!("expected Map, got {:?}", other),
        }
    }

    #[test]
    fn reader_returns_none_for_missing_account_response() {
        let mut module = CompletionModule::new(GateStore::new());
        let path = structfs_core_store::path!("complete/nonexistent/response");
        let result = module.read(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn writer_delegates_non_complete_paths_to_gate() {
        let mut module = CompletionModule::new(GateStore::new());
        // GateStore accepts writes to its known paths like defaults/model
        let path = structfs_core_store::path!("defaults/model");
        let result = module.write(&path, Record::parsed(Value::String("gpt-4o".into())));
        assert!(result.is_ok());
    }

    #[test]
    fn writer_errors_without_transport_on_complete_path() {
        let mut module = CompletionModule::new(GateStore::new());
        let request = sample_request();
        let json = serde_json::to_value(&request).unwrap();
        let value = structfs_serde_store::json_to_value(json);

        let path = structfs_core_store::path!("complete/anthropic");
        let result = module.write(&path, Record::parsed(value));
        assert!(result.is_err());
    }
}

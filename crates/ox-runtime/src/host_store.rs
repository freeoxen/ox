//! HostStore middleware — wraps a backend with effect interception.
//!
//! The HostStore sits between the Wasm agent and the StructFS backend,
//! intercepting effectful paths (event emission) and routing tool
//! operations through [`HostEffects::tool_store()`]. LLM completion and
//! tool execution are handled by the ToolStore — the host provides
//! event emission and tool dispatch via [`HostEffects`].

use ox_kernel::AgentEvent;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Store, Value, Writer, path};

/// Callback trait for operations requiring host-side effects.
///
/// The host (ox-cli) implements this to provide event emission to
/// the TUI or other observer, and to route tool operations through
/// a single [`ox_tools::ToolStore`].
pub trait HostEffects: Send {
    /// Emit an agent event to the TUI or other observer.
    fn emit_event(&mut self, event: AgentEvent);

    /// Access the tool store for tool dispatch. Returns a `dyn Store` so the
    /// concrete type can be a bare `ToolStore` or a `PolicyStore<ToolStore, _>`.
    fn tool_store(&mut self) -> &mut dyn Store;
}

/// StructFS middleware wrapping a backend with effect interception.
///
/// Reads and writes to certain paths are intercepted and routed:
///
/// - **`tools/*`** (read/write) — routes to ToolStore via effects
/// - **`events/emit`** (write) — emits an agent event
/// - **`prompt`** (read) — synthesizes a CompletionRequest from backend stores
/// - everything else — delegates to the backend
pub struct HostStore<B: Reader + Writer + Send, E: HostEffects> {
    /// The underlying backend for non-effectful operations.
    pub backend: B,
    /// The effects handler.
    pub effects: E,
    /// Error stash — the guest writes here before returning a non-zero exit code
    /// so the host can read the error message back.
    error_stash: Option<String>,
}

impl<B: Reader + Writer + Send, E: HostEffects> HostStore<B, E> {
    /// Create a new HostStore wrapping the given backend and effects handler.
    pub fn new(backend: B, effects: E) -> Self {
        Self {
            backend,
            effects,
            error_stash: None,
        }
    }

    /// Handle a read operation, intercepting effectful paths.
    pub fn handle_read(&mut self, path: &Path) -> Result<Option<Record>, StoreError> {
        if path == &path!("prompt") {
            tracing::debug!(path = %path, "effectful read: prompt synthesis");
            return ox_context::synthesize_prompt(&mut self.backend);
        }

        // Route tools/* reads to ToolStore via effects.
        if !path.is_empty() && path.components[0] == "tools" {
            let sub = Path::from_components(path.components[1..].to_vec());
            return self.effects.tool_store().read(&sub);
        }

        // Error stash — guest writes tool_results/__error before returning non-zero.
        if path == &path!("tool_results/__error") {
            return Ok(self
                .error_stash
                .as_ref()
                .map(|s| Record::parsed(Value::String(s.clone()))));
        }

        // Delegate everything else to the backend.
        self.backend.read(path)
    }

    /// Handle a write operation, intercepting effectful paths.
    pub fn handle_write(&mut self, path: &Path, data: Record) -> Result<Path, StoreError> {
        let prefix = if path.is_empty() {
            ""
        } else {
            path.components[0].as_str()
        };

        match prefix {
            "tools" => {
                let sub = Path::from_components(path.components[1..].to_vec());
                self.effects.tool_store().write(&sub, data)
            }
            "events" if path == &path!("events/emit") => {
                tracing::debug!(path = %path, "effectful write: events/emit");
                self.write_events_emit(data)
            }
            "events" if path == &path!("events/log") => {
                if let Some(Value::String(line)) = data.as_value() {
                    tracing::info!(target: "ox_wasm", "{line}");
                }
                Ok(path.clone())
            }
            "tool_results" if path == &path!("tool_results/__error") => {
                // Error stash from guest
                if let Some(Value::String(s)) = data.as_value() {
                    self.error_stash = Some(s.clone());
                }
                Ok(path.clone())
            }
            _ => self.backend.write(path, data),
        }
    }

    // -- Effectful path handlers -----------------------------------------------

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
    use ox_context::{Namespace, SystemProvider};
    use ox_gate::GateStore;
    use ox_history::HistoryProvider;

    struct MockEffects {
        events: Vec<String>,
        tool_store: ox_tools::ToolStore,
    }

    impl MockEffects {
        fn new() -> Self {
            Self {
                events: vec![],
                tool_store: ox_tools::ToolStore::empty(),
            }
        }
    }

    impl HostEffects for MockEffects {
        fn emit_event(&mut self, event: AgentEvent) {
            self.events.push(format!("{:?}", event));
        }

        fn tool_store(&mut self) -> &mut dyn structfs_core_store::Store {
            &mut self.tool_store
        }
    }

    fn make_namespace() -> Namespace {
        let mut ns = Namespace::new();
        ns.mount(
            "system",
            Box::new(SystemProvider::new("You are a test agent.".into())),
        );
        ns.mount("history", Box::new(HistoryProvider::new()));
        ns.mount("tools", Box::new(ox_tools::ToolStore::empty()));
        ns.mount("gate", Box::new(GateStore::new()));
        ns.write(
            &structfs_core_store::path!("gate/defaults/model"),
            Record::parsed(Value::String("test-model".into())),
        )
        .unwrap();
        ns.write(
            &structfs_core_store::path!("gate/defaults/max_tokens"),
            Record::parsed(Value::Integer(1024)),
        )
        .unwrap();
        ns
    }

    #[test]
    fn read_delegates_to_namespace() {
        let ns = make_namespace();
        let mut store = HostStore::new(ns, MockEffects::new());

        let result = store.handle_read(&path!("system")).unwrap();
        assert!(result.is_some());
        let value = result.unwrap().as_value().cloned().unwrap();
        assert_eq!(value, Value::String("You are a test agent.".into()));
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

        let msg = serde_json::json!({
            "role": "user",
            "content": "hello",
        });
        let value = structfs_serde_store::json_to_value(msg);
        let record = Record::parsed(value);
        let result = store.handle_write(&path!("history/append"), record);
        assert!(result.is_ok());
    }

    fn make_tool_store() -> ox_tools::ToolStore {
        use ox_tools::completion::CompletionModule;
        use ox_tools::fs::FsModule;
        use ox_tools::os::OsModule;
        use ox_tools::sandbox::PermissivePolicy;
        use std::sync::Arc;

        let policy = Arc::new(PermissivePolicy);
        let workspace = std::path::PathBuf::from("/tmp/test-workspace");
        let executor = std::path::PathBuf::from("/nonexistent/ox-tool-exec");

        let fs = FsModule::new(workspace.clone(), executor.clone(), policy.clone());
        let os = OsModule::new(workspace, executor, policy);
        let completions = CompletionModule::new(ox_gate::GateStore::new());

        ox_tools::ToolStore::new(fs, os, completions)
    }

    #[test]
    fn tools_path_routes_to_tool_store_read() {
        let ns = make_namespace();
        let mut effects = MockEffects::new();
        effects.tool_store = make_tool_store();
        let mut store = HostStore::new(ns, effects);

        let result = store.handle_read(&path!("tools/schemas")).unwrap();
        assert!(result.is_some(), "expected schemas from ToolStore");
    }

    #[test]
    fn tools_path_routes_to_effects_tool_store() {
        let ns = make_namespace();
        let mut store = HostStore::new(ns, MockEffects::new());

        // Empty ToolStore still returns schemas (just empty list tools)
        let result = store.handle_read(&path!("tools/schemas"));
        assert!(result.is_ok());
    }

    #[test]
    fn read_prompt_synthesizes_from_backend() {
        let ns = make_namespace();
        let mut store = HostStore::new(ns, MockEffects::new());

        let msg = serde_json::json!({"role": "user", "content": "hello"});
        let value = structfs_serde_store::json_to_value(msg);
        store
            .handle_write(&path!("history/append"), Record::parsed(value))
            .unwrap();

        let result = store.handle_read(&path!("prompt")).unwrap();
        assert!(result.is_some());
        let json =
            structfs_serde_store::value_to_json(result.unwrap().as_value().cloned().unwrap());
        let request: ox_kernel::CompletionRequest = serde_json::from_value(json).unwrap();
        assert_eq!(request.model, "test-model");
        assert_eq!(request.system, "You are a test agent.");
        assert_eq!(request.messages.len(), 1);
    }

    #[test]
    fn error_stash_roundtrips() {
        let ns = make_namespace();
        let mut store = HostStore::new(ns, MockEffects::new());

        // Write error
        let record = Record::parsed(Value::String("something broke".into()));
        store
            .handle_write(&path!("tool_results/__error"), record)
            .unwrap();

        // Read it back
        let result = store
            .handle_read(&path!("tool_results/__error"))
            .unwrap()
            .unwrap();
        assert_eq!(
            result.as_value(),
            Some(&Value::String("something broke".into()))
        );
    }
}

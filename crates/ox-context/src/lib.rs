//! Namespace store router and concrete providers for the ox agent framework.
//!
//! This crate provides [`Namespace`], a virtual filesystem that routes reads
//! and writes to mounted [`Store`] implementations by path prefix — similar to
//! how a Unix VFS mounts devices at path prefixes.
//!
//! Two concrete providers are included:
//!
//! - [`SystemProvider`] — holds the system prompt string
//! - [`ToolsProvider`] — read-only snapshot of tool schemas
//!
//! Reading `path!("prompt")` from the namespace synthesizes a complete
//! [`CompletionRequest`] by collecting state from all mounted providers.

use ox_kernel::CompletionRequest;
use std::collections::BTreeMap;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Store, Value, Writer, path};
use structfs_serde_store::{to_value, value_to_json};

// ---------------------------------------------------------------------------
// Namespace — routes reads/writes to mounted stores by path prefix
// ---------------------------------------------------------------------------

/// A namespace routes reads and writes to mounted stores by path prefix.
///
/// Paths are split on the first component: `path!("history/messages")` routes
/// to the store mounted at `"history"` with sub-path `path!("messages")`.
///
/// The special path `"prompt"` is synthetic — it assembles a CompletionRequest
/// by reading from sibling stores (system, history, tools, gate).
pub struct Namespace {
    mounts: BTreeMap<String, Box<dyn Store>>,
}

impl Namespace {
    /// Create an empty namespace with no mounted stores.
    pub fn new() -> Self {
        Self {
            mounts: BTreeMap::new(),
        }
    }

    /// Mount a store at the given path prefix.
    ///
    /// Subsequent reads/writes to paths starting with `prefix` will be
    /// routed to this store. Replaces any previously mounted store at
    /// the same prefix.
    pub fn mount(&mut self, prefix: &str, store: Box<dyn Store>) {
        self.mounts.insert(prefix.to_string(), store);
    }

    fn synthesize_prompt(&mut self) -> Result<Option<Record>, StoreError> {
        synthesize_prompt(self)
    }
}

/// Synthesize a [`CompletionRequest`] by reading prompt components from `reader`.
///
/// Reads the following paths:
/// - `system` → system prompt string
/// - `history/messages` → conversation messages array
/// - `tools/schemas` → tool schema array
/// - `gate/defaults/model` → model identifier string
/// - `gate/defaults/max_tokens` → token limit integer
///
/// When `reader` is a [`Namespace`], each path routes to the appropriate mounted
/// store. This function exists as a standalone so it can be called with any
/// [`Reader`] — for example a broker-backed adapter in the agent worker bridge.
pub fn synthesize_prompt(reader: &mut dyn Reader) -> Result<Option<Record>, StoreError> {
    // Read system prompt
    let system_str = {
        let record = reader.read(&path!("system"))?.ok_or_else(|| {
            StoreError::store("synthesize_prompt", "read", "system store returned None")
        })?;
        match record {
            Record::Parsed(Value::String(s)) => s,
            _ => {
                return Err(StoreError::store(
                    "synthesize_prompt",
                    "read",
                    "expected string from system store",
                ));
            }
        }
    };

    // Read history messages
    let messages_json = {
        let record = reader.read(&path!("history/messages"))?.ok_or_else(|| {
            StoreError::store("synthesize_prompt", "read", "history store returned None")
        })?;
        match record {
            Record::Parsed(v) => value_to_json(v),
            _ => {
                return Err(StoreError::store(
                    "synthesize_prompt",
                    "read",
                    "expected parsed record from history",
                ));
            }
        }
    };

    // Read tool schemas
    let tools_json = {
        let record = reader.read(&path!("tools/schemas"))?.ok_or_else(|| {
            StoreError::store("synthesize_prompt", "read", "tools store returned None")
        })?;
        match record {
            Record::Parsed(v) => value_to_json(v),
            _ => {
                return Err(StoreError::store(
                    "synthesize_prompt",
                    "read",
                    "expected parsed record from tools",
                ));
            }
        }
    };

    // Read model ID
    let model_id = {
        let record = reader.read(&path!("gate/defaults/model"))?.ok_or_else(|| {
            StoreError::store(
                "synthesize_prompt",
                "read",
                "gate store returned None for defaults/model",
            )
        })?;
        match record {
            Record::Parsed(Value::String(s)) => s,
            _ => {
                return Err(StoreError::store(
                    "synthesize_prompt",
                    "read",
                    "expected string from gate store for defaults/model",
                ));
            }
        }
    };

    // Read max_tokens
    let max_tokens = {
        let record = reader
            .read(&path!("gate/defaults/max_tokens"))?
            .ok_or_else(|| {
                StoreError::store(
                    "synthesize_prompt",
                    "read",
                    "gate store returned None for defaults/max_tokens",
                )
            })?;
        match record {
            Record::Parsed(Value::Integer(n)) => n as u32,
            _ => {
                return Err(StoreError::store(
                    "synthesize_prompt",
                    "read",
                    "expected integer from gate store for defaults/max_tokens",
                ));
            }
        }
    };

    let messages: Vec<serde_json::Value> = serde_json::from_value(messages_json)
        .map_err(|e| StoreError::store("synthesize_prompt", "read", e.to_string()))?;
    let tools: Vec<ox_kernel::ToolSchema> = serde_json::from_value(tools_json)
        .map_err(|e| StoreError::store("synthesize_prompt", "read", e.to_string()))?;

    let request = CompletionRequest {
        model: model_id,
        max_tokens,
        system: system_str,
        messages,
        tools,
        stream: true,
    };

    let value = to_value(&request)
        .map_err(|e| StoreError::store("synthesize_prompt", "read", e.to_string()))?;
    Ok(Some(Record::parsed(value)))
}

impl Default for Namespace {
    fn default() -> Self {
        Self::new()
    }
}

impl Reader for Namespace {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        if from == &path!("prompt") {
            return self.synthesize_prompt();
        }

        let (prefix, sub) = split_path(from);
        if prefix.is_empty() {
            return Ok(None);
        }
        match self.mounts.get_mut(prefix) {
            Some(store) => store.read(&sub),
            None => Ok(None),
        }
    }
}

impl Writer for Namespace {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let (prefix, sub) = split_path(to);
        match self.mounts.get_mut(prefix) {
            Some(store) => store.write(&sub, data),
            None => Err(StoreError::NoRoute { path: to.clone() }),
        }
    }
}

/// Split a path into the first component (prefix) and the remaining sub-path.
fn split_path(path: &Path) -> (&str, Path) {
    if path.is_empty() {
        return ("", Path::from_components(vec![]));
    }
    let prefix = path.components[0].as_str();
    let sub = Path::from_components(path.components[1..].to_vec());
    (prefix, sub)
}

// ---------------------------------------------------------------------------
// Concrete stores: System, Tools
// ---------------------------------------------------------------------------

/// Provides the system prompt string.
pub struct SystemProvider {
    prompt: String,
}

impl SystemProvider {
    /// Create a new provider with the given system prompt.
    pub fn new(prompt: String) -> Self {
        Self { prompt }
    }
}

impl Reader for SystemProvider {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = if from.is_empty() {
            ""
        } else {
            from.components[0].as_str()
        };
        match key {
            "snapshot" => {
                let state = Value::String(self.prompt.clone());
                if from.components.len() >= 2 {
                    match from.components[1].as_str() {
                        "hash" => {
                            let hash = ox_kernel::snapshot::snapshot_hash(&state);
                            Ok(Some(Record::parsed(Value::String(hash))))
                        }
                        "state" => Ok(Some(Record::parsed(state))),
                        _ => Ok(None),
                    }
                } else {
                    Ok(Some(Record::parsed(ox_kernel::snapshot::snapshot_record(
                        state,
                    ))))
                }
            }
            _ => Ok(Some(Record::parsed(Value::String(self.prompt.clone())))),
        }
    }
}

impl Writer for SystemProvider {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let key = if to.is_empty() {
            ""
        } else {
            to.components[0].as_str()
        };
        match key {
            "snapshot" => {
                let value = match data {
                    Record::Parsed(v) => v,
                    _ => {
                        return Err(StoreError::store(
                            "system",
                            "write",
                            "expected parsed record",
                        ));
                    }
                };
                let state = if to.components.len() >= 2 && to.components[1].as_str() == "state" {
                    value
                } else {
                    ox_kernel::snapshot::extract_snapshot_state(value)
                        .map_err(|e| StoreError::store("system", "write", e))?
                };
                match state {
                    Value::String(s) => {
                        self.prompt = s;
                        Ok(to.clone())
                    }
                    _ => Err(StoreError::store(
                        "system",
                        "write",
                        "snapshot state must be a string",
                    )),
                }
            }
            _ => match data {
                Record::Parsed(Value::String(s)) => {
                    self.prompt = s;
                    Ok(Path::from_components(vec![]))
                }
                _ => Err(StoreError::store(
                    "system",
                    "write",
                    "expected string value",
                )),
            },
        }
    }
}

/// Provides tool schemas (read-only snapshot).
pub struct ToolsProvider {
    schemas: Vec<ox_kernel::ToolSchema>,
}

impl ToolsProvider {
    /// Create a new provider with the given tool schemas.
    pub fn new(schemas: Vec<ox_kernel::ToolSchema>) -> Self {
        Self { schemas }
    }
}

impl Reader for ToolsProvider {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = if from.is_empty() {
            ""
        } else {
            from.components[0].as_str()
        };
        match key {
            "" | "schemas" => {
                let value = to_value(&self.schemas)
                    .map_err(|e| StoreError::store("tools", "read", e.to_string()))?;
                Ok(Some(Record::parsed(value)))
            }
            _ => Ok(None),
        }
    }
}

impl Writer for ToolsProvider {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let key = if to.is_empty() {
            ""
        } else {
            to.components[0].as_str()
        };
        match key {
            "" | "schemas" => {
                let value = match data {
                    Record::Parsed(v) => v,
                    _ => {
                        return Err(StoreError::store(
                            "tools",
                            "write",
                            "expected parsed record",
                        ));
                    }
                };
                let schemas: Vec<ox_kernel::ToolSchema> =
                    structfs_serde_store::from_value(value)
                        .map_err(|e| StoreError::store("tools", "write", e.to_string()))?;
                self.schemas = schemas;
                Ok(to.clone())
            }
            _ => Err(StoreError::store(
                "tools",
                "write",
                format!("unknown write path: {to}"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;

    fn unwrap_value(record: Record) -> Value {
        match record {
            Record::Parsed(v) => v,
            _ => panic!("expected parsed record"),
        }
    }

    #[test]
    fn system_snapshot_read_returns_hash_and_state() {
        let mut sp = SystemProvider::new("You are helpful.".to_string());
        let val = unwrap_value(sp.read(&path!("snapshot")).unwrap().unwrap());
        match &val {
            Value::Map(m) => {
                let hash = match m.get("hash").unwrap() {
                    Value::String(s) => s.clone(),
                    _ => panic!("expected string hash"),
                };
                assert_eq!(hash.len(), 16);
                let state = m.get("state").unwrap();
                assert_eq!(state, &Value::String("You are helpful.".to_string()));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn system_snapshot_read_hash_only() {
        let mut sp = SystemProvider::new("Hello".to_string());
        let val = unwrap_value(sp.read(&path!("snapshot/hash")).unwrap().unwrap());
        match val {
            Value::String(h) => assert_eq!(h.len(), 16),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn system_snapshot_read_state_only() {
        let mut sp = SystemProvider::new("Hello".to_string());
        let val = unwrap_value(sp.read(&path!("snapshot/state")).unwrap().unwrap());
        assert_eq!(val, Value::String("Hello".to_string()));
    }

    #[test]
    fn system_snapshot_write_restores_state() {
        let mut sp = SystemProvider::new("old prompt".to_string());
        let mut map = std::collections::BTreeMap::new();
        map.insert("state".to_string(), Value::String("new prompt".to_string()));
        sp.write(&path!("snapshot"), Record::parsed(Value::Map(map)))
            .unwrap();
        let val = unwrap_value(sp.read(&path!("")).unwrap().unwrap());
        assert_eq!(val, Value::String("new prompt".to_string()));
    }

    #[test]
    fn system_snapshot_write_state_path() {
        let mut sp = SystemProvider::new("old".to_string());
        sp.write(
            &path!("snapshot/state"),
            Record::parsed(Value::String("new".to_string())),
        )
        .unwrap();
        let val = unwrap_value(sp.read(&path!("")).unwrap().unwrap());
        assert_eq!(val, Value::String("new".to_string()));
    }

    #[test]
    fn system_snapshot_hash_changes_after_write() {
        let mut sp = SystemProvider::new("first".to_string());
        let h1 = match unwrap_value(sp.read(&path!("snapshot/hash")).unwrap().unwrap()) {
            Value::String(s) => s,
            _ => panic!("expected string"),
        };
        sp.write(
            &path!("snapshot/state"),
            Record::parsed(Value::String("second".to_string())),
        )
        .unwrap();
        let h2 = match unwrap_value(sp.read(&path!("snapshot/hash")).unwrap().unwrap()) {
            Value::String(s) => s,
            _ => panic!("expected string"),
        };
        assert_ne!(h1, h2);
    }

    // -- ToolsProvider snapshot tests --

    #[test]
    fn tools_snapshot_returns_none() {
        let mut tp = ToolsProvider::new(vec![]);
        let result = tp.read(&path!("snapshot")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn tools_snapshot_hash_returns_none() {
        let mut tp = ToolsProvider::new(vec![]);
        let result = tp.read(&path!("snapshot/hash")).unwrap();
        assert!(result.is_none());
    }

    // -- Integration: coordinator discovery via Namespace --
    // These tests mount all five store types to exercise the full RFC
    // discovery pattern through the Namespace router.

    fn build_full_namespace() -> Namespace {
        let mut ns = Namespace::new();
        ns.mount(
            "system",
            Box::new(SystemProvider::new("You are helpful.".to_string())),
        );
        ns.mount("tools", Box::new(ToolsProvider::new(vec![])));
        ns.mount("history", Box::new(ox_history::HistoryProvider::new()));
        ns.mount("gate", Box::new(ox_gate::GateStore::new()));
        ns
    }

    #[test]
    fn namespace_snapshot_discovery_all_stores() {
        let mut ns = build_full_namespace();

        // Participating stores return Some
        assert!(ns.read(&path!("system/snapshot")).unwrap().is_some());
        assert!(ns.read(&path!("history/snapshot")).unwrap().is_some());
        assert!(ns.read(&path!("gate/snapshot")).unwrap().is_some());

        // Non-participating store returns None
        assert!(ns.read(&path!("tools/snapshot")).unwrap().is_none());
    }

    #[test]
    fn namespace_history_snapshot_write_returns_error() {
        let mut ns = build_full_namespace();

        // History snapshot is read-only — write must fail through the namespace
        let result = ns.write(&path!("history/snapshot"), Record::parsed(Value::Null));
        assert!(result.is_err());
    }

    #[test]
    fn synthesize_prompt_standalone() {
        let mut ns = build_full_namespace();
        let user_msg = serde_json::json!({"role": "user", "content": "hello"});
        ns.write(
            &path!("history/append"),
            Record::parsed(structfs_serde_store::json_to_value(user_msg)),
        )
        .unwrap();

        let result = synthesize_prompt(&mut ns).unwrap().unwrap();
        let value = result.as_value().unwrap().clone();
        let json = structfs_serde_store::value_to_json(value);
        let request: CompletionRequest = serde_json::from_value(json).unwrap();
        assert_eq!(request.model, "claude-sonnet-4-20250514");
        assert_eq!(request.system, "You are helpful.");
        assert_eq!(request.messages.len(), 1);
        assert!(request.stream);
    }

    #[test]
    fn namespace_snapshot_roundtrip() {
        let mut ns = Namespace::new();
        ns.mount(
            "system",
            Box::new(SystemProvider::new("original".to_string())),
        );
        ns.mount("gate", Box::new(ox_gate::GateStore::new()));

        let sys_snap = unwrap_value(ns.read(&path!("system/snapshot/state")).unwrap().unwrap());
        let gate_snap = unwrap_value(ns.read(&path!("gate/snapshot/state")).unwrap().unwrap());

        ns.write(
            &path!("system"),
            Record::parsed(Value::String("changed".to_string())),
        )
        .unwrap();
        ns.write(
            &path!("gate/defaults/model"),
            Record::parsed(Value::String("gpt-4o".to_string())),
        )
        .unwrap();

        ns.write(&path!("system/snapshot/state"), Record::parsed(sys_snap))
            .unwrap();
        ns.write(&path!("gate/snapshot/state"), Record::parsed(gate_snap))
            .unwrap();

        let val = unwrap_value(ns.read(&path!("system")).unwrap().unwrap());
        assert_eq!(val, Value::String("original".to_string()));
        let val = unwrap_value(ns.read(&path!("gate/defaults/model")).unwrap().unwrap());
        assert_eq!(val, Value::String("claude-sonnet-4-20250514".to_string()));
    }

    #[test]
    fn tools_provider_accepts_schema_write() {
        let mut tp = ToolsProvider::new(vec![]);

        // Initially empty
        let record = tp.read(&path!("schemas")).unwrap().unwrap();
        match unwrap_value(record) {
            Value::Array(a) => assert!(a.is_empty()),
            _ => panic!("expected array"),
        }

        // Write schemas
        let schemas_json = serde_json::json!([
            {"name": "test_tool", "description": "A test", "input_schema": {"type": "object"}}
        ]);
        let schemas_value = structfs_serde_store::json_to_value(schemas_json);
        tp.write(&path!("schemas"), Record::parsed(schemas_value))
            .unwrap();

        // Read back — should have 1 schema
        let record = tp.read(&path!("schemas")).unwrap().unwrap();
        match unwrap_value(record) {
            Value::Array(a) => assert_eq!(a.len(), 1),
            _ => panic!("expected array"),
        }
    }
}

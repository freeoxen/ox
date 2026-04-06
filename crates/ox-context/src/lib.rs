//! Namespace store router and concrete providers for the ox agent framework.
//!
//! This crate provides [`Namespace`], a virtual filesystem that routes reads
//! and writes to mounted [`Store`] implementations by path prefix — similar to
//! how a Unix VFS mounts devices at path prefixes.
//!
//! Three concrete providers are included:
//!
//! - [`SystemProvider`] — holds the system prompt string
//! - [`ToolsProvider`] — read-only snapshot of tool schemas
//! - [`ModelProvider`] — model ID and max_tokens settings
//!
//! Reading `path!("prompt")` from the namespace synthesizes a complete
//! [`CompletionRequest`] by collecting state from all mounted providers.

use ox_kernel::CompletionRequest;
pub use ox_kernel::ModelInfo;
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
/// by reading from sibling stores (system, history, tools, model).
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
        let empty = path!("");

        // Read system prompt
        let system_str = {
            let store = self.mounts.get_mut("system").ok_or_else(|| {
                StoreError::store("namespace", "read", "no store mounted at 'system'")
            })?;
            let record = store.read(&empty)?.ok_or_else(|| {
                StoreError::store("namespace", "read", "system store returned None")
            })?;
            match record {
                Record::Parsed(Value::String(s)) => s,
                _ => {
                    return Err(StoreError::store(
                        "namespace",
                        "read",
                        "expected string from system store",
                    ));
                }
            }
        };

        // Read history messages
        let messages_json = {
            let store = self.mounts.get_mut("history").ok_or_else(|| {
                StoreError::store("namespace", "read", "no store mounted at 'history'")
            })?;
            let record = store.read(&path!("messages"))?.ok_or_else(|| {
                StoreError::store("namespace", "read", "history store returned None")
            })?;
            match record {
                Record::Parsed(v) => value_to_json(v),
                _ => {
                    return Err(StoreError::store(
                        "namespace",
                        "read",
                        "expected parsed record from history",
                    ));
                }
            }
        };

        // Read tool schemas
        let tools_json = {
            let store = self.mounts.get_mut("tools").ok_or_else(|| {
                StoreError::store("namespace", "read", "no store mounted at 'tools'")
            })?;
            let record = store.read(&path!("schemas"))?.ok_or_else(|| {
                StoreError::store("namespace", "read", "tools store returned None")
            })?;
            match record {
                Record::Parsed(v) => value_to_json(v),
                _ => {
                    return Err(StoreError::store(
                        "namespace",
                        "read",
                        "expected parsed record from tools",
                    ));
                }
            }
        };

        // Read model ID
        let model_id = {
            let store = self.mounts.get_mut("model").ok_or_else(|| {
                StoreError::store("namespace", "read", "no store mounted at 'model'")
            })?;
            let record = store.read(&path!("id"))?.ok_or_else(|| {
                StoreError::store("namespace", "read", "model store returned None for id")
            })?;
            match record {
                Record::Parsed(Value::String(s)) => s,
                _ => {
                    return Err(StoreError::store(
                        "namespace",
                        "read",
                        "expected string from model store for id",
                    ));
                }
            }
        };

        // Read max_tokens
        let max_tokens = {
            let store = self.mounts.get_mut("model").ok_or_else(|| {
                StoreError::store("namespace", "read", "no store mounted at 'model'")
            })?;
            let record = store.read(&path!("max_tokens"))?.ok_or_else(|| {
                StoreError::store(
                    "namespace",
                    "read",
                    "model store returned None for max_tokens",
                )
            })?;
            match record {
                Record::Parsed(Value::Integer(n)) => n as u32,
                _ => {
                    return Err(StoreError::store(
                        "namespace",
                        "read",
                        "expected integer from model store for max_tokens",
                    ));
                }
            }
        };

        let messages: Vec<serde_json::Value> = serde_json::from_value(messages_json)
            .map_err(|e| StoreError::store("namespace", "read", e.to_string()))?;
        let tools: Vec<ox_kernel::ToolSchema> = serde_json::from_value(tools_json)
            .map_err(|e| StoreError::store("namespace", "read", e.to_string()))?;

        let request = CompletionRequest {
            model: model_id,
            max_tokens,
            system: system_str,
            messages,
            tools,
            stream: true,
        };

        let value = to_value(&request)
            .map_err(|e| StoreError::store("namespace", "read", e.to_string()))?;
        Ok(Some(Record::parsed(value)))
    }
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
// Concrete stores: System, Tools, Model
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
                    Ok(Some(Record::parsed(ox_kernel::snapshot::snapshot_record(state))))
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
                    _ => return Err(StoreError::store("system", "write", "expected parsed record")),
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
                    _ => Err(StoreError::store("system", "write", "snapshot state must be a string")),
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
    fn write(&mut self, _to: &Path, _data: Record) -> Result<Path, StoreError> {
        Err(StoreError::store(
            "tools",
            "write",
            "tools store is read-only",
        ))
    }
}

/// Provides model ID and max_tokens settings.
pub struct ModelProvider {
    model: String,
    max_tokens: u32,
}

impl ModelProvider {
    /// Create a new provider with the given model ID and token limit.
    pub fn new(model: String, max_tokens: u32) -> Self {
        Self { model, max_tokens }
    }

    fn snapshot_state(&self) -> Value {
        let mut map = std::collections::BTreeMap::new();
        map.insert("max_tokens".to_string(), Value::Integer(self.max_tokens as i64));
        map.insert("model".to_string(), Value::String(self.model.clone()));
        Value::Map(map)
    }
}

impl Reader for ModelProvider {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = if from.is_empty() {
            ""
        } else {
            from.components[0].as_str()
        };
        match key {
            "" | "id" => Ok(Some(Record::parsed(Value::String(self.model.clone())))),
            "max_tokens" => Ok(Some(Record::parsed(Value::Integer(self.max_tokens as i64)))),
            "snapshot" => {
                let state = self.snapshot_state();
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
                    Ok(Some(Record::parsed(ox_kernel::snapshot::snapshot_record(state))))
                }
            }
            _ => Ok(None),
        }
    }
}

impl Writer for ModelProvider {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let key = if to.is_empty() {
            ""
        } else {
            to.components[0].as_str()
        };
        match key {
            "" | "id" => match data {
                Record::Parsed(Value::String(s)) => {
                    self.model = s;
                    Ok(to.clone())
                }
                _ => Err(StoreError::store("model", "write", "expected string for id")),
            },
            "max_tokens" => match data {
                Record::Parsed(Value::Integer(n)) => {
                    self.max_tokens = n as u32;
                    Ok(to.clone())
                }
                _ => Err(StoreError::store("model", "write", "expected integer for max_tokens")),
            },
            "snapshot" => {
                let value = match data {
                    Record::Parsed(v) => v,
                    _ => return Err(StoreError::store("model", "write", "expected parsed record")),
                };
                let state = if to.components.len() >= 2 && to.components[1].as_str() == "state" {
                    value
                } else {
                    ox_kernel::snapshot::extract_snapshot_state(value)
                        .map_err(|e| StoreError::store("model", "write", e))?
                };
                match state {
                    Value::Map(m) => {
                        if let Some(Value::String(model)) = m.get("model") {
                            self.model = model.clone();
                        }
                        if let Some(Value::Integer(n)) = m.get("max_tokens") {
                            self.max_tokens = *n as u32;
                        }
                        Ok(to.clone())
                    }
                    _ => Err(StoreError::store("model", "write", "snapshot state must be a map with model and max_tokens")),
                }
            }
            _ => Err(StoreError::store(
                "model",
                "write",
                format!("unknown path: {to}"),
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
        sp.write(&path!("snapshot"), Record::parsed(Value::Map(map))).unwrap();
        let val = unwrap_value(sp.read(&path!("")).unwrap().unwrap());
        assert_eq!(val, Value::String("new prompt".to_string()));
    }

    #[test]
    fn system_snapshot_write_state_path() {
        let mut sp = SystemProvider::new("old".to_string());
        sp.write(
            &path!("snapshot/state"),
            Record::parsed(Value::String("new".to_string())),
        ).unwrap();
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
        sp.write(&path!("snapshot/state"), Record::parsed(Value::String("second".to_string()))).unwrap();
        let h2 = match unwrap_value(sp.read(&path!("snapshot/hash")).unwrap().unwrap()) {
            Value::String(s) => s,
            _ => panic!("expected string"),
        };
        assert_ne!(h1, h2);
    }

    // -- ModelProvider snapshot tests --

    #[test]
    fn model_snapshot_read_returns_hash_and_state() {
        let mut mp = ModelProvider::new("claude-sonnet-4-20250514".to_string(), 4096);
        let val = unwrap_value(mp.read(&path!("snapshot")).unwrap().unwrap());
        match &val {
            Value::Map(m) => {
                let hash = match m.get("hash").unwrap() {
                    Value::String(s) => s.clone(),
                    _ => panic!("expected string hash"),
                };
                assert_eq!(hash.len(), 16);
                let state = m.get("state").unwrap();
                match state {
                    Value::Map(sm) => {
                        assert_eq!(
                            sm.get("model").unwrap(),
                            &Value::String("claude-sonnet-4-20250514".to_string())
                        );
                        assert_eq!(sm.get("max_tokens").unwrap(), &Value::Integer(4096));
                    }
                    _ => panic!("expected map state"),
                }
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn model_snapshot_read_state_only() {
        let mut mp = ModelProvider::new("gpt-4o".to_string(), 8192);
        let val = unwrap_value(mp.read(&path!("snapshot/state")).unwrap().unwrap());
        match val {
            Value::Map(m) => {
                assert_eq!(m.get("model").unwrap(), &Value::String("gpt-4o".to_string()));
                assert_eq!(m.get("max_tokens").unwrap(), &Value::Integer(8192));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn model_snapshot_read_hash_only() {
        let mut mp = ModelProvider::new("gpt-4o".to_string(), 8192);
        let val = unwrap_value(mp.read(&path!("snapshot/hash")).unwrap().unwrap());
        match val {
            Value::String(h) => assert_eq!(h.len(), 16),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn model_snapshot_write_restores_state() {
        let mut mp = ModelProvider::new("old-model".to_string(), 1024);
        let mut state_map = std::collections::BTreeMap::new();
        state_map.insert("model".to_string(), Value::String("new-model".to_string()));
        state_map.insert("max_tokens".to_string(), Value::Integer(8192));
        let mut snap_map = std::collections::BTreeMap::new();
        snap_map.insert("state".to_string(), Value::Map(state_map));
        mp.write(&path!("snapshot"), Record::parsed(Value::Map(snap_map))).unwrap();

        let val = unwrap_value(mp.read(&path!("id")).unwrap().unwrap());
        assert_eq!(val, Value::String("new-model".to_string()));
        let val = unwrap_value(mp.read(&path!("max_tokens")).unwrap().unwrap());
        assert_eq!(val, Value::Integer(8192));
    }

    #[test]
    fn model_snapshot_write_state_path() {
        let mut mp = ModelProvider::new("old".to_string(), 1024);
        let mut state_map = std::collections::BTreeMap::new();
        state_map.insert("model".to_string(), Value::String("new".to_string()));
        state_map.insert("max_tokens".to_string(), Value::Integer(2048));
        mp.write(&path!("snapshot/state"), Record::parsed(Value::Map(state_map))).unwrap();

        let val = unwrap_value(mp.read(&path!("id")).unwrap().unwrap());
        assert_eq!(val, Value::String("new".to_string()));
        let val = unwrap_value(mp.read(&path!("max_tokens")).unwrap().unwrap());
        assert_eq!(val, Value::Integer(2048));
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

    #[test]
    fn namespace_snapshot_discovery() {
        let mut ns = Namespace::new();
        ns.mount(
            "system",
            Box::new(SystemProvider::new("You are helpful.".to_string())),
        );
        ns.mount(
            "model",
            Box::new(ModelProvider::new(
                "claude-sonnet-4-20250514".to_string(),
                4096,
            )),
        );
        ns.mount("tools", Box::new(ToolsProvider::new(vec![])));

        // system participates
        assert!(ns.read(&path!("system/snapshot")).unwrap().is_some());

        // model participates
        assert!(ns.read(&path!("model/snapshot")).unwrap().is_some());

        // tools does NOT participate
        assert!(ns.read(&path!("tools/snapshot")).unwrap().is_none());
    }

    #[test]
    fn namespace_snapshot_roundtrip() {
        let mut ns = Namespace::new();
        ns.mount(
            "system",
            Box::new(SystemProvider::new("original".to_string())),
        );
        ns.mount(
            "model",
            Box::new(ModelProvider::new("model-a".to_string(), 1024)),
        );

        // Read snapshots
        let sys_snap = unwrap_value(ns.read(&path!("system/snapshot/state")).unwrap().unwrap());
        let model_snap = unwrap_value(ns.read(&path!("model/snapshot/state")).unwrap().unwrap());

        // Mutate
        ns.write(
            &path!("system"),
            Record::parsed(Value::String("changed".to_string())),
        )
        .unwrap();
        ns.write(
            &path!("model/id"),
            Record::parsed(Value::String("model-b".to_string())),
        )
        .unwrap();

        // Restore from snapshots
        ns.write(&path!("system/snapshot/state"), Record::parsed(sys_snap))
            .unwrap();
        ns.write(&path!("model/snapshot/state"), Record::parsed(model_snap))
            .unwrap();

        // Verify restoration
        let val = unwrap_value(ns.read(&path!("system")).unwrap().unwrap());
        assert_eq!(val, Value::String("original".to_string()));

        let val = unwrap_value(ns.read(&path!("model/id")).unwrap().unwrap());
        assert_eq!(val, Value::String("model-a".to_string()));
    }
}

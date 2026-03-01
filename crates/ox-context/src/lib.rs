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
    pub fn new() -> Self {
        Self {
            mounts: BTreeMap::new(),
        }
    }

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
    pub fn new(prompt: String) -> Self {
        Self { prompt }
    }
}

impl Reader for SystemProvider {
    fn read(&mut self, _from: &Path) -> Result<Option<Record>, StoreError> {
        Ok(Some(Record::parsed(Value::String(self.prompt.clone()))))
    }
}

impl Writer for SystemProvider {
    fn write(&mut self, _to: &Path, data: Record) -> Result<Path, StoreError> {
        match data {
            Record::Parsed(Value::String(s)) => {
                self.prompt = s;
                Ok(Path::from_components(vec![]))
            }
            _ => Err(StoreError::store(
                "system",
                "write",
                "expected string value",
            )),
        }
    }
}

/// Provides tool schemas (read-only snapshot).
pub struct ToolsProvider {
    schemas: Vec<ox_kernel::ToolSchema>,
}

impl ToolsProvider {
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

/// A model entry in the catalog.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
}

/// Provides model ID, max_tokens, and an optional catalog of available models.
pub struct ModelProvider {
    model: String,
    max_tokens: u32,
    catalog: Vec<ModelInfo>,
}

impl ModelProvider {
    pub fn new(model: String, max_tokens: u32) -> Self {
        Self {
            model,
            max_tokens,
            catalog: Vec::new(),
        }
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
            "catalog" => {
                let value = to_value(&self.catalog)
                    .map_err(|e| StoreError::store("model", "read", e.to_string()))?;
                Ok(Some(Record::parsed(value)))
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
                _ => Err(StoreError::store(
                    "model",
                    "write",
                    "expected string for id",
                )),
            },
            "max_tokens" => match data {
                Record::Parsed(Value::Integer(n)) => {
                    self.max_tokens = n as u32;
                    Ok(to.clone())
                }
                _ => Err(StoreError::store(
                    "model",
                    "write",
                    "expected integer for max_tokens",
                )),
            },
            "catalog" => match data {
                Record::Parsed(v) => {
                    let catalog: Vec<ModelInfo> = structfs_serde_store::from_value(v)
                        .map_err(|e| StoreError::store("model", "write", e.to_string()))?;
                    self.catalog = catalog;
                    Ok(to.clone())
                }
                _ => Err(StoreError::store(
                    "model",
                    "write",
                    "expected parsed record for catalog",
                )),
            },
            _ => Err(StoreError::store(
                "model",
                "write",
                format!("unknown path: {to}"),
            )),
        }
    }
}

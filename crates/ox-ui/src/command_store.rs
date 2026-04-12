//! CommandStore — StructFS Reader/Writer over CommandRegistry.
//!
//! Reads discover commands. Writes invoke or register them.

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

use crate::command_def::{CommandDef, CommandInvocation};
use crate::command_registry::CommandRegistry;

/// Callback for dispatching resolved commands to target stores.
pub type CommandDispatcher =
    Box<dyn FnMut(&Path, Record) -> Result<Path, StoreError> + Send + Sync>;

pub struct CommandStore {
    registry: CommandRegistry,
    dispatcher: Option<CommandDispatcher>,
}

impl CommandStore {
    pub fn new(registry: CommandRegistry) -> Self {
        CommandStore {
            registry,
            dispatcher: None,
        }
    }

    pub fn from_builtins() -> Self {
        Self::new(CommandRegistry::from_builtins())
    }

    pub fn set_dispatcher(&mut self, dispatcher: CommandDispatcher) {
        self.dispatcher = Some(dispatcher);
    }

    pub fn registry(&self) -> &CommandRegistry {
        &self.registry
    }
}

impl Reader for CommandStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let c = &from.components;
        match c.len() {
            0 => {
                let defs: Vec<Value> = self
                    .registry
                    .iter()
                    .map(|d| structfs_serde_store::to_value(d).unwrap())
                    .collect();
                Ok(Some(Record::parsed(Value::Array(defs))))
            }
            1 if c[0] == "commands" => {
                let defs: Vec<Value> = self
                    .registry
                    .iter()
                    .map(|d| structfs_serde_store::to_value(d).unwrap())
                    .collect();
                Ok(Some(Record::parsed(Value::Array(defs))))
            }
            1 if c[0] == "user_facing" => {
                let defs: Vec<Value> = self
                    .registry
                    .user_facing()
                    .map(|d| structfs_serde_store::to_value(d).unwrap())
                    .collect();
                Ok(Some(Record::parsed(Value::Array(defs))))
            }
            2 if c[0] == "commands" => match self.registry.get(&c[1]) {
                Some(def) => {
                    let value = structfs_serde_store::to_value(def).unwrap();
                    Ok(Some(Record::parsed(value)))
                }
                None => Ok(None),
            },
            _ => Ok(None),
        }
    }
}

impl Writer for CommandStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let action = if to.is_empty() {
            ""
        } else {
            to.components[0].as_str()
        };
        let value = data.as_value().ok_or_else(|| {
            StoreError::store("command", "write", "write data must contain a value")
        })?;

        match action {
            "invoke" => {
                let invocation: CommandInvocation = structfs_serde_store::from_value(value.clone())
                    .map_err(|e| {
                        StoreError::store("command", "invoke", format!("bad invocation: {e}"))
                    })?;
                let (path, record) = self
                    .registry
                    .resolve(&invocation)
                    .map_err(|e| StoreError::store("command", "invoke", e.to_string()))?;
                let dispatcher = self.dispatcher.as_mut().ok_or_else(|| {
                    StoreError::store("command", "invoke", "no dispatcher configured")
                })?;
                dispatcher(&path, record)
            }
            "register" => {
                let def: CommandDef =
                    structfs_serde_store::from_value(value.clone()).map_err(|e| {
                        StoreError::store("command", "register", format!("bad command def: {e}"))
                    })?;
                self.registry
                    .register(def)
                    .map_err(|e| StoreError::store("command", "register", e.to_string()))?;
                Ok(Path::parse("commands").unwrap())
            }
            "unregister" => {
                let name = match value {
                    Value::Map(m) => m.get("name").and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    }),
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                }
                .ok_or_else(|| StoreError::store("command", "unregister", "missing name"))?;
                self.registry
                    .unregister(&name)
                    .map_err(|e| StoreError::store("command", "unregister", e.to_string()))?;
                Ok(Path::parse("commands").unwrap())
            }
            _ => Err(StoreError::store(
                "command",
                "write",
                format!("unknown path: {action}"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use structfs_core_store::path;

    type DispatchLog = Arc<Mutex<Vec<(String, BTreeMap<String, Value>)>>>;

    fn mock_dispatcher() -> (CommandDispatcher, DispatchLog) {
        let log: DispatchLog = Arc::new(Mutex::new(Vec::new()));
        let log_clone = log.clone();
        let dispatcher: CommandDispatcher = Box::new(move |path, data| {
            let fields = match data.as_value() {
                Some(Value::Map(m)) => m.clone(),
                _ => BTreeMap::new(),
            };
            log_clone.lock().unwrap().push((path.to_string(), fields));
            Ok(path.clone())
        });
        (dispatcher, log)
    }

    fn test_store() -> (CommandStore, DispatchLog) {
        let (dispatcher, log) = mock_dispatcher();
        let mut store = CommandStore::from_builtins();
        store.set_dispatcher(dispatcher);
        (store, log)
    }

    #[test]
    fn read_all_commands() {
        let (mut store, _) = test_store();
        let result = store.read(&path!("commands")).unwrap().unwrap();
        match result.as_value().unwrap() {
            Value::Array(arr) => assert!(!arr.is_empty()),
            _ => panic!("expected array"),
        }
    }

    #[test]
    fn read_single_command() {
        let (mut store, _) = test_store();
        let result = store.read(&path!("commands/quit")).unwrap().unwrap();
        match result.as_value().unwrap() {
            Value::Map(m) => {
                assert_eq!(m.get("name"), Some(&Value::String("quit".into())));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn read_unknown_command_returns_none() {
        let (mut store, _) = test_store();
        let result = store.read(&path!("commands/nonexistent")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_user_facing() {
        let (mut store, _) = test_store();
        let result = store.read(&path!("user_facing")).unwrap().unwrap();
        match result.as_value().unwrap() {
            Value::Array(arr) => {
                for item in arr {
                    if let Value::Map(m) = item {
                        assert_eq!(m.get("user_facing"), Some(&Value::Bool(true)));
                    }
                }
            }
            _ => panic!("expected array"),
        }
    }

    #[test]
    fn invoke_dispatches_to_target() {
        let (mut store, log) = test_store();
        let inv_json = serde_json::json!({
            "command": "quit",
            "args": {}
        });
        let inv_value = structfs_serde_store::json_to_value(inv_json);
        store
            .write(&path!("invoke"), Record::parsed(inv_value))
            .unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, "ui/quit");
    }

    #[test]
    fn invoke_validates_params() {
        let (mut store, _) = test_store();
        // open requires thread_id — invoke without it should fail
        let inv_json = serde_json::json!({
            "command": "open",
            "args": {}
        });
        let inv_value = structfs_serde_store::json_to_value(inv_json);
        let result = store.write(&path!("invoke"), Record::parsed(inv_value));
        assert!(result.is_err());
    }

    #[test]
    fn register_adds_command() {
        let (mut store, _) = test_store();
        let def_json = serde_json::json!({
            "name": "custom",
            "target": "plugin/action",
            "params": [],
            "description": "Custom cmd",
            "user_facing": true
        });
        let def_value = structfs_serde_store::json_to_value(def_json);
        store
            .write(&path!("register"), Record::parsed(def_value))
            .unwrap();

        // Should now be discoverable
        let result = store.read(&path!("commands/custom")).unwrap();
        assert!(result.is_some());
    }
}

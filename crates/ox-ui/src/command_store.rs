//! CommandStore — StructFS Reader/Writer over CommandRegistry.
//!
//! Reads discover commands. Writes invoke, exec (raw text), or register.

use std::collections::BTreeMap;

use ox_path::oxpath;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

use crate::command::Dispatcher;
use crate::command_def::{CommandDef, CommandInvocation};
use crate::command_registry::CommandRegistry;

/// Parse a single line of command text into a [`CommandInvocation`].
///
/// Grammar: `command_name [rest]`. If `rest` contains any `key=value`
/// tokens, all `key=value` tokens are collected. Otherwise — and if the
/// registry has a command matching `command_name` — the whole `rest` is
/// bound to that command's first required parameter (positional sugar).
/// Unknown commands pass through with empty args; the error surfaces at
/// resolve time.
pub fn parse_command_text(input: &str, registry: &CommandRegistry) -> CommandInvocation {
    let input = input.trim();
    let mut parts = input.splitn(2, ' ');
    let command = parts.next().unwrap_or("").to_string();
    let rest = parts.next().unwrap_or("").trim();
    let mut args = BTreeMap::new();
    if !rest.is_empty() {
        let has_kv = rest.split_whitespace().any(|t| t.contains('='));
        if has_kv {
            for token in rest.split_whitespace() {
                if let Some((k, v)) = token.split_once('=') {
                    args.insert(k.to_string(), serde_json::Value::String(v.to_string()));
                }
            }
        } else if let Some(def) = registry.get(&command) {
            for param in &def.params {
                if param.required {
                    args.insert(
                        param.name.clone(),
                        serde_json::Value::String(rest.to_string()),
                    );
                    break;
                }
            }
        }
    }
    CommandInvocation { command, args }
}

pub struct CommandStore {
    registry: CommandRegistry,
    dispatcher: Option<Dispatcher>,
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

    pub fn set_dispatcher(&mut self, dispatcher: Dispatcher) {
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
            "exec" => {
                // Raw text → parse → resolve → dispatch. Empty/whitespace
                // input is a silent no-op (vim behavior on empty :Enter).
                let text = match value {
                    Value::String(s) => s.as_str(),
                    _ => {
                        return Err(StoreError::store(
                            "command",
                            "exec",
                            "exec payload must be a String",
                        ));
                    }
                };
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    return Ok(oxpath!("commands"));
                }
                let invocation = parse_command_text(trimmed, &self.registry);
                let (path, record) = self
                    .registry
                    .resolve(&invocation)
                    .map_err(|e| StoreError::store("command", "exec", e.to_string()))?;
                let dispatcher = self.dispatcher.as_mut().ok_or_else(|| {
                    StoreError::store("command", "exec", "no dispatcher configured")
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
                Ok(oxpath!("commands"))
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
                Ok(oxpath!("commands"))
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

    fn mock_dispatcher() -> (Dispatcher, DispatchLog) {
        let log: DispatchLog = Arc::new(Mutex::new(Vec::new()));
        let log_clone = log.clone();
        let dispatcher: Dispatcher = Box::new(move |path, data| {
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
    fn exec_bare_command_dispatches_to_target() {
        let (mut store, log) = test_store();
        store
            .write(&path!("exec"), Record::parsed(Value::String("quit".into())))
            .unwrap();
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, "ui/quit");
    }

    #[test]
    fn exec_with_positional_resolves_against_registry() {
        let (mut store, log) = test_store();
        store
            .write(
                &path!("exec"),
                Record::parsed(Value::String("open t_123".into())),
            )
            .unwrap();
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, "ui/open");
        assert_eq!(
            log[0].1.get("thread_id"),
            Some(&Value::String("t_123".into()))
        );
    }

    #[test]
    fn exec_with_kv_args() {
        let (mut store, log) = test_store();
        store
            .write(
                &path!("exec"),
                Record::parsed(Value::String("open thread_id=t_999".into())),
            )
            .unwrap();
        let log = log.lock().unwrap();
        assert_eq!(
            log[0].1.get("thread_id"),
            Some(&Value::String("t_999".into()))
        );
    }

    #[test]
    fn exec_empty_is_silent_noop() {
        let (mut store, log) = test_store();
        store
            .write(&path!("exec"), Record::parsed(Value::String("".into())))
            .unwrap();
        store
            .write(&path!("exec"), Record::parsed(Value::String("   ".into())))
            .unwrap();
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn exec_unknown_command_errors() {
        let (mut store, _log) = test_store();
        let result = store.write(&path!("exec"), Record::parsed(Value::String("nope".into())));
        assert!(result.is_err());
    }

    #[test]
    fn exec_missing_required_param_errors() {
        let (mut store, _log) = test_store();
        // open requires thread_id — plain `open` has no positional, no kv
        let result = store.write(&path!("exec"), Record::parsed(Value::String("open".into())));
        assert!(result.is_err());
    }

    #[test]
    fn exec_non_string_payload_errors() {
        let (mut store, _log) = test_store();
        let result = store.write(&path!("exec"), Record::parsed(Value::Integer(42)));
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

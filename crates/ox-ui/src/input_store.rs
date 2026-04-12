//! InputStore — context-aware key event interpreter.
//!
//! Holds a mutable binding table mapping (mode, key, screen?) to actions.
//! Key events carry context (mode, key, screen, current state). The store
//! resolves the most specific matching binding and dispatches commands
//! through a pluggable dispatcher.
//!
//! Bindings are queryable for help/discoverability: read `bindings/{mode}`
//! to get all bindings for a mode. The TUI renders these for "?" help.
//!
//! Bindings are writable for runtime customization: write to `bind`,
//! `unbind`, or `macro` to modify the table at runtime.

use std::collections::BTreeMap;

use ox_path::oxpath;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

// ---------------------------------------------------------------------------
// Binding model
// ---------------------------------------------------------------------------

/// When a binding is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingContext {
    pub mode: String,
    pub key: String,
    /// If Some, only active on this screen. If None, active on all screens.
    pub screen: Option<String>,
}

/// What a binding does when activated.
#[derive(Debug, Clone)]
pub enum Action {
    /// Write a single command to a target path.
    Command {
        target: Path,
        /// Additional fields to include in the dispatched command value.
        fields: Vec<ActionField>,
    },
    /// Command invocation through the command registry.
    Invoke {
        command: String,
        args: std::collections::BTreeMap<String, serde_json::Value>,
    },
    /// Execute a sequence of command actions in order.
    Macro(Vec<Action>),
}

/// A field to include in a dispatched command.
#[derive(Debug, Clone)]
pub enum ActionField {
    /// Include a static value.
    Static { key: String, value: Value },
    /// Include a value extracted from the key event's context data.
    /// `source` names the field in the event context; `key` names it
    /// in the dispatched command.
    FromContext { key: String, source: String },
}

/// A complete binding: activation context + action + description.
#[derive(Debug, Clone)]
pub struct Binding {
    pub context: BindingContext,
    pub action: Action,
    pub description: String,
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// Callback for dispatching commands to target stores.
///
/// In production, wraps a broker ClientHandle. In tests, a mock
/// that records dispatched commands.
pub type CommandDispatcher =
    Box<dyn FnMut(&Path, Record) -> Result<Path, StoreError> + Send + Sync>;

// ---------------------------------------------------------------------------
// InputStore
// ---------------------------------------------------------------------------

pub struct InputStore {
    bindings: Vec<Binding>,
    dispatcher: Option<CommandDispatcher>,
    txn_counter: u64,
}

impl InputStore {
    pub fn new(bindings: Vec<Binding>) -> Self {
        InputStore {
            bindings,
            dispatcher: None,
            txn_counter: 0,
        }
    }

    /// Set the command dispatcher.
    pub fn set_dispatcher(&mut self, dispatcher: CommandDispatcher) {
        self.dispatcher = Some(dispatcher);
    }

    fn next_txn(&mut self) -> String {
        self.txn_counter += 1;
        format!("input_{}", self.txn_counter)
    }

    // -- Binding resolution --

    /// Find the best binding for a (mode, key, screen) triple.
    ///
    /// Screen-specific bindings take priority over screen-agnostic ones.
    fn resolve(&self, mode: &str, key: &str, screen: Option<&str>) -> Option<&Binding> {
        let mut best: Option<&Binding> = None;
        for b in &self.bindings {
            if b.context.mode != mode || b.context.key != key {
                continue;
            }
            match (&b.context.screen, screen) {
                // Exact screen match — highest priority, return immediately
                (Some(bs), Some(s)) if bs == s => return Some(b),
                // Binding has no screen constraint — matches any screen
                (None, _) => {
                    if best.is_none() {
                        best = Some(b);
                    }
                }
                // Binding has screen constraint but doesn't match — skip
                _ => {}
            }
        }
        best
    }

    // -- Action execution --

    fn execute_action(
        &mut self,
        action: &Action,
        event_context: &BTreeMap<String, Value>,
    ) -> Result<Path, StoreError> {
        match action {
            Action::Command { target, fields } => {
                let mut cmd = BTreeMap::new();
                cmd.insert("txn".to_string(), Value::String(self.next_txn()));
                for field in fields {
                    match field {
                        ActionField::Static { key, value } => {
                            cmd.insert(key.clone(), value.clone());
                        }
                        ActionField::FromContext { key, source } => {
                            if let Some(val) = event_context.get(source) {
                                cmd.insert(key.clone(), val.clone());
                            }
                        }
                    }
                }
                let dispatcher = self.dispatcher.as_mut().ok_or_else(|| {
                    StoreError::store("input", "dispatch", "no dispatcher configured")
                })?;
                dispatcher(target, Record::parsed(Value::Map(cmd)))
            }
            Action::Invoke { command, args } => {
                // Build a CommandInvocation as a StructFS value and dispatch to command/invoke
                let inv = crate::command_def::CommandInvocation {
                    command: command.clone(),
                    args: args.clone(),
                };
                let inv_value = structfs_serde_store::to_value(&inv).map_err(|e| {
                    StoreError::store("input", "dispatch", format!("failed to serialize invocation: {e}"))
                })?;
                let target = Path::parse("command/invoke").unwrap();
                let dispatcher = self.dispatcher.as_mut().ok_or_else(|| {
                    StoreError::store("input", "dispatch", "no dispatcher configured")
                })?;
                dispatcher(&target, Record::parsed(inv_value))
            }
            Action::Macro(steps) => {
                let mut last_result = oxpath!();
                for step in steps {
                    last_result = self.execute_action(step, event_context)?;
                }
                Ok(last_result)
            }
        }
    }

    // -- Binding serialization for reads --

    fn binding_to_value(b: &Binding) -> Value {
        let mut map = BTreeMap::new();
        map.insert("mode".to_string(), Value::String(b.context.mode.clone()));
        map.insert("key".to_string(), Value::String(b.context.key.clone()));
        if let Some(ref s) = b.context.screen {
            map.insert("screen".to_string(), Value::String(s.clone()));
        }
        map.insert(
            "description".to_string(),
            Value::String(b.description.clone()),
        );
        match &b.action {
            Action::Command { target, .. } => {
                map.insert("target".to_string(), Value::String(target.to_string()));
                map.insert("type".to_string(), Value::String("command".to_string()));
            }
            Action::Invoke { command, .. } => {
                map.insert("command".to_string(), Value::String(command.clone()));
                map.insert("type".to_string(), Value::String("invoke".to_string()));
            }
            Action::Macro(steps) => {
                map.insert("steps".to_string(), Value::Integer(steps.len() as i64));
                map.insert("type".to_string(), Value::String("macro".to_string()));
            }
        }
        Value::Map(map)
    }

    fn bindings_matching(&self, mode: Option<&str>, screen: Option<&str>) -> Vec<Value> {
        self.bindings
            .iter()
            .filter(|b| {
                mode.is_none_or(|m| b.context.mode == m)
                    && screen.is_none_or(|s| {
                        b.context.screen.is_none() || b.context.screen.as_deref() == Some(s)
                    })
            })
            .map(Self::binding_to_value)
            .collect()
    }

    // -- Runtime binding modification --

    fn handle_bind(&mut self, value: &Value) -> Result<Path, StoreError> {
        let map = match value {
            Value::Map(m) => m,
            _ => return Err(StoreError::store("input", "bind", "expected Map")),
        };
        let mode = extract_str(map, "mode")
            .ok_or_else(|| StoreError::store("input", "bind", "missing mode"))?;
        let key = extract_str(map, "key")
            .ok_or_else(|| StoreError::store("input", "bind", "missing key"))?;
        let target_str = extract_str(map, "target")
            .ok_or_else(|| StoreError::store("input", "bind", "missing target"))?;
        let target = Path::parse(&target_str)
            .map_err(|e| StoreError::store("input", "bind", e.to_string()))?;
        let description = extract_str(map, "description").unwrap_or_default();
        let screen = extract_str(map, "screen");

        let ctx = BindingContext {
            mode: mode.clone(),
            key: key.clone(),
            screen: screen.clone(),
        };

        // Remove existing binding with same context
        self.bindings.retain(|b| b.context != ctx);

        // Parse fields if present
        let fields = parse_fields(map.get("fields"));

        self.bindings.push(Binding {
            context: ctx,
            action: Action::Command { target, fields },
            description,
        });
        Ok(Path::parse("bindings").unwrap())
    }

    fn handle_unbind(&mut self, value: &Value) -> Result<Path, StoreError> {
        let map = match value {
            Value::Map(m) => m,
            _ => return Err(StoreError::store("input", "unbind", "expected Map")),
        };
        let mode = extract_str(map, "mode")
            .ok_or_else(|| StoreError::store("input", "unbind", "missing mode"))?;
        let key = extract_str(map, "key")
            .ok_or_else(|| StoreError::store("input", "unbind", "missing key"))?;
        let screen = extract_str(map, "screen");

        let before = self.bindings.len();
        self.bindings.retain(|b| {
            !(b.context.mode == mode && b.context.key == key && b.context.screen == screen)
        });
        if self.bindings.len() == before {
            return Err(StoreError::store("input", "unbind", "binding not found"));
        }
        Ok(Path::parse("bindings").unwrap())
    }

    fn handle_macro_bind(&mut self, value: &Value) -> Result<Path, StoreError> {
        let map = match value {
            Value::Map(m) => m,
            _ => return Err(StoreError::store("input", "macro", "expected Map")),
        };
        let mode = extract_str(map, "mode")
            .ok_or_else(|| StoreError::store("input", "macro", "missing mode"))?;
        let key = extract_str(map, "key")
            .ok_or_else(|| StoreError::store("input", "macro", "missing key"))?;
        let description = extract_str(map, "description").unwrap_or_default();
        let screen = extract_str(map, "screen");

        let steps_arr = map
            .get("steps")
            .and_then(|v| match v {
                Value::Array(a) => Some(a),
                _ => None,
            })
            .ok_or_else(|| StoreError::store("input", "macro", "missing steps array"))?;

        let mut steps = Vec::new();
        for step in steps_arr {
            let step_map = match step {
                Value::Map(m) => m,
                _ => {
                    return Err(StoreError::store(
                        "input",
                        "macro",
                        "each step must be a Map",
                    ));
                }
            };
            let target_str = extract_str(step_map, "target")
                .ok_or_else(|| StoreError::store("input", "macro", "step missing target"))?;
            let target = Path::parse(&target_str)
                .map_err(|e| StoreError::store("input", "macro", e.to_string()))?;
            let fields = parse_fields(step_map.get("fields"));
            steps.push(Action::Command { target, fields });
        }

        let ctx = BindingContext {
            mode: mode.clone(),
            key: key.clone(),
            screen: screen.clone(),
        };
        self.bindings.retain(|b| b.context != ctx);
        self.bindings.push(Binding {
            context: ctx,
            action: Action::Macro(steps),
            description,
        });
        Ok(Path::parse("bindings").unwrap())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_str(map: &BTreeMap<String, Value>, key: &str) -> Option<String> {
    map.get(key).and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        _ => None,
    })
}

fn parse_fields(value: Option<&Value>) -> Vec<ActionField> {
    let arr = match value {
        Some(Value::Array(a)) => a,
        _ => return Vec::new(),
    };
    arr.iter()
        .filter_map(|item| {
            let m = match item {
                Value::Map(m) => m,
                _ => return None,
            };
            let key = extract_str(m, "key")?;
            if let Some(source) = extract_str(m, "source") {
                Some(ActionField::FromContext { key, source })
            } else {
                m.get("value")
                    .cloned()
                    .map(|value| ActionField::Static { key, value })
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

impl Reader for InputStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let c = &from.components;
        match c.len() {
            // "" → all bindings
            0 => Ok(Some(Record::parsed(Value::Array(
                self.bindings_matching(None, None),
            )))),
            // "bindings" → all bindings
            1 if c[0] == "bindings" => Ok(Some(Record::parsed(Value::Array(
                self.bindings_matching(None, None),
            )))),
            // "bindings/{mode}" → bindings for mode
            2 if c[0] == "bindings" => Ok(Some(Record::parsed(Value::Array(
                self.bindings_matching(Some(&c[1]), None),
            )))),
            // "bindings/{mode}/{screen}" → bindings for mode+screen
            3 if c[0] == "bindings" => Ok(Some(Record::parsed(Value::Array(
                self.bindings_matching(Some(&c[1]), Some(&c[2])),
            )))),
            _ => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

impl Writer for InputStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let action = if to.is_empty() {
            ""
        } else {
            to.components[0].as_str()
        };
        let value = data.as_value().ok_or_else(|| {
            StoreError::store("input", "write", "write data must contain a value")
        })?;

        match action {
            "key" => {
                // Key event dispatch: data is {mode, key, screen?, ...context}
                let map = match value {
                    Value::Map(m) => m,
                    _ => return Err(StoreError::store("input", "key", "key event must be a Map")),
                };
                let mode = extract_str(map, "mode")
                    .ok_or_else(|| StoreError::store("input", "key", "missing mode"))?;
                let key = extract_str(map, "key")
                    .ok_or_else(|| StoreError::store("input", "key", "missing key"))?;
                let screen = extract_str(map, "screen");

                let binding = self
                    .resolve(&mode, &key, screen.as_deref())
                    .ok_or_else(|| StoreError::store("input", "key", "no binding for key"))?;
                let action = binding.action.clone();

                self.execute_action(&action, map)
            }
            "bind" => self.handle_bind(value),
            "unbind" => self.handle_unbind(value),
            "macro" => self.handle_macro_bind(value),
            _ => Err(StoreError::store("input", "write", "unknown path")),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use structfs_core_store::path;

    fn simple_binding(mode: &str, key: &str, target: &str, desc: &str) -> Binding {
        Binding {
            context: BindingContext {
                mode: mode.to_string(),
                key: key.to_string(),
                screen: None,
            },
            action: Action::Command {
                target: Path::parse(target).unwrap(),
                fields: vec![],
            },
            description: desc.to_string(),
        }
    }

    fn screen_binding(mode: &str, key: &str, screen: &str, target: &str, desc: &str) -> Binding {
        Binding {
            context: BindingContext {
                mode: mode.to_string(),
                key: key.to_string(),
                screen: Some(screen.to_string()),
            },
            action: Action::Command {
                target: Path::parse(target).unwrap(),
                fields: vec![],
            },
            description: desc.to_string(),
        }
    }

    fn key_event(mode: &str, key: &str, screen: &str) -> Record {
        let mut map = BTreeMap::new();
        map.insert("mode".to_string(), Value::String(mode.to_string()));
        map.insert("key".to_string(), Value::String(key.to_string()));
        map.insert("screen".to_string(), Value::String(screen.to_string()));
        Record::parsed(Value::Map(map))
    }

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

    // -- Resolution tests --

    #[test]
    fn resolve_generic_binding() {
        let bindings = vec![simple_binding("normal", "j", "ui/select_next", "down")];
        let store = InputStore::new(bindings);
        let b = store.resolve("normal", "j", Some("inbox")).unwrap();
        assert_eq!(b.description, "down");
    }

    #[test]
    fn resolve_screen_specific_wins() {
        let bindings = vec![
            simple_binding("normal", "j", "ui/select_next", "generic"),
            screen_binding("normal", "j", "thread", "ui/scroll_down", "scroll"),
        ];
        let store = InputStore::new(bindings);

        // On inbox screen: generic wins
        let b = store.resolve("normal", "j", Some("inbox")).unwrap();
        assert_eq!(b.description, "generic");

        // On thread screen: specific wins
        let b = store.resolve("normal", "j", Some("thread")).unwrap();
        assert_eq!(b.description, "scroll");
    }

    #[test]
    fn resolve_no_match_returns_none() {
        let store = InputStore::new(vec![]);
        assert!(store.resolve("normal", "j", None).is_none());
    }

    // -- Key dispatch tests --

    #[test]
    fn dispatch_simple_command() {
        let bindings = vec![simple_binding("normal", "j", "ui/select_next", "down")];
        let mut store = InputStore::new(bindings);
        let (dispatcher, log) = mock_dispatcher();
        store.set_dispatcher(dispatcher);

        store
            .write(&path!("key"), key_event("normal", "j", "inbox"))
            .unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, "ui/select_next");
        assert!(log[0].1.contains_key("txn"));
    }

    #[test]
    fn dispatch_screen_specific() {
        let bindings = vec![
            screen_binding("normal", "j", "inbox", "ui/select_next", "down"),
            screen_binding("normal", "j", "thread", "ui/scroll_down", "scroll"),
        ];
        let mut store = InputStore::new(bindings);
        let (dispatcher, log) = mock_dispatcher();
        store.set_dispatcher(dispatcher);

        store
            .write(&path!("key"), key_event("normal", "j", "inbox"))
            .unwrap();
        store
            .write(&path!("key"), key_event("normal", "j", "thread"))
            .unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log[0].0, "ui/select_next");
        assert_eq!(log[1].0, "ui/scroll_down");
    }

    #[test]
    fn dispatch_with_context_field() {
        let bindings = vec![Binding {
            context: BindingContext {
                mode: "normal".to_string(),
                key: "j".to_string(),
                screen: None,
            },
            action: Action::Command {
                target: Path::parse("ui/select_next").unwrap(),
                fields: vec![ActionField::FromContext {
                    key: "from".to_string(),
                    source: "selected_row".to_string(),
                }],
            },
            description: "down".to_string(),
        }];
        let mut store = InputStore::new(bindings);
        let (dispatcher, log) = mock_dispatcher();
        store.set_dispatcher(dispatcher);

        let mut event = BTreeMap::new();
        event.insert("mode".to_string(), Value::String("normal".to_string()));
        event.insert("key".to_string(), Value::String("j".to_string()));
        event.insert("screen".to_string(), Value::String("inbox".to_string()));
        event.insert("selected_row".to_string(), Value::Integer(3));
        store
            .write(&path!("key"), Record::parsed(Value::Map(event)))
            .unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log[0].1.get("from"), Some(&Value::Integer(3)));
    }

    #[test]
    fn dispatch_no_binding_returns_error() {
        let mut store = InputStore::new(vec![]);
        let (dispatcher, _) = mock_dispatcher();
        store.set_dispatcher(dispatcher);

        let result = store.write(&path!("key"), key_event("normal", "z", "inbox"));
        assert!(result.is_err());
    }

    // -- Macro tests --

    #[test]
    fn dispatch_macro_executes_steps() {
        let bindings = vec![Binding {
            context: BindingContext {
                mode: "normal".to_string(),
                key: "G".to_string(),
                screen: None,
            },
            action: Action::Macro(vec![
                Action::Command {
                    target: Path::parse("ui/close").unwrap(),
                    fields: vec![],
                },
                Action::Command {
                    target: Path::parse("ui/set_status").unwrap(),
                    fields: vec![ActionField::Static {
                        key: "text".to_string(),
                        value: Value::String("Returned to inbox".to_string()),
                    }],
                },
            ]),
            description: "go home".to_string(),
        }];
        let mut store = InputStore::new(bindings);
        let (dispatcher, log) = mock_dispatcher();
        store.set_dispatcher(dispatcher);

        store
            .write(&path!("key"), key_event("normal", "G", "thread"))
            .unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].0, "ui/close");
        assert_eq!(log[1].0, "ui/set_status");
        assert_eq!(
            log[1].1.get("text"),
            Some(&Value::String("Returned to inbox".to_string()))
        );
    }

    // -- Read tests --

    #[test]
    fn read_all_bindings() {
        let bindings = vec![
            simple_binding("normal", "j", "ui/select_next", "down"),
            simple_binding("insert", "Esc", "ui/exit_insert", "exit"),
        ];
        let mut store = InputStore::new(bindings);
        let val = store.read(&path!("bindings")).unwrap().unwrap();
        let arr = match val.as_value().unwrap() {
            Value::Array(a) => a,
            _ => panic!("expected array"),
        };
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn read_bindings_by_mode() {
        let bindings = vec![
            simple_binding("normal", "j", "ui/select_next", "down"),
            simple_binding("normal", "k", "ui/select_prev", "up"),
            simple_binding("insert", "Esc", "ui/exit_insert", "exit"),
        ];
        let mut store = InputStore::new(bindings);
        let val = store.read(&path!("bindings/normal")).unwrap().unwrap();
        let arr = match val.as_value().unwrap() {
            Value::Array(a) => a,
            _ => panic!("expected array"),
        };
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn read_bindings_by_mode_and_screen() {
        let bindings = vec![
            simple_binding("normal", "j", "ui/select_next", "generic j"),
            screen_binding("normal", "j", "thread", "ui/scroll_down", "thread j"),
            simple_binding("normal", "k", "ui/select_prev", "generic k"),
        ];
        let mut store = InputStore::new(bindings);
        // bindings/normal/thread should return generic j (no screen) + thread j + generic k
        let val = store
            .read(&path!("bindings/normal/thread"))
            .unwrap()
            .unwrap();
        let arr = match val.as_value().unwrap() {
            Value::Array(a) => a,
            _ => panic!("expected array"),
        };
        assert_eq!(arr.len(), 3);
    }

    // -- Runtime modification tests --

    #[test]
    fn bind_adds_new_binding() {
        let mut store = InputStore::new(vec![]);
        let (dispatcher, log) = mock_dispatcher();
        store.set_dispatcher(dispatcher);

        let mut bind_val = BTreeMap::new();
        bind_val.insert("mode".to_string(), Value::String("normal".to_string()));
        bind_val.insert("key".to_string(), Value::String("x".to_string()));
        bind_val.insert("target".to_string(), Value::String("ui/close".to_string()));
        bind_val.insert(
            "description".to_string(),
            Value::String("close".to_string()),
        );
        store
            .write(&path!("bind"), Record::parsed(Value::Map(bind_val)))
            .unwrap();

        // Verify it works
        store
            .write(&path!("key"), key_event("normal", "x", "inbox"))
            .unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log[0].0, "ui/close");
    }

    #[test]
    fn bind_replaces_existing() {
        let bindings = vec![simple_binding("normal", "j", "ui/select_next", "old")];
        let mut store = InputStore::new(bindings);
        let (dispatcher, log) = mock_dispatcher();
        store.set_dispatcher(dispatcher);

        let mut bind_val = BTreeMap::new();
        bind_val.insert("mode".to_string(), Value::String("normal".to_string()));
        bind_val.insert("key".to_string(), Value::String("j".to_string()));
        bind_val.insert(
            "target".to_string(),
            Value::String("ui/scroll_down".to_string()),
        );
        bind_val.insert("description".to_string(), Value::String("new".to_string()));
        store
            .write(&path!("bind"), Record::parsed(Value::Map(bind_val)))
            .unwrap();

        store
            .write(&path!("key"), key_event("normal", "j", "inbox"))
            .unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log[0].0, "ui/scroll_down");
    }

    #[test]
    fn unbind_removes_binding() {
        let bindings = vec![simple_binding("normal", "j", "ui/select_next", "down")];
        let mut store = InputStore::new(bindings);

        let mut unbind_val = BTreeMap::new();
        unbind_val.insert("mode".to_string(), Value::String("normal".to_string()));
        unbind_val.insert("key".to_string(), Value::String("j".to_string()));
        store
            .write(&path!("unbind"), Record::parsed(Value::Map(unbind_val)))
            .unwrap();

        assert_eq!(store.bindings.len(), 0);
    }

    #[test]
    fn macro_bind_creates_sequence() {
        let mut store = InputStore::new(vec![]);
        let (dispatcher, log) = mock_dispatcher();
        store.set_dispatcher(dispatcher);

        let mut macro_val = BTreeMap::new();
        macro_val.insert("mode".to_string(), Value::String("normal".to_string()));
        macro_val.insert("key".to_string(), Value::String("G".to_string()));
        macro_val.insert(
            "description".to_string(),
            Value::String("go home".to_string()),
        );
        macro_val.insert(
            "steps".to_string(),
            Value::Array(vec![
                Value::Map({
                    let mut s = BTreeMap::new();
                    s.insert("target".to_string(), Value::String("ui/close".to_string()));
                    s
                }),
                Value::Map({
                    let mut s = BTreeMap::new();
                    s.insert(
                        "target".to_string(),
                        Value::String("ui/set_status".to_string()),
                    );
                    s
                }),
            ]),
        );
        store
            .write(&path!("macro"), Record::parsed(Value::Map(macro_val)))
            .unwrap();

        store
            .write(&path!("key"), key_event("normal", "G", "inbox"))
            .unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].0, "ui/close");
        assert_eq!(log[1].0, "ui/set_status");
    }

    // -- Error cases --

    #[test]
    fn dispatch_without_dispatcher_returns_error() {
        let bindings = vec![simple_binding("normal", "j", "ui/select_next", "down")];
        let mut store = InputStore::new(bindings);
        let result = store.write(&path!("key"), key_event("normal", "j", "inbox"));
        assert!(result.is_err());
    }

    #[test]
    fn key_event_missing_mode_returns_error() {
        let mut store = InputStore::new(vec![]);
        let (dispatcher, _) = mock_dispatcher();
        store.set_dispatcher(dispatcher);

        let mut bad = BTreeMap::new();
        bad.insert("key".to_string(), Value::String("j".to_string()));
        let result = store.write(&path!("key"), Record::parsed(Value::Map(bad)));
        assert!(result.is_err());
    }

    // -- Invoke tests --

    #[test]
    fn dispatch_invoke_action() {
        let bindings = vec![Binding {
            context: BindingContext {
                mode: "normal".to_string(),
                key: "q".to_string(),
                screen: None,
            },
            action: Action::Invoke {
                command: "quit".to_string(),
                args: BTreeMap::new(),
            },
            description: "quit".to_string(),
        }];
        let mut store = InputStore::new(bindings);
        let (dispatcher, log) = mock_dispatcher();
        store.set_dispatcher(dispatcher);

        store
            .write(&path!("key"), key_event("normal", "q", "inbox"))
            .unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, "command/invoke");
    }

    #[test]
    fn dispatch_invoke_with_args() {
        let mut args = BTreeMap::new();
        args.insert("context".to_string(), serde_json::Value::String("compose".to_string()));
        let bindings = vec![Binding {
            context: BindingContext {
                mode: "normal".to_string(),
                key: "c".to_string(),
                screen: None,
            },
            action: Action::Invoke {
                command: "compose".to_string(),
                args,
            },
            description: "compose".to_string(),
        }];
        let mut store = InputStore::new(bindings);
        let (dispatcher, log) = mock_dispatcher();
        store.set_dispatcher(dispatcher);

        store
            .write(&path!("key"), key_event("normal", "c", "inbox"))
            .unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log[0].0, "command/invoke");
        // The dispatched value should contain the invocation
        assert!(log[0].1.contains_key("command"));
    }
}

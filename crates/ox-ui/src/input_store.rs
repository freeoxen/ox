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
use ox_types::CommandName;
use ox_types::ui::{Mode, Screen};
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

// ---------------------------------------------------------------------------
// Binding model
// ---------------------------------------------------------------------------

/// When a binding is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingContext {
    pub mode: Mode,
    pub key: String,
    /// If Some, only active on this screen. If None, active on all screens.
    pub screen: Option<Screen>,
}

/// What a binding does when activated.
#[derive(Debug, Clone)]
pub enum Action {
    /// Command invocation through the command registry.
    Invoke {
        command: CommandName,
        args: std::collections::BTreeMap<String, serde_json::Value>,
    },
    /// Execute a sequence of actions in order.
    Macro(Vec<Action>),
}

/// A complete binding: activation context + action + description.
#[derive(Debug, Clone)]
pub struct Binding {
    pub context: BindingContext,
    pub action: Action,
    pub description: String,
    /// If true, this binding appears in the status bar hint line.
    /// Only a few key actions per screen should have this set.
    pub status_hint: bool,
}

// ---------------------------------------------------------------------------
// InputStore
// ---------------------------------------------------------------------------

use crate::command::Dispatcher;

/// Server-side resolver that computes the dispatch mode from the
/// client-supplied modal flags (carried in the request map) and the
/// broker's own live state. Returns the mode as a string matching
/// `Mode::as_str`. Configured at broker-setup time.
///
/// Why this exists: the alternative is for the client to compute mode
/// from a (possibly stale) view-state snapshot and ship it. That loses
/// any state change that happened between snapshot and dispatch — most
/// painfully `approval/pending` arriving from the kernel prologue
/// after thread re-entry. Resolving server-side closes the race for
/// every broker-owned field at once.
pub type ModeResolver = Box<
    dyn FnMut(&BTreeMap<String, Value>, Option<&str>) -> Result<String, StoreError> + Send + Sync,
>;

pub struct InputStore {
    bindings: Vec<Binding>,
    dispatcher: Option<Dispatcher>,
    mode_resolver: Option<ModeResolver>,
}

impl InputStore {
    pub fn new(bindings: Vec<Binding>) -> Self {
        InputStore {
            bindings,
            dispatcher: None,
            mode_resolver: None,
        }
    }

    /// Set the command dispatcher.
    pub fn set_dispatcher(&mut self, dispatcher: Dispatcher) {
        self.dispatcher = Some(dispatcher);
    }

    /// Set the mode resolver. When configured, key writes that omit
    /// `mode` will route through the resolver instead of erroring.
    /// Key writes that include `mode` continue to use it directly
    /// (legacy back-compat for tests and macros).
    pub fn set_mode_resolver(&mut self, resolver: ModeResolver) {
        self.mode_resolver = Some(resolver);
    }

    // -- Binding resolution --

    /// Find the best binding for a (mode, key, screen) triple.
    ///
    /// Screen-specific bindings take priority over screen-agnostic ones.
    /// The incoming mode/screen strings are compared against the typed enum's
    /// string representation so that the serde boundary doesn't require
    /// deserialization into the enum.
    fn resolve(&self, mode: &str, key: &str, screen: Option<&str>) -> Option<&Binding> {
        let mut best: Option<&Binding> = None;
        for b in &self.bindings {
            if b.context.mode.as_str() != mode || b.context.key != key {
                continue;
            }
            match (&b.context.screen, screen) {
                // Exact screen match — highest priority, return immediately
                (Some(bs), Some(s)) if bs.as_str() == s => return Some(b),
                // Binding has no screen constraint — matches any screen
                (None, _) if best.is_none() => {
                    best = Some(b);
                }
                // Binding has screen constraint but doesn't match — skip
                _ => {}
            }
        }
        best
    }

    // -- Action execution --

    #[allow(clippy::only_used_in_recursion)]
    fn execute_action(
        &mut self,
        action: &Action,
        event_context: &BTreeMap<String, Value>,
    ) -> Result<Path, StoreError> {
        match action {
            Action::Invoke { command, args } => {
                // Build a CommandInvocation as a StructFS value and dispatch to command/invoke
                let inv = crate::command_def::CommandInvocation {
                    command: command.as_str().to_string(),
                    args: args.clone(),
                };
                let inv_value = structfs_serde_store::to_value(&inv).map_err(|e| {
                    StoreError::store(
                        "input",
                        "dispatch",
                        format!("failed to serialize invocation: {e}"),
                    )
                })?;
                let target = oxpath!("command", "invoke");
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
        map.insert(
            "mode".to_string(),
            Value::String(b.context.mode.as_str().to_string()),
        );
        map.insert("key".to_string(), Value::String(b.context.key.clone()));
        if let Some(s) = b.context.screen {
            map.insert("screen".to_string(), Value::String(s.as_str().to_string()));
        }
        map.insert(
            "description".to_string(),
            Value::String(b.description.clone()),
        );
        if b.status_hint {
            map.insert("status_hint".to_string(), Value::Bool(true));
        }
        match &b.action {
            Action::Invoke { command, .. } => {
                map.insert(
                    "command".to_string(),
                    Value::String(command.as_str().to_string()),
                );
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
                mode.is_none_or(|m| b.context.mode.as_str() == m)
                    && screen.is_none_or(|s| {
                        b.context.screen.is_none()
                            || b.context.screen.map(|sc| sc.as_str()) == Some(s)
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
        let mode_str = extract_str(map, "mode")
            .ok_or_else(|| StoreError::store("input", "bind", "missing mode"))?;
        let mode = Mode::parse(&mode_str).ok_or_else(|| {
            StoreError::store("input", "bind", format!("unknown mode: {mode_str}"))
        })?;
        let key = extract_str(map, "key")
            .ok_or_else(|| StoreError::store("input", "bind", "missing key"))?;
        let command_str = extract_str(map, "command")
            .or_else(|| extract_str(map, "target")) // backwards compat
            .ok_or_else(|| StoreError::store("input", "bind", "missing command"))?;
        let command = CommandName::parse(&command_str).ok_or_else(|| {
            StoreError::store("input", "bind", format!("unknown command: {command_str}"))
        })?;
        let description = extract_str(map, "description").unwrap_or_default();
        let screen = match extract_str(map, "screen") {
            Some(s) => Some(Screen::parse(&s).ok_or_else(|| {
                StoreError::store("input", "bind", format!("unknown screen: {s}"))
            })?),
            None => None,
        };

        let ctx = BindingContext { mode, key, screen };

        // Remove existing binding with same context
        self.bindings.retain(|b| b.context != ctx);

        // Parse args if present
        let args = match map.get("args") {
            Some(Value::Map(m)) => m
                .iter()
                .map(|(k, v)| (k.clone(), structfs_serde_store::value_to_json(v.clone())))
                .collect(),
            _ => BTreeMap::new(),
        };

        self.bindings.push(Binding {
            context: ctx,
            action: Action::Invoke { command, args },
            description,
            status_hint: false,
        });
        Ok(oxpath!("bindings"))
    }

    fn handle_unbind(&mut self, value: &Value) -> Result<Path, StoreError> {
        let map = match value {
            Value::Map(m) => m,
            _ => return Err(StoreError::store("input", "unbind", "expected Map")),
        };
        let mode_str = extract_str(map, "mode")
            .ok_or_else(|| StoreError::store("input", "unbind", "missing mode"))?;
        let key = extract_str(map, "key")
            .ok_or_else(|| StoreError::store("input", "unbind", "missing key"))?;
        let screen_str = extract_str(map, "screen");

        let before = self.bindings.len();
        self.bindings.retain(|b| {
            !(b.context.mode.as_str() == mode_str
                && b.context.key == key
                && b.context.screen.map(|s| s.as_str().to_string()) == screen_str)
        });
        if self.bindings.len() == before {
            return Err(StoreError::store("input", "unbind", "binding not found"));
        }
        Ok(oxpath!("bindings"))
    }

    fn handle_macro_bind(&mut self, value: &Value) -> Result<Path, StoreError> {
        let map = match value {
            Value::Map(m) => m,
            _ => return Err(StoreError::store("input", "macro", "expected Map")),
        };
        let mode_str = extract_str(map, "mode")
            .ok_or_else(|| StoreError::store("input", "macro", "missing mode"))?;
        let mode = Mode::parse(&mode_str).ok_or_else(|| {
            StoreError::store("input", "macro", format!("unknown mode: {mode_str}"))
        })?;
        let key = extract_str(map, "key")
            .ok_or_else(|| StoreError::store("input", "macro", "missing key"))?;
        let description = extract_str(map, "description").unwrap_or_default();
        let screen = match extract_str(map, "screen") {
            Some(s) => Some(Screen::parse(&s).ok_or_else(|| {
                StoreError::store("input", "macro", format!("unknown screen: {s}"))
            })?),
            None => None,
        };

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
            let command_str = extract_str(step_map, "command")
                .or_else(|| extract_str(step_map, "target")) // backwards compat
                .ok_or_else(|| StoreError::store("input", "macro", "step missing command"))?;
            let command = CommandName::parse(&command_str).ok_or_else(|| {
                StoreError::store("input", "macro", format!("unknown command: {command_str}"))
            })?;
            let args = match step_map.get("args") {
                Some(Value::Map(m)) => m
                    .iter()
                    .map(|(k, v)| (k.clone(), structfs_serde_store::value_to_json(v.clone())))
                    .collect(),
                _ => BTreeMap::new(),
            };
            steps.push(Action::Invoke { command, args });
        }

        let ctx = BindingContext { mode, key, screen };
        self.bindings.retain(|b| b.context != ctx);
        self.bindings.push(Binding {
            context: ctx,
            action: Action::Macro(steps),
            description,
            status_hint: false,
        });
        Ok(oxpath!("bindings"))
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
                // Key event dispatch: data is {mode?, key, screen?, ...flags}.
                // When `mode` is present we use it directly (legacy
                // back-compat: tests and macros still ship explicit mode).
                // When absent, we route through the configured
                // `mode_resolver`, which combines the request's
                // client-local modal flags with the broker's own live
                // state. This is the path that closes
                // snapshot-vs-dispatch races for the TUI client.
                let map = match value {
                    Value::Map(m) => m,
                    _ => return Err(StoreError::store("input", "key", "key event must be a Map")),
                };
                let key = extract_str(map, "key")
                    .ok_or_else(|| StoreError::store("input", "key", "missing key"))?;
                let screen = extract_str(map, "screen");

                let mode = match extract_str(map, "mode") {
                    Some(m) => m,
                    None => {
                        let resolver = self.mode_resolver.as_mut().ok_or_else(|| {
                            StoreError::store(
                                "input",
                                "key",
                                "missing mode and no mode_resolver configured",
                            )
                        })?;
                        resolver(map, screen.as_deref())?
                    }
                };

                // Encode the dispatch outcome in the returned Path so
                // the client can route unbound keys without recomputing
                // mode locally:
                //   `Ok("unbound/<mode>")` → no binding matched; the
                //                            client routes the key
                //                            through its mode-specific
                //                            text-input fallback using
                //                            the mode the broker
                //                            resolved (NOT the client's
                //                            stale snapshot).
                //   `Ok(<dispatcher path>)` → binding fired, dispatched
                //                             through the action chain.
                //   `Err(...)`              → genuine error (bad
                //                             request, dispatcher
                //                             failure).
                //
                // This keeps every focus decision on the server side.
                // See `local/plans/focus-resolution.md`.
                let binding = match self.resolve(&mode, &key, screen.as_deref()) {
                    Some(b) => b,
                    None => {
                        return Ok(Path::from_components(vec!["unbound".to_string(), mode]));
                    }
                };
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

    fn simple_binding(mode: Mode, key: &str, command: CommandName, desc: &str) -> Binding {
        Binding {
            context: BindingContext {
                mode,
                key: key.to_string(),
                screen: None,
            },
            action: Action::Invoke {
                command,
                args: BTreeMap::new(),
            },
            description: desc.to_string(),
            status_hint: false,
        }
    }

    fn screen_binding(
        mode: Mode,
        key: &str,
        screen: Screen,
        command: CommandName,
        desc: &str,
    ) -> Binding {
        Binding {
            context: BindingContext {
                mode,
                key: key.to_string(),
                screen: Some(screen),
            },
            action: Action::Invoke {
                command,
                args: BTreeMap::new(),
            },
            description: desc.to_string(),
            status_hint: false,
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

    // -- Resolution tests --

    #[test]
    fn resolve_generic_binding() {
        let bindings = vec![simple_binding(
            Mode::Normal,
            "j",
            CommandName::SelectNext,
            "down",
        )];
        let store = InputStore::new(bindings);
        let b = store.resolve("normal", "j", Some("inbox")).unwrap();
        assert_eq!(b.description, "down");
    }

    #[test]
    fn resolve_screen_specific_wins() {
        let bindings = vec![
            simple_binding(Mode::Normal, "j", CommandName::SelectNext, "generic"),
            screen_binding(
                Mode::Normal,
                "j",
                Screen::Thread,
                CommandName::ScrollDown,
                "scroll",
            ),
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
        let bindings = vec![simple_binding(
            Mode::Normal,
            "j",
            CommandName::SelectNext,
            "down",
        )];
        let mut store = InputStore::new(bindings);
        let (dispatcher, log) = mock_dispatcher();
        store.set_dispatcher(dispatcher);

        store
            .write(&path!("key"), key_event("normal", "j", "inbox"))
            .unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, "command/invoke");
        assert!(log[0].1.contains_key("command"));
    }

    #[test]
    fn dispatch_screen_specific() {
        let bindings = vec![
            screen_binding(
                Mode::Normal,
                "j",
                Screen::Inbox,
                CommandName::SelectNext,
                "down",
            ),
            screen_binding(
                Mode::Normal,
                "j",
                Screen::Thread,
                CommandName::ScrollDown,
                "scroll",
            ),
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
        assert_eq!(log[0].0, "command/invoke");
        assert_eq!(log[1].0, "command/invoke");
    }

    #[test]
    fn dispatch_no_binding_returns_unbound_with_resolved_mode() {
        // Unbound keys are not errors — they return a Path encoding
        // the resolved mode so the client can route through its
        // mode-specific text-input fallback. See `crate::InputStore`
        // doc on the "key" write action.
        let mut store = InputStore::new(vec![]);
        let (dispatcher, _) = mock_dispatcher();
        store.set_dispatcher(dispatcher);

        let path = store
            .write(&path!("key"), key_event("normal", "z", "inbox"))
            .expect("unbound is Ok with an `unbound/<mode>` path");
        assert_eq!(path.components.len(), 2);
        assert_eq!(path.components[0].as_str(), "unbound");
        assert_eq!(path.components[1].as_str(), "normal");
    }

    // -- Macro tests --

    #[test]
    fn dispatch_macro_executes_steps() {
        let bindings = vec![Binding {
            context: BindingContext {
                mode: Mode::Normal,
                key: "G".to_string(),
                screen: None,
            },
            action: Action::Macro(vec![
                Action::Invoke {
                    command: CommandName::Close,
                    args: BTreeMap::new(),
                },
                Action::Invoke {
                    command: CommandName::ScrollToBottom,
                    args: BTreeMap::new(),
                },
            ]),
            description: "go home".to_string(),
            status_hint: false,
        }];
        let mut store = InputStore::new(bindings);
        let (dispatcher, log) = mock_dispatcher();
        store.set_dispatcher(dispatcher);

        store
            .write(&path!("key"), key_event("normal", "G", "thread"))
            .unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].0, "command/invoke");
        assert_eq!(log[1].0, "command/invoke");
    }

    // -- Read tests --

    #[test]
    fn read_all_bindings() {
        let bindings = vec![
            simple_binding(Mode::Normal, "j", CommandName::SelectNext, "down"),
            simple_binding(Mode::Insert, "Esc", CommandName::ExitInsert, "exit"),
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
            simple_binding(Mode::Normal, "j", CommandName::SelectNext, "down"),
            simple_binding(Mode::Normal, "k", CommandName::SelectPrev, "up"),
            simple_binding(Mode::Insert, "Esc", CommandName::ExitInsert, "exit"),
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
            simple_binding(Mode::Normal, "j", CommandName::SelectNext, "generic j"),
            screen_binding(
                Mode::Normal,
                "j",
                Screen::Thread,
                CommandName::ScrollDown,
                "thread j",
            ),
            simple_binding(Mode::Normal, "k", CommandName::SelectPrev, "generic k"),
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
        bind_val.insert("command".to_string(), Value::String("close".to_string()));
        bind_val.insert(
            "description".to_string(),
            Value::String("close".to_string()),
        );
        store
            .write(&path!("bind"), Record::parsed(Value::Map(bind_val)))
            .unwrap();

        // Verify it dispatches through command/invoke
        store
            .write(&path!("key"), key_event("normal", "x", "inbox"))
            .unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log[0].0, "command/invoke");
        assert!(log[0].1.contains_key("command"));
    }

    #[test]
    fn bind_replaces_existing() {
        let bindings = vec![simple_binding(
            Mode::Normal,
            "j",
            CommandName::SelectNext,
            "old",
        )];
        let mut store = InputStore::new(bindings);
        let (dispatcher, _log) = mock_dispatcher();
        store.set_dispatcher(dispatcher);

        let mut bind_val = BTreeMap::new();
        bind_val.insert("mode".to_string(), Value::String("normal".to_string()));
        bind_val.insert("key".to_string(), Value::String("j".to_string()));
        bind_val.insert(
            "command".to_string(),
            Value::String(CommandName::ScrollDown.to_string()),
        );
        bind_val.insert("description".to_string(), Value::String("new".to_string()));
        store
            .write(&path!("bind"), Record::parsed(Value::Map(bind_val)))
            .unwrap();

        // Verify the new binding replaced the old one
        assert_eq!(store.bindings.len(), 1);
        match &store.bindings[0].action {
            Action::Invoke { command, .. } => assert_eq!(*command, CommandName::ScrollDown),
            _ => panic!("expected Invoke"),
        }
    }

    #[test]
    fn unbind_removes_binding() {
        let bindings = vec![simple_binding(
            Mode::Normal,
            "j",
            CommandName::SelectNext,
            "down",
        )];
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
                    s.insert("command".to_string(), Value::String("close".to_string()));
                    s
                }),
                Value::Map({
                    let mut s = BTreeMap::new();
                    s.insert(
                        "command".to_string(),
                        Value::String("scroll_to_bottom".to_string()),
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
        assert_eq!(log[0].0, "command/invoke");
        assert_eq!(log[1].0, "command/invoke");
    }

    // -- Error cases --

    #[test]
    fn dispatch_without_dispatcher_returns_error() {
        let bindings = vec![simple_binding(
            Mode::Normal,
            "j",
            CommandName::SelectNext,
            "down",
        )];
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
                mode: Mode::Normal,
                key: "q".to_string(),
                screen: None,
            },
            action: Action::Invoke {
                command: CommandName::Quit,
                args: BTreeMap::new(),
            },
            description: "quit".to_string(),
            status_hint: false,
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
        args.insert(
            "context".to_string(),
            serde_json::Value::String("compose".to_string()),
        );
        let bindings = vec![Binding {
            context: BindingContext {
                mode: Mode::Normal,
                key: "c".to_string(),
                screen: None,
            },
            action: Action::Invoke {
                command: CommandName::Compose,
                args,
            },
            description: "compose".to_string(),
            status_hint: false,
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

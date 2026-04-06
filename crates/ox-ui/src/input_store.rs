//! InputStore — translates key events into command writes.
//!
//! Holds a binding table mapping (mode, key) → (target_path, description).
//! Writes to `input/{mode}/{key}` trigger command dispatch through a
//! pluggable CommandDispatcher. Reads return the binding table for
//! help/discoverability.

use std::collections::BTreeMap;

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

/// A single key binding: which key in which mode triggers which command.
#[derive(Debug, Clone)]
pub struct Binding {
    pub mode: String,
    pub key: String,
    pub target: Path,
    pub description: String,
}

/// Callback for dispatching commands. Receives the target path and
/// command value, and performs the write (typically through a broker
/// ClientHandle).
pub type CommandDispatcher = Box<dyn FnMut(&Path, Record) -> Result<Path, StoreError> + Send + Sync>;

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

    /// Set the command dispatcher (called after mounting in broker).
    pub fn set_dispatcher(&mut self, dispatcher: CommandDispatcher) {
        self.dispatcher = Some(dispatcher);
    }

    fn next_txn(&mut self) -> String {
        self.txn_counter += 1;
        format!("input_{}", self.txn_counter)
    }

    fn bindings_for_mode(&self, mode: &str) -> Vec<Value> {
        self.bindings
            .iter()
            .filter(|b| b.mode == mode)
            .map(|b| {
                let mut map = BTreeMap::new();
                map.insert("key".to_string(), Value::String(b.key.clone()));
                map.insert("target".to_string(), Value::String(b.target.to_string()));
                map.insert(
                    "description".to_string(),
                    Value::String(b.description.clone()),
                );
                Value::Map(map)
            })
            .collect()
    }

    fn find_binding(&self, mode: &str, key: &str) -> Option<&Binding> {
        self.bindings
            .iter()
            .find(|b| b.mode == mode && b.key == key)
    }
}

impl Reader for InputStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let path_str = from.to_string();
        match path_str.as_str() {
            "normal" => Ok(Some(Record::parsed(Value::Array(
                self.bindings_for_mode("normal"),
            )))),
            "insert" => Ok(Some(Record::parsed(Value::Array(
                self.bindings_for_mode("insert"),
            )))),
            "approval" => Ok(Some(Record::parsed(Value::Array(
                self.bindings_for_mode("approval"),
            )))),
            _ => Ok(None),
        }
    }
}

impl Writer for InputStore {
    fn write(&mut self, to: &Path, _data: Record) -> Result<Path, StoreError> {
        // Path format: "{mode}/{key}"
        let path_str = to.to_string();
        let parts: Vec<&str> = path_str.splitn(2, '/').collect();
        if parts.len() != 2 {
            return Err(StoreError::store(
                "input",
                "write",
                "expected path format mode/key",
            ));
        }
        let (mode, key) = (parts[0], parts[1]);

        let binding = self.find_binding(mode, key).ok_or_else(|| {
            StoreError::store("input", "write", "no binding for key")
        })?;
        let target = binding.target.clone();

        let txn = self.next_txn();
        let mut cmd_map = BTreeMap::new();
        cmd_map.insert("txn".to_string(), Value::String(txn));

        let dispatcher = self.dispatcher.as_mut().ok_or_else(|| {
            StoreError::store("input", "write", "no dispatcher configured")
        })?;
        dispatcher(&target, Record::parsed(Value::Map(cmd_map)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use structfs_core_store::path;

    fn test_bindings() -> Vec<Binding> {
        vec![
            Binding {
                mode: "normal".to_string(),
                key: "j".to_string(),
                target: path!("ui/select_next"),
                description: "Move selection down".to_string(),
            },
            Binding {
                mode: "normal".to_string(),
                key: "k".to_string(),
                target: path!("ui/select_prev"),
                description: "Move selection up".to_string(),
            },
            Binding {
                mode: "insert".to_string(),
                key: "Esc".to_string(),
                target: path!("ui/exit_insert"),
                description: "Exit insert mode".to_string(),
            },
        ]
    }

    #[test]
    fn read_bindings_by_mode() {
        let mut store = InputStore::new(test_bindings());
        let normal = store.read(&path!("normal")).unwrap().unwrap();
        let arr = match normal.as_value().unwrap() {
            Value::Array(a) => a,
            _ => panic!("expected array"),
        };
        assert_eq!(arr.len(), 2); // j, k

        let insert = store.read(&path!("insert")).unwrap().unwrap();
        let arr = match insert.as_value().unwrap() {
            Value::Array(a) => a,
            _ => panic!("expected array"),
        };
        assert_eq!(arr.len(), 1); // Esc
    }

    #[test]
    fn write_dispatches_to_target() {
        let mut store = InputStore::new(test_bindings());

        let dispatched: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let dispatched_clone = dispatched.clone();
        store.set_dispatcher(Box::new(move |path, data| {
            let txn = data
                .as_value()
                .and_then(|v| match v {
                    Value::Map(m) => m.get("txn").and_then(|t| match t {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    }),
                    _ => None,
                })
                .unwrap_or_default();
            dispatched_clone
                .lock()
                .unwrap()
                .push((path.to_string(), txn));
            Ok(path.clone())
        }));

        store
            .write(&path!("normal/j"), Record::parsed(Value::Null))
            .unwrap();

        let log = dispatched.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, "ui/select_next");
        assert!(log[0].1.starts_with("input_"));
    }

    #[test]
    fn write_unknown_binding_returns_error() {
        let mut store = InputStore::new(test_bindings());
        store.set_dispatcher(Box::new(|path, _| Ok(path.clone())));

        let result = store.write(&path!("normal/z"), Record::parsed(Value::Null));
        assert!(result.is_err());
    }

    #[test]
    fn write_without_dispatcher_returns_error() {
        let mut store = InputStore::new(test_bindings());
        let result = store.write(&path!("normal/j"), Record::parsed(Value::Null));
        assert!(result.is_err());
    }
}

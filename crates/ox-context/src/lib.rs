//! Namespace store router and concrete providers for the ox agent framework.
//!
//! This crate provides [`Namespace`], a virtual filesystem that routes reads
//! and writes to mounted [`Store`] implementations by path prefix — similar to
//! how a Unix VFS mounts devices at path prefixes.
//!
//! One concrete provider is included:
//!
//! - [`SystemProvider`] — holds the system prompt string

use ox_path::oxpath;
use std::collections::BTreeMap;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Store, Value, Writer};

// ---------------------------------------------------------------------------
// Namespace — routes reads/writes to mounted stores by path prefix
// ---------------------------------------------------------------------------

/// A namespace routes reads and writes to mounted stores by path prefix.
///
/// Paths are split on the first component: `path!("history/messages")` routes
/// to the store mounted at `"history"` with sub-path `path!("messages")`.
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
}

impl Default for Namespace {
    fn default() -> Self {
        Self::new()
    }
}

impl Reader for Namespace {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
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
            Some(store) => {
                let sub_path = store.write(&sub, data)?;
                let mut components = vec![prefix.to_string()];
                components.extend(sub_path.components);
                Ok(Path::from_components(components))
            }
            None => Err(StoreError::NoRoute { path: to.clone() }),
        }
    }
}

/// Split a path into the first component (prefix) and the remaining sub-path.
fn split_path(path: &Path) -> (&str, Path) {
    if path.is_empty() {
        return ("", oxpath!());
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
                    Ok(oxpath!())
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

    // -- Integration: coordinator discovery via Namespace --
    // These tests mount all five store types to exercise the full RFC
    // discovery pattern through the Namespace router.

    fn build_full_namespace() -> Namespace {
        let shared_log = ox_kernel::log::SharedLog::new();
        let mut ns = Namespace::new();
        ns.mount(
            "system",
            Box::new(SystemProvider::new("You are helpful.".to_string())),
        );
        ns.mount("tools", Box::new(ox_tools::ToolStore::empty()));
        ns.mount("history", Box::new(ox_history::HistoryView::new(shared_log)));
        ns.mount("gate", Box::new(ox_gate::GateStore::new()));
        ns
    }

    #[test]
    fn namespace_snapshot_discovery_participating_stores() {
        let mut ns = build_full_namespace();

        // System and gate participate in snapshots
        assert!(ns.read(&path!("system/snapshot")).unwrap().is_some());
        assert!(ns.read(&path!("gate/snapshot")).unwrap().is_some());

        // History is persisted through the ledger, not snapshots
        assert!(ns.read(&path!("history/snapshot")).unwrap().is_none());
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
}

//! ConfigStore — single authority for configuration resolution across all scopes.
//!
//! Owns four layers resolved in priority order (highest wins):
//! 1. Ephemeral per-thread (session-only)
//! 2. Saved per-thread (persisted via StoreBacking)
//! 3. Global user setting (persisted via StoreBacking)
//! 4. System default (hardcoded)

use crate::command::{Command, TxnLog};
use std::collections::BTreeMap;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

pub struct ConfigStore {
    /// Layer 4: hardcoded system defaults.
    defaults: BTreeMap<String, Value>,
    /// Layer 3: global user settings.
    global: BTreeMap<String, Value>,
    /// Layer 2: per-thread saved overrides. Key = thread_id.
    thread_saved: BTreeMap<String, BTreeMap<String, Value>>,
    /// Layer 1: per-thread ephemeral overrides. Key = thread_id.
    thread_ephemeral: BTreeMap<String, BTreeMap<String, Value>>,
    txn_log: TxnLog,
}

/// Config keys that are valid settings.
const CONFIG_KEYS: [&str; 4] = ["model", "provider", "max_tokens", "api_key"];

impl ConfigStore {
    pub fn new(defaults: BTreeMap<String, Value>) -> Self {
        Self {
            defaults,
            global: BTreeMap::new(),
            thread_saved: BTreeMap::new(),
            thread_ephemeral: BTreeMap::new(),
            txn_log: TxnLog::new(),
        }
    }

    /// Resolve a config key through the 4-layer cascade for a given thread.
    fn resolve_for_thread(&self, thread_id: &str, key: &str) -> Option<Value> {
        // Layer 1: ephemeral per-thread
        if let Some(overrides) = self.thread_ephemeral.get(thread_id) {
            if let Some(val) = overrides.get(key) {
                return Some(val.clone());
            }
        }
        // Layer 2: saved per-thread
        if let Some(overrides) = self.thread_saved.get(thread_id) {
            if let Some(val) = overrides.get(key) {
                return Some(val.clone());
            }
        }
        // Layers 3+4: global resolution
        self.resolve_global(key)
    }

    /// Resolve a config key through layers 3→4 (global + default).
    fn resolve_global(&self, key: &str) -> Option<Value> {
        // Layer 3: global user setting
        if let Some(val) = self.global.get(key) {
            return Some(val.clone());
        }
        // Layer 4: system default
        self.defaults.get(key).cloned()
    }

    /// Build a map of all effective global values.
    fn effective_global_map(&self) -> Value {
        let mut map = BTreeMap::new();
        for key in &CONFIG_KEYS {
            if let Some(val) = self.resolve_global(key) {
                // Mask api_key
                if *key == "api_key" {
                    map.insert(key.to_string(), Value::String("***".into()));
                } else {
                    map.insert(key.to_string(), val);
                }
            }
        }
        Value::Map(map)
    }

    /// Parse a thread-scoped path: "threads/{id}/{rest...}"
    fn parse_thread_path(path: &Path) -> Option<(String, Path)> {
        if path.components.len() >= 2 && path.components[0] == "threads" {
            let thread_id = path.components[1].clone();
            let sub = Path::from_components(path.components[2..].to_vec());
            Some((thread_id, sub))
        } else {
            None
        }
    }
}

impl Reader for ConfigStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        // Thread-scoped reads: config/threads/{id}/{key}
        if let Some((thread_id, sub)) = Self::parse_thread_path(from) {
            let key = if sub.is_empty() {
                ""
            } else {
                sub.components[0].as_str()
            };
            if key.is_empty() {
                // Return effective map for this thread
                let mut map = BTreeMap::new();
                for k in &CONFIG_KEYS {
                    if let Some(val) = self.resolve_for_thread(&thread_id, k) {
                        if *k == "api_key" {
                            map.insert(k.to_string(), Value::String("***".into()));
                        } else {
                            map.insert(k.to_string(), val);
                        }
                    }
                }
                return Ok(Some(Record::parsed(Value::Map(map))));
            }
            let val = self.resolve_for_thread(&thread_id, key);
            return match val {
                Some(v) => {
                    if key == "api_key" {
                        Ok(Some(Record::parsed(Value::String("***".into()))))
                    } else {
                        Ok(Some(Record::parsed(v)))
                    }
                }
                None => Ok(None),
            };
        }

        // Global reads
        let key = if from.is_empty() {
            ""
        } else {
            from.components[0].as_str()
        };

        match key {
            "" | "effective" => Ok(Some(Record::parsed(self.effective_global_map()))),
            "defaults" => Ok(Some(Record::parsed(Value::Map(self.defaults.clone())))),
            "api_key" => {
                if self.global.contains_key("api_key") || self.defaults.contains_key("api_key") {
                    Ok(Some(Record::parsed(Value::String("***".into()))))
                } else {
                    Ok(None)
                }
            }
            "api_key_raw" => Ok(self.resolve_global("api_key").map(Record::parsed)),
            _ => Ok(self.resolve_global(key).map(Record::parsed)),
        }
    }
}

impl Writer for ConfigStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let value = data.as_value().ok_or_else(|| {
            StoreError::store("config", "write", "write data must contain a value")
        })?;
        let cmd = Command::parse(value)?;

        if let Some(ref txn) = cmd.txn {
            if self.txn_log.is_duplicate(txn) {
                return Ok(Path::from_components(vec![]));
            }
        }

        // Thread-scoped writes: config/threads/{id}/set_{key}
        if let Some((thread_id, sub)) = Self::parse_thread_path(to) {
            let command = if sub.is_empty() {
                ""
            } else {
                sub.components[0].as_str()
            };
            let val = cmd
                .get_str("value")
                .map(|s| Value::String(s.to_string()))
                .or_else(|| cmd.fields.get("value").cloned())
                .ok_or_else(|| StoreError::store("config", "write", "missing value field"))?;
            let scope = cmd.get_str("scope").unwrap_or("ephemeral");

            let key = match command {
                "set_model" => "model",
                "set_provider" => "provider",
                "set_max_tokens" => "max_tokens",
                "set_api_key" => "api_key",
                _ => {
                    return Err(StoreError::store(
                        "config",
                        "write",
                        format!("unknown thread config command: {command}"),
                    ));
                }
            };

            match scope {
                "saved" => {
                    self.thread_saved
                        .entry(thread_id)
                        .or_default()
                        .insert(key.to_string(), val);
                }
                _ => {
                    self.thread_ephemeral
                        .entry(thread_id)
                        .or_default()
                        .insert(key.to_string(), val);
                }
            }
            return Ok(to.clone());
        }

        // Global writes
        let command = if to.is_empty() {
            ""
        } else {
            to.components[0].as_str()
        };
        let val = cmd
            .get_str("value")
            .map(|s| Value::String(s.to_string()))
            .or_else(|| cmd.fields.get("value").cloned())
            .ok_or_else(|| StoreError::store("config", "write", "missing value field"))?;

        match command {
            "set_model" => {
                self.global.insert("model".to_string(), val);
            }
            "set_provider" => {
                self.global.insert("provider".to_string(), val);
            }
            "set_max_tokens" => {
                self.global.insert("max_tokens".to_string(), val);
            }
            "set_api_key" => {
                self.global.insert("api_key".to_string(), val);
            }
            "set_model_if_unset" => {
                if !self.global.contains_key("model") {
                    self.global.insert("model".to_string(), val);
                }
            }
            "set_provider_if_unset" => {
                if !self.global.contains_key("provider") {
                    self.global.insert("provider".to_string(), val);
                }
            }
            "set_max_tokens_if_unset" => {
                if !self.global.contains_key("max_tokens") {
                    self.global.insert("max_tokens".to_string(), val);
                }
            }
            _ => {
                return Err(StoreError::store(
                    "config",
                    "write",
                    format!("unknown config command: {command}"),
                ));
            }
        }
        Ok(to.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::{Reader, Writer, path};

    fn store_with_defaults() -> ConfigStore {
        let mut defaults = BTreeMap::new();
        defaults.insert(
            "model".to_string(),
            Value::String("claude-sonnet-4-20250514".into()),
        );
        defaults.insert("provider".to_string(), Value::String("anthropic".into()));
        defaults.insert("max_tokens".to_string(), Value::Integer(4096));
        ConfigStore::new(defaults)
    }

    fn cmd(pairs: &[(&str, Value)]) -> Record {
        let mut map = BTreeMap::new();
        for (k, v) in pairs {
            map.insert(k.to_string(), v.clone());
        }
        Record::parsed(Value::Map(map))
    }

    fn read_val(store: &mut ConfigStore, key: &str) -> Value {
        let p = structfs_core_store::Path::parse(key).unwrap();
        store.read(&p).unwrap().unwrap().as_value().unwrap().clone()
    }

    // -- Default resolution --

    #[test]
    fn read_returns_system_default() {
        let mut store = store_with_defaults();
        assert_eq!(
            read_val(&mut store, "model"),
            Value::String("claude-sonnet-4-20250514".into())
        );
        assert_eq!(
            read_val(&mut store, "provider"),
            Value::String("anthropic".into())
        );
        assert_eq!(read_val(&mut store, "max_tokens"), Value::Integer(4096));
    }

    // -- Global overrides default --

    #[test]
    fn global_setting_overrides_default() {
        let mut store = store_with_defaults();
        store
            .write(
                &path!("set_model"),
                cmd(&[("value", Value::String("gpt-4o".into()))]),
            )
            .unwrap();
        assert_eq!(
            read_val(&mut store, "model"),
            Value::String("gpt-4o".into())
        );
    }

    // -- Per-thread resolution --

    #[test]
    fn thread_read_falls_through_to_global() {
        let mut store = store_with_defaults();
        store
            .write(
                &path!("set_model"),
                cmd(&[("value", Value::String("gpt-4o".into()))]),
            )
            .unwrap();
        // Thread t_abc has no override — should get global value
        let p = structfs_core_store::Path::parse("threads/t_abc/model").unwrap();
        let val = store.read(&p).unwrap().unwrap().as_value().unwrap().clone();
        assert_eq!(val, Value::String("gpt-4o".into()));
    }

    #[test]
    fn thread_saved_override_wins_over_global() {
        let mut store = store_with_defaults();
        store
            .write(
                &path!("set_model"),
                cmd(&[("value", Value::String("gpt-4o".into()))]),
            )
            .unwrap();
        // Set saved per-thread override
        let p = structfs_core_store::Path::parse("threads/t_abc/set_model").unwrap();
        store
            .write(
                &p,
                cmd(&[
                    ("value", Value::String("claude-opus-4-20250514".into())),
                    ("scope", Value::String("saved".into())),
                ]),
            )
            .unwrap();
        // Thread read should return the override
        let rp = structfs_core_store::Path::parse("threads/t_abc/model").unwrap();
        let val = store
            .read(&rp)
            .unwrap()
            .unwrap()
            .as_value()
            .unwrap()
            .clone();
        assert_eq!(val, Value::String("claude-opus-4-20250514".into()));
    }

    #[test]
    fn thread_ephemeral_wins_over_saved() {
        let mut store = store_with_defaults();
        // Set saved override
        let p = structfs_core_store::Path::parse("threads/t_abc/set_model").unwrap();
        store
            .write(
                &p,
                cmd(&[
                    ("value", Value::String("saved-model".into())),
                    ("scope", Value::String("saved".into())),
                ]),
            )
            .unwrap();
        // Set ephemeral override (default scope)
        store
            .write(
                &p,
                cmd(&[("value", Value::String("ephemeral-model".into()))]),
            )
            .unwrap();
        let rp = structfs_core_store::Path::parse("threads/t_abc/model").unwrap();
        let val = store
            .read(&rp)
            .unwrap()
            .unwrap()
            .as_value()
            .unwrap()
            .clone();
        assert_eq!(val, Value::String("ephemeral-model".into()));
    }

    #[test]
    fn different_threads_independent() {
        let mut store = store_with_defaults();
        let p1 = structfs_core_store::Path::parse("threads/t_1/set_model").unwrap();
        store
            .write(&p1, cmd(&[("value", Value::String("model-a".into()))]))
            .unwrap();
        let p2 = structfs_core_store::Path::parse("threads/t_2/set_model").unwrap();
        store
            .write(&p2, cmd(&[("value", Value::String("model-b".into()))]))
            .unwrap();

        let r1 = structfs_core_store::Path::parse("threads/t_1/model").unwrap();
        let r2 = structfs_core_store::Path::parse("threads/t_2/model").unwrap();
        assert_eq!(
            store
                .read(&r1)
                .unwrap()
                .unwrap()
                .as_value()
                .unwrap()
                .clone(),
            Value::String("model-a".into())
        );
        assert_eq!(
            store
                .read(&r2)
                .unwrap()
                .unwrap()
                .as_value()
                .unwrap()
                .clone(),
            Value::String("model-b".into())
        );
    }

    #[test]
    fn api_key_masked_on_read() {
        let mut store = store_with_defaults();
        store
            .write(
                &path!("set_api_key"),
                cmd(&[("value", Value::String("sk-secret-key-123".into()))]),
            )
            .unwrap();
        assert_eq!(read_val(&mut store, "api_key"), Value::String("***".into()));
    }

    #[test]
    fn api_key_readable_via_raw_path() {
        let mut store = store_with_defaults();
        store
            .write(
                &path!("set_api_key"),
                cmd(&[("value", Value::String("sk-secret".into()))]),
            )
            .unwrap();
        // Internal read for workers — raw unmasked value
        assert_eq!(
            read_val(&mut store, "api_key_raw"),
            Value::String("sk-secret".into())
        );
    }

    #[test]
    fn set_global_if_unset_preserves_existing() {
        let mut store = store_with_defaults();
        // Set model to gpt-4o
        store
            .write(
                &path!("set_model"),
                cmd(&[("value", Value::String("gpt-4o".into()))]),
            )
            .unwrap();
        // set_if_unset should NOT overwrite
        store
            .write(
                &path!("set_model_if_unset"),
                cmd(&[("value", Value::String("claude-sonnet-4-20250514".into()))]),
            )
            .unwrap();
        assert_eq!(
            read_val(&mut store, "model"),
            Value::String("gpt-4o".into())
        );
    }

    #[test]
    fn set_global_if_unset_sets_when_empty() {
        let mut store = store_with_defaults();
        // No global model set yet — only default
        store
            .write(
                &path!("set_model_if_unset"),
                cmd(&[("value", Value::String("gpt-4o".into()))]),
            )
            .unwrap();
        assert_eq!(
            read_val(&mut store, "model"),
            Value::String("gpt-4o".into())
        );
    }

    #[test]
    fn read_all_returns_effective_map() {
        let mut store = store_with_defaults();
        let val = read_val(&mut store, "");
        match val {
            Value::Map(m) => {
                assert!(m.contains_key("model"));
                assert!(m.contains_key("provider"));
                assert!(m.contains_key("max_tokens"));
            }
            _ => panic!("expected Map"),
        }
    }
}

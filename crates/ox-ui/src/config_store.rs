//! ConfigStore — single authority for configuration resolution across all scopes.
//!
//! Three layers resolved in priority order (highest wins):
//! 1. Per-thread (ephemeral, session-only)
//! 2. Runtime global (runtime changes, persisted on explicit save)
//! 3. Base (figment-resolved startup values, immutable after init)
//!
//! Reads and writes use the same paths — no command paths.
//! Global: config/gate/model, config/gate/provider
//! Per-thread: config/threads/{id}/gate/model

use std::collections::BTreeMap;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

pub struct ConfigStore {
    /// Startup-resolved values (from figment or defaults). Immutable after init.
    base: BTreeMap<String, Value>,
    /// Runtime global changes (user-set during session).
    runtime: BTreeMap<String, Value>,
    /// Per-thread overrides. Key = thread_id, Value = path→value map.
    threads: BTreeMap<String, BTreeMap<String, Value>>,
    /// Optional persistence backing. None = purely in-memory.
    backing: Option<Box<dyn ox_store_util::StoreBacking>>,
}

impl ConfigStore {
    /// Create with base values (from figment resolution or defaults).
    pub fn new(base: BTreeMap<String, Value>) -> Self {
        Self {
            base,
            runtime: BTreeMap::new(),
            threads: BTreeMap::new(),
            backing: None,
        }
    }

    /// Create with base values and a persistence backing.
    /// Loads saved values from backing into the base layer.
    pub fn with_backing(
        mut base: BTreeMap<String, Value>,
        backing: Box<dyn ox_store_util::StoreBacking>,
    ) -> Self {
        if let Ok(Some(Value::Map(saved))) = backing.load() {
            for (k, v) in saved {
                base.insert(k, v);
            }
        }
        Self {
            base,
            runtime: BTreeMap::new(),
            threads: BTreeMap::new(),
            backing: Some(backing),
        }
    }

    /// Attach a persistence backing after construction.
    pub fn set_backing(&mut self, backing: Box<dyn ox_store_util::StoreBacking>) {
        self.backing = Some(backing);
    }

    /// Persist the runtime layer to backing. API keys excluded.
    pub fn save_runtime(&self) -> Result<(), StoreError> {
        let Some(ref backing) = self.backing else {
            return Ok(());
        };
        let filtered: BTreeMap<String, Value> = self
            .runtime
            .iter()
            .filter(|(k, _)| !k.contains("api_key"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        backing.save(&Value::Map(filtered))
    }

    /// Resolve a path through the global layers (runtime → base).
    fn resolve_global(&self, path: &str) -> Option<Value> {
        self.runtime
            .get(path)
            .or_else(|| self.base.get(path))
            .cloned()
    }

    /// Resolve a path for a specific thread (thread → runtime → base).
    fn resolve_for_thread(&self, thread_id: &str, path: &str) -> Option<Value> {
        if let Some(overrides) = self.threads.get(thread_id) {
            if let Some(val) = overrides.get(path) {
                return Some(val.clone());
            }
        }
        self.resolve_global(path)
    }

    /// Parse a thread-scoped path: "threads/{id}/{rest...}"
    fn parse_thread_path(path: &Path) -> Option<(String, String)> {
        if path.components.len() >= 2 && path.components[0] == "threads" {
            let thread_id = path.components[1].clone();
            let sub = path.components[2..].join("/");
            Some((thread_id, sub))
        } else {
            None
        }
    }

    /// If a path ends with `_raw`, strip the suffix and return the base path.
    /// This allows `gate/api_key_raw` to resolve the unmasked `gate/api_key`.
    fn strip_raw_suffix(path: &str) -> Option<&str> {
        path.strip_suffix("_raw")
    }

    /// Build a map of all effective global values.
    fn effective_map(&self) -> Value {
        let mut map = BTreeMap::new();
        // Merge base, then runtime on top
        for (k, v) in &self.base {
            if k.contains("api_key") {
                map.insert(k.clone(), Value::String("***".into()));
            } else {
                map.insert(k.clone(), v.clone());
            }
        }
        for (k, v) in &self.runtime {
            if k.contains("api_key") {
                map.insert(k.clone(), Value::String("***".into()));
            } else {
                map.insert(k.clone(), v.clone());
            }
        }
        Value::Map(map)
    }
}

impl Reader for ConfigStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        // Thread-scoped reads: threads/{id}/{path}
        if let Some((thread_id, sub)) = Self::parse_thread_path(from) {
            if sub.is_empty() {
                // Return all effective values for this thread
                let mut map = BTreeMap::new();
                for (k, v) in &self.base {
                    map.insert(k.clone(), v.clone());
                }
                for (k, v) in &self.runtime {
                    map.insert(k.clone(), v.clone());
                }
                if let Some(overrides) = self.threads.get(&thread_id) {
                    for (k, v) in overrides {
                        map.insert(k.clone(), v.clone());
                    }
                }
                // Mask api_key paths
                for (k, v) in map.iter_mut() {
                    if k.contains("api_key") {
                        *v = Value::String("***".into());
                    }
                }
                return Ok(Some(Record::parsed(Value::Map(map))));
            }
            // _raw suffix: resolve the base path unmasked
            if let Some(base_path) = Self::strip_raw_suffix(&sub) {
                return Ok(self
                    .resolve_for_thread(&thread_id, base_path)
                    .map(Record::parsed));
            }
            // Mask api_key on read
            if sub.contains("api_key") {
                return match self.resolve_for_thread(&thread_id, &sub) {
                    Some(_) => Ok(Some(Record::parsed(Value::String("***".into())))),
                    None => Ok(None),
                };
            }
            return Ok(self
                .resolve_for_thread(&thread_id, &sub)
                .map(Record::parsed));
        }

        // Global reads
        let path_str = from.to_string();
        if path_str.is_empty() {
            return Ok(Some(Record::parsed(self.effective_map())));
        }
        // _raw suffix: resolve the base path unmasked
        if let Some(base_path) = Self::strip_raw_suffix(&path_str) {
            return Ok(self.resolve_global(base_path).map(Record::parsed));
        }
        // Mask api_key on read
        if path_str.contains("api_key") {
            return match self.resolve_global(&path_str) {
                Some(_) => Ok(Some(Record::parsed(Value::String("***".into())))),
                None => Ok(None),
            };
        }
        Ok(self.resolve_global(&path_str).map(Record::parsed))
    }
}

impl Writer for ConfigStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let value = data
            .as_value()
            .ok_or_else(|| StoreError::store("config", "write", "expected parsed value"))?
            .clone();

        // Thread-scoped writes: threads/{id}/{path}
        if let Some((thread_id, sub)) = Self::parse_thread_path(to) {
            if sub.is_empty() {
                return Err(StoreError::store(
                    "config",
                    "write",
                    "cannot write to thread root",
                ));
            }
            self.threads
                .entry(thread_id)
                .or_default()
                .insert(sub, value);
            return Ok(to.clone());
        }

        // Global writes
        let path_str = to.to_string();
        if path_str.is_empty() {
            return Err(StoreError::store(
                "config",
                "write",
                "cannot write to config root",
            ));
        }

        // "save" command: persist runtime config to backing
        if path_str == "save" {
            return self
                .save_runtime()
                .map(|()| to.clone());
        }

        self.runtime.insert(path_str, value);
        Ok(to.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::{Reader, Writer, path};

    fn store_with_defaults() -> ConfigStore {
        let mut base = BTreeMap::new();
        base.insert(
            "gate/model".to_string(),
            Value::String("claude-sonnet-4-20250514".into()),
        );
        base.insert(
            "gate/provider".to_string(),
            Value::String("anthropic".into()),
        );
        base.insert("gate/max_tokens".to_string(), Value::Integer(4096));
        ConfigStore::new(base)
    }

    fn read_val(store: &mut ConfigStore, path_str: &str) -> Option<Value> {
        let p = Path::parse(path_str).unwrap();
        store
            .read(&p)
            .unwrap()
            .map(|r| r.as_value().unwrap().clone())
    }

    #[test]
    fn read_returns_base_default() {
        let mut store = store_with_defaults();
        assert_eq!(
            read_val(&mut store, "gate/model"),
            Some(Value::String("claude-sonnet-4-20250514".into()))
        );
        assert_eq!(
            read_val(&mut store, "gate/provider"),
            Some(Value::String("anthropic".into()))
        );
        assert_eq!(
            read_val(&mut store, "gate/max_tokens"),
            Some(Value::Integer(4096))
        );
    }

    #[test]
    fn runtime_write_overrides_base() {
        let mut store = store_with_defaults();
        store
            .write(
                &path!("gate/model"),
                Record::parsed(Value::String("gpt-4o".into())),
            )
            .unwrap();
        assert_eq!(
            read_val(&mut store, "gate/model"),
            Some(Value::String("gpt-4o".into()))
        );
    }

    #[test]
    fn thread_falls_through_to_global() {
        let mut store = store_with_defaults();
        store
            .write(
                &path!("gate/model"),
                Record::parsed(Value::String("gpt-4o".into())),
            )
            .unwrap();
        let p = Path::parse("threads/t_abc/gate/model").unwrap();
        let val = store.read(&p).unwrap().unwrap().as_value().unwrap().clone();
        assert_eq!(val, Value::String("gpt-4o".into()));
    }

    #[test]
    fn thread_override_wins() {
        let mut store = store_with_defaults();
        let p = Path::parse("threads/t_abc/gate/model").unwrap();
        store
            .write(&p, Record::parsed(Value::String("per-thread".into())))
            .unwrap();
        let val = store.read(&p).unwrap().unwrap().as_value().unwrap().clone();
        assert_eq!(val, Value::String("per-thread".into()));
        // Global unchanged
        assert_eq!(
            read_val(&mut store, "gate/model"),
            Some(Value::String("claude-sonnet-4-20250514".into()))
        );
    }

    #[test]
    fn different_threads_independent() {
        let mut store = store_with_defaults();
        let p1 = Path::parse("threads/t_1/gate/model").unwrap();
        let p2 = Path::parse("threads/t_2/gate/model").unwrap();
        store
            .write(&p1, Record::parsed(Value::String("model-a".into())))
            .unwrap();
        store
            .write(&p2, Record::parsed(Value::String("model-b".into())))
            .unwrap();
        assert_eq!(
            store
                .read(&p1)
                .unwrap()
                .unwrap()
                .as_value()
                .unwrap()
                .clone(),
            Value::String("model-a".into())
        );
        assert_eq!(
            store
                .read(&p2)
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
                &path!("gate/api_key"),
                Record::parsed(Value::String("sk-secret".into())),
            )
            .unwrap();
        assert_eq!(
            read_val(&mut store, "gate/api_key"),
            Some(Value::String("***".into()))
        );
    }

    #[test]
    fn api_key_raw_unmasked() {
        let mut store = store_with_defaults();
        store
            .write(
                &path!("gate/api_key"),
                Record::parsed(Value::String("sk-secret".into())),
            )
            .unwrap();
        assert_eq!(
            read_val(&mut store, "gate/api_key_raw"),
            Some(Value::String("sk-secret".into()))
        );
    }

    #[test]
    fn read_root_returns_effective_map() {
        let mut store = store_with_defaults();
        let val = read_val(&mut store, "").unwrap();
        match val {
            Value::Map(m) => {
                assert!(m.contains_key("gate/model"));
                assert!(m.contains_key("gate/provider"));
            }
            _ => panic!("expected Map"),
        }
    }

    #[test]
    fn unknown_path_returns_none() {
        let mut store = store_with_defaults();
        assert_eq!(read_val(&mut store, "nonexistent/path"), None);
    }

    #[test]
    fn thread_api_key_masked() {
        let mut store = store_with_defaults();
        store
            .write(
                &path!("gate/api_key"),
                Record::parsed(Value::String("sk-secret".into())),
            )
            .unwrap();
        let p = Path::parse("threads/t_abc/gate/api_key").unwrap();
        let val = store.read(&p).unwrap().unwrap().as_value().unwrap().clone();
        assert_eq!(val, Value::String("***".into()));
    }

    #[test]
    fn save_runtime_persists_to_backing() {
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct CaptureBacking {
            saved: Arc<Mutex<Option<Value>>>,
        }
        impl ox_store_util::StoreBacking for CaptureBacking {
            fn load(&self) -> Result<Option<Value>, StoreError> {
                Ok(None)
            }
            fn save(&self, value: &Value) -> Result<(), StoreError> {
                *self.saved.lock().unwrap() = Some(value.clone());
                Ok(())
            }
        }

        let saved = Arc::new(Mutex::new(None));
        let backing = CaptureBacking {
            saved: saved.clone(),
        };
        let mut config = ConfigStore::new(BTreeMap::new());
        config.set_backing(Box::new(backing));

        config
            .write(
                &path!("gate/model"),
                Record::parsed(Value::String("gpt-4o".into())),
            )
            .unwrap();
        config.save_runtime().unwrap();

        let saved_val = saved.lock().unwrap().clone().unwrap();
        match saved_val {
            Value::Map(m) => {
                assert_eq!(
                    m.get("gate/model").unwrap(),
                    &Value::String("gpt-4o".into())
                );
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn save_runtime_excludes_api_key() {
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct CaptureBacking {
            saved: Arc<Mutex<Option<Value>>>,
        }
        impl ox_store_util::StoreBacking for CaptureBacking {
            fn load(&self) -> Result<Option<Value>, StoreError> {
                Ok(None)
            }
            fn save(&self, value: &Value) -> Result<(), StoreError> {
                *self.saved.lock().unwrap() = Some(value.clone());
                Ok(())
            }
        }

        let saved = Arc::new(Mutex::new(None));
        let backing = CaptureBacking {
            saved: saved.clone(),
        };
        let mut config = ConfigStore::new(BTreeMap::new());
        config.set_backing(Box::new(backing));

        config
            .write(
                &path!("gate/api_key"),
                Record::parsed(Value::String("sk-secret".into())),
            )
            .unwrap();
        config
            .write(
                &path!("gate/model"),
                Record::parsed(Value::String("gpt-4o".into())),
            )
            .unwrap();
        config.save_runtime().unwrap();

        let saved_val = saved.lock().unwrap().clone().unwrap();
        match saved_val {
            Value::Map(m) => {
                assert!(
                    !m.contains_key("gate/api_key"),
                    "api_key must not be persisted"
                );
                assert!(m.contains_key("gate/model"));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn write_save_triggers_persistence() {
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct CaptureBacking {
            saved: Arc<Mutex<Option<Value>>>,
        }
        impl ox_store_util::StoreBacking for CaptureBacking {
            fn load(&self) -> Result<Option<Value>, StoreError> {
                Ok(None)
            }
            fn save(&self, value: &Value) -> Result<(), StoreError> {
                *self.saved.lock().unwrap() = Some(value.clone());
                Ok(())
            }
        }

        let saved = Arc::new(Mutex::new(None));
        let backing = CaptureBacking {
            saved: saved.clone(),
        };
        let mut config = ConfigStore::new(BTreeMap::new());
        config.set_backing(Box::new(backing));

        config
            .write(
                &path!("gate/model"),
                Record::parsed(Value::String("gpt-4o".into())),
            )
            .unwrap();

        // Write to "save" triggers persistence
        config
            .write(&path!("save"), Record::parsed(Value::Null))
            .unwrap();

        let saved_val = saved.lock().unwrap().clone().unwrap();
        match saved_val {
            Value::Map(m) => {
                assert_eq!(
                    m.get("gate/model").unwrap(),
                    &Value::String("gpt-4o".into())
                );
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn with_backing_loads_saved_values_into_base() {
        struct PreloadBacking;
        impl ox_store_util::StoreBacking for PreloadBacking {
            fn load(&self) -> Result<Option<Value>, StoreError> {
                let mut m = BTreeMap::new();
                m.insert("gate/model".to_string(), Value::String("from-disk".into()));
                Ok(Some(Value::Map(m)))
            }
            fn save(&self, _value: &Value) -> Result<(), StoreError> {
                Ok(())
            }
        }

        let mut config = ConfigStore::with_backing(BTreeMap::new(), Box::new(PreloadBacking));
        let record = config.read(&path!("gate/model")).unwrap().unwrap();
        assert_eq!(
            record.as_value().unwrap(),
            &Value::String("from-disk".into())
        );
    }
}

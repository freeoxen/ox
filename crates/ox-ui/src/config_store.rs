//! ConfigStore — layered configuration with optional persistence.
//!
//! Two layers resolved in priority order (highest wins):
//! 1. Runtime (user changes during session, persistable)
//! 2. Base (figment-resolved startup values, immutable after init)
//!
//! No masking — consumers that need masking use a `Masked` wrapper.
//! No thread scoping — threads use `Cascade<LocalConfig, ReadOnly<handle>>`.
//! Reads and writes use flat string keys (e.g. gate/defaults/model).

use std::collections::BTreeMap;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

pub struct ConfigStore {
    /// Immutable startup values (figment-resolved or defaults).
    base: BTreeMap<String, Value>,
    /// Runtime changes (user-set during session).
    runtime: BTreeMap<String, Value>,
    /// Optional persistence for the runtime layer.
    backing: Option<Box<dyn ox_store_util::StoreBacking>>,
}

impl ConfigStore {
    /// Create with base values (from figment resolution or defaults).
    pub fn new(base: BTreeMap<String, Value>) -> Self {
        Self {
            base,
            runtime: BTreeMap::new(),
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
            backing: Some(backing),
        }
    }

    /// Attach a persistence backing after construction.
    pub fn set_backing(&mut self, backing: Box<dyn ox_store_util::StoreBacking>) {
        self.backing = Some(backing);
    }

    /// Persist effective config (base + runtime) to backing.
    /// API keys and null-deleted entries are excluded.
    pub fn save_runtime(&self) -> Result<(), StoreError> {
        let Some(ref backing) = self.backing else {
            return Ok(());
        };
        // Merge base with runtime overrides
        let mut effective = self.base.clone();
        for (k, v) in &self.runtime {
            match v {
                Value::Null => {
                    effective.remove(k);
                }
                _ => {
                    effective.insert(k.clone(), v.clone());
                }
            }
        }
        // Remove entries with Null values and API keys
        let filtered: BTreeMap<String, Value> = effective
            .into_iter()
            .filter(|(k, v)| !k.ends_with("/key") && *v != Value::Null)
            .collect();
        tracing::info!(key_count = filtered.len(), "saving runtime config");
        backing.save(&Value::Map(filtered))
    }
}

impl Reader for ConfigStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = from.to_string();

        // Root read: return all effective values as a map
        if key.is_empty() {
            tracing::debug!("config root read");
            let mut map = BTreeMap::new();
            for (k, v) in &self.base {
                map.insert(k.clone(), v.clone());
            }
            for (k, v) in &self.runtime {
                map.insert(k.clone(), v.clone());
            }
            return Ok(Some(Record::parsed(Value::Map(map))));
        }

        // Cascade: runtime → base (Null in runtime = deleted)
        if let Some(v) = self.runtime.get(&key) {
            if *v == Value::Null {
                return Ok(None);
            }
            return Ok(Some(Record::parsed(v.clone())));
        }
        if let Some(v) = self.base.get(&key) {
            return Ok(Some(Record::parsed(v.clone())));
        }
        Ok(None)
    }
}

impl Writer for ConfigStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let key = to.to_string();
        if key.is_empty() {
            return Err(StoreError::store("config", "write", "cannot write to root"));
        }

        // "save" command: persist runtime to backing
        if key == "save" {
            return self.save_runtime().map(|()| to.clone());
        }

        if !key.ends_with("/key") {
            tracing::debug!(key = %key, "config write");
        }

        let value = data
            .as_value()
            .ok_or_else(|| StoreError::store("config", "write", "expected parsed value"))?
            .clone();
        self.runtime.insert(key, value);
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
    fn unknown_path_returns_none() {
        let mut store = store_with_defaults();
        assert_eq!(read_val(&mut store, "nonexistent/path"), None);
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
    fn api_key_not_masked() {
        let mut store = store_with_defaults();
        store
            .write(
                &path!("gate/accounts/anthropic/key"),
                Record::parsed(Value::String("sk-secret".into())),
            )
            .unwrap();
        // ConfigStore no longer masks — masking is the consumer's job
        assert_eq!(
            read_val(&mut store, "gate/accounts/anthropic/key"),
            Some(Value::String("sk-secret".into()))
        );
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
            Value::Map(m) => assert_eq!(
                m.get("gate/model").unwrap(),
                &Value::String("gpt-4o".into())
            ),
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
                &path!("gate/accounts/anthropic/key"),
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
                assert!(!m.contains_key("gate/accounts/anthropic/key"));
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
        config
            .write(&path!("save"), Record::parsed(Value::Null))
            .unwrap();
        let saved_val = saved.lock().unwrap().clone().unwrap();
        match saved_val {
            Value::Map(m) => assert_eq!(
                m.get("gate/model").unwrap(),
                &Value::String("gpt-4o".into())
            ),
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
            fn save(&self, _: &Value) -> Result<(), StoreError> {
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

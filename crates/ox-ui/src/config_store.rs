//! ConfigStore — layered configuration with optional persistence.
//!
//! Two layers resolved in priority order (highest wins):
//! 1. Runtime (user changes during session, persistable)
//! 2. Base (figment-resolved startup values, immutable after init)
//!
//! No masking — consumers that need masking use a `Masked` wrapper.
//! No thread scoping — threads use `Cascade<LocalConfig, ReadOnly<handle>>`.
//! Reads and writes use flat string keys (e.g. gate/completions/primary).

use std::collections::BTreeMap;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

pub struct ConfigStore {
    /// Immutable startup values (figment-resolved or defaults).
    base: BTreeMap<String, Value>,
    /// Runtime changes (user-set during session).
    runtime: BTreeMap<String, Value>,
    /// Snapshot of `runtime` at the most recent successful `save_runtime`.
    /// Used to compute "is the in-memory state ahead of disk?" without
    /// inspecting the file: dirty when `runtime != runtime_at_last_save`.
    /// Initialized to an empty map so a fresh ConfigStore with no writes
    /// reports clean.
    runtime_at_last_save: BTreeMap<String, Value>,
    /// Optional persistence for the runtime layer.
    backing: Option<Box<dyn ox_store_util::StoreBacking>>,
}

impl ConfigStore {
    /// Create with base values (from figment resolution or defaults).
    pub fn new(base: BTreeMap<String, Value>) -> Self {
        Self {
            base,
            runtime: BTreeMap::new(),
            runtime_at_last_save: BTreeMap::new(),
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
            runtime_at_last_save: BTreeMap::new(),
            backing: Some(backing),
        }
    }

    /// `true` when the in-memory runtime layer carries unsaved changes.
    /// `false` immediately after a successful `save_runtime` (and from
    /// startup, before any writes). Read via the `_dirty` sentinel
    /// path so consumers behind the broker can subscribe through the
    /// regular Reader trait.
    pub fn is_dirty(&self) -> bool {
        self.runtime != self.runtime_at_last_save
    }

    /// Attach a persistence backing after construction.
    pub fn set_backing(&mut self, backing: Box<dyn ox_store_util::StoreBacking>) {
        self.backing = Some(backing);
    }

    /// Persist effective config (base + runtime) to backing.
    /// Null-deleted entries are excluded. API keys live in a separate
    /// `secret/` mount with its own `JsonFileBacking` (chmod 0600); they
    /// don't reach this code path at all.
    ///
    /// On success snapshots `runtime` into `runtime_at_last_save` so
    /// subsequent `is_dirty` reads return false until the next write.
    /// `&mut self` because the snapshot update mutates internal state;
    /// callers that already have the broker actor's mutable borrow
    /// (e.g., the special-case in `Writer::write` for the `save` key)
    /// pass `&mut self` through.
    pub fn save_runtime(&mut self) -> Result<(), StoreError> {
        let Some(ref backing) = self.backing else {
            return Ok(());
        };
        // Expand any runtime Map entries into their flat sub-keys
        // before merging with base. Without this, a runtime entry like
        // `gate/providers/LMStudio` = Map({dialect: "anthropic", ...})
        // would coexist with base's flat `gate/providers/LMStudio/dialect`
        // = "openai" — both as separate BTreeMap keys, and base wins at
        // serialization because TomlFileBacking's insert_nested processes
        // entries in lex order (parent first, then sub-keys overwrite
        // the parent's fields). Flattening converts every runtime entry
        // into its leaf sub-keys so runtime cleanly shadows base at the
        // same key.
        let mut flat_runtime: BTreeMap<String, Value> = BTreeMap::new();
        for (k, v) in &self.runtime {
            flatten_value_into(k, v, &mut flat_runtime);
        }
        // Merge base with the flattened runtime overrides.
        let mut effective = self.base.clone();
        for (k, v) in flat_runtime {
            match v {
                Value::Null => {
                    effective.remove(&k);
                }
                _ => {
                    effective.insert(k, v);
                }
            }
        }
        // Drop any leftover Null sentinels from the base layer.
        let filtered: BTreeMap<String, Value> = effective
            .into_iter()
            .filter(|(_, v)| *v != Value::Null)
            .collect();
        tracing::info!(key_count = filtered.len(), "saving runtime config");
        backing.save(&Value::Map(filtered))?;
        // Capture the runtime snapshot only after the backing write
        // succeeds — a failed save leaves dirty=true so the user
        // doesn't get a false "saved" indicator.
        self.runtime_at_last_save = self.runtime.clone();
        Ok(())
    }
}

/// Recursively expand a value into flat-keyed leaves under `prefix`.
/// `Value::Map` recurses into its fields; everything else is treated as
/// a leaf and inserted at `prefix`. Used in save to decompose runtime
/// parent Maps into the flat sub-keys the TOML backing serializes
/// natively.
fn flatten_value_into(prefix: &str, value: &Value, out: &mut BTreeMap<String, Value>) {
    match value {
        Value::Map(m) => {
            for (k, v) in m {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{}/{}", prefix, k)
                };
                flatten_value_into(&path, v, out);
            }
        }
        _ => {
            out.insert(prefix.to_string(), value.clone());
        }
    }
}

impl Reader for ConfigStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = from.to_string();

        // Sentinel: `_dirty` returns the current dirty state as a Bool.
        // Surfaces the runtime-vs-last-saved comparison through the
        // standard Reader trait so consumers behind the broker (e.g.
        // the settings renderer) can read it without reaching for a
        // store-specific accessor. Underscore prefix keeps it out of
        // the user's identifier namespace.
        if key == "_dirty" {
            return Ok(Some(Record::parsed(Value::Bool(self.is_dirty()))));
        }

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

        tracing::debug!(key = %key, "config write");

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
    fn writes_pass_through_unfiltered() {
        // ConfigStore is not opinionated about its key shape — anything
        // its consumer writes can be read back, and (since A0) is also
        // saved as-is. Keys belong to the secrets store at `secret/`.
        let mut store = store_with_defaults();
        store
            .write(
                &path!("gate/custom/path"),
                Record::parsed(Value::String("anything".into())),
            )
            .unwrap();
        assert_eq!(
            read_val(&mut store, "gate/custom/path"),
            Some(Value::String("anything".into()))
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
    fn save_runtime_drops_null_sentinels() {
        // A Null write means "delete" — the persisted map must not carry
        // it through to disk, even if a base value existed under the
        // same path.
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
        let mut base = BTreeMap::new();
        base.insert("gate/old".to_string(), Value::String("kept".into()));
        let mut config = ConfigStore::new(base);
        config.set_backing(Box::new(backing));
        config
            .write(&path!("gate/old"), Record::parsed(Value::Null))
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
                assert!(!m.contains_key("gate/old"));
                assert!(m.contains_key("gate/model"));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn save_runtime_parent_map_shadows_base_flat_sub_keys() {
        // The protocol-cycle bug: base loads a TOML provider as flat
        // sub-keys (gate/providers/LMStudio/dialect = "openai", etc.).
        // The cycle command writes a parent ProviderConfig Map at
        // gate/providers/LMStudio = Map({dialect: "anthropic", ...}).
        // Without flattening, base's "openai" sub-key would survive
        // into the saved file and silently override the new dialect.
        // This test pins the fix.
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

        let mut base = BTreeMap::new();
        base.insert(
            "gate/providers/LMStudio/dialect".to_string(),
            Value::String("openai".into()),
        );
        base.insert(
            "gate/providers/LMStudio/endpoint".to_string(),
            Value::String("http://127.0.0.1:1234".into()),
        );

        let saved = Arc::new(Mutex::new(None));
        let backing = CaptureBacking {
            saved: saved.clone(),
        };
        let mut config = ConfigStore::new(base);
        config.set_backing(Box::new(backing));

        // Cycle writes the parent Map (not the sub-keys directly).
        let mut new_provider = BTreeMap::new();
        new_provider.insert("dialect".to_string(), Value::String("anthropic".into()));
        new_provider.insert(
            "endpoint".to_string(),
            Value::String("http://127.0.0.1:1234".into()),
        );
        config
            .write(
                &path!("gate/providers/LMStudio"),
                Record::parsed(Value::Map(new_provider)),
            )
            .unwrap();
        config.save_runtime().unwrap();

        let saved_val = saved.lock().unwrap().clone().unwrap();
        match saved_val {
            Value::Map(m) => {
                assert_eq!(
                    m.get("gate/providers/LMStudio/dialect").unwrap(),
                    &Value::String("anthropic".into()),
                    "runtime parent-Map's dialect must shadow base's flat sub-key"
                );
                assert_eq!(
                    m.get("gate/providers/LMStudio/endpoint").unwrap(),
                    &Value::String("http://127.0.0.1:1234".into())
                );
                // The parent-key shape is gone; it was decomposed into
                // its leaf sub-keys.
                assert!(!m.contains_key("gate/providers/LMStudio"));
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

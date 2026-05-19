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

use ox_store_util::flatten_value_into;

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

        // Compute the effective flat-keyed view once: base shadowed by
        // runtime, with runtime `Null` entries treated as tombstones
        // that hide the corresponding base keys. Downstream leaf /
        // children-walk logic operates on this post-tombstone snapshot
        // — recursion threads the same view through every level so the
        // O(N·D) clone per recursive call stays a single O(N) clone.
        let effective = self.effective_map();
        read_in_effective(&effective, &key)
    }
}

/// Project a flat-keyed effective map into the tree-of-Maps shape the
/// StructFS read contract expects, rooted at `key` (empty = root).
/// Recurses into nested levels by calling itself on the same `&effective`
/// view, so each top-level read pays the merge/clone cost exactly once.
fn read_in_effective(
    effective: &BTreeMap<String, Value>,
    key: &str,
) -> Result<Option<Record>, StoreError> {
    let has_leaf = !key.is_empty() && effective.contains_key(key);
    let child_prefix = if key.is_empty() {
        String::new()
    } else {
        format!("{key}/")
    };
    let has_children = effective
        .keys()
        .any(|k| k.starts_with(&child_prefix) && k != key);

    if has_leaf && has_children {
        return Err(StoreError::store(
            "config",
            "read",
            format!("malformed store: path {key:?} has both a leaf value and child entries"),
        ));
    }
    if has_leaf {
        return Ok(Some(Record::parsed(effective[key].clone())));
    }
    if !has_children {
        return Ok(None);
    }

    let mut heads: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for full_key in effective.keys() {
        let Some(rest) = full_key.strip_prefix(&child_prefix) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let head = match rest.split_once('/') {
            Some((h, _)) => h.to_string(),
            None => rest.to_string(),
        };
        heads.insert(head);
    }

    let mut children: BTreeMap<String, Value> = BTreeMap::new();
    for head in heads {
        let sub_key = if child_prefix.is_empty() {
            head.clone()
        } else {
            format!("{child_prefix}{head}")
        };
        let Some(rec) = read_in_effective(effective, &sub_key)? else {
            continue;
        };
        let child_value = rec
            .as_value()
            .cloned()
            .ok_or_else(|| StoreError::store("config", "read", "child record had no value"))?;
        children.insert(head, child_value);
    }

    Ok(Some(Record::parsed(Value::Map(children))))
}

impl ConfigStore {
    /// Project base ∪ runtime into a single flat-keyed effective map.
    /// Runtime `Null` entries are tombstones — they delete the
    /// corresponding base key and don't appear in the result. This is
    /// the post-tombstone view that all reads (leaf, children, root)
    /// operate against, so callers never observe a stale base value
    /// behind a fresh delete.
    fn effective_map(&self) -> BTreeMap<String, Value> {
        let mut out = self.base.clone();
        for (k, v) in &self.runtime {
            if *v == Value::Null {
                out.remove(k);
            } else {
                out.insert(k.clone(), v.clone());
            }
        }
        out
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

        // StructFS convention: writing `Value::Null` deletes `to` AND
        // its subtree. ConfigStore has an immutable base layer
        // underneath the mutable runtime, so the cascade has to mask
        // every base descendant with a runtime tombstone (so reads at
        // those paths return `Ok(None)`) and drop runtime descendants
        // that already exist (the runtime entry is gone, no need to
        // tombstone it).
        //
        // Component-aware prefix: deleting `accounts` must not touch
        // `accounts_other`. The `<key>/` separator check enforces
        // alignment at component boundaries.
        if matches!(value, Value::Null) {
            let descendant_prefix = format!("{}/", key);

            // Tombstone every base descendant that the runtime hasn't
            // already shadowed, then collect runtime descendants for
            // removal. We can't mutate `runtime` while iterating it,
            // so collect keys first.
            let runtime_descendants: Vec<String> = self
                .runtime
                .keys()
                .filter(|k| k.starts_with(&descendant_prefix))
                .cloned()
                .collect();
            for k in runtime_descendants {
                self.runtime.remove(&k);
            }
            for base_key in self.base.keys() {
                if base_key.starts_with(&descendant_prefix) {
                    self.runtime.insert(base_key.clone(), Value::Null);
                }
            }

            // The target itself: tombstone if base has anything to
            // shadow (either an exact key or any descendant), else
            // remove from runtime — `Ok(None)` is the natural read
            // result when no layer holds the path.
            let base_shadows = self.base.contains_key(&key)
                || self.base.keys().any(|k| k.starts_with(&descendant_prefix));
            if base_shadows {
                self.runtime.insert(key, Value::Null);
            } else {
                self.runtime.remove(&key);
            }
            return Ok(to.clone());
        }

        // StructFS convention: a path is either a leaf or a Map of
        // children — never both. A Map written at a parent path means
        // "this is the full state under here". Flattening into leaves
        // keeps the runtime in the same shape `save_runtime` produces
        // on disk, so reads stay consistent: no parent-Map leaf
        // coexisting with base flat sub-keys (which would trip the
        // read-side malformed-store check). Sweeps stale runtime
        // entries under the prefix (the new Map supersedes them) and
        // tombstones base sub-keys absent from the new Map (the Map's
        // intent is authoritative). The flatten then overwrites
        // tombstones at keys the Map declares.
        if matches!(value, Value::Map(_)) {
            let descendant_prefix = format!("{}/", key);
            let stale_runtime: Vec<String> = self
                .runtime
                .keys()
                .filter(|k| **k == key || k.starts_with(&descendant_prefix))
                .cloned()
                .collect();
            for k in stale_runtime {
                self.runtime.remove(&k);
            }
            // Tombstone every base entry the new Map supersedes — both
            // descendants (base sub-keys absent from the new Map stay
            // hidden) and a base leaf at the exact path (a prior
            // Map-shaped leaf is now superseded by the flattened
            // sub-keys, otherwise it would coexist with them and trip
            // the malformed-store read check).
            if self.base.contains_key(&key) {
                self.runtime.insert(key.clone(), Value::Null);
            }
            for base_key in self.base.keys() {
                if base_key.starts_with(&descendant_prefix) {
                    self.runtime.insert(base_key.clone(), Value::Null);
                }
            }
            // Flatten directly into `self.runtime`: the inserts must run
            // AFTER the tombstones in the same map so present-key
            // tombstones are overwritten with the new Map's values.
            flatten_value_into(&key, &value, &mut self.runtime);
            return Ok(to.clone());
        }

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
        // Root read projects the effective tree-of-Maps: top-level
        // components are the only direct keys, and sub-paths nest under
        // their head. Same convention as LocalConfig.
        let mut store = store_with_defaults();
        let val = read_val(&mut store, "").unwrap();
        match val {
            Value::Map(m) => {
                assert_eq!(m.len(), 1, "root has one immediate child: {m:?}");
                let gate = match m.get("gate").expect("gate present") {
                    Value::Map(g) => g,
                    other => panic!("expected gate to be a Map; got {other:?}"),
                };
                assert!(gate.contains_key("model"));
                assert!(gate.contains_key("provider"));
            }
            _ => panic!("expected Map"),
        }
    }

    #[test]
    fn read_at_non_leaf_returns_immediate_children_map() {
        let mut base = BTreeMap::new();
        base.insert(
            "gate/accounts/alpha/endpoint".to_string(),
            Value::String("https://alpha".into()),
        );
        base.insert(
            "gate/accounts/beta/endpoint".to_string(),
            Value::String("https://beta".into()),
        );
        base.insert("gate/other".to_string(), Value::String("other".into()));
        let mut store = ConfigStore::new(base);

        let rec = store
            .read(&Path::parse("gate/accounts").unwrap())
            .expect("ok")
            .expect("non-leaf returns Some");
        let map = match rec.as_value().expect("value") {
            Value::Map(m) => m.clone(),
            other => panic!("expected Map; got {other:?}"),
        };
        assert!(map.contains_key("alpha"));
        assert!(map.contains_key("beta"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn read_at_leaf_still_returns_leaf_value() {
        let mut store = store_with_defaults();
        let p = Path::parse("gate/model").unwrap();
        let rec = store.read(&p).unwrap().expect("leaf");
        assert_eq!(
            rec.as_value().unwrap(),
            &Value::String("claude-sonnet-4-20250514".into())
        );
    }

    #[test]
    fn read_at_missing_path_returns_none() {
        let mut store = store_with_defaults();
        let p = Path::parse("not/a/real/prefix").unwrap();
        assert!(store.read(&p).unwrap().is_none());
    }

    #[test]
    fn runtime_null_tombstones_filter_from_children_map() {
        // Runtime Null entries are tombstones — base values masked, fresh
        // deletes — and must not appear in the children-Map at a non-leaf
        // read. The walk operates on the post-tombstone effective map.
        let mut base = BTreeMap::new();
        base.insert(
            "gate/accounts/alpha/endpoint".to_string(),
            Value::String("a".into()),
        );
        base.insert(
            "gate/accounts/beta/endpoint".to_string(),
            Value::String("b".into()),
        );
        let mut store = ConfigStore::new(base);
        // Tombstone the whole `alpha` subtree via Writer::write with Null.
        store
            .write(
                &Path::parse("gate/accounts/alpha").unwrap(),
                Record::parsed(Value::Null),
            )
            .unwrap();

        let rec = store
            .read(&Path::parse("gate/accounts").unwrap())
            .unwrap()
            .expect("non-leaf");
        let map = match rec.as_value().unwrap() {
            Value::Map(m) => m.clone(),
            other => panic!("expected Map; got {other:?}"),
        };
        assert!(
            !map.contains_key("alpha"),
            "tombstoned alpha must not appear; map={map:?}"
        );
        assert!(map.contains_key("beta"));
    }

    #[test]
    fn runtime_null_tombstone_on_single_leaf_preserves_sibling_leaves() {
        // Tombstoning one leaf inside a multi-leaf child must not evict
        // the child from the parent's children-Map — the surviving
        // siblings keep the child alive — and the child's own
        // children-Map must omit just the tombstoned leaf.
        let mut base = BTreeMap::new();
        base.insert(
            "gate/accounts/alpha/endpoint".to_string(),
            Value::String("https://alpha".into()),
        );
        base.insert(
            "gate/accounts/alpha/token".to_string(),
            Value::String("tok".into()),
        );
        base.insert(
            "gate/accounts/beta/endpoint".to_string(),
            Value::String("https://beta".into()),
        );
        let mut store = ConfigStore::new(base);
        store
            .write(
                &Path::parse("gate/accounts/alpha/endpoint").unwrap(),
                Record::parsed(Value::Null),
            )
            .unwrap();

        // Parent of the tombstoned leaf still lists `alpha`, because
        // `alpha/token` survives.
        let accounts = match read_val(&mut store, "gate/accounts").unwrap() {
            Value::Map(m) => m,
            other => panic!("expected Map; got {other:?}"),
        };
        assert!(accounts.contains_key("alpha"));
        assert!(accounts.contains_key("beta"));

        // `alpha`'s own children-Map drops the tombstoned `endpoint`
        // but keeps `token`.
        let alpha = match read_val(&mut store, "gate/accounts/alpha").unwrap() {
            Value::Map(m) => m,
            other => panic!("expected Map; got {other:?}"),
        };
        assert!(!alpha.contains_key("endpoint"));
        assert_eq!(
            alpha.get("token"),
            Some(&Value::String("tok".into()))
        );
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
        // A runtime parent Map at e.g. gate/providers/LMStudio must
        // shadow base's flat sub-keys (gate/providers/LMStudio/dialect,
        // /endpoint, ...) at save time. Without `save_runtime`'s
        // flattening pass, base sub-keys survive into the saved file
        // and silently override the parent Map's fields — the
        // serializer processes parent first, then sub-keys overwrite
        // its members. This test pins the shadow ordering.
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

    #[test]
    fn write_map_at_parent_clears_base_sub_keys_not_in_map() {
        // Writing a Value::Map at a parent path declares "this is the
        // full picture under here" — base sub-keys absent from the new
        // Map must be tombstoned, so reads don't see stale base leaves
        // mixed in with the new Map's intent. And the parent itself
        // must read as the Map view without colliding with surviving
        // base sub-keys (no malformed leaf+children error).
        let mut base = BTreeMap::new();
        base.insert(
            "gate/providers/lm/dialect".to_string(),
            Value::String("openai".into()),
        );
        base.insert(
            "gate/providers/lm/endpoint".to_string(),
            Value::String("http://old".into()),
        );
        base.insert(
            "gate/providers/lm/auth".to_string(),
            Value::String("none".into()),
        );
        let mut store = ConfigStore::new(base);

        let mut new_map = BTreeMap::new();
        new_map.insert("dialect".to_string(), Value::String("x".into()));
        new_map.insert("endpoint".to_string(), Value::String("y".into()));
        store
            .write(
                &Path::parse("gate/providers/lm").unwrap(),
                Record::parsed(Value::Map(new_map)),
            )
            .unwrap();

        assert_eq!(read_val(&mut store, "gate/providers/lm/auth"), None);
        assert_eq!(
            read_val(&mut store, "gate/providers/lm/dialect"),
            Some(Value::String("x".into()))
        );
        assert_eq!(
            read_val(&mut store, "gate/providers/lm/endpoint"),
            Some(Value::String("y".into()))
        );
        let parent = read_val(&mut store, "gate/providers/lm").expect("parent reads cleanly");
        match parent {
            Value::Map(m) => {
                assert_eq!(m.get("dialect"), Some(&Value::String("x".into())));
                assert_eq!(m.get("endpoint"), Some(&Value::String("y".into())));
                assert!(!m.contains_key("auth"));
            }
            other => panic!("expected Map at parent; got {other:?}"),
        }
    }

    #[test]
    fn write_map_at_parent_drops_stale_runtime_sub_keys() {
        // A prior write left flat sub-keys in runtime. A subsequent
        // Map write at the parent declares the new full state — stale
        // runtime sub-keys must be cleared so the new Map fully
        // describes what reads see.
        let mut store = ConfigStore::new(BTreeMap::new());
        store
            .write(
                &Path::parse("gate/providers/lm/dialect").unwrap(),
                Record::parsed(Value::String("stale".into())),
            )
            .unwrap();
        store
            .write(
                &Path::parse("gate/providers/lm/endpoint").unwrap(),
                Record::parsed(Value::String("stale".into())),
            )
            .unwrap();

        let mut new_map = BTreeMap::new();
        new_map.insert("dialect".to_string(), Value::String("new".into()));
        store
            .write(
                &Path::parse("gate/providers/lm").unwrap(),
                Record::parsed(Value::Map(new_map)),
            )
            .unwrap();

        assert_eq!(read_val(&mut store, "gate/providers/lm/endpoint"), None);
        assert_eq!(
            read_val(&mut store, "gate/providers/lm/dialect"),
            Some(Value::String("new".into()))
        );
    }

    #[test]
    fn write_map_at_parent_supersedes_prior_leaf_at_same_path() {
        // An earlier write put a leaf at the same path. The Map write
        // supersedes it — the leaf must not coexist with the Map's
        // children, or reads at the path return the malformed error.
        let mut store = ConfigStore::new(BTreeMap::new());
        store
            .write(
                &Path::parse("gate/providers/lm").unwrap(),
                Record::parsed(Value::String("legacy-leaf".into())),
            )
            .unwrap();

        let mut new_map = BTreeMap::new();
        new_map.insert("dialect".to_string(), Value::String("x".into()));
        store
            .write(
                &Path::parse("gate/providers/lm").unwrap(),
                Record::parsed(Value::Map(new_map)),
            )
            .unwrap();

        let parent = read_val(&mut store, "gate/providers/lm").expect("parent reads cleanly");
        match parent {
            Value::Map(m) => {
                assert_eq!(m.get("dialect"), Some(&Value::String("x".into())));
                assert_eq!(m.len(), 1);
            }
            other => panic!("expected Map view; got {other:?}"),
        }
    }

    #[test]
    fn null_write_cascades_subtree_and_filters_root_read() {
        // StructFS convention: writing `Value::Null` at K1 deletes K1
        // AND every descendant. ConfigStore has an immutable base layer
        // underneath the runtime, so the cascade has to mask base
        // descendants with runtime tombstones; descendants written into
        // runtime are dropped outright. The root-read Map (the shape
        // `child_names_under` walks) must reflect the post-delete world.
        let mut base = BTreeMap::new();
        base.insert("gate/k1".to_string(), Value::String("base-k1".into()));
        base.insert(
            "gate/k1/sub1".to_string(),
            Value::String("base-sub1".into()),
        );
        base.insert(
            "gate/k1/sub2".to_string(),
            Value::String("base-sub2".into()),
        );
        // Sibling that shares the string prefix `gate/k1` up to a
        // non-separator character — guards the component-aware match.
        base.insert("gate/k1_other".to_string(), Value::String("sibling".into()));
        let mut config = ConfigStore::new(base);

        // Add a runtime entry under the doomed subtree to confirm both
        // layers get swept.
        config
            .write(
                &path!("gate/k1/sub2/deep"),
                Record::parsed(Value::String("runtime-deep".into())),
            )
            .unwrap();

        config
            .write(&path!("gate/k1"), Record::parsed(Value::Null))
            .unwrap();

        // Reads at K1 and every descendant return `Ok(None)`.
        assert!(read_val(&mut config, "gate/k1").is_none());
        assert!(read_val(&mut config, "gate/k1/sub1").is_none());
        assert!(read_val(&mut config, "gate/k1/sub2").is_none());
        assert!(read_val(&mut config, "gate/k1/sub2/deep").is_none());

        // Sibling untouched.
        assert_eq!(
            read_val(&mut config, "gate/k1_other"),
            Some(Value::String("sibling".into()))
        );

        // Root-read Map (now a tree-of-Maps) drops every key under the
        // deleted subtree but retains the sibling. `gate/k1*` keys nest
        // under `gate`, and the deleted `k1` subtree is absent there.
        let root = read_val(&mut config, "").unwrap();
        match root {
            Value::Map(m) => {
                let gate = match m.get("gate").expect("gate present") {
                    Value::Map(g) => g,
                    other => panic!("expected gate to be a Map; got {other:?}"),
                };
                assert!(!gate.contains_key("k1"));
                assert_eq!(
                    gate.get("k1_other"),
                    Some(&Value::String("sibling".into()))
                );
            }
            _ => panic!("expected Map"),
        }
    }
}

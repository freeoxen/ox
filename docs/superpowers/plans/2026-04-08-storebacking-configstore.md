# StoreBacking + ConfigStore Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a persistence abstraction (StoreBacking) and a layered configuration system (ConfigStore) that resolves config across global and per-thread scopes, eliminating config duplication in App and AgentPool.

**Architecture:** StoreBacking trait in ox-kernel with JsonFileBacking in ox-inbox. ConfigStore in ox-ui owns all config at all scopes (system defaults → global → saved per-thread → ephemeral per-thread), resolves reads by cascading. ThreadRegistry routes config-path reads to ConfigStore via broker client. Workers inherit config automatically instead of receiving it at spawn.

**Tech Stack:** Rust, structfs-core-store (Reader/Writer/Store/Value/Record/Path), ox-broker (ClientHandle, SyncClientAdapter), serde_json, structfs-serde-store

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/ox-kernel/src/backing.rs` | Create | `StoreBacking` trait definition |
| `crates/ox-kernel/src/lib.rs` | Modify | Re-export StoreBacking |
| `crates/ox-inbox/src/file_backing.rs` | Create | `JsonFileBacking` implementation |
| `crates/ox-inbox/src/lib.rs` | Modify | Export JsonFileBacking |
| `crates/ox-ui/src/config_store.rs` | Create | ConfigStore with 4-layer cascade resolution |
| `crates/ox-ui/src/lib.rs` | Modify | Export ConfigStore |
| `crates/ox-cli/src/broker_setup.rs` | Modify | Mount ConfigStore at `config/`, pass config params |
| `crates/ox-cli/src/thread_registry.rs` | Modify | Route config paths to ConfigStore via broker client |
| `crates/ox-cli/src/agents.rs` | Modify | Remove config writes at spawn, simplify AgentPool |
| `crates/ox-cli/src/app.rs` | Modify | Remove model/provider fields |
| `crates/ox-cli/src/view_state.rs` | Modify | Read config from broker |
| `crates/ox-cli/src/main.rs` | Modify | Pass config to broker_setup |
| `crates/ox-cli/src/event_loop.rs` | Modify | Remove App.model/provider usage |

---

### Task 1: StoreBacking Trait

**Files:**
- Create: `crates/ox-kernel/src/backing.rs`
- Modify: `crates/ox-kernel/src/lib.rs`

- [ ] **Step 1: Create backing.rs with the trait**

Create `crates/ox-kernel/src/backing.rs`:

```rust
//! StoreBacking — platform-agnostic persistence abstraction.
//!
//! Stores that want durability accept an optional `Box<dyn StoreBacking>`.
//! On construction, `load()` populates initial state. On writes that change
//! state, `save()` flushes the current snapshot. Stores without a backing
//! are purely in-memory.

use structfs_core_store::{Error as StoreError, Value};

/// Persistence abstraction for StructFS stores.
///
/// Implementations handle the mechanics of durability (files, IndexedDB,
/// REST API, etc.). The store handles caching and the read/write protocol.
pub trait StoreBacking: Send + Sync {
    /// Load the full persisted state. Returns None if no prior state exists.
    fn load(&self) -> Result<Option<Value>, StoreError>;

    /// Persist the full state atomically (overwrite).
    fn save(&self, value: &Value) -> Result<(), StoreError>;
}
```

- [ ] **Step 2: Re-export from ox-kernel/src/lib.rs**

Add to `crates/ox-kernel/src/lib.rs` after `pub mod snapshot;`:

```rust
pub mod backing;
pub use backing::StoreBacking;
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p ox-kernel`
Expected: Compiles.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-kernel/src/backing.rs crates/ox-kernel/src/lib.rs
git commit -m "feat(ox-kernel): add StoreBacking trait for platform-agnostic persistence"
```

---

### Task 2: JsonFileBacking Implementation

**Files:**
- Create: `crates/ox-inbox/src/file_backing.rs`
- Modify: `crates/ox-inbox/src/lib.rs`

- [ ] **Step 1: Write tests for JsonFileBacking**

Create `crates/ox-inbox/src/file_backing.rs` with tests first:

```rust
//! JsonFileBacking — file-based StoreBacking for CLI persistence.
//!
//! Stores state as a JSON file. Load parses it, save writes atomically
//! (write-to-temp + rename).

use ox_kernel::StoreBacking;
use std::path::PathBuf;
use structfs_core_store::{Error as StoreError, Value};
use structfs_serde_store::{json_to_value, value_to_json};

/// File-backed persistence using JSON serialization.
pub struct JsonFileBacking {
    path: PathBuf,
}

impl JsonFileBacking {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl StoreBacking for JsonFileBacking {
    fn load(&self) -> Result<Option<Value>, StoreError> {
        if !self.path.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(&self.path)
            .map_err(|e| StoreError::store("JsonFileBacking", "load", e.to_string()))?;
        let json: serde_json::Value = serde_json::from_str(&data)
            .map_err(|e| StoreError::store("JsonFileBacking", "load", e.to_string()))?;
        let value = json_to_value(json);
        Ok(Some(value))
    }

    fn save(&self, value: &Value) -> Result<(), StoreError> {
        let json = value_to_json(value);
        let data = serde_json::to_string_pretty(&json)
            .map_err(|e| StoreError::store("JsonFileBacking", "save", e.to_string()))?;

        // Atomic write: write to temp, rename
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StoreError::store("JsonFileBacking", "save", e.to_string()))?;
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, &data)
            .map_err(|e| StoreError::store("JsonFileBacking", "save", e.to_string()))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| StoreError::store("JsonFileBacking", "save", e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn load_nonexistent_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let backing = JsonFileBacking::new(dir.path().join("missing.json"));
        assert!(backing.load().unwrap().is_none());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        let backing = JsonFileBacking::new(path);

        let mut map = BTreeMap::new();
        map.insert("model".to_string(), Value::String("gpt-4o".into()));
        map.insert("max_tokens".to_string(), Value::Integer(8192));
        let original = Value::Map(map);

        backing.save(&original).unwrap();
        let loaded = backing.load().unwrap().unwrap();
        assert_eq!(loaded, original);
    }

    #[test]
    fn save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deep/config.json");
        let backing = JsonFileBacking::new(path.clone());

        backing.save(&Value::String("test".into())).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn save_is_atomic_no_partial_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        let backing = JsonFileBacking::new(path.clone());

        // Save initial
        backing.save(&Value::String("first".into())).unwrap();

        // Save again — tmp should not linger
        backing.save(&Value::String("second".into())).unwrap();
        assert!(!path.with_extension("tmp").exists());

        let loaded = backing.load().unwrap().unwrap();
        assert_eq!(loaded, Value::String("second".into()));
    }
}
```

- [ ] **Step 2: Export from ox-inbox/src/lib.rs**

Add to `crates/ox-inbox/src/lib.rs` after existing pub mods:

```rust
pub mod file_backing;
pub use file_backing::JsonFileBacking;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-inbox -- file_backing`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-inbox/src/file_backing.rs crates/ox-inbox/src/lib.rs
git commit -m "feat(ox-inbox): add JsonFileBacking for file-based store persistence"
```

---

### Task 3: ConfigStore with Layered Resolution

**Files:**
- Create: `crates/ox-ui/src/config_store.rs`
- Modify: `crates/ox-ui/src/lib.rs`

This is the core of the design. ConfigStore owns 4 layers and resolves reads by cascading.

- [ ] **Step 1: Write tests for ConfigStore**

Create `crates/ox-ui/src/config_store.rs` starting with tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::{path, Reader, Writer};

    fn store_with_defaults() -> ConfigStore {
        let mut defaults = BTreeMap::new();
        defaults.insert("model".to_string(), Value::String("claude-sonnet-4-20250514".into()));
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
        assert_eq!(read_val(&mut store, "model"), Value::String("claude-sonnet-4-20250514".into()));
        assert_eq!(read_val(&mut store, "provider"), Value::String("anthropic".into()));
        assert_eq!(read_val(&mut store, "max_tokens"), Value::Integer(4096));
    }

    // -- Global overrides default --

    #[test]
    fn global_setting_overrides_default() {
        let mut store = store_with_defaults();
        store.write(
            &path!("set_model"),
            cmd(&[("value", Value::String("gpt-4o".into()))]),
        ).unwrap();
        assert_eq!(read_val(&mut store, "model"), Value::String("gpt-4o".into()));
    }

    // -- Per-thread resolution --

    #[test]
    fn thread_read_falls_through_to_global() {
        let mut store = store_with_defaults();
        store.write(
            &path!("set_model"),
            cmd(&[("value", Value::String("gpt-4o".into()))]),
        ).unwrap();
        // Thread t_abc has no override — should get global value
        let p = structfs_core_store::Path::parse("threads/t_abc/model").unwrap();
        let val = store.read(&p).unwrap().unwrap().as_value().unwrap().clone();
        assert_eq!(val, Value::String("gpt-4o".into()));
    }

    #[test]
    fn thread_saved_override_wins_over_global() {
        let mut store = store_with_defaults();
        store.write(
            &path!("set_model"),
            cmd(&[("value", Value::String("gpt-4o".into()))]),
        ).unwrap();
        // Set saved per-thread override
        let p = structfs_core_store::Path::parse("threads/t_abc/set_model").unwrap();
        store.write(&p, cmd(&[
            ("value", Value::String("claude-opus-4-20250514".into())),
            ("scope", Value::String("saved".into())),
        ])).unwrap();
        // Thread read should return the override
        let rp = structfs_core_store::Path::parse("threads/t_abc/model").unwrap();
        let val = store.read(&rp).unwrap().unwrap().as_value().unwrap().clone();
        assert_eq!(val, Value::String("claude-opus-4-20250514".into()));
    }

    #[test]
    fn thread_ephemeral_wins_over_saved() {
        let mut store = store_with_defaults();
        // Set saved override
        let p = structfs_core_store::Path::parse("threads/t_abc/set_model").unwrap();
        store.write(&p, cmd(&[
            ("value", Value::String("saved-model".into())),
            ("scope", Value::String("saved".into())),
        ])).unwrap();
        // Set ephemeral override (default scope)
        store.write(&p, cmd(&[
            ("value", Value::String("ephemeral-model".into())),
        ])).unwrap();
        let rp = structfs_core_store::Path::parse("threads/t_abc/model").unwrap();
        let val = store.read(&rp).unwrap().unwrap().as_value().unwrap().clone();
        assert_eq!(val, Value::String("ephemeral-model".into()));
    }

    #[test]
    fn different_threads_independent() {
        let mut store = store_with_defaults();
        let p1 = structfs_core_store::Path::parse("threads/t_1/set_model").unwrap();
        store.write(&p1, cmd(&[("value", Value::String("model-a".into()))])).unwrap();
        let p2 = structfs_core_store::Path::parse("threads/t_2/set_model").unwrap();
        store.write(&p2, cmd(&[("value", Value::String("model-b".into()))])).unwrap();

        let r1 = structfs_core_store::Path::parse("threads/t_1/model").unwrap();
        let r2 = structfs_core_store::Path::parse("threads/t_2/model").unwrap();
        assert_eq!(
            store.read(&r1).unwrap().unwrap().as_value().unwrap().clone(),
            Value::String("model-a".into())
        );
        assert_eq!(
            store.read(&r2).unwrap().unwrap().as_value().unwrap().clone(),
            Value::String("model-b".into())
        );
    }

    #[test]
    fn api_key_masked_on_read() {
        let mut store = store_with_defaults();
        store.write(
            &path!("set_api_key"),
            cmd(&[("value", Value::String("sk-secret-key-123".into()))]),
        ).unwrap();
        assert_eq!(read_val(&mut store, "api_key"), Value::String("***".into()));
    }

    #[test]
    fn api_key_readable_via_raw_path() {
        let mut store = store_with_defaults();
        store.write(
            &path!("set_api_key"),
            cmd(&[("value", Value::String("sk-secret".into()))]),
        ).unwrap();
        // Internal read for workers — raw unmasked value
        assert_eq!(read_val(&mut store, "api_key_raw"), Value::String("sk-secret".into()));
    }

    #[test]
    fn set_global_if_unset_preserves_existing() {
        let mut store = store_with_defaults();
        // Set model to gpt-4o
        store.write(
            &path!("set_model"),
            cmd(&[("value", Value::String("gpt-4o".into()))]),
        ).unwrap();
        // set_if_unset should NOT overwrite
        store.write(
            &path!("set_model_if_unset"),
            cmd(&[("value", Value::String("claude-sonnet-4-20250514".into()))]),
        ).unwrap();
        assert_eq!(read_val(&mut store, "model"), Value::String("gpt-4o".into()));
    }

    #[test]
    fn set_global_if_unset_sets_when_empty() {
        let mut store = store_with_defaults();
        // No global model set yet — only default
        store.write(
            &path!("set_model_if_unset"),
            cmd(&[("value", Value::String("gpt-4o".into()))]),
        ).unwrap();
        assert_eq!(read_val(&mut store, "model"), Value::String("gpt-4o".into()));
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
```

- [ ] **Step 2: Implement ConfigStore**

Add the implementation above the tests in `crates/ox-ui/src/config_store.rs`:

```rust
//! ConfigStore — single authority for configuration resolution across all scopes.
//!
//! Owns four layers resolved in priority order (highest wins):
//! 1. Ephemeral per-thread (session-only)
//! 2. Saved per-thread (persisted via StoreBacking)
//! 3. Global user setting (persisted via StoreBacking)
//! 4. System default (hardcoded)

use std::collections::BTreeMap;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};
use crate::command::{Command, TxnLog};

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
            let val = cmd.get_str("value").map(|s| Value::String(s.to_string()))
                .or_else(|| cmd.fields.get("value").cloned())
                .ok_or_else(|| StoreError::store("config", "write", "missing value field"))?;
            let scope = cmd.get_str("scope").unwrap_or("ephemeral");

            let key = match command {
                "set_model" => "model",
                "set_provider" => "provider",
                "set_max_tokens" => "max_tokens",
                "set_api_key" => "api_key",
                _ => return Err(StoreError::store("config", "write", format!("unknown thread config command: {command}"))),
            };

            match scope {
                "saved" => {
                    self.thread_saved.entry(thread_id).or_default().insert(key.to_string(), val);
                }
                _ => {
                    self.thread_ephemeral.entry(thread_id).or_default().insert(key.to_string(), val);
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
        let val = cmd.get_str("value").map(|s| Value::String(s.to_string()))
            .or_else(|| cmd.fields.get("value").cloned())
            .ok_or_else(|| StoreError::store("config", "write", "missing value field"))?;

        match command {
            "set_model" => { self.global.insert("model".to_string(), val); }
            "set_provider" => { self.global.insert("provider".to_string(), val); }
            "set_max_tokens" => { self.global.insert("max_tokens".to_string(), val); }
            "set_api_key" => { self.global.insert("api_key".to_string(), val); }
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
            _ => return Err(StoreError::store("config", "write", format!("unknown config command: {command}"))),
        }
        Ok(to.clone())
    }
}
```

- [ ] **Step 3: Export from ox-ui/src/lib.rs**

Add to `crates/ox-ui/src/lib.rs`:

```rust
pub mod config_store;
pub use config_store::ConfigStore;
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-ui -- config_store`
Expected: All 10 tests pass.

- [ ] **Step 5: Run full ox-ui tests**

Run: `cargo test -p ox-ui`
Expected: All tests pass (64 existing + 10 new = 74).

- [ ] **Step 6: Commit**

```bash
git add crates/ox-ui/src/config_store.rs crates/ox-ui/src/lib.rs
git commit -m "feat(ox-ui): add ConfigStore with 4-layer cascading config resolution"
```

---

### Task 4: Mount ConfigStore in Broker

**Files:**
- Modify: `crates/ox-cli/src/broker_setup.rs`
- Modify: `crates/ox-cli/src/main.rs`

- [ ] **Step 1: Update broker_setup to accept config params and mount ConfigStore**

In `crates/ox-cli/src/broker_setup.rs`, update the `setup` function:

```rust
pub async fn setup(
    inbox: InboxStore,
    bindings: Vec<Binding>,
    inbox_root: std::path::PathBuf,
    provider: String,
    model: String,
    max_tokens: u32,
    api_key: String,
) -> BrokerHandle {
```

Add ConfigStore import and mounting after existing mounts (before the ThreadRegistry mount):

```rust
    use ox_ui::ConfigStore;
    use structfs_core_store::{Record, Value, Writer};

    // Mount ConfigStore with system defaults + initial global settings
    let mut defaults = std::collections::BTreeMap::new();
    defaults.insert("model".to_string(), Value::String("claude-sonnet-4-20250514".into()));
    defaults.insert("provider".to_string(), Value::String("anthropic".into()));
    defaults.insert("max_tokens".to_string(), Value::Integer(4096));

    let mut config = ConfigStore::new(defaults);
    // Set initial globals from CLI args (if_unset preserves persisted values later)
    config.write(
        &path!("set_provider_if_unset"),
        Record::parsed(Value::Map({
            let mut m = std::collections::BTreeMap::new();
            m.insert("value".to_string(), Value::String(provider));
            m
        })),
    ).ok();
    config.write(
        &path!("set_model_if_unset"),
        Record::parsed(Value::Map({
            let mut m = std::collections::BTreeMap::new();
            m.insert("value".to_string(), Value::String(model));
            m
        })),
    ).ok();
    config.write(
        &path!("set_max_tokens_if_unset"),
        Record::parsed(Value::Map({
            let mut m = std::collections::BTreeMap::new();
            m.insert("value".to_string(), Value::Integer(max_tokens as i64));
            m
        })),
    ).ok();
    // API key always from CLI/env, not persisted
    config.write(
        &path!("set_api_key"),
        Record::parsed(Value::Map({
            let mut m = std::collections::BTreeMap::new();
            m.insert("value".to_string(), Value::String(api_key));
            m
        })),
    ).ok();

    servers.push(broker.mount(path!("config"), config).await);
```

- [ ] **Step 2: Update main.rs to pass config params to setup**

In `crates/ox-cli/src/main.rs`, update the `setup` call:

```rust
    let broker_handle = rt.block_on(broker_setup::setup(
        broker_inbox,
        broker_bindings,
        inbox_root.clone(),
        cli.provider.clone(),
        model.clone(),
        cli.max_tokens,
        api_key.clone(),
    ));
```

Note: we still pass model/provider/api_key to App::new for now (AgentPool still needs them). That gets cleaned up in Task 6.

- [ ] **Step 3: Update broker_setup tests**

Update the `test_inbox()` helper and all test calls to `setup()` to include the new params:

```rust
    async fn test_setup() -> BrokerHandle {
        let bindings = crate::bindings::default_bindings();
        setup(
            test_inbox(),
            bindings,
            test_inbox_root(),
            "anthropic".into(),
            "claude-sonnet-4-20250514".into(),
            4096,
            "test-key".into(),
        ).await
    }
```

Update all 3 existing tests to use `test_setup()`.

Add a test for config reads:

```rust
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn config_store_mounted_with_defaults() {
        let handle = test_setup().await;
        let client = handle.client();

        let model = client.read(&path!("config/model")).await.unwrap().unwrap();
        assert_eq!(model.as_value().unwrap(), &Value::String("claude-sonnet-4-20250514".into()));

        let provider = client.read(&path!("config/provider")).await.unwrap().unwrap();
        assert_eq!(provider.as_value().unwrap(), &Value::String("anthropic".into()));
    }
```

- [ ] **Step 4: Verify compilation and tests**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: Compiles, all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/broker_setup.rs crates/ox-cli/src/main.rs
git commit -m "feat(ox-cli): mount ConfigStore in broker with CLI-arg initialization"
```

---

### Task 5: ThreadRegistry Config Routing

**Files:**
- Modify: `crates/ox-cli/src/thread_registry.rs`

This is the key integration: ThreadRegistry routes `model/` and `gate/accounts/` reads to ConfigStore through its broker client.

- [ ] **Step 1: Give ThreadRegistry a broker client**

ThreadRegistry needs a `ClientHandle` to read from `config/`. Update `ThreadRegistry::new`:

```rust
pub struct ThreadRegistry {
    threads: HashMap<String, ThreadNamespace>,
    inbox_root: PathBuf,
    broker_client: Option<ox_broker::ClientHandle>,
}

impl ThreadRegistry {
    pub fn new(inbox_root: PathBuf) -> Self {
        Self {
            threads: HashMap::new(),
            inbox_root,
            broker_client: None,
        }
    }

    /// Set the broker client for config resolution.
    pub fn set_broker_client(&mut self, client: ox_broker::ClientHandle) {
        self.broker_client = Some(client);
    }
```

- [ ] **Step 2: Add config path detection and routing**

Add a method to detect config paths and resolve through ConfigStore:

```rust
    /// Check if a sub-path is a config path that should resolve through ConfigStore.
    fn is_config_path(sub: &Path) -> bool {
        if sub.is_empty() {
            return false;
        }
        match sub.components[0].as_str() {
            "model" => true,
            "gate" if sub.components.len() >= 2 && sub.components[1] == "accounts" => true,
            _ => false,
        }
    }

    /// Resolve a config read through ConfigStore via broker.
    /// Maps threads/{id}/model/id → config/threads/{id}/model
    fn resolve_config_read(
        &self,
        thread_id: &str,
        sub: &Path,
    ) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let Some(client) = &self.broker_client else {
            // No broker client — fall back to local store
            return Box::pin(std::future::ready(Ok(None)));
        };

        // Map model/{key} → config/threads/{thread_id}/model (or model/id, etc.)
        let config_key = if sub.components[0] == "model" {
            if sub.components.len() <= 1 || sub.components[1] == "id" {
                "model".to_string()
            } else if sub.components[1] == "max_tokens" {
                "max_tokens".to_string()
            } else {
                // Unknown model sub-path — fall through to local
                return Box::pin(std::future::ready(Ok(None)));
            }
        } else {
            // gate/accounts paths — not yet resolved through config
            // (future work: API key resolution)
            return Box::pin(std::future::ready(Ok(None)));
        };

        let config_path = structfs_core_store::Path::parse(
            &format!("config/threads/{thread_id}/{config_key}")
        ).unwrap();
        let client = client.clone();
        Box::pin(async move {
            client.read(&config_path).await
        })
    }
```

- [ ] **Step 3: Update AsyncReader to use config routing for model paths**

Modify the `AsyncReader` impl's read method:

```rust
impl AsyncReader for ThreadRegistry {
    fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let Some((thread_id, sub)) = Self::split_thread_path(from) else {
            return Box::pin(std::future::ready(Ok(None)));
        };

        // Approval paths → async ApprovalStore
        if let Some(approval_sub) = Self::is_approval_path(&sub) {
            let ns = self.ensure_mounted(&thread_id);
            return ns.approval.read(&approval_sub);
        }

        // Config paths → resolve through ConfigStore via broker
        if Self::is_config_path(&sub) {
            return self.resolve_config_read(&thread_id, &sub);
        }

        // Everything else → local sync stores
        let ns = self.ensure_mounted(&thread_id);
        let result = ns.read(&sub);
        Box::pin(std::future::ready(result))
    }
}
```

- [ ] **Step 4: Update AsyncWriter for config writes**

Config writes also route to ConfigStore:

```rust
impl AsyncWriter for ThreadRegistry {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
        let Some((thread_id, sub)) = Self::split_thread_path(to) else {
            return Box::pin(std::future::ready(Err(StoreError::NoRoute {
                path: to.clone(),
            })));
        };

        // Approval paths → async ApprovalStore
        if let Some(approval_sub) = Self::is_approval_path(&sub) {
            let ns = self.ensure_mounted(&thread_id);
            return ns.approval.write(&approval_sub, data);
        }

        // Config writes (model/id, model/max_tokens) → route to ConfigStore
        if Self::is_config_path(&sub) {
            if let Some(client) = &self.broker_client {
                // Map model/id → config/threads/{thread_id}/set_model
                let config_cmd = if sub.components[0] == "model" {
                    if sub.components.len() <= 1 || sub.components[1] == "id" {
                        Some("set_model")
                    } else if sub.components[1] == "max_tokens" {
                        Some("set_max_tokens")
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(cmd_name) = config_cmd {
                    let config_path = structfs_core_store::Path::parse(
                        &format!("config/threads/{thread_id}/{cmd_name}")
                    ).unwrap();
                    let client = client.clone();
                    return Box::pin(async move {
                        client.write(&config_path, data).await
                    });
                }
            }
        }

        // Ensure thread is mounted for non-config writes
        let ns = self.ensure_mounted(&thread_id);
        let result = ns.write(&sub, data);
        Box::pin(std::future::ready(result))
    }
}
```

- [ ] **Step 5: Set broker client in broker_setup**

In `crates/ox-cli/src/broker_setup.rs`, after creating the ThreadRegistry but before mounting it, set the broker client:

```rust
    // Mount ThreadRegistry at threads/ — lazy-mounts per-thread stores from disk
    let mut registry = crate::thread_registry::ThreadRegistry::new(inbox_root);
    registry.set_broker_client(broker.client());
    servers.push(
        broker.mount_async(path!("threads"), registry).await,
    );
```

- [ ] **Step 6: Add integration test**

Add a test to `broker_setup.rs` tests:

```rust
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thread_model_resolves_through_config() {
        let handle = test_setup().await;
        let client = handle.client();

        // Read model for a thread — should fall through to global config
        let model = client
            .read(&path!("threads/t_test/model/id"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            model.as_value().unwrap(),
            &Value::String("claude-sonnet-4-20250514".into())
        );

        // Change global model
        let mut cmd = BTreeMap::new();
        cmd.insert("value".to_string(), Value::String("gpt-4o".into()));
        client
            .write(&path!("config/set_model"), Record::parsed(Value::Map(cmd)))
            .await
            .unwrap();

        // Thread should now see the new global model
        let model = client
            .read(&path!("threads/t_test/model/id"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            model.as_value().unwrap(),
            &Value::String("gpt-4o".into())
        );
    }
```

- [ ] **Step 7: Verify compilation and tests**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: Compiles, all tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/ox-cli/src/thread_registry.rs crates/ox-cli/src/broker_setup.rs
git commit -m "feat(ox-cli): route thread config paths to ConfigStore via broker"
```

---

### Task 6: Remove Config from AgentPool and App

**Files:**
- Modify: `crates/ox-cli/src/agents.rs`
- Modify: `crates/ox-cli/src/app.rs`
- Modify: `crates/ox-cli/src/view_state.rs`
- Modify: `crates/ox-cli/src/event_loop.rs`
- Modify: `crates/ox-cli/src/main.rs`

- [ ] **Step 1: Remove model/provider config writes from spawn_worker**

In `crates/ox-cli/src/agents.rs`, in the `spawn_worker` function (around lines 229-265):

Delete the model config writes:
```rust
    // DELETE these lines (229-241):
    // adapter.write(&path!("model/id"), ...).ok();
    // adapter.write(&path!("model/max_tokens"), ...).ok();
```

Delete the gate API key writes:
```rust
    // DELETE these lines (243-265):
    // adapter.write(&path!("gate/accounts/..."), ...).ok();
```

Workers now inherit model/max_tokens from ConfigStore's cascade (via ThreadRegistry routing). The API key is read from ConfigStore when the worker constructs its transport.

- [ ] **Step 2: Update worker transport construction to read from ConfigStore**

In `spawn_worker`, the worker needs to read config from the broker. It already has a `scoped_client`. Change the `ProviderConfig` construction and `api_key_for_transport` to read from the broker:

```rust
    // Read config from broker (resolves through ConfigStore via ThreadRegistry)
    let model = match adapter.read(&path!("model/id")) {
        Ok(Some(r)) => match r.as_value() {
            Some(Value::String(s)) => s.clone(),
            _ => "claude-sonnet-4-20250514".to_string(),
        },
        _ => "claude-sonnet-4-20250514".to_string(),
    };
    let max_tokens = match adapter.read(&path!("model/max_tokens")) {
        Ok(Some(r)) => match r.as_value() {
            Some(Value::Integer(n)) => *n as u32,
            _ => 4096,
        },
        _ => 4096,
    };

    // Read provider and API key from global config (unscoped)
    let broker_client = broker.client();
    let provider = tokio::task::block_in_place(|| {
        rt_handle.block_on(async {
            match broker_client.read(&structfs_core_store::path!("config/provider")).await {
                Ok(Some(r)) => match r.as_value() {
                    Some(Value::String(s)) => s.clone(),
                    _ => "anthropic".to_string(),
                },
                _ => "anthropic".to_string(),
            }
        })
    });
    let api_key_for_transport = tokio::task::block_in_place(|| {
        rt_handle.block_on(async {
            match broker_client.read(&structfs_core_store::path!("config/api_key_raw")).await {
                Ok(Some(r)) => match r.as_value() {
                    Some(Value::String(s)) => s.clone(),
                    _ => String::new(),
                },
                _ => String::new(),
            }
        })
    });
    let provider_config = match provider.as_str() {
        "openai" => ProviderConfig::openai(),
        _ => ProviderConfig::anthropic(),
    };
```

- [ ] **Step 3: Simplify AgentPool — remove config fields**

Remove `model`, `provider`, `max_tokens`, `api_key` fields from `AgentPool`. Update `AgentPool::new()` to not require them. The pool no longer caches config — workers read it from the broker at spawn time.

```rust
pub struct AgentPool {
    module: AgentModule,
    threads: HashMap<String, ThreadHandle>,
    workspace: PathBuf,
    no_policy: bool,
    inbox: ox_inbox::InboxStore,
    inbox_root: PathBuf,
    broker: ox_broker::BrokerStore,
    rt_handle: tokio::runtime::Handle,
}
```

Update `AgentPool::new()` signature to remove model/provider/max_tokens/api_key params.

- [ ] **Step 4: Remove model/provider from App**

In `crates/ox-cli/src/app.rs`, remove `pub model: String` and `pub provider: String` fields. Remove them from `App::new()` constructor.

Update `App::new()` signature — remove model/provider params (they're in ConfigStore now).

App now has **4 fields**: pool, input_history, history_cursor, input_draft.

- [ ] **Step 5: Update ViewState to read config from broker**

In `crates/ox-cli/src/view_state.rs`:

Replace the App-borrowed model/provider fields:
```rust
    // Replace:
    //     pub model: &'a str,
    //     pub provider: &'a str,
    // With owned fields from broker:
    pub model: String,
    pub provider: String,
```

In `fetch_view_state`, read from ConfigStore:
```rust
    // Read config
    let model = match client.read(&structfs_core_store::path!("config/model")).await {
        Ok(Some(r)) => match r.as_value() {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    };
    let provider = match client.read(&structfs_core_store::path!("config/provider")).await {
        Ok(Some(r)) => match r.as_value() {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    };
```

Update ViewState construction: replace `model: &app.model` with `model,` and `provider: &app.provider` with `provider,`.

Since model/provider are now owned Strings, ViewState's lifetime 'a may no longer be needed if all remaining borrowed fields can become owned. Check — input_history is still `&'a [String]` and pending_customize is `&'a Option<CustomizeState>`. So 'a stays.

- [ ] **Step 6: Update main.rs**

Update `App::new()` call to remove model/provider/api_key/max_tokens params (they're now in ConfigStore). Only pass what AgentPool needs: workspace, inbox_root, no_policy, broker, rt_handle.

- [ ] **Step 7: Verify compilation and tests**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: Compiles, all tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/ox-cli/src/agents.rs crates/ox-cli/src/app.rs crates/ox-cli/src/view_state.rs crates/ox-cli/src/event_loop.rs crates/ox-cli/src/main.rs
git commit -m "refactor(ox-cli): remove config from AgentPool/App, workers read from ConfigStore"
```

---

### Task 7: Update Snapshot Coordinator

**Files:**
- Modify: `crates/ox-inbox/src/snapshot.rs`

- [ ] **Step 1: Remove "model" from PARTICIPATING_MOUNTS**

Model config is now managed by ConfigStore. The snapshot coordinator should no longer save/restore it.

In `crates/ox-inbox/src/snapshot.rs`, change:

```rust
pub const PARTICIPATING_MOUNTS: [&str; 3] = ["system", "model", "gate"];
```

To:

```rust
pub const PARTICIPATING_MOUNTS: [&str; 2] = ["system", "gate"];
```

Note: "gate" stays for now — it has non-config state (model catalog, transport state). The API key/provider portions of gate will move to ConfigStore in a future pass.

- [ ] **Step 2: Verify compilation and tests**

Run: `cargo check -p ox-cli && cargo test -p ox-cli && cargo test -p ox-inbox`
Expected: All tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/ox-inbox/src/snapshot.rs
git commit -m "refactor(ox-inbox): remove model from snapshot participating mounts"
```

---

### Task 8: Final Quality Gate and Status Update

**Files:**
- Modify: `docs/design/rfc/structfs-tui-status.md`

- [ ] **Step 1: Run formatter**

Run: `./scripts/fmt.sh`

- [ ] **Step 2: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: 14/14 pass.

- [ ] **Step 3: Verify App has 4 fields**

Run: `grep -A 8 'pub struct App' crates/ox-cli/src/app.rs`
Expected: pool, input_history, history_cursor, input_draft.

- [ ] **Step 4: Update status document**

Add entry to `docs/design/rfc/structfs-tui-status.md`:

```markdown
#### Phase 3: StoreBacking + ConfigStore (complete)
- StoreBacking trait in ox-kernel for platform-agnostic persistence
- JsonFileBacking in ox-inbox (load/save with atomic write)
- ConfigStore in ox-ui with 4-layer cascade: ephemeral thread → saved thread → global → default
- ConfigStore mounted at `config/` in broker, initialized from CLI args
- ThreadRegistry routes model config reads/writes to ConfigStore via broker client
- Workers read config from ConfigStore at spawn (no baked-in config)
- App reduced to 4 fields: pool, input_history, history_cursor, input_draft
- "model" removed from snapshot PARTICIPATING_MOUNTS
- **Spec:** `docs/superpowers/specs/2026-04-08-storebacking-configstore-design.md`
- **Plan:** `docs/superpowers/plans/2026-04-08-storebacking-configstore.md`
```

- [ ] **Step 5: Commit**

```bash
git add docs/design/rfc/structfs-tui-status.md
git commit -m "docs: update status for Phase 3 StoreBacking + ConfigStore completion"
```

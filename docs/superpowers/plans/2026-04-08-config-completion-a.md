# Config System Completion — Part A: Store Utilities + Config Handles

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create the ox-store-util crate with capability wrappers (ReadOnly, Masked, LocalConfig), refactor ConfigStore to path-based read/write, and wire stores with config Reader handles so they read config transparently without knowing the source.

**Architecture:** New ox-store-util crate for generic StructFS utilities. ConfigStore refactored to path-based namespace (no set_ commands). Stores gain optional config Reader handle via with_config() builder. ThreadRegistry passes SyncClientAdapter handles at mount time. Phase 3 redirect reverted.

**Tech Stack:** Rust, structfs-core-store (Reader/Writer/Store/Value/Record/Path), ox-broker (ClientHandle, SyncClientAdapter)

**Scope note:** This is Part A — structural/architectural changes. Part B (figment, TOML persistence, per-thread config files) builds on this foundation in a separate plan.

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/ox-store-util/Cargo.toml` | Create | Crate manifest |
| `crates/ox-store-util/src/lib.rs` | Create | Module exports |
| `crates/ox-store-util/src/read_only.rs` | Create | ReadOnly wrapper |
| `crates/ox-store-util/src/masked.rs` | Create | Masked wrapper |
| `crates/ox-store-util/src/local_config.rs` | Create | LocalConfig in-memory store |
| `crates/ox-store-util/src/backing.rs` | Create | StoreBacking trait (moved from ox-kernel) |
| `Cargo.toml` | Modify | Add workspace member |
| `crates/ox-kernel/src/lib.rs` | Modify | Re-export StoreBacking from ox-store-util |
| `crates/ox-kernel/Cargo.toml` | Modify | Add ox-store-util dependency |
| `crates/ox-ui/src/config_store.rs` | Modify | Path-based namespace, direct writes |
| `crates/ox-context/src/lib.rs` | Modify | ModelProvider with_config() |
| `crates/ox-gate/src/lib.rs` | Modify | GateStore with_config() |
| `crates/ox-cli/src/thread_registry.rs` | Modify | Revert redirect, wire config handles |
| `crates/ox-cli/src/view_state.rs` | Modify | Use Masked config for display |

---

### Task 1: Create ox-store-util Crate with ReadOnly + Masked

**Files:**
- Create: `crates/ox-store-util/Cargo.toml`
- Create: `crates/ox-store-util/src/lib.rs`
- Create: `crates/ox-store-util/src/read_only.rs`
- Create: `crates/ox-store-util/src/masked.rs`
- Modify: `Cargo.toml` (workspace)

- [ ] **Step 1: Create Cargo.toml**

Create `crates/ox-store-util/Cargo.toml`:

```toml
[package]
name = "ox-store-util"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
homepage.workspace = true
authors.workspace = true

[dependencies]
structfs-core-store.workspace = true
```

- [ ] **Step 2: Add to workspace**

In root `Cargo.toml`, add `"crates/ox-store-util"` to the `members` array.

- [ ] **Step 3: Create read_only.rs with tests**

Create `crates/ox-store-util/src/read_only.rs`:

```rust
//! ReadOnly — capability restriction wrapper that rejects all writes.

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Writer};

/// Wraps any Reader, rejecting all writes.
///
/// Use this to give stores read-only access to a config source.
pub struct ReadOnly<S> {
    inner: S,
}

impl<S> ReadOnly<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }

    /// Consume the wrapper and return the inner store.
    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: Reader> Reader for ReadOnly<S> {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        self.inner.read(from)
    }
}

impl<S: Send + Sync> Writer for ReadOnly<S> {
    fn write(&mut self, _to: &Path, _data: Record) -> Result<Path, StoreError> {
        Err(StoreError::store(
            "ReadOnly",
            "write",
            "this handle is read-only",
        ))
    }
}

// Send + Sync if inner is
unsafe impl<S: Send> Send for ReadOnly<S> {}
unsafe impl<S: Sync> Sync for ReadOnly<S> {}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::{Value, path};

    struct TestStore {
        value: Value,
    }

    impl Reader for TestStore {
        fn read(&mut self, _from: &Path) -> Result<Option<Record>, StoreError> {
            Ok(Some(Record::parsed(self.value.clone())))
        }
    }

    impl Writer for TestStore {
        fn write(&mut self, to: &Path, _data: Record) -> Result<Path, StoreError> {
            Ok(to.clone())
        }
    }

    #[test]
    fn read_passes_through() {
        let inner = TestStore {
            value: Value::String("hello".into()),
        };
        let mut ro = ReadOnly::new(inner);
        let result = ro.read(&path!("anything")).unwrap().unwrap();
        assert_eq!(result.as_value().unwrap(), &Value::String("hello".into()));
    }

    #[test]
    fn write_rejected() {
        let inner = TestStore {
            value: Value::Null,
        };
        let mut ro = ReadOnly::new(inner);
        let result = ro.write(&path!("anything"), Record::parsed(Value::Null));
        assert!(result.is_err());
    }

    #[test]
    fn into_inner_recovers_store() {
        let inner = TestStore {
            value: Value::Integer(42),
        };
        let ro = ReadOnly::new(inner);
        let mut recovered = ro.into_inner();
        let result = recovered.read(&path!("x")).unwrap().unwrap();
        assert_eq!(result.as_value().unwrap(), &Value::Integer(42));
    }
}
```

- [ ] **Step 4: Create masked.rs with tests**

Create `crates/ox-store-util/src/masked.rs`:

```rust
//! Masked — path-based masking wrapper that redacts specified paths on read.

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value};

/// Wraps a Reader, returning a mask value for specified paths.
///
/// Use this to hide sensitive data (API keys) when exposing config
/// to display layers.
pub struct Masked<S> {
    inner: S,
    masked_paths: Vec<String>,
    mask_value: Value,
}

impl<S> Masked<S> {
    /// Create a Masked wrapper.
    ///
    /// `masked_paths` are path strings to match against. A read path
    /// matches if it starts with any masked path.
    pub fn new(inner: S, masked_paths: Vec<String>, mask_value: Value) -> Self {
        Self {
            inner,
            masked_paths,
            mask_value,
        }
    }

    fn is_masked(&self, path: &Path) -> bool {
        let path_str = path.to_string();
        self.masked_paths
            .iter()
            .any(|masked| path_str.starts_with(masked))
    }
}

impl<S: Reader> Reader for Masked<S> {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        if self.is_masked(from) {
            // Only return mask if the underlying value exists
            match self.inner.read(from)? {
                Some(_) => Ok(Some(Record::parsed(self.mask_value.clone()))),
                None => Ok(None),
            }
        } else {
            self.inner.read(from)
        }
    }
}

unsafe impl<S: Send> Send for Masked<S> {}
unsafe impl<S: Sync> Sync for Masked<S> {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use structfs_core_store::path;

    struct MapStore {
        data: BTreeMap<String, Value>,
    }

    impl Reader for MapStore {
        fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
            let key = from.to_string();
            Ok(self.data.get(&key).map(|v| Record::parsed(v.clone())))
        }
    }

    fn test_store() -> MapStore {
        let mut data = BTreeMap::new();
        data.insert("model/id".to_string(), Value::String("gpt-4o".into()));
        data.insert(
            "gate/api_key".to_string(),
            Value::String("sk-secret".into()),
        );
        data.insert(
            "gate/provider".to_string(),
            Value::String("anthropic".into()),
        );
        MapStore { data }
    }

    #[test]
    fn unmasked_path_passes_through() {
        let mut masked = Masked::new(
            test_store(),
            vec!["gate/api_key".into()],
            Value::String("***".into()),
        );
        let result = masked.read(&path!("model/id")).unwrap().unwrap();
        assert_eq!(
            result.as_value().unwrap(),
            &Value::String("gpt-4o".into())
        );
    }

    #[test]
    fn masked_path_returns_mask_value() {
        let mut masked = Masked::new(
            test_store(),
            vec!["gate/api_key".into()],
            Value::String("***".into()),
        );
        let result = masked.read(&path!("gate/api_key")).unwrap().unwrap();
        assert_eq!(result.as_value().unwrap(), &Value::String("***".into()));
    }

    #[test]
    fn masked_nonexistent_returns_none() {
        let mut masked = Masked::new(
            test_store(),
            vec!["gate/api_key".into()],
            Value::String("***".into()),
        );
        let result = masked
            .read(&Path::parse("nonexistent").unwrap())
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn multiple_masked_paths() {
        let mut masked = Masked::new(
            test_store(),
            vec!["gate/api_key".into(), "model/id".into()],
            Value::String("REDACTED".into()),
        );
        let key = masked.read(&path!("gate/api_key")).unwrap().unwrap();
        assert_eq!(
            key.as_value().unwrap(),
            &Value::String("REDACTED".into())
        );
        let model = masked.read(&path!("model/id")).unwrap().unwrap();
        assert_eq!(
            model.as_value().unwrap(),
            &Value::String("REDACTED".into())
        );
        // Unmasked still works
        let provider = masked.read(&path!("gate/provider")).unwrap().unwrap();
        assert_eq!(
            provider.as_value().unwrap(),
            &Value::String("anthropic".into())
        );
    }
}
```

- [ ] **Step 5: Create lib.rs**

Create `crates/ox-store-util/src/lib.rs`:

```rust
//! StructFS store utilities — composable wrappers and helpers.
//!
//! Platform-agnostic utilities for working with StructFS stores:
//! - `ReadOnly<S>` — rejects writes, passes reads through
//! - `Masked<S>` — redacts specified paths on read

pub mod masked;
pub mod read_only;

pub use masked::Masked;
pub use read_only::ReadOnly;
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p ox-store-util`
Expected: 7 tests pass (3 ReadOnly + 4 Masked).

- [ ] **Step 7: Commit**

```bash
git add crates/ox-store-util/ Cargo.toml
git commit -m "feat(ox-store-util): new crate with ReadOnly and Masked wrapper stores"
```

---

### Task 2: Add LocalConfig to ox-store-util

**Files:**
- Create: `crates/ox-store-util/src/local_config.rs`
- Modify: `crates/ox-store-util/src/lib.rs`

- [ ] **Step 1: Create local_config.rs with tests**

Create `crates/ox-store-util/src/local_config.rs`:

```rust
//! LocalConfig — in-memory path-based Reader/Writer for standalone config.
//!
//! Used by ox-web (no broker) and tests. Values are stored in a flat
//! BTreeMap keyed by path strings.

use std::collections::BTreeMap;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

/// In-memory config store implementing Reader and Writer.
pub struct LocalConfig {
    values: BTreeMap<String, Value>,
}

impl LocalConfig {
    pub fn new() -> Self {
        Self {
            values: BTreeMap::new(),
        }
    }

    /// Set a value at a path (convenience for construction).
    pub fn set(&mut self, path: &str, value: Value) {
        self.values.insert(path.to_string(), value);
    }
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl Reader for LocalConfig {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = from.to_string();
        // Exact match
        if let Some(val) = self.values.get(&key) {
            return Ok(Some(Record::parsed(val.clone())));
        }
        // If reading root, return all values as a map
        if from.is_empty() {
            let mut map = BTreeMap::new();
            for (k, v) in &self.values {
                map.insert(k.clone(), v.clone());
            }
            return Ok(Some(Record::parsed(Value::Map(map))));
        }
        Ok(None)
    }
}

impl Writer for LocalConfig {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let key = to.to_string();
        let value = data
            .as_value()
            .ok_or_else(|| StoreError::store("LocalConfig", "write", "expected parsed value"))?
            .clone();
        self.values.insert(key, value);
        Ok(to.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;

    #[test]
    fn read_empty_returns_none() {
        let mut config = LocalConfig::new();
        let result = config.read(&path!("model/id")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn set_then_read() {
        let mut config = LocalConfig::new();
        config.set("model/id", Value::String("gpt-4o".into()));
        let result = config.read(&path!("model/id")).unwrap().unwrap();
        assert_eq!(
            result.as_value().unwrap(),
            &Value::String("gpt-4o".into())
        );
    }

    #[test]
    fn write_then_read() {
        let mut config = LocalConfig::new();
        config
            .write(
                &path!("gate/provider"),
                Record::parsed(Value::String("openai".into())),
            )
            .unwrap();
        let result = config.read(&path!("gate/provider")).unwrap().unwrap();
        assert_eq!(
            result.as_value().unwrap(),
            &Value::String("openai".into())
        );
    }

    #[test]
    fn read_root_returns_all() {
        let mut config = LocalConfig::new();
        config.set("model/id", Value::String("gpt-4o".into()));
        config.set("gate/provider", Value::String("openai".into()));
        let result = config
            .read(&Path::from_components(vec![]))
            .unwrap()
            .unwrap();
        match result.as_value().unwrap() {
            Value::Map(m) => {
                assert_eq!(m.len(), 2);
                assert!(m.contains_key("model/id"));
                assert!(m.contains_key("gate/provider"));
            }
            _ => panic!("expected Map"),
        }
    }

    #[test]
    fn write_overwrites_existing() {
        let mut config = LocalConfig::new();
        config.set("model/id", Value::String("old".into()));
        config
            .write(
                &path!("model/id"),
                Record::parsed(Value::String("new".into())),
            )
            .unwrap();
        let result = config.read(&path!("model/id")).unwrap().unwrap();
        assert_eq!(result.as_value().unwrap(), &Value::String("new".into()));
    }
}
```

- [ ] **Step 2: Update lib.rs**

Add to `crates/ox-store-util/src/lib.rs`:

```rust
pub mod local_config;
pub use local_config::LocalConfig;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-store-util`
Expected: 12 tests pass (3 ReadOnly + 4 Masked + 5 LocalConfig).

- [ ] **Step 4: Commit**

```bash
git add crates/ox-store-util/src/local_config.rs crates/ox-store-util/src/lib.rs
git commit -m "feat(ox-store-util): add LocalConfig in-memory Reader/Writer"
```

---

### Task 3: Move StoreBacking to ox-store-util

**Files:**
- Create: `crates/ox-store-util/src/backing.rs`
- Modify: `crates/ox-store-util/src/lib.rs`
- Modify: `crates/ox-kernel/src/lib.rs`
- Modify: `crates/ox-kernel/src/backing.rs`
- Modify: `crates/ox-kernel/Cargo.toml`
- Modify: `crates/ox-inbox/Cargo.toml`

- [ ] **Step 1: Copy backing.rs to ox-store-util**

Create `crates/ox-store-util/src/backing.rs` with the same content as `crates/ox-kernel/src/backing.rs` (the StoreBacking trait).

- [ ] **Step 2: Export from ox-store-util**

Add to `crates/ox-store-util/src/lib.rs`:

```rust
pub mod backing;
pub use backing::StoreBacking;
```

- [ ] **Step 3: Re-export from ox-kernel for backward compatibility**

In `crates/ox-kernel/Cargo.toml`, add:
```toml
ox-store-util = { path = "../ox-store-util" }
```

Replace `crates/ox-kernel/src/backing.rs` content with a re-export:

```rust
//! StoreBacking — re-exported from ox-store-util for backward compatibility.
pub use ox_store_util::backing::StoreBacking;
```

Keep the `pub mod backing;` and `pub use backing::StoreBacking;` in `crates/ox-kernel/src/lib.rs` unchanged — existing consumers (ox-inbox) continue to import from ox-kernel.

- [ ] **Step 4: Verify all crates compile**

Run: `cargo check`
Expected: Full workspace compiles.

- [ ] **Step 5: Run all tests**

Run: `cargo test -p ox-store-util && cargo test -p ox-kernel && cargo test -p ox-inbox`
Expected: All pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-store-util/src/backing.rs crates/ox-store-util/src/lib.rs crates/ox-kernel/src/backing.rs crates/ox-kernel/src/lib.rs crates/ox-kernel/Cargo.toml
git commit -m "refactor: move StoreBacking trait to ox-store-util, re-export from ox-kernel"
```

---

### Task 4: Refactor ConfigStore to Path-Based Namespace

**Files:**
- Modify: `crates/ox-ui/src/config_store.rs`
- Modify: `crates/ox-cli/src/broker_setup.rs`
- Modify: `crates/ox-cli/src/view_state.rs`
- Modify: `crates/ox-cli/src/agents.rs`

This is the biggest refactor. ConfigStore changes from flat keys with `set_` commands to hierarchical paths with direct writes.

- [ ] **Step 1: Rewrite ConfigStore Reader for path-based reads**

The key change: internal storage uses path-string keys (`"model/id"`, `"gate/api_key"`) instead of flat keys (`"model"`, `"api_key"`). Reads match on path strings.

Replace the internal storage and resolution methods in `crates/ox-ui/src/config_store.rs`. The full new implementation:

```rust
//! ConfigStore — single authority for configuration resolution across all scopes.
//!
//! Three layers resolved in priority order (highest wins):
//! 1. Per-thread (ephemeral, session-only)
//! 2. Runtime global (runtime changes, persisted on explicit save)
//! 3. Base (figment-resolved startup values, immutable after init)
//!
//! Reads and writes use the same paths — no command paths.
//! Global: config/model/id, config/gate/provider
//! Per-thread: config/threads/{id}/model/id

use std::collections::BTreeMap;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

pub struct ConfigStore {
    /// Startup-resolved values (from figment or defaults). Immutable after init.
    base: BTreeMap<String, Value>,
    /// Runtime global changes (user-set during session).
    runtime: BTreeMap<String, Value>,
    /// Per-thread overrides. Key = thread_id, Value = path→value map.
    threads: BTreeMap<String, BTreeMap<String, Value>>,
}

impl ConfigStore {
    /// Create with base values (from figment resolution or defaults).
    pub fn new(base: BTreeMap<String, Value>) -> Self {
        Self {
            base,
            runtime: BTreeMap::new(),
            threads: BTreeMap::new(),
        }
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

    /// Build a map of all effective global values.
    fn effective_map(&self) -> Value {
        let mut map = BTreeMap::new();
        // Merge base, then runtime on top
        for (k, v) in &self.base {
            if k.contains("api_key") && !k.ends_with("_raw") {
                map.insert(k.clone(), Value::String("***".into()));
            } else {
                map.insert(k.clone(), v.clone());
            }
        }
        for (k, v) in &self.runtime {
            if k.contains("api_key") && !k.ends_with("_raw") {
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
                    if k.contains("api_key") && !k.ends_with("_raw") {
                        *v = Value::String("***".into());
                    }
                }
                return Ok(Some(Record::parsed(Value::Map(map))));
            }
            // Mask api_key on read (but allow api_key_raw)
            if sub.contains("api_key") && !sub.ends_with("_raw") {
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
        // Mask api_key on read
        if path_str.contains("api_key") && !path_str.ends_with("_raw") {
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
        self.runtime.insert(path_str, value);
        Ok(to.clone())
    }
}
```

- [ ] **Step 2: Rewrite ConfigStore tests**

Replace the entire `#[cfg(test)] mod tests` block:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::{path, Reader, Writer};

    fn store_with_defaults() -> ConfigStore {
        let mut base = BTreeMap::new();
        base.insert(
            "model/id".to_string(),
            Value::String("claude-sonnet-4-20250514".into()),
        );
        base.insert("gate/provider".to_string(), Value::String("anthropic".into()));
        base.insert("model/max_tokens".to_string(), Value::Integer(4096));
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
            read_val(&mut store, "model/id"),
            Some(Value::String("claude-sonnet-4-20250514".into()))
        );
        assert_eq!(
            read_val(&mut store, "gate/provider"),
            Some(Value::String("anthropic".into()))
        );
        assert_eq!(
            read_val(&mut store, "model/max_tokens"),
            Some(Value::Integer(4096))
        );
    }

    #[test]
    fn runtime_write_overrides_base() {
        let mut store = store_with_defaults();
        store
            .write(
                &path!("model/id"),
                Record::parsed(Value::String("gpt-4o".into())),
            )
            .unwrap();
        assert_eq!(
            read_val(&mut store, "model/id"),
            Some(Value::String("gpt-4o".into()))
        );
    }

    #[test]
    fn thread_falls_through_to_global() {
        let mut store = store_with_defaults();
        store
            .write(
                &path!("model/id"),
                Record::parsed(Value::String("gpt-4o".into())),
            )
            .unwrap();
        let p = Path::parse("threads/t_abc/model/id").unwrap();
        let val = store.read(&p).unwrap().unwrap().as_value().unwrap().clone();
        assert_eq!(val, Value::String("gpt-4o".into()));
    }

    #[test]
    fn thread_override_wins() {
        let mut store = store_with_defaults();
        let p = Path::parse("threads/t_abc/model/id").unwrap();
        store
            .write(&p, Record::parsed(Value::String("per-thread".into())))
            .unwrap();
        let val = store.read(&p).unwrap().unwrap().as_value().unwrap().clone();
        assert_eq!(val, Value::String("per-thread".into()));
        // Global unchanged
        assert_eq!(
            read_val(&mut store, "model/id"),
            Some(Value::String("claude-sonnet-4-20250514".into()))
        );
    }

    #[test]
    fn different_threads_independent() {
        let mut store = store_with_defaults();
        let p1 = Path::parse("threads/t_1/model/id").unwrap();
        let p2 = Path::parse("threads/t_2/model/id").unwrap();
        store
            .write(&p1, Record::parsed(Value::String("model-a".into())))
            .unwrap();
        store
            .write(&p2, Record::parsed(Value::String("model-b".into())))
            .unwrap();
        assert_eq!(
            store.read(&p1).unwrap().unwrap().as_value().unwrap().clone(),
            Value::String("model-a".into())
        );
        assert_eq!(
            store.read(&p2).unwrap().unwrap().as_value().unwrap().clone(),
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
                assert!(m.contains_key("model/id"));
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
}
```

- [ ] **Step 3: Update broker_setup.rs for path-based writes**

In `crates/ox-cli/src/broker_setup.rs`, update the ConfigStore initialization to use path-based writes:

```rust
    // ConfigStore with base values from CLI args
    let mut base = std::collections::BTreeMap::new();
    base.insert("model/id".to_string(), Value::String("claude-sonnet-4-20250514".into()));
    base.insert("gate/provider".to_string(), Value::String("anthropic".into()));
    base.insert("model/max_tokens".to_string(), Value::Integer(4096));

    let mut config = ConfigStore::new(base);
    // Runtime overrides from CLI args
    config.write(&path!("gate/provider"), Record::parsed(Value::String(provider))).ok();
    config.write(&path!("model/id"), Record::parsed(Value::String(model))).ok();
    config.write(&path!("model/max_tokens"), Record::parsed(Value::Integer(max_tokens as i64))).ok();
    config.write(&path!("gate/api_key"), Record::parsed(Value::String(api_key))).ok();
```

Update the broker_setup test for config reads:

```rust
    async fn config_store_mounted_with_defaults() {
        let handle = test_setup().await;
        let client = handle.client();

        let model = client.read(&path!("config/model/id")).await.unwrap().unwrap();
        assert_eq!(model.as_value().unwrap(), &Value::String("claude-sonnet-4-20250514".into()));

        let provider = client.read(&path!("config/gate/provider")).await.unwrap().unwrap();
        assert_eq!(provider.as_value().unwrap(), &Value::String("anthropic".into()));
    }
```

- [ ] **Step 4: Update view_state.rs to read path-based config**

In `fetch_view_state`, update config reads:

```rust
    // Replace:
    //   client.read(&path!("config/model"))
    // With:
    client.read(&structfs_core_store::path!("config/model/id"))

    // Replace:
    //   client.read(&path!("config/provider"))
    // With:
    client.read(&structfs_core_store::path!("config/gate/provider"))
```

- [ ] **Step 5: Update agents.rs to read path-based config**

In `spawn_worker`, update the config reads:

```rust
    // Replace:
    //   unscoped.read(&path!("config/provider"))
    // With:
    unscoped.read(&structfs_core_store::path!("config/gate/provider"))

    // Replace:
    //   unscoped.read(&path!("config/api_key_raw"))
    // With:
    unscoped.read(&structfs_core_store::path!("config/gate/api_key_raw"))
```

- [ ] **Step 6: Update ThreadRegistry config routing for path-based reads**

In `thread_registry.rs`, update `resolve_config_read` to use path-based config keys:

The `is_config_path` and `resolve_config_read` currently map `model/id` → flat key `"model"`. Update to pass the full path through:

```rust
    fn resolve_config_read(
        &self,
        thread_id: &str,
        sub: &Path,
    ) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let Some(client) = &self.broker_client else {
            return Box::pin(std::future::ready(Ok(None)));
        };

        // Pass the sub-path directly to config/threads/{id}/{sub}
        let sub_str = sub.to_string();
        let config_path = structfs_core_store::Path::parse(
            &format!("config/threads/{thread_id}/{sub_str}")
        ).unwrap();
        let client = client.clone();
        Box::pin(async move { client.read(&config_path).await })
    }
```

Similarly update the write routing.

- [ ] **Step 7: Verify compilation and tests**

Run: `cargo test -p ox-ui && cargo test -p ox-cli`
Expected: All tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/ox-ui/src/config_store.rs crates/ox-cli/src/broker_setup.rs crates/ox-cli/src/view_state.rs crates/ox-cli/src/agents.rs crates/ox-cli/src/thread_registry.rs
git commit -m "refactor(ox-ui): ConfigStore path-based namespace, direct reads and writes"
```

---

### Task 5: Add Config Handle to ModelProvider

**Files:**
- Modify: `crates/ox-context/src/lib.rs`
- Modify: `crates/ox-context/Cargo.toml`

- [ ] **Step 1: Add ox-store-util dependency to ox-context**

In `crates/ox-context/Cargo.toml`, add:
```toml
ox-store-util = { path = "../ox-store-util" }
```

- [ ] **Step 2: Add config handle to ModelProvider**

In `crates/ox-context/src/lib.rs`, add a config field to ModelProvider:

```rust
pub struct ModelProvider {
    model: String,
    max_tokens: u32,
    config: Option<Box<dyn structfs_core_store::Store + Send + Sync>>,
}
```

Add builder method:

```rust
impl ModelProvider {
    pub fn new(model: String, max_tokens: u32) -> Self {
        Self { model, max_tokens, config: None }
    }

    pub fn with_config(mut self, config: Box<dyn structfs_core_store::Store + Send + Sync>) -> Self {
        self.config = Some(config);
        self
    }
```

- [ ] **Step 3: Update Reader to use config handle**

In the Reader impl, check config handle first:

```rust
impl Reader for ModelProvider {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = if from.is_empty() {
            ""
        } else {
            from.components[0].as_str()
        };
        match key {
            "" | "id" => {
                // Config handle takes priority
                if let Some(ref mut config) = self.config {
                    if let Ok(Some(record)) = config.read(&structfs_core_store::path!("model/id")) {
                        return Ok(Some(record));
                    }
                }
                Ok(Some(Record::parsed(Value::String(self.model.clone()))))
            }
            "max_tokens" => {
                if let Some(ref mut config) = self.config {
                    if let Ok(Some(record)) = config.read(&structfs_core_store::path!("model/max_tokens")) {
                        return Ok(Some(record));
                    }
                }
                Ok(Some(Record::parsed(Value::Integer(self.max_tokens as i64))))
            }
            "snapshot" => {
                // Snapshot reads local state (not config) for persistence
                // ... existing snapshot code unchanged ...
            }
            _ => Ok(None),
        }
    }
}
```

The snapshot path continues to read local fields — snapshots save the store's own state, not the config source.

- [ ] **Step 4: Add test for config handle**

Add to the existing tests in ox-context:

```rust
    #[test]
    fn model_provider_reads_from_config_handle() {
        use ox_store_util::LocalConfig;

        let mut config = LocalConfig::new();
        config.set("model/id", Value::String("config-model".into()));
        config.set("model/max_tokens", Value::Integer(8192));

        let mut provider = ModelProvider::new("default-model".into(), 4096)
            .with_config(Box::new(config));

        let model = provider.read(&path!("id")).unwrap().unwrap();
        assert_eq!(model.as_value().unwrap(), &Value::String("config-model".into()));

        let tokens = provider.read(&path!("max_tokens")).unwrap().unwrap();
        assert_eq!(tokens.as_value().unwrap(), &Value::Integer(8192));
    }

    #[test]
    fn model_provider_falls_back_to_local() {
        let mut provider = ModelProvider::new("local-model".into(), 2048);
        // No config handle — should use local fields
        let model = provider.read(&path!("id")).unwrap().unwrap();
        assert_eq!(model.as_value().unwrap(), &Value::String("local-model".into()));
    }
```

- [ ] **Step 5: Verify compilation and tests**

Run: `cargo test -p ox-context`
Expected: All existing + 2 new tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-context/src/lib.rs crates/ox-context/Cargo.toml
git commit -m "feat(ox-context): ModelProvider with_config() reads from config handle"
```

---

### Task 6: Wire Config Handles in ThreadRegistry

**Files:**
- Modify: `crates/ox-cli/src/thread_registry.rs`

- [ ] **Step 1: Revert the config redirect, wire config handles instead**

Remove from ThreadRegistry:
- `is_config_path()` method
- `resolve_config_read()` method
- Config path check in AsyncReader and AsyncWriter

Revert AsyncReader/AsyncWriter to the pre-Phase-3 versions that route everything through local sync stores (except approval).

- [ ] **Step 2: Wire config handles at mount time**

In `ensure_mounted`, when creating a ThreadNamespace, construct a config handle and pass it to ModelProvider:

```rust
    fn ensure_mounted(&mut self, thread_id: &str) -> &mut ThreadNamespace {
        if !self.threads.contains_key(thread_id) {
            let thread_dir = self.inbox_root.join("threads").join(thread_id);
            let mut ns = if thread_dir.exists() {
                ThreadNamespace::from_thread_dir(&thread_dir)
            } else {
                ThreadNamespace::new_default()
            };

            // Wire config handle if broker client is available
            if let Some(client) = &self.broker_client {
                let config_client = client.scoped(&format!("config/threads/{thread_id}"));
                let config_handle = ox_broker::SyncClientAdapter::new(
                    config_client,
                    tokio::runtime::Handle::current(),
                );
                let read_only = ox_store_util::ReadOnly::new(config_handle);
                // Replace ModelProvider with one that has a config handle
                let model_with_config = ox_context::ModelProvider::new(
                    "claude-sonnet-4-20250514".into(),
                    4096,
                ).with_config(Box::new(read_only));
                ns.model = model_with_config;
            }

            self.threads.insert(thread_id.to_string(), ns);
        }
        self.threads.get_mut(thread_id).expect("just inserted")
    }
```

Note: This creates a new ModelProvider and replaces the one from `new_default()` or `from_thread_dir()`. The local default values don't matter because the config handle takes priority on reads. For restored threads, the snapshot data goes to the local fields but config handle reads override them.

- [ ] **Step 3: Add ox-store-util and ox-context dependencies to ox-cli**

In `crates/ox-cli/Cargo.toml`, add:
```toml
ox-store-util = { path = "../ox-store-util" }
```

(ox-context should already be a dependency via ox-runtime or similar.)

- [ ] **Step 4: Update integration test**

The existing `thread_model_resolves_through_config` test should still work — the model value now flows through the config handle on ModelProvider instead of the ThreadRegistry redirect, but the end result is the same: reading `threads/{id}/model/id` returns the ConfigStore value.

Run the test to verify: `cargo test -p ox-cli -- thread_model_resolves_through_config`

- [ ] **Step 5: Verify compilation and all tests**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: All 67+ tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/thread_registry.rs crates/ox-cli/Cargo.toml
git commit -m "refactor(ox-cli): revert ThreadRegistry redirect, wire ReadOnly config handles at mount"
```

---

### Task 7: Final Quality Gate

**Files:**
- Modify: `docs/design/rfc/structfs-tui-status.md`

- [ ] **Step 1: Run formatter**

Run: `./scripts/fmt.sh`

- [ ] **Step 2: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: All code gates pass (fmt, clippy, check, test).

- [ ] **Step 3: Verify file counts**

Run: `wc -l crates/ox-store-util/src/*.rs | sort -n`

Expected:
- read_only.rs: ~80 lines
- masked.rs: ~110 lines
- local_config.rs: ~100 lines
- backing.rs: ~20 lines
- lib.rs: ~15 lines

- [ ] **Step 4: Update status document**

Add entry to `docs/design/rfc/structfs-tui-status.md`:

```markdown
#### Phase 4a: Config System Completion — Store Utilities + Config Handles
- New `ox-store-util` crate: ReadOnly, Masked, LocalConfig, StoreBacking (moved from ox-kernel)
- ConfigStore refactored to path-based namespace (model/id, gate/api_key — no set_ commands)
- Stores read config through Reader handle via with_config() builder
- ModelProvider reads from config handle, falls back to local fields for standalone
- ThreadRegistry wires ReadOnly<SyncClientAdapter> config handles at mount time
- Phase 3 ThreadRegistry redirect reverted — stores own their reads
- **Spec:** `docs/superpowers/specs/2026-04-08-config-completion-design.md`
- **Plan:** `docs/superpowers/plans/2026-04-08-config-completion-a.md`
```

- [ ] **Step 5: Commit**

```bash
git add docs/design/rfc/structfs-tui-status.md
git commit -m "docs: update status for Phase 4a Config System Completion"
```

# ConfigStore S-Tier Refactor

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace ConfigStore's tangled cascade/masking/persistence with composable StructFS primitives: `Cascade<A, B>` for layered reads, `Masked<S>` for display consumers, and a simplified ConfigStore that's just two `LocalConfig` layers with a backing.

**Architecture:** ConfigStore shrinks from ~240 lines of special-case logic to ~80 lines. Thread scoping moves out — each thread gets a `Cascade<LocalConfig, ReadOnly<SyncClientAdapter>>` as its config handle, where the `LocalConfig` holds per-thread overrides and the adapter falls through to ConfigStore for globals. Masking moves out — ViewState gets a `Masked` handle, GateStore gets a plain `ReadOnly` handle. The `_raw` suffix hack and hardcoded `api_key` substring matching are deleted.

**Tech Stack:** Existing structfs-core-store, ox-store-util (LocalConfig, ReadOnly, Masked, StoreBacking), ox-broker (SyncClientAdapter)

---

## File Map

| Action | File | Responsibility |
|--------|------|----------------|
| Create | `crates/ox-store-util/src/cascade.rs` | Cascade<A, B> — try A, fall back to B |
| Modify | `crates/ox-store-util/src/lib.rs` | Export Cascade |
| Modify | `crates/ox-ui/src/config_store.rs` | Rewrite: two LocalConfig layers + backing, no masking/threads |
| Modify | `crates/ox-cli/src/thread_registry.rs` | Wire Cascade<LocalConfig, ReadOnly> per thread |
| Modify | `crates/ox-cli/src/view_state.rs` | No changes needed (reads go through broker, not masked here) |
| Modify | `crates/ox-gate/src/lib.rs` | Remove `s != "***"` guard from config_string |
| Modify | `crates/ox-cli/src/broker_setup.rs` | Update tests for simplified ConfigStore |

---

### Task 1: Cascade<A, B> wrapper in ox-store-util

A generic wrapper that tries the primary store first, falling back to a secondary store on `None`. Implements Reader (cascade reads) and Writer (writes go to primary).

**Files:**
- Create: `crates/ox-store-util/src/cascade.rs`
- Modify: `crates/ox-store-util/src/lib.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/ox-store-util/src/cascade.rs`:

```rust
//! Cascade<A, B> — layered read with fallback.
//!
//! Reads try A first; if A returns None, falls back to B.
//! Writes go to A (the overlay). B is read-only from Cascade's perspective.

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Writer};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalConfig;
    use structfs_core_store::{Value, path};

    #[test]
    fn primary_value_wins() {
        let mut primary = LocalConfig::new();
        primary.set("gate/model", Value::String("primary-model".into()));
        let mut fallback = LocalConfig::new();
        fallback.set("gate/model", Value::String("fallback-model".into()));

        let mut cascade = Cascade::new(primary, fallback);
        let record = cascade.read(&path!("gate/model")).unwrap().unwrap();
        assert_eq!(
            record.as_value().unwrap(),
            &Value::String("primary-model".into())
        );
    }

    #[test]
    fn falls_back_when_primary_returns_none() {
        let primary = LocalConfig::new(); // empty
        let mut fallback = LocalConfig::new();
        fallback.set("gate/model", Value::String("fallback-model".into()));

        let mut cascade = Cascade::new(primary, fallback);
        let record = cascade.read(&path!("gate/model")).unwrap().unwrap();
        assert_eq!(
            record.as_value().unwrap(),
            &Value::String("fallback-model".into())
        );
    }

    #[test]
    fn both_none_returns_none() {
        let primary = LocalConfig::new();
        let fallback = LocalConfig::new();

        let mut cascade = Cascade::new(primary, fallback);
        let result = cascade.read(&path!("gate/model")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn writes_go_to_primary() {
        let primary = LocalConfig::new();
        let mut fallback = LocalConfig::new();
        fallback.set("gate/model", Value::String("fallback".into()));

        let mut cascade = Cascade::new(primary, fallback);
        cascade
            .write(
                &path!("gate/model"),
                Record::parsed(Value::String("written".into())),
            )
            .unwrap();

        // Read should return the written value (from primary)
        let record = cascade.read(&path!("gate/model")).unwrap().unwrap();
        assert_eq!(
            record.as_value().unwrap(),
            &Value::String("written".into())
        );
    }

    #[test]
    fn write_to_primary_does_not_affect_fallback() {
        let primary = LocalConfig::new();
        let mut fallback = LocalConfig::new();
        fallback.set("gate/model", Value::String("original".into()));

        let mut cascade = Cascade::new(primary, fallback);
        cascade
            .write(
                &path!("gate/model"),
                Record::parsed(Value::String("override".into())),
            )
            .unwrap();

        // Fallback still has original value
        let record = cascade.fallback.read(&path!("gate/model")).unwrap().unwrap();
        assert_eq!(
            record.as_value().unwrap(),
            &Value::String("original".into())
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ox-store-util -- cascade 2>&1 | head -10`
Expected: compilation error — `Cascade` not defined.

- [ ] **Step 3: Implement Cascade**

Add above the tests in the same file:

```rust
/// Layered store: reads try `primary` first, fall back to `fallback`.
/// Writes always go to `primary`.
pub struct Cascade<A, B> {
    pub primary: A,
    pub fallback: B,
}

impl<A, B> Cascade<A, B> {
    pub fn new(primary: A, fallback: B) -> Self {
        Self { primary, fallback }
    }
}

impl<A: Reader, B: Reader> Reader for Cascade<A, B> {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        match self.primary.read(from)? {
            Some(record) => Ok(Some(record)),
            None => self.fallback.read(from),
        }
    }
}

impl<A: Writer, B> Writer for Cascade<A, B> {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        self.primary.write(to, data)
    }
}

// SAFETY: Cascade is Send/Sync if both inner stores are.
unsafe impl<A: Send, B: Send> Send for Cascade<A, B> {}
unsafe impl<A: Sync, B: Sync> Sync for Cascade<A, B> {}
```

- [ ] **Step 4: Export from lib.rs**

In `crates/ox-store-util/src/lib.rs`, add:

```rust
pub mod cascade;
pub use cascade::Cascade;
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ox-store-util -- cascade`
Expected: All 5 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-store-util/src/cascade.rs crates/ox-store-util/src/lib.rs
git commit -m "feat(ox-store-util): Cascade<A, B> layered read with fallback"
```

---

### Task 2: Rewrite ConfigStore as two LocalConfig layers

ConfigStore is rewritten to use `LocalConfig` for both layers. All masking logic removed. All thread-scoping logic removed. The `_raw` suffix hack removed. What remains: cascade read (runtime → base), writes to runtime, `save` command, persistence.

**Files:**
- Modify: `crates/ox-ui/src/config_store.rs`

- [ ] **Step 1: Rewrite ConfigStore**

Replace the entire file with:

```rust
//! ConfigStore — layered configuration with optional persistence.
//!
//! Two layers resolved in priority order (highest wins):
//! 1. Runtime (user changes during session, persistable)
//! 2. Base (figment-resolved startup values, immutable after init)
//!
//! No masking — consumers that need masking use a `Masked` wrapper.
//! No thread scoping — threads use `Cascade<LocalConfig, ReadOnly<handle>>`.
//! Reads and writes use the same paths: gate/model, gate/provider, etc.

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
}

impl Reader for ConfigStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = from.to_string();

        // Root read: return all effective values as a map
        if key.is_empty() {
            let mut map = BTreeMap::new();
            for (k, v) in &self.base {
                map.insert(k.clone(), v.clone());
            }
            for (k, v) in &self.runtime {
                map.insert(k.clone(), v.clone());
            }
            return Ok(Some(Record::parsed(Value::Map(map))));
        }

        // Cascade: runtime → base
        if let Some(v) = self.runtime.get(&key) {
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
    use structfs_core_store::{path, Reader, Writer};

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
    fn api_key_not_masked_anymore() {
        // ConfigStore no longer masks — masking is the consumer's job
        let mut store = store_with_defaults();
        store
            .write(
                &path!("gate/api_key"),
                Record::parsed(Value::String("sk-secret".into())),
            )
            .unwrap();
        assert_eq!(
            read_val(&mut store, "gate/api_key"),
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
                assert!(!m.contains_key("gate/api_key"));
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
```

Note: Tests that relied on the old thread-scoping and masking behavior are removed. Thread scoping is tested in Task 3 (ThreadRegistry). Masking is tested through the existing `Masked` wrapper tests in ox-store-util.

- [ ] **Step 2: Run tests**

Run: `cargo test -p ox-ui`
Expected: All tests pass. Some downstream compilation errors may appear — fix in later steps.

- [ ] **Step 3: Fix downstream compilation**

Run: `cargo check 2>&1 | head -40`

The broker_setup tests that read `config/threads/{id}/gate/model` will break because ConfigStore no longer handles thread paths. The `thread_model_reads_from_gate_store` test and the `thread_gate_reads_*` tests should still pass because they go through ThreadRegistry → GateStore → config handle → ConfigStore (global, no thread prefix).

However, the config handle scoping needs to change. Currently ThreadRegistry scopes to `config/threads/{thread_id}`, which relied on ConfigStore's internal thread routing. Now the config handle should scope to just `config/` since ConfigStore is global-only.

Fix in `crates/ox-cli/src/thread_registry.rs`, `ensure_mounted()`:

Change:
```rust
let config_client = client.scoped(&format!("config/threads/{thread_id}"));
```
To:
```rust
let config_client = client.scoped("config");
```

This means GateStore reads `gate/model` through the config handle, which becomes `config/gate/model` via the broker, which ConfigStore resolves as a global read. Correct.

- [ ] **Step 4: Remove `_raw` suffix usage from GateStore**

In `crates/ox-gate/src/lib.rs`, update `config_string` to remove the `s != "***"` guard — ConfigStore no longer masks:

```rust
fn config_string(&mut self, path_str: &str) -> Option<String> {
    let config = self.config.as_mut()?;
    let path = Path::parse(path_str).ok()?;
    let record = config.read(&path).ok()??;
    match record.as_value() {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}
```

Update the bootstrap key resolution in the Reader impl — change `gate/api_key_raw` to `gate/api_key`:

Find the block in the `"accounts"` arm that checks config for the bootstrap key:
```rust
if let Some(k) = self
    .config_string("gate/api_key_raw")
    .or_else(|| self.config_string("gate/api_key"))
```

Replace with:
```rust
if let Some(k) = self.config_string("gate/api_key")
```

Do the same in `completion_tool_schemas` and `create_completion_tools` — replace the `api_key_raw` fallback chain with just `api_key`.

- [ ] **Step 5: Update broker_setup tests**

In `crates/ox-cli/src/broker_setup.rs`, remove or update any tests that read `config/threads/{id}/...` paths directly (these no longer route through ConfigStore). The `thread_gate_reads_api_key_from_config` and `thread_gate_reads_model_from_config` tests should still work because they read through `threads/{id}/gate/...` which goes through ThreadRegistry → GateStore → config handle → ConfigStore.

- [ ] **Step 6: Run all tests**

Run: `cargo test`
Expected: All pass.

- [ ] **Step 7: Commit**

```bash
git add crates/ox-ui/src/config_store.rs crates/ox-gate/src/lib.rs crates/ox-cli/src/thread_registry.rs crates/ox-cli/src/broker_setup.rs
git commit -m "refactor: simplify ConfigStore — remove masking, thread scoping, _raw suffix"
```

---

### Task 3: Wire Cascade config handles in ThreadRegistry

Thread config handles change from `ReadOnly<SyncClientAdapter>` (scoped to `config/threads/{id}`) to `Cascade<LocalConfig, ReadOnly<SyncClientAdapter>>` (scoped to `config/`). The `LocalConfig` primary holds per-thread overrides (empty initially). The `ReadOnly` fallback reads from ConfigStore globals.

**Files:**
- Modify: `crates/ox-cli/src/thread_registry.rs`

- [ ] **Step 1: Update ensure_mounted()**

Change the config handle wiring in `ensure_mounted()`:

```rust
// Wire config handle into GateStore if broker client is available
if let Some(client) = &self.broker_client {
    let config_client = client.scoped("config");
    let config_adapter = ox_broker::SyncClientAdapter::new(
        config_client,
        tokio::runtime::Handle::current(),
    );
    let read_only = ox_store_util::ReadOnly::new(config_adapter);
    let thread_overrides = ox_store_util::LocalConfig::new();
    let cascade = ox_store_util::Cascade::new(thread_overrides, read_only);
    ns.gate = GateStore::new().with_config(Box::new(cascade));
}
```

- [ ] **Step 2: Add import**

Ensure `ox_store_util` imports are available. The crate is already a dependency of ox-cli.

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-cli`
Expected: All pass. The `thread_gate_reads_api_key_from_config` and `thread_gate_reads_model_from_config` integration tests prove end-to-end: ConfigStore → broker → SyncClientAdapter → ReadOnly → Cascade → GateStore.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-cli/src/thread_registry.rs
git commit -m "refactor(ox-cli): Cascade<LocalConfig, ReadOnly> config handles per thread"
```

---

### Task 4: Format, quality gates, status doc

**Files:**
- Modify: `docs/design/rfc/structfs-tui-status.md`

- [ ] **Step 1: Run formatter**

Run: `./scripts/fmt.sh`

- [ ] **Step 2: Commit formatter changes if any**

```bash
git add -A && git commit -m "style: apply formatter to ConfigStore S-tier refactor"
```

- [ ] **Step 3: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: 14/14 pass.

- [ ] **Step 4: Update status doc**

Add after the Phase 4b entry:

```markdown
#### Phase 4c: ConfigStore S-Tier Refactor (complete)
- Cascade<A, B> wrapper in ox-store-util — layered reads with fallback
- ConfigStore simplified: two flat layers (base + runtime), no masking, no thread scoping
- Thread config handles: Cascade<LocalConfig, ReadOnly<SyncClientAdapter>>
- Masking removed from ConfigStore — consumers use Masked wrapper
- _raw suffix hack removed — GateStore reads gate/api_key directly
- ConfigStore: ~80 lines (was ~240)
- **Plan:** `docs/superpowers/plans/2026-04-08-config-store-stier.md`
```

- [ ] **Step 5: Commit**

```bash
git add docs/design/rfc/structfs-tui-status.md
git commit -m "docs: update status for Phase 4c ConfigStore S-tier refactor"
```

# Snapshot Lens (Plan A) — Per-Store Snapshot Path Handling

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `snapshot` path handling to SystemProvider, ModelProvider, HistoryProvider, and GateStore so that reading `snapshot` returns `{hash, state}` and writing `snapshot` restores from `{state}`.

**Architecture:** Each store opts into the snapshot lens by handling the `"snapshot"` path prefix in its existing `Reader::read` / `Writer::write` match arms. A shared `snapshot_hash` helper computes the truncated SHA-256. ToolsProvider explicitly returns `None` for `snapshot`.

**Tech Stack:** Rust, sha2 crate, structfs-core-store (Value, Record, Path), structfs-serde-store (json_to_value, value_to_json, to_value, from_value)

---

## File Structure

| File | Responsibility |
|------|---------------|
| `Cargo.toml` (workspace root) | Add `sha2` workspace dependency |
| `crates/ox-kernel/Cargo.toml` | Add `sha2` dep |
| `crates/ox-kernel/src/snapshot.rs` | `snapshot_hash(state: &Value) -> String` helper + `snapshot_record(state: Value) -> Value` builder |
| `crates/ox-kernel/src/lib.rs` | `pub mod snapshot;` + re-export |
| `crates/ox-context/Cargo.toml` | Add `sha2` dep (via ox-kernel re-export) |
| `crates/ox-context/src/lib.rs` | Add snapshot arms to SystemProvider, ModelProvider, ToolsProvider readers/writers |
| `crates/ox-history/src/lib.rs` | Add snapshot read arm + write-error arm to HistoryProvider |
| `crates/ox-gate/src/lib.rs` | Add snapshot arms to GateStore reader/writer |

---

### Task 1: Snapshot Hash Helper in ox-kernel

**Files:**
- Modify: `Cargo.toml:27-29` (workspace deps)
- Modify: `crates/ox-kernel/Cargo.toml:15-19`
- Create: `crates/ox-kernel/src/snapshot.rs`
- Modify: `crates/ox-kernel/src/lib.rs:1-28`

- [ ] **Step 1: Write the failing test**

Create `crates/ox-kernel/src/snapshot.rs` with tests only:

```rust
//! Snapshot lens helpers — hash computation and record building.

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::Value;
    use std::collections::BTreeMap;

    #[test]
    fn hash_of_string_value() {
        let state = Value::String("hello".to_string());
        let hash = snapshot_hash(&state);
        // SHA-256 of `"hello"` (JSON-serialized) truncated to 16 hex chars
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hash_is_deterministic() {
        let state = Value::String("test prompt".to_string());
        let h1 = snapshot_hash(&state);
        let h2 = snapshot_hash(&state);
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_of_map_is_sorted_key_order() {
        // BTreeMap guarantees sorted keys, so JSON output is deterministic
        let mut map = BTreeMap::new();
        map.insert("z".to_string(), Value::Integer(1));
        map.insert("a".to_string(), Value::Integer(2));
        let state = Value::Map(map);

        let h1 = snapshot_hash(&state);
        let h2 = snapshot_hash(&state);
        assert_eq!(h1, h2);
    }

    #[test]
    fn snapshot_record_contains_hash_and_state() {
        let state = Value::String("prompt".to_string());
        let record = snapshot_record(state.clone());

        match &record {
            Value::Map(m) => {
                assert!(m.contains_key("hash"));
                assert!(m.contains_key("state"));
                assert_eq!(m.get("state").unwrap(), &state);
                match m.get("hash").unwrap() {
                    Value::String(h) => assert_eq!(h.len(), 16),
                    _ => panic!("hash should be a string"),
                }
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn extract_state_from_full_snapshot() {
        let state = Value::String("data".to_string());
        let snap = snapshot_record(state.clone());
        let extracted = extract_snapshot_state(snap).unwrap();
        assert_eq!(extracted, state);
    }

    #[test]
    fn extract_state_from_state_only() {
        // Writing just the state value (not wrapped in {state: ...}) should also work
        let state = Value::String("data".to_string());
        let extracted = extract_snapshot_state(state.clone()).unwrap();
        assert_eq!(extracted, state);
    }
}
```

- [ ] **Step 2: Add sha2 workspace dep and wire up the module**

Add `sha2` to workspace root `Cargo.toml` under `[workspace.dependencies]`:

```toml
sha2 = "0.10"
```

Add `sha2` to `crates/ox-kernel/Cargo.toml` under `[dependencies]`:

```toml
sha2 = { workspace = true }
```

Add to `crates/ox-kernel/src/lib.rs` after the existing `pub use` block:

```rust
pub mod snapshot;
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p ox-kernel snapshot -- --nocapture`
Expected: compilation errors — `snapshot_hash`, `snapshot_record`, `extract_snapshot_state` not defined

- [ ] **Step 4: Write minimal implementation**

Add to the top of `crates/ox-kernel/src/snapshot.rs`, above the `#[cfg(test)]`:

```rust
//! Snapshot lens helpers — hash computation and record building.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use structfs_core_store::Value;
use structfs_serde_store::value_to_json;

/// Compute the snapshot hash: SHA-256 of the JSON-serialized state, truncated to 16 hex chars.
///
/// StructFS `Value::Map` uses `BTreeMap` (sorted keys), so output is deterministic.
pub fn snapshot_hash(state: &Value) -> String {
    let json = value_to_json(state.clone());
    let json_bytes = serde_json::to_vec(&json).expect("Value always serializes to JSON");
    let digest = Sha256::digest(&json_bytes);
    hex::encode(&digest[..8])
}

/// Build a snapshot Value: `{"hash": "<16 hex>", "state": <value>}`.
pub fn snapshot_record(state: Value) -> Value {
    let hash = snapshot_hash(&state);
    let mut map = BTreeMap::new();
    map.insert("hash".to_string(), Value::String(hash));
    map.insert("state".to_string(), state);
    Value::Map(map)
}

/// Extract the restorable state from a written snapshot value.
///
/// Accepts either `{"state": <value>, ...}` (full snapshot map) or a bare value
/// (treated as the state directly). The `hash` field, if present, is ignored —
/// the store recomputes it.
pub fn extract_snapshot_state(value: Value) -> Result<Value, String> {
    match value {
        Value::Map(ref m) if m.contains_key("state") => {
            Ok(m.get("state").unwrap().clone())
        }
        other => Ok(other),
    }
}
```

Wait — we need the `hex` crate too, or we can do the hex encoding manually. Let's avoid the extra dep and format it ourselves:

Replace the hash function body:

```rust
pub fn snapshot_hash(state: &Value) -> String {
    let json = value_to_json(state.clone());
    let json_bytes = serde_json::to_vec(&json).expect("Value always serializes to JSON");
    let digest = Sha256::digest(&json_bytes);
    // Truncate to first 8 bytes (16 hex characters)
    digest[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
```

Also add `serde_json` to the imports:

```rust
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use structfs_core_store::Value;
use structfs_serde_store::value_to_json;
```

(serde_json is already a dep of ox-kernel)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ox-kernel snapshot -- --nocapture`
Expected: all 6 tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/ox-kernel/src/snapshot.rs crates/ox-kernel/src/lib.rs crates/ox-kernel/Cargo.toml Cargo.toml
git commit -m "feat(ox-kernel): add snapshot lens hash + record helpers"
```

---

### Task 2: SystemProvider Snapshot

**Files:**
- Modify: `crates/ox-context/src/lib.rs:240-260` (Reader + Writer impls)

- [ ] **Step 1: Write the failing test**

Add a `#[cfg(test)] mod tests` block at the bottom of `crates/ox-context/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ox_kernel::snapshot::{snapshot_hash, snapshot_record};
    use structfs_core_store::path;
    use structfs_serde_store::value_to_json;

    // -- SystemProvider snapshot tests --

    #[test]
    fn system_snapshot_read_returns_hash_and_state() {
        let mut sp = SystemProvider::new("You are helpful.".to_string());
        let record = sp.read(&path!("snapshot")).unwrap().unwrap();
        let val = record.into_value().unwrap();

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
        let record = sp.read(&path!("snapshot/hash")).unwrap().unwrap();
        match record.into_value().unwrap() {
            Value::String(h) => assert_eq!(h.len(), 16),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn system_snapshot_read_state_only() {
        let mut sp = SystemProvider::new("Hello".to_string());
        let record = sp.read(&path!("snapshot/state")).unwrap().unwrap();
        assert_eq!(record.into_value().unwrap(), Value::String("Hello".to_string()));
    }

    #[test]
    fn system_snapshot_write_restores_state() {
        let mut sp = SystemProvider::new("old prompt".to_string());

        // Write a snapshot with new state
        let mut map = std::collections::BTreeMap::new();
        map.insert("state".to_string(), Value::String("new prompt".to_string()));
        sp.write(&path!("snapshot"), Record::parsed(Value::Map(map))).unwrap();

        // Verify restored
        let record = sp.read(&path!("")).unwrap().unwrap();
        assert_eq!(record.into_value().unwrap(), Value::String("new prompt".to_string()));
    }

    #[test]
    fn system_snapshot_write_state_path() {
        let mut sp = SystemProvider::new("old".to_string());
        sp.write(
            &path!("snapshot/state"),
            Record::parsed(Value::String("new".to_string())),
        ).unwrap();
        let record = sp.read(&path!("")).unwrap().unwrap();
        assert_eq!(record.into_value().unwrap(), Value::String("new".to_string()));
    }

    #[test]
    fn system_snapshot_hash_changes_after_write() {
        let mut sp = SystemProvider::new("first".to_string());
        let h1 = match sp.read(&path!("snapshot/hash")).unwrap().unwrap().into_value().unwrap() {
            Value::String(s) => s,
            _ => panic!("expected string"),
        };

        sp.write(&path!("snapshot/state"), Record::parsed(Value::String("second".to_string()))).unwrap();

        let h2 = match sp.read(&path!("snapshot/hash")).unwrap().unwrap().into_value().unwrap() {
            Value::String(s) => s,
            _ => panic!("expected string"),
        };
        assert_ne!(h1, h2);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-context -- --nocapture`
Expected: FAIL — `snapshot` path not handled, returns `Some(prompt string)` for any path

- [ ] **Step 3: Implement snapshot path handling in SystemProvider::read**

Replace the `Reader` impl for `SystemProvider` in `crates/ox-context/src/lib.rs` (lines 240-244):

```rust
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
                    Ok(Some(Record::parsed(ox_kernel::snapshot::snapshot_record(state))))
                }
            }
            _ => Ok(Some(Record::parsed(Value::String(self.prompt.clone())))),
        }
    }
}
```

- [ ] **Step 4: Implement snapshot path handling in SystemProvider::write**

Replace the `Writer` impl for `SystemProvider` in `crates/ox-context/src/lib.rs` (lines 246-260):

```rust
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
                    _ => return Err(StoreError::store("system", "write", "expected parsed record")),
                };
                // snapshot/state writes the state directly
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
                    _ => Err(StoreError::store("system", "write", "snapshot state must be a string")),
                }
            }
            _ => match data {
                Record::Parsed(Value::String(s)) => {
                    self.prompt = s;
                    Ok(Path::from_components(vec![]))
                }
                _ => Err(StoreError::store("system", "write", "expected string value")),
            },
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ox-context -- --nocapture`
Expected: all 6 system snapshot tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/ox-context/src/lib.rs
git commit -m "feat(ox-context): add snapshot lens to SystemProvider"
```

---

### Task 3: ModelProvider Snapshot

**Files:**
- Modify: `crates/ox-context/src/lib.rs:315-367` (Reader + Writer impls)

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` block in `crates/ox-context/src/lib.rs`:

```rust
    // -- ModelProvider snapshot tests --

    #[test]
    fn model_snapshot_read_returns_hash_and_state() {
        let mut mp = ModelProvider::new("claude-sonnet-4-20250514".to_string(), 4096);
        let record = mp.read(&path!("snapshot")).unwrap().unwrap();
        let val = record.into_value().unwrap();

        match &val {
            Value::Map(m) => {
                let hash = match m.get("hash").unwrap() {
                    Value::String(s) => s.clone(),
                    _ => panic!("expected string hash"),
                };
                assert_eq!(hash.len(), 16);

                let state = m.get("state").unwrap();
                match state {
                    Value::Map(sm) => {
                        assert_eq!(
                            sm.get("model").unwrap(),
                            &Value::String("claude-sonnet-4-20250514".to_string())
                        );
                        assert_eq!(sm.get("max_tokens").unwrap(), &Value::Integer(4096));
                    }
                    _ => panic!("expected map state"),
                }
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn model_snapshot_read_state_only() {
        let mut mp = ModelProvider::new("gpt-4o".to_string(), 8192);
        let record = mp.read(&path!("snapshot/state")).unwrap().unwrap();
        let val = record.into_value().unwrap();
        match val {
            Value::Map(m) => {
                assert_eq!(m.get("model").unwrap(), &Value::String("gpt-4o".to_string()));
                assert_eq!(m.get("max_tokens").unwrap(), &Value::Integer(8192));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn model_snapshot_read_hash_only() {
        let mut mp = ModelProvider::new("gpt-4o".to_string(), 8192);
        let record = mp.read(&path!("snapshot/hash")).unwrap().unwrap();
        match record.into_value().unwrap() {
            Value::String(h) => assert_eq!(h.len(), 16),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn model_snapshot_write_restores_state() {
        let mut mp = ModelProvider::new("old-model".to_string(), 1024);

        let mut state_map = std::collections::BTreeMap::new();
        state_map.insert("model".to_string(), Value::String("new-model".to_string()));
        state_map.insert("max_tokens".to_string(), Value::Integer(8192));
        let mut snap_map = std::collections::BTreeMap::new();
        snap_map.insert("state".to_string(), Value::Map(state_map));

        mp.write(&path!("snapshot"), Record::parsed(Value::Map(snap_map))).unwrap();

        // Verify
        let record = mp.read(&path!("id")).unwrap().unwrap();
        assert_eq!(record.into_value().unwrap(), Value::String("new-model".to_string()));
        let record = mp.read(&path!("max_tokens")).unwrap().unwrap();
        assert_eq!(record.into_value().unwrap(), Value::Integer(8192));
    }

    #[test]
    fn model_snapshot_write_state_path() {
        let mut mp = ModelProvider::new("old".to_string(), 1024);

        let mut state_map = std::collections::BTreeMap::new();
        state_map.insert("model".to_string(), Value::String("new".to_string()));
        state_map.insert("max_tokens".to_string(), Value::Integer(2048));

        mp.write(&path!("snapshot/state"), Record::parsed(Value::Map(state_map))).unwrap();

        let record = mp.read(&path!("id")).unwrap().unwrap();
        assert_eq!(record.into_value().unwrap(), Value::String("new".to_string()));
        let record = mp.read(&path!("max_tokens")).unwrap().unwrap();
        assert_eq!(record.into_value().unwrap(), Value::Integer(2048));
    }
```

- [ ] **Step 2: Run test to verify they fail**

Run: `cargo test -p ox-context model_snapshot -- --nocapture`
Expected: FAIL — `snapshot` path returns `None` in current ModelProvider

- [ ] **Step 3: Implement snapshot in ModelProvider::read**

Replace the `Reader` impl for `ModelProvider` in `crates/ox-context/src/lib.rs`:

```rust
impl Reader for ModelProvider {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = if from.is_empty() {
            ""
        } else {
            from.components[0].as_str()
        };
        match key {
            "" | "id" => Ok(Some(Record::parsed(Value::String(self.model.clone())))),
            "max_tokens" => Ok(Some(Record::parsed(Value::Integer(self.max_tokens as i64)))),
            "snapshot" => {
                let state = self.snapshot_state();
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
                    Ok(Some(Record::parsed(ox_kernel::snapshot::snapshot_record(state))))
                }
            }
            _ => Ok(None),
        }
    }
}
```

Add a helper method on `ModelProvider`:

```rust
impl ModelProvider {
    pub fn new(model: String, max_tokens: u32) -> Self {
        Self { model, max_tokens }
    }

    fn snapshot_state(&self) -> Value {
        let mut map = std::collections::BTreeMap::new();
        map.insert("max_tokens".to_string(), Value::Integer(self.max_tokens as i64));
        map.insert("model".to_string(), Value::String(self.model.clone()));
        Value::Map(map)
    }
}
```

- [ ] **Step 4: Implement snapshot in ModelProvider::write**

Replace the `Writer` impl for `ModelProvider`:

```rust
impl Writer for ModelProvider {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let key = if to.is_empty() {
            ""
        } else {
            to.components[0].as_str()
        };
        match key {
            "" | "id" => match data {
                Record::Parsed(Value::String(s)) => {
                    self.model = s;
                    Ok(to.clone())
                }
                _ => Err(StoreError::store("model", "write", "expected string for id")),
            },
            "max_tokens" => match data {
                Record::Parsed(Value::Integer(n)) => {
                    self.max_tokens = n as u32;
                    Ok(to.clone())
                }
                _ => Err(StoreError::store("model", "write", "expected integer for max_tokens")),
            },
            "snapshot" => {
                let value = match data {
                    Record::Parsed(v) => v,
                    _ => return Err(StoreError::store("model", "write", "expected parsed record")),
                };
                let state = if to.components.len() >= 2 && to.components[1].as_str() == "state" {
                    value
                } else {
                    ox_kernel::snapshot::extract_snapshot_state(value)
                        .map_err(|e| StoreError::store("model", "write", e))?
                };
                match state {
                    Value::Map(m) => {
                        if let Some(Value::String(model)) = m.get("model") {
                            self.model = model.clone();
                        }
                        if let Some(Value::Integer(n)) = m.get("max_tokens") {
                            self.max_tokens = *n as u32;
                        }
                        Ok(to.clone())
                    }
                    _ => Err(StoreError::store("model", "write", "snapshot state must be a map with model and max_tokens")),
                }
            }
            _ => Err(StoreError::store("model", "write", format!("unknown path: {to}"))),
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ox-context -- --nocapture`
Expected: all model + system snapshot tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/ox-context/src/lib.rs
git commit -m "feat(ox-context): add snapshot lens to ModelProvider"
```

---

### Task 4: ToolsProvider Snapshot (explicit null)

**Files:**
- Modify: `crates/ox-context/src/lib.rs:274-289` (Reader impl)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/ox-context/src/lib.rs`:

```rust
    // -- ToolsProvider snapshot tests --

    #[test]
    fn tools_snapshot_returns_none() {
        let mut tp = ToolsProvider::new(vec![]);
        let result = tp.read(&path!("snapshot")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn tools_snapshot_hash_returns_none() {
        let mut tp = ToolsProvider::new(vec![]);
        let result = tp.read(&path!("snapshot/hash")).unwrap();
        assert!(result.is_none());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-context tools_snapshot -- --nocapture`
Expected: FAIL — `snapshot` currently falls through to `_ => Ok(None)`, which... actually passes. Let's verify this is the intentional behavior and not an accident. The current ToolsProvider returns None for any unrecognized path, so the test should pass already. Let's adjust — we still want to verify the behavior is stable and intentional.

Run: `cargo test -p ox-context tools_snapshot -- --nocapture`
Expected: PASS (behavior already correct — ToolsProvider returns None for unknown paths including "snapshot")

- [ ] **Step 3: Commit (test-only)**

```bash
git add crates/ox-context/src/lib.rs
git commit -m "test(ox-context): verify ToolsProvider returns None for snapshot"
```

---

### Task 5: HistoryProvider Snapshot

**Files:**
- Modify: `crates/ox-history/Cargo.toml:15-19`
- Modify: `crates/ox-history/src/lib.rs:181-239`

- [ ] **Step 1: Write the failing tests**

Add to the bottom of `crates/ox-history/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;

    fn append_user_msg(hp: &mut HistoryProvider, text: &str) {
        let json = serde_json::json!({"role": "user", "content": text});
        let value = json_to_value(json);
        hp.write(&path!("append"), Record::parsed(value)).unwrap();
    }

    fn append_assistant_msg(hp: &mut HistoryProvider, text: &str) {
        let json = serde_json::json!({
            "role": "assistant",
            "content": [{"type": "text", "text": text}]
        });
        let value = json_to_value(json);
        hp.write(&path!("append"), Record::parsed(value)).unwrap();
    }

    #[test]
    fn snapshot_empty_history() {
        let mut hp = HistoryProvider::new();
        let record = hp.read(&path!("snapshot")).unwrap().unwrap();
        let val = record.into_value().unwrap();

        match &val {
            Value::Map(m) => {
                assert_eq!(m.get("hash").unwrap(), &Value::String("0000000000000000".to_string()));
                let state = m.get("state").unwrap();
                match state {
                    Value::Map(sm) => {
                        assert_eq!(sm.get("count").unwrap(), &Value::Integer(0));
                        assert_eq!(sm.get("last_hash").unwrap(), &Value::Null);
                    }
                    _ => panic!("expected map state"),
                }
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn snapshot_with_messages() {
        let mut hp = HistoryProvider::new();
        append_user_msg(&mut hp, "hello");
        append_assistant_msg(&mut hp, "hi there");

        let record = hp.read(&path!("snapshot")).unwrap().unwrap();
        let val = record.into_value().unwrap();

        match &val {
            Value::Map(m) => {
                let hash = match m.get("hash").unwrap() {
                    Value::String(s) => s.clone(),
                    _ => panic!("expected string hash"),
                };
                assert_eq!(hash.len(), 16);
                assert_ne!(hash, "0000000000000000");

                let state = m.get("state").unwrap();
                match state {
                    Value::Map(sm) => {
                        assert_eq!(sm.get("count").unwrap(), &Value::Integer(2));
                        match sm.get("last_hash").unwrap() {
                            Value::String(lh) => assert_eq!(lh.len(), 16),
                            _ => panic!("expected string last_hash"),
                        }
                    }
                    _ => panic!("expected map state"),
                }
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn snapshot_hash_subpath() {
        let mut hp = HistoryProvider::new();
        append_user_msg(&mut hp, "test");
        let record = hp.read(&path!("snapshot/hash")).unwrap().unwrap();
        match record.into_value().unwrap() {
            Value::String(h) => assert_eq!(h.len(), 16),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn snapshot_state_subpath() {
        let mut hp = HistoryProvider::new();
        append_user_msg(&mut hp, "test");
        let record = hp.read(&path!("snapshot/state")).unwrap().unwrap();
        match record.into_value().unwrap() {
            Value::Map(m) => {
                assert_eq!(m.get("count").unwrap(), &Value::Integer(1));
                assert!(m.contains_key("last_hash"));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn snapshot_write_returns_error() {
        let mut hp = HistoryProvider::new();
        let result = hp.write(&path!("snapshot"), Record::parsed(Value::Null));
        assert!(result.is_err());
    }

    #[test]
    fn snapshot_last_hash_changes_on_append() {
        let mut hp = HistoryProvider::new();
        append_user_msg(&mut hp, "first");

        let h1 = match hp.read(&path!("snapshot/state")).unwrap().unwrap().into_value().unwrap() {
            Value::Map(m) => match m.get("last_hash").unwrap() {
                Value::String(s) => s.clone(),
                _ => panic!("expected string"),
            },
            _ => panic!("expected map"),
        };

        append_assistant_msg(&mut hp, "second");

        let h2 = match hp.read(&path!("snapshot/state")).unwrap().unwrap().into_value().unwrap() {
            Value::Map(m) => match m.get("last_hash").unwrap() {
                Value::String(s) => s.clone(),
                _ => panic!("expected string"),
            },
            _ => panic!("expected map"),
        };

        assert_ne!(h1, h2);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ox-history snapshot -- --nocapture`
Expected: FAIL — `snapshot` path returns `None`

- [ ] **Step 3: Implement snapshot in HistoryProvider::read**

First, add `sha2` as a dependency to `crates/ox-history/Cargo.toml`:

```toml
sha2 = { workspace = true }
```

Replace the `Reader` impl for `HistoryProvider` in `crates/ox-history/src/lib.rs`:

```rust
impl Reader for HistoryProvider {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = if from.is_empty() {
            ""
        } else {
            from.components[0].as_str()
        };
        match key {
            "" | "messages" => {
                let wire = self.to_wire_messages();
                let json = serde_json::to_value(wire)
                    .map_err(|e| StoreError::store("history", "read", e.to_string()))?;
                Ok(Some(Record::parsed(json_to_value(json))))
            }
            "count" => Ok(Some(Record::parsed(Value::Integer(
                self.messages.len() as i64
            )))),
            "snapshot" => {
                let state = self.snapshot_state();
                if from.components.len() >= 2 {
                    match from.components[1].as_str() {
                        "hash" => {
                            let hash = self.snapshot_outer_hash(&state);
                            Ok(Some(Record::parsed(Value::String(hash))))
                        }
                        "state" => Ok(Some(Record::parsed(state))),
                        _ => Ok(None),
                    }
                } else {
                    let hash = self.snapshot_outer_hash(&state);
                    let mut map = std::collections::BTreeMap::new();
                    map.insert("hash".to_string(), Value::String(hash));
                    map.insert("state".to_string(), state);
                    Ok(Some(Record::parsed(Value::Map(map))))
                }
            }
            _ => Ok(None),
        }
    }
}
```

Add helper methods on `HistoryProvider`. Add these imports at the top of the file:

```rust
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
```

Add these methods to the `impl HistoryProvider` block:

```rust
    /// Content hash of a single message: SHA-256 of its JSON, truncated to 16 hex chars.
    fn message_hash(msg: &serde_json::Value) -> String {
        let bytes = serde_json::to_vec(msg).expect("message always serializes");
        let digest = Sha256::digest(&bytes);
        digest[..8].iter().map(|b| format!("{b:02x}")).collect()
    }

    fn snapshot_state(&self) -> Value {
        let mut map = BTreeMap::new();
        map.insert("count".to_string(), Value::Integer(self.messages.len() as i64));

        if self.messages.is_empty() {
            map.insert("last_hash".to_string(), Value::Null);
        } else {
            let wire = self.to_wire_messages();
            let last = wire.last().unwrap();
            map.insert(
                "last_hash".to_string(),
                Value::String(Self::message_hash(last)),
            );
        }
        Value::Map(map)
    }

    /// Outer snapshot hash. For empty history, returns all zeros.
    fn snapshot_outer_hash(&self, state: &Value) -> String {
        if self.messages.is_empty() {
            "0000000000000000".to_string()
        } else {
            ox_kernel::snapshot::snapshot_hash(state)
        }
    }
```

- [ ] **Step 4: Implement snapshot write rejection in HistoryProvider::write**

Replace the `Writer` impl for `HistoryProvider`:

```rust
impl Writer for HistoryProvider {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let key = if to.is_empty() {
            ""
        } else {
            to.components[0].as_str()
        };
        match key {
            "" | "append" => {
                let value = match data {
                    Record::Parsed(v) => v,
                    _ => {
                        return Err(StoreError::store(
                            "history",
                            "write",
                            "expected parsed record",
                        ));
                    }
                };
                let json = value_to_json(value);
                let msg = parse_wire_message(&json)
                    .map_err(|e| StoreError::store("history", "write", e))?;
                self.messages.push(msg);
                Ok(to.clone())
            }
            "clear" => {
                self.messages.clear();
                Ok(to.clone())
            }
            "snapshot" => Err(StoreError::store(
                "history",
                "write",
                "snapshot write not supported — restore history via ledger replay through append",
            )),
            _ => Err(StoreError::store(
                "history",
                "write",
                format!("unknown write path: {to}"),
            )),
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ox-history -- --nocapture`
Expected: all 7 snapshot tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/ox-history/src/lib.rs crates/ox-history/Cargo.toml
git commit -m "feat(ox-history): add snapshot lens (read-only, write returns error)"
```

---

### Task 6: GateStore Snapshot

**Files:**
- Modify: `crates/ox-gate/src/lib.rs:115-371`

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` block in `crates/ox-gate/src/lib.rs`:

```rust
    // -- Snapshot tests --

    #[test]
    fn snapshot_read_returns_hash_and_state() {
        let mut gate = GateStore::new();
        let record = gate.read(&path!("snapshot")).unwrap().unwrap();
        let val = record.into_value().unwrap();

        match &val {
            Value::Map(m) => {
                let hash = match m.get("hash").unwrap() {
                    Value::String(s) => s.clone(),
                    _ => panic!("expected string hash"),
                };
                assert_eq!(hash.len(), 16);

                let state = m.get("state").unwrap();
                match state {
                    Value::Map(sm) => {
                        assert!(sm.contains_key("bootstrap"));
                        assert!(sm.contains_key("providers"));
                        assert!(sm.contains_key("accounts"));
                        // Verify keys are excluded from accounts
                        let accounts = match sm.get("accounts").unwrap() {
                            Value::Map(a) => a,
                            _ => panic!("expected map"),
                        };
                        for (_name, acct) in accounts {
                            let acct_json = value_to_json(acct.clone());
                            assert!(acct_json.get("key").is_none(), "API keys must be excluded from snapshot");
                        }
                    }
                    _ => panic!("expected map state"),
                }
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn snapshot_read_hash_only() {
        let mut gate = GateStore::new();
        let record = gate.read(&path!("snapshot/hash")).unwrap().unwrap();
        match record.into_value().unwrap() {
            Value::String(h) => assert_eq!(h.len(), 16),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn snapshot_read_state_only() {
        let mut gate = GateStore::new();
        let record = gate.read(&path!("snapshot/state")).unwrap().unwrap();
        match record.into_value().unwrap() {
            Value::Map(m) => {
                assert!(m.contains_key("bootstrap"));
                assert!(m.contains_key("providers"));
                assert!(m.contains_key("accounts"));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn snapshot_excludes_api_keys() {
        let mut gate = GateStore::new();
        // Set an API key
        gate.write(
            &path!("accounts/anthropic/key"),
            Record::parsed(Value::String("sk-secret".to_string())),
        )
        .unwrap();

        let record = gate.read(&path!("snapshot/state")).unwrap().unwrap();
        let json = value_to_json(record.into_value().unwrap());
        let accounts = &json["accounts"];
        // No account should have a "key" field
        for (_name, acct) in accounts.as_object().unwrap() {
            assert!(acct.get("key").is_none(), "API keys must not appear in snapshot");
        }
    }

    #[test]
    fn snapshot_write_restores_state() {
        let mut gate = GateStore::new();

        // Set a key first (should be cleared on restore)
        gate.write(
            &path!("accounts/anthropic/key"),
            Record::parsed(Value::String("sk-secret".to_string())),
        )
        .unwrap();

        // Build a snapshot state with different config
        let state_json = serde_json::json!({
            "bootstrap": "openai",
            "providers": {
                "openai": {
                    "dialect": "openai",
                    "endpoint": "https://api.openai.com/v1/chat/completions",
                    "version": ""
                }
            },
            "accounts": {
                "openai": {
                    "model": "gpt-4o",
                    "provider": "openai"
                }
            }
        });
        let state = json_to_value(state_json);
        let mut snap_map = std::collections::BTreeMap::new();
        snap_map.insert("state".to_string(), state);

        gate.write(&path!("snapshot"), Record::parsed(Value::Map(snap_map)))
            .unwrap();

        // Verify bootstrap changed
        let record = gate.read(&path!("bootstrap")).unwrap().unwrap();
        match record.into_value().unwrap() {
            Value::String(s) => assert_eq!(s, "openai"),
            _ => panic!("expected string"),
        }

        // Verify only openai provider remains
        assert!(gate.read(&path!("providers/anthropic")).unwrap().is_none());
        assert!(gate.read(&path!("providers/openai")).unwrap().is_some());

        // Verify only openai account remains
        assert!(gate.read(&path!("accounts/anthropic")).unwrap().is_none());

        // Verify API keys are cleared (empty)
        let record = gate.read(&path!("accounts/openai/key")).unwrap().unwrap();
        match record.into_value().unwrap() {
            Value::String(s) => assert!(s.is_empty(), "keys should be empty after restore"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn snapshot_write_via_state_path() {
        let mut gate = GateStore::new();

        let state_json = serde_json::json!({
            "bootstrap": "openai",
            "providers": {
                "openai": {
                    "dialect": "openai",
                    "endpoint": "https://api.openai.com/v1/chat/completions",
                    "version": ""
                }
            },
            "accounts": {
                "openai": {
                    "model": "gpt-4o",
                    "provider": "openai"
                }
            }
        });
        let state = json_to_value(state_json);

        gate.write(&path!("snapshot/state"), Record::parsed(state)).unwrap();

        let record = gate.read(&path!("bootstrap")).unwrap().unwrap();
        match record.into_value().unwrap() {
            Value::String(s) => assert_eq!(s, "openai"),
            _ => panic!("expected string"),
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ox-gate snapshot -- --nocapture`
Expected: FAIL — `snapshot` path returns `None` in current GateStore reader

- [ ] **Step 3: Implement snapshot in GateStore::read**

Add this import to the top of `crates/ox-gate/src/lib.rs`:

```rust
use std::collections::BTreeMap;
use structfs_serde_store::json_to_value;
```

Add a helper method to `impl GateStore`:

```rust
    /// Build the snapshot state: bootstrap + providers + accounts (keys excluded).
    fn snapshot_state(&self) -> Value {
        let mut state = BTreeMap::new();

        // bootstrap
        state.insert(
            "bootstrap".to_string(),
            Value::String(self.bootstrap.clone()),
        );

        // providers (all fields)
        let mut providers_map = BTreeMap::new();
        for (name, config) in &self.providers {
            let v = to_value(config).expect("ProviderConfig always serializes");
            providers_map.insert(name.clone(), v);
        }
        state.insert("providers".to_string(), Value::Map(providers_map));

        // accounts (exclude keys)
        let mut accounts_map = BTreeMap::new();
        for (name, config) in &self.accounts {
            let mut acct = BTreeMap::new();
            acct.insert("model".to_string(), Value::String(config.model.clone()));
            acct.insert(
                "provider".to_string(),
                Value::String(config.provider.clone()),
            );
            accounts_map.insert(name.clone(), Value::Map(acct));
        }
        state.insert("accounts".to_string(), Value::Map(accounts_map));

        Value::Map(state)
    }
```

Add the `"snapshot"` arm to `Reader` impl, in the `match first` block before the `_ => Ok(None)` arm:

```rust
            "snapshot" => {
                let state = self.snapshot_state();
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
                    Ok(Some(Record::parsed(
                        ox_kernel::snapshot::snapshot_record(state),
                    )))
                }
            }
```

- [ ] **Step 4: Implement snapshot in GateStore::write**

Add the `"snapshot"` arm to `Writer` impl, before the final `_ =>` arm:

```rust
            "snapshot" => {
                let value = match data {
                    Record::Parsed(v) => v,
                    _ => {
                        return Err(StoreError::store(
                            "gate",
                            "write",
                            "expected parsed record",
                        ))
                    }
                };
                let state = if to.components.len() >= 2
                    && to.components[1].as_str() == "state"
                {
                    value
                } else {
                    ox_kernel::snapshot::extract_snapshot_state(value)
                        .map_err(|e| StoreError::store("gate", "write", e))?
                };
                self.restore_from_snapshot(state)?;
                Ok(to.clone())
            }
```

Add the restore helper to `impl GateStore`:

```rust
    /// Restore the store from a snapshot state value.
    fn restore_from_snapshot(&mut self, state: Value) -> Result<(), StoreError> {
        let state_map = match state {
            Value::Map(m) => m,
            _ => {
                return Err(StoreError::store(
                    "gate",
                    "write",
                    "snapshot state must be a map",
                ))
            }
        };

        // bootstrap
        if let Some(Value::String(b)) = state_map.get("bootstrap") {
            self.bootstrap = b.clone();
        }

        // providers — full replacement
        if let Some(providers_val) = state_map.get("providers") {
            let providers_json = structfs_serde_store::value_to_json(providers_val.clone());
            let providers: HashMap<String, ProviderConfig> =
                serde_json::from_value(providers_json)
                    .map_err(|e| StoreError::store("gate", "write", e.to_string()))?;
            self.providers = providers;
        }

        // accounts — full replacement, keys cleared
        if let Some(accounts_val) = state_map.get("accounts") {
            let mut new_accounts = HashMap::new();
            match accounts_val {
                Value::Map(accts) => {
                    for (name, acct_val) in accts {
                        let acct_json =
                            structfs_serde_store::value_to_json(acct_val.clone());
                        let provider = acct_json
                            .get("provider")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let model = acct_json
                            .get("model")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        new_accounts.insert(
                            name.clone(),
                            AccountConfig {
                                provider,
                                key: String::new(), // keys always cleared on restore
                                model,
                            },
                        );
                    }
                }
                _ => {
                    return Err(StoreError::store(
                        "gate",
                        "write",
                        "accounts must be a map",
                    ))
                }
            }
            self.accounts = new_accounts;
        }

        Ok(())
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ox-gate -- --nocapture`
Expected: all snapshot tests + existing tests pass

- [ ] **Step 6: Run full workspace check**

Run: `cargo check`
Expected: clean build, no errors

Run: `cargo test`
Expected: all tests pass across all crates

- [ ] **Step 7: Commit**

```bash
git add crates/ox-gate/src/lib.rs
git commit -m "feat(ox-gate): add snapshot lens (keys excluded from state)"
```

---

### Task 7: Cross-Store Integration Smoke Test

**Files:**
- Modify: `crates/ox-context/src/lib.rs` (add integration test)

This test verifies the coordinator discovery pattern from the RFC: reading `{mount}/snapshot` from each mount and assembling the context.

- [ ] **Step 1: Write the integration test**

Add to the `tests` module in `crates/ox-context/src/lib.rs`:

```rust
    // -- Integration: coordinator discovery via Namespace --

    #[test]
    fn namespace_snapshot_discovery() {
        let mut ns = Namespace::new();
        ns.mount("system", Box::new(SystemProvider::new("You are helpful.".to_string())));
        ns.mount("model", Box::new(ModelProvider::new("claude-sonnet-4-20250514".to_string(), 4096)));
        ns.mount("tools", Box::new(ToolsProvider::new(vec![])));

        // system participates
        let record = ns.read(&path!("system/snapshot")).unwrap();
        assert!(record.is_some());

        // model participates
        let record = ns.read(&path!("model/snapshot")).unwrap();
        assert!(record.is_some());

        // tools does NOT participate
        let record = ns.read(&path!("tools/snapshot")).unwrap();
        assert!(record.is_none());
    }

    #[test]
    fn namespace_snapshot_roundtrip() {
        let mut ns = Namespace::new();
        ns.mount("system", Box::new(SystemProvider::new("original".to_string())));
        ns.mount("model", Box::new(ModelProvider::new("model-a".to_string(), 1024)));

        // Read snapshots
        let sys_snap = ns.read(&path!("system/snapshot/state")).unwrap().unwrap().into_value().unwrap();
        let model_snap = ns.read(&path!("model/snapshot/state")).unwrap().unwrap().into_value().unwrap();

        // Mutate
        ns.write(&path!("system"), Record::parsed(Value::String("changed".to_string()))).unwrap();
        ns.write(&path!("model/id"), Record::parsed(Value::String("model-b".to_string()))).unwrap();

        // Restore from snapshots
        ns.write(&path!("system/snapshot/state"), Record::parsed(sys_snap)).unwrap();
        ns.write(&path!("model/snapshot/state"), Record::parsed(model_snap)).unwrap();

        // Verify restoration
        let record = ns.read(&path!("system")).unwrap().unwrap();
        assert_eq!(record.into_value().unwrap(), Value::String("original".to_string()));

        let record = ns.read(&path!("model/id")).unwrap().unwrap();
        assert_eq!(record.into_value().unwrap(), Value::String("model-a".to_string()));
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p ox-context namespace_snapshot -- --nocapture`
Expected: PASS — these tests exercise the already-implemented snapshot paths through the Namespace router

- [ ] **Step 3: Run full quality gates**

Run: `./scripts/quality_gates.sh`
Expected: all 14 gates pass

- [ ] **Step 4: Commit**

```bash
git add crates/ox-context/src/lib.rs
git commit -m "test(ox-context): add namespace snapshot discovery + roundtrip integration tests"
```

---

## Summary

| Task | Store | Read | Write | Tests |
|------|-------|------|-------|-------|
| 1 | (ox-kernel helpers) | — | — | 6 unit tests |
| 2 | SystemProvider | `snapshot`, `snapshot/hash`, `snapshot/state` | `snapshot`, `snapshot/state` → restore | 6 tests |
| 3 | ModelProvider | `snapshot`, `snapshot/hash`, `snapshot/state` | `snapshot`, `snapshot/state` → restore | 5 tests |
| 4 | ToolsProvider | `snapshot` → None | N/A (read-only) | 2 tests |
| 5 | HistoryProvider | `snapshot`, `snapshot/hash`, `snapshot/state` | `snapshot` → error | 7 tests |
| 6 | GateStore | `snapshot`, `snapshot/hash`, `snapshot/state` | `snapshot`, `snapshot/state` → restore (keys cleared) | 6 tests |
| 7 | (integration) | Namespace discovery + roundtrip | Namespace roundtrip | 2 tests |

**Total: 34 tests across 7 commits.**

Plan B (SnapshotStore coordinator, thread directory format, SQLite migration, ox-cli integration) follows after Plan A lands.

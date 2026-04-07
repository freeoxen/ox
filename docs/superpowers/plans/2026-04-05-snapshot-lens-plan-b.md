# Snapshot Lens Plan B — Thread Directory + Coordinator

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the raw JSONL persistence with portable thread directories (context.json + ledger.jsonl + view.json) coordinated by a snapshot function that reads/writes namespace store snapshots.

**Architecture:** A content-addressed ledger replaces the raw message dump. A snapshot coordinator reads `{mount}/snapshot/state` from each participating store in the Namespace and assembles context.json. On restore, context.json writes back to each store's snapshot path and the ledger replays through `history/append`. SQLite gains `last_seq`/`last_hash` columns as a derived cache. The coordinator is a function that takes `&mut Namespace` (matching the `synthesize_prompt()` pattern), not a Store.

**Tech Stack:** Rust, sha2, serde_json, rusqlite, structfs-core-store, structfs-serde-store

**Dependencies from Plan A:** `ox_kernel::snapshot::{snapshot_hash, snapshot_record, extract_snapshot_state}`, snapshot path handling in SystemProvider, ModelProvider, HistoryProvider, GateStore.

---

## File Structure

| File | Responsibility |
|------|---------------|
| `crates/ox-inbox/src/ledger.rs` (create) | LedgerEntry type, content-addressed hash chain, append/read JSONL |
| `crates/ox-inbox/src/thread_dir.rs` (create) | Thread directory I/O: context.json, view.json, ledger.jsonl wiring |
| `crates/ox-inbox/src/snapshot.rs` (create) | Snapshot coordinator: save/restore namespace state to/from thread directory |
| `crates/ox-inbox/src/schema.rs` (modify) | Add `last_seq`, `last_hash` columns via migration |
| `crates/ox-inbox/src/model.rs` (modify) | Add `last_seq`, `last_hash` fields to ThreadMetadata |
| `crates/ox-inbox/src/writer.rs` (modify) | Write-through for last_seq/last_hash on message append |
| `crates/ox-inbox/src/lib.rs` (modify) | Wire up new modules |
| `crates/ox-inbox/Cargo.toml` (modify) | Add sha2 dep |
| `crates/ox-cli/src/agents.rs` (modify) | Replace save_history/restore with snapshot coordinator |

---

### Task 1: Ledger Types and Hash Chain

**Files:**
- Modify: `crates/ox-inbox/Cargo.toml`
- Create: `crates/ox-inbox/src/ledger.rs`
- Modify: `crates/ox-inbox/src/lib.rs`

The ledger is an append-only JSONL file where each entry has a sequence number, a content hash of the message, and a parent hash forming a chain.

- [ ] **Step 1: Add sha2 dependency**

Add to `crates/ox-inbox/Cargo.toml` under `[dependencies]`:

```toml
sha2 = { workspace = true }
```

- [ ] **Step 2: Write the failing tests**

Create `crates/ox-inbox/src/ledger.rs` with tests only:

```rust
//! Content-addressed ledger — append-only message log with hash chain.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_hash_is_deterministic() {
        let msg = serde_json::json!({"role": "user", "content": "hello"});
        let h1 = entry_hash(&msg);
        let h2 = entry_hash(&msg);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn entry_hash_differs_for_different_messages() {
        let m1 = serde_json::json!({"role": "user", "content": "hello"});
        let m2 = serde_json::json!({"role": "user", "content": "world"});
        assert_ne!(entry_hash(&m1), entry_hash(&m2));
    }

    #[test]
    fn append_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");

        let msg1 = serde_json::json!({"role": "user", "content": "first"});
        let e1 = append_entry(&path, &msg1, None).unwrap();
        assert_eq!(e1.seq, 0);
        assert!(e1.parent.is_none());

        let msg2 = serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "reply"}]});
        let e2 = append_entry(&path, &msg2, Some(&e1)).unwrap();
        assert_eq!(e2.seq, 1);
        assert_eq!(e2.parent.as_deref(), Some(e1.hash.as_str()));

        let entries = read_ledger(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].seq, 0);
        assert_eq!(entries[1].seq, 1);
        assert_eq!(entries[1].parent.as_deref(), Some(entries[0].hash.as_str()));
    }

    #[test]
    fn read_last_entry_returns_none_for_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        assert!(read_last_entry(&path).unwrap().is_none());
    }

    #[test]
    fn read_last_entry_returns_latest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");

        let msg1 = serde_json::json!({"role": "user", "content": "first"});
        let e1 = append_entry(&path, &msg1, None).unwrap();
        let msg2 = serde_json::json!({"role": "user", "content": "second"});
        let _e2 = append_entry(&path, &msg2, Some(&e1)).unwrap();

        let last = read_last_entry(&path).unwrap().unwrap();
        assert_eq!(last.seq, 1);
    }

    #[test]
    fn hash_chain_integrity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");

        let msg1 = serde_json::json!({"role": "user", "content": "a"});
        let e1 = append_entry(&path, &msg1, None).unwrap();
        let msg2 = serde_json::json!({"role": "user", "content": "b"});
        let e2 = append_entry(&path, &msg2, Some(&e1)).unwrap();
        let msg3 = serde_json::json!({"role": "user", "content": "c"});
        let e3 = append_entry(&path, &msg3, Some(&e2)).unwrap();

        let entries = read_ledger(&path).unwrap();
        // Verify chain: each entry's parent matches previous entry's hash
        assert!(entries[0].parent.is_none());
        assert_eq!(entries[1].parent.as_deref(), Some(entries[0].hash.as_str()));
        assert_eq!(entries[2].parent.as_deref(), Some(entries[1].hash.as_str()));
        // Verify hashes are independently computed from message content
        assert_eq!(entries[0].hash, entry_hash(&entries[0].msg));
        assert_eq!(entries[2].hash, e3.hash);
    }

    #[test]
    fn read_nonexistent_file_returns_empty() {
        let path = std::path::Path::new("/tmp/nonexistent_ledger_test.jsonl");
        let entries = read_ledger(path).unwrap();
        assert!(entries.is_empty());
    }
}
```

- [ ] **Step 3: Wire up the module**

Add to `crates/ox-inbox/src/lib.rs` after `pub(crate) mod jsonl;`:

```rust
pub mod ledger;
```

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test -p ox-inbox ledger -- --nocapture`
Expected: compilation errors — types and functions not defined

- [ ] **Step 5: Implement the ledger module**

Add implementation above the `#[cfg(test)]` block in `crates/ox-inbox/src/ledger.rs`:

```rust
//! Content-addressed ledger — append-only message log with hash chain.

use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

/// A single ledger entry with content-addressed hash and parent chain.
#[derive(Debug, Clone)]
pub struct LedgerEntry {
    pub seq: u64,
    pub hash: String,
    pub parent: Option<String>,
    pub msg: serde_json::Value,
}

/// Compute the content hash of a message: SHA-256 of its JSON, truncated to 16 hex chars.
pub fn entry_hash(msg: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(msg).expect("message always serializes");
    let digest = Sha256::digest(&bytes);
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Append a new entry to the ledger file. Returns the created entry.
///
/// `prev` is the previous entry (for parent hash and seq computation).
/// Pass `None` for the first entry.
pub fn append_entry(
    path: &Path,
    msg: &serde_json::Value,
    prev: Option<&LedgerEntry>,
) -> Result<LedgerEntry, String> {
    let seq = prev.map_or(0, |e| e.seq + 1);
    let parent = prev.map(|e| e.hash.clone());
    let hash = entry_hash(msg);

    let entry = LedgerEntry {
        seq,
        hash: hash.clone(),
        parent: parent.clone(),
        msg: msg.clone(),
    };

    let line = serde_json::json!({
        "seq": seq,
        "hash": hash,
        "parent": parent,
        "msg": msg,
    });
    let line_str = serde_json::to_string(&line).map_err(|e| e.to_string())?;

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    writeln!(file, "{line_str}").map_err(|e| e.to_string())?;

    Ok(entry)
}

/// Read all entries from a ledger file.
pub fn read_ledger(path: &Path) -> Result<Vec<LedgerEntry>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();
    for line in reader.lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.is_empty() {
            continue;
        }
        let json: serde_json::Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        let seq = json["seq"].as_u64().ok_or("missing seq")?;
        let hash = json["hash"].as_str().ok_or("missing hash")?.to_string();
        let parent = json["parent"].as_str().map(|s| s.to_string());
        let msg = json.get("msg").ok_or("missing msg")?.clone();
        entries.push(LedgerEntry {
            seq,
            hash,
            parent,
            msg,
        });
    }
    Ok(entries)
}

/// Read just the last entry from a ledger file (efficient — reads from end).
pub fn read_last_entry(path: &Path) -> Result<Option<LedgerEntry>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let last_line = content.lines().rev().find(|l| !l.is_empty());
    match last_line {
        None => Ok(None),
        Some(line) => {
            let json: serde_json::Value =
                serde_json::from_str(line).map_err(|e| e.to_string())?;
            let seq = json["seq"].as_u64().ok_or("missing seq")?;
            let hash = json["hash"].as_str().ok_or("missing hash")?.to_string();
            let parent = json["parent"].as_str().map(|s| s.to_string());
            let msg = json.get("msg").ok_or("missing msg")?.clone();
            Ok(Some(LedgerEntry {
                seq,
                hash,
                parent,
                msg,
            }))
        }
    }
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p ox-inbox ledger -- --nocapture`
Expected: all 7 tests pass

- [ ] **Step 7: Commit**

```bash
git add crates/ox-inbox/Cargo.toml crates/ox-inbox/src/ledger.rs crates/ox-inbox/src/lib.rs
git commit -m 'feat(ox-inbox): content-addressed ledger with hash chain'
```

---

### Task 2: Thread Directory I/O

**Files:**
- Create: `crates/ox-inbox/src/thread_dir.rs`
- Modify: `crates/ox-inbox/src/lib.rs`

Thread directories have the format:
```
~/.ox/threads/{thread_id}/
  context.json    — snapshot of non-history stores + metadata
  ledger.jsonl    — content-addressed message log
  view.json       — projection manifest (default for now)
```

- [ ] **Step 1: Write the failing tests**

Create `crates/ox-inbox/src/thread_dir.rs` with tests only:

```rust
//! Thread directory format — read/write context.json and view.json.

#[cfg(test)]
mod tests {
    use super::*;

    fn make_context(thread_id: &str, title: &str) -> ContextFile {
        let mut stores = std::collections::BTreeMap::new();
        stores.insert(
            "system".to_string(),
            serde_json::json!("You are helpful."),
        );
        stores.insert(
            "model".to_string(),
            serde_json::json!({"model": "claude-sonnet-4-20250514", "max_tokens": 4096}),
        );
        ContextFile {
            version: 1,
            thread_id: thread_id.to_string(),
            title: title.to_string(),
            labels: vec!["backend".to_string()],
            created_at: 1712345678,
            updated_at: 1712345900,
            stores,
        }
    }

    #[test]
    fn write_and_read_context() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_context("t_abc123", "Test thread");
        write_context(dir.path(), &ctx).unwrap();

        let read_back = read_context(dir.path()).unwrap().unwrap();
        assert_eq!(read_back.version, 1);
        assert_eq!(read_back.thread_id, "t_abc123");
        assert_eq!(read_back.title, "Test thread");
        assert_eq!(read_back.labels, vec!["backend"]);
        assert_eq!(read_back.stores.len(), 2);
        assert_eq!(read_back.stores["system"], serde_json::json!("You are helpful."));
    }

    #[test]
    fn read_context_returns_none_for_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_context(dir.path()).unwrap().is_none());
    }

    #[test]
    fn write_and_read_default_view() {
        let dir = tempfile::tempdir().unwrap();
        write_default_view(dir.path()).unwrap();

        let view = read_view(dir.path()).unwrap().unwrap();
        assert!(view.parent.is_none());
        assert_eq!(view.include.len(), 1);
        assert_eq!(view.include[0].start, 0);
        assert!(view.include[0].end.is_none());
        assert!(view.masks.is_empty());
        assert!(view.replacements.is_empty());
    }

    #[test]
    fn read_view_returns_none_for_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_view(dir.path()).unwrap().is_none());
    }
}
```

- [ ] **Step 2: Wire up the module**

Add to `crates/ox-inbox/src/lib.rs`:

```rust
pub mod thread_dir;
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ox-inbox thread_dir -- --nocapture`
Expected: compilation errors

- [ ] **Step 4: Implement thread_dir module**

Add implementation above the `#[cfg(test)]` block in `crates/ox-inbox/src/thread_dir.rs`:

```rust
//! Thread directory format — read/write context.json and view.json.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// The context.json file — snapshot of non-history stores + thread metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextFile {
    pub version: u32,
    pub thread_id: String,
    pub title: String,
    pub labels: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Store snapshots keyed by mount name (e.g. "system", "model", "gate").
    /// Values are the snapshot state for each store (serde_json::Value).
    #[serde(flatten)]
    pub stores: BTreeMap<String, serde_json::Value>,
}

/// A range of sequence numbers to include in the view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncludeRange {
    pub start: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
}

/// The view.json file — projection manifest defining what the agent sees.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewFile {
    pub parent: Option<String>,
    pub include: Vec<IncludeRange>,
    pub masks: Vec<u64>,
    pub replacements: BTreeMap<String, serde_json::Value>,
}

/// Write context.json to a thread directory.
pub fn write_context(dir: &Path, ctx: &ContextFile) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(ctx).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("context.json"), json).map_err(|e| e.to_string())
}

/// Read context.json from a thread directory. Returns None if file doesn't exist.
pub fn read_context(dir: &Path) -> Result<Option<ContextFile>, String> {
    let path = dir.join("context.json");
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let ctx: ContextFile = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    Ok(Some(ctx))
}

/// Write a default view.json (include all, no masks, no replacements).
pub fn write_default_view(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let view = ViewFile {
        parent: None,
        include: vec![IncludeRange {
            start: 0,
            end: None,
        }],
        masks: vec![],
        replacements: BTreeMap::new(),
    };
    let json = serde_json::to_string_pretty(&view).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("view.json"), json).map_err(|e| e.to_string())
}

/// Read view.json from a thread directory. Returns None if file doesn't exist.
pub fn read_view(dir: &Path) -> Result<Option<ViewFile>, String> {
    let path = dir.join("view.json");
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let view: ViewFile = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    Ok(Some(view))
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ox-inbox thread_dir -- --nocapture`
Expected: all 4 tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/ox-inbox/src/thread_dir.rs crates/ox-inbox/src/lib.rs
git commit -m 'feat(ox-inbox): thread directory format (context.json + view.json)'
```

---

### Task 3: Snapshot Coordinator

**Files:**
- Create: `crates/ox-inbox/src/snapshot.rs`
- Modify: `crates/ox-inbox/src/lib.rs`
- Modify: `crates/ox-inbox/Cargo.toml` (add ox-context, ox-kernel dev-deps for tests)

The coordinator reads `{mount}/snapshot/state` from each participating mount in the Namespace and assembles a `ContextFile`. On restore, it writes each state back via `{mount}/snapshot/state` and replays ledger entries through `history/append`.

- [ ] **Step 1: Write the failing tests**

Create `crates/ox-inbox/src/snapshot.rs`:

```rust
//! Snapshot coordinator — saves/restores namespace state to/from thread directories.

#[cfg(test)]
mod tests {
    use super::*;
    use ox_context::{ModelProvider, Namespace, SystemProvider, ToolsProvider};
    use ox_gate::GateStore;
    use ox_history::HistoryProvider;
    use structfs_core_store::{Reader, Record, Writer, path};
    use structfs_serde_store::json_to_value;

    fn build_namespace() -> Namespace {
        let mut ns = Namespace::new();
        ns.mount("system", Box::new(SystemProvider::new("You are helpful.".to_string())));
        ns.mount("model", Box::new(ModelProvider::new("claude-sonnet-4-20250514".to_string(), 4096)));
        ns.mount("tools", Box::new(ToolsProvider::new(vec![])));
        ns.mount("history", Box::new(HistoryProvider::new()));
        ns.mount("gate", Box::new(GateStore::new()));
        ns
    }

    #[test]
    fn save_creates_context_and_view() {
        let dir = tempfile::tempdir().unwrap();
        let thread_dir = dir.path().join("t_test1");
        let mut ns = build_namespace();

        save(
            &mut ns,
            &thread_dir,
            "t_test1",
            "Test thread",
            &["backend".to_string()],
            1712345678,
            &PARTICIPATING_MOUNTS,
        ).unwrap();

        assert!(thread_dir.join("context.json").exists());
        assert!(thread_dir.join("view.json").exists());

        let ctx = crate::thread_dir::read_context(&thread_dir).unwrap().unwrap();
        assert_eq!(ctx.thread_id, "t_test1");
        assert_eq!(ctx.title, "Test thread");
        assert!(ctx.stores.contains_key("system"));
        assert!(ctx.stores.contains_key("model"));
        assert!(ctx.stores.contains_key("gate"));
        // tools should NOT be in stores (non-participant)
        assert!(!ctx.stores.contains_key("tools"));
        // history should NOT be in context.json (persisted in ledger)
        assert!(!ctx.stores.contains_key("history"));
    }

    #[test]
    fn save_and_restore_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let thread_dir = dir.path().join("t_test2");
        let mut ns = build_namespace();

        // Add a message to history
        let msg = serde_json::json!({"role": "user", "content": "hello"});
        ns.write(&path!("history/append"), Record::parsed(json_to_value(msg))).unwrap();

        save(
            &mut ns,
            &thread_dir,
            "t_test2",
            "Roundtrip test",
            &[],
            1712345678,
            &PARTICIPATING_MOUNTS,
        ).unwrap();

        // Build a fresh namespace and restore into it
        let mut ns2 = build_namespace();
        restore(&mut ns2, &thread_dir, &PARTICIPATING_MOUNTS).unwrap();

        // Verify system prompt restored
        let record = ns2.read(&path!("system")).unwrap().unwrap();
        match record.as_value().unwrap() {
            structfs_core_store::Value::String(s) => assert_eq!(s, "You are helpful."),
            _ => panic!("expected string"),
        }

        // Verify model restored
        let record = ns2.read(&path!("model/id")).unwrap().unwrap();
        match record.as_value().unwrap() {
            structfs_core_store::Value::String(s) => assert_eq!(s, "claude-sonnet-4-20250514"),
            _ => panic!("expected string"),
        }

        // Verify history restored (1 message)
        let record = ns2.read(&path!("history/count")).unwrap().unwrap();
        match record.as_value().unwrap() {
            structfs_core_store::Value::Integer(n) => assert_eq!(*n, 1),
            _ => panic!("expected integer"),
        }
    }

    #[test]
    fn save_appends_ledger_entries() {
        let dir = tempfile::tempdir().unwrap();
        let thread_dir = dir.path().join("t_test3");
        let mut ns = build_namespace();

        let msg1 = serde_json::json!({"role": "user", "content": "first"});
        ns.write(&path!("history/append"), Record::parsed(json_to_value(msg1))).unwrap();
        let msg2 = serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "reply"}]});
        ns.write(&path!("history/append"), Record::parsed(json_to_value(msg2))).unwrap();

        save(
            &mut ns,
            &thread_dir,
            "t_test3",
            "Ledger test",
            &[],
            1712345678,
            &PARTICIPATING_MOUNTS,
        ).unwrap();

        let entries = crate::ledger::read_ledger(&thread_dir.join("ledger.jsonl")).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].seq, 0);
        assert_eq!(entries[1].seq, 1);
        assert_eq!(entries[1].parent.as_deref(), Some(entries[0].hash.as_str()));
    }

    #[test]
    fn incremental_save_appends_new_messages_only() {
        let dir = tempfile::tempdir().unwrap();
        let thread_dir = dir.path().join("t_test4");
        let mut ns = build_namespace();

        // First save with 1 message
        let msg1 = serde_json::json!({"role": "user", "content": "first"});
        ns.write(&path!("history/append"), Record::parsed(json_to_value(msg1))).unwrap();
        save(
            &mut ns,
            &thread_dir,
            "t_test4",
            "Incremental test",
            &[],
            1712345678,
            &PARTICIPATING_MOUNTS,
        ).unwrap();

        let entries = crate::ledger::read_ledger(&thread_dir.join("ledger.jsonl")).unwrap();
        assert_eq!(entries.len(), 1);

        // Second save with 2 more messages
        let msg2 = serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "reply"}]});
        ns.write(&path!("history/append"), Record::parsed(json_to_value(msg2))).unwrap();
        let msg3 = serde_json::json!({"role": "user", "content": "follow-up"});
        ns.write(&path!("history/append"), Record::parsed(json_to_value(msg3))).unwrap();
        save(
            &mut ns,
            &thread_dir,
            "t_test4",
            "Incremental test",
            &[],
            1712345678,
            &PARTICIPATING_MOUNTS,
        ).unwrap();

        let entries = crate::ledger::read_ledger(&thread_dir.join("ledger.jsonl")).unwrap();
        assert_eq!(entries.len(), 3); // only 2 new appended
        assert_eq!(entries[2].parent.as_deref(), Some(entries[1].hash.as_str()));
    }
}
```

- [ ] **Step 2: Add dev-dependencies for testing**

Add to `crates/ox-inbox/Cargo.toml`:

```toml
[dev-dependencies]
ox-context = { path = "../ox-context" }
ox-gate = { path = "../ox-gate" }
ox-history = { path = "../ox-history" }
```

Wire up in `crates/ox-inbox/src/lib.rs`:

```rust
pub mod snapshot;
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ox-inbox snapshot -- --nocapture`
Expected: compilation errors

- [ ] **Step 4: Implement the snapshot coordinator**

Add implementation above the `#[cfg(test)]` block in `crates/ox-inbox/src/snapshot.rs`:

```rust
//! Snapshot coordinator — saves/restores namespace state to/from thread directories.

use crate::ledger;
use crate::thread_dir::{self, ContextFile};
use std::collections::BTreeMap;
use std::path::Path;
use structfs_core_store::{Reader, Record, Writer};
use structfs_serde_store::{json_to_value, value_to_json};

/// Mounts that participate in context.json snapshots.
/// History is excluded — it lives in the ledger, not context.json.
pub const PARTICIPATING_MOUNTS: [&str; 3] = ["system", "model", "gate"];

/// Save namespace state to a thread directory.
///
/// - Reads `{mount}/snapshot/state` for each participating mount → writes context.json
/// - Reads `history/messages` → appends new messages to ledger.jsonl (incremental)
/// - Creates view.json if it doesn't exist
pub fn save(
    namespace: &mut dyn structfs_core_store::Store,
    thread_dir: &Path,
    thread_id: &str,
    title: &str,
    labels: &[String],
    updated_at: i64,
    mounts: &[&str],
) -> Result<SaveResult, String> {
    std::fs::create_dir_all(thread_dir).map_err(|e| e.to_string())?;

    // 1. Read snapshot states from participating mounts
    let mut stores = BTreeMap::new();
    for &mount in mounts {
        let path = structfs_core_store::Path::parse(&format!("{mount}/snapshot/state"))
            .map_err(|e| e.to_string())?;
        if let Ok(Some(record)) = namespace.read(&path) {
            if let Some(value) = record.as_value() {
                stores.insert(mount.to_string(), value_to_json(value.clone()));
            }
        }
    }

    // 2. Read existing context for created_at, or use updated_at for new threads
    let created_at = thread_dir::read_context(thread_dir)
        .ok()
        .flatten()
        .map(|c| c.created_at)
        .unwrap_or(updated_at);

    // 3. Write context.json
    let ctx = ContextFile {
        version: 1,
        thread_id: thread_id.to_string(),
        title: title.to_string(),
        labels: labels.to_vec(),
        created_at,
        updated_at,
        stores,
    };
    thread_dir::write_context(thread_dir, &ctx)?;

    // 4. Write default view.json if it doesn't exist
    if !thread_dir.join("view.json").exists() {
        thread_dir::write_default_view(thread_dir)?;
    }

    // 5. Append new messages to ledger (incremental)
    let ledger_path = thread_dir.join("ledger.jsonl");
    let last_entry = ledger::read_last_entry(&ledger_path)?;
    let existing_count = last_entry.as_ref().map_or(0, |e| e.seq + 1);

    // Read all messages from history
    let messages_path = structfs_core_store::Path::parse("history/messages")
        .map_err(|e| e.to_string())?;
    let messages = match namespace.read(&messages_path) {
        Ok(Some(record)) => {
            let json = value_to_json(record.as_value().ok_or("expected value")?.clone());
            json.as_array().cloned().unwrap_or_default()
        }
        _ => Vec::new(),
    };

    // Append only messages beyond what's already in the ledger
    let mut prev = last_entry;
    let mut last_seq: i64 = prev.as_ref().map_or(-1, |e| e.seq as i64);
    let mut last_hash: Option<String> = prev.as_ref().map(|e| e.hash.clone());

    for msg in messages.iter().skip(existing_count as usize) {
        let entry = ledger::append_entry(&ledger_path, msg, prev.as_ref())?;
        last_seq = entry.seq as i64;
        last_hash = Some(entry.hash.clone());
        prev = Some(entry);
    }

    Ok(SaveResult {
        last_seq,
        last_hash,
    })
}

/// Result from a save operation — used to update SQLite cache.
#[derive(Debug, Clone)]
pub struct SaveResult {
    pub last_seq: i64,
    pub last_hash: Option<String>,
}

/// Restore namespace state from a thread directory.
///
/// - Reads context.json → writes `{mount}/snapshot/state` for each store
/// - Reads ledger.jsonl → replays all messages through `history/append`
pub fn restore(
    namespace: &mut dyn structfs_core_store::Store,
    thread_dir: &Path,
    mounts: &[&str],
) -> Result<(), String> {
    // 1. Restore context (non-history stores)
    let ctx = thread_dir::read_context(thread_dir)?
        .ok_or("no context.json in thread directory")?;

    for &mount in mounts {
        if let Some(state_json) = ctx.stores.get(mount) {
            let path = structfs_core_store::Path::parse(&format!("{mount}/snapshot/state"))
                .map_err(|e| e.to_string())?;
            let value = json_to_value(state_json.clone());
            namespace
                .write(&path, Record::parsed(value))
                .map_err(|e| e.to_string())?;
        }
    }

    // 2. Replay ledger through history/append
    let ledger_path = thread_dir.join("ledger.jsonl");
    let entries = ledger::read_ledger(&ledger_path)?;
    let history_path = structfs_core_store::Path::parse("history/append")
        .map_err(|e| e.to_string())?;

    for entry in &entries {
        let value = json_to_value(entry.msg.clone());
        namespace
            .write(&history_path, Record::parsed(value))
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ox-inbox snapshot -- --nocapture`
Expected: all 4 tests pass

- [ ] **Step 6: Run full workspace tests**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 7: Commit**

```bash
git add crates/ox-inbox/Cargo.toml crates/ox-inbox/src/snapshot.rs crates/ox-inbox/src/lib.rs
git commit -m 'feat(ox-inbox): snapshot coordinator for save/restore via namespace'
```

---

### Task 4: SQLite Migration — Add last_seq and last_hash

**Files:**
- Modify: `crates/ox-inbox/src/schema.rs`
- Modify: `crates/ox-inbox/src/model.rs`
- Modify: `crates/ox-inbox/src/reader.rs`
- Modify: `crates/ox-inbox/src/writer.rs`

- [ ] **Step 1: Write the failing test**

Add to the existing test module in `crates/ox-inbox/src/lib.rs`:

```rust
    #[test]
    fn schema_has_last_seq_and_last_hash_columns() {
        let (store, _dir) = test_store();
        let db = store.db.lock().unwrap();
        // Verify columns exist by querying them
        let (last_seq, last_hash): (i64, Option<String>) = db
            .query_row(
                "SELECT last_seq, last_hash FROM threads LIMIT 0",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or((-1, None)); // No rows is fine, just checking columns exist
        // The default for new schema is -1 and NULL
        assert_eq!(last_seq, -1);
        assert!(last_hash.is_none());
    }
```

Actually, `LIMIT 0` returns no rows. Let's create a thread first:

```rust
    #[test]
    fn new_thread_has_default_last_seq_and_last_hash() {
        let (mut store, _dir) = test_store();
        let mut map = std::collections::BTreeMap::new();
        map.insert("title".to_string(), Value::String("Test".to_string()));
        let path = store
            .write(&structfs_core_store::path!("threads"), Record::parsed(Value::Map(map)))
            .unwrap();
        let id = path.iter().nth(1).unwrap().clone();

        let db = store.db.lock().unwrap();
        let (last_seq, last_hash): (i64, Option<String>) = db
            .query_row(
                "SELECT last_seq, last_hash FROM threads WHERE id = ?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(last_seq, -1);
        assert!(last_hash.is_none());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-inbox new_thread_has_default -- --nocapture`
Expected: FAIL — column `last_seq` does not exist

- [ ] **Step 3: Add columns to schema**

In `crates/ox-inbox/src/schema.rs`, add the two columns to the `CREATE TABLE threads` statement:

```sql
            token_count   INTEGER NOT NULL DEFAULT 0,
            last_seq      INTEGER NOT NULL DEFAULT -1,
            last_hash     TEXT
```

Also add migration for existing databases (after the CREATE TABLE statements, before the CREATE INDEX statements):

```sql
        -- Migration: add last_seq and last_hash if missing
        -- (safe to run on new DBs — columns already exist from CREATE TABLE)
```

Actually, `CREATE TABLE IF NOT EXISTS` won't add columns to an existing table. We need ALTER TABLE for migration. Add after all CREATE TABLE/INDEX statements:

```rust
    // Migrate: add columns if missing (for databases created before this version)
    let has_last_seq: bool = conn
        .prepare("SELECT last_seq FROM threads LIMIT 0")
        .is_ok();
    if !has_last_seq {
        conn.execute_batch(
            "ALTER TABLE threads ADD COLUMN last_seq INTEGER NOT NULL DEFAULT -1;
             ALTER TABLE threads ADD COLUMN last_hash TEXT;",
        )?;
    }
```

Full updated `schema.rs`:

```rust
use rusqlite::Connection;

pub fn initialize(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS threads (
            id            TEXT PRIMARY KEY,
            title         TEXT NOT NULL,
            parent_id     TEXT REFERENCES threads(id),
            inbox_state   TEXT NOT NULL DEFAULT 'inbox',
            thread_state  TEXT NOT NULL DEFAULT 'running',
            block_reason  TEXT,
            created_at    INTEGER NOT NULL,
            updated_at    INTEGER NOT NULL,
            token_count   INTEGER NOT NULL DEFAULT 0,
            last_seq      INTEGER NOT NULL DEFAULT -1,
            last_hash     TEXT
        );

        CREATE TABLE IF NOT EXISTS labels (
            thread_id     TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
            label         TEXT NOT NULL,
            PRIMARY KEY (thread_id, label)
        );

        CREATE TABLE IF NOT EXISTS tasks (
            id            TEXT PRIMARY KEY,
            thread_id     TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
            title         TEXT NOT NULL,
            status        TEXT NOT NULL DEFAULT 'pending',
            created_at    INTEGER NOT NULL,
            updated_at    INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_threads_inbox_state ON threads(inbox_state);
        CREATE INDEX IF NOT EXISTS idx_threads_thread_state ON threads(thread_state);
        CREATE INDEX IF NOT EXISTS idx_threads_updated_at ON threads(updated_at);
        CREATE INDEX IF NOT EXISTS idx_threads_parent_id ON threads(parent_id);
        CREATE INDEX IF NOT EXISTS idx_labels_label ON labels(label);
        CREATE INDEX IF NOT EXISTS idx_tasks_thread_id ON tasks(thread_id);
        ",
    )?;

    // Migrate: add columns if missing (for databases created before this version)
    let has_last_seq: bool = conn
        .prepare("SELECT last_seq FROM threads LIMIT 0")
        .is_ok();
    if !has_last_seq {
        conn.execute_batch(
            "ALTER TABLE threads ADD COLUMN last_seq INTEGER NOT NULL DEFAULT -1;
             ALTER TABLE threads ADD COLUMN last_hash TEXT;",
        )?;
    }

    Ok(())
}
```

- [ ] **Step 4: Add fields to ThreadMetadata**

In `crates/ox-inbox/src/model.rs`, add to the `ThreadMetadata` struct:

```rust
    pub last_seq: i64,
    pub last_hash: Option<String>,
```

Add to `to_value()`:

```rust
        map.insert("last_seq".to_string(), Value::Integer(self.last_seq));
        if let Some(ref h) = self.last_hash {
            map.insert("last_hash".to_string(), Value::String(h.clone()));
        }
```

Update tests in model.rs to include the new fields in test fixtures.

- [ ] **Step 5: Update reader.rs to read the new columns**

Find where `ThreadMetadata` is constructed from SQL rows in `reader.rs` and add the two new columns to the SELECT and row extraction. The fields default to `-1` and `None`.

- [ ] **Step 6: Update writer.rs to accept last_seq/last_hash updates**

In the thread update path of `writer.rs`, handle `last_seq` and `last_hash` fields in the update map, adding them to the UPDATE SET clause.

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p ox-inbox -- --nocapture`
Expected: all existing tests + new test pass

- [ ] **Step 8: Commit**

```bash
git add crates/ox-inbox/src/schema.rs crates/ox-inbox/src/model.rs crates/ox-inbox/src/reader.rs crates/ox-inbox/src/writer.rs crates/ox-inbox/src/lib.rs
git commit -m 'feat(ox-inbox): add last_seq/last_hash columns to SQLite schema'
```

---

### Task 5: ox-cli Integration — Replace save_history with Snapshot Coordinator

**Files:**
- Modify: `crates/ox-cli/src/agents.rs`

This task replaces the current `save_history()` function (which overwrites a raw JSONL file) with the snapshot coordinator's `save()` function, and replaces the manual JSONL restore with the coordinator's `restore()` function.

- [ ] **Step 1: Update the restore path in agent_worker**

In `crates/ox-cli/src/agents.rs`, replace the JSONL restore block (lines 222-242) with:

```rust
    // Restore conversation state from thread directory if it exists
    let thread_dir = inbox_root.join("threads").join(&thread_id);
    if thread_dir.join("context.json").exists() {
        // New format: restore from snapshot
        ox_inbox::snapshot::restore(
            &mut namespace,
            &thread_dir,
            &ox_inbox::snapshot::PARTICIPATING_MOUNTS,
        )
        .ok();
    } else {
        // Legacy format: restore from raw JSONL
        let jsonl_path = thread_dir.join(format!("{thread_id}.jsonl"));
        if jsonl_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&jsonl_path) {
                for line in content.lines() {
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                        namespace
                            .write(
                                &path!("history/append"),
                                Record::parsed(json_to_value(json)),
                            )
                            .ok();
                    }
                }
            }
        }
    }
```

- [ ] **Step 2: Replace save_history with snapshot save**

Replace the `save_history()` function (lines 310-340) with:

```rust
/// Save the conversation state from the namespace to the thread directory.
fn save_thread_state(
    namespace: &mut Namespace,
    inbox_root: &std::path::Path,
    thread_id: &str,
    title: &str,
) {
    let thread_dir = inbox_root.join("threads").join(thread_id);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    ox_inbox::snapshot::save(
        namespace,
        &thread_dir,
        thread_id,
        title,
        &[],
        now,
        &ox_inbox::snapshot::PARTICIPATING_MOUNTS,
    )
    .ok();
}
```

- [ ] **Step 3: Update the call site**

Replace the `save_history(&mut namespace, &inbox_root, &thread_id);` call (line 287) with:

```rust
        save_thread_state(&mut namespace, &inbox_root, &thread_id, "Thread");
```

Note: The title is "Thread" as a default since we don't have the title in the worker scope. This can be improved later when the worker receives the title from the pool.

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p ox-cli`
Expected: clean build

- [ ] **Step 5: Run full workspace tests**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/agents.rs
git commit -m 'feat(ox-cli): use snapshot coordinator for save/restore'
```

---

### Task 6: Integration Test — Full Save/Restore Lifecycle

**Files:**
- Modify: `crates/ox-inbox/src/snapshot.rs` (add integration test)

- [ ] **Step 1: Write a full lifecycle test**

Add to the test module in `crates/ox-inbox/src/snapshot.rs`:

```rust
    #[test]
    fn full_lifecycle_save_mutate_restore() {
        let dir = tempfile::tempdir().unwrap();
        let thread_dir = dir.path().join("t_lifecycle");
        let mut ns = build_namespace();

        // Build initial state
        let msg1 = serde_json::json!({"role": "user", "content": "hello"});
        ns.write(&path!("history/append"), Record::parsed(json_to_value(msg1))).unwrap();
        let msg2 = serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "hi there"}]});
        ns.write(&path!("history/append"), Record::parsed(json_to_value(msg2))).unwrap();

        // Save
        let result = save(
            &mut ns,
            &thread_dir,
            "t_lifecycle",
            "Lifecycle test",
            &["test".to_string()],
            1712345678,
            &PARTICIPATING_MOUNTS,
        ).unwrap();
        assert_eq!(result.last_seq, 1);
        assert!(result.last_hash.is_some());

        // Verify thread directory structure
        assert!(thread_dir.join("context.json").exists());
        assert!(thread_dir.join("ledger.jsonl").exists());
        assert!(thread_dir.join("view.json").exists());

        // Restore into fresh namespace
        let mut ns2 = build_namespace();
        restore(&mut ns2, &thread_dir, &PARTICIPATING_MOUNTS).unwrap();

        // Verify all state restored
        let record = ns2.read(&path!("system")).unwrap().unwrap();
        match record.as_value().unwrap() {
            structfs_core_store::Value::String(s) => assert_eq!(s, "You are helpful."),
            _ => panic!("expected string"),
        }

        let record = ns2.read(&path!("history/count")).unwrap().unwrap();
        match record.as_value().unwrap() {
            structfs_core_store::Value::Integer(n) => assert_eq!(*n, 2),
            _ => panic!("expected integer"),
        }

        // Verify ledger has proper hash chain
        let entries = crate::ledger::read_ledger(&thread_dir.join("ledger.jsonl")).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].parent.is_none());
        assert_eq!(entries[1].parent.as_deref(), Some(entries[0].hash.as_str()));
    }

    #[test]
    fn context_json_excludes_api_keys() {
        let dir = tempfile::tempdir().unwrap();
        let thread_dir = dir.path().join("t_keys");
        let mut ns = build_namespace();

        // Set an API key
        ns.write(
            &path!("gate/accounts/anthropic/key"),
            Record::parsed(structfs_core_store::Value::String("sk-secret".to_string())),
        ).unwrap();

        save(
            &mut ns,
            &thread_dir,
            "t_keys",
            "Keys test",
            &[],
            1712345678,
            &PARTICIPATING_MOUNTS,
        ).unwrap();

        // Read context.json and verify no API keys
        let content = std::fs::read_to_string(thread_dir.join("context.json")).unwrap();
        assert!(!content.contains("sk-secret"), "API key must not appear in context.json");
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p ox-inbox snapshot -- --nocapture`
Expected: all 6 snapshot tests pass

- [ ] **Step 3: Run full quality gates**

Run: `./scripts/quality_gates.sh`
Expected: all gates pass

- [ ] **Step 4: Commit**

```bash
git add crates/ox-inbox/src/snapshot.rs
git commit -m 'test(ox-inbox): full lifecycle save/restore + API key exclusion tests'
```

---

## Summary

| Task | What | Files | Tests |
|------|------|-------|-------|
| 1 | Content-addressed ledger with hash chain | ledger.rs (new) | 7 |
| 2 | Thread directory I/O (context.json, view.json) | thread_dir.rs (new) | 4 |
| 3 | Snapshot coordinator (save/restore via namespace) | snapshot.rs (new) | 4 |
| 4 | SQLite migration (last_seq, last_hash) | schema.rs, model.rs, reader.rs, writer.rs | 1+ |
| 5 | ox-cli integration (replace save_history) | agents.rs | compile check |
| 6 | Integration tests + quality gates | snapshot.rs | 2 |

**Total: ~18 new tests across 6 commits.**

### What's deferred to future work:

- **Startup reconciliation** — hash-based consistency check between SQLite and ledger files
- **Write-through** — updating SQLite last_seq/last_hash automatically on each message append
- **deltas.jsonl** — context change audit log
- **View projection engine** — evaluating masks, replacements, includes
- **Legacy migration** — converting old `{thread_id}.jsonl` files to ledger format on first access

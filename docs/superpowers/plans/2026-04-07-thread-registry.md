# ThreadRegistry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace ad-hoc `thread_mount.rs` functions with a ThreadRegistry — an AsyncStore mounted at `threads/` that owns per-thread store lifecycle, routes internally, and lazy-mounts from disk on first access.

**Architecture:** ThreadRegistry implements AsyncReader + AsyncWriter, holds a HashMap of ThreadNamespaces. Each ThreadNamespace holds the 6 per-thread stores (SystemProvider, HistoryProvider, ModelProvider, ToolsProvider, GateStore, ApprovalStore). The ThreadRegistry routes `{id}/{store}/{path}` internally — sync stores wrapped in `ready()`, approval store returns its async future directly. Lazy mount from thread directory on first access. From the broker's perspective, `threads/` is one opaque mount.

**Tech Stack:** Rust, ox-broker (AsyncReader/AsyncWriter/mount_async), ox-context, ox-history, ox-gate, ox-ui (ApprovalStore), ox-inbox (snapshot)

**Spec:** `docs/superpowers/specs/2026-04-07-thread-registry-design.md`

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/ox-context/src/lib.rs` | Modify | ToolsProvider accepts writes to "schemas" |
| `crates/ox-cli/src/thread_registry.rs` | Create | ThreadRegistry (AsyncStore) + ThreadNamespace (sync routing + approval), lazy mount |
| `crates/ox-cli/src/broker_setup.rs` | Modify | Mount ThreadRegistry at `threads/`, remove global ApprovalStore |
| `crates/ox-cli/src/agents.rs` | Modify | Worker skips mounting, writes tool schemas via adapter |
| `crates/ox-cli/src/thread_mount.rs` | Delete | Replaced by ThreadRegistry |
| `crates/ox-cli/src/main.rs` | Modify | Replace thread_mount module with thread_registry |

## Key Design: Sync + Async Routing

ThreadNamespace implements sync `Reader + Writer` for its 5 sync stores (system, history, model, tools, gate). This lets `ox_inbox::snapshot::save/restore` work directly (they take `&mut dyn Store`).

The ApprovalStore is async — its `write("request")` returns a deferred future. ThreadNamespace holds it separately and does NOT route to it through sync Reader/Writer.

The ThreadRegistry's `AsyncReader`/`AsyncWriter` impl checks the path:
- `{id}/approval/{sub}` → delegates to `ns.approval.read/write` (returns BoxFuture)
- `{id}/{store}/{sub}` → delegates to `ns.read/write` (sync, wrapped in `Box::pin(ready(...))`)

---

### Task 1: Make ToolsProvider writable

ToolsProvider currently rejects all writes. The worker needs to write tool schemas after lazy mount (schemas aren't in the snapshot — they depend on runtime tool registration).

**Files:**
- Modify: `crates/ox-context/src/lib.rs`

- [ ] **Step 1: Write test**

Add to the tests module in `crates/ox-context/src/lib.rs`:

```rust
#[test]
fn tools_provider_accepts_schema_write() {
    let mut tp = ToolsProvider::new(vec![]);

    // Initially empty
    let record = tp.read(&path!("schemas")).unwrap().unwrap();
    match unwrap_value(record) {
        Value::Array(a) => assert!(a.is_empty()),
        _ => panic!("expected array"),
    }

    // Write schemas
    let schemas_json = serde_json::json!([
        {"name": "test_tool", "description": "A test", "input_schema": {"type": "object"}}
    ]);
    let schemas_value = structfs_serde_store::json_to_value(schemas_json);
    tp.write(&path!("schemas"), Record::parsed(schemas_value)).unwrap();

    // Read back
    let record = tp.read(&path!("schemas")).unwrap().unwrap();
    match unwrap_value(record) {
        Value::Array(a) => assert_eq!(a.len(), 1),
        _ => panic!("expected array"),
    }
}
```

- [ ] **Step 2: Run test — verify fails**

Run: `cargo test -p ox-context tools_provider_accepts`
Expected: FAIL — "tools store is read-only"

- [ ] **Step 3: Implement write handler**

Replace the Writer impl for ToolsProvider (around line 355):

```rust
impl Writer for ToolsProvider {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let key = if to.is_empty() {
            ""
        } else {
            to.components[0].as_str()
        };
        match key {
            "" | "schemas" => {
                let value = match data {
                    Record::Parsed(v) => v,
                    _ => {
                        return Err(StoreError::store(
                            "tools",
                            "write",
                            "expected parsed record",
                        ));
                    }
                };
                let schemas: Vec<ox_kernel::ToolSchema> =
                    structfs_serde_store::from_value(value)
                        .map_err(|e| StoreError::store("tools", "write", e.to_string()))?;
                self.schemas = schemas;
                Ok(to.clone())
            }
            _ => Err(StoreError::store(
                "tools",
                "write",
                format!("unknown write path: {to}"),
            )),
        }
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-context`
Expected: All pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-context/src/lib.rs
git commit -m "feat(ox-context): make ToolsProvider writable for schema updates"
```

---

### Task 2: ThreadRegistry + ThreadNamespace

The core implementation. Independently testable — doesn't touch any other files yet.

**Files:**
- Create: `crates/ox-cli/src/thread_registry.rs`
- Modify: `crates/ox-cli/src/main.rs` (add `pub(crate) mod thread_registry;`)

- [ ] **Step 1: Create thread_registry.rs**

```rust
//! ThreadRegistry — lazy-mounting AsyncStore for per-thread namespaces.
//!
//! Mounted at `threads/` in the broker. Routes `{id}/{store}/{path}` internally.
//! Lazy-mounts thread stores from disk on first access. From the broker's
//! perspective, `threads/` is one opaque mount.

use std::collections::HashMap;
use std::path::PathBuf;

use ox_broker::async_store::{AsyncReader, AsyncWriter, BoxFuture};
use ox_context::{ModelProvider, SystemProvider, ToolsProvider};
use ox_gate::GateStore;
use ox_history::HistoryProvider;
use ox_ui::ApprovalStore;
use structfs_core_store::{
    Error as StoreError, Path, Reader, Record, Store, Value, Writer, path,
};

// ---------------------------------------------------------------------------
// ThreadNamespace — per-thread store collection with sync routing
// ---------------------------------------------------------------------------

/// A per-thread store collection. Routes reads/writes by first path component.
///
/// Implements sync `Reader + Writer` for the 5 sync stores (system, history,
/// model, tools, gate). The ApprovalStore is async and handled separately
/// by the ThreadRegistry.
pub struct ThreadNamespace {
    system: SystemProvider,
    history: HistoryProvider,
    model: ModelProvider,
    tools: ToolsProvider,
    gate: GateStore,
    pub approval: ApprovalStore,
}

impl ThreadNamespace {
    /// Create a fresh namespace with default stores.
    pub fn new_default() -> Self {
        Self {
            system: SystemProvider::new(crate::agents::SYSTEM_PROMPT.to_string()),
            history: HistoryProvider::new(),
            model: ModelProvider::new("claude-sonnet-4-20250514".to_string(), 4096),
            tools: ToolsProvider::new(vec![]),
            gate: GateStore::new(),
            approval: ApprovalStore::new(),
        }
    }

    /// Create a namespace and restore from a thread directory on disk.
    pub fn from_thread_dir(thread_dir: &std::path::Path) -> Result<Self, String> {
        let mut ns = Self::new_default();

        if thread_dir.join("context.json").exists() {
            // Restore from snapshot (context.json + ledger.jsonl)
            ox_inbox::snapshot::restore(
                &mut ns,
                thread_dir,
                &ox_inbox::snapshot::PARTICIPATING_MOUNTS,
            )?;
        } else {
            // Legacy: try raw JSONL
            let thread_id = thread_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            let jsonl_path = thread_dir.join(format!("{thread_id}.jsonl"));
            if jsonl_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&jsonl_path) {
                    for line in content.lines() {
                        if line.is_empty() {
                            continue;
                        }
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                            ns.write(
                                &path!("history/append"),
                                Record::parsed(structfs_serde_store::json_to_value(json)),
                            )
                            .ok();
                        }
                    }
                }
            }
        }

        Ok(ns)
    }

    /// Route a path to the appropriate sync store.
    fn route(&mut self, path: &Path) -> Option<(&mut dyn Store, Path)> {
        if path.is_empty() {
            return None;
        }
        let store_name = path.components[0].as_str();
        let sub = Path::from_components(path.components[1..].to_vec());
        let store: &mut dyn Store = match store_name {
            "system" => &mut self.system,
            "history" => &mut self.history,
            "model" => &mut self.model,
            "tools" => &mut self.tools,
            "gate" => &mut self.gate,
            _ => return None,
        };
        Some((store, sub))
    }
}

impl Reader for ThreadNamespace {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        if let Some((store, sub)) = self.route(from) {
            store.read(&sub)
        } else {
            Ok(None)
        }
    }
}

impl Writer for ThreadNamespace {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        if let Some((store, sub)) = self.route(to) {
            store.write(&sub, data)
        } else {
            Err(StoreError::NoRoute { path: to.clone() })
        }
    }
}

// ---------------------------------------------------------------------------
// ThreadRegistry — the AsyncStore mounted at "threads/"
// ---------------------------------------------------------------------------

/// Lazy-mounting async store for per-thread namespaces.
///
/// Routes `{thread_id}/{store}/{path}` to the appropriate ThreadNamespace.
/// Mounts from disk on first access. Holds all thread state.
pub struct ThreadRegistry {
    threads: HashMap<String, ThreadNamespace>,
    inbox_root: PathBuf,
}

impl ThreadRegistry {
    pub fn new(inbox_root: PathBuf) -> Self {
        Self {
            threads: HashMap::new(),
            inbox_root,
        }
    }

    /// Ensure a thread's stores are mounted. Lazy-loads from disk if needed.
    fn ensure_mounted(&mut self, thread_id: &str) -> &mut ThreadNamespace {
        if !self.threads.contains_key(thread_id) {
            let thread_dir = self.inbox_root.join("threads").join(thread_id);
            let ns = if thread_dir.exists() {
                ThreadNamespace::from_thread_dir(&thread_dir)
                    .unwrap_or_else(|_| ThreadNamespace::new_default())
            } else {
                ThreadNamespace::new_default()
            };
            self.threads.insert(thread_id.to_string(), ns);
        }
        self.threads.get_mut(thread_id).unwrap()
    }

    /// Split a path into thread_id and sub-path.
    /// Input: "t_abc/history/messages" → ("t_abc", "history/messages")
    fn split_thread_path(path: &Path) -> Option<(String, Path)> {
        if path.is_empty() {
            return None;
        }
        let thread_id = path.components[0].clone();
        let sub = Path::from_components(path.components[1..].to_vec());
        Some((thread_id, sub))
    }

    /// Check if a sub-path routes to the approval store.
    fn is_approval_path(sub: &Path) -> Option<Path> {
        if !sub.is_empty() && sub.components[0] == "approval" {
            Some(Path::from_components(sub.components[1..].to_vec()))
        } else {
            None
        }
    }

    /// Unmount a thread, saving state to disk first.
    #[allow(dead_code)]
    pub fn unmount(&mut self, thread_id: &str) {
        if let Some(mut ns) = self.threads.remove(thread_id) {
            // Save to disk before dropping
            let thread_dir = self.inbox_root.join("threads").join(thread_id);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            ox_inbox::snapshot::save(
                &mut ns,
                &thread_dir,
                thread_id,
                "", // title — read from context if needed
                &[],
                now,
                &ox_inbox::snapshot::PARTICIPATING_MOUNTS,
            )
            .ok();
        }
    }
}

impl AsyncReader for ThreadRegistry {
    fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let Some((thread_id, sub)) = Self::split_thread_path(from) else {
            return Box::pin(std::future::ready(Ok(None)));
        };

        let ns = self.ensure_mounted(&thread_id);

        // Approval store — async read (always resolves immediately, but trait is async)
        if let Some(approval_sub) = Self::is_approval_path(&sub) {
            return ns.approval.read(&approval_sub);
        }

        // Sync stores — wrap in ready()
        let result = ns.read(&sub);
        Box::pin(std::future::ready(result))
    }
}

impl AsyncWriter for ThreadRegistry {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
        let Some((thread_id, sub)) = Self::split_thread_path(to) else {
            return Box::pin(std::future::ready(Err(StoreError::NoRoute {
                path: to.clone(),
            })));
        };

        let ns = self.ensure_mounted(&thread_id);

        // Approval store — async write (may defer for "request")
        if let Some(approval_sub) = Self::is_approval_path(&sub) {
            return ns.approval.write(&approval_sub, data);
        }

        // Sync stores — wrap in ready()
        let result = ns.write(&sub, data);
        Box::pin(std::future::ready(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::{Value, path};

    #[test]
    fn lazy_mount_from_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = ThreadRegistry::new(dir.path().to_path_buf());

        // Reading from a nonexistent thread creates a fresh namespace
        let fut = registry.read(&path!("t_new/system"));
        let result = futures_or_poll(fut);
        assert!(result.is_ok());
        let record = result.unwrap();
        assert!(record.is_some()); // Default system prompt
    }

    #[test]
    fn lazy_mount_from_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let inbox_root = dir.path().to_path_buf();

        // Create a thread directory with a snapshot
        {
            let mut ns = ThreadNamespace::new_default();
            // Write a user message
            ns.write(
                &path!("history/append"),
                Record::parsed(structfs_serde_store::json_to_value(
                    serde_json::json!({"role": "user", "content": "hello from snapshot"}),
                )),
            )
            .unwrap();

            // Save snapshot
            let thread_dir = inbox_root.join("threads").join("t_snap");
            ox_inbox::snapshot::save(
                &mut ns,
                &thread_dir,
                "t_snap",
                "Snapshot test",
                &[],
                1234567890,
                &ox_inbox::snapshot::PARTICIPATING_MOUNTS,
            )
            .unwrap();
        }

        // Now lazy-mount via registry
        let mut registry = ThreadRegistry::new(inbox_root);
        let fut = registry.read(&path!("t_snap/history/count"));
        let result = futures_or_poll(fut);
        let record = result.unwrap().unwrap();
        assert_eq!(record.as_value().unwrap(), &Value::Integer(1));
    }

    #[test]
    fn routes_to_correct_store() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = ThreadRegistry::new(dir.path().to_path_buf());

        // Write to history through registry
        let msg = serde_json::json!({"role": "user", "content": "test"});
        let fut = registry.write(
            &path!("t_route/history/append"),
            Record::parsed(structfs_serde_store::json_to_value(msg)),
        );
        let result = futures_or_poll(fut);
        assert!(result.is_ok());

        // Read back
        let fut = registry.read(&path!("t_route/history/count"));
        let result = futures_or_poll(fut);
        assert_eq!(
            result.unwrap().unwrap().as_value().unwrap(),
            &Value::Integer(1),
        );

        // Read model
        let fut = registry.read(&path!("t_route/model/id"));
        let result = futures_or_poll(fut);
        assert!(result.unwrap().is_some());
    }

    #[test]
    fn multiple_threads_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = ThreadRegistry::new(dir.path().to_path_buf());

        // Write to thread A
        let msg_a = serde_json::json!({"role": "user", "content": "thread A"});
        let fut = registry.write(
            &path!("t_a/history/append"),
            Record::parsed(structfs_serde_store::json_to_value(msg_a)),
        );
        futures_or_poll(fut).unwrap();

        // Write to thread B
        let msg_b = serde_json::json!({"role": "user", "content": "thread B"});
        let fut = registry.write(
            &path!("t_b/history/append"),
            Record::parsed(structfs_serde_store::json_to_value(msg_b)),
        );
        futures_or_poll(fut).unwrap();

        // Each has 1 message
        let fut = registry.read(&path!("t_a/history/count"));
        assert_eq!(
            futures_or_poll(fut).unwrap().unwrap().as_value().unwrap(),
            &Value::Integer(1),
        );
        let fut = registry.read(&path!("t_b/history/count"));
        assert_eq!(
            futures_or_poll(fut).unwrap().unwrap().as_value().unwrap(),
            &Value::Integer(1),
        );
    }

    #[test]
    fn empty_path_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = ThreadRegistry::new(dir.path().to_path_buf());
        let fut = registry.read(&Path::from_components(vec![]));
        assert!(futures_or_poll(fut).unwrap().is_none());
    }

    /// Helper: poll a BoxFuture that should resolve immediately.
    fn futures_or_poll<T>(
        mut fut: std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>,
    ) -> T {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

        static VTABLE: RawWakerVTable = RawWakerVTable::new(
            |p| RawWaker::new(p, &VTABLE),
            |_| {},
            |_| {},
            |_| {},
        );
        let raw = RawWaker::new(std::ptr::null(), &VTABLE);
        let waker = unsafe { Waker::from_raw(raw) };
        let mut cx = Context::from_waker(&waker);
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(val) => val,
            Poll::Pending => panic!("future unexpectedly pending"),
        }
    }
}
```

- [ ] **Step 2: Register module**

In `crates/ox-cli/src/main.rs`, add:
```rust
pub(crate) mod thread_registry;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-cli thread_registry`
Expected: All 5 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-cli/src/thread_registry.rs crates/ox-cli/src/main.rs
git commit -m "feat(ox-cli): add ThreadRegistry with lazy mount from disk"
```

---

### Task 3: Wire ThreadRegistry + simplify agent_worker

Mount ThreadRegistry at `threads/` in broker_setup. Remove thread mounting from agent_worker. Worker writes tool schemas and API key via adapter. Delete thread_mount.rs.

These changes are coupled — they must be done together to compile.

**Files:**
- Modify: `crates/ox-cli/src/broker_setup.rs`
- Modify: `crates/ox-cli/src/agents.rs`
- Delete: `crates/ox-cli/src/thread_mount.rs`
- Modify: `crates/ox-cli/src/main.rs`

- [ ] **Step 1: Mount ThreadRegistry in broker_setup**

In `crates/ox-cli/src/broker_setup.rs`:

Add the inbox_root parameter to `setup()`:

```rust
pub async fn setup(inbox: InboxStore, bindings: Vec<Binding>, inbox_root: PathBuf) -> BrokerHandle {
```

Replace the global ApprovalStore mount with ThreadRegistry:

```rust
    // Mount ThreadRegistry at threads/ — lazy-mounts per-thread stores from disk
    servers.push(
        broker
            .mount_async(
                path!("threads"),
                crate::thread_registry::ThreadRegistry::new(inbox_root),
            )
            .await,
    );
```

Remove:
```rust
    // Mount ApprovalStore (per-app for now; per-thread in C3c)
    servers.push(
        broker
            .mount_async(path!("approval"), ApprovalStore::new())
            .await,
    );
```

Update the `setup()` tests to pass `inbox_root`.

- [ ] **Step 2: Simplify agent_worker**

In `crates/ox-cli/src/agents.rs`, the worker currently:
1. Builds ThreadConfig
2. Calls mount_thread (mounts stores)
3. Creates SyncClientAdapter
4. Calls restore_thread_state
5. Does legacy JSONL restore

Replace steps 1-4 with: just create the SyncClientAdapter. The ThreadRegistry lazy-mounts when the adapter first reads/writes.

Remove these blocks from agent_worker:
- The `ThreadConfig` construction (lines ~207-214)
- The `mount_thread` call (lines ~215-223)
- The `restore_thread_state` call (line ~260)
- The legacy JSONL restore block (lines ~262-282)
- The `unmount_thread` call at the end of the function

The worker still needs to:
- Write tool schemas via adapter: `adapter.write(&path!("tools/schemas"), schemas_value)`
- Write API key to gate: `adapter.write(&path!("gate/accounts/{provider}/key"), key_value)`
- Create the scoped adapter (unchanged)

New agent_worker setup after creating the adapter:

```rust
// Write tool schemas (ThreadRegistry created the stores with defaults;
// the worker provides the actual schemas from its ToolRegistry)
let schemas_value = structfs_serde_store::to_value(&tools.schemas())
    .map_err(|e| e.to_string());
if let Ok(val) = schemas_value {
    adapter.write(&path!("tools/schemas"), Record::parsed(val)).ok();
}

// Write API key to gate store
adapter.write(
    &ox_kernel::Path::from_components(vec![
        "gate".to_string(),
        "accounts".to_string(),
        provider.clone(),
        "key".to_string(),
    ]),
    Record::parsed(Value::String(api_key.clone())),
).ok();
```

Remove `use crate::thread_mount;` and all references to ThreadConfig, mount_thread, unmount_thread, restore_thread_state.

- [ ] **Step 3: Delete thread_mount.rs**

```bash
rm crates/ox-cli/src/thread_mount.rs
```

In `crates/ox-cli/src/main.rs`, remove:
```rust
pub(crate) mod thread_mount;
```

- [ ] **Step 4: Update main.rs to pass inbox_root to broker_setup**

In `crates/ox-cli/src/main.rs`, update the broker_setup call:

```rust
let broker_handle = rt.block_on(broker_setup::setup(broker_inbox, broker_bindings, inbox_root.clone()));
```

- [ ] **Step 5: Compile + test**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: Compiles. Tests pass (thread_mount tests are gone, thread_registry tests + broker_setup tests pass).

Note: broker_setup tests need updating to pass inbox_root. Use `tempfile::tempdir()` for the test inbox_root.

- [ ] **Step 6: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: 14/14 pass.

- [ ] **Step 7: Commit**

```bash
git add -u
git commit -m "feat(ox-cli): mount ThreadRegistry at threads/, delete thread_mount.rs"
```

---

### Task 4: Update status document

**Files:**
- Modify: `docs/design/rfc/structfs-tui-status.md`

- [ ] **Step 1: Add C7 section**

Add ThreadRegistry section under Phase C. Update "What's Next" to remove the thread lifecycle concern. Note that the thread history loading bug is fixed.

- [ ] **Step 2: Commit**

```bash
git add docs/design/rfc/structfs-tui-status.md
git commit -m "docs: update status/handoff for C7 ThreadRegistry completion"
```

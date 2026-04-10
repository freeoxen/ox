# Pico-Process Agent Execution Model

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Wasm agent a self-driving pico-process whose complete state is the StructFS namespace (context), with async tool execution via handles, crash recovery by reading context on startup, and portable serialization of the full context.

**Architecture:** The agent runs its own loop — the host never drives it. Tool calls are syscalls: the agent writes a request, the host executes it asynchronously and returns a handle. The agent writes a batch of handles to `tools/await`, reads the batch result. The namespace IS the context — system, history, tools, gate, in-flight handles — all serializable, resumable, portable. TurnStore as a stateful queue is removed; handles ARE the state.

**Tech Stack:** Rust (edition 2024), StructFS (structfs-core-store, structfs-serde-store), wasmtime 29, ox-kernel, ox-tools, ox-runtime, ox-wasm.

---

## Scope Check

This plan covers five subsystems that build on each other:

1. **Handle registry** — ToolStore returns execution handles on write, resolves them on read. No behavioral change to the agent yet (writes still block, but return handles).
2. **Async spawn** — Tool writes spawn execution asynchronously, return handles immediately. Reads block until the handle resolves.
3. **Batch await** — `tools/await` path accepts a set of handles, returns a composite handle. Reads block until all resolve.
4. **Agent loop rewrite** — Wasm agent uses handle pattern. TurnStore removed. Crash recovery via context reading on startup.
5. **Context serialization** — Full namespace snapshot/restore covers tool execution state. Portable context bundles.

Each subsystem produces working, testable software on its own. The agent works at each stage — the execution model progressively improves.

---

## File Structure

### New files

| File | Responsibility |
|------|---------------|
| `crates/ox-tools/src/handle.rs` | `ExecHandle` type (execution ID + status), `HandleRegistry` (maps IDs to results or pending JoinHandles) |

### Modified files

| File | Change |
|------|--------|
| `crates/ox-tools/src/lib.rs` | ToolStore gains HandleRegistry, write returns handle paths, read resolves handles. Remove TurnStore field and `turn/*` routing. |
| `crates/ox-tools/src/turn.rs` | Delete entirely — replaced by HandleRegistry |
| `crates/ox-tools/src/sandbox.rs` | Split `sandboxed_exec` into `sandboxed_spawn` (returns Child) + `sandboxed_await` (waits on Child) |
| `crates/ox-tools/src/fs.rs` | `FsModule::execute` returns immediately with a spawn handle |
| `crates/ox-tools/src/os.rs` | `OsModule::execute` returns immediately with a spawn handle |
| `crates/ox-wasm/src/lib.rs` | Agent loop uses handle pattern: fire writes → collect handles → write to `tools/await` → read batch result → write to history. Crash recovery on startup. |
| `crates/ox-runtime/src/host_store.rs` | No structural change — tools/* routing through HostEffects stays the same. Handles flow through naturally. |
| `crates/ox-inbox/src/snapshot.rs` | `PARTICIPATING_MOUNTS` gains `"tools"` for handle/execution state serialization |
| `crates/ox-kernel/src/lib.rs` | Remove `KernelState` enum — the agent's phase is derived from context (history), not internal state |

### Deleted files

| File | Reason |
|------|--------|
| `crates/ox-tools/src/turn.rs` | Replaced by HandleRegistry. TurnStore's queue model is superseded by handles-as-state. |

---

## Subsystem 1: Handle Registry (foundation)

ToolStore writes return handle paths. Reads of handle paths return the result. Writes still block during execution (no async yet), but the returned handle is the canonical path for reading the result. This is a refactor of the existing `last_result` pattern with proper IDs.

### Task 1: Create handle types and HandleRegistry

**Files:**
- Create: `crates/ox-tools/src/handle.rs`

- [ ] **Step 1: Write the failing test**

```rust
// In crates/ox-tools/src/handle.rs

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::Value;

    #[test]
    fn registry_store_and_retrieve() {
        let mut reg = HandleRegistry::new();
        let id = reg.next_id();
        reg.store_result(&id, Ok(Value::String("hello".into())));
        let result = reg.take_result(&id);
        assert!(result.is_some());
        let value = result.unwrap().unwrap();
        assert_eq!(value, Value::String("hello".into()));
    }

    #[test]
    fn take_result_removes_entry() {
        let mut reg = HandleRegistry::new();
        let id = reg.next_id();
        reg.store_result(&id, Ok(Value::String("hello".into())));
        let _ = reg.take_result(&id);
        assert!(reg.take_result(&id).is_none());
    }

    #[test]
    fn ids_are_unique() {
        let mut reg = HandleRegistry::new();
        let a = reg.next_id();
        let b = reg.next_id();
        assert_ne!(a, b);
    }

    #[test]
    fn unknown_id_returns_none() {
        let mut reg = HandleRegistry::new();
        assert!(reg.take_result("nonexistent").is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-tools -- handle::tests -v`
Expected: FAIL — module `handle` doesn't exist.

- [ ] **Step 3: Write HandleRegistry implementation**

```rust
// crates/ox-tools/src/handle.rs

//! Execution handle registry — maps handle IDs to tool execution results.
//!
//! Each tool write returns a handle ID. The handle can be read to retrieve
//! the result. In the synchronous model, results are stored immediately.
//! In the async model (future), handles wrap JoinHandles that resolve
//! when execution completes.

use std::collections::HashMap;
use structfs_core_store::Value;

/// Unique execution handle identifier.
pub type HandleId = String;

/// A completed execution result — Ok(value) or Err(error_value).
pub type ExecResult = Result<Value, Value>;

/// Registry mapping handle IDs to execution results.
///
/// In the current synchronous model, results are stored immediately
/// after execution. In the future async model, this will also hold
/// pending JoinHandles.
pub struct HandleRegistry {
    counter: u64,
    results: HashMap<HandleId, ExecResult>,
}

impl HandleRegistry {
    pub fn new() -> Self {
        Self {
            counter: 0,
            results: HashMap::new(),
        }
    }

    /// Generate a unique handle ID.
    pub fn next_id(&mut self) -> HandleId {
        self.counter += 1;
        format!("exec/{:04}", self.counter)
    }

    /// Store a completed result for a handle.
    pub fn store_result(&mut self, id: &str, result: ExecResult) {
        self.results.insert(id.to_string(), result);
    }

    /// Take the result for a handle, removing it from the registry.
    /// Returns None if the handle doesn't exist or hasn't completed.
    pub fn take_result(&mut self, id: &str) -> Option<ExecResult> {
        self.results.remove(id)
    }

    /// Read the result for a handle without removing it.
    pub fn peek_result(&self, id: &str) -> Option<&ExecResult> {
        self.results.get(id)
    }

    /// Return all handle IDs that have completed results.
    pub fn completed_ids(&self) -> Vec<HandleId> {
        self.results.keys().cloned().collect()
    }

    /// Clear all results.
    pub fn clear(&mut self) {
        self.results.clear();
    }
}

impl Default for HandleRegistry {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 4: Add `pub mod handle;` to `crates/ox-tools/src/lib.rs`**

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p ox-tools -- handle::tests -v`
Expected: PASS — all 4 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-tools/src/handle.rs crates/ox-tools/src/lib.rs
git commit -m "feat(ox-tools): add HandleRegistry for execution handle tracking"
```

---

### Task 2: ToolStore writes return handle paths

Replace the `last_result: BTreeMap<String, Value>` pattern in ToolStore with HandleRegistry. Tool writes execute synchronously (for now) and store the result under a handle ID. The write returns the handle path. Reads of handle paths return the result.

**Files:**
- Modify: `crates/ox-tools/src/lib.rs`

- [ ] **Step 1: Write the failing test**

```rust
// Add to existing tests in crates/ox-tools/src/lib.rs or a new test file

#[test]
fn tool_write_returns_handle_path() {
    let mut store = make_tool_store(); // existing helper
    let input = serde_json::json!({"path": "/tmp/test-workspace/test.txt"});
    let input_value = structfs_serde_store::json_to_value(input);
    let path = Path::parse("fs_read").unwrap();

    let result = store.write(&path, Record::parsed(input_value));
    // Write should succeed and return a handle path like "exec/0001"
    assert!(result.is_ok());
    let handle = result.unwrap();
    assert!(handle.to_string().starts_with("exec/"),
        "expected handle path starting with 'exec/', got: {}", handle);
}

#[test]
fn handle_path_readable_for_result() {
    let mut store = make_tool_store();
    let input = serde_json::json!({"path": "/tmp/test-workspace/test.txt"});
    let input_value = structfs_serde_store::json_to_value(input);
    let path = Path::parse("fs_read").unwrap();

    let handle = store.write(&path, Record::parsed(input_value)).unwrap();
    let result = store.read(&handle);
    assert!(result.is_ok());
    // Result should be Some (the tool output or error)
    assert!(result.unwrap().is_some());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ox-tools -- tool_write_returns_handle -v`
Expected: FAIL — writes currently return the input path, not a handle.

- [ ] **Step 3: Replace `last_result` with `HandleRegistry` in ToolStore**

In `crates/ox-tools/src/lib.rs`:

Remove the `last_result: BTreeMap<String, Value>` field. Add `handles: HandleRegistry` field.

Update the constructor:
```rust
Self {
    fs,
    os,
    completions,
    name_map,
    native_tools: HashMap::new(),
    handles: HandleRegistry::new(),
    turn: TurnStore::new(), // remove in Task 5
}
```

Update `execute_module` to store result in HandleRegistry and return a handle path:
```rust
fn execute_module(
    &mut self,
    module: &str,
    op: &str,
    input: &serde_json::Value,
) -> Result<Path, StoreError> {
    let handle_id = self.handles.next_id();
    let result = match module {
        "fs" => self.fs.execute(op, input),
        "os" => self.os.execute(op, input),
        _ => Err(format!("unknown module: {module}")),
    };

    match result {
        Ok(json_val) => {
            let value = structfs_serde_store::json_to_value(json_val);
            self.handles.store_result(&handle_id, Ok(value));
        }
        Err(e) => {
            self.handles.store_result(&handle_id, Err(Value::String(e)));
        }
    }

    Path::parse(&handle_id)
        .map_err(|e| StoreError::store("ToolStore", "execute", e.to_string()))
}
```

Update the Writer impl's `"fs" | "os"` arm to return the handle path from `execute_module`:
```rust
"fs" | "os" => {
    if path.len() < 2 {
        return Err(StoreError::store(
            "ToolStore", "write",
            format!("missing operation in path: {first}/"),
        ));
    }
    let op = path.components[1].clone();
    let value = data.as_value().ok_or_else(|| {
        StoreError::store("ToolStore", "write", "expected Parsed record")
    })?.clone();
    let json_input = structfs_serde_store::value_to_json(value);
    self.execute_module(first, &op, &json_input)
}
```

Update native tool writes similarly — store result in `handles`, return handle path.

Update the Reader impl to resolve handle paths:
```rust
// At the top of Reader::read, before resolve_path:
if !from.is_empty() && from.components[0] == "exec" {
    // Handle path read — return the stored result
    let handle_id = from.to_string();
    return match self.handles.peek_result(&handle_id) {
        Some(Ok(v)) => Ok(Some(Record::parsed(v.clone()))),
        Some(Err(v)) => Ok(Some(Record::parsed(v.clone()))),
        None => Ok(None),
    };
}
```

Remove all `last_result` reads from the `"fs" | "os"` read arm (they used to read from `last_result` — now reads go through handle paths).

- [ ] **Step 4: Update native tool write to use HandleRegistry**

Same pattern: `handles.next_id()`, execute, `handles.store_result()`, return handle path.

- [ ] **Step 5: Run all ToolStore tests**

Run: `cargo test -p ox-tools -v`
Expected: PASS — existing tests may need updates for handle-path returns.

- [ ] **Step 6: Run workspace tests**

Run: `cargo test --workspace`
Expected: PASS — the Wasm agent currently ignores the returned path from tool writes (uses a constructed path instead), so it should still work. The `tools/{wire_name}/result` read path needs a compatibility shim until the agent is updated in Task 6.

Note: the existing read path `tools/{wire_name}/result` must continue to work for now. Add a compatibility read: when reading `{wire_name}/result`, return the most recent result for any handle from that wire name. This keeps the existing Wasm agent working until Task 6 rewrites the loop.

- [ ] **Step 7: Commit**

```bash
git add -u crates/ox-tools/
git commit -m "refactor(ox-tools): ToolStore writes return handle paths via HandleRegistry"
```

---

## Subsystem 2: Async Spawn

Tool writes return handles immediately. Execution happens on a background thread. Handle reads block until the result is ready.

### Task 3: Split sandboxed_exec into spawn + await

**Files:**
- Modify: `crates/ox-tools/src/sandbox.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn sandboxed_spawn_returns_child_handle() {
    use std::process::Command;

    let policy = PermissivePolicy;
    let intent = AccessIntent::ReadFile(PathBuf::from("/tmp/test.txt"));
    let exec_cmd = ExecCommand {
        op: "fs/read".to_string(),
        args: serde_json::json!({"path": "/tmp/test.txt"}),
    };
    // This will fail because the executor binary doesn't exist,
    // but sandboxed_spawn should at least construct the command.
    // For a real test, we'd use a mock executor.
    let result = sandboxed_spawn(&intent, &exec_cmd, Path::new("/nonexistent"), &policy);
    // Expect an error (executor doesn't exist) but NOT a compile error
    assert!(result.is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-tools -- sandboxed_spawn -v`
Expected: FAIL — `sandboxed_spawn` doesn't exist.

- [ ] **Step 3: Implement sandboxed_spawn**

```rust
/// Spawn a sandboxed subprocess without waiting for it to complete.
/// Returns a SpawnedExec containing the Child process.
///
/// Not available on wasm32 targets.
#[cfg(not(target_arch = "wasm32"))]
pub fn sandboxed_spawn(
    intent: &AccessIntent,
    exec_cmd: &ExecCommand,
    executor_bin: &std::path::Path,
    policy: &dyn SandboxPolicy,
) -> Result<SpawnedExec, String> {
    use std::process::Command;

    let base = Command::new(executor_bin);
    let mut cmd = policy.apply(intent, base)?;

    let input_json = serde_json::to_string(exec_cmd)
        .map_err(|e| format!("failed to serialize command: {e}"))?;

    cmd.arg("--tool-exec")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn()
        .map_err(|e| format!("failed to spawn executor: {e}"))?;

    // Write JSON to stdin
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().ok_or("failed to open stdin")?;
        stdin.write_all(input_json.as_bytes())
            .map_err(|e| format!("failed to write to stdin: {e}"))?;
    }
    // Drop stdin to signal EOF
    child.stdin.take();

    Ok(SpawnedExec { child })
}

/// A spawned subprocess that hasn't been waited on yet.
#[cfg(not(target_arch = "wasm32"))]
pub struct SpawnedExec {
    child: std::process::Child,
}

#[cfg(not(target_arch = "wasm32"))]
impl SpawnedExec {
    /// Block until the subprocess completes and parse the result.
    pub fn await_result(mut self) -> Result<serde_json::Value, String> {
        let output = self.child.wait_with_output()
            .map_err(|e| format!("failed to wait on executor: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("executor exited with {}: {}", output.status, stderr));
        }

        let result: ExecResult = serde_json::from_slice(&output.stdout)
            .map_err(|e| format!("failed to parse executor output: {e}"))?;

        if result.ok {
            Ok(result.value)
        } else {
            Err(result.value.as_str().unwrap_or("unknown error").to_string())
        }
    }
}
```

- [ ] **Step 4: Reimplement `sandboxed_exec` in terms of spawn + await**

```rust
#[cfg(not(target_arch = "wasm32"))]
pub fn sandboxed_exec(
    intent: &AccessIntent,
    exec_cmd: &ExecCommand,
    executor_bin: &std::path::Path,
    policy: &dyn SandboxPolicy,
) -> Result<serde_json::Value, String> {
    let spawned = sandboxed_spawn(intent, exec_cmd, executor_bin, policy)?;
    spawned.await_result()
}
```

- [ ] **Step 5: Run all sandbox tests**

Run: `cargo test -p ox-tools -- sandbox -v`
Expected: PASS — `sandboxed_exec` behavior unchanged, just composed differently.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-tools/src/sandbox.rs
git commit -m "refactor(ox-tools): split sandboxed_exec into spawn + await"
```

---

### Task 4: FsModule and OsModule return spawn handles

**Files:**
- Modify: `crates/ox-tools/src/fs.rs`
- Modify: `crates/ox-tools/src/os.rs`

- [ ] **Step 1: Add `spawn` method to FsModule alongside existing `execute`**

```rust
/// Spawn an fs operation without blocking. Returns a SpawnedExec.
/// The caller can later call `spawned.await_result()` to get the result.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn(&self, op: &str, input: &Value) -> Result<SpawnedExec, String> {
    let path_str = input.get("path").and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'path' field".to_string())?;
    let resolved = self.resolve_path(path_str)?;

    let (intent, full_op) = match op {
        "read" => (AccessIntent::ReadFile(resolved.clone()), "fs/read"),
        "write" => (AccessIntent::WriteFile(resolved.clone()), "fs/write"),
        "edit" => (AccessIntent::ReadWriteFile(resolved.clone()), "fs/edit"),
        _ => return Err(format!("unknown fs operation: {op}")),
    };

    let mut args = input.clone();
    if let Some(obj) = args.as_object_mut() {
        obj.insert("path".to_string(), Value::String(resolved.to_string_lossy().into()));
    }

    let exec_cmd = ExecCommand { op: full_op.to_string(), args };
    sandboxed_spawn(&intent, &exec_cmd, &self.executor_bin, self.policy.as_ref())
}
```

- [ ] **Step 2: Add `spawn` method to OsModule**

Same pattern as FsModule — `spawn` creates a `SpawnedExec`, existing `execute` keeps working.

- [ ] **Step 3: Run existing integration tests**

Run: `cargo test -p ox-tools -v`
Expected: PASS — `execute` unchanged, `spawn` is additive.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-tools/src/fs.rs crates/ox-tools/src/os.rs
git commit -m "feat(ox-tools): add spawn() to FsModule and OsModule for async execution"
```

---

### Task 5: ToolStore uses spawn + thread for async execution

**Files:**
- Modify: `crates/ox-tools/src/lib.rs`
- Modify: `crates/ox-tools/src/handle.rs`

- [ ] **Step 1: Add pending handle support to HandleRegistry**

```rust
// In handle.rs, add:
use std::sync::{Arc, Mutex};

/// A pending result that will be filled by a background thread.
pub struct PendingHandle {
    result: Arc<Mutex<Option<ExecResult>>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl PendingHandle {
    /// Create a new pending handle and the sender that fills it.
    pub fn new() -> (PendingHandle, PendingSender) {
        let result = Arc::new(Mutex::new(None));
        let sender = PendingSender { result: result.clone() };
        (PendingHandle { result, join: None }, sender)
    }

    /// Set the JoinHandle for the background thread.
    pub fn set_join(&mut self, join: std::thread::JoinHandle<()>) {
        self.join = Some(join);
    }

    /// Block until the result is available.
    pub fn wait(self) -> ExecResult {
        if let Some(join) = self.join {
            join.join().ok();
        }
        let lock = self.result.lock().unwrap();
        lock.clone().unwrap_or(Err(Value::String("handle was dropped".into())))
    }

    /// Check if the result is ready without blocking.
    pub fn is_ready(&self) -> bool {
        self.result.lock().unwrap().is_some()
    }
}

pub struct PendingSender {
    result: Arc<Mutex<Option<ExecResult>>>,
}

impl PendingSender {
    pub fn send(self, result: ExecResult) {
        *self.result.lock().unwrap() = Some(result);
    }
}
```

Extend HandleRegistry:
```rust
pub struct HandleRegistry {
    counter: u64,
    results: HashMap<HandleId, ExecResult>,
    pending: HashMap<HandleId, PendingHandle>,
}

impl HandleRegistry {
    /// Store a pending handle that will be filled later.
    pub fn store_pending(&mut self, id: &str, handle: PendingHandle) {
        self.pending.insert(id.to_string(), handle);
    }

    /// Resolve a pending handle — blocks until the result is ready,
    /// moves it from pending to results, returns the result.
    pub fn resolve(&mut self, id: &str) -> Option<ExecResult> {
        if let Some(pending) = self.pending.remove(id) {
            let result = pending.wait();
            self.results.insert(id.to_string(), result.clone());
            Some(result)
        } else {
            self.results.get(id).cloned()
        }
    }

    /// Resolve all pending handles. Returns the IDs that were resolved.
    pub fn resolve_all(&mut self) -> Vec<HandleId> {
        let ids: Vec<HandleId> = self.pending.keys().cloned().collect();
        for id in &ids {
            self.resolve(id);
        }
        ids
    }
}
```

- [ ] **Step 2: ToolStore spawns execution on background threads**

In `execute_module`, instead of calling `self.fs.execute()` (which blocks), call `self.fs.spawn()` and wrap in a thread:

```rust
fn execute_module_async(
    &mut self,
    module: &str,
    op: &str,
    input: &serde_json::Value,
) -> Result<Path, StoreError> {
    let handle_id = self.handles.next_id();

    // For wasm32 or native tools, execute synchronously
    #[cfg(target_arch = "wasm32")]
    {
        let result = Err(Value::String("not available on wasm32".into()));
        self.handles.store_result(&handle_id, result);
        return Path::parse(&handle_id)
            .map_err(|e| StoreError::store("ToolStore", "execute", e.to_string()));
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let spawned = match module {
            "fs" => self.fs.spawn(op, input),
            "os" => self.os.spawn(op, input),
            _ => return Err(StoreError::store("ToolStore", "execute", format!("unknown module: {module}"))),
        };

        match spawned {
            Ok(exec) => {
                let (pending, sender) = PendingHandle::new();
                let thread = std::thread::spawn(move || {
                    let result = match exec.await_result() {
                        Ok(json_val) => Ok(structfs_serde_store::json_to_value(json_val)),
                        Err(e) => Err(Value::String(e)),
                    };
                    sender.send(result);
                });
                let mut pending = pending;
                pending.set_join(thread);
                self.handles.store_pending(&handle_id, pending);
            }
            Err(e) => {
                self.handles.store_result(&handle_id, Err(Value::String(e)));
            }
        }

        Path::parse(&handle_id)
            .map_err(|e| StoreError::store("ToolStore", "execute", e.to_string()))
    }
}
```

- [ ] **Step 3: Handle reads block-until-ready**

Update the Reader impl's handle resolution:
```rust
if !from.is_empty() && from.components[0] == "exec" {
    let handle_id = from.to_string();
    // resolve() blocks if the handle is still pending
    return match self.handles.resolve(&handle_id) {
        Some(Ok(v)) => Ok(Some(Record::parsed(v))),
        Some(Err(v)) => Ok(Some(Record::parsed(v))),
        None => Ok(None),
    };
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-tools -v`
Run: `cargo test --workspace`
Expected: PASS — existing code still works, handle reads block-until-ready.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-tools/src/handle.rs crates/ox-tools/src/lib.rs
git commit -m "feat(ox-tools): async tool execution via spawned handles"
```

---

## Subsystem 3: Batch Await

### Task 6: tools/await path accepts handle set, returns batch handle

**Files:**
- Modify: `crates/ox-tools/src/lib.rs`
- Modify: `crates/ox-tools/src/handle.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn await_path_resolves_multiple_handles() {
    let mut store = make_tool_store();
    // Store two fake results directly
    store.handles.store_result("exec/0001", Ok(Value::String("result1".into())));
    store.handles.store_result("exec/0002", Ok(Value::String("result2".into())));

    // Write the handles to tools/await
    let handles_json = serde_json::json!(["exec/0001", "exec/0002"]);
    let handles_value = structfs_serde_store::json_to_value(handles_json);
    let batch_handle = store.write(
        &Path::parse("await").unwrap(),
        Record::parsed(handles_value),
    ).unwrap();

    // Read the batch handle → returns all results
    let result = store.read(&batch_handle).unwrap().unwrap();
    let json = structfs_serde_store::value_to_json(result.as_value().unwrap().clone());
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 2);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-tools -- await_path -v`
Expected: FAIL — `await` path not handled.

- [ ] **Step 3: Implement `await` write path**

In ToolStore's Writer impl, add an `"await"` arm:
```rust
"await" => {
    // Data is a JSON array of handle ID strings
    let value = data.as_value().ok_or_else(|| {
        StoreError::store("ToolStore", "await", "expected parsed record")
    })?.clone();
    let json = structfs_serde_store::value_to_json(value);
    let handle_ids: Vec<String> = match json {
        serde_json::Value::Array(arr) => arr.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => return Err(StoreError::store("ToolStore", "await", "expected array of handle IDs")),
    };

    // Resolve all handles (blocks until each completes)
    let mut results = Vec::new();
    for id in &handle_ids {
        let result = self.handles.resolve(id);
        let value = match result {
            Some(Ok(v)) => serde_json::json!({"handle": id, "value": structfs_serde_store::value_to_json(v)}),
            Some(Err(v)) => serde_json::json!({"handle": id, "error": structfs_serde_store::value_to_json(v)}),
            None => serde_json::json!({"handle": id, "error": "unknown handle"}),
        };
        results.push(value);
    }

    // Store the batch result under a new handle
    let batch_id = self.handles.next_id();
    let batch_value = structfs_serde_store::json_to_value(serde_json::Value::Array(results));
    self.handles.store_result(&batch_id, Ok(batch_value));

    Path::parse(&batch_id)
        .map_err(|e| StoreError::store("ToolStore", "await", e.to_string()))
}
```

Also add `"await"` to the `resolve_path` match:
```rust
"fs" | "os" | "completions" | "schemas" | "turn" | "await" => Some(ResolvedPath::Direct(path)),
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-tools -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/ox-tools/src/lib.rs
git commit -m "feat(ox-tools): tools/await resolves batch of handles"
```

---

## Subsystem 4: Agent Loop Rewrite

### Task 7: Wasm agent uses handle pattern

**Files:**
- Modify: `crates/ox-wasm/src/lib.rs`

- [ ] **Step 1: Rewrite the tool execution section of agent_main**

Replace the current tool execution loop (lines 260-348) with the handle pattern:

```rust
// Fire all tool writes — collect handles.
// Each write returns immediately with a handle path.
let mut handles = Vec::new();
for tc in &tool_calls {
    // Emit tool_call_start event for TUI.
    let start_event = serde_json::json!({"type": "tool_call_start", "name": tc.name});
    bridge.write(
        &path!("events/emit"),
        Record::parsed(structfs_serde_store::json_to_value(start_event)),
    ).ok();

    // Write tool input — returns handle immediately.
    let input_value = structfs_serde_store::json_to_value(tc.input.clone());
    let tool_path = Path::parse(&format!("tools/{}", tc.name))
        .map_err(|e| e.to_string())?;
    let handle = bridge.write(&tool_path, Record::parsed(input_value))
        .map_err(|e| e.to_string())?;

    handles.push((tc.id.clone(), tc.name.clone(), handle));
}

// Write all handles to tools/await — blocks until all resolve.
let handle_ids: Vec<serde_json::Value> = handles.iter()
    .map(|(_, _, h)| serde_json::Value::String(h.to_string()))
    .collect();
let await_value = structfs_serde_store::json_to_value(
    serde_json::Value::Array(handle_ids),
);
let batch_handle = bridge.write(
    &path!("tools/await"),
    Record::parsed(await_value),
).map_err(|e| e.to_string())?;

// Read batch result.
let batch_record = bridge.read(&batch_handle)
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "no batch result".to_string())?;
let batch_json = structfs_serde_store::value_to_json(
    batch_record.as_value().cloned().unwrap_or(Value::Null),
);
let batch_arr = batch_json.as_array().cloned().unwrap_or_default();

// Convert to ToolResults and emit events.
let mut results = Vec::new();
for (i, (tool_use_id, tool_name, _)) in handles.iter().enumerate() {
    let entry = batch_arr.get(i).cloned().unwrap_or_default();
    let result_str = if let Some(val) = entry.get("value") {
        serde_json::to_string(val).unwrap_or_default()
    } else if let Some(err) = entry.get("error") {
        format!("error: {}", serde_json::to_string(err).unwrap_or_default())
    } else {
        "error: unknown".to_string()
    };

    let end_event = serde_json::json!({
        "type": "tool_call_result",
        "name": tool_name,
        "result": &result_str,
    });
    bridge.write(
        &path!("events/emit"),
        Record::parsed(structfs_serde_store::json_to_value(end_event)),
    ).ok();

    results.push(ToolResult {
        tool_use_id: tool_use_id.clone(),
        content: serde_json::Value::String(result_str),
    });
}

// Write tool results to history.
let history_json = ox_kernel::serialize_tool_results(&results);
let history_value = structfs_serde_store::json_to_value(history_json);
bridge.write(&path!("history/append"), Record::parsed(history_value))
    .map_err(|e| e.to_string())?;
```

- [ ] **Step 2: Build the Wasm agent**

Run: `cargo check --target wasm32-unknown-unknown -p ox-wasm`
Expected: PASS — note that on wasm32, tool execution is stubbed (no subprocesses), so the async path won't activate, but the handle protocol works the same.

- [ ] **Step 3: Build the agent binary and run integration test**

Run: `./scripts/build-agent.sh && cargo test -p ox-runtime -- load_and_run_agent_wasm -v`
Expected: PASS — the integration test exercises the full Wasm → HostStore → ToolStore path.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-wasm/src/lib.rs
git commit -m "refactor(ox-wasm): agent loop uses handle pattern for tool execution"
```

---

### Task 8: Remove TurnStore

**Files:**
- Delete: `crates/ox-tools/src/turn.rs`
- Modify: `crates/ox-tools/src/lib.rs`
- Modify: `crates/ox-cli/src/policy_check.rs`

- [ ] **Step 1: Remove `turn` field and `turn/*` routing from ToolStore**

In `crates/ox-tools/src/lib.rs`:
- Remove `use crate::turn::{EffectOutcome, TurnStore};`
- Remove `turn: TurnStore` field
- Remove `TurnStore::new()` from constructor
- Remove the `"turn"` arm from both Reader and Writer impls
- Remove `"turn"` from `resolve_path` match

- [ ] **Step 2: Remove `"turn"` from PolicyCheck skip list**

In `crates/ox-cli/src/policy_check.rs`, change:
```rust
"schemas" | "completions" | "turn" => return PolicyDecision::Allow,
```
to:
```rust
"schemas" | "completions" => return PolicyDecision::Allow,
```

- [ ] **Step 3: Delete `crates/ox-tools/src/turn.rs`**

- [ ] **Step 4: Remove `pub mod turn;` from `crates/ox-tools/src/lib.rs`**

- [ ] **Step 5: Run tests**

Run: `cargo test --workspace`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add -u
git commit -m "refactor: remove TurnStore — handles ARE the state"
```

---

### Task 9: Crash recovery — agent reads context on startup

**Files:**
- Modify: `crates/ox-wasm/src/lib.rs`

- [ ] **Step 1: Add recovery check at start of agent_main**

Before the main loop, the agent reads history to detect mid-turn crashes. If the last message is an assistant message with tool_use blocks and there are no corresponding tool result messages after it, the agent re-issues the tool calls.

```rust
fn agent_main() -> Result<(), String> {
    let mut bridge = HostBridge;

    // Read context to detect crash recovery scenario.
    // If the last assistant message has tool_use blocks with no tool results
    // following, we crashed mid-execution. Re-derive tool calls from history.
    let pending_tool_calls = detect_pending_tool_calls(&mut bridge)?;

    let model = /* ... existing model read ... */;
    let default_account = /* ... existing account read ... */;
    let mut kernel = Kernel::new(model);

    // If we have pending tool calls from a crash, execute them first
    // before entering the normal loop.
    if !pending_tool_calls.is_empty() {
        execute_tool_calls(&mut bridge, &pending_tool_calls)?;
    }

    loop {
        // ... existing loop ...
    }
}

/// Check history for unresolved tool calls (crash recovery).
fn detect_pending_tool_calls(bridge: &mut HostBridge) -> Result<Vec<ToolCall>, String> {
    let messages_record = bridge.read(&path!("history/messages"))
        .map_err(|e| e.to_string())?;
    let Some(record) = messages_record else { return Ok(vec![]); };
    let json = structfs_serde_store::value_to_json(
        record.as_value().cloned().unwrap_or(Value::Null),
    );
    let messages = json.as_array().cloned().unwrap_or_default();

    if messages.is_empty() { return Ok(vec![]); }

    let last = &messages[messages.len() - 1];
    let role = last.get("role").and_then(|v| v.as_str()).unwrap_or("");

    if role != "assistant" { return Ok(vec![]); }

    // Check if the assistant message has tool_use blocks
    let content = last.get("content").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let tool_calls: Vec<ToolCall> = content.iter()
        .filter_map(|block| {
            if block.get("type")?.as_str()? != "tool_use" { return None; }
            Some(ToolCall {
                id: block.get("id")?.as_str()?.to_string(),
                name: block.get("name")?.as_str()?.to_string(),
                input: block.get("input")?.clone(),
            })
        })
        .collect();

    if tool_calls.is_empty() { return Ok(vec![]); }

    // If the NEXT message after assistant is tool results, recovery isn't needed
    // (But we checked last message, so if it's assistant with tool_use, and there's
    // no tool_result after it, we need recovery)
    Ok(tool_calls)
}
```

- [ ] **Step 2: Extract tool execution into a shared function**

The tool execution code (fire writes → collect handles → await → history) is used both in the main loop and in crash recovery. Extract it to `execute_tool_calls`:

```rust
fn execute_tool_calls(bridge: &mut HostBridge, tool_calls: &[ToolCall]) -> Result<(), String> {
    let mut handles = Vec::new();
    for tc in tool_calls {
        let start_event = serde_json::json!({"type": "tool_call_start", "name": tc.name});
        bridge.write(
            &path!("events/emit"),
            Record::parsed(structfs_serde_store::json_to_value(start_event)),
        ).ok();

        let input_value = structfs_serde_store::json_to_value(tc.input.clone());
        let tool_path = Path::parse(&format!("tools/{}", tc.name))
            .map_err(|e| e.to_string())?;
        let handle = bridge.write(&tool_path, Record::parsed(input_value))
            .map_err(|e| e.to_string())?;
        handles.push((tc.id.clone(), tc.name.clone(), handle));
    }

    let handle_ids: Vec<serde_json::Value> = handles.iter()
        .map(|(_, _, h)| serde_json::Value::String(h.to_string()))
        .collect();
    let batch_handle = bridge.write(
        &path!("tools/await"),
        Record::parsed(structfs_serde_store::json_to_value(
            serde_json::Value::Array(handle_ids),
        )),
    ).map_err(|e| e.to_string())?;

    let batch_record = bridge.read(&batch_handle)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no batch result".to_string())?;
    let batch_json = structfs_serde_store::value_to_json(
        batch_record.as_value().cloned().unwrap_or(Value::Null),
    );
    let batch_arr = batch_json.as_array().cloned().unwrap_or_default();

    let mut results = Vec::new();
    for (i, (tool_use_id, tool_name, _)) in handles.iter().enumerate() {
        let entry = batch_arr.get(i).cloned().unwrap_or_default();
        let result_str = if let Some(val) = entry.get("value") {
            serde_json::to_string(val).unwrap_or_default()
        } else if let Some(err) = entry.get("error") {
            format!("error: {}", serde_json::to_string(err).unwrap_or_default())
        } else {
            "error: unknown".to_string()
        };

        let end_event = serde_json::json!({
            "type": "tool_call_result", "name": tool_name, "result": &result_str,
        });
        bridge.write(
            &path!("events/emit"),
            Record::parsed(structfs_serde_store::json_to_value(end_event)),
        ).ok();

        results.push(ToolResult {
            tool_use_id: tool_use_id.clone(),
            content: serde_json::Value::String(result_str),
        });
    }

    let history_json = ox_kernel::serialize_tool_results(&results);
    let history_value = structfs_serde_store::json_to_value(history_json);
    bridge.write(&path!("history/append"), Record::parsed(history_value))
        .map_err(|e| e.to_string())?;

    Ok(())
}
```

- [ ] **Step 3: Build and test**

Run: `cargo check --target wasm32-unknown-unknown -p ox-wasm`
Run: `./scripts/build-agent.sh && cargo test -p ox-runtime -- load_and_run -v`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/ox-wasm/src/lib.rs
git commit -m "feat(ox-wasm): crash recovery — detect pending tool calls from context"
```

---

## Subsystem 5: Context Serialization

### Task 10: Extend snapshot to cover full context

**Files:**
- Modify: `crates/ox-inbox/src/snapshot.rs`

- [ ] **Step 1: Add "tools" to PARTICIPATING_MOUNTS**

```rust
pub const PARTICIPATING_MOUNTS: [&str; 3] = ["system", "gate", "tools"];
```

This requires ToolStore to implement snapshot reads (`snapshot/state` and `snapshot/hash`). HandleRegistry state should be included — completed handles are part of the context. Pending handles cannot be serialized (they contain thread JoinHandles), but their existence should be recorded so crash recovery knows what was in flight.

- [ ] **Step 2: Add snapshot support to ToolStore**

In `crates/ox-tools/src/lib.rs`, add snapshot handling to the Reader impl:

```rust
"snapshot" => {
    if path.len() >= 2 {
        match path.components[1].as_str() {
            "hash" => {
                let state = self.snapshot_state();
                let hash = ox_kernel::snapshot::snapshot_hash(&state);
                Ok(Some(Record::parsed(Value::String(hash))))
            }
            "state" => {
                Ok(Some(Record::parsed(self.snapshot_state())))
            }
            _ => Ok(None),
        }
    } else {
        let state = self.snapshot_state();
        Ok(Some(Record::parsed(ox_kernel::snapshot::snapshot_record(state))))
    }
}
```

And to the Writer impl:
```rust
"snapshot" => {
    // Restore from snapshot state
    // (schemas and module config are reconstructed, only handle results are restored)
    Ok(path.clone())
}
```

Add the `snapshot_state` helper:
```rust
fn snapshot_state(&self) -> Value {
    // Snapshot the completed handle results
    let mut map = BTreeMap::new();
    let completed: Vec<serde_json::Value> = self.handles.completed_ids()
        .iter()
        .filter_map(|id| {
            self.handles.peek_result(id).map(|r| {
                let value = match r {
                    Ok(v) => serde_json::json!({"id": id, "ok": structfs_serde_store::value_to_json(v.clone())}),
                    Err(v) => serde_json::json!({"id": id, "err": structfs_serde_store::value_to_json(v.clone())}),
                };
                value
            })
        })
        .collect();
    map.insert(
        "handles".to_string(),
        structfs_serde_store::json_to_value(serde_json::Value::Array(completed)),
    );
    Value::Map(map)
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test --workspace`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/ox-inbox/src/snapshot.rs crates/ox-tools/src/lib.rs
git commit -m "feat: extend context snapshot to cover tool execution state"
```

---

### Task 11: Remove KernelState enum

**Files:**
- Modify: `crates/ox-kernel/src/lib.rs`

The `KernelState` enum (`Idle`, `Streaming`, `Executing`) tracks internal kernel phase. But the agent's phase is derivable from context: read history, check last message type. The kernel doesn't need internal state tracking — it's the agent's job to call the right methods in order.

- [ ] **Step 1: Remove `KernelState` enum and `state` field from Kernel**

```rust
pub struct Kernel {
    model: String,
}

impl Kernel {
    pub fn new(model: String) -> Self {
        Self { model }
    }
}
```

- [ ] **Step 2: Remove state assertions from three-phase methods**

Remove `assert_eq!(self.state, KernelState::Idle)` from `initiate_completion`, `consume_events`, `complete_turn`. Remove `self.state = KernelState::Streaming` etc.

- [ ] **Step 3: Remove `pub fn state(&self)` method**

- [ ] **Step 4: Check for any consumers of `KernelState`**

Run: `grep -rn "KernelState\|\.state()" crates/ --include="*.rs"`
Fix any references.

- [ ] **Step 5: Run tests**

Run: `cargo test --workspace`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/ox-kernel/src/lib.rs
git commit -m "refactor(ox-kernel): remove KernelState — agent phase is derived from context"
```

---

### Task 12: Run quality gates

**Files:** None (verification only)

- [ ] **Step 1: Format**

Run: `./scripts/fmt.sh`

- [ ] **Step 2: Run all quality gates**

Run: `./scripts/quality_gates.sh`
Expected: 15/15 pass

- [ ] **Step 3: Commit any formatting fixes**

```bash
git add -u
git commit -m "chore: formatting after pico-process agent refactor"
```

---

## Verification Criteria

- [ ] Tool writes return handle paths (`exec/NNNN`), not input paths
- [ ] Handle reads resolve to tool results (blocking if pending)
- [ ] `tools/await` accepts array of handle IDs, returns batch handle
- [ ] Wasm agent uses fire-all-writes → await → read pattern
- [ ] No TurnStore in codebase (turn.rs deleted, no `turn/*` routing)
- [ ] Agent detects mid-turn crash from context and re-executes pending tool calls
- [ ] Context snapshot includes tool execution state (PARTICIPATING_MOUNTS includes "tools")
- [ ] KernelState enum removed — phase derived from context
- [ ] 15/15 quality gates pass
- [ ] `cargo test --workspace` passes
- [ ] `cargo check --target wasm32-unknown-unknown -p ox-wasm` passes

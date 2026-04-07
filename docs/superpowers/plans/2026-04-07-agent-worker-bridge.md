# Agent Worker Bridge Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Agent workers get scoped `ClientHandle`s from the broker instead of building `Namespace`s directly, connecting the agent loop to the StructFS TUI architecture.

**Architecture:** Make `HostStore` generic over its read/write backend (`Namespace` or `SyncClientAdapter`). Extract prompt synthesis from `Namespace` into a reusable function. Add `SyncClientAdapter` to bridge async `ClientHandle` to sync `Reader`/`Writer`. Mount per-thread stores in the broker. Wire `agent_worker` to use the broker path.

**Tech Stack:** Rust, structfs-core-store (Reader/Writer/Store/Path/Record/Value), ox-broker (BrokerStore/ClientHandle), ox-runtime (HostStore/AgentModule), tokio (block_on for sync-over-async bridge)

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/ox-context/src/lib.rs` | Modify | Extract `synthesize_prompt()` into public standalone function |
| `crates/ox-runtime/src/host_store.rs` | Modify | Generic `HostStore<B, E>`, own `tool_results`, intercept `prompt` |
| `crates/ox-runtime/src/engine.rs` | Modify | Generic `AgentState<B, E>` and `AgentModule::run<B, E>` |
| `crates/ox-runtime/src/lib.rs` | Modify | Update re-exports for new type params |
| `crates/ox-broker/src/sync_adapter.rs` | Create | `SyncClientAdapter` — sync Reader/Writer over async ClientHandle |
| `crates/ox-broker/src/lib.rs` | Modify | Add `#[derive(Clone)]`, export sync_adapter module |
| `crates/ox-cli/src/thread_mount.rs` | Create | `mount_thread()` / `unmount_thread()` — per-thread store lifecycle |
| `crates/ox-cli/src/agents.rs` | Modify | Wire worker through broker instead of Namespace |
| `crates/ox-cli/src/app.rs` | Modify | Pass broker + rt_handle to AgentPool |
| `crates/ox-cli/src/broker_setup.rs` | Modify | Return broker for sharing with AgentPool |
| `crates/ox-cli/src/main.rs` | Modify | Thread broker through to App/AgentPool |

---

### Task 1: Extract prompt synthesis from Namespace

Prompt synthesis currently lives inside `Namespace::synthesize_prompt()` (ox-context/src/lib.rs:54-178). We need it callable with any `Reader` backend — both `Namespace` and the future `SyncClientAdapter`. Extract it into a public function that takes `&mut dyn Reader`.

**Files:**
- Modify: `crates/ox-context/src/lib.rs:54-178`
- Test: existing tests in same file (must still pass)

- [ ] **Step 1: Write test for standalone synthesis function**

Add to `crates/ox-context/src/lib.rs` tests module:

```rust
#[test]
fn synthesize_prompt_standalone() {
    let mut ns = build_full_namespace();
    // Write a user message so there's history
    let user_msg = serde_json::json!({"role": "user", "content": "hello"});
    ns.write(
        &path!("history/append"),
        Record::parsed(structfs_serde_store::json_to_value(user_msg)),
    )
    .unwrap();

    let result = synthesize_prompt(&mut ns).unwrap().unwrap();
    let value = result.as_value().unwrap().clone();
    let json = structfs_serde_store::value_to_json(value);
    let request: CompletionRequest = serde_json::from_value(json).unwrap();
    assert_eq!(request.model, "claude-sonnet-4-20250514");
    assert_eq!(request.system, "You are helpful.");
    assert_eq!(request.messages.len(), 1);
    assert!(request.stream);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-context synthesize_prompt_standalone`
Expected: FAIL — `synthesize_prompt` function doesn't exist yet.

- [ ] **Step 3: Extract synthesis into public function**

In `crates/ox-context/src/lib.rs`, add this public function above the `Namespace` struct:

```rust
/// Synthesize a CompletionRequest by reading from system, history, tools, and model paths.
///
/// Works with any Reader: a Namespace routes to mounted stores, a SyncClientAdapter
/// routes through the broker to per-thread stores. The reader must resolve:
/// - `system` → String (system prompt)
/// - `history/messages` → Array (conversation messages)
/// - `tools/schemas` → Array (tool schemas)
/// - `model/id` → String (model identifier)
/// - `model/max_tokens` → Integer (token limit)
pub fn synthesize_prompt(reader: &mut dyn Reader) -> Result<Option<Record>, StoreError> {
    let system_str = {
        let record = reader.read(&path!("system"))?.ok_or_else(|| {
            StoreError::store("prompt", "read", "system store returned None")
        })?;
        match record {
            Record::Parsed(Value::String(s)) => s,
            _ => {
                return Err(StoreError::store(
                    "prompt",
                    "read",
                    "expected string from system store",
                ));
            }
        }
    };

    let messages_json = {
        let record = reader.read(&path!("history/messages"))?.ok_or_else(|| {
            StoreError::store("prompt", "read", "history store returned None")
        })?;
        match record {
            Record::Parsed(v) => value_to_json(v),
            _ => {
                return Err(StoreError::store(
                    "prompt",
                    "read",
                    "expected parsed record from history",
                ));
            }
        }
    };

    let tools_json = {
        let record = reader.read(&path!("tools/schemas"))?.ok_or_else(|| {
            StoreError::store("prompt", "read", "tools store returned None")
        })?;
        match record {
            Record::Parsed(v) => value_to_json(v),
            _ => {
                return Err(StoreError::store(
                    "prompt",
                    "read",
                    "expected parsed record from tools",
                ));
            }
        }
    };

    let model_id = {
        let record = reader.read(&path!("model/id"))?.ok_or_else(|| {
            StoreError::store("prompt", "read", "model store returned None for id")
        })?;
        match record {
            Record::Parsed(Value::String(s)) => s,
            _ => {
                return Err(StoreError::store(
                    "prompt",
                    "read",
                    "expected string from model store for id",
                ));
            }
        }
    };

    let max_tokens = {
        let record = reader.read(&path!("model/max_tokens"))?.ok_or_else(|| {
            StoreError::store("prompt", "read", "model store returned None for max_tokens")
        })?;
        match record {
            Record::Parsed(Value::Integer(n)) => n as u32,
            _ => {
                return Err(StoreError::store(
                    "prompt",
                    "read",
                    "expected integer from model store for max_tokens",
                ));
            }
        }
    };

    let messages: Vec<serde_json::Value> = serde_json::from_value(messages_json)
        .map_err(|e| StoreError::store("prompt", "read", e.to_string()))?;
    let tools: Vec<ox_kernel::ToolSchema> = serde_json::from_value(tools_json)
        .map_err(|e| StoreError::store("prompt", "read", e.to_string()))?;

    let request = CompletionRequest {
        model: model_id,
        max_tokens,
        system: system_str,
        messages,
        tools,
        stream: true,
    };

    let value = to_value(&request)
        .map_err(|e| StoreError::store("prompt", "read", e.to_string()))?;
    Ok(Some(Record::parsed(value)))
}
```

Then replace `Namespace::synthesize_prompt` body to delegate:

```rust
fn synthesize_prompt(&mut self) -> Result<Option<Record>, StoreError> {
    synthesize_prompt(self)
}
```

- [ ] **Step 4: Run all ox-context tests**

Run: `cargo test -p ox-context`
Expected: All pass (existing tests + new standalone test).

- [ ] **Step 5: Commit**

```bash
git add crates/ox-context/src/lib.rs
git commit -m "refactor(ox-context): extract synthesize_prompt into standalone function"
```

---

### Task 2: Make HostStore generic over backend

Currently `HostStore<E>` wraps a `Namespace`. Make it `HostStore<B, E>` where `B: Reader + Writer + Send`. The `tool_results` SimpleStore moves from being mounted in the backend to being owned directly by HostStore. HostStore intercepts `prompt` reads (calls `ox_context::synthesize_prompt` on the backend).

**Files:**
- Modify: `crates/ox-runtime/src/host_store.rs`
- Modify: `crates/ox-runtime/src/engine.rs`
- Modify: `crates/ox-runtime/src/lib.rs`
- Modify: `crates/ox-cli/src/agents.rs` (update type at callsite)
- Test: `crates/ox-runtime/src/host_store.rs` (existing tests, adapted)

- [ ] **Step 1: Update HostStore struct and impl to be generic**

In `crates/ox-runtime/src/host_store.rs`:

Change the struct:

```rust
/// StructFS middleware wrapping a backend with effect interception.
///
/// Reads and writes to certain paths are intercepted and routed to
/// the [`HostEffects`] handler instead of the underlying backend:
///
/// - **`prompt`** (read) — synthesizes CompletionRequest from sub-paths
/// - **`gate/complete`** (write) — triggers LLM completion
/// - **`gate/response`** (read) — returns pending stream events
/// - **`tools/execute`** (write) — executes a tool call
/// - **`events/emit`** (write) — emits an agent event
/// - **`tool_results/*`** (read/write) — tool output storage
pub struct HostStore<B: Reader + Writer + Send, E: HostEffects> {
    /// The underlying backend for non-effectful operations.
    pub backend: B,
    /// The effects handler.
    pub effects: E,
    /// Pending stream events from the most recent completion.
    pending_events: Option<Vec<StreamEvent>>,
    /// In-memory store for tool execution results.
    tool_results: SimpleStore,
}
```

Update `new`:

```rust
impl<B: Reader + Writer + Send, E: HostEffects> HostStore<B, E> {
    pub fn new(backend: B, effects: E) -> Self {
        Self {
            backend,
            effects,
            pending_events: None,
            tool_results: SimpleStore::new(),
        }
    }
```

Update `handle_read`:

```rust
    pub fn handle_read(&mut self, path: &Path) -> Result<Option<Record>, StoreError> {
        if path == &path!("gate/response") {
            return self.read_gate_response();
        }
        if path == &path!("prompt") {
            return ox_context::synthesize_prompt(&mut self.backend);
        }
        if !path.is_empty() && path.components[0] == "tool_results" {
            let sub = Path::from_components(path.components[1..].to_vec());
            return self.tool_results.read(&sub);
        }
        self.backend.read(path)
    }
```

Update `handle_write`:

```rust
    pub fn handle_write(&mut self, path: &Path, data: Record) -> Result<Path, StoreError> {
        let prefix = if path.is_empty() {
            ""
        } else {
            path.components[0].as_str()
        };

        match prefix {
            "gate" if path == &path!("gate/complete") => self.write_gate_complete(data),
            "tools" if path == &path!("tools/execute") => self.write_tools_execute(data),
            "events" if path == &path!("events/emit") => self.write_events_emit(data),
            "tool_results" => {
                let sub = Path::from_components(path.components[1..].to_vec());
                self.tool_results.write(&sub, data)
            }
            _ => self.backend.write(path, data),
        }
    }
```

Update `write_tools_execute` — write to `self.tool_results` instead of `self.namespace`:

```rust
    fn write_tools_execute(&mut self, data: Record) -> Result<Path, StoreError> {
        let call: ToolCall = record_to_typed(data, "tools", "execute")?;

        self.effects.emit_event(AgentEvent::ToolCallStart {
            name: call.name.clone(),
        });

        let result = self
            .effects
            .execute_tool(&call)
            .map_err(|e| StoreError::store("tools", "execute", e))?;

        self.effects.emit_event(AgentEvent::ToolCallResult {
            name: call.name.clone(),
            result: result.clone(),
        });

        let result_path = Path::parse(&format!("tool_results/{}", call.id))
            .map_err(|e| StoreError::store("tools", "execute", e.to_string()))?;
        let sub = Path::from_components(result_path.components[1..].to_vec());
        let result_record = Record::parsed(Value::String(result));
        self.tool_results.write(&sub, result_record)?;

        Ok(result_path)
    }
```

- [ ] **Step 2: Update engine.rs for generic type parameters**

In `crates/ox-runtime/src/engine.rs`:

Change `AgentState`:

```rust
pub struct AgentState<B: Reader + Writer + Send, E: HostEffects> {
    pub host_store: HostStore<B, E>,
    pending_result: Option<Vec<u8>>,
}
```

Change `AgentModule::run` signature:

```rust
    pub fn run<B: Reader + Writer + Send + 'static, E: HostEffects + 'static>(
        &self,
        host_store: HostStore<B, E>,
    ) -> (HostStore<B, E>, Result<(), String>) {
```

Update all references inside `run`:
- `Linker<AgentState<E>>` → `Linker<AgentState<B, E>>`
- `Caller<'_, AgentState<E>>` → `Caller<'_, AgentState<B, E>>`
- `Store::new(&self.engine, state)` unchanged (wasmtime Store)

Update helper functions:

```rust
fn get_memory<B: Reader + Writer + Send, E: HostEffects>(
    caller: &mut Caller<'_, AgentState<B, E>>,
) -> Option<wasmtime::Memory> { ... }

fn read_guest_string<B: Reader + Writer + Send, E: HostEffects>(
    caller: &Caller<'_, AgentState<B, E>>,
    memory: &wasmtime::Memory,
    ptr: i32,
    len: i32,
) -> Result<String, String> { ... }

fn set_pending<B: Reader + Writer + Send, E: HostEffects>(
    caller: &mut Caller<'_, AgentState<B, E>>,
    bytes: &[u8],
) { ... }
```

- [ ] **Step 3: Update re-exports in ox-runtime/src/lib.rs**

```rust
pub use engine::{AgentModule, AgentRuntime, AgentState};
pub use host_store::{HostEffects, HostStore};
```

No change needed — the re-exports work with the new generics since they're just type names.

- [ ] **Step 4: Update callsite in agents.rs**

In `crates/ox-cli/src/agents.rs`, the `agent_worker` function currently does:

```rust
let host_store = HostStore::new(namespace, effects);
let (returned_store, result) = module.run(host_store);
namespace = returned_store.namespace;
```

Change to:

```rust
let host_store = HostStore::new(namespace, effects);
let (returned_store, result) = module.run(host_store);
namespace = returned_store.backend;
```

Also update `save_thread_state` if it accesses `returned_store.namespace` — change to `returned_store.backend`.

Find all `.namespace` references in agents.rs on the returned HostStore and change to `.backend`:
- Line ~312: `namespace = returned_store.namespace;` → `namespace = returned_store.backend;`

- [ ] **Step 5: Update tests in host_store.rs**

The existing tests use `HostStore::new(ns, MockEffects::new())` where `ns` is a `Namespace`. These should continue to work because `Namespace` implements `Reader + Writer + Send`. The only change:

In `write_tools_execute_calls_effects` test, the assertion reads from `store.handle_read(&result_path)` which now reads from `self.tool_results` instead of `self.namespace`. Behavior is identical — the test passes.

In `write_non_effectful_delegates_to_namespace`, rename mental model but code is unchanged:

```rust
// Writing to history/append should delegate to backend
```

Add a test for prompt interception:

```rust
#[test]
fn read_prompt_synthesizes_from_backend() {
    let ns = make_namespace();
    let mut store = HostStore::new(ns, MockEffects::new());

    // Write a user message
    let msg = serde_json::json!({"role": "user", "content": "hello"});
    let value = structfs_serde_store::json_to_value(msg);
    store.handle_write(&path!("history/append"), Record::parsed(value)).unwrap();

    // Read prompt should synthesize a CompletionRequest
    let result = store.handle_read(&path!("prompt")).unwrap();
    assert!(result.is_some());
    let json = structfs_serde_store::value_to_json(
        result.unwrap().as_value().cloned().unwrap()
    );
    let request: ox_kernel::CompletionRequest =
        serde_json::from_value(json).unwrap();
    assert_eq!(request.model, "test-model");
    assert_eq!(request.system, "You are a test agent.");
    assert_eq!(request.messages.len(), 1);
}
```

- [ ] **Step 6: Run all tests**

Run: `cargo test -p ox-runtime && cargo test -p ox-context`
Expected: All pass.

- [ ] **Step 7: Run ox-cli check (compile only)**

Run: `cargo check -p ox-cli`
Expected: Compiles. (Full test requires agent.wasm which may not be present.)

- [ ] **Step 8: Commit**

```bash
git add crates/ox-runtime/src/host_store.rs crates/ox-runtime/src/engine.rs crates/ox-runtime/src/lib.rs crates/ox-cli/src/agents.rs
git commit -m "refactor(ox-runtime): make HostStore generic over backend type"
```

---

### Task 3: SyncClientAdapter + Clone BrokerStore

Add `SyncClientAdapter` — a sync `Reader`/`Writer` that bridges to an async `ClientHandle` via `tokio::runtime::Handle::block_on()`. Also derive `Clone` on `BrokerStore` so workers can hold their own handle.

**Files:**
- Create: `crates/ox-broker/src/sync_adapter.rs`
- Modify: `crates/ox-broker/src/lib.rs`
- Modify: `crates/ox-broker/Cargo.toml` (add structfs-core-store Reader/Writer to deps — already present)

- [ ] **Step 1: Write tests for SyncClientAdapter**

Create `crates/ox-broker/src/sync_adapter.rs`:

```rust
//! SyncClientAdapter — synchronous Reader/Writer over an async ClientHandle.
//!
//! Used by agent workers running on plain OS threads to read/write through
//! the broker. The adapter holds a `tokio::runtime::Handle` and calls
//! `block_on()` to bridge the sync/async boundary.

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Writer};

use crate::ClientHandle;

/// Synchronous adapter for an async [`ClientHandle`].
///
/// Implements `Reader` and `Writer` by blocking on the handle's async
/// operations. Must be used from a thread that is NOT inside a tokio
/// runtime (e.g., a plain OS thread spawned with `std::thread::spawn`).
pub struct SyncClientAdapter {
    client: ClientHandle,
    handle: tokio::runtime::Handle,
}

impl SyncClientAdapter {
    /// Create a new adapter.
    ///
    /// `client` is the (possibly scoped) broker client.
    /// `handle` is a tokio runtime handle for executing async operations.
    pub fn new(client: ClientHandle, handle: tokio::runtime::Handle) -> Self {
        Self { client, handle }
    }
}

impl Reader for SyncClientAdapter {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        self.handle.block_on(self.client.read(from))
    }
}

impl Writer for SyncClientAdapter {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        self.handle.block_on(self.client.write(to, data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BrokerStore;
    use structfs_core_store::{Value, path};

    #[test]
    fn sync_adapter_reads_from_broker() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        let (broker, _handles) = rt.block_on(async {
            let broker = BrokerStore::default();
            let store = crate::test_support::MemoryStore::with(
                "greeting",
                Value::String("hello".to_string()),
            );
            let h = broker.mount(path!("data"), store).await;
            (broker, vec![h])
        });

        let client = broker.client().scoped("data");
        let mut adapter = SyncClientAdapter::new(client, rt.handle().clone());

        let result = adapter.read(&path!("greeting")).unwrap().unwrap();
        assert_eq!(
            result.as_value().unwrap(),
            &Value::String("hello".to_string()),
        );
    }

    #[test]
    fn sync_adapter_writes_to_broker() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        let (broker, _handles) = rt.block_on(async {
            let broker = BrokerStore::default();
            let store = crate::test_support::MemoryStore::new();
            let h = broker.mount(path!("data"), store).await;
            (broker, vec![h])
        });

        let scoped = broker.client().scoped("data");
        let mut adapter = SyncClientAdapter::new(scoped, rt.handle().clone());

        adapter
            .write(&path!("key"), Record::parsed(Value::Integer(42)))
            .unwrap();

        // Verify via unscoped client
        let full_client = broker.client();
        let result = rt
            .block_on(full_client.read(&path!("data/key")))
            .unwrap()
            .unwrap();
        assert_eq!(result.as_value().unwrap(), &Value::Integer(42));
    }

    #[test]
    fn sync_adapter_no_route_returns_error() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        let broker = BrokerStore::default();
        let client = broker.client().scoped("nonexistent");
        let mut adapter = SyncClientAdapter::new(client, rt.handle().clone());

        let result = adapter.read(&path!("anything"));
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Add Clone to BrokerStore and export sync_adapter**

In `crates/ox-broker/src/lib.rs`:

Add the module declaration:

```rust
mod sync_adapter;
```

Add to the public exports:

```rust
pub use sync_adapter::SyncClientAdapter;
```

Derive Clone on BrokerStore:

```rust
#[derive(Clone)]
pub struct BrokerStore {
    inner: Arc<Mutex<broker::BrokerInner>>,
    default_timeout: Duration,
}
```

Also make `test_support` module `pub` (not just `pub(crate)`) so the sync_adapter tests can access `MemoryStore`. Actually, test_support is already `pub(crate)` and the tests are inside the crate, so it works. No change needed.

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-broker`
Expected: All pass (existing + 3 new sync_adapter tests).

- [ ] **Step 4: Commit**

```bash
git add crates/ox-broker/src/sync_adapter.rs crates/ox-broker/src/lib.rs
git commit -m "feat(ox-broker): add SyncClientAdapter and Clone for BrokerStore"
```

---

### Task 4: Thread mount/unmount

Functions that mount per-thread stores (SystemProvider, HistoryProvider, ToolsProvider, ModelProvider, GateStore) in the broker at `threads/{id}/` prefixes. Snapshot restore writes through the broker client after mounting.

**Files:**
- Create: `crates/ox-cli/src/thread_mount.rs`
- Test: `crates/ox-cli/src/thread_mount.rs` (integration tests)

- [ ] **Step 1: Write integration test**

Create `crates/ox-cli/src/thread_mount.rs`:

```rust
//! Thread mount/unmount — manages per-thread store lifecycle in the broker.

use ox_broker::BrokerStore;
use ox_context::{ModelProvider, SystemProvider, ToolsProvider};
use ox_gate::GateStore;
use ox_history::HistoryProvider;
use ox_kernel::{Record, ToolSchema, Value};
use structfs_core_store::{Path, Writer, path};
use tokio::task::JoinHandle;

/// Configuration for mounting a thread's stores.
pub struct ThreadConfig {
    pub system_prompt: String,
    pub model: String,
    pub max_tokens: u32,
    pub tool_schemas: Vec<ToolSchema>,
    pub provider: String,
    pub api_key: String,
}

/// Handles returned from mounting a thread's stores.
pub struct ThreadMountHandles {
    pub server_handles: Vec<JoinHandle<()>>,
    pub thread_id: String,
}

/// The store prefixes mounted per thread, in order.
const THREAD_STORES: [&str; 5] = ["system", "history", "tools", "model", "gate"];

/// Mount all stores for a thread in the broker.
///
/// Creates: `threads/{thread_id}/system`, `threads/{thread_id}/history`, etc.
/// Returns handles that must be kept alive for the server tasks.
pub async fn mount_thread(
    broker: &BrokerStore,
    thread_id: &str,
    config: ThreadConfig,
) -> Result<ThreadMountHandles, String> {
    let prefix = format!("threads/{thread_id}");
    let mut handles = Vec::new();

    handles.push(
        broker
            .mount(
                Path::parse(&format!("{prefix}/system")).map_err(|e| e.to_string())?,
                SystemProvider::new(config.system_prompt),
            )
            .await,
    );

    handles.push(
        broker
            .mount(
                Path::parse(&format!("{prefix}/history")).map_err(|e| e.to_string())?,
                HistoryProvider::new(),
            )
            .await,
    );

    handles.push(
        broker
            .mount(
                Path::parse(&format!("{prefix}/tools")).map_err(|e| e.to_string())?,
                ToolsProvider::new(config.tool_schemas),
            )
            .await,
    );

    handles.push(
        broker
            .mount(
                Path::parse(&format!("{prefix}/model")).map_err(|e| e.to_string())?,
                ModelProvider::new(config.model, config.max_tokens),
            )
            .await,
    );

    // Set up GateStore with API key
    let mut gate = GateStore::new();
    gate.write(
        &Path::from_components(vec![
            "accounts".to_string(),
            config.provider,
            "key".to_string(),
        ]),
        Record::parsed(Value::String(config.api_key)),
    )
    .ok();
    handles.push(
        broker
            .mount(
                Path::parse(&format!("{prefix}/gate")).map_err(|e| e.to_string())?,
                gate,
            )
            .await,
    );

    Ok(ThreadMountHandles {
        server_handles: handles,
        thread_id: thread_id.to_string(),
    })
}

/// Unmount all stores for a thread from the broker.
pub async fn unmount_thread(broker: &BrokerStore, thread_id: &str) {
    let prefix = format!("threads/{thread_id}");
    for store_name in &THREAD_STORES {
        if let Ok(path) = Path::parse(&format!("{prefix}/{store_name}")) {
            broker.unmount(&path).await;
        }
    }
}

/// Restore a thread's state from its thread directory via the broker client.
///
/// Reads context.json + ledger.jsonl from disk, writes snapshot state and
/// history messages through the broker. Must be called AFTER mount_thread.
pub fn restore_thread_state(
    adapter: &mut ox_broker::SyncClientAdapter,
    inbox_root: &std::path::Path,
    thread_id: &str,
) -> Result<(), String> {
    let thread_dir = inbox_root.join("threads").join(thread_id);
    if !thread_dir.join("context.json").exists() {
        // No saved state — fresh thread
        return Ok(());
    }
    // SyncClientAdapter implements Reader + Writer + Store, so pass it directly.
    // The adapter is scoped to threads/{id}/, so writes to "system/snapshot/state"
    // resolve to "threads/{id}/system/snapshot/state" in the broker.
    ox_inbox::snapshot::restore(adapter, &thread_dir, &ox_inbox::snapshot::PARTICIPATING_MOUNTS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::Reader;

    fn test_config() -> ThreadConfig {
        ThreadConfig {
            system_prompt: "You are helpful.".to_string(),
            model: "test-model".to_string(),
            max_tokens: 1024,
            tool_schemas: vec![],
            provider: "anthropic".to_string(),
            api_key: "sk-test".to_string(),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mount_thread_creates_all_stores() {
        let broker = BrokerStore::default();
        let _handles = mount_thread(&broker, "t_test1", test_config()).await.unwrap();

        let client = broker.client();

        // Read system prompt
        let result = client
            .read(&path!("threads/t_test1/system"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result.as_value().unwrap(),
            &Value::String("You are helpful.".to_string()),
        );

        // Read model
        let result = client
            .read(&path!("threads/t_test1/model/id"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result.as_value().unwrap(),
            &Value::String("test-model".to_string()),
        );

        // Read max_tokens
        let result = client
            .read(&path!("threads/t_test1/model/max_tokens"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.as_value().unwrap(), &Value::Integer(1024));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scoped_client_reads_thread_stores() {
        let broker = BrokerStore::default();
        let _handles = mount_thread(&broker, "t_test2", test_config()).await.unwrap();

        // Agent worker perspective: scoped client
        let agent = broker.client().scoped("threads/t_test2");

        let result = agent.read(&path!("system")).await.unwrap().unwrap();
        assert_eq!(
            result.as_value().unwrap(),
            &Value::String("You are helpful.".to_string()),
        );

        // Write history and read back
        let msg = serde_json::json!({"role": "user", "content": "hello"});
        let value = structfs_serde_store::json_to_value(msg);
        agent
            .write(&path!("history/append"), Record::parsed(value))
            .await
            .unwrap();

        let count = agent.read(&path!("history/count")).await.unwrap().unwrap();
        assert_eq!(count.as_value().unwrap(), &Value::Integer(1));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unmount_thread_removes_all_stores() {
        let broker = BrokerStore::default();
        let _handles = mount_thread(&broker, "t_test3", test_config()).await.unwrap();

        unmount_thread(&broker, "t_test3").await;

        let client = broker.client();
        let result = client.read(&path!("threads/t_test3/system")).await;
        assert!(result.is_err(), "should be NoRoute after unmount");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_adapter_with_mounted_thread() {
        let broker = BrokerStore::default();
        let _handles = mount_thread(&broker, "t_test4", test_config()).await.unwrap();

        let scoped = broker.client().scoped("threads/t_test4");
        let handle = tokio::runtime::Handle::current();

        // Run sync operations from a blocking task (simulates OS thread)
        tokio::task::spawn_blocking(move || {
            let mut adapter = ox_broker::SyncClientAdapter::new(scoped, handle);

            // Read system prompt
            let result = adapter.read(&path!("system")).unwrap().unwrap();
            assert_eq!(
                result.as_value().unwrap(),
                &Value::String("You are helpful.".to_string()),
            );

            // Write + read history
            let msg = serde_json::json!({"role": "user", "content": "test"});
            let value = structfs_serde_store::json_to_value(msg);
            adapter
                .write(&path!("history/append"), Record::parsed(value))
                .unwrap();

            let count = adapter.read(&path!("history/count")).unwrap().unwrap();
            assert_eq!(count.as_value().unwrap(), &Value::Integer(1));

            // Prompt synthesis through adapter
            let prompt = ox_context::synthesize_prompt(&mut adapter).unwrap().unwrap();
            assert!(prompt.as_value().is_some());
        })
        .await
        .unwrap();
    }
}
```

- [ ] **Step 2: Register module in ox-cli**

In `crates/ox-cli/src/main.rs` (or wherever modules are declared), add:

```rust
mod thread_mount;
```

If modules are declared in `lib.rs` or a top-level `mod.rs`, add there instead. Check the existing module declarations in `crates/ox-cli/src/main.rs` and add `mod thread_mount;` alongside the others.

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-cli thread_mount`
Expected: All 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-cli/src/thread_mount.rs crates/ox-cli/src/main.rs
git commit -m "feat(ox-cli): add thread mount/unmount for per-thread broker stores"
```

---

### Task 5: Wire agent_worker through broker

Replace the Namespace construction in `agent_worker()` with broker-mediated stores. The worker thread mounts its stores, gets a scoped client, wraps it in `SyncClientAdapter`, builds `HostStore<SyncClientAdapter, CliEffects>`, and runs the module. On completion, saves state and unmounts.

**Files:**
- Modify: `crates/ox-cli/src/agents.rs`
- Modify: `crates/ox-cli/src/app.rs` (pass broker to AgentPool)
- Modify: `crates/ox-cli/src/broker_setup.rs` (expose broker for sharing)
- Modify: `crates/ox-cli/src/main.rs` (thread broker through)

- [ ] **Step 1: Add broker + rt_handle to AgentPool**

In `crates/ox-cli/src/agents.rs`, modify `AgentPool`:

```rust
pub struct AgentPool {
    module: AgentModule,
    threads: HashMap<String, ThreadHandle>,
    event_tx: mpsc::Sender<AppEvent>,
    control_tx: mpsc::Sender<AppControl>,
    model: String,
    provider: String,
    max_tokens: u32,
    api_key: String,
    workspace: PathBuf,
    no_policy: bool,
    inbox: ox_inbox::InboxStore,
    inbox_root: PathBuf,
    broker: ox_broker::BrokerStore,
    rt_handle: tokio::runtime::Handle,
}
```

Update `AgentPool::new` to accept and store `broker` and `rt_handle`:

```rust
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        model: String,
        provider: String,
        max_tokens: u32,
        api_key: String,
        workspace: PathBuf,
        no_policy: bool,
        inbox: ox_inbox::InboxStore,
        inbox_root: PathBuf,
        event_tx: mpsc::Sender<AppEvent>,
        control_tx: mpsc::Sender<AppControl>,
        broker: ox_broker::BrokerStore,
        rt_handle: tokio::runtime::Handle,
    ) -> Result<Self, String> {
        let runtime = AgentRuntime::new()?;
        let module = runtime.load_module_from_bytes(AGENT_WASM)?;
        Ok(Self {
            module,
            threads: HashMap::new(),
            event_tx,
            control_tx,
            model,
            provider,
            max_tokens,
            api_key,
            workspace,
            no_policy,
            inbox,
            inbox_root,
            broker,
            rt_handle,
        })
    }
```

- [ ] **Step 2: Rewrite spawn_worker to pass broker + handle**

In `crates/ox-cli/src/agents.rs`, modify `spawn_worker`:

```rust
    fn spawn_worker(&mut self, thread_id: String, title: String) {
        let (prompt_tx, prompt_rx) = mpsc::channel::<String>();
        self.threads
            .insert(thread_id.clone(), ThreadHandle { prompt_tx });

        let module = self.module.clone();
        let event_tx = self.event_tx.clone();
        let control_tx = self.control_tx.clone();
        let model = self.model.clone();
        let provider = self.provider.clone();
        let max_tokens = self.max_tokens;
        let api_key = self.api_key.clone();
        let workspace = self.workspace.clone();
        let no_policy = self.no_policy;
        let inbox_root = self.inbox_root.clone();
        let broker = self.broker.clone();
        let rt_handle = self.rt_handle.clone();

        thread::spawn(move || {
            agent_worker(
                thread_id, title, module, model, provider, max_tokens, api_key,
                workspace, no_policy, inbox_root, prompt_rx, event_tx, control_tx,
                broker, rt_handle,
            );
        });
    }
```

- [ ] **Step 3: Rewrite agent_worker to use broker path**

Replace the body of `agent_worker()` in `crates/ox-cli/src/agents.rs`. The new version:

1. Mounts per-thread stores via `thread_mount::mount_thread`
2. Gets a scoped client + SyncClientAdapter
3. Restores snapshot via `thread_mount::restore_thread_state`
4. Also restores legacy JSONL format via the adapter
5. Runs the agent loop with `HostStore<SyncClientAdapter, CliEffects>`
6. Saves state and unmounts on exit

```rust
#[allow(clippy::too_many_arguments)]
fn agent_worker(
    thread_id: String,
    title: String,
    module: AgentModule,
    model: String,
    provider: String,
    max_tokens: u32,
    api_key: String,
    workspace: PathBuf,
    no_policy: bool,
    inbox_root: PathBuf,
    prompt_rx: mpsc::Receiver<String>,
    event_tx: mpsc::Sender<AppEvent>,
    control_tx: mpsc::Sender<AppControl>,
    broker: ox_broker::BrokerStore,
    rt_handle: tokio::runtime::Handle,
) {
    // Build tool registry
    let extra_tools = crate::tools::standard_tools(workspace.clone());
    let mut tools = ToolRegistry::new();
    for tool in extra_tools {
        tools.register(tool);
    }

    let policy = if no_policy {
        crate::policy::PolicyGuard::permissive()
    } else {
        crate::policy::PolicyGuard::load(&workspace)
    };

    // Mount per-thread stores in the broker
    let config = crate::thread_mount::ThreadConfig {
        system_prompt: SYSTEM_PROMPT.to_string(),
        model: model.clone(),
        max_tokens,
        tool_schemas: tools.schemas(),
        provider: provider.clone(),
        api_key: api_key.clone(),
    };

    let _mount_handles = match rt_handle.block_on(
        crate::thread_mount::mount_thread(&broker, &thread_id, config),
    ) {
        Ok(h) => h,
        Err(e) => {
            event_tx
                .send(AppEvent::Done {
                    thread_id: thread_id.clone(),
                    result: Err(format!("mount failed: {e}")),
                })
                .ok();
            return;
        }
    };

    // Get scoped client and sync adapter
    let scoped_client = broker.client().scoped(&format!("threads/{thread_id}"));
    let mut adapter = ox_broker::SyncClientAdapter::new(scoped_client, rt_handle.clone());

    // Restore conversation state from thread directory
    let thread_dir = inbox_root.join("threads").join(&thread_id);
    if thread_dir.join("context.json").exists() {
        crate::thread_mount::restore_thread_state(&mut adapter, &inbox_root, &thread_id).ok();
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
                        adapter
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

    // Read provider config from gate store for transport
    let provider_config = {
        let gate_path = ox_kernel::Path::parse("gate/accounts")
            .ok()
            .and_then(|p| {
                adapter.read(&p).ok().flatten().and_then(|r| {
                    r.as_value().cloned()
                })
            });
        // Fall back to default config
        crate::app::provider_config_for(&provider)
    };
    let api_key_for_transport = api_key.clone();

    // Register completion tools
    let send_config = provider_config.clone();
    let send_key = api_key_for_transport.clone();
    let send = Arc::new(crate::transport::make_send_fn(send_config, send_key));
    // Re-read gate store through broker to create completion tools
    let mut gate_for_tools = GateStore::new();
    for tool in gate_for_tools.create_completion_tools(send) {
        tools.register(tool);
    }

    // Ownership ping-pong state
    let mut tools = tools;
    let mut policy = policy;
    let mut client = reqwest::blocking::Client::new();

    while let Ok(input) = prompt_rx.recv() {
        // Write user message to history through the adapter
        let user_json = serde_json::json!({"role": "user", "content": input});
        if let Err(e) = adapter.write(
            &path!("history/append"),
            Record::parsed(json_to_value(user_json)),
        ) {
            event_tx
                .send(AppEvent::Done {
                    thread_id: thread_id.clone(),
                    result: Err::<String, _>(e.to_string()),
                })
                .ok();
            continue;
        }

        let effects = CliEffects {
            thread_id: thread_id.clone(),
            client,
            config: provider_config.clone(),
            api_key: api_key_for_transport.clone(),
            tools,
            policy,
            event_tx: event_tx.clone(),
            control_tx: control_tx.clone(),
            stats: PolicyStats::default(),
        };

        let host_store = HostStore::new(adapter, effects);
        let (returned_store, result) = module.run(host_store);

        adapter = returned_store.backend;
        client = returned_store.effects.client;
        tools = returned_store.effects.tools;
        let stats = returned_store.effects.stats.clone();
        policy = returned_store.effects.policy;

        // Persist conversation state
        save_thread_state(&mut adapter, &inbox_root, &thread_id, &title, &event_tx);

        event_tx
            .send(AppEvent::PolicyStats {
                thread_id: thread_id.clone(),
                stats,
            })
            .ok();

        let done_result = match result {
            Ok(()) => Ok(String::new()),
            Err(e) => Err(e),
        };
        event_tx
            .send(AppEvent::Done {
                thread_id: thread_id.clone(),
                result: done_result,
            })
            .ok();
    }

    // Unmount on worker exit
    rt_handle.block_on(crate::thread_mount::unmount_thread(&broker, &thread_id));
}
```

- [ ] **Step 4: Update save_thread_state to use adapter**

Change the signature from `namespace: &mut Namespace` to `adapter: &mut dyn structfs_core_store::Store`:

```rust
fn save_thread_state(
    store: &mut dyn structfs_core_store::Store,
    inbox_root: &std::path::Path,
    thread_id: &str,
    title: &str,
    event_tx: &mpsc::Sender<AppEvent>,
) {
    let thread_dir = inbox_root.join("threads").join(thread_id);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let result = ox_inbox::snapshot::save(
        store,
        &thread_dir,
        thread_id,
        title,
        &[],
        now,
        &ox_inbox::snapshot::PARTICIPATING_MOUNTS,
    );

    if let Ok(save_result) = result {
        event_tx
            .send(AppEvent::SaveComplete {
                thread_id: thread_id.to_string(),
                last_seq: save_result.last_seq,
                last_hash: save_result.last_hash,
                updated_at: now,
            })
            .ok();
    }
}
```

- [ ] **Step 5: Update App::new to pass broker**

In `crates/ox-cli/src/app.rs`, modify `App::new` to accept `broker: ox_broker::BrokerStore` and pass it to `AgentPool::new`:

Find the `App::new` constructor. Add `broker: ox_broker::BrokerStore` parameter. Pass `broker` and `tokio::runtime::Handle::current()` to `AgentPool::new`.

Note: `App::new` is called from the async context in `main.rs`, so `tokio::runtime::Handle::current()` works. If App::new is not called from async context, pass `rt_handle` as a parameter instead.

- [ ] **Step 6: Update broker_setup.rs to share broker**

In `crates/ox-cli/src/broker_setup.rs`, ensure `BrokerHandle` exposes the broker for cloning:

```rust
impl BrokerHandle {
    pub fn client(&self) -> ox_broker::ClientHandle {
        self.broker.client()
    }

    pub fn broker(&self) -> &ox_broker::BrokerStore {
        &self.broker
    }
}
```

- [ ] **Step 7: Update main.rs to thread broker through**

In `crates/ox-cli/src/main.rs`, find where `App::new` is called and pass `broker_handle.broker().clone()`.

- [ ] **Step 8: Add helper function for provider config**

In `crates/ox-cli/src/app.rs`, add a public helper if one doesn't exist:

```rust
/// Get provider config for a given provider name.
pub fn provider_config_for(provider: &str) -> ox_gate::ProviderConfig {
    match provider {
        "openai" => ox_gate::ProviderConfig::openai(),
        _ => ox_gate::ProviderConfig::anthropic(),
    }
}
```

If `read_provider_config_from_gate` is already public, use that instead. The point is that the worker needs to get the provider config without directly owning a GateStore (it's in the broker now).

- [ ] **Step 9: Compile check**

Run: `cargo check -p ox-cli`
Expected: Compiles clean.

- [ ] **Step 10: Run all workspace tests**

Run: `cargo test --workspace`
Expected: All pass. If agent.wasm isn't present, the integration test in ox-runtime skips (that's OK).

- [ ] **Step 11: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: All 14 gates pass.

- [ ] **Step 12: Commit**

```bash
git add crates/ox-cli/src/agents.rs crates/ox-cli/src/app.rs crates/ox-cli/src/broker_setup.rs crates/ox-cli/src/main.rs
git commit -m "feat(ox-cli): wire agent workers through broker with scoped ClientHandle"
```

---

### Task 6: Update status document

**Files:**
- Modify: `docs/design/rfc/structfs-tui-status.md`

- [ ] **Step 1: Update status doc**

Add C4 section under "Phase C: StructFS TUI Rewrite":

```markdown
#### C4: Agent Worker Bridge (complete, N tests)
- `crates/ox-broker/src/sync_adapter.rs` — SyncClientAdapter (sync Reader/Writer over async ClientHandle)
- `crates/ox-cli/src/thread_mount.rs` — mount/unmount per-thread stores, snapshot restore via broker
- `crates/ox-context/src/lib.rs` — extracted `synthesize_prompt()` standalone function
- `crates/ox-runtime/src/host_store.rs` — HostStore<B, E> generic over backend
- `crates/ox-cli/src/agents.rs` — agent_worker uses scoped ClientHandle through broker
- BrokerStore derives Clone for sharing across threads
```

Update "What's Next" to remove "Agent Worker Bridge" and move "Draw Rewrite" to highest priority.

- [ ] **Step 2: Commit**

```bash
git add docs/design/rfc/structfs-tui-status.md
git commit -m "docs: update status/handoff for C4 completion"
```

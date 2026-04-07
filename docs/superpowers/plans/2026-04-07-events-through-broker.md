# Events Through Broker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route all agent streaming events and approval flow through the broker, eliminating AppEvent/mpsc channels and all parallel state.

**Architecture:** Add AsyncReader/AsyncWriter traits to ox-broker for stores that need deferred replies. Rewrite ApprovalStore as async (deferred write blocks until response). CliEffects writes to `turn/*` paths and `approval/request` instead of mpsc. ViewState reads everything from broker. AppEvent, event_rx, control_rx, thread_views, streaming_turns, handle_event, drain_agent_events — all deleted.

**Tech Stack:** Rust, tokio (oneshot for deferred replies), ox-broker, ox-ui, ox-cli, structfs-core-store

**Spec:** `docs/superpowers/specs/2026-04-07-events-through-broker-design.md`

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/ox-broker/src/async_store.rs` | Create | AsyncReader, AsyncWriter traits, BoxFuture type alias |
| `crates/ox-broker/src/server.rs` | Modify | Add async_server_loop, spawn_async_server |
| `crates/ox-broker/src/lib.rs` | Modify | Add mount_async method, export async_store module |
| `crates/ox-ui/src/approval_store.rs` | Modify | Rewrite as AsyncReader + AsyncWriter with deferred write |
| `crates/ox-ui/src/lib.rs` | Modify | Update exports |
| `crates/ox-ui/Cargo.toml` | Modify | Add tokio dependency for oneshot |
| `crates/ox-cli/src/thread_mount.rs` | Modify | Mount async ApprovalStore per-thread |
| `crates/ox-cli/src/agents.rs` | Modify | CliEffects uses broker writes, remove event_tx/control_tx |
| `crates/ox-cli/src/app.rs` | Modify | Remove AppEvent, AppControl, thread_views, streaming_turns, event_rx, control_rx, handle_event, drain_agent_events, pending_approval |
| `crates/ox-cli/src/view_state.rs` | Modify | Read turn/*, approval/pending from broker; remove thread_views reference |
| `crates/ox-cli/src/tui.rs` | Modify | Remove drain_agent_events, control_rx polling; approval dialog reads from ViewState, writes response through broker |
| `crates/ox-cli/src/main.rs` | Modify | Remove event/control channel creation |

---

### Task 1: Broker Async Store Support

Add `AsyncReader` and `AsyncWriter` traits to ox-broker. These are broker-internal — not exported to ox-kernel or any public StructFS interface. The async server loop calls store methods sequentially but spawns write futures as independent tasks (for deferred replies).

**Files:**
- Create: `crates/ox-broker/src/async_store.rs`
- Modify: `crates/ox-broker/src/server.rs`
- Modify: `crates/ox-broker/src/lib.rs`

- [ ] **Step 1: Write tests for async store mounting**

Add to `crates/ox-broker/src/lib.rs` integration tests:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mount_async_store_reads_and_writes() {
    use crate::async_store::{AsyncReader, AsyncWriter, BoxFuture};

    struct SimpleAsync {
        data: std::collections::BTreeMap<String, Value>,
    }

    impl AsyncReader for SimpleAsync {
        fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
            let result = Ok(self.data.get(&from.to_string()).map(|v| Record::parsed(v.clone())));
            Box::pin(std::future::ready(result))
        }
    }

    impl AsyncWriter for SimpleAsync {
        fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
            if let Some(value) = data.as_value() {
                self.data.insert(to.to_string(), value.clone());
            }
            Box::pin(std::future::ready(Ok(to.clone())))
        }
    }

    let broker = BrokerStore::default();
    let mut store = SimpleAsync { data: std::collections::BTreeMap::new() };
    store.data.insert("greeting".to_string(), Value::String("hello".to_string()));

    broker.mount_async(path!("async_test"), store).await;

    let client = broker.client();
    let result = client.read(&path!("async_test/greeting")).await.unwrap().unwrap();
    assert_eq!(result.as_value().unwrap(), &Value::String("hello".to_string()));

    client.write(&path!("async_test/name"), Record::parsed(Value::String("world".to_string()))).await.unwrap();
    let result = client.read(&path!("async_test/name")).await.unwrap().unwrap();
    assert_eq!(result.as_value().unwrap(), &Value::String("world".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_store_deferred_write() {
    use crate::async_store::{AsyncReader, AsyncWriter, BoxFuture};
    use tokio::sync::oneshot;
    use std::sync::{Arc, Mutex};

    /// A store where write("block") defers until unblock() is called.
    struct DeferredStore {
        sender: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    }

    impl AsyncReader for DeferredStore {
        fn read(&mut self, _from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
            Box::pin(std::future::ready(Ok(None)))
        }
    }

    impl AsyncWriter for DeferredStore {
        fn write(&mut self, to: &Path, _data: Record) -> BoxFuture<Result<Path, StoreError>> {
            if to.to_string() == "block" {
                let (tx, rx) = oneshot::channel();
                *self.sender.lock().unwrap() = Some(tx);
                let path = to.clone();
                Box::pin(async move {
                    let _ = rx.await;
                    Ok(path)
                })
            } else if to.to_string() == "unblock" {
                if let Some(tx) = self.sender.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                Box::pin(std::future::ready(Ok(to.clone())))
            } else {
                Box::pin(std::future::ready(Ok(to.clone())))
            }
        }
    }

    let sender = Arc::new(Mutex::new(None));
    let broker = BrokerStore::default();
    broker.mount_async(path!("deferred"), DeferredStore { sender: sender.clone() }).await;

    let client1 = broker.client();
    let client2 = broker.client();

    // Spawn a task that writes "block" — it should not resolve yet
    let blocked = tokio::spawn(async move {
        client1.write(&path!("deferred/block"), Record::parsed(Value::Null)).await
    });

    // Give it a moment to register
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!blocked.is_finished(), "blocked write should still be pending");

    // Unblock it
    client2.write(&path!("deferred/unblock"), Record::parsed(Value::Null)).await.unwrap();

    // Now the blocked write should resolve
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        blocked,
    ).await.unwrap().unwrap();
    assert!(result.is_ok());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ox-broker mount_async`
Expected: FAIL — `async_store` module and `mount_async` don't exist.

- [ ] **Step 3: Create async_store.rs with traits**

Create `crates/ox-broker/src/async_store.rs`:

```rust
//! Async store traits — broker-internal, not a StructFS primitive.
//!
//! Stores implementing these traits can return futures that resolve
//! later (deferred replies). The broker's async server loop spawns
//! write futures as independent tasks so a deferred write doesn't
//! block the store from handling other requests.
//!
//! SCOPE: These traits live in ox-broker only. They are NOT exported
//! by ox-kernel, ox-core, or any public ox crate. Wasm modules and
//! the StructFS interface use synchronous Reader/Writer exclusively.

use std::future::Future;
use std::pin::Pin;

use structfs_core_store::{Error as StoreError, Path, Record};

/// A boxed, Send, 'static future. Used as the return type for async
/// store operations so the broker can spawn them as tasks.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Async version of structfs Reader. Returns a boxed future.
///
/// Reads are typically fast (resolve immediately). The async signature
/// exists for consistency with AsyncWriter.
pub trait AsyncReader: Send + 'static {
    fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>>;
}

/// Async version of structfs Writer. Returns a boxed future.
///
/// The store produces the future synchronously (`&mut self`), setting
/// up internal state (e.g., creating a oneshot channel). The returned
/// future is `'static + Send` — it does NOT borrow the store. It
/// resolves independently, possibly much later (deferred reply).
///
/// For fast writes: return `Box::pin(std::future::ready(Ok(path)))`.
/// For deferred writes: return `Box::pin(async { receiver.await... })`.
pub trait AsyncWriter: Send + 'static {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>>;
}
```

- [ ] **Step 4: Add async server loop to server.rs**

In `crates/ox-broker/src/server.rs`, add:

```rust
use crate::async_store::{AsyncReader, AsyncWriter};

/// Spawn an async server task that handles deferred write futures.
///
/// Reads are resolved inline (fast). Writes are spawned as independent
/// tasks so a deferred write doesn't block the server from handling
/// subsequent requests.
pub(crate) async fn spawn_async_server<S: AsyncReader + AsyncWriter>(
    inner: Arc<Mutex<BrokerInner>>,
    prefix: structfs_core_store::Path,
    store: S,
) -> tokio::task::JoinHandle<()> {
    let rx = {
        let mut inner_guard = inner.lock().await;
        inner_guard.mount(prefix)
    };

    tokio::spawn(async move {
        async_server_loop(store, rx).await;
    })
}

/// Async server loop: reads inline, writes spawned.
async fn async_server_loop<S: AsyncReader + AsyncWriter>(
    mut store: S,
    mut rx: tokio::sync::mpsc::Receiver<Request>,
) {
    while let Some(request) = rx.recv().await {
        match request {
            Request::Read { path, reply } => {
                let result = store.read(&path).await;
                let _ = reply.send(result);
            }
            Request::Write { path, data, reply } => {
                let fut = store.write(&path, data);
                tokio::spawn(async move {
                    let result = fut.await;
                    let _ = reply.send(result);
                });
            }
        }
    }
}
```

- [ ] **Step 5: Add mount_async to BrokerStore**

In `crates/ox-broker/src/lib.rs`, add module declaration and method:

```rust
pub mod async_store;
```

And on BrokerStore:

```rust
/// Mount an async store at the given prefix.
///
/// Async stores can return deferred write futures that resolve later.
/// Used for stores that need cross-client coordination (e.g., approval
/// flow where one client's write blocks until another client responds).
pub async fn mount_async<S: async_store::AsyncReader + async_store::AsyncWriter>(
    &self,
    prefix: structfs_core_store::Path,
    store: S,
) -> tokio::task::JoinHandle<()> {
    server::spawn_async_server(self.inner.clone(), prefix, store).await
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p ox-broker`
Expected: All pass (existing + 2 new async tests).

- [ ] **Step 7: Commit**

```bash
git add crates/ox-broker/src/async_store.rs crates/ox-broker/src/server.rs crates/ox-broker/src/lib.rs
git commit -m "feat(ox-broker): add AsyncReader/AsyncWriter traits and mount_async"
```

---

### Task 2: Async ApprovalStore + Per-Thread Mount

Rewrite ApprovalStore to implement AsyncReader + AsyncWriter. Write to `request` creates a oneshot channel and returns a future that blocks until `response` is written. Mount it per-thread in thread_mount.

**Files:**
- Modify: `crates/ox-ui/src/approval_store.rs`
- Modify: `crates/ox-ui/src/lib.rs`
- Modify: `crates/ox-ui/Cargo.toml`
- Modify: `crates/ox-cli/src/thread_mount.rs`

- [ ] **Step 1: Add tokio dependency to ox-ui**

In `crates/ox-ui/Cargo.toml`, add:

```toml
tokio = { workspace = true }
```

- [ ] **Step 2: Rewrite ApprovalStore**

Replace `crates/ox-ui/src/approval_store.rs` with an async implementation.

The key design: `write("request")` stores the request data + a `oneshot::Sender<String>` in the store's state, and returns a future that awaits the corresponding `oneshot::Receiver<String>`. `write("response")` sends the decision string on the stored sender, resolving the deferred future. `read("pending")` returns the stored request data or Null.

```rust
//! ApprovalStore — async store for the permission approval flow.
//!
//! Implements AsyncReader + AsyncWriter (broker-internal traits).
//! Write to "request" returns a deferred future that blocks until
//! a separate write to "response" provides the decision.

use ox_broker::async_store::{AsyncReader, AsyncWriter, BoxFuture};
use structfs_core_store::{Error as StoreError, Path, Record, Value, path};
use std::collections::BTreeMap;

/// An approval request pending user decision.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub tool_name: String,
    pub input_preview: String,
}

pub struct ApprovalStore {
    pending: Option<ApprovalRequest>,
    /// Sender for the deferred request write. When response is written,
    /// we send the decision string here, resolving the blocked future.
    deferred_tx: Option<tokio::sync::oneshot::Sender<String>>,
}

impl ApprovalStore {
    pub fn new() -> Self {
        Self {
            pending: None,
            deferred_tx: None,
        }
    }
}

impl Default for ApprovalStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncReader for ApprovalStore {
    fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let key = if from.is_empty() {
            ""
        } else {
            from.components[0].as_str()
        };

        let result = match key {
            "pending" => {
                match &self.pending {
                    Some(req) => {
                        let mut map = BTreeMap::new();
                        map.insert(
                            "tool_name".to_string(),
                            Value::String(req.tool_name.clone()),
                        );
                        map.insert(
                            "input_preview".to_string(),
                            Value::String(req.input_preview.clone()),
                        );
                        Ok(Some(Record::parsed(Value::Map(map))))
                    }
                    None => Ok(Some(Record::parsed(Value::Null))),
                }
            }
            "response" => {
                // Response is consumed by the deferred future, not readable
                Ok(Some(Record::parsed(Value::Null)))
            }
            _ => Ok(None),
        };

        Box::pin(std::future::ready(result))
    }
}

impl AsyncWriter for ApprovalStore {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
        let key = if to.is_empty() {
            ""
        } else {
            to.components[0].as_str()
        };

        match key {
            "request" => {
                // Parse the request
                let value = match data.as_value() {
                    Some(Value::Map(m)) => m,
                    _ => {
                        return Box::pin(std::future::ready(Err(StoreError::store(
                            "approval",
                            "write",
                            "expected map with tool_name and input_preview",
                        ))));
                    }
                };

                let tool_name = match value.get("tool_name") {
                    Some(Value::String(s)) => s.clone(),
                    _ => {
                        return Box::pin(std::future::ready(Err(StoreError::store(
                            "approval",
                            "write",
                            "missing tool_name",
                        ))));
                    }
                };
                let input_preview = match value.get("input_preview") {
                    Some(Value::String(s)) => s.clone(),
                    _ => String::new(),
                };

                // Store the request and create the deferred channel
                self.pending = Some(ApprovalRequest {
                    tool_name,
                    input_preview,
                });

                let (tx, rx) = tokio::sync::oneshot::channel::<String>();
                self.deferred_tx = Some(tx);

                // Return a future that blocks until response arrives
                let path = to.clone();
                Box::pin(async move {
                    match rx.await {
                        Ok(_decision) => Ok(path),
                        Err(_) => Err(StoreError::store(
                            "approval",
                            "write",
                            "approval request cancelled (receiver dropped)",
                        )),
                    }
                })
            }
            "response" => {
                // Send the decision to the deferred request future
                let decision = match data.as_value() {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Map(m)) => {
                        // For custom nodes, serialize as JSON string
                        serde_json::to_string(
                            &structfs_serde_store::value_to_json(Value::Map(m.clone())),
                        )
                        .unwrap_or_default()
                    }
                    _ => "deny_once".to_string(),
                };

                if let Some(tx) = self.deferred_tx.take() {
                    let _ = tx.send(decision);
                }
                self.pending = None;

                Box::pin(std::future::ready(Ok(to.clone())))
            }
            _ => Box::pin(std::future::ready(Err(StoreError::store(
                "approval",
                "write",
                format!("unknown path: {to}"),
            )))),
        }
    }
}
```

- [ ] **Step 3: Update ox-ui exports**

In `crates/ox-ui/src/lib.rs`, the ApprovalStore export stays the same. If the old sync ApprovalStore was re-exported, update the import. The type name doesn't change — just the trait impls.

Check if ox-ui/lib.rs re-exports ApprovalStore and ApprovalRequest. Update as needed.

- [ ] **Step 4: Update broker_setup.rs to use mount_async for ApprovalStore**

In `crates/ox-cli/src/broker_setup.rs`, the global ApprovalStore mount:

Change from:
```rust
servers.push(broker.mount(path!("approval"), ApprovalStore::new()).await);
```
to:
```rust
servers.push(broker.mount_async(path!("approval"), ApprovalStore::new()).await);
```

This is the global approval store. Per-thread mounts come next.

- [ ] **Step 5: Mount per-thread ApprovalStore in thread_mount.rs**

In `crates/ox-cli/src/thread_mount.rs`, add an ApprovalStore mount per thread:

After the gate mount, add:
```rust
handles.push(
    broker
        .mount_async(
            Path::parse(&format!("{prefix}/approval")).map_err(|e| e.to_string())?,
            ox_ui::ApprovalStore::new(),
        )
        .await,
);
```

Update `THREAD_STORES` to include "approval":
```rust
const THREAD_STORES: [&str; 6] = ["system", "history", "tools", "model", "gate", "approval"];
```

- [ ] **Step 6: Write test for async approval flow through broker**

Add to `crates/ox-cli/src/thread_mount.rs` tests:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_deferred_write_through_broker() {
    let broker = BrokerStore::default();
    let _handles = mount_thread(&broker, "t_approval", test_config()).await.unwrap();

    let agent_client = broker.client().scoped("threads/t_approval");
    let tui_client = broker.client().scoped("threads/t_approval");

    // Agent writes approval request (should block)
    let agent_task = tokio::spawn(async move {
        let mut map = std::collections::BTreeMap::new();
        map.insert("tool_name".to_string(), Value::String("bash".to_string()));
        map.insert("input_preview".to_string(), Value::String("rm -rf /".to_string()));
        agent_client
            .write(&path!("approval/request"), Record::parsed(Value::Map(map)))
            .await
    });

    // Give it time to register
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!agent_task.is_finished(), "agent should be blocked");

    // TUI reads pending
    let pending = tui_client.read(&path!("approval/pending")).await.unwrap().unwrap();
    let val = pending.as_value().unwrap();
    match val {
        Value::Map(m) => {
            assert_eq!(m.get("tool_name").unwrap(), &Value::String("bash".to_string()));
        }
        _ => panic!("expected map"),
    }

    // TUI writes response
    tui_client
        .write(
            &path!("approval/response"),
            Record::parsed(Value::String("allow_once".to_string())),
        )
        .await
        .unwrap();

    // Agent's write should now resolve
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        agent_task,
    ).await.unwrap().unwrap();
    assert!(result.is_ok());
}
```

- [ ] **Step 7: Run tests**

Run: `cargo test -p ox-broker && cargo test -p ox-cli thread_mount`
Expected: All pass.

- [ ] **Step 8: Commit**

```bash
git add crates/ox-ui/src/approval_store.rs crates/ox-ui/src/lib.rs crates/ox-ui/Cargo.toml crates/ox-cli/src/broker_setup.rs crates/ox-cli/src/thread_mount.rs
git commit -m "feat(ox-ui): rewrite ApprovalStore as async with deferred write"
```

---

### Task 3: CliEffects Broker Writes

Replace `event_tx` and `control_tx` in CliEffects with broker writes. Add `scoped_client` (for turn/* and approval writes) and `broker_client` (for inbox writes). The agent_worker creates both clients and passes them to CliEffects.

**Files:**
- Modify: `crates/ox-cli/src/agents.rs`
- Modify: `crates/ox-cli/src/app.rs` (remove event_tx/control_tx from AgentPool)

- [ ] **Step 1: Update CliEffects struct**

In `crates/ox-cli/src/agents.rs`, change CliEffects:

Remove:
- `event_tx: mpsc::Sender<AppEvent>`
- `control_tx: mpsc::Sender<AppControl>`

Add:
- `scoped_client: ox_broker::ClientHandle` — scoped to `threads/{thread_id}`, for turn/* and approval writes
- `broker_client: ox_broker::ClientHandle` — unscoped, for inbox writes
- `rt_handle: tokio::runtime::Handle` — for block_on

- [ ] **Step 2: Rewrite emit_event**

Replace the HostEffects::emit_event impl:

```rust
fn emit_event(&mut self, event: AgentEvent) {
    match event {
        AgentEvent::TurnStart => {
            self.rt_handle.block_on(
                self.scoped_client.write(
                    &path!("history/turn/thinking"),
                    Record::parsed(Value::Bool(true)),
                )
            ).ok();
        }
        AgentEvent::TextDelta(text) => {
            self.rt_handle.block_on(
                self.scoped_client.write(
                    &path!("history/turn/streaming"),
                    Record::parsed(Value::String(text)),
                )
            ).ok();
        }
        AgentEvent::ToolCallStart { name } => {
            let mut map = std::collections::BTreeMap::new();
            map.insert("name".to_string(), Value::String(name));
            map.insert("status".to_string(), Value::String("running".to_string()));
            self.rt_handle.block_on(
                self.scoped_client.write(
                    &path!("history/turn/tool"),
                    Record::parsed(Value::Map(map)),
                )
            ).ok();
        }
        AgentEvent::ToolCallResult { .. } => {
            self.rt_handle.block_on(
                self.scoped_client.write(
                    &path!("history/turn/tool"),
                    Record::parsed(Value::Null),
                )
            ).ok();
        }
        AgentEvent::TurnEnd => {
            self.rt_handle.block_on(
                self.scoped_client.write(
                    &path!("history/turn/thinking"),
                    Record::parsed(Value::Bool(false)),
                )
            ).ok();
        }
        AgentEvent::Error(e) => {
            // Append error as a message, clear thinking
            let msg = serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": format!("error: {e}")}]});
            self.rt_handle.block_on(
                self.scoped_client.write(
                    &path!("history/append"),
                    Record::parsed(structfs_serde_store::json_to_value(msg)),
                )
            ).ok();
            self.rt_handle.block_on(
                self.scoped_client.write(
                    &path!("history/turn/thinking"),
                    Record::parsed(Value::Bool(false)),
                )
            ).ok();
        }
    }
}
```

- [ ] **Step 3: Rewrite complete() streaming callback**

In CliEffects::complete(), the streaming callback currently sends TextDelta via event_tx. Replace with broker write:

```rust
let scoped = self.scoped_client.clone();
let handle = self.rt_handle.clone();
let (events, usage) = crate::transport::streaming_fetch(
    &self.client,
    &self.config,
    &self.api_key,
    request,
    &|event| {
        if let StreamEvent::TextDelta(text) = event {
            handle.block_on(
                scoped.write(
                    &path!("history/turn/streaming"),
                    Record::parsed(Value::String(text.clone())),
                )
            ).ok();
        }
    },
)?;
```

After the fetch, write usage:
```rust
if usage.input_tokens > 0 || usage.output_tokens > 0 {
    let mut map = std::collections::BTreeMap::new();
    map.insert("in".to_string(), Value::Integer(usage.input_tokens as i64));
    map.insert("out".to_string(), Value::Integer(usage.output_tokens as i64));
    self.rt_handle.block_on(
        self.scoped_client.write(
            &path!("history/turn/tokens"),
            Record::parsed(Value::Map(map)),
        )
    ).ok();
}
```

- [ ] **Step 4: Rewrite execute_tool approval flow**

In CliEffects::execute_tool(), for the `Ask` case, replace the mpsc pattern with a broker write:

```rust
crate::policy::CheckResult::Ask { tool, input_preview, .. } => {
    self.stats.asked += 1;
    // Write approval request through broker — blocks until TUI responds
    let mut req_map = std::collections::BTreeMap::new();
    req_map.insert("tool_name".to_string(), Value::String(tool));
    req_map.insert("input_preview".to_string(), Value::String(input_preview));
    let approval_result = self.rt_handle.block_on(
        self.scoped_client.write(
            &path!("approval/request"),
            Record::parsed(Value::Map(req_map)),
        )
    );

    // If write succeeded, the response has been written — read it
    if approval_result.is_ok() {
        let response = self.rt_handle.block_on(
            self.scoped_client.read(&path!("approval/response"))
        ).ok().flatten().and_then(|r| r.as_value().cloned());

        let decision = match response {
            Some(Value::String(s)) => s,
            _ => "deny_once".to_string(),
        };

        match decision.as_str() {
            "allow_once" => {
                self.stats.allowed += 1;
                self.execute_tool_inner(call)
            }
            "allow_session" => {
                self.policy.session_allow(&call.name, &call.input);
                self.stats.allowed += 1;
                self.execute_tool_inner(call)
            }
            "allow_always" => {
                self.policy.persist_allow(&call.name, &call.input);
                self.stats.allowed += 1;
                self.execute_tool_inner(call)
            }
            "deny_session" => {
                self.policy.session_deny(&call.name, &call.input);
                self.stats.denied += 1;
                Err("denied by user".into())
            }
            "deny_always" => {
                self.policy.persist_deny(&call.name, &call.input);
                self.stats.denied += 1;
                Err("denied by user".into())
            }
            _ => {
                // deny_once or unknown
                self.stats.denied += 1;
                Err("denied by user".into())
            }
        }
    } else {
        self.stats.denied += 1;
        Err("denied: approval timeout or error".into())
    }
}
```

Note: CustomNode handling is deferred — the TUI writes a JSON-encoded custom node as the response string. Parse it back if needed. For now, the TUI can write the decision string ("allow_once", "deny_once", etc.) for standard responses.

- [ ] **Step 5: Update agent_worker to create and pass broker clients**

In agent_worker, after creating the scoped adapter:

```rust
let scoped_client = broker.client().scoped(&format!("threads/{thread_id}"));
let broker_client = broker.client(); // unscoped for inbox writes
```

When creating CliEffects:
```rust
let effects = CliEffects {
    thread_id: thread_id.clone(),
    client,
    config: provider_config.clone(),
    api_key: api_key_for_transport.clone(),
    tools,
    policy,
    scoped_client: scoped_client_for_effects,
    broker_client: broker_client.clone(),
    rt_handle: rt_handle.clone(),
    stats: PolicyStats::default(),
};
```

After module.run returns, write commit and inbox updates:
```rust
// Commit the turn (finalizes streaming text → committed message)
adapter.write(&path!("history/commit"), Record::parsed(Value::Null)).ok();

// Save thread state (snapshot to disk)
save_thread_state(&mut adapter, &inbox_root, &thread_id, &title);

// Write save results to inbox through unscoped broker client
// (replaces the old SaveComplete event)
if let Ok(save_result) = save_result_opt {
    let mut update = std::collections::BTreeMap::new();
    update.insert("last_seq".to_string(), Value::Integer(save_result.last_seq));
    if let Some(hash) = &save_result.last_hash {
        update.insert("last_hash".to_string(), Value::String(hash.clone()));
    }
    update.insert("updated_at".to_string(), Value::Integer(now));
    let update_path = ox_kernel::Path::from_components(vec![
        "inbox".to_string(),
        "threads".to_string(),
        thread_id.clone(),
    ]);
    rt_handle.block_on(broker_client.write(&update_path, Record::parsed(Value::Map(update)))).ok();
}
```

- [ ] **Step 6: Remove event_tx/control_tx from AgentPool**

In `crates/ox-cli/src/agents.rs`, remove `event_tx` and `control_tx` fields from AgentPool. Remove them from `AgentPool::new` parameters. Remove from `spawn_worker` clones.

In `crates/ox-cli/src/app.rs`, remove event_tx/control_tx creation and passing to AgentPool::new.

- [ ] **Step 7: Compile check**

Run: `cargo check -p ox-cli`
Expected: Errors in tui.rs (drain_agent_events, control_rx, etc. still referenced). That's expected — Task 4 cleans those up.

Actually, we need everything to compile. So we must also update App::new and the event loop in this task. See Task 4.

- [ ] **Step 8: Commit (combined with Task 4 if needed)**

Defer commit until Task 4 is done (they must compile together).

---

### Task 4: ViewState + Event Loop + App Cleanup

Update ViewState to read turn/* and approval/pending from broker. Remove all mpsc-related code from App and the event loop. This task MUST be done together with Task 3 (they compile as one unit).

**Files:**
- Modify: `crates/ox-cli/src/view_state.rs`
- Modify: `crates/ox-cli/src/tui.rs`
- Modify: `crates/ox-cli/src/app.rs`
- Modify: `crates/ox-cli/src/main.rs`

- [ ] **Step 1: Update ViewState struct**

In `crates/ox-cli/src/view_state.rs`:

Remove:
- `pub thread_views: &'a HashMap<String, ThreadView>`
- `pub committed_messages: Vec<ChatMessage>`
- `pub pending_approval: &'a Option<ApprovalState>`
- `pub pending_customize: &'a Option<CustomizeState>`

Add:
- `pub messages: Vec<ChatMessage>` — from broker (committed + in-progress turn)
- `pub thinking: bool` — from broker `turn/thinking`
- `pub tool_status: Option<(String, String)>` — from broker `turn/tool`
- `pub turn_tokens: (u32, u32)` — from broker `turn/tokens`
- `pub approval_pending: Option<ApprovalRequest>` — from broker `approval/pending`

Keep `pending_customize: &'a Option<CustomizeState>` if customize dialog state stays in App for now.

- [ ] **Step 2: Update fetch_view_state**

In the thread screen branch, add turn/* and approval reads:

```rust
if screen == "thread" {
    if let Some(ref tid) = active_thread {
        // Read messages (includes in-progress turn automatically)
        let msg_path = Path::from_components(vec![
            "threads".to_string(), tid.clone(),
            "history".to_string(), "messages".to_string(),
        ]);
        if let Ok(Some(record)) = client.read(&msg_path).await {
            if let Some(Value::Array(arr)) = record.as_value() {
                messages = parse_chat_messages(arr);
            }
        }

        // Read turn state
        let thinking_path = Path::from_components(vec![
            "threads".to_string(), tid.clone(),
            "history".to_string(), "turn".to_string(), "thinking".to_string(),
        ]);
        if let Ok(Some(record)) = client.read(&thinking_path).await {
            if let Some(Value::Bool(b)) = record.as_value() {
                thinking = *b;
            }
        }

        let tool_path = Path::from_components(vec![
            "threads".to_string(), tid.clone(),
            "history".to_string(), "turn".to_string(), "tool".to_string(),
        ]);
        if let Ok(Some(record)) = client.read(&tool_path).await {
            if let Some(Value::Map(m)) = record.as_value() {
                let name = m.get("name").and_then(|v| match v {
                    Value::String(s) => Some(s.clone()), _ => None,
                });
                let status = m.get("status").and_then(|v| match v {
                    Value::String(s) => Some(s.clone()), _ => None,
                });
                if let (Some(n), Some(s)) = (name, status) {
                    tool_status = Some((n, s));
                }
            }
        }

        let tokens_path = Path::from_components(vec![
            "threads".to_string(), tid.clone(),
            "history".to_string(), "turn".to_string(), "tokens".to_string(),
        ]);
        if let Ok(Some(record)) = client.read(&tokens_path).await {
            if let Some(Value::Map(m)) = record.as_value() {
                let in_t = m.get("in").and_then(|v| match v { Value::Integer(i) => Some(*i as u32), _ => None }).unwrap_or(0);
                let out_t = m.get("out").and_then(|v| match v { Value::Integer(i) => Some(*i as u32), _ => None }).unwrap_or(0);
                turn_tokens = (in_t, out_t);
            }
        }

        // Read approval pending
        let approval_path = Path::from_components(vec![
            "threads".to_string(), tid.clone(),
            "approval".to_string(), "pending".to_string(),
        ]);
        if let Ok(Some(record)) = client.read(&approval_path).await {
            if let Some(Value::Map(m)) = record.as_value() {
                if m.contains_key("tool_name") {
                    let tool_name = m.get("tool_name").and_then(|v| match v {
                        Value::String(s) => Some(s.clone()), _ => None,
                    }).unwrap_or_default();
                    let input_preview = m.get("input_preview").and_then(|v| match v {
                        Value::String(s) => Some(s.clone()), _ => None,
                    }).unwrap_or_default();
                    approval_pending = Some(ox_ui::ApprovalRequest { tool_name, input_preview });
                }
            }
        }
    }
}
```

- [ ] **Step 3: Update draw functions for new ViewState fields**

In `crates/ox-cli/src/tui.rs`:

The draw function builds a ThreadView from ViewState. Change from using `vs.thread_views` to:

```rust
let view = ThreadView {
    messages: vs.messages.clone(),
    thinking: vs.thinking,
    tokens_in: vs.turn_tokens.0,
    tokens_out: vs.turn_tokens.1,
    policy_stats: Default::default(),
};
```

For the approval dialog, read from `vs.approval_pending` instead of `vs.pending_approval`.

For draw_status_bar, read tokens from `vs.turn_tokens` and thinking from `vs.thinking`.

- [ ] **Step 4: Update approval dialog key handling**

In tui.rs, the approval dialog currently reads from `app.pending_approval` and sends response via the stored `respond` channel. Change to:

1. Show dialog when `vs.approval_pending.is_some()` (read from ViewState)
2. Keep `app.approval_selected: usize` for arrow key navigation
3. On Enter/shortcut: write response to broker via client

```rust
// In handle_approval_key equivalent:
let decision = match key {
    'y' | 'Y' => "allow_once",
    's' | 'S' => "allow_session",
    'a' | 'A' => "allow_always",
    'n' | 'N' | Esc => "deny_once",
    'd' | 'D' => "deny_always",
    Enter => APPROVAL_OPTIONS[app.approval_selected].1,
    // ...
};

if let Some(ref tid) = selected_thread_id_for_approval {
    let response_path = Path::from_components(vec![
        "threads".to_string(), tid.clone(),
        "approval".to_string(), "response".to_string(),
    ]);
    client.write(&response_path, Record::parsed(Value::String(decision.to_string()))).await.ok();
    app.approval_selected = 0;
}
```

- [ ] **Step 5: Remove dead state from App**

In `crates/ox-cli/src/app.rs`:

Remove fields:
- `event_rx: mpsc::Receiver<AppEvent>`
- `control_rx: mpsc::Receiver<AppControl>`
- `thread_views: HashMap<String, ThreadView>`
- `streaming_turns: HashMap<String, StreamingTurn>`
- `pending_approval: Option<ApprovalState>`

Remove types:
- `AppEvent` enum
- `AppControl` enum
- `ApprovalResponse` enum
- `ApprovalState` struct (keep a simplified version or use ApprovalRequest from ox-ui)
- `ThreadView` struct
- `StreamingTurn` struct (both in app.rs and view_state.rs)

Remove methods:
- `handle_event()`
- `drain_agent_events()`
- `update_streaming()`
- `update_thread_state()` — inbox writes now happen in agent_worker
- `active_thinking()` — replaced by ViewState.thinking

Add field:
- `approval_selected: usize` — for dialog navigation (simple counter)

- [ ] **Step 6: Update event loop**

In tui.rs `run_async`:

Remove:
- `app.drain_agent_events()` call
- `control_rx.try_recv()` block
- `pending_approval` creation from control events

The loop becomes:
```
loop {
    // 1. Fetch ViewState from broker
    // 2. Draw
    // 3. Handle pending_action
    // 4. Poll terminal events
    // 5. Handle key/mouse events
}
```

- [ ] **Step 7: Update main.rs**

Remove event/control channel creation. `App::new` no longer takes `event_tx`/`control_tx` (they were removed from AgentPool in Task 3).

- [ ] **Step 8: Compile + test**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: Compiles. All tests pass.

- [ ] **Step 9: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: 14/14 pass.

- [ ] **Step 10: Commit**

```bash
git add -u
git commit -m "feat(ox-cli): route all events through broker, eliminate AppEvent/mpsc channels"
```

---

### Task 5: Update Status Document

**Files:**
- Modify: `docs/design/rfc/structfs-tui-status.md`

- [ ] **Step 1: Add C6 section and update What's Next**

Add C6 under Phase C. Update What's Next to remove Events Through Broker, promote remaining items.

- [ ] **Step 2: Commit**

```bash
git add docs/design/rfc/structfs-tui-status.md
git commit -m "docs: update status/handoff for C6 completion"
```

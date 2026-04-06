# BrokerStore (Plan C1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build an async BrokerStore that routes StructFS read/write operations between clients and servers by path prefix, enabling cross-store communication for the TUI architecture.

**Architecture:** The BrokerStore is an async request/response multiplexer adapted from appiware's broker_store.rs. Clients send read/write requests on paths. The broker matches the path prefix to a mounted server and forwards the request. Servers are synchronous StructFS stores wrapped in async tasks. Clients get async handles. Dynamic mount/unmount of servers at runtime.

**Tech Stack:** Rust, tokio (async runtime), structfs-core-store (Path, Record, Value, StoreError), tokio::sync (oneshot, mpsc)

**Spec:** `docs/superpowers/specs/2026-04-06-structfs-tui-design.md`

**Reference:** `../appiware/host/src/broker_store.rs` — the original async broker pattern

---

## File Structure

| File | Responsibility |
|------|---------------|
| `crates/ox-broker/Cargo.toml` | New crate: async broker for StructFS |
| `crates/ox-broker/src/lib.rs` | Module exports, BrokerStore public API |
| `crates/ox-broker/src/types.rs` | Request, Response, Action types for ox's Record/Value |
| `crates/ox-broker/src/broker.rs` | BrokerInner state machine — routing, queuing, action resolution |
| `crates/ox-broker/src/client.rs` | ClientHandle — async read/write against the broker |
| `crates/ox-broker/src/server.rs` | ServerHandle — wraps sync Reader/Writer stores as async servers |
| `crates/ox-broker/src/mount.rs` | Dynamic mount/unmount — register/deregister server prefixes at runtime |
| `Cargo.toml` (workspace root) | Add ox-broker to workspace members + tokio workspace dep |

---

### Task 1: Crate Scaffold and Core Types

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/ox-broker/Cargo.toml`
- Create: `crates/ox-broker/src/lib.rs`
- Create: `crates/ox-broker/src/types.rs`

The broker uses ox's existing StructFS types (Path, Record, Value, StoreError)
rather than appiware's generic serde-based approach. This simplifies the
protocol — no erased_serde, no generic type parameters on read/write.

- [ ] **Step 1: Create crate directory and Cargo.toml**

```toml
# crates/ox-broker/Cargo.toml
[package]
name = "ox-broker"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
homepage.workspace = true
authors.workspace = true
description = "Async BrokerStore for StructFS — routes reads/writes between stores by path prefix"
readme = "README.md"
keywords = ["agent", "ai", "structfs", "broker", "async"]
categories = ["data-structures", "asynchronous"]

[dependencies]
structfs-core-store = { workspace = true }
tokio = { workspace = true }
```

- [ ] **Step 2: Add to workspace**

Add `"crates/ox-broker"` to the `members` list in root `Cargo.toml`.

Add tokio as a workspace dependency if not present:
```toml
tokio = { version = "1", features = ["sync", "time", "rt"] }
```

- [ ] **Step 3: Write types.rs with tests**

```rust
//! Request/Response types for the broker protocol.
//!
//! Adapted from appiware's broker_store.rs but using ox's StructFS types
//! (Record, Value, Path, StoreError) instead of generic serde types.

use structfs_core_store::{Path, Record, Error as StoreError};

/// A request routed through the broker from client to server.
#[derive(Debug)]
pub struct Request {
    /// Unique action ID for matching responses.
    pub action_id: u64,
    /// The operation to perform.
    pub kind: RequestKind,
    /// Path relative to the server's mount prefix.
    /// If the client wrote to "threads/t_abc/history/append" and the server
    /// is mounted at "threads/t_abc/", the request path is "history/append".
    pub path: Path,
}

/// The kind of operation requested.
#[derive(Debug)]
pub enum RequestKind {
    Read,
    Write(Record),
}

/// A response from a server back to the waiting client.
#[derive(Debug)]
pub enum Response {
    Read(Result<Option<Record>, StoreError>),
    Write(Result<Path, StoreError>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::{Value, path};

    #[test]
    fn request_read_construction() {
        let req = Request {
            action_id: 1,
            kind: RequestKind::Read,
            path: path!("history/messages"),
        };
        assert_eq!(req.action_id, 1);
        assert!(matches!(req.kind, RequestKind::Read));
    }

    #[test]
    fn request_write_construction() {
        let record = Record::parsed(Value::String("hello".to_string()));
        let req = Request {
            action_id: 2,
            kind: RequestKind::Write(record),
            path: path!("history/append"),
        };
        assert_eq!(req.action_id, 2);
        assert!(matches!(req.kind, RequestKind::Write(_)));
    }

    #[test]
    fn response_read_ok() {
        let resp = Response::Read(Ok(Some(Record::parsed(Value::Integer(42)))));
        assert!(matches!(resp, Response::Read(Ok(Some(_)))));
    }

    #[test]
    fn response_read_none() {
        let resp = Response::Read(Ok(None));
        assert!(matches!(resp, Response::Read(Ok(None))));
    }

    #[test]
    fn response_write_ok() {
        let resp = Response::Write(Ok(path!("result/path")));
        assert!(matches!(resp, Response::Write(Ok(_))));
    }

    #[test]
    fn response_write_err() {
        let resp = Response::Write(Err(StoreError::store("test", "write", "failed")));
        assert!(matches!(resp, Response::Write(Err(_))));
    }
}
```

- [ ] **Step 4: Write lib.rs**

```rust
//! Async BrokerStore for StructFS — routes reads/writes between stores
//! by path prefix.
//!
//! The broker is the central routing layer for the ox TUI architecture.
//! Clients send read/write requests on paths. The broker matches the path
//! prefix to a mounted server and forwards the request. Servers are
//! synchronous StructFS stores wrapped in async tasks.

pub mod types;

pub use types::{Request, RequestKind, Response};
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo check -p ox-broker`
Expected: clean build

- [ ] **Step 6: Run tests**

Run: `cargo test -p ox-broker`
Expected: 6 type tests pass

- [ ] **Step 7: Commit**

```
git add Cargo.toml crates/ox-broker/
git commit -m 'feat(ox-broker): new crate with request/response types for StructFS broker

Adapts appiware broker_store.rs protocol types to use ox StructFS
types (Path, Record, Value, StoreError) instead of generic serde.
Request carries an action ID, operation kind (read/write), and a
path relative to the server mount prefix. Response carries the
Result matching the request kind.'
```

---

### Task 2: BrokerInner — Routing State Machine

**Files:**
- Create: `crates/ox-broker/src/broker.rs`
- Modify: `crates/ox-broker/src/lib.rs`

The core state machine: maps path prefixes to server channels, queues
requests when no server is ready, and matches responses to waiting clients.

- [ ] **Step 1: Write broker.rs with the core struct and routing**

```rust
//! BrokerInner — the core routing state machine.
//!
//! Manages the mapping from path prefixes to server channels, queues
//! requests when servers aren't ready, and resolves responses back
//! to waiting clients.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::Duration;
use tokio::sync::oneshot;

use structfs_core_store::{Error as StoreError, Path, Record};

use crate::types::{Request, RequestKind, Response};

type ActionId = u64;

/// A pending action waiting for a server to fulfill it.
enum Action {
    Read {
        path: Path,
        tx: oneshot::Sender<Result<Option<Record>, StoreError>>,
    },
    Write {
        path: Path,
        data: Record,
        tx: oneshot::Sender<Result<Path, StoreError>>,
    },
}

/// Channel for sending requests to a server task.
type ServerTx = tokio::sync::mpsc::Sender<Request>;

/// The inner state of the broker, protected by a mutex.
pub(crate) struct BrokerInner {
    /// Next action ID to assign.
    next_action_id: ActionId,
    /// Pending actions keyed by action ID.
    actions: HashMap<ActionId, Action>,
    /// Mounted servers keyed by path prefix (sorted for longest-prefix matching).
    servers: BTreeMap<String, ServerTx>,
    /// Whether the broker has been shut down.
    shut_down: bool,
}

impl BrokerInner {
    pub fn new() -> Self {
        Self {
            next_action_id: 0,
            actions: HashMap::new(),
            servers: BTreeMap::new(),
            shut_down: false,
        }
    }

    /// Mount a server at the given prefix. Returns the receiver end for
    /// the server task to listen on.
    pub fn mount(
        &mut self,
        prefix: &str,
    ) -> tokio::sync::mpsc::Receiver<Request> {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        self.servers.insert(prefix.to_string(), tx);
        rx
    }

    /// Unmount a server at the given prefix.
    pub fn unmount(&mut self, prefix: &str) {
        self.servers.remove(prefix);
    }

    /// Find the server whose prefix is the longest match for the given path.
    /// Returns the server's sender and the sub-path (path with prefix stripped).
    fn route(&self, path: &Path) -> Option<(&ServerTx, Path)> {
        let path_str = path.to_string();
        // Find longest matching prefix
        let mut best: Option<(&str, &ServerTx)> = None;
        for (prefix, tx) in &self.servers {
            if path_str == *prefix
                || path_str.starts_with(&format!("{}/", prefix))
                || prefix.is_empty()
            {
                match best {
                    None => best = Some((prefix.as_str(), tx)),
                    Some((best_prefix, _)) if prefix.len() > best_prefix.len() => {
                        best = Some((prefix.as_str(), tx))
                    }
                    _ => {}
                }
            }
        }
        best.map(|(prefix, tx)| {
            let sub_path = if prefix.is_empty() {
                path.clone()
            } else {
                let sub_str = path_str
                    .strip_prefix(prefix)
                    .unwrap_or("")
                    .trim_start_matches('/');
                if sub_str.is_empty() {
                    Path::from_components(vec![])
                } else {
                    Path::parse(sub_str).unwrap_or_else(|_| Path::from_components(vec![]))
                }
            };
            (tx, sub_path)
        })
    }

    /// Submit a read request. Returns a oneshot receiver for the result.
    pub fn submit_read(
        &mut self,
        path: &Path,
    ) -> Result<oneshot::Receiver<Result<Option<Record>, StoreError>>, StoreError> {
        if self.shut_down {
            return Err(StoreError::store("broker", "read", "broker has shut down"));
        }

        let (server_tx, sub_path) = self.route(path).ok_or_else(|| {
            StoreError::NoRoute { path: path.clone() }
        })?;

        let action_id = self.next_action_id;
        self.next_action_id += 1;

        let (tx, rx) = oneshot::channel();
        let action = Action::Read {
            path: sub_path.clone(),
            tx,
        };
        self.actions.insert(action_id, action);

        let request = Request {
            action_id,
            kind: RequestKind::Read,
            path: sub_path,
        };

        // Try to send to server. If the channel is full or closed,
        // clean up the action and return an error.
        if server_tx.try_send(request).is_err() {
            self.actions.remove(&action_id);
            return Err(StoreError::store(
                "broker",
                "read",
                format!("server for prefix matching '{}' is unavailable", path),
            ));
        }

        Ok(rx)
    }

    /// Submit a write request. Returns a oneshot receiver for the result.
    pub fn submit_write(
        &mut self,
        path: &Path,
        data: Record,
    ) -> Result<oneshot::Receiver<Result<Path, StoreError>>, StoreError> {
        if self.shut_down {
            return Err(StoreError::store("broker", "write", "broker has shut down"));
        }

        let (server_tx, sub_path) = self.route(path).ok_or_else(|| {
            StoreError::NoRoute { path: path.clone() }
        })?;

        let action_id = self.next_action_id;
        self.next_action_id += 1;

        let (tx, rx) = oneshot::channel();
        let action = Action::Write {
            path: sub_path.clone(),
            data: data.clone(),
            tx,
        };
        self.actions.insert(action_id, action);

        let request = Request {
            action_id,
            kind: RequestKind::Write(data),
            path: sub_path,
        };

        if server_tx.try_send(request).is_err() {
            self.actions.remove(&action_id);
            return Err(StoreError::store(
                "broker",
                "write",
                format!("server for prefix matching '{}' is unavailable", path),
            ));
        }

        Ok(rx)
    }

    /// Resolve an action with a response from the server.
    pub fn resolve(&mut self, action_id: ActionId, response: Response) {
        if let Some(action) = self.actions.remove(&action_id) {
            match (action, response) {
                (Action::Read { tx, .. }, Response::Read(result)) => {
                    tx.send(result).ok();
                }
                (Action::Write { tx, .. }, Response::Write(result)) => {
                    tx.send(result).ok();
                }
                _ => {
                    // Mismatched response type — should not happen
                }
            }
        }
    }

    /// Shut down the broker, failing all pending actions.
    pub fn shut_down(&mut self) {
        self.shut_down = true;
        let action_ids: Vec<ActionId> = self.actions.keys().cloned().collect();
        for id in action_ids {
            if let Some(action) = self.actions.remove(&id) {
                let err = StoreError::store("broker", "shutdown", "broker has shut down");
                match action {
                    Action::Read { tx, .. } => { tx.send(Err(err)).ok(); }
                    Action::Write { tx, .. } => { tx.send(Err(err)).ok(); }
                }
            }
        }
        self.servers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::{path, Value};

    #[test]
    fn mount_and_route() {
        let mut inner = BrokerInner::new();
        let _rx = inner.mount("ui");

        let (server_tx, sub_path) = inner.route(&path!("ui/selected_row")).unwrap();
        assert_eq!(sub_path.to_string(), "selected_row");
    }

    #[test]
    fn longest_prefix_wins() {
        let mut inner = BrokerInner::new();
        let _rx1 = inner.mount("threads");
        let _rx2 = inner.mount("threads/t_abc");

        let (_, sub_path) = inner.route(&path!("threads/t_abc/history/messages")).unwrap();
        assert_eq!(sub_path.to_string(), "history/messages");
    }

    #[test]
    fn no_route_returns_none() {
        let inner = BrokerInner::new();
        assert!(inner.route(&path!("nonexistent/path")).is_none());
    }

    #[test]
    fn unmount_removes_route() {
        let mut inner = BrokerInner::new();
        let _rx = inner.mount("ui");
        assert!(inner.route(&path!("ui/mode")).is_some());

        inner.unmount("ui");
        assert!(inner.route(&path!("ui/mode")).is_none());
    }

    #[test]
    fn shut_down_rejects_new_requests() {
        let mut inner = BrokerInner::new();
        let _rx = inner.mount("ui");
        inner.shut_down();

        let result = inner.submit_read(&path!("ui/mode"));
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Wire up in lib.rs**

```rust
pub mod broker;
pub mod types;

pub use types::{Request, RequestKind, Response};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-broker`
Expected: all type tests + broker routing tests pass

- [ ] **Step 4: Commit**

```
git add crates/ox-broker/
git commit -m 'feat(ox-broker): add BrokerInner routing state machine

Longest-prefix matching routes paths to mounted servers. Servers
receive requests via tokio mpsc channels. Actions are tracked by ID
for response matching. Dynamic mount/unmount supported. Shutdown
fails all pending actions gracefully.'
```

---

### Task 3: ClientHandle — Async Read/Write

**Files:**
- Create: `crates/ox-broker/src/client.rs`
- Modify: `crates/ox-broker/src/lib.rs`

The client handle provides async read/write against the broker. This is
what the TUI event loop and agent workers hold.

- [ ] **Step 1: Write client.rs**

```rust
//! ClientHandle — async read/write against the broker.
//!
//! Each client holds an Arc<Mutex<BrokerInner>> and submits requests
//! through it. The request blocks (async await) until the server
//! fulfills it.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use structfs_core_store::{Error as StoreError, Path, Record};

use crate::broker::BrokerInner;

/// An async handle for reading and writing through the broker.
///
/// Clients submit requests and await responses. Multiple clients
/// can exist for the same broker.
#[derive(Clone)]
pub struct ClientHandle {
    inner: Arc<Mutex<BrokerInner>>,
    /// Optional path prefix prepended to all operations.
    /// Used for scoped clients (e.g., agent worker scoped to its thread).
    scope: Option<String>,
    /// Timeout for operations.
    timeout: Duration,
}

impl ClientHandle {
    pub(crate) fn new(inner: Arc<Mutex<BrokerInner>>, timeout: Duration) -> Self {
        Self {
            inner,
            scope: None,
            timeout,
        }
    }

    /// Create a scoped client that prepends `prefix` to all paths.
    ///
    /// The scoped client sees a sub-namespace: writing to "history/append"
    /// actually writes to "{prefix}/history/append" in the broker.
    pub fn scoped(&self, prefix: &str) -> Self {
        let new_scope = match &self.scope {
            Some(existing) => format!("{}/{}", existing, prefix),
            None => prefix.to_string(),
        };
        Self {
            inner: self.inner.clone(),
            scope: Some(new_scope),
            timeout: self.timeout,
        }
    }

    /// Resolve the full path by prepending the scope prefix.
    fn resolve_path(&self, path: &Path) -> Result<Path, StoreError> {
        match &self.scope {
            None => Ok(path.clone()),
            Some(prefix) => {
                let path_str = path.to_string();
                let full = if path_str.is_empty() {
                    prefix.clone()
                } else {
                    format!("{}/{}", prefix, path_str)
                };
                Path::parse(&full).map_err(|e| {
                    StoreError::store("client", "resolve", e.to_string())
                })
            }
        }
    }

    /// Async read from the broker.
    pub async fn read(&self, path: &Path) -> Result<Option<Record>, StoreError> {
        let full_path = self.resolve_path(path)?;
        let rx = {
            let mut inner = self.inner.lock().await;
            inner.submit_read(&full_path)?
        };

        tokio::time::timeout(self.timeout, rx)
            .await
            .map_err(|_| {
                StoreError::store("client", "read", format!("timeout reading '{}'", full_path))
            })?
            .map_err(|_| {
                StoreError::store("client", "read", format!("server dropped for '{}'", full_path))
            })?
    }

    /// Async write to the broker.
    pub async fn write(
        &self,
        path: &Path,
        data: Record,
    ) -> Result<Path, StoreError> {
        let full_path = self.resolve_path(path)?;
        let rx = {
            let mut inner = self.inner.lock().await;
            inner.submit_write(&full_path, data)?
        };

        tokio::time::timeout(self.timeout, rx)
            .await
            .map_err(|_| {
                StoreError::store("client", "write", format!("timeout writing '{}'", full_path))
            })?
            .map_err(|_| {
                StoreError::store("client", "write", format!("server dropped for '{}'", full_path))
            })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::{path, Value};

    #[tokio::test]
    async fn scoped_client_prepends_prefix() {
        let inner = Arc::new(Mutex::new(BrokerInner::new()));
        let client = ClientHandle::new(inner, Duration::from_secs(5));
        let scoped = client.scoped("threads/t_abc");

        let resolved = scoped.resolve_path(&path!("history/messages")).unwrap();
        assert_eq!(resolved.to_string(), "threads/t_abc/history/messages");
    }

    #[tokio::test]
    async fn nested_scopes_compose() {
        let inner = Arc::new(Mutex::new(BrokerInner::new()));
        let client = ClientHandle::new(inner, Duration::from_secs(5));
        let scoped = client.scoped("threads").scoped("t_abc");

        let resolved = scoped.resolve_path(&path!("history")).unwrap();
        assert_eq!(resolved.to_string(), "threads/t_abc/history");
    }

    #[tokio::test]
    async fn read_without_server_returns_no_route() {
        let inner = Arc::new(Mutex::new(BrokerInner::new()));
        let client = ClientHandle::new(inner, Duration::from_secs(1));

        let result = client.read(&path!("nonexistent")).await;
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Update lib.rs**

```rust
pub mod broker;
pub mod client;
pub mod types;

pub use client::ClientHandle;
pub use types::{Request, RequestKind, Response};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-broker`
Expected: all tests pass

- [ ] **Step 4: Commit**

```
git add crates/ox-broker/
git commit -m 'feat(ox-broker): add ClientHandle with async read/write and path scoping

ClientHandle provides async read/write against the broker with
configurable timeouts. Scoped clients prepend a prefix to all paths,
enabling agent workers to operate on a sub-namespace without knowing
their full path. Scopes compose (scoped("threads").scoped("t_abc")
produces "threads/t_abc" prefix).'
```

---

### Task 4: ServerHandle — Wrap Sync Stores

**Files:**
- Create: `crates/ox-broker/src/server.rs`
- Modify: `crates/ox-broker/src/lib.rs`

The server handle wraps a synchronous StructFS `Store` (Reader + Writer)
and runs it as an async task, receiving requests from the broker and
sending responses back.

- [ ] **Step 1: Write server.rs**

```rust
//! ServerHandle — wraps a synchronous Reader/Writer store as an async
//! server in the broker.
//!
//! Each server runs as a tokio task that receives requests from its
//! channel, calls the synchronous store, and resolves the action in
//! the broker.

use std::sync::Arc;
use tokio::sync::Mutex;

use structfs_core_store::{Reader, Record, Store, Writer};

use crate::broker::BrokerInner;
use crate::types::{Request, RequestKind, Response};

/// Spawn a server task that wraps a synchronous Store.
///
/// The store is moved into the task and exclusively owned by it.
/// Requests arrive via the broker's channel; responses are resolved
/// back through the broker's action map.
///
/// Returns a JoinHandle for the server task.
pub fn spawn_server<S: Store + Send + 'static>(
    inner: Arc<Mutex<BrokerInner>>,
    prefix: &str,
    store: S,
) -> tokio::task::JoinHandle<()> {
    let prefix = prefix.to_string();
    let rx = {
        // We need to block on the mutex briefly to mount
        // This is called during setup, not in the hot path
        let mut inner_guard = inner.blocking_lock();
        inner_guard.mount(&prefix)
    };

    tokio::spawn(async move {
        server_loop(inner, store, rx).await;
    })
}

/// The server loop: receive requests, call the store, resolve responses.
async fn server_loop<S: Store>(
    inner: Arc<Mutex<BrokerInner>>,
    mut store: S,
    mut rx: tokio::sync::mpsc::Receiver<Request>,
) {
    while let Some(request) = rx.recv().await {
        let action_id = request.action_id;

        let response = match request.kind {
            RequestKind::Read => {
                let result = store.read(&request.path);
                Response::Read(result)
            }
            RequestKind::Write(data) => {
                let result = store.write(&request.path, data);
                Response::Write(result)
            }
        };

        let mut inner_guard = inner.lock().await;
        inner_guard.resolve(action_id, response);
    }
}

/// Spawn a server task from a store that also needs a ClientHandle
/// for cross-store communication.
///
/// The `setup` closure receives the store and a ClientHandle, returning
/// the store (or a wrapper) that will serve requests. This allows
/// stores like InputStore to hold a client handle for writing to
/// other stores.
pub fn spawn_server_with_client<S, F>(
    inner: Arc<Mutex<BrokerInner>>,
    prefix: &str,
    setup: F,
) -> tokio::task::JoinHandle<()>
where
    S: Store + Send + 'static,
    F: FnOnce(crate::ClientHandle) -> S + Send + 'static,
{
    let prefix_str = prefix.to_string();
    let client = crate::ClientHandle::new(
        inner.clone(),
        std::time::Duration::from_secs(30),
    );
    let rx = {
        let mut inner_guard = inner.blocking_lock();
        inner_guard.mount(&prefix_str)
    };

    let store = setup(client);

    tokio::spawn(async move {
        server_loop(inner, store, rx).await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::{path, Value, Error as StoreError, Path};
    use std::collections::BTreeMap;

    /// A trivial in-memory store for testing.
    struct MemoryStore {
        data: BTreeMap<String, Value>,
    }

    impl MemoryStore {
        fn new() -> Self {
            Self { data: BTreeMap::new() }
        }
    }

    impl Reader for MemoryStore {
        fn read(
            &mut self,
            from: &Path,
        ) -> Result<Option<Record>, StoreError> {
            Ok(self.data.get(&from.to_string()).map(|v| Record::parsed(v.clone())))
        }
    }

    impl Writer for MemoryStore {
        fn write(
            &mut self,
            to: &Path,
            data: Record,
        ) -> Result<Path, StoreError> {
            if let Some(value) = data.as_value() {
                self.data.insert(to.to_string(), value.clone());
            }
            Ok(to.clone())
        }
    }

    #[tokio::test]
    async fn server_handles_read_and_write() {
        let inner = Arc::new(Mutex::new(BrokerInner::new()));
        let client = crate::ClientHandle::new(inner.clone(), std::time::Duration::from_secs(5));

        let mut store = MemoryStore::new();
        store.data.insert(
            "greeting".to_string(),
            Value::String("hello".to_string()),
        );

        let _handle = spawn_server(inner, "test", store);

        // Read existing value
        let result = client.read(&path!("test/greeting")).await.unwrap();
        let value = result.unwrap();
        assert_eq!(value.as_value().unwrap(), &Value::String("hello".to_string()));

        // Write new value
        client
            .write(
                &path!("test/name"),
                Record::parsed(Value::String("world".to_string())),
            )
            .await
            .unwrap();

        // Read it back
        let result = client.read(&path!("test/name")).await.unwrap();
        let value = result.unwrap();
        assert_eq!(value.as_value().unwrap(), &Value::String("world".to_string()));
    }

    #[tokio::test]
    async fn multiple_servers_route_correctly() {
        let inner = Arc::new(Mutex::new(BrokerInner::new()));
        let client = crate::ClientHandle::new(inner.clone(), std::time::Duration::from_secs(5));

        let mut store_a = MemoryStore::new();
        store_a.data.insert("value".to_string(), Value::String("A".to_string()));
        let _ha = spawn_server(inner.clone(), "store_a", store_a);

        let mut store_b = MemoryStore::new();
        store_b.data.insert("value".to_string(), Value::String("B".to_string()));
        let _hb = spawn_server(inner, "store_b", store_b);

        let a = client.read(&path!("store_a/value")).await.unwrap().unwrap();
        assert_eq!(a.as_value().unwrap(), &Value::String("A".to_string()));

        let b = client.read(&path!("store_b/value")).await.unwrap().unwrap();
        assert_eq!(b.as_value().unwrap(), &Value::String("B".to_string()));
    }

    #[tokio::test]
    async fn scoped_client_writes_to_correct_server() {
        let inner = Arc::new(Mutex::new(BrokerInner::new()));
        let client = crate::ClientHandle::new(inner.clone(), std::time::Duration::from_secs(5));

        let _handle = spawn_server(inner, "threads/t_abc", MemoryStore::new());

        let scoped = client.scoped("threads/t_abc");
        scoped
            .write(
                &path!("msg"),
                Record::parsed(Value::String("hello".to_string())),
            )
            .await
            .unwrap();

        // Read via unscoped client at full path
        let result = client.read(&path!("threads/t_abc/msg")).await.unwrap().unwrap();
        assert_eq!(result.as_value().unwrap(), &Value::String("hello".to_string()));
    }
}
```

- [ ] **Step 2: Update lib.rs**

```rust
pub mod broker;
pub mod client;
pub mod server;
pub mod types;

pub use client::ClientHandle;
pub use server::{spawn_server, spawn_server_with_client};
pub use types::{Request, RequestKind, Response};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-broker`
Expected: all tests pass including server integration tests

- [ ] **Step 4: Commit**

```
git add crates/ox-broker/
git commit -m 'feat(ox-broker): add ServerHandle wrapping sync stores as async tasks

Synchronous Reader/Writer stores are wrapped in tokio tasks that
receive requests from the broker channel and resolve responses.
spawn_server() takes ownership of a Store and runs it. Multiple
servers route correctly by path prefix. Scoped clients write to
the correct server. spawn_server_with_client() provides a ClientHandle
to stores that need cross-store communication.'
```

---

### Task 5: BrokerStore Public API

**Files:**
- Modify: `crates/ox-broker/src/lib.rs`

The top-level BrokerStore struct that ties everything together: creates
the shared inner, provides methods for mounting stores and minting
client handles.

- [ ] **Step 1: Add BrokerStore to lib.rs**

```rust
//! Async BrokerStore for StructFS — routes reads/writes between stores
//! by path prefix.

pub mod broker;
pub mod client;
pub mod server;
pub mod types;

pub use client::ClientHandle;
pub use server::{spawn_server, spawn_server_with_client};
pub use types::{Request, RequestKind, Response};

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// The top-level BrokerStore — creates the shared routing state and
/// provides methods for mounting stores and minting client handles.
pub struct BrokerStore {
    inner: Arc<Mutex<broker::BrokerInner>>,
    default_timeout: Duration,
}

impl BrokerStore {
    /// Create a new broker with the given default timeout for operations.
    pub fn new(default_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(broker::BrokerInner::new())),
            default_timeout,
        }
    }

    /// Create a client handle for reading/writing through the broker.
    pub fn client(&self) -> ClientHandle {
        ClientHandle::new(self.inner.clone(), self.default_timeout)
    }

    /// Mount a synchronous Store at the given prefix and spawn its
    /// server task. Returns the JoinHandle for the server.
    pub fn mount<S: structfs_core_store::Store + Send + 'static>(
        &self,
        prefix: &str,
        store: S,
    ) -> tokio::task::JoinHandle<()> {
        spawn_server(self.inner.clone(), prefix, store)
    }

    /// Mount a store that needs a ClientHandle for cross-store
    /// communication. The setup closure receives a ClientHandle and
    /// returns the store to serve.
    pub fn mount_with_client<S, F>(
        &self,
        prefix: &str,
        setup: F,
    ) -> tokio::task::JoinHandle<()>
    where
        S: structfs_core_store::Store + Send + 'static,
        F: FnOnce(ClientHandle) -> S + Send + 'static,
    {
        spawn_server_with_client(self.inner.clone(), prefix, setup)
    }

    /// Unmount a server at the given prefix.
    pub async fn unmount(&self, prefix: &str) {
        let mut inner = self.inner.lock().await;
        inner.unmount(prefix);
    }

    /// Shut down the broker, failing all pending actions.
    pub async fn shut_down(&self) {
        let mut inner = self.inner.lock().await;
        inner.shut_down();
    }
}

impl Default for BrokerStore {
    fn default() -> Self {
        Self::new(Duration::from_secs(30))
    }
}
```

- [ ] **Step 2: Add end-to-end integration test**

Add to a new `tests/` directory or at the bottom of lib.rs:

```rust
#[cfg(test)]
mod integration_tests {
    use super::*;
    use structfs_core_store::{
        path, Error as StoreError, Path, Reader, Record, Value, Writer,
    };
    use std::collections::BTreeMap;

    struct MemoryStore {
        data: BTreeMap<String, Value>,
    }

    impl MemoryStore {
        fn new() -> Self {
            Self { data: BTreeMap::new() }
        }
        fn with(key: &str, value: Value) -> Self {
            let mut data = BTreeMap::new();
            data.insert(key.to_string(), value);
            Self { data }
        }
    }

    impl Reader for MemoryStore {
        fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
            Ok(self.data.get(&from.to_string()).map(|v| Record::parsed(v.clone())))
        }
    }

    impl Writer for MemoryStore {
        fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
            if let Some(value) = data.as_value() {
                self.data.insert(to.to_string(), value.clone());
            }
            Ok(to.clone())
        }
    }

    #[tokio::test]
    async fn full_broker_lifecycle() {
        let broker = BrokerStore::default();
        let client = broker.client();

        // Mount two stores
        let _ui = broker.mount(
            "ui",
            MemoryStore::with("mode", Value::String("normal".to_string())),
        );
        let _inbox = broker.mount("inbox", MemoryStore::new());

        // Read from ui store
        let mode = client.read(&path!("ui/mode")).await.unwrap().unwrap();
        assert_eq!(
            mode.as_value().unwrap(),
            &Value::String("normal".to_string())
        );

        // Write to inbox store
        client
            .write(
                &path!("inbox/thread_count"),
                Record::parsed(Value::Integer(5)),
            )
            .await
            .unwrap();

        // Read it back
        let count = client
            .read(&path!("inbox/thread_count"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(count.as_value().unwrap(), &Value::Integer(5));

        // Unmount and verify no route
        broker.unmount("inbox").await;
        let result = client.read(&path!("inbox/thread_count")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn scoped_client_for_agent_worker() {
        let broker = BrokerStore::default();

        // Mount a thread namespace
        broker.mount(
            "threads/t_abc",
            MemoryStore::with("prompt", Value::String("You are helpful.".to_string())),
        );

        // Agent worker gets a scoped client
        let agent = broker.client().scoped("threads/t_abc");

        // Agent reads "prompt" — broker resolves as "threads/t_abc/prompt"
        let prompt = agent.read(&path!("prompt")).await.unwrap().unwrap();
        assert_eq!(
            prompt.as_value().unwrap(),
            &Value::String("You are helpful.".to_string())
        );

        // Agent writes "history/msg" — broker resolves as "threads/t_abc/history/msg"
        agent
            .write(
                &path!("history/msg"),
                Record::parsed(Value::String("hello".to_string())),
            )
            .await
            .unwrap();

        // TUI client reads the same data at full path
        let tui = broker.client();
        let msg = tui
            .read(&path!("threads/t_abc/history/msg"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            msg.as_value().unwrap(),
            &Value::String("hello".to_string())
        );
    }

    #[tokio::test]
    async fn shutdown_fails_pending_operations() {
        let broker = BrokerStore::default();
        let _ui = broker.mount("ui", MemoryStore::new());

        broker.shut_down().await;

        let client = broker.client();
        let result = client.read(&path!("ui/mode")).await;
        assert!(result.is_err());
    }
}
```

- [ ] **Step 3: Run all tests**

Run: `cargo test -p ox-broker`
Expected: all unit + integration tests pass

- [ ] **Step 4: Run workspace check**

Run: `cargo check`
Expected: clean build

- [ ] **Step 5: Commit**

```
git add crates/ox-broker/
git commit -m 'feat(ox-broker): add BrokerStore public API with mount/unmount/client

BrokerStore ties together the routing state machine, client handles,
and server tasks. mount() wraps a sync Store in an async task.
mount_with_client() provides a ClientHandle to stores needing
cross-store communication. Scoped clients give agent workers a
sub-namespace view. Full lifecycle test: mount, read, write, unmount,
shutdown.'
```

---

## Summary

| Task | What | Tests |
|------|------|-------|
| 1 | Crate scaffold + Request/Response types | 6 |
| 2 | BrokerInner routing state machine | 5 |
| 3 | ClientHandle with async read/write + scoping | 3 |
| 4 | ServerHandle wrapping sync stores | 3 |
| 5 | BrokerStore public API + integration tests | 3 |

**Total: ~20 tests across 5 commits in a new `ox-broker` crate.**

After Plan C1, we have a working async broker that can route reads/writes
between any number of synchronous StructFS stores. Plan C2 builds the
stores (UiStore, InputStore, ThreadStore, etc.) and Plan C3 wires
everything into the TUI.

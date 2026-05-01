//! Async BrokerStore for StructFS — routes reads/writes between stores
//! by path prefix.

pub mod async_store;
mod broker;
mod client;
pub mod dispatching_store;
mod server;
pub mod subscription;
mod sync_adapter;
mod types;

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::BTreeMap;
    use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

    /// A trivial in-memory store for testing broker routing.
    pub struct MemoryStore {
        pub data: BTreeMap<String, Value>,
    }

    impl MemoryStore {
        pub fn new() -> Self {
            Self {
                data: BTreeMap::new(),
            }
        }
        pub fn with(key: &str, value: Value) -> Self {
            let mut data = BTreeMap::new();
            data.insert(key.to_string(), value);
            Self { data }
        }
    }

    impl Reader for MemoryStore {
        fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
            Ok(self
                .data
                .get(&from.to_string())
                .map(|v| Record::parsed(v.clone())))
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
}

pub use client::ClientHandle;
pub use sync_adapter::SyncClientAdapter;
pub use types::Request;

use std::sync::{Arc, RwLock as StdRwLock};
use std::time::Duration;
use tokio::sync::Mutex;

use structfs_core_store::{Error as StoreError, Path, Record, Reader, Writer};

use crate::async_store::BoxFuture;
use crate::dispatching_store::{DispatchingStore, SnapshotReader, TokioSpawnHandle};
use crate::subscription::{AsyncWriter as SubAsyncWriter, SpawnHandle, Subscription, SubscriptionRegistry};

/// Default cascade bound — the maximum depth of subscription-triggered
/// recursive writes. Per spec §3.3, default 64.
const DEFAULT_CASCADE_BOUND: usize = 64;

/// Substrate that writes through the broker's `BrokerInner::submit_write`,
/// bypassing the dispatcher (this is the "below" layer the dispatcher
/// applies writes onto). Held inside the dispatcher.
struct BrokerSubstrate {
    inner: Arc<Mutex<broker::BrokerInner>>,
    timeout: Duration,
}

impl SubAsyncWriter for BrokerSubstrate {
    fn write(&self, path: Path, record: Record) -> BoxFuture<Result<Path, StoreError>> {
        let inner = self.inner.clone();
        let timeout = self.timeout;
        Box::pin(async move {
            let rx = {
                let mut guard = inner.lock().await;
                guard.submit_write(&path, record)?
            };
            tokio::time::timeout(timeout, rx)
                .await
                .map_err(|_| {
                    StoreError::store(
                        "broker",
                        "write",
                        format!("timeout writing '{}'", path),
                    )
                })?
                .map_err(|_| {
                    StoreError::store(
                        "broker",
                        "write",
                        format!("server dropped for '{}'", path),
                    )
                })?
        })
    }
}

/// SnapshotReader for the broker. Reads use `BrokerInner::submit_read`.
/// `snapshot()` returns a `Reader` that issues reads through the same
/// path on demand — there is no point-in-time freezing because the
/// broker has no global "version" we can pin against. This is the
/// honest semantic: the snapshot reads the current state at each call,
/// which is fine for sub handlers that read just a few paths.
struct BrokerSnapshotReader {
    inner: Arc<Mutex<broker::BrokerInner>>,
    timeout: Duration,
}

impl BrokerSnapshotReader {
    fn read_now(&self, path: &Path) -> Result<Option<Record>, StoreError> {
        // Synchronous read against an async substrate. Use
        // `block_in_place` + `block_on` to bridge — this is the same
        // pattern the existing ProxyStore uses (see lib.rs).
        let inner = self.inner.clone();
        let timeout = self.timeout;
        let path = path.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let rx = {
                    let mut guard = inner.lock().await;
                    guard.submit_read(&path)?
                };
                tokio::time::timeout(timeout, rx)
                    .await
                    .map_err(|_| {
                        StoreError::store(
                            "broker",
                            "read",
                            format!("timeout reading '{}'", path),
                        )
                    })?
                    .map_err(|_| {
                        StoreError::store(
                            "broker",
                            "read",
                            format!("server dropped for '{}'", path),
                        )
                    })?
            })
        })
    }
}

impl SnapshotReader for BrokerSnapshotReader {
    fn snapshot(&self) -> Box<dyn Reader> {
        Box::new(LiveReader {
            inner: self.inner.clone(),
            timeout: self.timeout,
        })
    }
    fn read_path(&self, path: &Path) -> Result<Option<Record>, StoreError> {
        self.read_now(path)
    }
}

/// Reader returned by `BrokerSnapshotReader::snapshot`. Each `read` issues
/// a fresh broker read — no point-in-time freezing (see comment on
/// `BrokerSnapshotReader`).
struct LiveReader {
    inner: Arc<Mutex<broker::BrokerInner>>,
    timeout: Duration,
}

impl Reader for LiveReader {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let inner = self.inner.clone();
        let timeout = self.timeout;
        let path = from.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let rx = {
                    let mut guard = inner.lock().await;
                    guard.submit_read(&path)?
                };
                tokio::time::timeout(timeout, rx)
                    .await
                    .map_err(|_| {
                        StoreError::store(
                            "broker",
                            "read",
                            format!("timeout reading '{}'", path),
                        )
                    })?
                    .map_err(|_| {
                        StoreError::store(
                            "broker",
                            "read",
                            format!("server dropped for '{}'", path),
                        )
                    })?
            })
        })
    }
}

/// The top-level BrokerStore — creates the shared routing state and
/// provides methods for mounting stores and minting client handles.
#[derive(Clone)]
pub struct BrokerStore {
    inner: Arc<Mutex<broker::BrokerInner>>,
    default_timeout: Duration,
    /// The dispatcher writes go through. Always present (empty registry
    /// by default = no-op dispatch). Cloning the BrokerStore shares the
    /// dispatcher Arc, so handles minted from a single broker share one
    /// subscription set.
    dispatcher: Arc<DispatchingStore>,
    /// Registry handle — exposed via `register_subscription`. Same Arc
    /// the dispatcher reads from.
    subs: Arc<StdRwLock<SubscriptionRegistry>>,
}

impl BrokerStore {
    /// Create a new broker with the given default timeout. The dispatcher
    /// is created with an empty subscription registry, a `TokioSpawnHandle`
    /// for spawning, and the default cascade bound (64).
    pub fn new(default_timeout: Duration) -> Self {
        Self::with_components(
            default_timeout,
            Arc::new(StdRwLock::new(SubscriptionRegistry::new())),
            Arc::new(TokioSpawnHandle),
            DEFAULT_CASCADE_BOUND,
        )
    }

    /// Create a broker with explicit subscription components. Useful for
    /// tests that supply a `MockSpawn` or a pre-populated registry.
    pub fn with_components(
        default_timeout: Duration,
        subs: Arc<StdRwLock<SubscriptionRegistry>>,
        spawn: Arc<dyn SpawnHandle>,
        cascade_bound: usize,
    ) -> Self {
        let inner = Arc::new(Mutex::new(broker::BrokerInner::new()));
        let substrate: Arc<dyn SubAsyncWriter> = Arc::new(BrokerSubstrate {
            inner: inner.clone(),
            timeout: default_timeout,
        });
        let reader: Arc<dyn SnapshotReader> = Arc::new(BrokerSnapshotReader {
            inner: inner.clone(),
            timeout: default_timeout,
        });
        let dispatcher = Arc::new(DispatchingStore::new(
            substrate,
            reader,
            subs.clone(),
            spawn,
            cascade_bound,
        ));
        Self {
            inner,
            default_timeout,
            dispatcher,
            subs,
        }
    }

    /// Register a subscription. Subsequent writes whose path matches one
    /// of the subscription's `watches()` patterns invoke its `handle`.
    pub fn register_subscription(&self, sub: Arc<dyn Subscription>) {
        self.subs
            .write()
            .expect("registry lock poisoned")
            .register(sub);
    }

    /// Convenience: register multiple subscriptions in one call. Order
    /// of iteration is the registration order (relevant when multiple
    /// subs match the same write — see spec §3.3).
    pub fn register_subscriptions<I>(&self, subs: I)
    where
        I: IntoIterator<Item = Arc<dyn Subscription>>,
    {
        let mut guard = self.subs.write().expect("registry lock poisoned");
        for sub in subs {
            guard.register(sub);
        }
    }

    /// Create a client handle for reading/writing through the broker.
    pub fn client(&self) -> ClientHandle {
        ClientHandle::new(self.inner.clone(), self.default_timeout)
            .with_dispatcher(self.dispatcher.clone())
    }

    /// Mount a synchronous Store at the given prefix and spawn its
    /// server task. Returns the JoinHandle for the server.
    pub async fn mount<S: Reader + Writer + Send + 'static>(
        &self,
        prefix: structfs_core_store::Path,
        store: S,
    ) -> tokio::task::JoinHandle<()> {
        server::spawn_server(self.inner.clone(), prefix, store).await
    }

    /// Mount a store that needs a ClientHandle for cross-store
    /// communication. The setup closure receives a ClientHandle and
    /// returns the store to serve.
    pub async fn mount_with_client<S, F>(
        &self,
        prefix: structfs_core_store::Path,
        setup: F,
    ) -> tokio::task::JoinHandle<()>
    where
        S: Reader + Writer + Send + 'static,
        F: FnOnce(ClientHandle) -> S + Send + 'static,
    {
        server::spawn_server_with_client(self.inner.clone(), prefix, self.default_timeout, setup)
            .await
    }

    /// Unmount a server at the given prefix.
    pub async fn unmount(&self, prefix: &structfs_core_store::Path) {
        let mut inner = self.inner.lock().await;
        inner.unmount(prefix);
    }

    /// Mount an async store at the given prefix and spawn its server task.
    ///
    /// Reads are resolved inline; writes are spawned as independent tasks so a
    /// deferred write does not block the store from handling subsequent requests.
    pub async fn mount_async<S: async_store::AsyncReader + async_store::AsyncWriter>(
        &self,
        prefix: structfs_core_store::Path,
        store: S,
    ) -> tokio::task::JoinHandle<()> {
        server::spawn_async_server(self.inner.clone(), prefix, store).await
    }

    /// Shut down the broker, rejecting all future requests.
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

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::test_support::MemoryStore;
    use structfs_core_store::{Error as StoreError, Path, Record, Value, path};

    #[tokio::test]
    async fn full_broker_lifecycle() {
        let broker = BrokerStore::default();
        let client = broker.client();

        // Mount two stores
        let _ui = broker
            .mount(
                path!("ui"),
                MemoryStore::with("mode", Value::String("normal".to_string())),
            )
            .await;
        let _inbox = broker.mount(path!("inbox"), MemoryStore::new()).await;

        // Read from ui store
        let mode = client.read(&path!("ui/mode")).await.unwrap().unwrap();
        assert_eq!(
            mode.as_value().unwrap(),
            &Value::String("normal".to_string()),
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
        broker.unmount(&path!("inbox")).await;
        let result = client.read(&path!("inbox/thread_count")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn scoped_client_for_agent_worker() {
        let broker = BrokerStore::default();

        // Mount a thread namespace
        broker
            .mount(
                path!("threads/t_abc"),
                MemoryStore::with("prompt", Value::String("You are helpful.".to_string())),
            )
            .await;

        // Agent worker gets a scoped client
        let agent = broker.client().scoped("threads/t_abc");

        // Agent reads "prompt" — broker resolves as "threads/t_abc/prompt"
        let prompt = agent.read(&path!("prompt")).await.unwrap().unwrap();
        assert_eq!(
            prompt.as_value().unwrap(),
            &Value::String("You are helpful.".to_string()),
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
        assert_eq!(msg.as_value().unwrap(), &Value::String("hello".to_string()),);
    }

    #[tokio::test]
    async fn shutdown_fails_pending_operations() {
        let broker = BrokerStore::default();
        let _ui = broker.mount(path!("ui"), MemoryStore::new()).await;

        broker.shut_down().await;

        let client = broker.client();
        let result = client.read(&path!("ui/mode")).await;
        assert!(result.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mount_with_client_enables_cross_store_communication() {
        let broker = BrokerStore::default();

        // Mount a data store
        broker
            .mount(
                path!("data"),
                MemoryStore::with("greeting", Value::String("hello".to_string())),
            )
            .await;

        // Mount a store that reads from "data" via its client handle.
        // The sync Reader impl uses block_in_place to bridge to async,
        // which is the same pattern the Wasm host bridge will use.
        broker
            .mount_with_client(path!("proxy"), |client| ProxyStore { client })
            .await;

        // Read through the proxy — it reads from data store via broker
        let tui = broker.client();
        let result = tui.read(&path!("proxy/greeting")).await.unwrap().unwrap();
        assert_eq!(
            result.as_value().unwrap(),
            &Value::String("hello".to_string()),
        );
    }

    /// A store that proxies reads to another store via a ClientHandle.
    struct ProxyStore {
        client: ClientHandle,
    }

    impl Reader for ProxyStore {
        fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
            let full_path = Path::parse(&format!("data/{}", from))
                .map_err(|e| StoreError::store("proxy", "read", e.to_string()))?;
            // block_in_place allows sync code to call async within a
            // multi-thread runtime — same pattern as the Wasm host bridge.
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(self.client.read(&full_path))
            })
        }
    }

    impl Writer for ProxyStore {
        fn write(&mut self, to: &Path, _data: Record) -> Result<Path, StoreError> {
            Ok(to.clone())
        }
    }

    #[tokio::test]
    async fn write_typed_then_read_typed() {
        use serde::{Deserialize, Serialize};
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        struct Greeting {
            message: String,
            count: u32,
        }

        let broker = BrokerStore::default();
        let store = MemoryStore::new();
        let _h = broker.mount(path!("data"), store).await;
        let client = broker.client();

        let greeting = Greeting {
            message: "hello".to_string(),
            count: 42,
        };
        client
            .write_typed(&path!("data/greeting"), &greeting)
            .await
            .unwrap();

        let back: Option<Greeting> = client.read_typed(&path!("data/greeting")).await.unwrap();
        assert_eq!(back, Some(greeting));
    }

    #[tokio::test]
    async fn read_typed_returns_none_for_missing() {
        use serde::Deserialize;
        #[derive(Debug, Deserialize)]
        struct Anything {
            _x: String,
        }

        let broker = BrokerStore::default();
        let store = MemoryStore::new();
        let _h = broker.mount(path!("data"), store).await;
        let client = broker.client();

        let result: Option<Anything> = client.read_typed(&path!("data/nonexistent")).await.unwrap();
        assert!(result.is_none());
    }

    // ---- AsyncReader / AsyncWriter tests ----

    use crate::async_store::{AsyncReader, AsyncWriter, BoxFuture};

    /// A simple async store backed by a BTreeMap — all operations resolve immediately.
    struct AsyncMemoryStore {
        data: std::collections::BTreeMap<String, Value>,
    }

    impl AsyncMemoryStore {
        fn new() -> Self {
            Self {
                data: std::collections::BTreeMap::new(),
            }
        }
        fn with(key: &str, value: Value) -> Self {
            let mut s = Self::new();
            s.data.insert(key.to_string(), value);
            s
        }
    }

    impl AsyncReader for AsyncMemoryStore {
        fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
            let result = Ok(self
                .data
                .get(&from.to_string())
                .map(|v| Record::parsed(v.clone())));
            Box::pin(async move { result })
        }
    }

    impl AsyncWriter for AsyncMemoryStore {
        fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
            if let Some(value) = data.as_value() {
                self.data.insert(to.to_string(), value.clone());
            }
            let path = to.clone();
            Box::pin(async move { Ok(path) })
        }
    }

    #[tokio::test]
    async fn mount_async_store_reads_and_writes() {
        let broker = BrokerStore::default();
        let client = broker.client();

        let store = AsyncMemoryStore::with("greeting", Value::String("hello".to_string()));
        let _handle = broker.mount_async(path!("async_mem"), store).await;

        // Read existing value
        let result = client
            .read(&path!("async_mem/greeting"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result.as_value().unwrap(),
            &Value::String("hello".to_string()),
        );

        // Write a new value
        client
            .write(
                &path!("async_mem/name"),
                Record::parsed(Value::String("world".to_string())),
            )
            .await
            .unwrap();

        // Read it back
        let result = client
            .read(&path!("async_mem/name"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result.as_value().unwrap(),
            &Value::String("world".to_string()),
        );
    }

    /// A store that defers write("block") until write("unblock") is called.
    struct DeferredWriteStore {
        blocker: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    }

    impl DeferredWriteStore {
        fn new() -> Self {
            Self {
                blocker: Arc::new(tokio::sync::Mutex::new(None)),
            }
        }
    }

    impl AsyncReader for DeferredWriteStore {
        fn read(&mut self, _from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
            Box::pin(async move { Ok(None) })
        }
    }

    impl AsyncWriter for DeferredWriteStore {
        fn write(&mut self, to: &Path, _data: Record) -> BoxFuture<Result<Path, StoreError>> {
            let key = to.to_string();
            let blocker = self.blocker.clone();
            let path = to.clone();
            Box::pin(async move {
                if key == "block" {
                    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
                    {
                        let mut guard = blocker.lock().await;
                        *guard = Some(tx);
                    }
                    // Wait until "unblock" fires the sender
                    let _ = rx.await;
                } else if key == "unblock" {
                    let mut guard = blocker.lock().await;
                    if let Some(tx) = guard.take() {
                        let _ = tx.send(());
                    }
                }
                Ok(path)
            })
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_store_deferred_write() {
        let broker = BrokerStore::default();
        let client_a = broker.client();
        let client_b = broker.client();

        let store = DeferredWriteStore::new();
        let _handle = broker.mount_async(path!("deferred"), store).await;

        // Kick off a write that will block until "unblock" is sent
        let block_fut = tokio::spawn({
            let client = client_a.clone();
            async move {
                client
                    .write(
                        &path!("deferred/block"),
                        Record::parsed(Value::String("waiting".to_string())),
                    )
                    .await
            }
        });

        // Give the block write time to register the sender
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // block_fut should still be pending — it hasn't resolved yet
        assert!(!block_fut.is_finished());

        // Send "unblock" from a second client — this resolves the block write
        client_b
            .write(
                &path!("deferred/unblock"),
                Record::parsed(Value::String("go".to_string())),
            )
            .await
            .unwrap();

        // Now the block write should resolve
        block_fut.await.unwrap().unwrap();
    }

    // ---- Subscription dispatch through the broker (F4) ----

    use crate::subscription::{
        PathPattern, SubCtx, Subscription, SubscriptionId, Write as SubWrite,
    };

    /// Test subscription that, on every matching write, returns one
    /// extra write to a fixed status path. The handler clones an Arc
    /// indicator so the test can assert it actually ran.
    struct StatusSub {
        id: SubscriptionId,
        watches: Vec<PathPattern>,
        fired: Arc<std::sync::Mutex<u32>>,
    }

    impl Subscription for StatusSub {
        fn id(&self) -> &SubscriptionId {
            &self.id
        }
        fn watches(&self) -> &[PathPattern] {
            &self.watches
        }
        fn handle(&self, _ctx: SubCtx<'_>) -> Vec<SubWrite> {
            *self.fired.lock().unwrap() += 1;
            vec![SubWrite {
                path: path!("status/last"),
                record: Record::parsed(Value::String("ok".to_string())),
            }]
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn registered_subscription_fires_on_write() {
        let broker = BrokerStore::default();
        let _h = broker.mount(path!("status"), MemoryStore::new()).await;
        let _t = broker.mount(path!("trigger"), MemoryStore::new()).await;

        let fired = Arc::new(std::sync::Mutex::new(0));
        broker.register_subscription(Arc::new(StatusSub {
            id: SubscriptionId("status-writer".to_string()),
            watches: vec![PathPattern::Exact(path!("trigger"))],
            fired: fired.clone(),
        }));

        let client = broker.client();
        client
            .write(&path!("trigger"), Record::parsed(Value::Integer(1)))
            .await
            .unwrap();

        assert_eq!(
            *fired.lock().unwrap(),
            1,
            "subscription handler should have fired once"
        );

        // The status path should now hold "ok".
        let status = client.read(&path!("status/last")).await.unwrap().unwrap();
        assert_eq!(
            status.as_value().unwrap(),
            &Value::String("ok".to_string()),
        );
    }
}

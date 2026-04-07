//! Async BrokerStore for StructFS — routes reads/writes between stores
//! by path prefix.

mod broker;
mod client;
mod server;
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

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use structfs_core_store::{Reader, Writer};

/// The top-level BrokerStore — creates the shared routing state and
/// provides methods for mounting stores and minting client handles.
#[derive(Clone)]
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
}

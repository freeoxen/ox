use std::sync::Arc;

use async_trait::async_trait;
use ox_broker::async_store::{AsyncReader, AsyncWriter};
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Writer};
use tokio::sync::Mutex;

use ox_inbox::remote_state::RemoteNodeRecord;

#[async_trait]
pub trait StorePort: Send + Sync {
    async fn read(&self, path: &Path) -> Result<Option<Record>, StoreError>;
    async fn write(&self, path: &Path, record: Record) -> Result<Path, StoreError>;
}

/// Adapts an existing repository-local async StructFS Store without exposing
/// any provider- or transport-specific method to the coordinator.
pub struct AsyncStorePort<S> {
    inner: Mutex<S>,
}

impl<S> AsyncStorePort<S> {
    pub fn new(store: S) -> Self {
        Self {
            inner: Mutex::new(store),
        }
    }
}

#[async_trait]
impl<S> StorePort for AsyncStorePort<S>
where
    S: AsyncReader + AsyncWriter + Send,
{
    async fn read(&self, path: &Path) -> Result<Option<Record>, StoreError> {
        let future = {
            let mut store = self.inner.lock().await;
            store.read(path)
        };
        future.await
    }

    async fn write(&self, path: &Path, record: Record) -> Result<Path, StoreError> {
        let future = {
            let mut store = self.inner.lock().await;
            store.write(path, record)
        };
        future.await
    }
}

/// Adapts the existing synchronous `InboxStore` Store boundary. SQLite stays
/// owned by `ox-inbox`; the manager never receives its connection.
pub struct SyncStorePort<S> {
    inner: std::sync::Mutex<S>,
}

impl<S> SyncStorePort<S> {
    pub fn new(store: S) -> Self {
        Self {
            inner: std::sync::Mutex::new(store),
        }
    }
}

#[async_trait]
impl<S> StorePort for SyncStorePort<S>
where
    S: Reader + Writer + Send,
{
    async fn read(&self, path: &Path) -> Result<Option<Record>, StoreError> {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .read(path)
    }

    async fn write(&self, path: &Path, record: Record) -> Result<Path, StoreError> {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .write(path, record)
    }
}

#[async_trait]
pub trait WorkerStoreConnector: Send + Sync {
    async fn connect(&self, node: &RemoteNodeRecord) -> Result<Arc<dyn StorePort>, StoreError>;
}

use std::sync::Arc;

use ox_broker::async_store::{
    AsyncReader as BrokerAsyncReader, AsyncWriter as BrokerAsyncWriter, BoxFuture,
};
use structfs_core_store::{Error as StoreError, Path, Record};
use tokio::sync::Mutex;

/// One immutable, path-confined Store root shared by transport connections.
///
/// The mutex is held only while the broker async Store constructs its detached
/// `'static` future. It is always released before the operation is awaited.
pub struct ExportRoot<S> {
    store: Arc<Mutex<S>>,
    root: Path,
}

impl<S> ExportRoot<S> {
    pub fn new(store: S, root: Path) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            root,
        }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }
}

impl<S> Clone for ExportRoot<S> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            root: self.root.clone(),
        }
    }
}

impl<S> ExportRoot<S>
where
    S: BrokerAsyncReader + BrokerAsyncWriter + Send + 'static,
{
    pub fn read(&self, relative: Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let store = self.store.clone();
        let path = self.root.join(&relative);
        Box::pin(async move {
            let future = {
                let mut store = store.lock().await;
                store.read(&path)
            };
            future.await
        })
    }

    pub fn write(&self, relative: Path, record: Record) -> BoxFuture<Result<Path, StoreError>> {
        let store = self.store.clone();
        let root = self.root.clone();
        let path = root.join(&relative);
        Box::pin(async move {
            let future = {
                let mut store = store.lock().await;
                store.write(&path, record)
            };
            let result = future.await?;
            result.strip_prefix(&root).ok_or_else(|| {
                StoreError::store(
                    "structfs_transport",
                    "write",
                    "exported Store returned a path outside its supplied root",
                )
            })
        })
    }
}

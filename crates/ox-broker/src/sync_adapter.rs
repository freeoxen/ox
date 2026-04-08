//! SyncClientAdapter — synchronous Reader/Writer over an async ClientHandle.

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
    pub fn new(client: ClientHandle, handle: tokio::runtime::Handle) -> Self {
        Self { client, handle }
    }
}

impl Reader for SyncClientAdapter {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        tokio::task::block_in_place(|| self.handle.block_on(self.client.read(from)))
    }
}

impl Writer for SyncClientAdapter {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        tokio::task::block_in_place(|| self.handle.block_on(self.client.write(to, data)))
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

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

impl SyncClientAdapter {
    /// Write a serializable value through the sync adapter.
    pub fn write_typed<T: serde::Serialize>(
        &mut self,
        to: &Path,
        value: &T,
    ) -> Result<Path, StoreError> {
        let v = structfs_serde_store::to_value(value)
            .map_err(|e| StoreError::store("broker", "write_typed", e.to_string()))?;
        self.write(to, Record::parsed(v))
    }

    /// Read a deserializable value through the sync adapter.
    ///
    /// Returns `Ok(None)` if the path does not exist or the record has no value.
    pub fn read_typed<T: serde::de::DeserializeOwned>(
        &mut self,
        from: &Path,
    ) -> Result<Option<T>, StoreError> {
        match self.read(from)? {
            Some(record) => match record.as_value() {
                Some(value) => {
                    let typed = structfs_serde_store::from_value(value.clone())
                        .map_err(|e| StoreError::store("broker", "read_typed", e.to_string()))?;
                    Ok(Some(typed))
                }
                None => Ok(None),
            },
            None => Ok(None),
        }
    }
}

impl Reader for SyncClientAdapter {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        tracing::debug!(%from, "sync adapter read");
        tokio::task::block_in_place(|| self.handle.block_on(self.client.read(from)))
    }
}

impl Writer for SyncClientAdapter {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        tracing::debug!(%to, "sync adapter write");
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
    fn sync_adapter_write_typed_then_read_typed() {
        use serde::{Deserialize, Serialize};
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        struct Item {
            name: String,
            value: i32,
        }

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

        let client = broker.client().scoped("data");
        let mut adapter = SyncClientAdapter::new(client, rt.handle().clone());

        let item = Item {
            name: "widget".to_string(),
            value: 99,
        };
        adapter.write_typed(&path!("item"), &item).unwrap();

        let back: Option<Item> = adapter.read_typed(&path!("item")).unwrap();
        assert_eq!(back, Some(item));
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

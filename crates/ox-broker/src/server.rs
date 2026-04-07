//! ServerHandle — wraps a synchronous Reader/Writer store as an async
//! server in the broker.
//!
//! Each server runs as a tokio task. Requests arrive with an embedded
//! reply channel — the server responds directly without going back
//! through the broker.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use structfs_core_store::{Reader, Writer};

use crate::broker::BrokerInner;
use crate::types::Request;

/// Spawn a server task that wraps a synchronous store (Reader + Writer).
///
/// The store is moved into the task and exclusively owned by it.
/// Requests arrive via the broker's channel; the server responds
/// directly on the reply channel embedded in each request.
pub(crate) async fn spawn_server<S: Reader + Writer + Send + 'static>(
    inner: Arc<Mutex<BrokerInner>>,
    prefix: structfs_core_store::Path,
    store: S,
) -> tokio::task::JoinHandle<()> {
    let rx = {
        let mut inner_guard = inner.lock().await;
        inner_guard.mount(prefix)
    };

    tokio::spawn(async move {
        server_loop(store, rx).await;
    })
}

/// The server loop: receive requests, call the store, reply directly.
async fn server_loop<S: Reader + Writer>(
    mut store: S,
    mut rx: tokio::sync::mpsc::Receiver<Request>,
) {
    while let Some(request) = rx.recv().await {
        match request {
            Request::Read { path, reply } => {
                let _ = reply.send(store.read(&path));
            }
            Request::Write { path, data, reply } => {
                let _ = reply.send(store.write(&path, data));
            }
        }
    }
}

/// Spawn a server task from a store that needs a [`ClientHandle`](crate::ClientHandle)
/// for cross-store communication.
///
/// The `setup` closure receives a `ClientHandle` and returns the store.
pub(crate) async fn spawn_server_with_client<S, F>(
    inner: Arc<Mutex<BrokerInner>>,
    prefix: structfs_core_store::Path,
    timeout: Duration,
    setup: F,
) -> tokio::task::JoinHandle<()>
where
    S: Reader + Writer + Send + 'static,
    F: FnOnce(crate::ClientHandle) -> S + Send + 'static,
{
    let client = crate::ClientHandle::new(inner.clone(), timeout);
    let rx = {
        let mut inner_guard = inner.lock().await;
        inner_guard.mount(prefix)
    };

    tokio::spawn(async move {
        let store = setup(client);
        server_loop(store, rx).await;
    })
}

/// Spawn a server task that wraps an async store (AsyncReader + AsyncWriter).
///
/// Reads are resolved inline; writes are spawned as independent tasks so a
/// deferred write does not block the store from handling subsequent requests.
pub(crate) async fn spawn_async_server<
    S: crate::async_store::AsyncReader + crate::async_store::AsyncWriter,
>(
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

/// The async server loop: reads resolved inline, writes spawned as tasks.
async fn async_server_loop<S: crate::async_store::AsyncReader + crate::async_store::AsyncWriter>(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MemoryStore;
    use structfs_core_store::{Record, Value, path};

    #[tokio::test]
    async fn server_handles_read_and_write() {
        let inner = Arc::new(Mutex::new(BrokerInner::new()));
        let client = crate::ClientHandle::new(inner.clone(), Duration::from_secs(5));

        let mut store = MemoryStore::new();
        store
            .data
            .insert("greeting".to_string(), Value::String("hello".to_string()));

        let _handle = spawn_server(inner, path!("test"), store).await;

        // Read existing value
        let result = client.read(&path!("test/greeting")).await.unwrap();
        let value = result.unwrap();
        assert_eq!(
            value.as_value().unwrap(),
            &Value::String("hello".to_string())
        );

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
        assert_eq!(
            value.as_value().unwrap(),
            &Value::String("world".to_string())
        );
    }

    #[tokio::test]
    async fn multiple_servers_route_correctly() {
        let inner = Arc::new(Mutex::new(BrokerInner::new()));
        let client = crate::ClientHandle::new(inner.clone(), Duration::from_secs(5));

        let mut store_a = MemoryStore::new();
        store_a
            .data
            .insert("value".to_string(), Value::String("A".to_string()));
        let _ha = spawn_server(inner.clone(), path!("store_a"), store_a).await;

        let mut store_b = MemoryStore::new();
        store_b
            .data
            .insert("value".to_string(), Value::String("B".to_string()));
        let _hb = spawn_server(inner, path!("store_b"), store_b).await;

        let a = client.read(&path!("store_a/value")).await.unwrap().unwrap();
        assert_eq!(a.as_value().unwrap(), &Value::String("A".to_string()));

        let b = client.read(&path!("store_b/value")).await.unwrap().unwrap();
        assert_eq!(b.as_value().unwrap(), &Value::String("B".to_string()));
    }

    #[tokio::test]
    async fn scoped_client_writes_to_correct_server() {
        let inner = Arc::new(Mutex::new(BrokerInner::new()));
        let client = crate::ClientHandle::new(inner.clone(), Duration::from_secs(5));

        let _handle = spawn_server(inner, path!("threads/t_abc"), MemoryStore::new()).await;

        let scoped = client.scoped("threads/t_abc");
        scoped
            .write(
                &path!("msg"),
                Record::parsed(Value::String("hello".to_string())),
            )
            .await
            .unwrap();

        // Read via unscoped client at full path
        let result = client
            .read(&path!("threads/t_abc/msg"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result.as_value().unwrap(),
            &Value::String("hello".to_string())
        );
    }
}

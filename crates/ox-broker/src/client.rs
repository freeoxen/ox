//! ClientHandle — async read/write against the broker.
//!
//! Each client holds a shared reference to the broker state and submits
//! requests through it. The request blocks (async await) until the
//! server fulfills it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use structfs_core_store::{Error as StoreError, Path, Record, Value};
use structfs_serde_store::{DetachedTypedReader, DetachedTypedWriter};

use crate::broker::BrokerInner;
use crate::dispatching_store::DispatchingStore;

/// An async handle for reading and writing through the broker.
///
/// Clients submit requests and await responses. Multiple clients
/// can exist for the same broker.
#[derive(Clone)]
pub struct ClientHandle {
    inner: Arc<Mutex<BrokerInner>>,
    /// Optional path prefix prepended to all operations.
    scope: Option<Path>,
    /// Timeout for operations.
    timeout: Duration,
    /// Optional subscription dispatcher. When set, writes go through it
    /// (which applies the substrate write and dispatches subscriptions);
    /// when None (e.g. legacy direct construction in unit tests), writes
    /// go straight to the broker. Production `BrokerStore::client()` always
    /// installs a dispatcher (with possibly-empty registry).
    dispatcher: Option<Arc<DispatchingStore>>,
}

impl ClientHandle {
    pub(crate) fn new(inner: Arc<Mutex<BrokerInner>>, timeout: Duration) -> Self {
        Self {
            inner,
            scope: None,
            timeout,
            dispatcher: None,
        }
    }

    /// Attach a subscription dispatcher. All writes after this go through
    /// it. Called by `BrokerStore::client()`.
    pub(crate) fn with_dispatcher(mut self, dispatcher: Arc<DispatchingStore>) -> Self {
        self.dispatcher = Some(dispatcher);
        self
    }

    /// Return a clone with a different timeout.
    pub fn with_timeout(&self, timeout: Duration) -> Self {
        Self {
            inner: self.inner.clone(),
            scope: self.scope.clone(),
            timeout,
            dispatcher: self.dispatcher.clone(),
        }
    }

    /// Create a scoped client that prepends `prefix` to all paths.
    ///
    /// The scoped client sees a sub-namespace: writing to "history/append"
    /// actually writes to "{prefix}/history/append" in the broker.
    /// Scopes compose: `client.scoped("threads").scoped("t_abc")` produces
    /// a client with prefix "threads/t_abc".
    pub fn scoped(&self, prefix: &str) -> Self {
        let prefix_path = Path::parse(prefix).expect("scope prefix must be a valid path");
        let new_scope = match &self.scope {
            Some(existing) => existing.join(&prefix_path),
            None => prefix_path,
        };
        Self {
            inner: self.inner.clone(),
            scope: Some(new_scope),
            timeout: self.timeout,
            dispatcher: self.dispatcher.clone(),
        }
    }

    /// Resolve the full path by prepending the scope prefix.
    fn resolve_path(&self, path: &Path) -> Path {
        match &self.scope {
            None => path.clone(),
            Some(scope) => {
                if path.is_empty() {
                    scope.clone()
                } else {
                    scope.join(path)
                }
            }
        }
    }

    /// Async read from the broker.
    pub async fn read(&self, path: &Path) -> Result<Option<Record>, StoreError> {
        let full_path = self.resolve_path(path);
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
                StoreError::store(
                    "client",
                    "read",
                    format!("server dropped for '{}'", full_path),
                )
            })?
    }

    /// Write a serializable value to the broker.
    ///
    /// Converts `value` to a StructFS `Value` via `structfs_serde_store::to_value`,
    /// wraps it in a `Record::parsed`, and writes it.
    pub async fn write_typed<T: serde::Serialize>(
        &self,
        to: &Path,
        value: &T,
    ) -> Result<Path, StoreError> {
        self.clone().write_typed_detached(to, value).await
    }

    /// Read a deserializable value from the broker.
    ///
    /// Returns `Ok(None)` only if the path does not exist. Raw records fail with
    /// `UnsupportedFormat`; typed conversion errors retain their codec details.
    pub async fn read_typed<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        from: &Path,
    ) -> Result<Option<T>, StoreError> {
        self.clone().read_typed_detached(from).await
    }

    /// Enumerate every leaf under `prefix` as `(full_path, record)` pairs.
    ///
    /// **Why this is broker-aware code, not "read once and filter":** the
    /// `Reader` trait has no `list` operation, and the convention adopted
    /// by stores like `LocalConfig` and `ConfigStore` is "reading at the
    /// mount root returns a flat `Value::Map` keyed by sub-paths." Reading
    /// at a non-root sub-prefix (e.g. `config/gate/accounts` when only
    /// `config` is mounted) returns `None` because the store's `read`
    /// only matches exact keys.
    ///
    /// To enumerate a sub-subtree we walk **back** from `prefix` toward the
    /// empty path, calling `read` at each ancestor. The first ancestor that
    /// returns `Some(Record)` whose `Value` is a `Map` is presumed to be a
    /// mount root; its keys (relative to that ancestor) are then filtered
    /// to retain only those that share the original `prefix`'s suffix
    /// after the ancestor. Each surviving entry is inserted into the result
    /// map under its full reconstituted path.
    ///
    /// If `read(prefix)` itself returns a non-Map leaf (a single value
    /// stored at exactly the prefix), the result is a one-entry map.
    ///
    /// If no ancestor (including root) returns a Map and the prefix itself
    /// is missing, returns an empty map — the subtree is empty or unmounted.
    ///
    /// Returns `Err` only on broker / store errors; missing data is `Ok`
    /// with an empty map.
    pub async fn read_subtree(&self, prefix: &Path) -> Result<BTreeMap<Path, Record>, StoreError> {
        // Try the prefix itself first. If it resolves to a leaf (non-Map)
        // value, that's the entire subtree.
        match self.read(prefix).await {
            Ok(Some(record)) => match record.as_value() {
                Some(Value::Map(map)) => {
                    // Reading at the prefix yielded a Map directly — the
                    // prefix is a mount root. Reconstitute full paths.
                    return Ok(reconstitute(
                        prefix,
                        map.iter().map(|(k, v)| (k.as_str(), v)),
                    ));
                }
                Some(_) => {
                    // Leaf at exactly this prefix. Single-entry result.
                    let mut out = BTreeMap::new();
                    out.insert(prefix.clone(), record);
                    return Ok(out);
                }
                None => {} // Raw record without parsed value; fall through.
            },
            Ok(None) => {} // Fall through to ancestor-walk.
            Err(StoreError::NoRoute { .. }) => return Ok(BTreeMap::new()),
            Err(e) => return Err(e),
        }

        // Walk back toward root, looking for an ancestor that returns a Map.
        // `prefix.len()` decreasing to 0 covers everything from "drop one
        // component" down to the empty root path.
        for end in (0..prefix.len()).rev() {
            let ancestor = prefix.slice(0, end);
            let read_result = self.read(&ancestor).await;
            match read_result {
                Ok(Some(record)) => {
                    if let Some(Value::Map(map)) = record.as_value() {
                        // Filter to keys whose ancestor-relative path has
                        // the remaining suffix as prefix.
                        let suffix = prefix
                            .strip_prefix(&ancestor)
                            .expect("ancestor is a prefix of prefix by construction");
                        let suffix_str = suffix.to_string();
                        let prefix_match = if suffix_str.is_empty() {
                            String::new()
                        } else {
                            format!("{}/", suffix_str)
                        };
                        let filtered = map.iter().filter_map(|(k, v)| {
                            if k == &suffix_str || k.starts_with(&prefix_match) {
                                Some((k.as_str(), v))
                            } else {
                                None
                            }
                        });
                        return Ok(reconstitute(&ancestor, filtered));
                    }
                    // Ancestor exists but isn't a Map; keep walking up.
                }
                Ok(None) => continue,
                Err(StoreError::NoRoute { .. }) => return Ok(BTreeMap::new()),
                Err(e) => return Err(e),
            }
        }

        Ok(BTreeMap::new())
    }

    /// Await an accepted write without discarding its eventual reply on timeout.
    /// The caller must independently supervise this future until completion;
    /// dropping it can still lose a newly allocated handle. Subscriptions run
    /// through the same dispatcher as ordinary writes.
    pub async fn write_owned(&self, path: &Path, data: Record) -> Result<Path, StoreError> {
        let full_path = self.resolve_path(path);
        let inner = self.inner.clone();
        let target = full_path.clone();
        let record = data.clone();
        let pending = Box::pin(async move {
            let rx = inner.lock().await.submit_write(&target, record)?;
            rx.await.map_err(|_| {
                StoreError::store("client", "write", "server dropped accepted write")
            })?
        });
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.write_owned(&full_path, data, pending).await
        } else {
            pending.await
        }
    }

    /// Async write to the broker.
    ///
    /// When a subscription dispatcher is attached (the production path —
    /// `BrokerStore::client()` always attaches one), writes go through it.
    /// The dispatcher applies the substrate write and then runs matching
    /// subscriptions; the public return value mirrors the substrate write.
    pub async fn write(&self, path: &Path, data: Record) -> Result<Path, StoreError> {
        let full_path = self.resolve_path(path);
        if let Some(dispatcher) = &self.dispatcher {
            return dispatcher.write(&full_path, data).await;
        }

        // Direct path — used by unit tests that build a ClientHandle
        // without a BrokerStore. No subscription dispatch.
        let rx = {
            let mut inner = self.inner.lock().await;
            inner.submit_write(&full_path, data)?
        };

        tokio::time::timeout(self.timeout, rx)
            .await
            .map_err(|_| {
                StoreError::store(
                    "client",
                    "write",
                    format!("timeout writing '{}'", full_path),
                )
            })?
            .map_err(|_| {
                StoreError::store(
                    "client",
                    "write",
                    format!("server dropped for '{}'", full_path),
                )
            })?
    }
}

/// Reconstitute full paths from `(relative_key, value)` pairs by prepending
/// `base` to each key. Used by `read_subtree` to convert a mount-relative
/// `Value::Map` into absolute-path entries.
fn reconstitute<'a, I>(base: &Path, entries: I) -> BTreeMap<Path, Record>
where
    I: IntoIterator<Item = (&'a str, &'a Value)>,
{
    let mut out = BTreeMap::new();
    for (rel_key, val) in entries {
        // Skip keys that don't parse as paths — defensive; broker stores
        // produce well-formed keys, but a malformed one shouldn't crash
        // a snapshot build.
        let Ok(rel_path) = Path::parse(rel_key) else {
            tracing::warn!(rel_key, "read_subtree: skipping malformed key");
            continue;
        };
        let full_path = if base.is_empty() {
            rel_path
        } else {
            base.join(&rel_path)
        };
        // Nested Map values represent immediate-children projections
        // under the StructFS read-at-prefix convention. The `read_subtree`
        // contract is "all leaf entries under prefix, full paths" — so
        // recurse into Maps to flatten them out. Without this, a store
        // that returns nested Maps at non-leaf paths leaves its leaves
        // unreachable to consumers that only know `read_subtree`.
        if let Value::Map(_) = val {
            for (leaf_path, leaf_val) in flatten_value(&full_path, val) {
                out.insert(leaf_path, Record::parsed(leaf_val));
            }
        } else {
            out.insert(full_path, Record::parsed(val.clone()));
        }
    }
    out
}

/// Walk a `Value`, yielding `(path, leaf_value)` for every non-Map leaf
/// reachable from `base`. A bare leaf at `base` yields a single entry.
/// Arrays are leaves: structfs addresses through Maps only, so an
/// `Value::Array` here belongs in the flat output as-is rather than
/// being indexed into.
fn flatten_value(base: &Path, value: &Value) -> Vec<(Path, Value)> {
    let mut out = Vec::new();
    flatten_into(base, value, &mut out);
    out
}

fn flatten_into(base: &Path, value: &Value, out: &mut Vec<(Path, Value)>) {
    match value {
        Value::Map(m) => {
            for (k, v) in m {
                let Ok(seg) = Path::parse(k) else {
                    tracing::warn!(key = %k, "read_subtree: skipping malformed sub-key");
                    continue;
                };
                let child_path = if base.is_empty() {
                    seg
                } else {
                    base.join(&seg)
                };
                flatten_into(&child_path, v, out);
            }
        }
        _ => out.push((base.clone(), value.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;

    struct RecordStore(BTreeMap<Path, Record>);

    impl structfs_core_store::Reader for RecordStore {
        fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
            Ok(self.0.get(from).cloned())
        }
    }

    impl structfs_core_store::Writer for RecordStore {
        fn write(&mut self, to: &Path, record: Record) -> Result<Path, StoreError> {
            self.0.insert(to.clone(), record);
            Ok(to.clone())
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn typed_facades_preserve_presence_and_structured_errors() {
        use structfs_core_store::{CodecErrorKind, CodecOperation, Format};

        #[derive(Debug, serde::Deserialize)]
        enum Mode {
            Ready,
        }

        let broker = crate::BrokerStore::default();
        let _server = broker
            .mount(path!("data"), RecordStore(BTreeMap::new()))
            .await;
        let client = broker.client().scoped("data");
        let mut sync =
            crate::SyncClientAdapter::new(client.clone(), tokio::runtime::Handle::current());
        assert_eq!(
            client.read_typed::<i64>(&path!("missing")).await.unwrap(),
            None
        );
        assert_eq!(sync.read_typed::<i64>(&path!("missing")).unwrap(), None);
        client.write_typed(&path!("parsed"), &42_i64).await.unwrap();
        assert_eq!(
            client.read_typed::<i64>(&path!("parsed")).await.unwrap(),
            Some(42)
        );
        assert_eq!(sync.read_typed::<i64>(&path!("parsed")).unwrap(), Some(42));

        for format in [Format::JSON, Format::OCTET_STREAM] {
            client
                .write(&path!("raw"), Record::raw(b"42".to_vec(), format.clone()))
                .await
                .unwrap();
            for result in [
                client.read_typed::<i64>(&path!("raw")).await,
                sync.read_typed::<i64>(&path!("raw")),
            ] {
                assert!(
                    matches!(result, Err(StoreError::UnsupportedFormat(found)) if found == format)
                );
            }
        }

        client
            .write_typed(&path!("mode"), &"Unknown")
            .await
            .unwrap();
        for error in [
            client.read_typed::<Mode>(&path!("mode")).await.unwrap_err(),
            sync.read_typed::<Mode>(&path!("mode")).unwrap_err(),
        ] {
            let StoreError::Codec {
                kind,
                operation,
                message,
                ..
            } = error
            else {
                panic!("expected structured codec error: {error:?}");
            };
            assert_eq!(kind, CodecErrorKind::TypeMismatch);
            assert_eq!(operation, CodecOperation::Decode);
            assert!(message.contains("unknown variant"), "{message}");
            assert!(message.contains("Unknown"), "{message}");
            assert!(message.contains("Ready"), "{message}");
        }
    }

    #[tokio::test]
    async fn typed_serialization_failure_does_not_write() {
        struct Invalid;
        impl serde::Serialize for Invalid {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("intentional invalid input"))
            }
        }

        let broker = crate::BrokerStore::default();
        let _server = broker
            .mount(path!("data"), RecordStore(BTreeMap::new()))
            .await;
        let client = broker.client().scoped("data");
        client.write_typed(&path!("value"), &42_i64).await.unwrap();
        let error = client
            .write_typed(&path!("value"), &Invalid)
            .await
            .unwrap_err();
        assert!(matches!(error, StoreError::Codec { .. }));
        assert!(error.to_string().contains("intentional invalid input"));
        assert_eq!(
            client.read_typed::<i64>(&path!("value")).await.unwrap(),
            Some(42)
        );
    }

    #[tokio::test]
    async fn scoped_client_prepends_prefix() {
        let inner = Arc::new(Mutex::new(BrokerInner::new()));
        let client = ClientHandle::new(inner, Duration::from_secs(5));
        let scoped = client.scoped("threads/t_abc");

        let resolved = scoped.resolve_path(&path!("history/messages"));
        assert_eq!(resolved.to_string(), "threads/t_abc/history/messages");
    }

    #[tokio::test]
    async fn nested_scopes_compose() {
        let inner = Arc::new(Mutex::new(BrokerInner::new()));
        let client = ClientHandle::new(inner, Duration::from_secs(5));
        let scoped = client.scoped("threads").scoped("t_abc");

        let resolved = scoped.resolve_path(&path!("history"));
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

// Client handles can be mounted or exported through StructFS directly. The
// detached futures own a cloned handle and path; they never borrow the client.
impl structfs_core_store::DetachedReader for ClientHandle {
    fn read_detached(
        &mut self,
        from: &Path,
    ) -> structfs_core_store::DetachedFuture<Option<Record>> {
        let client = self.clone();
        let path = from.clone();
        Box::pin(async move { client.read(&path).await })
    }
}
impl structfs_core_store::DetachedWriter for ClientHandle {
    fn write_detached(
        &mut self,
        to: &Path,
        data: Record,
    ) -> structfs_core_store::DetachedFuture<Path> {
        let client = self.clone();
        let path = to.clone();
        Box::pin(async move { client.write(&path, data).await })
    }
}

#[cfg(test)]
mod detached_tests {
    use super::*;
    use structfs_core_store::{DetachedFuture, DetachedReader, DetachedWriter, path};

    struct DeferredRead {
        started: Arc<tokio::sync::Notify>,
        send: Option<tokio::sync::oneshot::Sender<()>>,
        recv: Option<tokio::sync::oneshot::Receiver<()>>,
    }
    impl DetachedReader for DeferredRead {
        fn read_detached(&mut self, _: &Path) -> DetachedFuture<Option<Record>> {
            let recv = self.recv.take().expect("one read");
            let started = self.started.clone();
            Box::pin(async move {
                started.notify_one();
                recv.await
                    .map_err(|e| StoreError::store("test", "read", e.to_string()))?;
                Ok(Some(Record::parsed(Value::Integer(42))))
            })
        }
    }
    impl DetachedWriter for DeferredRead {
        fn write_detached(&mut self, to: &Path, _: Record) -> DetachedFuture<Path> {
            let send = self.send.take().expect("one write");
            let path = to.clone();
            Box::pin(async move {
                let _ = send.send(());
                Ok(path)
            })
        }
    }

    #[tokio::test]
    async fn detached_typed_client_can_be_mounted_and_reused_while_read_is_parked() {
        let broker = crate::BrokerStore::new(Duration::from_secs(2));
        let started = Arc::new(tokio::sync::Notify::new());
        let (send, recv) = tokio::sync::oneshot::channel();
        broker
            .mount_async(
                path!("source"),
                DeferredRead {
                    started: started.clone(),
                    send: Some(send),
                    recv: Some(recv),
                },
            )
            .await;
        broker
            .mount_async(path!("alias"), broker.client().scoped("source"))
            .await;
        let mut client = broker.client().scoped("alias");
        let read = client.read_typed_detached::<i64>(&path!("value"));
        // A borrowing AsyncReader would keep client exclusively borrowed here.
        let write = client.write_typed_detached(&path!("release"), &());
        drop(client);
        let read = tokio::spawn(read);
        started.notified().await;
        assert_eq!(write.await.unwrap(), path!("release"));
        assert_eq!(read.await.unwrap().unwrap(), Some(42));
    }
}

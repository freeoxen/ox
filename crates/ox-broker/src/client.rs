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
        let v = structfs_serde_store::to_value(value)
            .map_err(|e| StoreError::store("broker", "write_typed", e.to_string()))?;
        self.write(to, Record::parsed(v)).await
    }

    /// Read a deserializable value from the broker.
    ///
    /// Returns `Ok(None)` if the path does not exist or the record has no value.
    pub async fn read_typed<T: serde::de::DeserializeOwned>(
        &self,
        from: &Path,
    ) -> Result<Option<T>, StoreError> {
        match self.read(from).await? {
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

//! ClientHandle — async read/write against the broker.
//!
//! Each client holds a shared reference to the broker state and submits
//! requests through it. The request blocks (async await) until the
//! server fulfills it.

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
    scope: Option<Path>,
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

    /// Async write to the broker.
    pub async fn write(&self, path: &Path, data: Record) -> Result<Path, StoreError> {
        let full_path = self.resolve_path(path);
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

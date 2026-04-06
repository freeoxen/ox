//! Core routing state machine for the broker.
//!
//! `BrokerInner` maps path prefixes to server channels and routes
//! requests to the appropriate server. Responses flow directly from
//! server to client via the reply channel embedded in each request.
//!
//! Routing uses StructFS `Path` component matching — no string
//! conversion in the hot path. Servers are sorted by prefix length
//! descending so the first `has_prefix` hit is the longest match.

use structfs_core_store::{Error as StoreError, Path, Record};
use tokio::sync::{mpsc, oneshot};

use crate::types::Request;

/// A mounted server: its prefix and channel sender.
struct MountEntry {
    prefix: Path,
    tx: mpsc::Sender<Request>,
}

/// The core routing state machine.
///
/// Servers are kept sorted by prefix component count descending.
/// Routing iterates until the first `has_prefix` match — which is
/// the longest prefix by construction.
pub(crate) struct BrokerInner {
    servers: Vec<MountEntry>,
    shut_down: bool,
}

impl BrokerInner {
    pub fn new() -> Self {
        BrokerInner {
            servers: Vec::new(),
            shut_down: false,
        }
    }

    /// Mount a server at the given prefix. Returns the receiver for
    /// requests routed to that prefix.
    pub fn mount(&mut self, prefix: Path) -> mpsc::Receiver<Request> {
        let (tx, rx) = mpsc::channel(64);
        self.servers.push(MountEntry { prefix, tx });
        self.servers
            .sort_by(|a, b| b.prefix.len().cmp(&a.prefix.len()));
        rx
    }

    /// Remove a server at the given prefix.
    pub fn unmount(&mut self, prefix: &Path) {
        self.servers.retain(|entry| entry.prefix != *prefix);
    }

    /// Find the server with the longest matching prefix for the given path.
    ///
    /// Returns a clone of the server sender and the sub-path with prefix
    /// stripped. Because servers are sorted longest-first, the first match
    /// is the longest prefix.
    ///
    /// Cloning the `mpsc::Sender` is an `Arc` refcount bump — the cost
    /// of decoupling the route lookup from the mutable submit that follows.
    fn route(&self, path: &Path) -> Option<(mpsc::Sender<Request>, Path)> {
        for entry in &self.servers {
            if entry.prefix.is_empty() || path.has_prefix(&entry.prefix) {
                let sub_path = if entry.prefix.is_empty() {
                    path.clone()
                } else {
                    path.strip_prefix(&entry.prefix)
                        .unwrap_or_else(|| Path::from_components(vec![]))
                };
                return Some((entry.tx.clone(), sub_path));
            }
        }
        None
    }

    /// Submit a read request, routing it to the appropriate server.
    ///
    /// The reply channel is embedded in the request — the server responds
    /// directly. Returns the receiver end for the caller to await.
    pub fn submit_read(
        &mut self,
        path: &Path,
    ) -> Result<oneshot::Receiver<Result<Option<Record>, StoreError>>, StoreError> {
        if self.shut_down {
            return Err(StoreError::store("broker", "read", "broker is shut down"));
        }

        let (server_tx, sub_path) = self
            .route(path)
            .ok_or_else(|| StoreError::NoRoute { path: path.clone() })?;

        let (reply_tx, reply_rx) = oneshot::channel();
        let request = Request::Read {
            path: sub_path,
            reply: reply_tx,
        };

        server_tx
            .try_send(request)
            .map_err(|_| StoreError::store("broker", "read", "server channel full"))?;

        Ok(reply_rx)
    }

    /// Submit a write request, routing it to the appropriate server.
    pub fn submit_write(
        &mut self,
        path: &Path,
        data: Record,
    ) -> Result<oneshot::Receiver<Result<Path, StoreError>>, StoreError> {
        if self.shut_down {
            return Err(StoreError::store("broker", "write", "broker is shut down"));
        }

        let (server_tx, sub_path) = self
            .route(path)
            .ok_or_else(|| StoreError::NoRoute { path: path.clone() })?;

        let (reply_tx, reply_rx) = oneshot::channel();
        let request = Request::Write {
            path: sub_path,
            data,
            reply: reply_tx,
        };

        server_tx
            .try_send(request)
            .map_err(|_| StoreError::store("broker", "write", "server channel full"))?;

        Ok(reply_rx)
    }

    /// Shut down the broker, rejecting all future requests.
    pub fn shut_down(&mut self) {
        self.shut_down = true;
        self.servers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;

    #[test]
    fn mount_and_route() {
        let mut inner = BrokerInner::new();
        let _rx = inner.mount(path!("ui"));
        let (_, sub_path) = inner.route(&path!("ui/selected_row")).unwrap();
        assert_eq!(sub_path.to_string(), "selected_row");
    }

    #[test]
    fn longest_prefix_wins() {
        let mut inner = BrokerInner::new();
        let _rx1 = inner.mount(path!("threads"));
        let _rx2 = inner.mount(path!("threads/t_abc"));
        let (_, sub_path) = inner
            .route(&path!("threads/t_abc/history/messages"))
            .unwrap();
        assert_eq!(sub_path.to_string(), "history/messages");
    }

    #[test]
    fn no_route_returns_none() {
        let inner = BrokerInner::new();
        assert!(inner.route(&path!("nonexistent/path")).is_none());
    }

    #[test]
    fn unmount_removes_route() {
        let mut inner = BrokerInner::new();
        let _rx = inner.mount(path!("ui"));
        assert!(inner.route(&path!("ui/mode")).is_some());
        inner.unmount(&path!("ui"));
        assert!(inner.route(&path!("ui/mode")).is_none());
    }

    #[test]
    fn shut_down_rejects_new_requests() {
        let mut inner = BrokerInner::new();
        let _rx = inner.mount(path!("ui"));
        inner.shut_down();
        let result = inner.submit_read(&path!("ui/mode"));
        assert!(result.is_err());
    }

    #[test]
    fn backpressure_when_channel_full() {
        let mut inner = BrokerInner::new();
        let _rx = inner.mount(path!("ui")); // hold rx, never read from it

        // Fill the channel (capacity 64)
        for i in 0..64 {
            let result = inner.submit_read(&path!("ui/mode"));
            assert!(result.is_ok(), "request {} should succeed", i);
        }

        // 65th should fail — channel is full
        let result = inner.submit_read(&path!("ui/mode"));
        assert!(result.is_err());
    }

    #[test]
    fn root_mount_catches_all() {
        let mut inner = BrokerInner::new();
        let _rx = inner.mount(Path::from_components(vec![]));
        let (_, sub_path) = inner.route(&path!("anything/at/all")).unwrap();
        assert_eq!(sub_path.to_string(), "anything/at/all");
    }

    #[test]
    fn specific_prefix_wins_over_root() {
        let mut inner = BrokerInner::new();
        let _rx_root = inner.mount(Path::from_components(vec![]));
        let _rx_ui = inner.mount(path!("ui"));
        let (_, sub_path) = inner.route(&path!("ui/mode")).unwrap();
        // "ui" mount should win over root, stripping the prefix
        assert_eq!(sub_path.to_string(), "mode");
    }
}

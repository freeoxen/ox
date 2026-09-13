//! Core routing state machine for the broker.
//!
//! `BrokerInner` maps path prefixes to server channels and routes
//! requests to the appropriate server. Responses flow directly from
//! server to client via the reply channel embedded in each request.
//!
//! Routing uses StructFS `Path` component matching — no string
//! conversion in the hot path. Upstream PathTrie finds the deepest mounted
//! ancestor in time proportional to the path depth.

use structfs_core_store::{Error as StoreError, Path, PathTrie, Record};
use tokio::sync::{mpsc, oneshot};

use crate::types::Request;

/// The core routing state machine.
///
/// A trie node retains registrations in order so duplicate mounts preserve
/// the existing first-registration-wins behavior until explicitly unmounted.
pub(crate) struct BrokerInner {
    servers: PathTrie<Vec<mpsc::Sender<Request>>>,
    shut_down: bool,
}

impl BrokerInner {
    pub fn new() -> Self {
        BrokerInner {
            servers: PathTrie::new(),
            shut_down: false,
        }
    }

    /// Mount a server at the given prefix. Returns the receiver for
    /// requests routed to that prefix.
    pub fn mount(&mut self, prefix: Path) -> mpsc::Receiver<Request> {
        let (tx, rx) = mpsc::channel(64);
        match self.servers.get_mut(&prefix) {
            Some(registrations) => registrations.push(tx),
            None => {
                self.servers.insert(&prefix, vec![tx]);
            }
        }
        rx
    }

    /// Remove a server at the given prefix.
    pub fn unmount(&mut self, prefix: &Path) {
        self.servers.remove(prefix);
        // Exact removal leaves trie nodes behind. Prune empty branches so
        // repeated mounting of transient conversations does not retain paths.
        let mut branch = prefix.clone();
        while self
            .servers
            .get_subtrie(&branch)
            .is_some_and(PathTrie::is_empty)
        {
            self.servers.remove_subtree(&branch);
            if branch.is_empty() {
                break;
            }
            branch = branch.slice(0, branch.len() - 1);
        }
    }

    /// Find the deepest mounted ancestor and return its mount-relative suffix.
    fn route(&self, path: &Path) -> Option<(mpsc::Sender<Request>, Path)> {
        let (registrations, suffix) = self.servers.find_ancestor(path)?;
        Some((registrations.first()?.clone(), suffix))
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
        self.servers = PathTrie::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;

    #[test]
    fn duplicate_mount_precedence_and_exact_unmount_are_preserved() {
        let mut broker = BrokerInner::new();
        let _root = broker.mount(path!());
        let _first = broker.mount(path!("a"));
        let (first, _) = broker.route(&path!("a/key")).unwrap();
        let _second = broker.mount(path!("a"));
        let _child = broker.mount(path!("a/b"));
        let (selected, suffix) = broker.route(&path!("a/key")).unwrap();
        assert!(first.same_channel(&selected));
        assert_eq!(suffix, path!("key"));
        let (child, _) = broker.route(&path!("a/b/key")).unwrap();
        broker.unmount(&path!("a"));
        let (selected, suffix) = broker.route(&path!("a/b/key")).unwrap();
        assert!(
            child.same_channel(&selected),
            "exact unmount retains child mounts"
        );
        assert_eq!(suffix, path!("key"));
        let (selected, suffix) = broker.route(&path!("a/key")).unwrap();
        assert!(!first.same_channel(&selected), "parent falls back to root");
        assert_eq!(suffix, path!("a/key"));
        broker.unmount(&path!("a/b"));
        assert!(
            broker.servers.get_subtrie(&path!("a")).is_none(),
            "transient mount paths must not accumulate empty trie nodes"
        );
    }
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

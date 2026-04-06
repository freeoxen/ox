//! Core routing state machine for the broker.
//!
//! `BrokerInner` maps path prefixes to server channels and routes
//! requests to the appropriate server. Responses flow directly from
//! server to client via the reply channel embedded in each request.

use std::collections::BTreeMap;

use structfs_core_store::{Error as StoreError, Path, Record};
use tokio::sync::{mpsc, oneshot};

use crate::types::Request;

/// Sender half of a server channel.
pub(crate) type ServerTx = mpsc::Sender<Request>;

/// The core routing state machine.
///
/// Maps path prefixes to server channels. Each request carries its own
/// reply channel, so the broker only handles routing — not response
/// matching.
pub(crate) struct BrokerInner {
    servers: BTreeMap<String, ServerTx>,
    shut_down: bool,
}

impl BrokerInner {
    pub fn new() -> Self {
        BrokerInner {
            servers: BTreeMap::new(),
            shut_down: false,
        }
    }

    /// Mount a server at the given prefix. Returns the receiver for
    /// requests routed to that prefix.
    pub fn mount(&mut self, prefix: &str) -> mpsc::Receiver<Request> {
        let (tx, rx) = mpsc::channel(64);
        self.servers.insert(prefix.to_string(), tx);
        rx
    }

    /// Remove a server from the given prefix.
    pub fn unmount(&mut self, prefix: &str) {
        self.servers.remove(prefix);
    }

    /// Find the server with the longest matching prefix for the given path.
    ///
    /// Returns the server's sender and the sub-path (path with prefix stripped).
    fn route(&self, path: &Path) -> Option<(&ServerTx, Path)> {
        let path_str = path.to_string();
        let mut best: Option<(&str, &ServerTx)> = None;

        for (prefix, tx) in &self.servers {
            let matches = prefix.is_empty()
                || path_str == *prefix
                || (path_str.starts_with(prefix.as_str())
                    && path_str.as_bytes().get(prefix.len()) == Some(&b'/'));

            if matches {
                match best {
                    None => best = Some((prefix.as_str(), tx)),
                    Some((current_prefix, _)) if prefix.len() > current_prefix.len() => {
                        best = Some((prefix.as_str(), tx));
                    }
                    _ => {}
                }
            }
        }

        best.map(|(prefix, tx)| {
            let sub_path = if prefix.is_empty() {
                path.clone()
            } else {
                let prefix_path = Path::parse(prefix).expect("mounted prefix must be valid");
                path.strip_prefix(&prefix_path).unwrap_or_else(|| {
                    Path::from_components(vec![])
                })
            };
            (tx, sub_path)
        })
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

        let (server_tx, sub_path) = {
            let (tx, sp) = self
                .route(path)
                .ok_or_else(|| StoreError::NoRoute { path: path.clone() })?;
            (tx.clone(), sp)
        };

        let (reply_tx, reply_rx) = oneshot::channel();
        let request = Request::Read {
            path: sub_path,
            reply: reply_tx,
        };

        server_tx.try_send(request).map_err(|_| {
            StoreError::store("broker", "read", "server channel unavailable")
        })?;

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

        let (server_tx, sub_path) = {
            let (tx, sp) = self
                .route(path)
                .ok_or_else(|| StoreError::NoRoute { path: path.clone() })?;
            (tx.clone(), sp)
        };

        let (reply_tx, reply_rx) = oneshot::channel();
        let request = Request::Write {
            path: sub_path,
            data,
            reply: reply_tx,
        };

        server_tx.try_send(request).map_err(|_| {
            StoreError::store("broker", "write", "server channel unavailable")
        })?;

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
        let _rx = inner.mount("ui");
        let (_server_tx, sub_path) = inner.route(&path!("ui/selected_row")).unwrap();
        assert_eq!(sub_path.to_string(), "selected_row");
    }

    #[test]
    fn longest_prefix_wins() {
        let mut inner = BrokerInner::new();
        let _rx1 = inner.mount("threads");
        let _rx2 = inner.mount("threads/t_abc");
        let (_, sub_path) = inner.route(&path!("threads/t_abc/history/messages")).unwrap();
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
        let _rx = inner.mount("ui");
        assert!(inner.route(&path!("ui/mode")).is_some());
        inner.unmount("ui");
        assert!(inner.route(&path!("ui/mode")).is_none());
    }

    #[test]
    fn shut_down_rejects_new_requests() {
        let mut inner = BrokerInner::new();
        let _rx = inner.mount("ui");
        inner.shut_down();
        let result = inner.submit_read(&path!("ui/mode"));
        assert!(result.is_err());
    }
}

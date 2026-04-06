//! Core routing state machine for the broker.
//!
//! `BrokerInner` maps path prefixes to server channels, queues requests,
//! and matches responses back to waiting clients.

use std::collections::{BTreeMap, HashMap};

use structfs_core_store::{Error as StoreError, Path, Record};
use tokio::sync::{mpsc, oneshot};

use crate::types::{Request, RequestKind, Response};

/// Monotonic action identifier.
pub(crate) type ActionId = u64;

/// Sender half of a server channel.
pub(crate) type ServerTx = mpsc::Sender<Request>;

/// A pending action waiting for a server response.
pub(crate) enum Action {
    Read {
        path: Path,
        tx: oneshot::Sender<Result<Option<Record>, StoreError>>,
    },
    Write {
        path: Path,
        data: Record,
        tx: oneshot::Sender<Result<Path, StoreError>>,
    },
}

/// The core routing state machine.
///
/// Maps path prefixes to server channels, queues requests, and matches
/// responses to waiting clients via oneshot channels.
pub(crate) struct BrokerInner {
    next_action_id: u64,
    actions: HashMap<ActionId, Action>,
    servers: BTreeMap<String, ServerTx>,
    shut_down: bool,
}

impl BrokerInner {
    pub fn new() -> Self {
        BrokerInner {
            next_action_id: 0,
            actions: HashMap::new(),
            servers: BTreeMap::new(),
            shut_down: false,
        }
    }

    /// Mount a server at the given prefix. Returns the receiver for requests
    /// routed to that prefix.
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
    /// Returns the server sender and the sub-path (path with prefix stripped).
    pub fn route(&self, path: &Path) -> Option<(&ServerTx, Path)> {
        let path_str = path.to_string();
        let mut best: Option<(&str, &ServerTx)> = None;

        for (prefix, tx) in &self.servers {
            let matches = path_str == *prefix
                || path_str.starts_with(&format!("{}/", prefix))
                || prefix.is_empty();

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
                    // Exact match case: path equals prefix, remainder is empty
                    Path::from_components(vec![])
                })
            };
            (tx, sub_path)
        })
    }

    /// Submit a read request, routing it to the appropriate server.
    ///
    /// Returns a oneshot receiver that will contain the result.
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

        let action_id = self.next_action_id;
        self.next_action_id += 1;

        let (tx, rx) = oneshot::channel();
        let request = Request {
            action_id,
            kind: RequestKind::Read,
            path: sub_path,
        };

        server_tx.try_send(request).map_err(|_| {
            StoreError::store("broker", "read", "server channel unavailable")
        })?;

        self.actions.insert(action_id, Action::Read { path: path.clone(), tx });
        Ok(rx)
    }

    /// Submit a write request, routing it to the appropriate server.
    ///
    /// Returns a oneshot receiver that will contain the result.
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

        let action_id = self.next_action_id;
        self.next_action_id += 1;

        let (tx, rx) = oneshot::channel();
        let request = Request {
            action_id,
            kind: RequestKind::Write(data.clone()),
            path: sub_path,
        };

        server_tx.try_send(request).map_err(|_| {
            StoreError::store("broker", "write", "server channel unavailable")
        })?;

        self.actions.insert(action_id, Action::Write { path: path.clone(), data, tx });
        Ok(rx)
    }

    /// Resolve a pending action with the server's response.
    pub fn resolve(&mut self, action_id: ActionId, response: Response) {
        if let Some(action) = self.actions.remove(&action_id) {
            match (action, response) {
                (Action::Read { tx, .. }, Response::Read(result)) => {
                    let _ = tx.send(result);
                }
                (Action::Write { tx, .. }, Response::Write(result)) => {
                    let _ = tx.send(result);
                }
                // Mismatched action/response type: drop silently
                _ => {}
            }
        }
    }

    /// Shut down the broker: reject new requests and drain all pending actions.
    pub fn shut_down(&mut self) {
        self.shut_down = true;

        // Drain all pending actions with shutdown errors
        let action_ids: Vec<ActionId> = self.actions.keys().copied().collect();
        for action_id in action_ids {
            if let Some(action) = self.actions.remove(&action_id) {
                match action {
                    Action::Read { tx, .. } => {
                        let _ = tx.send(Err(StoreError::store(
                            "broker",
                            "read",
                            "broker is shut down",
                        )));
                    }
                    Action::Write { tx, .. } => {
                        let _ = tx.send(Err(StoreError::store(
                            "broker",
                            "write",
                            "broker is shut down",
                        )));
                    }
                }
            }
        }

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

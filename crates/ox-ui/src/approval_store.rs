//! ApprovalStore — per-thread approval request/response state (async, deferred).
//!
//! The agent writes to "request" to post an approval request — the returned
//! future blocks until the TUI writes to "response" with a decision.
//! The TUI reads "pending" to discover the current request.

use ox_broker::async_store::{AsyncReader, AsyncWriter, BoxFuture};
use ox_types::ApprovalRequest;
use structfs_core_store::{Error as StoreError, Path, Record, Value};

pub struct ApprovalStore {
    pending: Option<ApprovalRequest>,
    deferred_tx: Option<tokio::sync::oneshot::Sender<ox_types::Decision>>,
}

impl ApprovalStore {
    pub fn new() -> Self {
        ApprovalStore {
            pending: None,
            deferred_tx: None,
        }
    }

    /// Get the tool name from the current pending request, if any.
    pub fn pending_tool_name(&self) -> Option<String> {
        self.pending.as_ref().map(|r| r.tool_name.clone())
    }
}

impl Default for ApprovalStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncReader for ApprovalStore {
    fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let key = if from.is_empty() {
            ""
        } else {
            from.components[0].as_str()
        };
        let result = match key {
            "pending" => match &self.pending {
                Some(req) => match structfs_serde_store::to_value(req) {
                    Ok(v) => Ok(Some(Record::parsed(v))),
                    Err(e) => Err(StoreError::store("approval", "pending", e.to_string())),
                },
                None => Ok(Some(Record::parsed(Value::Null))),
            },
            _ => Ok(None),
        };
        Box::pin(std::future::ready(result))
    }
}

impl AsyncWriter for ApprovalStore {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
        let action = if to.is_empty() {
            ""
        } else {
            to.components[0].as_str()
        };
        let value = match data.as_value() {
            Some(v) => v.clone(),
            None => {
                return Box::pin(std::future::ready(Err(StoreError::store(
                    "approval",
                    "write",
                    "write data must contain a value",
                ))));
            }
        };

        match action {
            "request" => {
                let req: ApprovalRequest = match structfs_serde_store::from_value(value) {
                    Ok(r) => r,
                    Err(e) => {
                        return Box::pin(std::future::ready(Err(StoreError::store(
                            "approval",
                            "request",
                            e.to_string(),
                        ))));
                    }
                };

                self.pending = Some(req);

                let (tx, rx) = tokio::sync::oneshot::channel::<ox_types::Decision>();
                self.deferred_tx = Some(tx);

                Box::pin(async move {
                    // Block until the response arrives via the oneshot channel.
                    // The decision is encoded in the returned path so the
                    // caller can parse it (e.g. "request/allow_once").
                    let decision = rx.await.map_err(|_| {
                        StoreError::store(
                            "approval",
                            "request",
                            "response channel dropped without a response",
                        )
                    })?;
                    Ok(Path::from_components(vec![
                        "request".to_string(),
                        decision.as_str().to_string(),
                    ]))
                })
            }
            "response" => {
                let resp: ox_types::ApprovalResponse =
                    match structfs_serde_store::from_value(value.clone()) {
                        Ok(r) => r,
                        Err(e) => {
                            return Box::pin(std::future::ready(Err(StoreError::store(
                                "approval",
                                "response",
                                e.to_string(),
                            ))));
                        }
                    };
                let decision = resp.decision;

                // Unblock the deferred request future
                if let Some(tx) = self.deferred_tx.take() {
                    let _ = tx.send(decision);
                }
                self.pending = None;

                let path = to.clone();
                Box::pin(std::future::ready(Ok(path)))
            }
            _ => Box::pin(std::future::ready(Err(StoreError::store(
                "approval",
                "write",
                "unknown approval path",
            )))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use structfs_core_store::path;

    #[tokio::test]
    async fn initial_state_has_no_pending() {
        let mut store = ApprovalStore::new();
        let pending = store.read(&path!("pending")).await.unwrap().unwrap();
        assert_eq!(pending.as_value().unwrap(), &Value::Null);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn request_creates_pending_and_blocks() {
        let mut store = ApprovalStore::new();
        let mut map = BTreeMap::new();
        map.insert("tool_name".to_string(), Value::String("bash".to_string()));
        let mut input_map = BTreeMap::new();
        input_map.insert("command".to_string(), Value::String("ls -la".to_string()));
        map.insert("tool_input".to_string(), Value::Map(input_map));

        // Write request — capture the deferred future but don't await yet
        let deferred = store.write(&path!("request"), Record::parsed(Value::Map(map)));

        // Pending should be set
        let pending = store.read(&path!("pending")).await.unwrap().unwrap();
        let m = match pending.as_value().unwrap() {
            Value::Map(m) => m,
            _ => panic!("expected map"),
        };
        assert_eq!(
            m.get("tool_name").unwrap(),
            &Value::String("bash".to_string())
        );

        // Write response to unblock
        store
            .write(
                &path!("response"),
                Record::parsed(
                    structfs_serde_store::to_value(&ox_types::ApprovalResponse {
                        decision: ox_types::Decision::AllowOnce,
                    })
                    .unwrap(),
                ),
            )
            .await
            .unwrap();

        // Now the deferred future should resolve
        let result = deferred.await;
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn response_clears_pending() {
        let mut store = ApprovalStore::new();
        let mut map = BTreeMap::new();
        map.insert("tool_name".to_string(), Value::String("bash".to_string()));
        let _deferred = store.write(&path!("request"), Record::parsed(Value::Map(map)));

        store
            .write(
                &path!("response"),
                Record::parsed(
                    structfs_serde_store::to_value(&ox_types::ApprovalResponse {
                        decision: ox_types::Decision::AllowOnce,
                    })
                    .unwrap(),
                ),
            )
            .await
            .unwrap();

        // Pending is cleared
        let pending = store.read(&path!("pending")).await.unwrap().unwrap();
        assert_eq!(pending.as_value().unwrap(), &Value::Null);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn request_clears_previous_deferred() {
        let mut store = ApprovalStore::new();

        // First request
        let mut map = BTreeMap::new();
        map.insert("tool_name".to_string(), Value::String("bash".to_string()));
        map.insert("tool_input".to_string(), Value::Map(BTreeMap::new()));
        let first_deferred = store.write(&path!("request"), Record::parsed(Value::Map(map)));

        // Second request overwrites; first sender is dropped
        let mut map2 = BTreeMap::new();
        map2.insert("tool_name".to_string(), Value::String("write".to_string()));
        map2.insert("tool_input".to_string(), Value::Map(BTreeMap::new()));
        let _second_deferred = store.write(&path!("request"), Record::parsed(Value::Map(map2)));

        // The first deferred should error (sender dropped)
        let result = first_deferred.await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn pending_tool_name_returns_name() {
        let mut store = ApprovalStore::new();
        assert_eq!(store.pending_tool_name(), None);

        let mut map = BTreeMap::new();
        map.insert("tool_name".to_string(), Value::String("bash".to_string()));
        let mut input = BTreeMap::new();
        input.insert("command".to_string(), Value::String("ls".to_string()));
        map.insert("tool_input".to_string(), Value::Map(input));
        let _deferred = store.write(&path!("request"), Record::parsed(Value::Map(map)));

        assert_eq!(store.pending_tool_name(), Some("bash".to_string()));
    }
}

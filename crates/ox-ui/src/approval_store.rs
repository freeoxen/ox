//! ApprovalStore — per-thread approval request/response state (async, deferred).
//!
//! The agent writes to "request" to post an approval request — the returned
//! future blocks until the TUI writes to "response" with a decision.
//! The TUI reads "pending" to discover the current request.

use std::collections::BTreeMap;

use ox_broker::async_store::{AsyncReader, AsyncWriter, BoxFuture};
use structfs_core_store::{Error as StoreError, Path, Record, Value};

/// An approval request from the agent.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub tool_name: String,
    pub input_preview: String,
}

pub struct ApprovalStore {
    pending: Option<ApprovalRequest>,
    deferred_tx: Option<tokio::sync::oneshot::Sender<String>>,
}

impl ApprovalStore {
    pub fn new() -> Self {
        ApprovalStore {
            pending: None,
            deferred_tx: None,
        }
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
            "pending" => Ok(Some(Record::parsed(match &self.pending {
                Some(req) => {
                    let mut map = BTreeMap::new();
                    map.insert(
                        "tool_name".to_string(),
                        Value::String(req.tool_name.clone()),
                    );
                    map.insert(
                        "input_preview".to_string(),
                        Value::String(req.input_preview.clone()),
                    );
                    Value::Map(map)
                }
                None => Value::Null,
            }))),
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
                let map = match value {
                    Value::Map(m) => m,
                    _ => {
                        return Box::pin(std::future::ready(Err(StoreError::store(
                            "approval",
                            "request",
                            "request must be a Map with tool_name and input_preview",
                        ))));
                    }
                };
                let tool_name = match map.get("tool_name") {
                    Some(Value::String(s)) => s.clone(),
                    _ => {
                        return Box::pin(std::future::ready(Err(StoreError::store(
                            "approval",
                            "request",
                            "missing tool_name",
                        ))));
                    }
                };
                let input_preview = match map.get("input_preview") {
                    Some(Value::String(s)) => s.clone(),
                    _ => String::new(),
                };

                self.pending = Some(ApprovalRequest {
                    tool_name,
                    input_preview,
                });

                let (tx, rx) = tokio::sync::oneshot::channel::<String>();
                self.deferred_tx = Some(tx);

                Box::pin(async move {
                    // Block until the response arrives via the oneshot channel.
                    // The decision string is encoded in the returned path so the
                    // caller can parse it (e.g. "request/allow_once").
                    let decision = rx.await.map_err(|_| {
                        StoreError::store(
                            "approval",
                            "request",
                            "response channel dropped without a response",
                        )
                    })?;
                    Ok(Path::from_components(vec!["request".to_string(), decision]))
                })
            }
            "response" => {
                let decision = match value {
                    Value::String(s) => s,
                    _ => {
                        return Box::pin(std::future::ready(Err(StoreError::store(
                            "approval",
                            "response",
                            "response must be a String decision",
                        ))));
                    }
                };

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
        map.insert(
            "input_preview".to_string(),
            Value::String("ls -la".to_string()),
        );

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
                Record::parsed(Value::String("allow_once".to_string())),
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
                Record::parsed(Value::String("allow_once".to_string())),
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
        let first_deferred = store.write(&path!("request"), Record::parsed(Value::Map(map)));

        // Second request overwrites; first sender is dropped
        let mut map2 = BTreeMap::new();
        map2.insert("tool_name".to_string(), Value::String("write".to_string()));
        let _second_deferred = store.write(&path!("request"), Record::parsed(Value::Map(map2)));

        // The first deferred should error (sender dropped)
        let result = first_deferred.await;
        assert!(result.is_err());
    }
}

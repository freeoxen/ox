//! ApprovalStore — per-thread approval request/response state.
//!
//! The agent writes to "request" to post an approval request.
//! The TUI reads "pending" to discover requests.
//! The user writes to "response" to post a decision.
//! The agent reads "response" to get the decision.

use std::collections::BTreeMap;

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

/// An approval request from the agent.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub tool_name: String,
    pub input_preview: String,
}

pub struct ApprovalStore {
    pending: Option<ApprovalRequest>,
    response: Option<String>,
}

impl ApprovalStore {
    pub fn new() -> Self {
        ApprovalStore {
            pending: None,
            response: None,
        }
    }
}

impl Default for ApprovalStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Reader for ApprovalStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = if from.is_empty() {
            ""
        } else {
            from.components[0].as_str()
        };
        match key {
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
            "response" => Ok(Some(Record::parsed(match &self.response {
                Some(decision) => Value::String(decision.clone()),
                None => Value::Null,
            }))),
            _ => Ok(None),
        }
    }
}

impl Writer for ApprovalStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let action = if to.is_empty() {
            ""
        } else {
            to.components[0].as_str()
        };
        let value = data.as_value().ok_or_else(|| {
            StoreError::store("approval", "write", "write data must contain a value")
        })?;

        match action {
            "request" => {
                let map = match value {
                    Value::Map(m) => m,
                    _ => {
                        return Err(StoreError::store(
                            "approval",
                            "request",
                            "request must be a Map with tool_name and input_preview",
                        ))
                    }
                };
                let tool_name = map
                    .get("tool_name")
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        StoreError::store("approval", "request", "missing tool_name")
                    })?;
                let input_preview = map
                    .get("input_preview")
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();

                self.pending = Some(ApprovalRequest {
                    tool_name,
                    input_preview,
                });
                self.response = None;
                Ok(to.clone())
            }
            "response" => {
                let decision = match value {
                    Value::String(s) => s.clone(),
                    _ => {
                        return Err(StoreError::store(
                            "approval",
                            "response",
                            "response must be a String decision",
                        ))
                    }
                };
                self.response = Some(decision);
                self.pending = None;
                Ok(to.clone())
            }
            _ => Err(StoreError::store(
                "approval",
                "write",
                "unknown approval path",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;

    #[test]
    fn initial_state_has_no_pending() {
        let mut store = ApprovalStore::new();
        let pending = store.read(&path!("pending")).unwrap().unwrap();
        assert_eq!(pending.as_value().unwrap(), &Value::Null);
    }

    #[test]
    fn request_creates_pending() {
        let mut store = ApprovalStore::new();
        let mut map = BTreeMap::new();
        map.insert("tool_name".to_string(), Value::String("bash".to_string()));
        map.insert(
            "input_preview".to_string(),
            Value::String("ls -la".to_string()),
        );
        store
            .write(&path!("request"), Record::parsed(Value::Map(map)))
            .unwrap();

        let pending = store.read(&path!("pending")).unwrap().unwrap();
        let m = match pending.as_value().unwrap() {
            Value::Map(m) => m,
            _ => panic!("expected map"),
        };
        assert_eq!(
            m.get("tool_name").unwrap(),
            &Value::String("bash".to_string())
        );
    }

    #[test]
    fn response_clears_pending() {
        let mut store = ApprovalStore::new();
        let mut map = BTreeMap::new();
        map.insert("tool_name".to_string(), Value::String("bash".to_string()));
        store
            .write(&path!("request"), Record::parsed(Value::Map(map)))
            .unwrap();

        store
            .write(
                &path!("response"),
                Record::parsed(Value::String("allow_once".to_string())),
            )
            .unwrap();

        // Pending is cleared
        let pending = store.read(&path!("pending")).unwrap().unwrap();
        assert_eq!(pending.as_value().unwrap(), &Value::Null);

        // Response is available
        let resp = store.read(&path!("response")).unwrap().unwrap();
        assert_eq!(
            resp.as_value().unwrap(),
            &Value::String("allow_once".to_string())
        );
    }

    #[test]
    fn request_clears_previous_response() {
        let mut store = ApprovalStore::new();

        // First cycle
        let mut map = BTreeMap::new();
        map.insert("tool_name".to_string(), Value::String("bash".to_string()));
        store
            .write(&path!("request"), Record::parsed(Value::Map(map)))
            .unwrap();
        store
            .write(
                &path!("response"),
                Record::parsed(Value::String("allow_once".to_string())),
            )
            .unwrap();

        // Second request clears old response
        let mut map2 = BTreeMap::new();
        map2.insert("tool_name".to_string(), Value::String("write".to_string()));
        store
            .write(&path!("request"), Record::parsed(Value::Map(map2)))
            .unwrap();

        let resp = store.read(&path!("response")).unwrap().unwrap();
        assert_eq!(resp.as_value().unwrap(), &Value::Null);
    }
}

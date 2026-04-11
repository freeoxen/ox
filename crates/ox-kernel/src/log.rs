//! Structured log store — append-only record of all agent activity.
//!
//! Every LLM response, tool call, tool result, and meta event is recorded
//! as a [`LogEntry`]. The log is the source of truth; history views are
//! projections over it (follow-up work).

use crate::ContentBlock;
use serde::{Deserialize, Serialize};
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

/// Source metadata for an assistant response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSource {
    pub account: String,
    pub model: String,
}

/// A single entry in the structured log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LogEntry {
    #[serde(rename = "user")]
    User { content: String },

    #[serde(rename = "assistant")]
    Assistant {
        content: Vec<ContentBlock>,
        #[serde(skip_serializing_if = "Option::is_none")]
        source: Option<LogSource>,
    },

    #[serde(rename = "tool_call")]
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        id: String,
        output: serde_json::Value,
        #[serde(default)]
        is_error: bool,
    },

    #[serde(rename = "meta")]
    Meta { data: serde_json::Value },
}

/// Append-only structured log implementing StructFS Reader/Writer.
///
/// Read paths:
/// - `""` or `"entries"` → all entries as JSON array
/// - `"count"` → entry count as Integer
/// - `"last/{n}"` → last n entries as JSON array
///
/// Write paths:
/// - `""` or `"append"` → deserialize Value as LogEntry, append
pub struct LogStore {
    entries: Vec<LogEntry>,
}

impl LogStore {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn entries(&self) -> &[LogEntry] {
        &self.entries
    }
}

impl Default for LogStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Reader for LogStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = if from.is_empty() {
            "entries"
        } else {
            from.components[0].as_str()
        };

        match key {
            "entries" => {
                let json = serde_json::to_value(&self.entries)
                    .map_err(|e| StoreError::store("LogStore", "read", e.to_string()))?;
                Ok(Some(Record::parsed(
                    structfs_serde_store::json_to_value(json),
                )))
            }
            "count" => Ok(Some(Record::parsed(Value::Integer(
                self.entries.len() as i64,
            )))),
            "last" => {
                if from.len() < 2 {
                    return Err(StoreError::store(
                        "LogStore",
                        "read",
                        "last requires a count: last/{n}",
                    ));
                }
                let n: usize = from.components[1]
                    .parse()
                    .map_err(|e: std::num::ParseIntError| {
                        StoreError::store("LogStore", "read", e.to_string())
                    })?;
                let start = self.entries.len().saturating_sub(n);
                let json = serde_json::to_value(&self.entries[start..])
                    .map_err(|e| StoreError::store("LogStore", "read", e.to_string()))?;
                Ok(Some(Record::parsed(
                    structfs_serde_store::json_to_value(json),
                )))
            }
            _ => Ok(None),
        }
    }
}

impl Writer for LogStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let key = if to.is_empty() {
            "append"
        } else {
            to.components[0].as_str()
        };

        match key {
            "append" => {
                let value = data.as_value().ok_or_else(|| {
                    StoreError::store("LogStore", "write", "expected Parsed record")
                })?;
                let json = structfs_serde_store::value_to_json(value.clone());
                let entry: LogEntry = serde_json::from_value(json).map_err(|e| {
                    StoreError::store("LogStore", "write", format!("invalid LogEntry: {e}"))
                })?;
                self.entries.push(entry);
                Ok(to.clone())
            }
            _ => Err(StoreError::store(
                "LogStore",
                "write",
                format!("unknown write path: {key}"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::{path, Path, Reader, Record, Writer};

    #[test]
    fn append_and_read_all() {
        let mut log = LogStore::new();
        log.write(
            &path!("append"),
            Record::parsed(structfs_serde_store::json_to_value(
                serde_json::json!({"type": "user", "content": "hello"}),
            )),
        )
        .unwrap();
        let record = log.read(&path!("entries")).unwrap().unwrap();
        let json = structfs_serde_store::value_to_json(record.as_value().unwrap().clone());
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "user");
        assert_eq!(arr[0]["content"], "hello");
    }

    #[test]
    fn read_count() {
        let mut log = LogStore::new();
        for msg in ["a", "b", "c"] {
            log.write(
                &path!("append"),
                Record::parsed(structfs_serde_store::json_to_value(
                    serde_json::json!({"type": "user", "content": msg}),
                )),
            )
            .unwrap();
        }
        let record = log.read(&path!("count")).unwrap().unwrap();
        assert_eq!(
            record.as_value().unwrap(),
            &structfs_core_store::Value::Integer(3)
        );
    }

    #[test]
    fn read_last_n() {
        let mut log = LogStore::new();
        for i in 0..5 {
            log.write(
                &path!("append"),
                Record::parsed(structfs_serde_store::json_to_value(
                    serde_json::json!({"type": "user", "content": format!("msg{i}")}),
                )),
            )
            .unwrap();
        }
        let record = log
            .read(&Path::parse("last/2").unwrap())
            .unwrap()
            .unwrap();
        let json = structfs_serde_store::value_to_json(record.as_value().unwrap().clone());
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["content"], "msg3");
        assert_eq!(arr[1]["content"], "msg4");
    }

    #[test]
    fn empty_read_returns_empty_array() {
        let mut log = LogStore::new();
        let record = log.read(&path!("entries")).unwrap().unwrap();
        let json = structfs_serde_store::value_to_json(record.as_value().unwrap().clone());
        assert_eq!(json, serde_json::json!([]));
    }

    #[test]
    fn assistant_entry_with_source() {
        let mut log = LogStore::new();
        log.write(
            &path!("append"),
            Record::parsed(structfs_serde_store::json_to_value(serde_json::json!({
                "type": "assistant",
                "content": [{"type": "text", "text": "hi"}],
                "source": {"account": "anthropic", "model": "claude"}
            }))),
        )
        .unwrap();
        let record = log.read(&path!("entries")).unwrap().unwrap();
        let json = structfs_serde_store::value_to_json(record.as_value().unwrap().clone());
        let entry = &json.as_array().unwrap()[0];
        assert_eq!(entry["source"]["account"], "anthropic");
    }

    #[test]
    fn tool_call_and_result_entries() {
        let mut log = LogStore::new();
        log.write(
            &path!("append"),
            Record::parsed(structfs_serde_store::json_to_value(serde_json::json!({
                "type": "tool_call", "id": "tc1", "name": "read_file",
                "input": {"path": "src/main.rs"}
            }))),
        )
        .unwrap();
        log.write(
            &path!("append"),
            Record::parsed(structfs_serde_store::json_to_value(serde_json::json!({
                "type": "tool_result", "id": "tc1",
                "output": "file contents", "is_error": false
            }))),
        )
        .unwrap();
        let record = log.read(&path!("count")).unwrap().unwrap();
        assert_eq!(
            record.as_value().unwrap(),
            &structfs_core_store::Value::Integer(2)
        );
    }
}

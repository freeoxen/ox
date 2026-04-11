//! Conversation history as a StructFS store for the ox agent framework.
//!
//! [`HistoryView`] projects conversation history from a [`SharedLog`] into
//! wire-format messages via the StructFS [`Reader`]/[`Writer`] interface.
//! The log is the source of truth; history is a derived view.
//!
//! Read paths:
//! - `"messages"` → wire-format JSON array
//! - `"count"` → message count
//! - `"turn/{streaming,thinking,tool,tokens}"` → ephemeral turn state
//!
//! Write paths:
//! - `"append"` → convert wire-format message to LogEntry, append to SharedLog
//! - `"turn/{streaming,thinking,tool,tokens}"` → update ephemeral turn state
//! - `"commit"` → finalize streaming text into a committed assistant message

mod turn;
pub use turn::TurnState;

use ox_kernel::log::{LogEntry, SharedLog};
use ox_kernel::{ContentBlock, serialize_assistant_message};
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};
use structfs_serde_store::{json_to_value, value_to_json};

// ---------------------------------------------------------------------------
// HistoryView — projection over the structured log
// ---------------------------------------------------------------------------

/// History as a projection over the structured log.
///
/// Reads messages by projecting log entries into wire-format messages.
/// Writes to "append" convert wire-format messages into log entries.
/// Turn state provides ephemeral UI feedback (streaming, thinking, tool status).
pub struct HistoryView {
    shared: SharedLog,
    pub turn: TurnState,
}

impl HistoryView {
    pub fn new(shared: SharedLog) -> Self {
        Self {
            shared,
            turn: TurnState::new(),
        }
    }

    /// Project log entries into wire-format messages.
    fn project_messages(&self) -> Vec<serde_json::Value> {
        let entries = self.shared.entries();
        let mut messages = Vec::new();
        let mut i = 0;
        while i < entries.len() {
            match &entries[i] {
                LogEntry::User { content, .. } => {
                    messages.push(serde_json::json!({"role": "user", "content": content}));
                    i += 1;
                }
                LogEntry::Assistant { content, .. } => {
                    messages.push(serialize_assistant_message(content));
                    i += 1;
                }
                LogEntry::ToolResult { .. } => {
                    // Group consecutive tool results into one user message
                    let mut result_blocks = Vec::new();
                    while i < entries.len() {
                        if let LogEntry::ToolResult { id, output, .. } = &entries[i] {
                            let content_str = match output {
                                serde_json::Value::String(s) => s.clone(),
                                other => serde_json::to_string(other).unwrap_or_default(),
                            };
                            result_blocks.push(serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": id,
                                "content": content_str,
                            }));
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    messages.push(serde_json::json!({"role": "user", "content": result_blocks}));
                }
                LogEntry::ToolCall { .. } | LogEntry::Meta { .. } => {
                    // Skip: tool calls are embedded in assistant content,
                    // meta entries are not conversation messages
                    i += 1;
                }
            }
        }
        messages
    }
}

impl Reader for HistoryView {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = if from.is_empty() {
            "messages"
        } else {
            from.components[0].as_str()
        };
        match key {
            "messages" | "" => {
                let mut messages = self.project_messages();
                // Append in-progress turn as a partial assistant message
                if self.turn.is_active() && !self.turn.streaming.is_empty() {
                    messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": [{"type": "text", "text": self.turn.streaming}]
                    }));
                }
                let json = serde_json::Value::Array(messages);
                let value = json_to_value(json);
                Ok(Some(Record::parsed(value)))
            }
            "count" => {
                let count = self.project_messages().len();
                Ok(Some(Record::parsed(Value::Integer(count as i64))))
            }
            "turn" => {
                if from.components.len() >= 2 {
                    let sub = from.components[1].as_str();
                    Ok(self.turn.read(sub).map(Record::parsed))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }
}

impl Writer for HistoryView {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let key = if to.is_empty() {
            "append"
        } else {
            to.components[0].as_str()
        };
        match key {
            "append" => {
                let value = data.as_value().ok_or_else(|| {
                    StoreError::store("HistoryView", "write", "expected Parsed record")
                })?;
                let json = value_to_json(value.clone());
                // Parse wire-format message and convert to LogEntry
                let role = json.get("role").and_then(|v| v.as_str()).unwrap_or("");
                match role {
                    "user" => {
                        let content_val = json.get("content").cloned().unwrap_or_default();
                        // Check if it's a tool_result message
                        if let Some(arr) = content_val.as_array() {
                            if arr
                                .first()
                                .and_then(|v| v.get("type"))
                                .and_then(|v| v.as_str())
                                == Some("tool_result")
                            {
                                // Tool results
                                for item in arr {
                                    let id = item
                                        .get("tool_use_id")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let output = item
                                        .get("content")
                                        .cloned()
                                        .unwrap_or(serde_json::Value::Null);
                                    self.shared.append(LogEntry::ToolResult {
                                        id,
                                        output,
                                        is_error: false,
                                        scope: None,
                                    });
                                }
                                return Ok(to.clone());
                            }
                        }
                        // Regular user message
                        let content = match content_val {
                            serde_json::Value::String(s) => s,
                            other => serde_json::to_string(&other).unwrap_or_default(),
                        };
                        self.shared.append(LogEntry::User {
                            content,
                            scope: None,
                        });
                    }
                    "assistant" => {
                        let content_json = json
                            .get("content")
                            .cloned()
                            .unwrap_or(serde_json::json!([]));
                        let content: Vec<ContentBlock> =
                            serde_json::from_value(content_json).unwrap_or_default();
                        self.shared.append(LogEntry::Assistant {
                            content,
                            source: None,
                            scope: None,
                        });
                    }
                    _ => {
                        return Err(StoreError::store(
                            "HistoryView",
                            "write",
                            format!("unknown role: {role}"),
                        ));
                    }
                }
                Ok(to.clone())
            }
            "turn" => {
                if to.components.len() >= 2 {
                    let sub = to.components[1].as_str();
                    let value = data.as_value().ok_or_else(|| {
                        StoreError::store(
                            "HistoryView",
                            "write",
                            "expected parsed record for turn write",
                        )
                    })?;
                    if self.turn.write(sub, value) {
                        Ok(to.clone())
                    } else {
                        Err(StoreError::store("HistoryView", "write", "invalid turn write"))
                    }
                } else {
                    Err(StoreError::store(
                        "HistoryView",
                        "write",
                        "turn write requires sub-path (e.g. turn/streaming)",
                    ))
                }
            }
            "commit" => {
                // Clear turn state. The kernel already committed the assistant
                // message to log/append via record_turn — we just need to reset
                // the ephemeral streaming/thinking state for the next turn.
                self.turn.clear();
                Ok(to.clone())
            }
            _ => Err(StoreError::store(
                "HistoryView",
                "write",
                format!("unknown path: {key}"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;

    fn unwrap_value(record: Record) -> Value {
        match record {
            Record::Parsed(v) => v,
            _ => panic!("expected parsed record"),
        }
    }

    #[test]
    fn history_view_empty() {
        let shared = SharedLog::new();
        let mut hv = HistoryView::new(shared);
        let messages = hv.read(&path!("messages")).unwrap().unwrap();
        let val = unwrap_value(messages);
        match &val {
            Value::Array(arr) => assert!(arr.is_empty()),
            _ => panic!("expected array"),
        }
    }

    #[test]
    fn history_view_projects_user_and_assistant() {
        let shared = SharedLog::new();
        shared.append(LogEntry::User {
            content: "hello".into(),
            scope: None,
        });
        shared.append(LogEntry::Assistant {
            content: vec![ox_kernel::ContentBlock::Text {
                text: "hi there".into(),
            }],
            source: None,
            scope: None,
        });
        let mut hv = HistoryView::new(shared);
        let messages = hv.read(&path!("messages")).unwrap().unwrap();
        let json = value_to_json(unwrap_value(messages));
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["role"], "user");
        assert_eq!(arr[0]["content"], "hello");
        assert_eq!(arr[1]["role"], "assistant");
    }

    #[test]
    fn history_view_groups_tool_results() {
        let shared = SharedLog::new();
        shared.append(LogEntry::ToolResult {
            id: "tc1".into(),
            output: serde_json::Value::String("result1".into()),
            is_error: false,
            scope: None,
        });
        shared.append(LogEntry::ToolResult {
            id: "tc2".into(),
            output: serde_json::Value::String("result2".into()),
            is_error: false,
            scope: None,
        });
        let mut hv = HistoryView::new(shared);
        let messages = hv.read(&path!("messages")).unwrap().unwrap();
        let json = value_to_json(unwrap_value(messages));
        let arr = json.as_array().unwrap();
        // Two tool results should be grouped into one user message
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["role"], "user");
        let content = arr[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["tool_use_id"], "tc1");
        assert_eq!(content[1]["tool_use_id"], "tc2");
    }

    #[test]
    fn history_view_skips_tool_call_and_meta() {
        let shared = SharedLog::new();
        shared.append(LogEntry::User {
            content: "hello".into(),
            scope: None,
        });
        shared.append(LogEntry::ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            input: serde_json::json!({}),
            scope: None,
        });
        shared.append(LogEntry::Meta {
            data: serde_json::json!({"info": "test"}),
        });
        let mut hv = HistoryView::new(shared);
        let messages = hv.read(&path!("messages")).unwrap().unwrap();
        let json = value_to_json(unwrap_value(messages));
        let arr = json.as_array().unwrap();
        // Only the user message should appear
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["role"], "user");
    }

    #[test]
    fn history_view_count() {
        let shared = SharedLog::new();
        shared.append(LogEntry::User {
            content: "hello".into(),
            scope: None,
        });
        shared.append(LogEntry::Assistant {
            content: vec![ox_kernel::ContentBlock::Text { text: "hi".into() }],
            source: None,
            scope: None,
        });
        let mut hv = HistoryView::new(shared);
        let count = hv.read(&path!("count")).unwrap().unwrap();
        assert_eq!(unwrap_value(count), Value::Integer(2));
    }

    #[test]
    fn history_view_write_user_message() {
        let shared = SharedLog::new();
        let mut hv = HistoryView::new(shared.clone());
        let json = serde_json::json!({"role": "user", "content": "hello"});
        hv.write(&path!("append"), Record::parsed(json_to_value(json)))
            .unwrap();
        let entries = shared.entries();
        assert_eq!(entries.len(), 1);
        assert!(matches!(&entries[0], LogEntry::User { content, .. } if content == "hello"));
    }

    #[test]
    fn history_view_write_assistant_message() {
        let shared = SharedLog::new();
        let mut hv = HistoryView::new(shared.clone());
        let json = serde_json::json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "hi there"}]
        });
        hv.write(&path!("append"), Record::parsed(json_to_value(json)))
            .unwrap();
        let entries = shared.entries();
        assert_eq!(entries.len(), 1);
        assert!(matches!(&entries[0], LogEntry::Assistant { .. }));
    }

    #[test]
    fn history_view_write_tool_result_message() {
        let shared = SharedLog::new();
        let mut hv = HistoryView::new(shared.clone());
        let json = serde_json::json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "tc1", "content": "output"}
            ]
        });
        hv.write(&path!("append"), Record::parsed(json_to_value(json)))
            .unwrap();
        let entries = shared.entries();
        assert_eq!(entries.len(), 1);
        assert!(matches!(&entries[0], LogEntry::ToolResult { id, .. } if id == "tc1"));
    }

    #[test]
    fn history_view_write_unknown_role_error() {
        let shared = SharedLog::new();
        let mut hv = HistoryView::new(shared);
        let json = serde_json::json!({"role": "system", "content": "nope"});
        let result = hv.write(&path!("append"), Record::parsed(json_to_value(json)));
        assert!(result.is_err());
    }

    #[test]
    fn history_view_write_unknown_path_error() {
        let shared = SharedLog::new();
        let mut hv = HistoryView::new(shared);
        let result = hv.write(&path!("nonexistent"), Record::parsed(Value::Null));
        assert!(result.is_err());
    }

    #[test]
    fn history_view_read_unknown_path_returns_none() {
        let shared = SharedLog::new();
        let mut hv = HistoryView::new(shared);
        let result = hv.read(&path!("nonexistent")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn history_view_turn_streaming() {
        let shared = SharedLog::new();
        let mut hv = HistoryView::new(shared);
        hv.write(
            &path!("turn/streaming"),
            Record::parsed(Value::String("hello ".into())),
        )
        .unwrap();
        hv.write(
            &path!("turn/streaming"),
            Record::parsed(Value::String("world".into())),
        )
        .unwrap();
        let val = hv.read(&path!("turn/streaming")).unwrap().unwrap();
        assert_eq!(
            unwrap_value(val),
            Value::String("hello world".into())
        );
    }

    #[test]
    fn history_view_commit_clears_turn_state() {
        let shared = SharedLog::new();
        let mut hv = HistoryView::new(shared.clone());
        hv.write(
            &path!("turn/streaming"),
            Record::parsed(Value::String("streamed text".into())),
        )
        .unwrap();
        hv.write(
            &path!("turn/thinking"),
            Record::parsed(Value::Bool(true)),
        )
        .unwrap();
        hv.write(&path!("commit"), Record::parsed(Value::Null))
            .unwrap();
        // Turn state should be cleared
        assert!(!hv.turn.is_active());
        assert!(hv.turn.streaming.is_empty());
        // Commit does NOT write to the log — the kernel already did via log/append
        assert!(shared.is_empty());
    }

    #[test]
    fn history_view_commit_empty_is_noop() {
        let shared = SharedLog::new();
        let mut hv = HistoryView::new(shared.clone());
        hv.write(&path!("commit"), Record::parsed(Value::Null))
            .unwrap();
        assert!(shared.is_empty());
    }

    #[test]
    fn history_view_streaming_appears_in_messages() {
        let shared = SharedLog::new();
        shared.append(LogEntry::User {
            content: "hello".into(),
            scope: None,
        });
        let mut hv = HistoryView::new(shared);
        hv.write(
            &path!("turn/thinking"),
            Record::parsed(Value::Bool(true)),
        )
        .unwrap();
        hv.write(
            &path!("turn/streaming"),
            Record::parsed(Value::String("partial response".into())),
        )
        .unwrap();
        let messages = hv.read(&path!("messages")).unwrap().unwrap();
        let json = value_to_json(unwrap_value(messages));
        let arr = json.as_array().unwrap();
        // User message + partial assistant message
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[1]["role"], "assistant");
        assert_eq!(arr[1]["content"][0]["text"], "partial response");
    }

    #[test]
    fn history_view_turn_no_subpath_returns_none() {
        let shared = SharedLog::new();
        let mut hv = HistoryView::new(shared);
        assert!(hv.read(&path!("turn")).unwrap().is_none());
    }

    #[test]
    fn history_view_turn_write_no_subpath_errors() {
        let shared = SharedLog::new();
        let mut hv = HistoryView::new(shared);
        let result = hv.write(&path!("turn"), Record::parsed(Value::Null));
        assert!(result.is_err());
    }
}

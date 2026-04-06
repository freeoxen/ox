//! Conversation history as a StructFS store for the ox agent framework.
//!
//! [`HistoryProvider`] stores a `Vec<Message>` and exposes it via the
//! StructFS [`Reader`]/[`Writer`] interface. The kernel appends messages
//! by writing to `path!("history/append")` and reads them back as
//! wire-format JSON via `path!("history/messages")`.
//!
//! Also provides [`parse_wire_message`] for converting Anthropic Messages API
//! JSON into typed [`Message`] values.

use ox_kernel::{ContentBlock, Message, ToolResult};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};
use structfs_serde_store::{json_to_value, value_to_json};

// ---------------------------------------------------------------------------
// HistoryProvider — Reader/Writer impl over Vec<Message>
// ---------------------------------------------------------------------------

/// Stores conversation history and exposes it via the Reader/Writer interface.
///
/// Read paths:
/// - `""` or `"messages"` → wire-format JSON array (as StructFS Value)
/// - `"count"` → message count (as Value::Integer)
///
/// Write paths:
/// - `""` or `"append"` → parse StructFS Value → wire JSON → Message, append
/// - `"clear"` → clear all
pub struct HistoryProvider {
    messages: Vec<Message>,
}

impl HistoryProvider {
    /// Create an empty history.
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    /// Direct access to the stored messages.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Content hash of a single message: SHA-256 of its JSON, truncated to 16 hex chars.
    fn message_hash(msg: &serde_json::Value) -> String {
        let bytes = serde_json::to_vec(msg).expect("message always serializes");
        let digest = Sha256::digest(&bytes);
        digest[..8].iter().map(|b| format!("{b:02x}")).collect()
    }

    fn snapshot_state(&self) -> Value {
        let mut map = BTreeMap::new();
        map.insert(
            "count".to_string(),
            Value::Integer(self.messages.len() as i64),
        );

        if self.messages.is_empty() {
            map.insert("last_hash".to_string(), Value::Null);
        } else {
            let wire = self.to_wire_messages();
            let last = wire.last().unwrap();
            map.insert(
                "last_hash".to_string(),
                Value::String(Self::message_hash(last)),
            );
        }
        Value::Map(map)
    }

    /// Outer snapshot hash. For empty history, returns all zeros per the
    /// snapshot protocol RFC — `snapshot_hash({"count":0,"last_hash":null})`
    /// would produce a non-zero hash, but the convention is that an empty
    /// history has a zero sentinel so coordinators can detect it cheaply.
    fn snapshot_outer_hash(&self, state: &Value) -> String {
        if self.messages.is_empty() {
            "0000000000000000".to_string()
        } else {
            ox_kernel::snapshot::snapshot_hash(state)
        }
    }

    fn to_wire_messages(&self) -> Vec<serde_json::Value> {
        self.messages
            .iter()
            .map(|msg| match msg {
                Message::User { content } => serde_json::json!({
                    "role": "user",
                    "content": content,
                }),
                Message::Assistant { content } => {
                    let blocks: Vec<serde_json::Value> = content
                        .iter()
                        .map(|b| match b {
                            ContentBlock::Text { text } => serde_json::json!({
                                "type": "text",
                                "text": text,
                            }),
                            ContentBlock::ToolUse(tc) => serde_json::json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.name,
                                "input": tc.input,
                            }),
                        })
                        .collect();
                    serde_json::json!({
                        "role": "assistant",
                        "content": blocks,
                    })
                }
                Message::ToolResult { results } => {
                    let content: Vec<serde_json::Value> = results
                        .iter()
                        .map(|r| {
                            serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": r.tool_use_id,
                                "content": r.content,
                            })
                        })
                        .collect();
                    serde_json::json!({
                        "role": "user",
                        "content": content,
                    })
                }
            })
            .collect()
    }
}

impl Default for HistoryProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse an Anthropic Messages API wire-format JSON object into a typed [`Message`].
///
/// Handles user messages, assistant messages (with text and tool_use blocks),
/// and tool result messages.
pub fn parse_wire_message(wire: &serde_json::Value) -> Result<Message, String> {
    let role = wire
        .get("role")
        .and_then(|r| r.as_str())
        .ok_or("missing role")?;

    match role {
        "user" => {
            // Could be a plain user message or tool results
            if let Some(content_arr) = wire.get("content").and_then(|c| c.as_array())
                && content_arr
                    .first()
                    .and_then(|c| c.get("type"))
                    .and_then(|t| t.as_str())
                    == Some("tool_result")
            {
                let results = content_arr
                    .iter()
                    .filter_map(|item| {
                        let tool_use_id = item.get("tool_use_id")?.as_str()?.to_string();
                        let content = item.get("content")?.as_str()?.to_string();
                        Some(ToolResult {
                            tool_use_id,
                            content,
                        })
                    })
                    .collect();
                return Ok(Message::ToolResult { results });
            }
            // Plain user message
            let content = wire
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            Ok(Message::User { content })
        }
        "assistant" => {
            let content = parse_assistant_content(wire);
            Ok(Message::Assistant { content })
        }
        _ => Err(format!("unknown role: {role}")),
    }
}

fn parse_assistant_content(wire: &serde_json::Value) -> Vec<ContentBlock> {
    let content_arr = match wire.get("content").and_then(|c| c.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    content_arr
        .iter()
        .filter_map(|item| {
            let typ = item.get("type")?.as_str()?;
            match typ {
                "text" => {
                    let text = item.get("text")?.as_str()?.to_string();
                    Some(ContentBlock::Text { text })
                }
                "tool_use" => {
                    let id = item.get("id")?.as_str()?.to_string();
                    let name = item.get("name")?.as_str()?.to_string();
                    let input = item.get("input")?.clone();
                    Some(ContentBlock::ToolUse(ox_kernel::ToolCall {
                        id,
                        name,
                        input,
                    }))
                }
                _ => None,
            }
        })
        .collect()
}

impl Reader for HistoryProvider {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = if from.is_empty() {
            ""
        } else {
            from.components[0].as_str()
        };
        match key {
            "" | "messages" => {
                let wire = self.to_wire_messages();
                let json = serde_json::to_value(wire)
                    .map_err(|e| StoreError::store("history", "read", e.to_string()))?;
                Ok(Some(Record::parsed(json_to_value(json))))
            }
            "count" => Ok(Some(Record::parsed(Value::Integer(
                self.messages.len() as i64
            )))),
            "snapshot" => {
                let state = self.snapshot_state();
                if from.components.len() >= 2 {
                    match from.components[1].as_str() {
                        "hash" => {
                            let hash = self.snapshot_outer_hash(&state);
                            Ok(Some(Record::parsed(Value::String(hash))))
                        }
                        "state" => Ok(Some(Record::parsed(state))),
                        _ => Ok(None),
                    }
                } else {
                    let hash = self.snapshot_outer_hash(&state);
                    let mut map = BTreeMap::new();
                    map.insert("hash".to_string(), Value::String(hash));
                    map.insert("state".to_string(), state);
                    Ok(Some(Record::parsed(Value::Map(map))))
                }
            }
            _ => Ok(None),
        }
    }
}

impl Writer for HistoryProvider {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let key = if to.is_empty() {
            ""
        } else {
            to.components[0].as_str()
        };
        match key {
            "" | "append" => {
                let value = match data {
                    Record::Parsed(v) => v,
                    _ => {
                        return Err(StoreError::store(
                            "history",
                            "write",
                            "expected parsed record",
                        ));
                    }
                };
                let json = value_to_json(value);
                let msg = parse_wire_message(&json)
                    .map_err(|e| StoreError::store("history", "write", e))?;
                self.messages.push(msg);
                Ok(to.clone())
            }
            "clear" => {
                self.messages.clear();
                Ok(to.clone())
            }
            "snapshot" => Err(StoreError::store(
                "history",
                "write",
                "snapshot write not supported — restore history via ledger replay through append",
            )),
            _ => Err(StoreError::store(
                "history",
                "write",
                format!("unknown write path: {to}"),
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

    fn append_user_msg(hp: &mut HistoryProvider, text: &str) {
        let json = serde_json::json!({"role": "user", "content": text});
        let value = json_to_value(json);
        hp.write(&path!("append"), Record::parsed(value)).unwrap();
    }

    fn append_assistant_msg(hp: &mut HistoryProvider, text: &str) {
        let json = serde_json::json!({
            "role": "assistant",
            "content": [{"type": "text", "text": text}]
        });
        let value = json_to_value(json);
        hp.write(&path!("append"), Record::parsed(value)).unwrap();
    }

    #[test]
    fn snapshot_empty_history() {
        let mut hp = HistoryProvider::new();
        let val = unwrap_value(hp.read(&path!("snapshot")).unwrap().unwrap());
        match &val {
            Value::Map(m) => {
                assert_eq!(
                    m.get("hash").unwrap(),
                    &Value::String("0000000000000000".to_string())
                );
                let state = m.get("state").unwrap();
                match state {
                    Value::Map(sm) => {
                        assert_eq!(sm.get("count").unwrap(), &Value::Integer(0));
                        assert_eq!(sm.get("last_hash").unwrap(), &Value::Null);
                    }
                    _ => panic!("expected map state"),
                }
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn snapshot_with_messages() {
        let mut hp = HistoryProvider::new();
        append_user_msg(&mut hp, "hello");
        append_assistant_msg(&mut hp, "hi there");

        let val = unwrap_value(hp.read(&path!("snapshot")).unwrap().unwrap());
        match &val {
            Value::Map(m) => {
                let hash = match m.get("hash").unwrap() {
                    Value::String(s) => s.clone(),
                    _ => panic!("expected string hash"),
                };
                assert_eq!(hash.len(), 16);
                assert_ne!(hash, "0000000000000000");

                let state = m.get("state").unwrap();
                match state {
                    Value::Map(sm) => {
                        assert_eq!(sm.get("count").unwrap(), &Value::Integer(2));
                        match sm.get("last_hash").unwrap() {
                            Value::String(lh) => assert_eq!(lh.len(), 16),
                            _ => panic!("expected string last_hash"),
                        }
                    }
                    _ => panic!("expected map state"),
                }
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn snapshot_hash_subpath() {
        let mut hp = HistoryProvider::new();
        append_user_msg(&mut hp, "test");
        let val = unwrap_value(hp.read(&path!("snapshot/hash")).unwrap().unwrap());
        match val {
            Value::String(h) => assert_eq!(h.len(), 16),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn snapshot_state_subpath() {
        let mut hp = HistoryProvider::new();
        append_user_msg(&mut hp, "test");
        let val = unwrap_value(hp.read(&path!("snapshot/state")).unwrap().unwrap());
        match val {
            Value::Map(m) => {
                assert_eq!(m.get("count").unwrap(), &Value::Integer(1));
                assert!(m.contains_key("last_hash"));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn snapshot_write_returns_error() {
        let mut hp = HistoryProvider::new();
        let result = hp.write(&path!("snapshot"), Record::parsed(Value::Null));
        assert!(result.is_err());
    }

    #[test]
    fn snapshot_last_hash_changes_on_append() {
        let mut hp = HistoryProvider::new();
        append_user_msg(&mut hp, "first");

        let h1 = match unwrap_value(hp.read(&path!("snapshot/state")).unwrap().unwrap()) {
            Value::Map(m) => match m.get("last_hash").unwrap() {
                Value::String(s) => s.clone(),
                _ => panic!("expected string"),
            },
            _ => panic!("expected map"),
        };

        append_assistant_msg(&mut hp, "second");

        let h2 = match unwrap_value(hp.read(&path!("snapshot/state")).unwrap().unwrap()) {
            Value::Map(m) => match m.get("last_hash").unwrap() {
                Value::String(s) => s.clone(),
                _ => panic!("expected string"),
            },
            _ => panic!("expected map"),
        };

        assert_ne!(h1, h2);
    }
}

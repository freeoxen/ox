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
            _ => Err(StoreError::store(
                "history",
                "write",
                format!("unknown write path: {to}"),
            )),
        }
    }
}

use ox_kernel::{ContentBlock, Message, Provider, ToolResult, Value};

// ---------------------------------------------------------------------------
// HistoryProvider — Provider impl over Vec<Message>
// ---------------------------------------------------------------------------

/// Stores conversation history and exposes it via the Provider interface.
///
/// Read paths:
/// - `""` or `"messages"` → wire-format JSON array
/// - `"count"` → message count
///
/// Write paths:
/// - `""` or `"append"` → parse wire JSON into Message, append
/// - `"clear"` → clear all
pub struct HistoryProvider {
    messages: Vec<Message>,
}

impl HistoryProvider {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

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

/// Parse a wire-format JSON message into a typed `Message`.
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

impl Provider for HistoryProvider {
    fn read(&self, path: &str) -> Option<Value> {
        match path {
            "" | "messages" => serde_json::to_value(self.to_wire_messages()).ok(),
            "count" => Some(serde_json::json!(self.messages.len())),
            _ => None,
        }
    }

    fn write(&mut self, path: &str, value: Value) -> Result<(), String> {
        match path {
            "" | "append" => {
                let msg = parse_wire_message(&value)?;
                self.messages.push(msg);
                Ok(())
            }
            "clear" => {
                self.messages.clear();
                Ok(())
            }
            _ => Err(format!("unknown write path: {path}")),
        }
    }
}

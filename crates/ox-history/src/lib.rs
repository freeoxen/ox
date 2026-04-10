//! Conversation history as a StructFS store for the ox agent framework.
//!
//! [`HistoryProvider`] stores a `Vec<Message>` and exposes it via the
//! StructFS [`Reader`]/[`Writer`] interface. The kernel appends messages
//! by writing to `path!("history/append")` and reads them back as
//! wire-format JSON via `path!("history/messages")`.
//!
//! Also provides [`parse_wire_message`] for converting Anthropic Messages API
//! JSON into typed [`Message`] values.

mod turn;
pub use turn::TurnState;

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
    pub turn: TurnState,
}

impl HistoryProvider {
    /// Create an empty history.
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            turn: TurnState::new(),
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
                        let content = item.get("content").cloned().unwrap_or(serde_json::Value::Null);
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
                let mut wire = self.to_wire_messages();
                // Append in-progress turn as a partial assistant message
                if self.turn.is_active() && !self.turn.streaming.is_empty() {
                    wire.push(serde_json::json!({
                        "role": "assistant",
                        "content": [{"type": "text", "text": self.turn.streaming}]
                    }));
                }
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
            "turn" => {
                // Delegate to turn state for sub-paths like "turn/streaming"
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
                // Clear streaming text when an assistant message is appended —
                // the streaming content is now committed as part of this message,
                // so it shouldn't also appear as a partial in reads.
                if matches!(msg, Message::Assistant { .. }) {
                    self.turn.streaming.clear();
                }
                self.messages.push(msg);
                tracing::debug!(message_count = self.messages.len(), "message appended");
                Ok(to.clone())
            }
            "clear" => {
                self.messages.clear();
                Ok(to.clone())
            }
            "turn" => {
                // Delegate to turn state for sub-paths like "turn/streaming"
                if to.components.len() >= 2 {
                    let sub = to.components[1].as_str();
                    let value = match &data {
                        Record::Parsed(v) => v,
                        _ => {
                            return Err(StoreError::store(
                                "history",
                                "write",
                                "expected parsed record for turn write",
                            ));
                        }
                    };
                    if self.turn.write(sub, value) {
                        Ok(to.clone())
                    } else {
                        Err(StoreError::store("history", "write", "invalid turn write"))
                    }
                } else {
                    Err(StoreError::store(
                        "history",
                        "write",
                        "turn write requires sub-path (e.g. turn/streaming)",
                    ))
                }
            }
            "commit" => {
                // Finalize in-progress turn: streaming text becomes a committed message
                if !self.turn.streaming.is_empty() {
                    let content = vec![ContentBlock::Text {
                        text: self.turn.streaming.clone(),
                    }];
                    self.messages.push(Message::Assistant { content });
                }
                self.turn.clear();
                tracing::debug!(message_count = self.messages.len(), "history committed");
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

    #[test]
    fn turn_streaming_visible_in_messages() {
        let mut hp = HistoryProvider::new();
        append_user_msg(&mut hp, "hello");

        // Start streaming
        hp.write(
            &path!("turn/streaming"),
            Record::parsed(Value::String("Hi there".to_string())),
        )
        .unwrap();
        hp.write(&path!("turn/thinking"), Record::parsed(Value::Bool(true)))
            .unwrap();

        // Messages should include the in-progress turn
        let messages = hp.read(&path!("messages")).unwrap().unwrap();
        let val = unwrap_value(messages);
        let arr = match &val {
            Value::Array(a) => a,
            _ => panic!("expected array"),
        };
        assert_eq!(arr.len(), 2); // user + partial assistant
    }

    #[test]
    fn commit_finalizes_turn() {
        let mut hp = HistoryProvider::new();
        append_user_msg(&mut hp, "hello");

        // Stream content
        hp.write(
            &path!("turn/streaming"),
            Record::parsed(Value::String("Response text".to_string())),
        )
        .unwrap();

        // Commit
        hp.write(&path!("commit"), Record::parsed(Value::Null))
            .unwrap();

        // Turn should be clear
        assert!(!hp.turn.is_active());

        // Message should be committed
        let count = hp.read(&path!("count")).unwrap().unwrap();
        assert_eq!(unwrap_value(count), Value::Integer(2));
    }

    #[test]
    fn turn_read_paths() {
        let mut hp = HistoryProvider::new();
        hp.write(&path!("turn/thinking"), Record::parsed(Value::Bool(true)))
            .unwrap();

        let val = hp.read(&path!("turn/thinking")).unwrap().unwrap();
        assert_eq!(unwrap_value(val), Value::Bool(true));
    }

    #[test]
    fn append_assistant_clears_streaming() {
        let mut hp = HistoryProvider::new();
        append_user_msg(&mut hp, "hello");

        // Simulate streaming
        hp.write(
            &path!("turn/streaming"),
            Record::parsed(Value::String("Streaming text".to_string())),
        )
        .unwrap();
        hp.write(&path!("turn/thinking"), Record::parsed(Value::Bool(true)))
            .unwrap();

        // Append the committed assistant message (as the kernel does mid-turn)
        append_assistant_msg(&mut hp, "Streaming text");

        // Messages should NOT have the streaming partial duplicated
        let messages = hp.read(&path!("messages")).unwrap().unwrap();
        let val = unwrap_value(messages);
        let arr = match &val {
            Value::Array(a) => a,
            _ => panic!("expected array"),
        };
        // Should be 2: user + committed assistant. NOT 3 (user + committed + partial).
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn commit_empty_turn_is_noop() {
        let mut hp = HistoryProvider::new();
        append_user_msg(&mut hp, "hello");

        // Commit with no streaming content
        hp.write(&path!("commit"), Record::parsed(Value::Null))
            .unwrap();

        // Should still have just the user message
        let count = hp.read(&path!("count")).unwrap().unwrap();
        assert_eq!(unwrap_value(count), Value::Integer(1));
    }

    // ---- parse_wire_message error/edge cases ----

    #[test]
    fn parse_wire_message_missing_role() {
        let wire = serde_json::json!({"content": "hello"});
        let result = parse_wire_message(&wire);
        assert_eq!(result.unwrap_err(), "missing role");
    }

    #[test]
    fn parse_wire_message_unknown_role() {
        let wire = serde_json::json!({"role": "system", "content": "you are helpful"});
        let result = parse_wire_message(&wire);
        assert!(result.unwrap_err().contains("unknown role: system"));
    }

    #[test]
    fn parse_wire_message_user_without_content() {
        let wire = serde_json::json!({"role": "user"});
        let msg = parse_wire_message(&wire).unwrap();
        match msg {
            Message::User { content } => assert_eq!(content, ""),
            _ => panic!("expected user message"),
        }
    }

    #[test]
    fn parse_wire_message_user_with_non_string_content() {
        // content is an array but not tool_result type — falls through to plain user
        let wire = serde_json::json!({"role": "user", "content": [{"type": "text", "text": "hi"}]});
        let msg = parse_wire_message(&wire).unwrap();
        match msg {
            Message::User { content } => assert_eq!(content, ""),
            _ => panic!("expected user message"),
        }
    }

    #[test]
    fn parse_wire_message_tool_result() {
        let wire = serde_json::json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "tu_1", "content": "result text"},
                {"type": "tool_result", "tool_use_id": "tu_2", "content": "result 2"}
            ]
        });
        let msg = parse_wire_message(&wire).unwrap();
        match msg {
            Message::ToolResult { results } => {
                assert_eq!(results.len(), 2);
                assert_eq!(results[0].tool_use_id, "tu_1");
                assert_eq!(results[1].content, serde_json::Value::String("result 2".into()));
            }
            _ => panic!("expected tool result"),
        }
    }

    #[test]
    fn parse_wire_message_tool_result_missing_fields() {
        // Items with missing tool_use_id should be filtered out;
        // missing content is allowed (becomes Value::Null).
        let wire = serde_json::json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "tu_1"},
                {"type": "tool_result", "content": "orphan"},
                {"type": "tool_result", "tool_use_id": "tu_3", "content": "ok"}
            ]
        });
        let msg = parse_wire_message(&wire).unwrap();
        match msg {
            Message::ToolResult { results } => {
                assert_eq!(results.len(), 2);
                assert_eq!(results[0].tool_use_id, "tu_1");
                assert_eq!(results[0].content, serde_json::Value::Null);
                assert_eq!(results[1].tool_use_id, "tu_3");
                assert_eq!(results[1].content, serde_json::Value::String("ok".into()));
            }
            _ => panic!("expected tool result"),
        }
    }

    #[test]
    fn parse_wire_message_assistant_with_tool_use() {
        let wire = serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Let me help."},
                {"type": "tool_use", "id": "tu_1", "name": "bash", "input": {"cmd": "ls"}}
            ]
        });
        let msg = parse_wire_message(&wire).unwrap();
        match msg {
            Message::Assistant { content } => {
                assert_eq!(content.len(), 2);
                match &content[0] {
                    ContentBlock::Text { text } => assert_eq!(text, "Let me help."),
                    _ => panic!("expected text block"),
                }
                match &content[1] {
                    ContentBlock::ToolUse(tc) => {
                        assert_eq!(tc.id, "tu_1");
                        assert_eq!(tc.name, "bash");
                    }
                    _ => panic!("expected tool_use block"),
                }
            }
            _ => panic!("expected assistant message"),
        }
    }

    #[test]
    fn parse_wire_message_assistant_no_content() {
        let wire = serde_json::json!({"role": "assistant"});
        let msg = parse_wire_message(&wire).unwrap();
        match msg {
            Message::Assistant { content } => assert!(content.is_empty()),
            _ => panic!("expected assistant message"),
        }
    }

    #[test]
    fn parse_assistant_content_unknown_type() {
        // Unknown block types should be filtered out
        let wire = serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "text": "hmm"},
                {"type": "text", "text": "hello"}
            ]
        });
        let msg = parse_wire_message(&wire).unwrap();
        match msg {
            Message::Assistant { content } => {
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ContentBlock::Text { text } => assert_eq!(text, "hello"),
                    _ => panic!("expected text block"),
                }
            }
            _ => panic!("expected assistant message"),
        }
    }

    #[test]
    fn parse_assistant_content_text_missing_text_field() {
        // text block without "text" key should be filtered out
        let wire = serde_json::json!({
            "role": "assistant",
            "content": [{"type": "text"}]
        });
        let msg = parse_wire_message(&wire).unwrap();
        match msg {
            Message::Assistant { content } => assert!(content.is_empty()),
            _ => panic!("expected assistant message"),
        }
    }

    #[test]
    fn parse_assistant_content_tool_use_missing_fields() {
        // tool_use block without "id" should be filtered out
        let wire = serde_json::json!({
            "role": "assistant",
            "content": [{"type": "tool_use", "name": "bash", "input": {}}]
        });
        let msg = parse_wire_message(&wire).unwrap();
        match msg {
            Message::Assistant { content } => assert!(content.is_empty()),
            _ => panic!("expected assistant message"),
        }
    }

    // ---- Writer error paths ----

    #[test]
    fn write_unknown_path_error() {
        let mut hp = HistoryProvider::new();
        let result = hp.write(&path!("nonexistent"), Record::parsed(Value::Null));
        assert!(result.is_err());
    }

    #[test]
    fn write_turn_no_subpath_error() {
        let mut hp = HistoryProvider::new();
        let result = hp.write(&path!("turn"), Record::parsed(Value::Null));
        assert!(result.is_err());
    }

    #[test]
    fn write_turn_invalid_subpath_error() {
        let mut hp = HistoryProvider::new();
        let result = hp.write(
            &path!("turn/nonexistent"),
            Record::parsed(Value::String("test".to_string())),
        );
        assert!(result.is_err());
    }

    #[test]
    fn write_append_invalid_message_error() {
        let mut hp = HistoryProvider::new();
        // A value that will fail parse_wire_message (no role)
        let json = serde_json::json!({"content": "no role"});
        let value = json_to_value(json);
        let result = hp.write(&path!("append"), Record::parsed(value));
        assert!(result.is_err());
    }

    // ---- Reader edge cases ----

    #[test]
    fn read_unknown_path_returns_none() {
        let mut hp = HistoryProvider::new();
        let result = hp.read(&path!("nonexistent")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_turn_no_subpath_returns_none() {
        let mut hp = HistoryProvider::new();
        let result = hp.read(&path!("turn")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_turn_unknown_subpath_returns_none() {
        let mut hp = HistoryProvider::new();
        let result = hp.read(&path!("turn/nonexistent")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_snapshot_unknown_subpath_returns_none() {
        let mut hp = HistoryProvider::new();
        let result = hp.read(&path!("snapshot/nonexistent")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_empty_path_returns_messages() {
        let mut hp = HistoryProvider::new();
        append_user_msg(&mut hp, "hello");
        let result = hp.read(&Path::from_components(vec![])).unwrap().unwrap();
        let val = unwrap_value(result);
        match &val {
            Value::Array(arr) => assert_eq!(arr.len(), 1),
            _ => panic!("expected array"),
        }
    }

    #[test]
    fn write_empty_path_appends() {
        let mut hp = HistoryProvider::new();
        let json = serde_json::json!({"role": "user", "content": "hello"});
        let value = json_to_value(json);
        hp.write(&Path::from_components(vec![]), Record::parsed(value))
            .unwrap();
        assert_eq!(hp.messages().len(), 1);
    }

    #[test]
    fn write_clear_path() {
        let mut hp = HistoryProvider::new();
        append_user_msg(&mut hp, "hello");
        assert_eq!(hp.messages().len(), 1);
        hp.write(&path!("clear"), Record::parsed(Value::Null))
            .unwrap();
        assert_eq!(hp.messages().len(), 0);
    }

    // ---- Tool result serialization roundtrip ----

    #[test]
    fn tool_result_message_roundtrip() {
        let mut hp = HistoryProvider::new();
        let wire = serde_json::json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "tu_1", "content": "output"}
            ]
        });
        let value = json_to_value(wire);
        hp.write(&path!("append"), Record::parsed(value)).unwrap();

        let messages = hp.read(&path!("messages")).unwrap().unwrap();
        let val = unwrap_value(messages);
        match &val {
            Value::Array(arr) => {
                assert_eq!(arr.len(), 1);
                // The tool result should be serialized back as a user message with content array
            }
            _ => panic!("expected array"),
        }
    }

    // ---- HistoryProvider::default ----

    #[test]
    fn default_creates_empty() {
        let hp = HistoryProvider::default();
        assert_eq!(hp.messages().len(), 0);
        assert!(!hp.turn.is_active());
    }

    // ---- Snapshot hash empty vs non-empty ----

    #[test]
    fn snapshot_empty_hash_is_zero_sentinel() {
        let mut hp = HistoryProvider::new();
        let val = unwrap_value(hp.read(&path!("snapshot/hash")).unwrap().unwrap());
        assert_eq!(val, Value::String("0000000000000000".to_string()));
    }

    #[test]
    fn turn_read_streaming() {
        let mut hp = HistoryProvider::new();
        let val = hp.read(&path!("turn/streaming")).unwrap().unwrap();
        assert_eq!(unwrap_value(val), Value::String(String::new()));
    }

    #[test]
    fn turn_read_tokens() {
        let mut hp = HistoryProvider::new();
        let val = hp.read(&path!("turn/tokens")).unwrap().unwrap();
        match unwrap_value(val) {
            Value::Map(m) => {
                assert_eq!(m.get("in"), Some(&Value::Integer(0)));
                assert_eq!(m.get("out"), Some(&Value::Integer(0)));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn turn_read_tool_null_when_inactive() {
        let mut hp = HistoryProvider::new();
        let val = hp.read(&path!("turn/tool")).unwrap().unwrap();
        assert_eq!(unwrap_value(val), Value::Null);
    }

    #[test]
    fn write_snapshot_error() {
        let mut hp = HistoryProvider::new();
        let result = hp.write(&path!("snapshot"), Record::parsed(Value::Null));
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("snapshot write not supported"));
    }
}

//! Parsing helpers — convert raw StructFS Values into typed display structs.

use structfs_core_store::Value;

use crate::types::ChatMessage;

// ---------------------------------------------------------------------------
// InboxThread — parsed thread metadata for inbox display
// ---------------------------------------------------------------------------

/// Parsed thread metadata for inbox display.
#[derive(Debug, Clone)]
pub struct InboxThread {
    pub id: String,
    pub title: String,
    pub thread_state: String,
    pub labels: Vec<String>,
    pub token_count: i64,
    pub last_seq: i64,
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Convert a slice of StructFS Values (Anthropic wire-format messages) into
/// `ChatMessage`s suitable for display.
pub fn parse_chat_messages(values: &[Value]) -> Vec<ChatMessage> {
    let mut out = Vec::new();
    for val in values {
        parse_one_message(val, &mut out);
    }
    out
}

fn parse_one_message(val: &Value, out: &mut Vec<ChatMessage>) {
    let map = match val {
        Value::Map(m) => m,
        _ => return,
    };

    let role = match map.get("role") {
        Some(Value::String(s)) => s.as_str(),
        _ => return,
    };

    let content = match map.get("content") {
        Some(v) => v,
        None => return,
    };

    match role {
        "user" => parse_user_content(content, out),
        "assistant" => parse_assistant_content(content, out),
        _ => {}
    }
}

fn parse_user_content(content: &Value, out: &mut Vec<ChatMessage>) {
    // Plain string content
    if let Value::String(s) = content {
        out.push(ChatMessage::User(s.clone()));
        return;
    }

    // Array content — look for tool_result blocks
    if let Value::Array(arr) = content {
        for item in arr {
            if let Value::Map(block) = item {
                if block.get("type") == Some(&Value::String("tool_result".to_string())) {
                    let content_str = match block.get("content") {
                        Some(Value::String(s)) => s.clone(),
                        _ => String::new(),
                    };
                    out.push(ChatMessage::ToolResult {
                        name: "tool".to_string(),
                        output: content_str,
                    });
                }
            }
        }
    }
}

fn parse_assistant_content(content: &Value, out: &mut Vec<ChatMessage>) {
    // Plain string content
    if let Value::String(s) = content {
        out.push(ChatMessage::AssistantChunk(s.clone()));
        return;
    }

    // Array of content blocks
    if let Value::Array(blocks) = content {
        let mut text = String::new();
        for block in blocks {
            let Value::Map(block_map) = block else {
                continue;
            };
            let block_type = match block_map.get("type") {
                Some(Value::String(s)) => s.as_str(),
                _ => continue,
            };
            match block_type {
                "text" => {
                    if let Some(Value::String(s)) = block_map.get("text") {
                        text.push_str(s);
                    }
                }
                "tool_use" => {
                    // Flush accumulated text before tool_use
                    if !text.is_empty() {
                        out.push(ChatMessage::AssistantChunk(std::mem::take(&mut text)));
                    }
                    let name = match block_map.get("name") {
                        Some(Value::String(s)) => s.clone(),
                        _ => "unknown".to_string(),
                    };
                    out.push(ChatMessage::ToolCall { name });
                }
                _ => {}
            }
        }
        if !text.is_empty() {
            out.push(ChatMessage::AssistantChunk(text));
        }
    }
}

/// Parse a broker inbox Value into a list of `InboxThread`s.
pub fn parse_inbox_threads(value: &Value) -> Vec<InboxThread> {
    let arr = match value {
        Value::Array(a) => a,
        _ => return Vec::new(),
    };

    let mut threads = Vec::with_capacity(arr.len());
    for item in arr {
        let Value::Map(map) = item else { continue };
        let id = match map.get("id") {
            Some(Value::String(s)) => s.clone(),
            _ => continue,
        };
        let title = match map.get("title") {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        let thread_state = match map.get("thread_state") {
            Some(Value::String(s)) => s.clone(),
            _ => "running".to_string(),
        };
        let labels = match map.get("labels") {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let token_count = match map.get("token_count") {
            Some(Value::Integer(n)) => *n,
            _ => 0,
        };
        let last_seq = match map.get("last_seq") {
            Some(Value::Integer(n)) => *n,
            _ => -1,
        };
        threads.push(InboxThread {
            id,
            title,
            thread_state,
            labels,
            token_count,
            last_seq,
        });
    }
    threads
}

// ---------------------------------------------------------------------------
// Standalone search filter
// ---------------------------------------------------------------------------

/// Check whether a thread matches all search chips and the live query.
#[allow(dead_code)]
pub fn search_matches(
    chips: &[String],
    live_query: &str,
    title: &str,
    labels: &[String],
    state: &str,
) -> bool {
    let hay = format!(
        "{} {} {}",
        title.to_lowercase(),
        labels
            .iter()
            .map(|l| l.to_lowercase())
            .collect::<Vec<_>>()
            .join(" "),
        state.to_lowercase()
    );
    for chip in chips {
        if !hay.contains(&chip.to_lowercase()) {
            return false;
        }
    }
    if !live_query.is_empty() && !hay.contains(&live_query.to_lowercase()) {
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use structfs_core_store::Value;

    /// Helper to build a Value::Map from key-value pairs.
    fn map(pairs: Vec<(&str, Value)>) -> Value {
        let mut m = BTreeMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v);
        }
        Value::Map(m)
    }

    fn s(val: &str) -> Value {
        Value::String(val.to_string())
    }

    #[test]
    fn parse_user_string_message() {
        let msgs = vec![map(vec![("role", s("user")), ("content", s("hello"))])];
        let parsed = parse_chat_messages(&msgs);
        assert_eq!(parsed.len(), 1);
        match &parsed[0] {
            ChatMessage::User(text) => assert_eq!(text, "hello"),
            other => panic!("expected User, got {other:?}"),
        }
    }

    #[test]
    fn parse_assistant_text_block() {
        let text_block = map(vec![("type", s("text")), ("text", s("hi there"))]);
        let msgs = vec![map(vec![
            ("role", s("assistant")),
            ("content", Value::Array(vec![text_block])),
        ])];
        let parsed = parse_chat_messages(&msgs);
        assert_eq!(parsed.len(), 1);
        match &parsed[0] {
            ChatMessage::AssistantChunk(text) => assert_eq!(text, "hi there"),
            other => panic!("expected AssistantChunk, got {other:?}"),
        }
    }

    #[test]
    fn parse_assistant_tool_use() {
        let tool_block = map(vec![
            ("type", s("tool_use")),
            ("id", s("tu_123")),
            ("name", s("read_file")),
            ("input", map(vec![("path", s("/tmp/x"))])),
        ]);
        let msgs = vec![map(vec![
            ("role", s("assistant")),
            ("content", Value::Array(vec![tool_block])),
        ])];
        let parsed = parse_chat_messages(&msgs);
        assert_eq!(parsed.len(), 1);
        match &parsed[0] {
            ChatMessage::ToolCall { name } => assert_eq!(name, "read_file"),
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn parse_user_tool_result() {
        let result_block = map(vec![
            ("type", s("tool_result")),
            ("tool_use_id", s("tu_123")),
            ("content", s("file contents here")),
        ]);
        let msgs = vec![map(vec![
            ("role", s("user")),
            ("content", Value::Array(vec![result_block])),
        ])];
        let parsed = parse_chat_messages(&msgs);
        assert_eq!(parsed.len(), 1);
        match &parsed[0] {
            ChatMessage::ToolResult { name, output } => {
                assert_eq!(name, "tool");
                assert_eq!(output, "file contents here");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn parse_mixed_conversation() {
        let user_msg = map(vec![("role", s("user")), ("content", s("explain this"))]);
        let text_block = map(vec![("type", s("text")), ("text", s("Sure, "))]);
        let tool_block = map(vec![
            ("type", s("tool_use")),
            ("id", s("tu_1")),
            ("name", s("grep")),
            ("input", map(vec![])),
        ]);
        let text_block2 = map(vec![("type", s("text")), ("text", s("done."))]);
        let assistant_msg = map(vec![
            ("role", s("assistant")),
            (
                "content",
                Value::Array(vec![text_block, tool_block, text_block2]),
            ),
        ]);
        let result_block = map(vec![
            ("type", s("tool_result")),
            ("tool_use_id", s("tu_1")),
            ("content", s("match found")),
        ]);
        let user_result = map(vec![
            ("role", s("user")),
            ("content", Value::Array(vec![result_block])),
        ]);

        let msgs = vec![user_msg, assistant_msg, user_result];
        let parsed = parse_chat_messages(&msgs);

        // User, AssistantChunk("Sure, "), ToolCall(grep), AssistantChunk("done."), ToolResult
        assert_eq!(parsed.len(), 5);
        assert!(matches!(&parsed[0], ChatMessage::User(t) if t == "explain this"));
        assert!(matches!(&parsed[1], ChatMessage::AssistantChunk(t) if t == "Sure, "));
        assert!(matches!(&parsed[2], ChatMessage::ToolCall { name } if name == "grep"));
        assert!(matches!(&parsed[3], ChatMessage::AssistantChunk(t) if t == "done."));
        assert!(
            matches!(&parsed[4], ChatMessage::ToolResult { name, output } if name == "tool" && output == "match found")
        );
    }

    #[test]
    fn parse_inbox_threads_basic() {
        let thread = map(vec![
            ("id", s("t_abc")),
            ("title", s("My thread")),
            ("thread_state", s("running")),
            ("labels", Value::Array(vec![s("bug")])),
            ("token_count", Value::Integer(1500)),
            ("last_seq", Value::Integer(3)),
        ]);
        let threads = parse_inbox_threads(&Value::Array(vec![thread]));
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, "t_abc");
        assert_eq!(threads[0].title, "My thread");
        assert_eq!(threads[0].thread_state, "running");
        assert_eq!(threads[0].labels, vec!["bug".to_string()]);
        assert_eq!(threads[0].token_count, 1500);
        assert_eq!(threads[0].last_seq, 3);
    }

    #[test]
    fn parse_inbox_threads_non_array_returns_empty() {
        let result = parse_inbox_threads(&Value::String("not an array".to_string()));
        assert!(result.is_empty());
    }

    #[test]
    fn parse_chat_messages_empty() {
        let parsed = parse_chat_messages(&[]);
        assert!(parsed.is_empty());
    }

    #[test]
    fn parse_unknown_role_ignored() {
        let msgs = vec![map(vec![
            ("role", s("system")),
            ("content", s("you are helpful")),
        ])];
        let parsed = parse_chat_messages(&msgs);
        assert!(parsed.is_empty());
    }

    #[test]
    fn parse_assistant_plain_string_content() {
        let msgs = vec![map(vec![
            ("role", s("assistant")),
            ("content", s("plain text response")),
        ])];
        let parsed = parse_chat_messages(&msgs);
        assert_eq!(parsed.len(), 1);
        assert!(matches!(&parsed[0], ChatMessage::AssistantChunk(t) if t == "plain text response"));
    }
}

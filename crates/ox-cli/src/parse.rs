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
// HistoryEntry — parsed history entries with duplicate detection
// (Superseded by LogDisplayEntry for production use; retained for tests)
// ---------------------------------------------------------------------------
#[allow(dead_code)]
/// A single parsed history entry (one wire-format message).
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub index: usize,
    pub role: String,
    pub summary: String,
    pub block_count: usize,
    pub text_len: usize,
    pub blocks: Vec<HistoryBlock>,
    pub flags: EntryFlags,
}

/// A single content block within a history entry.
#[derive(Debug, Clone)]
pub struct HistoryBlock {
    pub block_type: String,
    pub text: Option<String>,
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
    pub input_json: Option<String>,
}

/// Duplicate-detection flags for a history entry.
#[derive(Debug, Clone, Default)]
pub struct EntryFlags {
    pub duplicate_content: bool,
    pub duplicate_of: Option<usize>,
}

/// Convert a slice of StructFS Values (Anthropic wire-format messages) into
/// `HistoryEntry`s with duplicate detection applied.
#[allow(dead_code)]
pub fn parse_history_entries(values: &[Value]) -> Vec<HistoryEntry> {
    let mut entries: Vec<HistoryEntry> = values
        .iter()
        .enumerate()
        .filter_map(|(index, val)| {
            let map = match val {
                Value::Map(m) => m,
                _ => return None,
            };
            let role = match map.get("role") {
                Some(Value::String(s)) => s.clone(),
                _ => return None,
            };
            let content = match map.get("content") {
                Some(v) => v,
                None => return None,
            };
            let (blocks, summary, block_count, text_len) = parse_content_blocks(&role, content);
            Some(HistoryEntry {
                index,
                role,
                summary,
                block_count,
                text_len,
                blocks,
                flags: EntryFlags::default(),
            })
        })
        .collect();

    detect_duplicates(&mut entries);
    entries
}

/// Parse content into blocks, returning (blocks, summary, block_count, text_len).
fn parse_content_blocks(_role: &str, content: &Value) -> (Vec<HistoryBlock>, String, usize, usize) {
    match content {
        Value::String(s) => {
            let block = HistoryBlock {
                block_type: "text".to_string(),
                text: Some(s.clone()),
                tool_name: None,
                tool_use_id: None,
                input_json: None,
            };
            let summary = truncate_summary(s, 80);
            let text_len = s.len();
            (vec![block], summary, 1, text_len)
        }
        Value::Array(arr) => {
            let mut blocks = Vec::new();
            let mut total_text_len = 0usize;
            let mut first_summary: Option<String> = None;

            for item in arr {
                let Value::Map(block_map) = item else {
                    continue;
                };
                let block_type = match block_map.get("type") {
                    Some(Value::String(s)) => s.clone(),
                    _ => continue,
                };

                match block_type.as_str() {
                    "text" => {
                        let text = match block_map.get("text") {
                            Some(Value::String(s)) => Some(s.clone()),
                            _ => None,
                        };
                        if let Some(ref t) = text {
                            total_text_len += t.len();
                            if first_summary.is_none() {
                                first_summary = Some(truncate_summary(t, 80));
                            }
                        }
                        blocks.push(HistoryBlock {
                            block_type,
                            text,
                            tool_name: None,
                            tool_use_id: None,
                            input_json: None,
                        });
                    }
                    "tool_use" => {
                        let name = match block_map.get("name") {
                            Some(Value::String(s)) => Some(s.clone()),
                            _ => None,
                        };
                        let tool_use_id = match block_map.get("id") {
                            Some(Value::String(s)) => Some(s.clone()),
                            _ => None,
                        };
                        let input_json = block_map.get("input").map(format_value);
                        if first_summary.is_none() {
                            let label = name.as_deref().unwrap_or("unknown");
                            first_summary = Some(format!("tool_use: {label}"));
                        }
                        blocks.push(HistoryBlock {
                            block_type,
                            text: None,
                            tool_name: name,
                            tool_use_id,
                            input_json,
                        });
                    }
                    "tool_result" => {
                        let text = match block_map.get("content") {
                            Some(Value::String(s)) => Some(s.clone()),
                            _ => None,
                        };
                        let tool_use_id = match block_map.get("tool_use_id") {
                            Some(Value::String(s)) => Some(s.clone()),
                            _ => None,
                        };
                        if let Some(ref t) = text {
                            total_text_len += t.len();
                            if first_summary.is_none() {
                                first_summary = Some(truncate_summary(t, 80));
                            }
                        }
                        blocks.push(HistoryBlock {
                            block_type,
                            text,
                            tool_name: None,
                            tool_use_id,
                            input_json: None,
                        });
                    }
                    _ => {
                        blocks.push(HistoryBlock {
                            block_type,
                            text: None,
                            tool_name: None,
                            tool_use_id: None,
                            input_json: None,
                        });
                    }
                }
            }

            let block_count = blocks.len();
            let summary = first_summary.unwrap_or_default();
            (blocks, summary, block_count, total_text_len)
        }
        _ => (Vec::new(), String::new(), 0, 0),
    }
}

/// Return the first line of `s`, truncated to `max` chars.
fn truncate_summary(s: &str, max: usize) -> String {
    let first_line = s.lines().next().unwrap_or(s);
    if first_line.len() <= max {
        first_line.to_string()
    } else {
        first_line[..max].to_string()
    }
}

/// Compact JSON-like formatting of a StructFS Value.
fn format_value(val: &Value) -> String {
    match val {
        Value::String(s) => format!("{s:?}"),
        Value::Integer(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        Value::Bytes(b) => format!("<{} bytes>", b.len()),
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(format_value).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Map(m) => {
            let items: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("{k:?}: {}", format_value(v)))
                .collect();
            format!("{{{}}}", items.join(", "))
        }
    }
}

/// Concatenate all text content from blocks into a single string.
pub fn concat_text(blocks: &[HistoryBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| b.text.as_deref())
        .collect::<Vec<_>>()
        .join("")
}

/// Detect duplicate entries by comparing text content against prior same-role entries.
#[allow(dead_code)]
fn detect_duplicates(entries: &mut [HistoryEntry]) {
    for i in 1..entries.len() {
        let current_role = entries[i].role.clone();
        let current_text = concat_text(&entries[i].blocks);
        if current_text.is_empty() {
            continue;
        }
        // Walk backwards to find the most recent same-role entry.
        let mut found: Option<usize> = None;
        for j in (0..i).rev() {
            if entries[j].role == current_role {
                found = Some(j);
                break;
            }
        }
        if let Some(prev_idx) = found {
            let prev_text = concat_text(&entries[prev_idx].blocks);
            if prev_text == current_text {
                entries[i].flags.duplicate_content = true;
                entries[i].flags.duplicate_of = Some(prev_idx);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// LogDisplayEntry — parsed log entries for history explorer
// ---------------------------------------------------------------------------

/// A single log entry parsed for display in the history explorer.
#[derive(Debug, Clone)]
pub struct LogDisplayEntry {
    pub index: usize,
    /// The LogEntry type tag: "user", "assistant", "tool_call", "tool_result",
    /// "turn_start", "turn_end", "approval_requested", "approval_resolved",
    /// "error", "meta".
    pub entry_type: String,
    /// Primary display text (summary line).
    pub summary: String,
    /// Expandable detail blocks (for message types).
    pub blocks: Vec<HistoryBlock>,
    /// Metadata fields for rendering.
    pub meta: LogEntryMeta,
    /// Duplicate detection flags (applies to user/assistant only).
    pub flags: EntryFlags,
}

/// Type-specific metadata for rendering.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct LogEntryMeta {
    pub scope: Option<String>,
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
    pub decision: Option<ox_types::Decision>,
    pub input_preview: Option<String>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub block_count: usize,
    pub text_len: usize,
}

fn get_string(map: &std::collections::BTreeMap<String, Value>, key: &str) -> Option<String> {
    match map.get(key) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn get_u32(map: &std::collections::BTreeMap<String, Value>, key: &str) -> Option<u32> {
    match map.get(key) {
        Some(Value::Integer(n)) => Some(*n as u32),
        _ => None,
    }
}

/// Parse a slice of LogEntry StructFS Values into `LogDisplayEntry`s with
/// duplicate detection applied to user/assistant entries.
pub fn parse_log_entries(values: &[Value]) -> Vec<LogDisplayEntry> {
    let mut entries: Vec<LogDisplayEntry> = values
        .iter()
        .enumerate()
        .filter_map(|(index, val)| {
            let map = match val {
                Value::Map(m) => m,
                _ => return None,
            };
            let entry_type = get_string(map, "type")?;

            match entry_type.as_str() {
                "user" | "assistant" => {
                    let content = map.get("content")?;
                    let (blocks, summary, block_count, text_len) =
                        parse_content_blocks(&entry_type, content);
                    Some(LogDisplayEntry {
                        index,
                        entry_type,
                        summary,
                        blocks,
                        meta: LogEntryMeta {
                            block_count,
                            text_len,
                            ..Default::default()
                        },
                        flags: EntryFlags::default(),
                    })
                }
                "tool_call" => {
                    let name = get_string(map, "name").unwrap_or_default();
                    let id = get_string(map, "id");
                    let input_json = map.get("input").map(format_value);
                    let summary = format!("tool_call: {name}");
                    Some(LogDisplayEntry {
                        index,
                        entry_type,
                        summary,
                        blocks: Vec::new(),
                        meta: LogEntryMeta {
                            tool_name: Some(name),
                            tool_use_id: id,
                            input_preview: input_json,
                            ..Default::default()
                        },
                        flags: EntryFlags::default(),
                    })
                }
                "tool_result" => {
                    let id = get_string(map, "id");
                    let output = get_string(map, "output").unwrap_or_default();
                    let text_len = output.len();
                    let summary = truncate_summary(&output, 80);
                    let block = HistoryBlock {
                        block_type: "tool_result".to_string(),
                        text: Some(output),
                        tool_name: None,
                        tool_use_id: id.clone(),
                        input_json: None,
                    };
                    Some(LogDisplayEntry {
                        index,
                        entry_type,
                        summary,
                        blocks: vec![block],
                        meta: LogEntryMeta {
                            tool_use_id: id,
                            block_count: 1,
                            text_len,
                            ..Default::default()
                        },
                        flags: EntryFlags::default(),
                    })
                }
                "turn_start" | "turn_end" => {
                    let scope = get_string(map, "scope");
                    let input_tokens = get_u32(map, "input_tokens");
                    let output_tokens = get_u32(map, "output_tokens");
                    let summary = match &scope {
                        Some(s) => format!("{entry_type}: {s}"),
                        None => entry_type.clone(),
                    };
                    Some(LogDisplayEntry {
                        index,
                        entry_type,
                        summary,
                        blocks: Vec::new(),
                        meta: LogEntryMeta {
                            scope,
                            input_tokens,
                            output_tokens,
                            ..Default::default()
                        },
                        flags: EntryFlags::default(),
                    })
                }
                "approval_requested" => {
                    let tool_name = get_string(map, "tool_name");
                    let input_preview = get_string(map, "input_preview");
                    let summary = format!(
                        "{}: \"{}\"",
                        tool_name.as_deref().unwrap_or("unknown"),
                        input_preview.as_deref().unwrap_or("")
                    );
                    Some(LogDisplayEntry {
                        index,
                        entry_type,
                        summary,
                        blocks: Vec::new(),
                        meta: LogEntryMeta {
                            tool_name,
                            input_preview,
                            ..Default::default()
                        },
                        flags: EntryFlags::default(),
                    })
                }
                "approval_resolved" => {
                    let tool_name = get_string(map, "tool_name");
                    let decision_str = get_string(map, "decision");
                    let decision: Option<ox_types::Decision> = decision_str.as_deref().and_then(
                        |s| serde_json::from_value(serde_json::Value::String(s.to_string())).ok(),
                    );
                    let summary = format!(
                        "{}: {}",
                        tool_name.as_deref().unwrap_or("unknown"),
                        decision.map(|d| d.as_str()).unwrap_or("unknown")
                    );
                    Some(LogDisplayEntry {
                        index,
                        entry_type,
                        summary,
                        blocks: Vec::new(),
                        meta: LogEntryMeta {
                            tool_name,
                            decision,
                            ..Default::default()
                        },
                        flags: EntryFlags::default(),
                    })
                }
                "error" => {
                    let message = get_string(map, "message").unwrap_or_default();
                    let summary = truncate_summary(&message, 80);
                    Some(LogDisplayEntry {
                        index,
                        entry_type,
                        summary,
                        blocks: Vec::new(),
                        meta: LogEntryMeta::default(),
                        flags: EntryFlags::default(),
                    })
                }
                "meta" => {
                    let data_preview = map
                        .get("data")
                        .map(format_value)
                        .unwrap_or_else(|| "{}".to_string());
                    let summary = truncate_summary(&data_preview, 80);
                    Some(LogDisplayEntry {
                        index,
                        entry_type,
                        summary,
                        blocks: Vec::new(),
                        meta: LogEntryMeta::default(),
                        flags: EntryFlags::default(),
                    })
                }
                _ => None,
            }
        })
        .collect();

    // Duplicate detection for user/assistant entries only.
    detect_log_duplicates(&mut entries);
    entries
}

/// Detect duplicate log entries by comparing text content of user/assistant entries.
fn detect_log_duplicates(entries: &mut [LogDisplayEntry]) {
    for i in 1..entries.len() {
        let et = &entries[i].entry_type;
        if et != "user" && et != "assistant" {
            continue;
        }
        let current_text = concat_text(&entries[i].blocks);
        if current_text.is_empty() {
            continue;
        }
        let mut found: Option<usize> = None;
        for j in (0..i).rev() {
            if entries[j].entry_type == entries[i].entry_type {
                found = Some(j);
                break;
            }
        }
        if let Some(prev_idx) = found {
            let prev_text = concat_text(&entries[prev_idx].blocks);
            if prev_text == current_text {
                entries[i].flags.duplicate_content = true;
                entries[i].flags.duplicate_of = Some(prev_idx);
            }
        }
    }
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

    #[test]
    fn parse_history_entries_basic() {
        let user_msg = map(vec![("role", s("user")), ("content", s("hello"))]);
        let text_block = map(vec![("type", s("text")), ("text", s("hi there"))]);
        let assistant_msg = map(vec![
            ("role", s("assistant")),
            ("content", Value::Array(vec![text_block])),
        ]);
        let entries = parse_history_entries(&[user_msg, assistant_msg]);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].index, 0);
        assert_eq!(entries[0].role, "user");
        assert_eq!(entries[0].summary, "hello");
        assert_eq!(entries[0].block_count, 1);
        assert_eq!(entries[0].text_len, 5);
        assert!(!entries[0].flags.duplicate_content);
        assert_eq!(entries[1].index, 1);
        assert_eq!(entries[1].role, "assistant");
        assert_eq!(entries[1].summary, "hi there");
        assert_eq!(entries[1].block_count, 1);
        assert_eq!(entries[1].text_len, 8);
    }

    #[test]
    fn parse_history_entries_duplicate_detection() {
        let msg1 = map(vec![
            ("role", s("assistant")),
            ("content", s("hello world")),
        ]);
        let msg2 = map(vec![("role", s("user")), ("content", s("ok"))]);
        let msg3 = map(vec![
            ("role", s("assistant")),
            ("content", s("hello world")),
        ]);
        let entries = parse_history_entries(&[msg1, msg2, msg3]);
        assert_eq!(entries.len(), 3);
        assert!(!entries[0].flags.duplicate_content);
        assert!(!entries[1].flags.duplicate_content);
        assert!(entries[2].flags.duplicate_content);
        assert_eq!(entries[2].flags.duplicate_of, Some(0));
    }

    #[test]
    fn parse_history_entries_tool_blocks() {
        let tool_use_block = map(vec![
            ("type", s("tool_use")),
            ("id", s("toolu_01ABC")),
            ("name", s("read_file")),
            ("input", map(vec![("path", s("/tmp/x"))])),
        ]);
        let assistant_msg = map(vec![
            ("role", s("assistant")),
            ("content", Value::Array(vec![tool_use_block])),
        ]);
        let entries = parse_history_entries(&[assistant_msg]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].block_count, 1);
        assert_eq!(entries[0].blocks[0].block_type, "tool_use");
        assert_eq!(entries[0].blocks[0].tool_name.as_deref(), Some("read_file"));
        assert_eq!(
            entries[0].blocks[0].tool_use_id.as_deref(),
            Some("toolu_01ABC")
        );
        assert!(entries[0].blocks[0].input_json.is_some());
        assert!(entries[0].summary.contains("tool_use: read_file"));
    }

    #[test]
    fn parse_log_entries_mixed_types() {
        let entries = vec![
            map(vec![("type", s("turn_start")), ("scope", s("root"))]),
            map(vec![("type", s("user")), ("content", s("hello"))]),
            map(vec![
                ("type", s("assistant")),
                (
                    "content",
                    Value::Array(vec![map(vec![
                        ("type", s("text")),
                        ("text", s("I'll check")),
                    ])]),
                ),
            ]),
            map(vec![
                ("type", s("approval_requested")),
                ("tool_name", s("bash")),
                ("input_preview", s("ls -la")),
            ]),
            map(vec![
                ("type", s("approval_resolved")),
                ("tool_name", s("bash")),
                ("decision", s("allow_once")),
            ]),
            map(vec![
                ("type", s("tool_call")),
                ("id", s("tc1")),
                ("name", s("bash")),
                ("input", map(vec![("command", s("ls -la"))])),
            ]),
            map(vec![
                ("type", s("tool_result")),
                ("id", s("tc1")),
                ("output", s("file1\nfile2")),
            ]),
            map(vec![
                ("type", s("turn_end")),
                ("scope", s("root")),
                ("input_tokens", Value::Integer(100)),
                ("output_tokens", Value::Integer(50)),
            ]),
        ];
        let parsed = parse_log_entries(&entries);
        assert_eq!(parsed.len(), 8);
        assert_eq!(parsed[0].entry_type, "turn_start");
        assert_eq!(parsed[1].entry_type, "user");
        assert_eq!(parsed[2].entry_type, "assistant");
        assert_eq!(parsed[3].entry_type, "approval_requested");
        assert_eq!(parsed[4].entry_type, "approval_resolved");
        assert_eq!(parsed[5].entry_type, "tool_call");
        assert_eq!(parsed[6].entry_type, "tool_result");
        assert_eq!(parsed[7].entry_type, "turn_end");
    }
}

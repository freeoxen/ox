# Pico-Process Agent Execution Model

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the three-phase kernel state machine with stateless free functions: `run_turn` drives the full agentic loop for sync consumers (ox-wasm pico-process), composable building blocks (`synthesize`, `accumulate_response`, `record_turn`) serve async consumers (ox-web). A structured log records all activity.

**Architecture:** The kernel becomes a set of free functions — no `Kernel` struct, no `KernelState`. `run_turn(context, emit)` reads context to determine state (startup and crash recovery are the same path), calls completion via `tools/completions/complete/{account}`, processes the response, executes tool calls, records to log and history, and loops until no more tool calls. The structured log is append-only and records every event. History continues to work alongside the log during this phase; migration to history-as-log-projection is a follow-up.

**Tech Stack:** Rust (edition 2024), StructFS (structfs-core-store, structfs-serde-store), ox-kernel, ox-tools, ox-context, ox-core, ox-wasm, ox-web.

**Deferred to follow-up plans:**
- `complete` as an LLM-facing tool with ref resolution (requires solving nested-borrow for namespace access)
- History views as projections over the structured log
- Handle registry, async execution, `tools/await`
- Multi-completion turns, inner completion context, fork

---

## Scope Check

This plan covers four subsystems that build on each other:

1. **Structured log store** — append-only StructFS store recording all agent activity.
2. **Stateless kernel API** — free functions replacing the `Kernel` struct and three-phase state machine.
3. **Consumer updates** — ox-wasm uses `run_turn`, ox-web uses building blocks.
4. **Cleanup** — remove `Kernel` struct, `KernelState`, `synthesize_prompt`, `TurnStore`, magic `"prompt"` path.

Each subsystem produces working, testable software. The agent works at each stage.

---

## File Structure

### New files

| File | Responsibility |
|------|---------------|
| `crates/ox-kernel/src/log.rs` | `LogEntry` enum, `LogSource` struct, `LogStore` (append-only, Reader/Writer) |
| `crates/ox-kernel/src/run.rs` | Free functions: `synthesize`, `accumulate_response`, `record_turn`, `execute_tools`, `record_tool_results`, `run_turn`. Stream event codec: `stream_event_to_json`, `json_to_stream_event`, `agent_event_to_json`, `deserialize_events`. |

### Modified files

| File | Change |
|------|--------|
| `crates/ox-kernel/src/lib.rs` | Add `pub mod log; pub mod run;`. Re-export key functions from `run`. Keep `Kernel` struct until Task 7. |
| `crates/ox-core/src/lib.rs` | Mount `LogStore` at `"log"`. Remove `kernel` field from `Agent`. Update re-exports. Add integration tests for `run_turn`. |
| `crates/ox-wasm/src/lib.rs` | Replace manual three-phase loop with `ox_kernel::run_turn()`. Remove `json_to_stream_event`, `deserialize_events`. |
| `crates/ox-web/src/lib.rs` | Replace `kernel.initiate_completion()` → `ox_kernel::synthesize()`, `kernel.consume_events()` → `ox_kernel::accumulate_response()`, `kernel.complete_turn()` → `ox_kernel::record_turn()`. Remove `Kernel::new()`. |
| `crates/ox-context/src/lib.rs` | Remove `synthesize_prompt()`, remove magic `"prompt"` intercept in `Namespace::read()`, remove unused imports. |
| `crates/ox-tools/src/lib.rs` | Remove `TurnStore` field and `turn/*` routing. Remove `pub mod turn;`. |

### Deleted files

| File | Reason |
|------|--------|
| `crates/ox-tools/src/turn.rs` | Replaced by kernel loop + structured log. |

---

## Subsystem 1: Structured Log Store

### Task 1: LogEntry types + LogStore with Reader/Writer

**Files:**
- Create: `crates/ox-kernel/src/log.rs`
- Modify: `crates/ox-kernel/src/lib.rs` (add `pub mod log;`)

- [ ] **Step 1: Write the failing tests**

```rust
// crates/ox-kernel/src/log.rs — tests at the bottom

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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ox-kernel -- log::tests -v`
Expected: FAIL — module `log` doesn't exist.

- [ ] **Step 3: Write LogEntry, LogSource, and LogStore**

```rust
// crates/ox-kernel/src/log.rs

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
```

- [ ] **Step 4: Add `pub mod log;` to `crates/ox-kernel/src/lib.rs`**

Add after the existing module declarations (after `pub mod backing;`).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ox-kernel -- log::tests -v`
Expected: PASS — all 6 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-kernel/src/log.rs crates/ox-kernel/src/lib.rs
git commit -m "feat(ox-kernel): add LogStore — append-only structured log with Reader/Writer"
```

---

## Subsystem 2: Stateless Kernel API

### Task 2: Stream event codec + synthesize + accumulate_response + record helpers

**Files:**
- Create: `crates/ox-kernel/src/run.rs`
- Modify: `crates/ox-kernel/src/lib.rs` (add `pub mod run;` and re-exports)

- [ ] **Step 1: Write the failing tests**

```rust
// At the bottom of crates/ox-kernel/src/run.rs

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use structfs_core_store::{path, Path, Record, Value};

    // ------- MockStore -------

    struct MockStore {
        data: BTreeMap<String, Value>,
        appended: Vec<(String, Value)>,
    }

    impl MockStore {
        fn new() -> Self {
            Self {
                data: BTreeMap::new(),
                appended: Vec::new(),
            }
        }

        fn set(&mut self, path: &str, value: Value) {
            self.data.insert(path.to_string(), value);
        }
    }

    impl Reader for MockStore {
        fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
            let key = from.to_string();
            Ok(self.data.get(&key).map(|v| Record::parsed(v.clone())))
        }
    }

    impl Writer for MockStore {
        fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
            if let Record::Parsed(v) = &data {
                self.appended.push((to.to_string(), v.clone()));
            }
            let value = match data {
                Record::Parsed(v) => v,
                _ => return Err(StoreError::store("mock", "write", "expected parsed")),
            };
            self.data.insert(to.to_string(), value);
            Ok(to.clone())
        }
    }

    // ------- Tests -------

    #[test]
    fn stream_event_json_roundtrip() {
        let events = vec![
            StreamEvent::TextDelta("hello".into()),
            StreamEvent::ToolUseStart {
                id: "t1".into(),
                name: "read_file".into(),
            },
            StreamEvent::ToolUseInputDelta("{\"path\":".into()),
            StreamEvent::MessageStop,
            StreamEvent::Error("oops".into()),
        ];
        for event in &events {
            let json = stream_event_to_json(event);
            let roundtripped = json_to_stream_event(&json).unwrap();
            assert_eq!(format!("{event:?}"), format!("{roundtripped:?}"));
        }
    }

    #[test]
    fn synthesize_assembles_request() {
        let mut store = MockStore::new();
        store.set("system", Value::String("You are helpful.".into()));
        store.set(
            "history/messages",
            structfs_serde_store::json_to_value(
                serde_json::json!([{"role": "user", "content": "hello"}]),
            ),
        );
        store.set(
            "tools/schemas",
            structfs_serde_store::json_to_value(serde_json::json!([])),
        );
        store.set("gate/defaults/model", Value::String("test-model".into()));
        store.set("gate/defaults/max_tokens", Value::Integer(100));

        let request = synthesize(&mut store).unwrap();
        assert_eq!(request.model, "test-model");
        assert_eq!(request.system, "You are helpful.");
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.max_tokens, 100);
        assert!(request.stream);
    }

    #[test]
    fn synthesize_fails_on_missing_system() {
        let mut store = MockStore::new();
        assert!(synthesize(&mut store).is_err());
    }

    #[test]
    fn accumulate_text_only() {
        let events = vec![
            StreamEvent::TextDelta("hello ".into()),
            StreamEvent::TextDelta("world".into()),
            StreamEvent::MessageStop,
        ];
        let mut emitted = vec![];
        let content =
            accumulate_response(events, &mut |e| emitted.push(format!("{e:?}"))).unwrap();
        assert_eq!(content.len(), 1);
        match &content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hello world"),
            _ => panic!("expected text block"),
        }
    }

    #[test]
    fn accumulate_tool_use() {
        let events = vec![
            StreamEvent::ToolUseStart {
                id: "t1".into(),
                name: "read_file".into(),
            },
            StreamEvent::ToolUseInputDelta("{\"path\": \"src/main.rs\"}".into()),
            StreamEvent::MessageStop,
        ];
        let content = accumulate_response(events, &mut |_| {}).unwrap();
        assert_eq!(content.len(), 1);
        match &content[0] {
            ContentBlock::ToolUse(tc) => {
                assert_eq!(tc.name, "read_file");
                assert_eq!(tc.input["path"], "src/main.rs");
            }
            _ => panic!("expected tool use"),
        }
    }

    #[test]
    fn accumulate_mixed_text_and_tools() {
        let events = vec![
            StreamEvent::TextDelta("I'll read it.".into()),
            StreamEvent::ToolUseStart {
                id: "t1".into(),
                name: "read_file".into(),
            },
            StreamEvent::ToolUseInputDelta("{}".into()),
            StreamEvent::MessageStop,
        ];
        let content = accumulate_response(events, &mut |_| {}).unwrap();
        assert_eq!(content.len(), 2);
        assert!(matches!(&content[0], ContentBlock::Text { .. }));
        assert!(matches!(&content[1], ContentBlock::ToolUse(_)));
    }

    #[test]
    fn record_turn_writes_history_and_extracts_tools() {
        let mut store = MockStore::new();
        let content = vec![
            ContentBlock::Text {
                text: "Let me read that.".into(),
            },
            ContentBlock::ToolUse(ToolCall {
                id: "tc1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "src/main.rs"}),
            }),
        ];
        let tool_calls = record_turn(&mut store, &content).unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "read_file");
        assert!(store.appended.iter().any(|(p, _)| p == "history/append"));
    }

    #[test]
    fn record_turn_no_tools() {
        let mut store = MockStore::new();
        let content = vec![ContentBlock::Text {
            text: "Done.".into(),
        }];
        let tool_calls = record_turn(&mut store, &content).unwrap();
        assert!(tool_calls.is_empty());
    }

    #[test]
    fn record_tool_results_writes_history() {
        let mut store = MockStore::new();
        let results = vec![ToolResult {
            tool_use_id: "tc1".into(),
            content: serde_json::json!("file contents"),
        }];
        record_tool_results(&mut store, &results).unwrap();
        assert!(store.appended.iter().any(|(p, _)| p == "history/append"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ox-kernel -- run::tests -v`
Expected: FAIL — module `run` doesn't exist.

- [ ] **Step 3: Write the implementation**

```rust
// crates/ox-kernel/src/run.rs

//! Stateless agent execution — free functions for running agent turns.
//!
//! These replace the `Kernel` struct's three-phase API. `run_turn` drives
//! the full agentic loop for sync consumers (ox-wasm pico-process). The
//! building blocks (`synthesize`, `accumulate_response`, `record_turn`)
//! are individually callable for async consumers (ox-web).

use crate::{
    AgentEvent, CompletionRequest, ContentBlock, StreamEvent, ToolCall, ToolResult, ToolSchema,
    serialize_assistant_message, serialize_tool_results,
};
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Store, Value, Writer, path};

// ---------------------------------------------------------------------------
// Stream event codec
// ---------------------------------------------------------------------------

/// Serialize a StreamEvent to JSON.
pub fn stream_event_to_json(event: &StreamEvent) -> serde_json::Value {
    match event {
        StreamEvent::TextDelta(text) => serde_json::json!({"type": "text_delta", "text": text}),
        StreamEvent::ToolUseStart { id, name } => {
            serde_json::json!({"type": "tool_use_start", "id": id, "name": name})
        }
        StreamEvent::ToolUseInputDelta(delta) => {
            serde_json::json!({"type": "tool_use_input_delta", "delta": delta})
        }
        StreamEvent::MessageStop => serde_json::json!({"type": "message_stop"}),
        StreamEvent::Error(msg) => serde_json::json!({"type": "error", "message": msg}),
    }
}

/// Deserialize a StreamEvent from JSON.
pub fn json_to_stream_event(json: &serde_json::Value) -> Result<StreamEvent, String> {
    let obj = json
        .as_object()
        .ok_or_else(|| "expected JSON object for StreamEvent".to_string())?;
    let event_type = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'type' field".to_string())?;
    match event_type {
        "text_delta" => Ok(StreamEvent::TextDelta(
            obj.get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        )),
        "tool_use_start" => Ok(StreamEvent::ToolUseStart {
            id: obj
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            name: obj
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        }),
        "tool_use_input_delta" => Ok(StreamEvent::ToolUseInputDelta(
            obj.get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        )),
        "message_stop" => Ok(StreamEvent::MessageStop),
        "error" => Ok(StreamEvent::Error(
            obj.get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error")
                .to_string(),
        )),
        other => Err(format!("unknown StreamEvent type: {other}")),
    }
}

/// Serialize an AgentEvent to JSON (for events/emit transport).
pub fn agent_event_to_json(event: &AgentEvent) -> serde_json::Value {
    match event {
        AgentEvent::TurnStart => serde_json::json!({"type": "turn_start"}),
        AgentEvent::TextDelta(text) => serde_json::json!({"type": "text_delta", "text": text}),
        AgentEvent::ToolCallStart { name } => {
            serde_json::json!({"type": "tool_call_start", "name": name})
        }
        AgentEvent::ToolCallResult { name, result } => {
            serde_json::json!({"type": "tool_call_result", "name": name, "result": result})
        }
        AgentEvent::TurnEnd => serde_json::json!({"type": "turn_end"}),
        AgentEvent::Error(e) => serde_json::json!({"type": "error", "message": e}),
    }
}

/// Deserialize stream events from a StructFS Record (JSON array of event objects).
pub fn deserialize_events(record: Record) -> Result<Vec<StreamEvent>, String> {
    let value = record
        .as_value()
        .ok_or_else(|| "expected parsed record for events".to_string())?;
    let json = structfs_serde_store::value_to_json(value.clone());
    let arr = json
        .as_array()
        .ok_or_else(|| "expected JSON array of events".to_string())?;
    arr.iter().map(json_to_stream_event).collect()
}

// ---------------------------------------------------------------------------
// Building blocks
// ---------------------------------------------------------------------------

/// Read context and assemble a CompletionRequest.
///
/// Reads from: `system`, `history/messages`, `tools/schemas`,
/// `gate/defaults/model`, `gate/defaults/max_tokens`.
pub fn synthesize(context: &mut dyn Reader) -> Result<CompletionRequest, String> {
    let system_str = match context
        .read(&path!("system"))
        .map_err(|e| e.to_string())?
    {
        Some(Record::Parsed(Value::String(s))) => s,
        Some(_) => return Err("expected string from system store".into()),
        None => return Err("system store returned None".into()),
    };

    let messages_json = match context
        .read(&path!("history/messages"))
        .map_err(|e| e.to_string())?
    {
        Some(Record::Parsed(v)) => structfs_serde_store::value_to_json(v),
        _ => return Err("expected parsed record from history".into()),
    };

    let tools_json = match context
        .read(&path!("tools/schemas"))
        .map_err(|e| e.to_string())?
    {
        Some(Record::Parsed(v)) => structfs_serde_store::value_to_json(v),
        _ => return Err("expected parsed record from tools".into()),
    };

    let model_id = match context
        .read(&path!("gate/defaults/model"))
        .map_err(|e| e.to_string())?
    {
        Some(Record::Parsed(Value::String(s))) => s,
        _ => return Err("expected string from gate defaults/model".into()),
    };

    let max_tokens = match context
        .read(&path!("gate/defaults/max_tokens"))
        .map_err(|e| e.to_string())?
    {
        Some(Record::Parsed(Value::Integer(n))) => n as u32,
        _ => return Err("expected integer from gate defaults/max_tokens".into()),
    };

    let messages: Vec<serde_json::Value> =
        serde_json::from_value(messages_json).map_err(|e| e.to_string())?;
    let tools: Vec<ToolSchema> =
        serde_json::from_value(tools_json).map_err(|e| e.to_string())?;

    Ok(CompletionRequest {
        model: model_id,
        max_tokens,
        system: system_str,
        messages,
        tools,
        stream: true,
    })
}

/// Accumulate stream events into content blocks, emitting AgentEvents.
///
/// Pure function — no state, no store access.
pub fn accumulate_response(
    events: Vec<StreamEvent>,
    emit: &mut dyn FnMut(AgentEvent),
) -> Result<Vec<ContentBlock>, String> {
    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut current_text = String::new();
    let mut current_tool: Option<(String, String, String)> = None;

    for event in events {
        match event {
            StreamEvent::TextDelta(text) => {
                flush_tool(&mut blocks, &mut current_tool);
                current_text.push_str(&text);
                emit(AgentEvent::TextDelta(text));
            }
            StreamEvent::ToolUseStart { id, name } => {
                flush_text(&mut blocks, &mut current_text);
                flush_tool(&mut blocks, &mut current_tool);
                current_tool = Some((id, name, String::new()));
            }
            StreamEvent::ToolUseInputDelta(delta) => {
                if let Some((_, _, ref mut input_json)) = current_tool {
                    input_json.push_str(&delta);
                }
            }
            StreamEvent::MessageStop => break,
            StreamEvent::Error(e) => {
                flush_text(&mut blocks, &mut current_text);
                flush_tool(&mut blocks, &mut current_tool);
                emit(AgentEvent::Error(e.clone()));
                return Err(e);
            }
        }
    }

    flush_text(&mut blocks, &mut current_text);
    flush_tool(&mut blocks, &mut current_tool);
    Ok(blocks)
}

fn flush_text(blocks: &mut Vec<ContentBlock>, text: &mut String) {
    if !text.is_empty() {
        blocks.push(ContentBlock::Text {
            text: std::mem::take(text),
        });
    }
}

fn flush_tool(blocks: &mut Vec<ContentBlock>, tool: &mut Option<(String, String, String)>) {
    if let Some((id, name, input_json)) = tool.take() {
        let input: serde_json::Value =
            serde_json::from_str(&input_json).unwrap_or(serde_json::Value::Null);
        blocks.push(ContentBlock::ToolUse(ToolCall { id, name, input }));
    }
}

/// Write assistant message to history, extract tool calls.
pub fn record_turn(
    context: &mut dyn Writer,
    content: &[ContentBlock],
) -> Result<Vec<ToolCall>, String> {
    let assistant_json = serialize_assistant_message(content);
    let record = Record::parsed(structfs_serde_store::json_to_value(assistant_json));
    context
        .write(&path!("history/append"), record)
        .map_err(|e| e.to_string())?;

    let tool_calls: Vec<ToolCall> = content
        .iter()
        .filter_map(|block| {
            if let ContentBlock::ToolUse(tc) = block {
                Some(tc.clone())
            } else {
                None
            }
        })
        .collect();

    Ok(tool_calls)
}

/// Write tool results to history.
pub fn record_tool_results(
    context: &mut dyn Writer,
    results: &[ToolResult],
) -> Result<(), String> {
    let results_json = serialize_tool_results(results);
    let record = Record::parsed(structfs_serde_store::json_to_value(results_json));
    context
        .write(&path!("history/append"), record)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Execute tool calls through the store. Returns tool results.
///
/// For each tool call: write input to `tools/{wire_name}`, read result
/// from `tools/{wire_name}/result`.
pub fn execute_tools(
    context: &mut dyn Store,
    tool_calls: &[ToolCall],
    emit: &mut dyn FnMut(AgentEvent),
) -> Result<Vec<ToolResult>, String> {
    let mut results = Vec::new();

    for tc in tool_calls {
        emit(AgentEvent::ToolCallStart {
            name: tc.name.clone(),
        });

        let input_value = structfs_serde_store::json_to_value(tc.input.clone());
        let tool_path =
            Path::parse(&format!("tools/{}", tc.name)).map_err(|e| e.to_string())?;

        let (result_str, _is_error) =
            match context.write(&tool_path, Record::parsed(input_value)) {
                Ok(_) => {
                    let result_path = Path::parse(&format!("tools/{}/result", tc.name))
                        .map_err(|e| e.to_string())?;
                    match context.read(&result_path) {
                        Ok(Some(record)) => {
                            let val = record.as_value().cloned().unwrap_or(Value::Null);
                            let json = structfs_serde_store::value_to_json(val);
                            (serde_json::to_string(&json).unwrap_or_default(), false)
                        }
                        Ok(None) => (format!("error: no result for tool {}", tc.name), true),
                        Err(e) => (format!("error: {e}"), true),
                    }
                }
                Err(e) => (e.to_string(), true),
            };

        emit(AgentEvent::ToolCallResult {
            name: tc.name.clone(),
            result: result_str.clone(),
        });

        results.push(ToolResult {
            tool_use_id: tc.id.clone(),
            content: serde_json::Value::String(result_str),
        });
    }

    Ok(results)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Send a completion request through the store and return deserialized events.
fn send_completion(
    context: &mut dyn Store,
    account: &str,
    request: &CompletionRequest,
) -> Result<Vec<StreamEvent>, String> {
    let request_json = serde_json::to_value(request).map_err(|e| e.to_string())?;
    let request_value = structfs_serde_store::json_to_value(request_json);
    let complete_path = Path::parse(&format!("tools/completions/complete/{account}"))
        .map_err(|e| e.to_string())?;
    context
        .write(&complete_path, Record::parsed(request_value))
        .map_err(|e| e.to_string())?;

    let response_path =
        Path::parse(&format!("tools/completions/complete/{account}/response"))
            .map_err(|e| e.to_string())?;
    let response = context
        .read(&response_path)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no completion response".to_string())?;

    deserialize_events(response)
}

/// Read the default account name from gate config.
fn read_default_account(context: &mut dyn Reader) -> Result<String, String> {
    match context
        .read(&path!("gate/defaults/account"))
        .map_err(|e| e.to_string())?
    {
        Some(Record::Parsed(Value::String(s))) => Ok(s),
        _ => Ok("anthropic".to_string()),
    }
}

/// Write a log entry (best-effort — missing "log" mount doesn't break the loop).
fn log_entry(context: &mut dyn Writer, entry: serde_json::Value) {
    let _ = context.write(
        &path!("log/append"),
        Record::parsed(structfs_serde_store::json_to_value(entry)),
    );
}

// ---------------------------------------------------------------------------
// Full loop
// ---------------------------------------------------------------------------

/// Run one complete agent turn to resolution.
///
/// Reads context, determines state, fires completion via
/// `tools/completions/complete/{account}`, processes responses, executes
/// tool calls, records to history and log, and loops until no more tool
/// calls.
///
/// Stateless — all state is in the context. Startup and crash recovery
/// are the same code path.
pub fn run_turn(
    context: &mut dyn Store,
    emit: &mut dyn FnMut(AgentEvent),
) -> Result<(), String> {
    let account = read_default_account(context)?;

    loop {
        emit(AgentEvent::TurnStart);

        let request = synthesize(context)?;
        let events = send_completion(context, &account, &request)?;
        let content = accumulate_response(events, emit)?;

        // Log the assistant response
        let content_json: Vec<serde_json::Value> = content
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } => serde_json::json!({"type": "text", "text": text}),
                ContentBlock::ToolUse(tc) => serde_json::json!({
                    "type": "tool_use", "id": tc.id, "name": tc.name, "input": tc.input
                }),
            })
            .collect();
        log_entry(
            context,
            serde_json::json!({"type": "assistant", "content": content_json}),
        );

        let tool_calls = record_turn(context, &content)?;

        if tool_calls.is_empty() {
            emit(AgentEvent::TurnEnd);
            return Ok(());
        }

        // Log tool calls
        for tc in &tool_calls {
            log_entry(
                context,
                serde_json::json!({
                    "type": "tool_call", "id": tc.id, "name": tc.name, "input": tc.input
                }),
            );
        }

        let results = execute_tools(context, &tool_calls, emit)?;

        // Log tool results
        for result in &results {
            log_entry(
                context,
                serde_json::json!({
                    "type": "tool_result",
                    "id": result.tool_use_id,
                    "output": result.content,
                    "is_error": false
                }),
            );
        }

        record_tool_results(context, &results)?;
    }
}
```

- [ ] **Step 4: Add `pub mod run;` and re-exports to `crates/ox-kernel/src/lib.rs`**

Add after existing module declarations:
```rust
pub mod run;
pub use run::{
    accumulate_response, agent_event_to_json, deserialize_events, execute_tools,
    json_to_stream_event, record_tool_results, record_turn, run_turn,
    stream_event_to_json, synthesize,
};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ox-kernel -- run::tests -v`
Expected: PASS — all tests.

- [ ] **Step 6: Run full workspace check**

Run: `cargo check --workspace`
Expected: PASS — new functions are additive, nothing removed yet.

- [ ] **Step 7: Commit**

```bash
git add crates/ox-kernel/src/run.rs crates/ox-kernel/src/lib.rs
git commit -m "feat(ox-kernel): stateless kernel API — synthesize, accumulate, record_turn, execute_tools, run_turn"
```

---

### Task 3: Integration test for run_turn

**Files:**
- Modify: `crates/ox-core/src/lib.rs` (add integration tests)

- [ ] **Step 1: Write the integration tests**

Append to `crates/ox-core/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ox_kernel::{
        AgentEvent, CompletionRequest, StreamEvent, run_turn,
    };
    use ox_tools::completion::CompletionTransport;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Mock transport returning a sequence of canned responses.
    struct SequentialTransport {
        responses: Mutex<VecDeque<(Vec<StreamEvent>, u32, u32)>>,
    }

    impl SequentialTransport {
        fn new(responses: Vec<(Vec<StreamEvent>, u32, u32)>) -> Self {
            Self {
                responses: Mutex::new(VecDeque::from(responses)),
            }
        }
    }

    impl CompletionTransport for SequentialTransport {
        fn send(
            &self,
            _request: &CompletionRequest,
            on_event: &dyn Fn(&StreamEvent),
        ) -> Result<(Vec<StreamEvent>, u32, u32), String> {
            let resp = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or("no more canned responses")?;
            for event in &resp.0 {
                on_event(event);
            }
            Ok(resp)
        }
    }

    fn make_namespace(transport: SequentialTransport) -> Namespace {
        let mut tool_store = ox_tools::ToolStore::empty();
        tool_store
            .completions_mut()
            .set_transport(Box::new(transport));

        let mut ns = Namespace::new();
        ns.mount(
            "system",
            Box::new(SystemProvider::new("You are a test bot.".into())),
        );
        ns.mount("history", Box::new(HistoryProvider::new()));
        ns.mount("tools", Box::new(tool_store));
        ns.mount("gate", Box::new(ox_gate::GateStore::new()));
        ns.mount("log", Box::new(ox_kernel::log::LogStore::new()));
        ns
    }

    fn seed_user_message(ns: &mut Namespace, text: &str) {
        ns.write(
            &ox_kernel::path!("history/append"),
            ox_kernel::Record::parsed(structfs_serde_store::json_to_value(
                serde_json::json!({"role": "user", "content": text}),
            )),
        )
        .unwrap();
    }

    #[test]
    fn run_turn_text_only_response() {
        let transport = SequentialTransport::new(vec![(
            vec![
                StreamEvent::TextDelta("Hello!".into()),
                StreamEvent::MessageStop,
            ],
            10,
            5,
        )]);

        let mut ns = make_namespace(transport);
        seed_user_message(&mut ns, "hi");

        let mut events = vec![];
        run_turn(&mut ns, &mut |e| events.push(format!("{e:?}"))).unwrap();

        assert!(events.iter().any(|e| e.contains("TurnStart")));
        assert!(events.iter().any(|e| e.contains("TurnEnd")));

        // History: user + assistant = 2
        let count = ns
            .read(&ox_kernel::path!("history/count"))
            .unwrap()
            .unwrap();
        assert_eq!(count.as_value().unwrap(), &ox_kernel::Value::Integer(2));

        // Log should have the assistant entry
        let log_count = ns
            .read(&ox_kernel::path!("log/count"))
            .unwrap()
            .unwrap();
        assert_eq!(
            log_count.as_value().unwrap(),
            &ox_kernel::Value::Integer(1)
        );
    }

    #[test]
    fn run_turn_with_tool_call() {
        let transport = SequentialTransport::new(vec![
            // First response: tool call
            (
                vec![
                    StreamEvent::ToolUseStart {
                        id: "tc1".into(),
                        name: "echo_tool".into(),
                    },
                    StreamEvent::ToolUseInputDelta("{\"text\": \"ping\"}".into()),
                    StreamEvent::MessageStop,
                ],
                10,
                5,
            ),
            // Second response: text only
            (
                vec![
                    StreamEvent::TextDelta("pong".into()),
                    StreamEvent::MessageStop,
                ],
                10,
                5,
            ),
        ]);

        let mut tool_store = ox_tools::ToolStore::empty();
        tool_store
            .completions_mut()
            .set_transport(Box::new(transport));

        let echo = ox_tools::native::FnTool::new(
            "echo_tool",
            "native/echo",
            "echoes input",
            serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
            |input| Ok(input),
        );
        tool_store.register_native(Box::new(echo));

        let mut ns = Namespace::new();
        ns.mount(
            "system",
            Box::new(SystemProvider::new("Test.".into())),
        );
        ns.mount("history", Box::new(HistoryProvider::new()));
        ns.mount("tools", Box::new(tool_store));
        ns.mount("gate", Box::new(ox_gate::GateStore::new()));
        ns.mount("log", Box::new(ox_kernel::log::LogStore::new()));
        seed_user_message(&mut ns, "echo ping");

        let mut events = vec![];
        run_turn(&mut ns, &mut |e| events.push(format!("{e:?}"))).unwrap();

        assert!(events.iter().any(|e| e.contains("ToolCallStart")));
        assert!(events.iter().any(|e| e.contains("ToolCallResult")));
        // Two loop iterations = two TurnStarts
        let turn_starts = events.iter().filter(|e| e.contains("TurnStart")).count();
        assert_eq!(turn_starts, 2);
        // History: user + assistant(tool) + tool_result + assistant(text) = 4
        let count = ns
            .read(&ox_kernel::path!("history/count"))
            .unwrap()
            .unwrap();
        assert_eq!(count.as_value().unwrap(), &ox_kernel::Value::Integer(4));
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p ox-core -v`
Expected: PASS — both integration tests.

- [ ] **Step 3: Commit**

```bash
git add crates/ox-core/src/lib.rs
git commit -m "test(ox-core): integration tests for run_turn with mock transport"
```

---

## Subsystem 3: Consumer Updates

### Task 4: ox-wasm uses run_turn

**Files:**
- Modify: `crates/ox-wasm/src/lib.rs`

- [ ] **Step 1: Replace `agent_main` body**

Replace the entire `agent_main` function (lines 201–350) with:

```rust
fn agent_main() -> Result<(), String> {
    let mut bridge = HostBridge;

    let mut emit = |event: ox_kernel::AgentEvent| {
        let json = ox_kernel::agent_event_to_json(&event);
        let json_str = serde_json::to_string(&json).unwrap_or_default();
        let _ = host_write("events/emit", &json_str);
    };

    ox_kernel::run_turn(&mut bridge, &mut emit)
}
```

- [ ] **Step 2: Remove dead code**

Remove from `crates/ox-wasm/src/lib.rs`:
- The `json_to_stream_event` function (lines 117–165)
- The `deserialize_events` function (lines 167–176)
- Update the top import: replace `use ox_kernel::{AgentEvent, Kernel, StreamEvent, ToolResult};` with `use ox_kernel::AgentEvent;` (the only type still used is `AgentEvent` in the emit closure, but it's qualified via `ox_kernel::AgentEvent` in the closure so the import can be removed entirely if unused elsewhere)

Keep: `HostBridge`, `host_read`, `host_write`, extern imports, `run()` entry point, `wasm_subscriber` module.

- [ ] **Step 3: Verify wasm target compiles**

Run: `cargo check --target wasm32-unknown-unknown -p ox-wasm`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-wasm/src/lib.rs
git commit -m "refactor(ox-wasm): replace manual three-phase loop with ox_kernel::run_turn"
```

---

### Task 5: ox-web uses new kernel functions

**Files:**
- Modify: `crates/ox-web/src/lib.rs`

ox-web keeps its manual async loop (it needs the yield point for `fetch_completion().await`) but switches from the `Kernel` struct's methods to the new free functions.

- [ ] **Step 1: Update run_agentic_loop**

In `run_agentic_loop` (~line 720):

Remove the `Kernel::new(model_id)` line (~line 772). Remove the `kernel` variable entirely.

Replace the loop body. Change:
- `kernel.initiate_completion(&mut *context_ref.borrow_mut())` → `ox_kernel::synthesize(&mut *context_ref.borrow_mut())`
- `kernel.consume_events(events, &mut emit)` → `ox_kernel::accumulate_response(events, &mut emit)`
- `kernel.complete_turn(&mut *context_ref.borrow_mut(), &content)` → `ox_kernel::record_turn(&mut *context_ref.borrow_mut(), &content)`

After the tool execution loop, replace the manual `serialize_tool_results` + `history/append` write with:
```rust
ox_kernel::record_tool_results(&mut *context_ref.borrow_mut(), &results)?;
```

- [ ] **Step 2: Remove Kernel import**

Remove `Kernel` from the ox-kernel import list if present. The `model_id` read from gate can also be removed (synthesize reads it internally).

- [ ] **Step 3: Verify wasm target compiles**

Run: `cargo check --target wasm32-unknown-unknown -p ox-web`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-web/src/lib.rs
git commit -m "refactor(ox-web): use stateless kernel functions instead of Kernel struct"
```

---

## Subsystem 4: Cleanup

### Task 6: Remove Kernel struct, KernelState, synthesize_prompt, TurnStore

**Files:**
- Modify: `crates/ox-kernel/src/lib.rs`
- Modify: `crates/ox-context/src/lib.rs`
- Modify: `crates/ox-tools/src/lib.rs`
- Delete: `crates/ox-tools/src/turn.rs`
- Modify: `crates/ox-core/src/lib.rs`

- [ ] **Step 1: Remove Kernel struct and KernelState from ox-kernel**

In `crates/ox-kernel/src/lib.rs`:
- Delete the `KernelState` enum (~lines 211–219)
- Delete the `Kernel` struct and all its `impl` blocks (~lines 221–427), including the private `accumulate_response` and `flush_pending` methods (now in `run.rs`)
- Delete the `MockStore` and all tests that test the `Kernel` struct (the `#[cfg(test)] mod tests` block at the bottom — ~lines 486 to end). These tests are superseded by the `run::tests` module.
- Remove `Kernel` and `KernelState` from any `pub use` re-exports

Keep: `CompletionRequest`, `StreamEvent`, `AgentEvent`, `Message`, `ContentBlock`, `ToolCall`, `ToolResult`, `ToolSchema`, `ModelInfo`, `serialize_assistant_message`, `serialize_tool_results`, and all StructFS re-exports.

- [ ] **Step 2: Remove synthesize_prompt and magic "prompt" path from ox-context**

In `crates/ox-context/src/lib.rs`:
- Delete the standalone `pub fn synthesize_prompt(reader: &mut dyn Reader)` function (~lines 69–199)
- Delete the `fn synthesize_prompt(&mut self)` method on Namespace (~lines 52–54)
- In `Namespace::read()`, remove the "prompt" intercept (~lines 210–212):
  ```rust
  // DELETE these three lines:
  if from == &path!("prompt") {
      return self.synthesize_prompt();
  }
  ```
- Remove the `use ox_kernel::CompletionRequest;` import (~line 14)
- Remove the `use structfs_serde_store::{to_value, value_to_json};` import (~line 18) — no longer used after synthesize_prompt is gone. The tests use `structfs_serde_store::json_to_value` with the full path.
- Delete the `synthesize_prompt_standalone` test (~lines 468–485)

- [ ] **Step 3: Remove TurnStore from ox-tools**

In `crates/ox-tools/src/lib.rs`:
- Remove `pub mod turn;` (~line 8)
- Remove `use crate::turn::{EffectOutcome, TurnStore};` (~line 19)
- Remove `turn: TurnStore` field from `ToolStore` struct (~line 49)
- Remove `turn: TurnStore::new(),` from the `Self { ... }` in `ToolStore::new()` (~line 74)
- Remove `"turn"` from the `resolve_path` match arm (~line 161): change `"fs" | "os" | "completions" | "schemas" | "turn"` to `"fs" | "os" | "completions" | "schemas"`
- Remove the `"turn"` arm from `Reader::read` (~lines 269–316)
- Remove the `"turn"` arm from `Writer::write` (~lines 404–483)

Delete `crates/ox-tools/src/turn.rs` entirely.

- [ ] **Step 4: Update Agent struct in ox-core**

In `crates/ox-core/src/lib.rs`:
- Remove `kernel: Kernel` field from `Agent` struct
- Remove `Kernel::new("default".into())` from `Agent::new()` — also add LogStore mount:

```rust
pub struct Agent {
    context: Namespace,
    subscribers: Vec<Box<dyn FnMut(AgentEvent)>>,
}

impl Agent {
    pub fn new(system_prompt: String, tool_store: ox_tools::ToolStore) -> Self {
        let mut context = Namespace::new();
        context.mount("system", Box::new(SystemProvider::new(system_prompt)));
        context.mount("history", Box::new(HistoryProvider::new()));
        context.mount("tools", Box::new(tool_store));
        context.mount("gate", Box::new(ox_gate::GateStore::new()));
        context.mount("log", Box::new(ox_kernel::log::LogStore::new()));

        Self {
            context,
            subscribers: Vec::new(),
        }
    }
}
```

- Update re-exports: remove `Kernel` from `pub use ox_kernel::{...}`. Add `run_turn, synthesize, accumulate_response, record_turn, execute_tools, record_tool_results`.

- [ ] **Step 5: Run full workspace check and tests**

Run: `cargo check --workspace && cargo test --workspace`
Expected: PASS — no code references the removed types.

If compilation errors from other crates reference `Kernel`, `KernelState`, `synthesize_prompt`, or `TurnStore`, fix them (likely unused imports or test helpers).

- [ ] **Step 6: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: All gates pass.

- [ ] **Step 7: Commit**

```bash
git add -u crates/
git rm crates/ox-tools/src/turn.rs
git commit -m "$(cat <<'EOF'
refactor: remove Kernel struct, KernelState, synthesize_prompt, TurnStore

The kernel is now a set of free functions (run_turn, synthesize,
accumulate_response, record_turn, execute_tools). All state lives in the
context namespace. The structured log records all activity.
EOF
)"
```

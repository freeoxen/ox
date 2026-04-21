//! Structured log store — append-only record of all agent activity.
//!
//! Every LLM response, tool call, tool result, and meta event is recorded
//! as a [`LogEntry`]. The log is the source of truth; history views are
//! projections over it (follow-up work).

use crate::ContentBlock;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

/// Source metadata for an assistant response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSource {
    pub account: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Why a turn was aborted. See [`LogEntry::TurnAborted`].
///
/// `CrashDuringStream` — the assistant was mid-stream when the process
/// exited (full detection lands alongside `AssistantProgress` in Task 4).
/// `CrashBeforeFirstToken` — a `TurnStart` was written but no progress
/// was ever observed before the crash.
/// `UserCanceledAfterCrash` — the user explicitly aborted a turn during
/// the post-crash reconfirm flow (reserved for Task 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnAbortReason {
    CrashDuringStream,
    CrashBeforeFirstToken,
    UserCanceledAfterCrash,
}

/// Why a tool dispatch was aborted. See [`LogEntry::ToolAborted`].
///
/// `CrashDuringDispatch` — the process exited after `Assistant(tool_use)`
/// (and optionally `ApprovalResolved(allow)`) but before the matching
/// `ToolResult` was written. The tool's side effect may be partial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolAbortReason {
    CrashDuringDispatch,
}

/// A single entry in the structured log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LogEntry {
    #[serde(rename = "user")]
    User {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },

    #[serde(rename = "assistant")]
    Assistant {
        content: Vec<ContentBlock>,
        #[serde(skip_serializing_if = "Option::is_none")]
        source: Option<LogSource>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
        /// Links this response to its CompletionEnd entry.
        #[serde(default)]
        completion_id: u64,
    },

    #[serde(rename = "tool_call")]
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        id: String,
        output: serde_json::Value,
        #[serde(default)]
        is_error: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },

    #[serde(rename = "meta")]
    Meta { data: serde_json::Value },

    #[serde(rename = "turn_start")]
    TurnStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },

    #[serde(rename = "turn_end")]
    TurnEnd {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default)]
        input_tokens: u32,
        #[serde(default)]
        output_tokens: u32,
        #[serde(default)]
        cache_creation_input_tokens: u32,
        #[serde(default)]
        cache_read_input_tokens: u32,
    },

    #[serde(rename = "completion_end")]
    CompletionEnd {
        scope: String,
        model: String,
        completion_id: u64,
        input_tokens: u32,
        output_tokens: u32,
        cache_creation_input_tokens: u32,
        cache_read_input_tokens: u32,
    },

    #[serde(rename = "approval_requested")]
    ApprovalRequested {
        tool_name: String,
        input_preview: String,
        /// `true` when the kernel re-requested approval after a crash
        /// interrupted an in-flight tool dispatch (Task 3c). The UI
        /// uses this flag to surface a Retry/Skip/Cancel modal instead
        /// of the normal allow/deny modal; the plan's durability
        /// contract requires that no tool side effect be re-executed
        /// without an explicit user confirmation, which this flag
        /// gates.
        ///
        /// Elided from the wire when `false` (the overwhelmingly
        /// common case) to keep the happy-path JSON unchanged. Missing
        /// on deserialize → `false`, so ledgers written before Task 3a
        /// continue to deserialize cleanly.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        post_crash_reconfirm: bool,
    },

    #[serde(rename = "approval_resolved")]
    ApprovalResolved {
        tool_name: String,
        decision: ox_types::Decision,
    },

    #[serde(rename = "error")]
    Error {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },

    /// A turn was aborted — written by the mount lifecycle when the log
    /// tail indicates a crash mid-turn, or (future) by `run_turn` when
    /// the user cancels during post-crash reconfirm.
    ///
    /// See [`TurnAbortReason`].
    #[serde(rename = "turn_aborted")]
    TurnAborted { reason: TurnAbortReason },

    /// A tool dispatch was aborted — written by the mount lifecycle
    /// when an `Assistant(tool_use)` has no matching `ToolResult` on
    /// restart. See [`ToolAbortReason`]. The `tool_use_id` pairs this
    /// entry with the original `ToolCall` / `Assistant(tool_use)`.
    #[serde(rename = "tool_aborted")]
    ToolAborted {
        tool_use_id: String,
        reason: ToolAbortReason,
    },
}

/// A durability sink for log entries.
///
/// Implementors synchronously commit an entry to stable storage before
/// returning. When installed via [`SharedLog::with_durability`], the sink is
/// called from inside `append`'s critical section — the entry becomes visible
/// to readers only after `commit` has succeeded.
///
/// The concrete impl in this workspace is `ox_inbox::ledger_writer::LedgerWriterHandle`;
/// keeping the type as a trait object here prevents ox-kernel from depending
/// on ox-inbox (which already depends on ox-kernel).
pub trait Durability: Send + Sync {
    fn commit(&self, entry: &LogEntry) -> Result<(), StoreError>;
}

/// Shared append-only log backing. Both LogStore and HistoryView read
/// from and write to the same underlying `Vec<LogEntry>`.
///
/// Wrapping the Vec and the optional durability handle in a single mutex
/// preserves the invariant **append-order equals observation-order**: the
/// commit completes, then the entry is pushed, before any reader can see it.
#[derive(Clone)]
pub struct SharedLog {
    inner: Arc<Mutex<SharedLogInner>>,
}

struct SharedLogInner {
    entries: Vec<LogEntry>,
    durability: Option<Arc<dyn Durability>>,
}

impl SharedLog {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SharedLogInner {
                entries: Vec::new(),
                durability: None,
            })),
        }
    }

    /// Install a durability sink. When set, `append` commits the entry to the
    /// sink *before* pushing it to the in-memory Vec. Call **after** replay
    /// has populated the log — replay entries must not be re-persisted.
    ///
    /// Returns the previous sink, if any.
    pub fn with_durability(&self, sink: Arc<dyn Durability>) -> Option<Arc<dyn Durability>> {
        let mut inner = self.inner.lock().unwrap();
        inner.durability.replace(sink)
    }

    /// Uninstall the durability sink. Used by crash-harness tests that need
    /// to sever per-append durability (e.g. to compare a pre-durability
    /// snapshot against an ad-hoc replay).
    pub fn clear_durability(&self) -> Option<Arc<dyn Durability>> {
        self.inner.lock().unwrap().durability.take()
    }

    /// Append an entry. When a durability sink is installed, commits to the
    /// sink first; the entry becomes visible to readers only after a
    /// successful commit. On commit failure the entry is **not** pushed and
    /// the error propagates.
    ///
    /// The mutex is held across the commit to preserve
    /// **append-order == observation-order** under concurrent callers. A
    /// two-lock design (separate durability and entries locks) would let ack
    /// arrival order diverge from push order. Readers of `entries()` block
    /// for the commit window (~1–5ms); this is accepted in the plan's design
    /// properties in exchange for a simple ordering invariant.
    pub fn append(&self, entry: LogEntry) -> Result<(), StoreError> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(sink) = inner.durability.as_ref() {
            sink.commit(&entry)?;
        }
        inner.entries.push(entry);
        Ok(())
    }

    pub fn entries(&self) -> Vec<LogEntry> {
        self.inner.lock().unwrap().entries.clone()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn last_n(&self, n: usize) -> Vec<LogEntry> {
        let inner = self.inner.lock().unwrap();
        let start = inner.entries.len().saturating_sub(n);
        inner.entries[start..].to_vec()
    }

    /// Find a tool result by its tool_use_id and return the output.
    pub fn tool_result_output(&self, tool_use_id: &str) -> Option<serde_json::Value> {
        let inner = self.inner.lock().unwrap();
        for entry in inner.entries.iter().rev() {
            if let LogEntry::ToolResult { id, output, .. } = entry {
                if id == tool_use_id {
                    return Some(output.clone());
                }
            }
        }
        None
    }
}

impl Default for SharedLog {
    fn default() -> Self {
        Self::new()
    }
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
    shared: SharedLog,
}

impl LogStore {
    pub fn new() -> Self {
        Self {
            shared: SharedLog::new(),
        }
    }

    pub fn from_shared(shared: SharedLog) -> Self {
        Self { shared }
    }

    pub fn shared(&self) -> &SharedLog {
        &self.shared
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
                let entries = self.shared.entries();
                let json = serde_json::to_value(&entries)
                    .map_err(|e| StoreError::store("LogStore", "read", e.to_string()))?;
                Ok(Some(Record::parsed(structfs_serde_store::json_to_value(
                    json,
                ))))
            }
            "count" => Ok(Some(Record::parsed(Value::Integer(
                self.shared.len() as i64
            )))),
            "last" => {
                if from.len() < 2 {
                    return Err(StoreError::store(
                        "LogStore",
                        "read",
                        "last requires a count: last/{n}",
                    ));
                }
                let n: usize =
                    from.components[1]
                        .parse()
                        .map_err(|e: std::num::ParseIntError| {
                            StoreError::store("LogStore", "read", e.to_string())
                        })?;
                let entries = self.shared.last_n(n);
                let json = serde_json::to_value(&entries)
                    .map_err(|e| StoreError::store("LogStore", "read", e.to_string()))?;
                Ok(Some(Record::parsed(structfs_serde_store::json_to_value(
                    json,
                ))))
            }
            "results" => {
                if from.len() < 2 {
                    return Err(StoreError::store(
                        "LogStore",
                        "read",
                        "results requires tool_use_id: results/{id}",
                    ));
                }
                let tool_use_id = &from.components[1];
                let output = self.shared.tool_result_output(tool_use_id).ok_or_else(|| {
                    StoreError::store(
                        "LogStore",
                        "read",
                        format!("no tool result with id: {tool_use_id}"),
                    )
                })?;
                let full = match &output {
                    serde_json::Value::String(s) => s.clone(),
                    other => serde_json::to_string(other).unwrap_or_default(),
                };

                if from.len() == 2 {
                    return Ok(Some(Record::parsed(Value::String(full))));
                }

                let sub = from.components[2].as_str();
                match sub {
                    "line_count" => {
                        let count = full.lines().count() as i64;
                        Ok(Some(Record::parsed(Value::Integer(count))))
                    }
                    "lines" => {
                        if from.len() < 5 {
                            return Err(StoreError::store(
                                "LogStore",
                                "read",
                                "lines requires offset and limit: results/{id}/lines/{offset}/{limit}",
                            ));
                        }
                        let offset: usize =
                            from.components[3]
                                .parse()
                                .map_err(|e: std::num::ParseIntError| {
                                    StoreError::store("LogStore", "read", e.to_string())
                                })?;
                        let limit: usize =
                            from.components[4]
                                .parse()
                                .map_err(|e: std::num::ParseIntError| {
                                    StoreError::store("LogStore", "read", e.to_string())
                                })?;
                        let lines: Vec<&str> = full.lines().collect();
                        let total = lines.len();
                        let start = offset.min(total);
                        let end = (start + limit).min(total);
                        let sliced = format!(
                            "[lines {}-{} of {}]\n{}",
                            start + 1,
                            end,
                            total,
                            lines[start..end].join("\n"),
                        );
                        Ok(Some(Record::parsed(Value::String(sliced))))
                    }
                    _ => Err(StoreError::store(
                        "LogStore",
                        "read",
                        format!("unknown results sub-path: {sub}"),
                    )),
                }
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
                self.shared.append(entry)?;
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
    use structfs_core_store::{Path, Reader, Record, Writer, path};

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
        let record = log.read(&Path::parse("last/2").unwrap()).unwrap().unwrap();
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

    #[test]
    fn tool_result_output_found() {
        let log = SharedLog::new();
        log.append(LogEntry::ToolResult {
            id: "tc_abc".into(),
            output: serde_json::Value::String("hello world".into()),
            is_error: false,
            scope: None,
        })
        .unwrap();
        let result = log.tool_result_output("tc_abc");
        assert_eq!(
            result,
            Some(serde_json::Value::String("hello world".into()))
        );
    }

    #[test]
    fn tool_result_output_not_found() {
        let log = SharedLog::new();
        log.append(LogEntry::User {
            content: "hi".into(),
            scope: None,
        })
        .unwrap();
        assert_eq!(log.tool_result_output("tc_missing"), None);
    }

    #[test]
    fn tool_result_output_returns_last_match() {
        let log = SharedLog::new();
        log.append(LogEntry::ToolResult {
            id: "tc_dup".into(),
            output: serde_json::Value::String("first".into()),
            is_error: false,
            scope: None,
        })
        .unwrap();
        log.append(LogEntry::ToolResult {
            id: "tc_dup".into(),
            output: serde_json::Value::String("second".into()),
            is_error: false,
            scope: None,
        })
        .unwrap();
        assert_eq!(
            log.tool_result_output("tc_dup"),
            Some(serde_json::Value::String("second".into()))
        );
    }

    #[test]
    fn read_result_by_id() {
        let mut log = LogStore::new();
        log.write(
            &path!("append"),
            Record::parsed(structfs_serde_store::json_to_value(serde_json::json!({
                "type": "tool_result",
                "id": "tc_42",
                "output": "the full output\nline two\nline three",
                "is_error": false
            }))),
        )
        .unwrap();
        let record = log
            .read(&Path::parse("results/tc_42").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            record.as_value().unwrap(),
            &Value::String("the full output\nline two\nline three".into())
        );
    }

    #[test]
    fn read_result_not_found() {
        let mut log = LogStore::new();
        let result = log.read(&Path::parse("results/tc_missing").unwrap());
        assert!(result.is_err());
    }

    #[test]
    fn read_result_line_count() {
        let mut log = LogStore::new();
        log.write(
            &path!("append"),
            Record::parsed(structfs_serde_store::json_to_value(serde_json::json!({
                "type": "tool_result",
                "id": "tc_lc",
                "output": "line1\nline2\nline3",
                "is_error": false
            }))),
        )
        .unwrap();
        let record = log
            .read(&Path::parse("results/tc_lc/line_count").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(record.as_value().unwrap(), &Value::Integer(3));
    }

    #[test]
    fn turn_start_entry_round_trip() {
        let mut log = LogStore::new();
        log.write(
            &path!("append"),
            Record::parsed(structfs_serde_store::json_to_value(
                serde_json::json!({"type": "turn_start", "scope": "root"}),
            )),
        )
        .unwrap();
        let record = log.read(&path!("entries")).unwrap().unwrap();
        let json = structfs_serde_store::value_to_json(record.as_value().unwrap().clone());
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "turn_start");
        assert_eq!(arr[0]["scope"], "root");
    }

    #[test]
    fn turn_end_entry_round_trip() {
        let mut log = LogStore::new();
        log.write(
            &path!("append"),
            Record::parsed(structfs_serde_store::json_to_value(
                serde_json::json!({"type": "turn_end", "scope": "root", "input_tokens": 100, "output_tokens": 50}),
            )),
        )
        .unwrap();
        let record = log.read(&path!("entries")).unwrap().unwrap();
        let json = structfs_serde_store::value_to_json(record.as_value().unwrap().clone());
        let entry = &json.as_array().unwrap()[0];
        assert_eq!(entry["type"], "turn_end");
        assert_eq!(entry["input_tokens"], 100);
        assert_eq!(entry["output_tokens"], 50);
    }

    #[test]
    fn approval_entries_round_trip() {
        let mut log = LogStore::new();
        log.write(
            &path!("append"),
            Record::parsed(structfs_serde_store::json_to_value(
                serde_json::json!({"type": "approval_requested", "tool_name": "bash", "input_preview": "ls -la"}),
            )),
        )
        .unwrap();
        log.write(
            &path!("append"),
            Record::parsed(structfs_serde_store::json_to_value(
                serde_json::json!({"type": "approval_resolved", "tool_name": "bash", "decision": "allow_once"}),
            )),
        )
        .unwrap();
        let record = log.read(&path!("entries")).unwrap().unwrap();
        let json = structfs_serde_store::value_to_json(record.as_value().unwrap().clone());
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["type"], "approval_requested");
        assert_eq!(arr[0]["tool_name"], "bash");
        assert_eq!(arr[1]["type"], "approval_resolved");
        assert_eq!(arr[1]["decision"], "allow_once");
    }

    #[test]
    fn approval_requested_post_crash_reconfirm_elided_when_false() {
        // The flag must not appear on the wire in the happy path; this
        // keeps existing ledgers byte-identical and the audit stream
        // quiet when no crash has happened.
        let entry = LogEntry::ApprovalRequested {
            tool_name: "bash".into(),
            input_preview: "ls".into(),
            post_crash_reconfirm: false,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("post_crash_reconfirm").is_none(), "got {json}");
        assert_eq!(json["type"], "approval_requested");
    }

    #[test]
    fn approval_requested_post_crash_reconfirm_emitted_when_true() {
        let entry = LogEntry::ApprovalRequested {
            tool_name: "bash".into(),
            input_preview: "ls".into(),
            post_crash_reconfirm: true,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["post_crash_reconfirm"], true);
    }

    #[test]
    fn approval_requested_defaults_false_on_legacy_payload() {
        // Ledgers written pre-Task-3a did not carry the flag. Missing
        // on the wire must deserialize to `false`.
        let json = serde_json::json!({
            "type": "approval_requested",
            "tool_name": "bash",
            "input_preview": "ls -la",
        });
        let entry: LogEntry = serde_json::from_value(json).unwrap();
        match entry {
            LogEntry::ApprovalRequested {
                post_crash_reconfirm,
                ..
            } => {
                assert!(!post_crash_reconfirm);
            }
            other => panic!("expected ApprovalRequested, got {other:?}"),
        }
    }

    #[test]
    fn approval_requested_round_trip_with_flag_set() {
        let entry = LogEntry::ApprovalRequested {
            tool_name: "bash".into(),
            input_preview: "rm -rf /".into(),
            post_crash_reconfirm: true,
        };
        let json = serde_json::to_value(&entry).unwrap();
        let back: LogEntry = serde_json::from_value(json).unwrap();
        match back {
            LogEntry::ApprovalRequested {
                tool_name,
                input_preview,
                post_crash_reconfirm,
            } => {
                assert_eq!(tool_name, "bash");
                assert_eq!(input_preview, "rm -rf /");
                assert!(post_crash_reconfirm);
            }
            other => panic!("expected ApprovalRequested, got {other:?}"),
        }
    }

    #[test]
    fn error_entry_round_trip() {
        let mut log = LogStore::new();
        log.write(
            &path!("append"),
            Record::parsed(structfs_serde_store::json_to_value(
                serde_json::json!({"type": "error", "message": "connection failed", "scope": "root"}),
            )),
        )
        .unwrap();
        let record = log.read(&path!("entries")).unwrap().unwrap();
        let json = structfs_serde_store::value_to_json(record.as_value().unwrap().clone());
        let entry = &json.as_array().unwrap()[0];
        assert_eq!(entry["type"], "error");
        assert_eq!(entry["message"], "connection failed");
    }

    #[test]
    fn turn_aborted_entry_round_trip() {
        let mut log = LogStore::new();
        log.write(
            &path!("append"),
            Record::parsed(structfs_serde_store::json_to_value(
                serde_json::json!({"type": "turn_aborted", "reason": "crash_during_stream"}),
            )),
        )
        .unwrap();
        log.write(
            &path!("append"),
            Record::parsed(structfs_serde_store::json_to_value(
                serde_json::json!({"type": "turn_aborted", "reason": "crash_before_first_token"}),
            )),
        )
        .unwrap();
        log.write(
            &path!("append"),
            Record::parsed(structfs_serde_store::json_to_value(
                serde_json::json!({"type": "turn_aborted", "reason": "user_canceled_after_crash"}),
            )),
        )
        .unwrap();
        let record = log.read(&path!("entries")).unwrap().unwrap();
        let json = structfs_serde_store::value_to_json(record.as_value().unwrap().clone());
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["type"], "turn_aborted");
        assert_eq!(arr[0]["reason"], "crash_during_stream");
        assert_eq!(arr[1]["reason"], "crash_before_first_token");
        assert_eq!(arr[2]["reason"], "user_canceled_after_crash");
    }

    #[test]
    fn tool_aborted_entry_round_trip() {
        let mut log = LogStore::new();
        log.write(
            &path!("append"),
            Record::parsed(structfs_serde_store::json_to_value(serde_json::json!({
                "type": "tool_aborted",
                "tool_use_id": "tc_xyz",
                "reason": "crash_during_dispatch",
            }))),
        )
        .unwrap();
        let record = log.read(&path!("entries")).unwrap().unwrap();
        let json = structfs_serde_store::value_to_json(record.as_value().unwrap().clone());
        let entry = &json.as_array().unwrap()[0];
        assert_eq!(entry["type"], "tool_aborted");
        assert_eq!(entry["tool_use_id"], "tc_xyz");
        assert_eq!(entry["reason"], "crash_during_dispatch");
    }

    #[test]
    fn read_result_lines_slice() {
        let mut log = LogStore::new();
        let output = (0..10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        log.write(
            &path!("append"),
            Record::parsed(structfs_serde_store::json_to_value(serde_json::json!({
                "type": "tool_result",
                "id": "tc_sl",
                "output": output,
                "is_error": false
            }))),
        )
        .unwrap();
        let record = log
            .read(&Path::parse("results/tc_sl/lines/2/3").unwrap())
            .unwrap()
            .unwrap();
        let val = match record.as_value().unwrap() {
            Value::String(s) => s.clone(),
            _ => panic!("expected string"),
        };
        assert!(val.starts_with("[lines 3-5 of 10]"));
        assert!(val.contains("line 2"));
        assert!(val.contains("line 3"));
        assert!(val.contains("line 4"));
        assert!(!val.contains("line 5"));
    }
}

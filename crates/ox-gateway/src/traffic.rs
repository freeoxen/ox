//! Traffic log — full request/response capture, opt-in.
//!
//! Mounted at `gateway/traffic` when `OX_GATEWAY_TRAFFIC_LOG` is set. Two
//! sinks fed from one `append` write path:
//!
//! - **JSONL** (`traffic.jsonl`): every record verbatim — `kind:
//!   "completion"` records from the dispatch task (decoded request,
//!   upstream body, all stream events, terminal status, usage) and
//!   `kind: "http"` access records from the router middleware.
//! - **Ledger** (`~/.ox/threads/t_gateway_YYYYMMDD/`): completion records
//!   re-emitted as ox conversation-ledger entries — the same hash-chained
//!   `user`/`turn_start`/`completion_end`/`assistant`/`turn_end` shape
//!   ox-cli writes, in a daily thread dir with `context.json` +
//!   `view.json`, so gateway conversations show up in the ox history UI.
//!
//! Everything here contains complete prompt and completion text; the
//! feature is off unless explicitly enabled.

use ox_inbox::ledger::{append_entry, read_ledger_with_repair, LedgerEntry};
use ox_inbox::thread_dir::{self, ContextFile};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

pub struct TrafficLogStore {
    jsonl: Box<dyn ox_store_util::StoreBacking + Send + Sync>,
    ledger: Option<LedgerSink>,
}

impl TrafficLogStore {
    pub fn new(
        jsonl: Box<dyn ox_store_util::StoreBacking + Send + Sync>,
        threads_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            jsonl,
            ledger: threads_dir.map(LedgerSink::new),
        }
    }
}

impl Reader for TrafficLogStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        if from.is_empty() {
            let value = self.jsonl.load()?.unwrap_or(Value::Array(vec![]));
            return Ok(Some(Record::parsed(value)));
        }
        Ok(None)
    }
}

impl Writer for TrafficLogStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        if to.is_empty() || to[0].as_str() != "append" {
            return Err(StoreError::store(
                "traffic",
                "write",
                "only 'append' path supported",
            ));
        }
        let value = data
            .as_value()
            .ok_or_else(|| StoreError::store("traffic", "write", "expected parsed record"))?;
        self.jsonl.append(value)?;

        if let Some(sink) = &self.ledger {
            let json = structfs_serde_store::value_to_json(value.clone());
            if json.get("kind").and_then(|k| k.as_str()) == Some("completion") {
                // Ledger emission is best-effort: a bad thread dir must not
                // fail the completion path that triggered the log write.
                if let Err(e) = sink.append_completion(&json) {
                    tracing::warn!(error = %e, "traffic ledger append failed");
                }
            }
        }
        Ok(to.clone())
    }
}

/// Daily gateway thread in ox's conversation-ledger format.
struct LedgerSink {
    threads_dir: PathBuf,
    /// (day-stamp, ledger path, last entry, next completion_id).
    state: Mutex<Option<(String, PathBuf, Option<LedgerEntry>, u64)>>,
}

impl LedgerSink {
    fn new(threads_dir: PathBuf) -> Self {
        Self {
            threads_dir,
            state: Mutex::new(None),
        }
    }

    fn append_completion(&self, record: &serde_json::Value) -> Result<(), String> {
        let now_s = now_ms() / 1000;
        let day = day_stamp(now_s);
        let mut guard = self.state.lock().map_err(|_| "sink lock poisoned".to_string())?;

        // Open (or roll to) today's thread.
        if guard.as_ref().map(|(d, ..)| d != &day).unwrap_or(true) {
            let thread_id = format!("t_gateway_{day}");
            let dir = self.threads_dir.join(&thread_id);
            let ledger_path = dir.join("ledger.jsonl");

            if thread_dir::read_context(&dir).map_err(|e| e)?.is_none() {
                thread_dir::write_context(
                    &dir,
                    &ContextFile {
                        version: 1,
                        thread_id: thread_id.clone(),
                        title: format!("Gateway traffic {day}"),
                        labels: vec!["gateway".to_string()],
                        created_at: now_s as i64,
                        updated_at: now_s as i64,
                        stores: BTreeMap::new(),
                    },
                )?;
                thread_dir::write_default_view(&dir)?;
            }

            // Continue an existing chain across daemon restarts.
            let (last, turns) = match read_ledger_with_repair(&ledger_path) {
                Ok(outcome) => {
                    let turns = outcome
                        .entries
                        .iter()
                        .filter(|e| e.msg.get("type").and_then(|t| t.as_str()) == Some("turn_start"))
                        .count() as u64;
                    (outcome.entries.into_iter().last(), turns)
                }
                Err(_) => (None, 0),
            };
            *guard = Some((day.clone(), ledger_path, last, turns));
        }

        let (_, ledger_path, last, completion_id) = guard.as_mut().expect("state set above");
        let cid = *completion_id;
        *completion_id += 1;

        let msgs = completion_msgs(record, cid);
        for msg in msgs {
            let entry = append_entry(ledger_path, &msg, last.as_ref())?;
            *last = Some(entry);
        }

        // Touch updated_at so the thread sorts correctly in the inbox.
        if let Some(dir) = ledger_path.parent() {
            if let Ok(Some(mut ctx)) = thread_dir::read_context(dir) {
                ctx.updated_at = now_s as i64;
                let _ = thread_dir::write_context(dir, &ctx);
            }
        }
        Ok(())
    }
}

/// Project one completion traffic record into ox ledger messages:
/// `user` → `turn_start` → `completion_end` → `assistant` → `turn_end`.
fn completion_msgs(record: &serde_json::Value, completion_id: u64) -> Vec<serde_json::Value> {
    let request = &record["request"];
    let events = record["events"].as_array().cloned().unwrap_or_default();
    let usage = &record["usage"];
    // CompletionStatus is internally tagged: {"state": "...", "model_id": ...}.
    let model = record["status"]["model_id"].as_str().unwrap_or("").to_string();

    let mut msgs = Vec::new();
    msgs.push(serde_json::json!({
        "type": "user",
        "content": last_user_content(request),
    }));
    msgs.push(serde_json::json!({ "type": "turn_start", "scope": "root" }));

    let usage_fields = |m: &mut serde_json::Map<String, serde_json::Value>| {
        for key in [
            "input_tokens",
            "output_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        ] {
            m.insert(key.into(), usage.get(key).cloned().unwrap_or(serde_json::json!(0)));
        }
        m.insert("model".into(), serde_json::json!(model));
    };

    let mut completion_end = serde_json::Map::new();
    completion_end.insert("type".into(), serde_json::json!("completion_end"));
    completion_end.insert("scope".into(), serde_json::json!("root"));
    completion_end.insert("completion_id".into(), serde_json::json!(completion_id));
    usage_fields(&mut completion_end);
    msgs.push(serde_json::Value::Object(completion_end));

    msgs.push(serde_json::json!({
        "type": "assistant",
        "scope": "root",
        "completion_id": completion_id,
        "content": assistant_content(&events, &record["status"]),
    }));

    let mut turn_end = serde_json::Map::new();
    turn_end.insert("type".into(), serde_json::json!("turn_end"));
    turn_end.insert("scope".into(), serde_json::json!("root"));
    usage_fields(&mut turn_end);
    msgs.push(serde_json::Value::Object(turn_end));

    msgs
}

/// The newest user message's text. Prior turns repeat in every stateless
/// API call, so per-request logging keeps only the new content — the
/// thread accumulates the conversation the same way ox-cli's does.
fn last_user_content(request: &serde_json::Value) -> String {
    let Some(messages) = request.get("messages").and_then(|m| m.as_array()) else {
        return String::new();
    };
    let Some(last_user) = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
    else {
        return String::new();
    };
    match last_user.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.map(|v| v.to_string()).unwrap_or_default(),
    }
}

/// Assistant content blocks reconstructed from the event stream — text and
/// tool_use blocks in order. A failed request renders its reason as a text
/// block so every renderer shows it.
fn assistant_content(
    events: &[serde_json::Value],
    status: &serde_json::Value,
) -> Vec<serde_json::Value> {
    let mut blocks: Vec<serde_json::Value> = Vec::new();
    let mut text = String::new();
    let mut tool: Option<(String, String, String)> = None;

    let flush_text = |blocks: &mut Vec<serde_json::Value>, text: &mut String| {
        if !text.is_empty() {
            blocks.push(serde_json::json!({ "type": "text", "text": std::mem::take(text) }));
        }
    };
    let flush_tool = |blocks: &mut Vec<serde_json::Value>, tool: &mut Option<(String, String, String)>| {
        if let Some((id, name, input)) = tool.take() {
            let input = serde_json::from_str::<serde_json::Value>(&input)
                .unwrap_or(serde_json::json!({}));
            blocks.push(serde_json::json!({ "type": "tool_use", "id": id, "name": name, "input": input }));
        }
    };

    for ev in events {
        match ev.get("type").and_then(|t| t.as_str()) {
            Some("text_delta") => {
                flush_tool(&mut blocks, &mut tool);
                if let Some(t) = ev.get("text").and_then(|t| t.as_str()) {
                    text.push_str(t);
                }
            }
            Some("tool_use_start") => {
                flush_text(&mut blocks, &mut text);
                flush_tool(&mut blocks, &mut tool);
                tool = Some((
                    ev.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    ev.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    String::new(),
                ));
            }
            Some("tool_use_input_delta") => {
                if let Some((_, _, input)) = tool.as_mut() {
                    input.push_str(ev.get("delta").and_then(|v| v.as_str()).unwrap_or(""));
                }
            }
            _ => {}
        }
    }
    flush_text(&mut blocks, &mut text);
    flush_tool(&mut blocks, &mut tool);

    if status.get("state").and_then(|s| s.as_str()) == Some("failed") {
        let reason = status.get("reason").and_then(|r| r.as_str()).unwrap_or("unknown");
        blocks.push(serde_json::json!({
            "type": "text",
            "text": format!("[gateway error] {reason}"),
        }));
    }
    blocks
}

/// Axum middleware appending one `kind: "http"` access record per request
/// (method, path, status, duration — no bodies; completion bodies are
/// captured by the dispatch-side records). The dashboard's own polling
/// endpoints are excluded so a 5-second refresh doesn't flood the log.
pub async fn http_log_middleware(
    axum::extract::State(client): axum::extract::State<ox_broker::ClientHandle>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let started = std::time::Instant::now();
    let resp = next.run(req).await;
    if path != "/stats" && path != "/dashboard" {
        let record = serde_json::json!({
            "kind": "http",
            "ts_ms": now_ms(),
            "method": method,
            "path": path,
            "status": resp.status().as_u16(),
            "duration_ms": started.elapsed().as_millis() as u64,
        });
        let value = structfs_serde_store::json_to_value(record);
        let client = client.clone();
        tokio::spawn(async move {
            let _ = client
                .write(
                    &ox_path::oxpath!("gateway", "traffic", "append"),
                    Record::parsed(value),
                )
                .await;
        });
    }
    resp
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn day_stamp(now_s: u64) -> String {
    // Days since epoch → YYYYMMDD without a chrono dependency.
    let days = now_s / 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}{m:02}{d:02}")
}

/// Howard Hinnant's days-to-civil algorithm (public domain).
fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

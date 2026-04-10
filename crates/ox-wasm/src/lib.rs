//! Ox agent as a Wasm module — drives the kernel loop via host StructFS imports.
//!
//! This crate compiles to `wasm32-unknown-unknown` and produces a `.wasm` file.
//! The guest exports a single `run()` entry point. All I/O flows through three
//! host-provided StructFS functions (`store_read`, `store_write`, `store_result`)
//! imported from the `"ox"` module namespace.

use ox_kernel::{AgentEvent, Kernel, StreamEvent, ToolResult};
use structfs_core_store::{Error, Path, Reader, Record, Value, Writer, path};

mod wasm_subscriber;

// ---------------------------------------------------------------------------
// Host function imports (from the "ox" Wasm import module)
// ---------------------------------------------------------------------------

#[link(wasm_import_module = "ox")]
unsafe extern "C" {
    fn store_read(path_ptr: i32, path_len: i32) -> i32;
    fn store_write(path_ptr: i32, path_len: i32, data_ptr: i32, data_len: i32) -> i32;
    fn store_result(buf_ptr: i32);
}

// ---------------------------------------------------------------------------
// Safe wrappers around host imports
// ---------------------------------------------------------------------------

fn host_read(path: &str) -> Result<Option<String>, String> {
    let n = unsafe { store_read(path.as_ptr() as i32, path.len() as i32) };
    if n > 0 {
        let mut buf = vec![0u8; n as usize];
        unsafe { store_result(buf.as_mut_ptr() as i32) };
        String::from_utf8(buf).map(Some).map_err(|e| e.to_string())
    } else if n == 0 {
        Ok(None)
    } else {
        let mut buf = vec![0u8; (-n) as usize];
        unsafe { store_result(buf.as_mut_ptr() as i32) };
        Err(String::from_utf8(buf).unwrap_or_else(|_| "unknown error".into()))
    }
}

fn host_write(path: &str, data: &str) -> Result<String, String> {
    let n = unsafe {
        store_write(
            path.as_ptr() as i32,
            path.len() as i32,
            data.as_ptr() as i32,
            data.len() as i32,
        )
    };
    if n > 0 {
        let mut buf = vec![0u8; n as usize];
        unsafe { store_result(buf.as_mut_ptr() as i32) };
        String::from_utf8(buf).map_err(|e| e.to_string())
    } else {
        let err_len = if n == 0 { 0 } else { (-n) as usize };
        if err_len > 0 {
            let mut buf = vec![0u8; err_len];
            unsafe { store_result(buf.as_mut_ptr() as i32) };
            Err(String::from_utf8(buf).unwrap_or_else(|_| "unknown error".into()))
        } else {
            Err("write failed with unknown error".into())
        }
    }
}

// ---------------------------------------------------------------------------
// HostBridge — implements StructFS Reader + Writer via host calls
// ---------------------------------------------------------------------------

struct HostBridge;

// Safety: wasm32-unknown-unknown is single-threaded; Send+Sync are trivially satisfied.
unsafe impl Send for HostBridge {}
unsafe impl Sync for HostBridge {}

impl Reader for HostBridge {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, Error> {
        let path_str = from.to_string();
        match host_read(&path_str) {
            Ok(Some(json)) => {
                let json_value: serde_json::Value = serde_json::from_str(&json)
                    .map_err(|e| Error::store("HostBridge", "read", e.to_string()))?;
                Ok(Some(Record::parsed(structfs_serde_store::json_to_value(
                    json_value,
                ))))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(Error::store("HostBridge", "read", e)),
        }
    }
}

impl Writer for HostBridge {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, Error> {
        let path_str = to.to_string();
        let value = data
            .as_value()
            .ok_or_else(|| Error::store("HostBridge", "write", "expected parsed record"))?;
        let json = structfs_serde_store::value_to_json(value.clone());
        let json_str = serde_json::to_string(&json)
            .map_err(|e| Error::store("HostBridge", "write", e.to_string()))?;

        match host_write(&path_str, &json_str) {
            Ok(canonical) => Path::parse(&canonical)
                .map_err(|e| Error::store("HostBridge", "write", e.to_string())),
            Err(e) => Err(Error::store("HostBridge", "write", e)),
        }
    }
}

// ---------------------------------------------------------------------------
// StreamEvent deserialization (manual — no serde derives on StreamEvent)
// ---------------------------------------------------------------------------

fn json_to_stream_event(json: &serde_json::Value) -> Result<StreamEvent, String> {
    let obj = json
        .as_object()
        .ok_or_else(|| "expected JSON object for StreamEvent".to_string())?;
    let event_type = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'type' field in StreamEvent".to_string())?;

    match event_type {
        "text_delta" => {
            let text = obj
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing 'text' in text_delta event".to_string())?;
            Ok(StreamEvent::TextDelta(text.to_string()))
        }
        "tool_use_start" => {
            let id = obj
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing 'id' in tool_use_start event".to_string())?;
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing 'name' in tool_use_start event".to_string())?;
            Ok(StreamEvent::ToolUseStart {
                id: id.to_string(),
                name: name.to_string(),
            })
        }
        "tool_use_input_delta" => {
            let delta = obj
                .get("delta")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing 'delta' in tool_use_input_delta event".to_string())?;
            Ok(StreamEvent::ToolUseInputDelta(delta.to_string()))
        }
        "message_stop" => Ok(StreamEvent::MessageStop),
        "error" => {
            let message = obj
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            Ok(StreamEvent::Error(message.to_string()))
        }
        other => Err(format!("unknown StreamEvent type: {other}")),
    }
}

fn deserialize_events(record: Record) -> Result<Vec<StreamEvent>, String> {
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
// Exported entry point
// ---------------------------------------------------------------------------

/// Guest entry point. Returns 0 on success, nonzero on error.
#[unsafe(no_mangle)]
pub extern "C" fn run() -> i32 {
    // Install the wasm subscriber so tracing calls from ox-kernel etc.
    // route through the host bridge.
    let _ = tracing::subscriber::set_global_default(wasm_subscriber::WasmSubscriber);

    match agent_main() {
        Ok(()) => 0,
        Err(e) => {
            // Stash the error message where the host can read it back.
            // Must be valid JSON since host_write parses data as JSON.
            let json = serde_json::to_string(&e).unwrap_or_else(|_| "\"unknown error\"".into());
            let _ = host_write("tool_results/__error", &json);
            1
        }
    }
}

fn agent_main() -> Result<(), String> {
    let mut bridge = HostBridge;

    // Read model identifier from the namespace.
    let model = match bridge.read(&path!("gate/defaults/model")) {
        Ok(Some(record)) => match record.as_value() {
            Some(Value::String(m)) => m.clone(),
            _ => "unknown".to_string(),
        },
        _ => "unknown".to_string(),
    };

    // Read default account for completion routing.
    let default_account = match bridge.read(&path!("gate/defaults/account")) {
        Ok(Some(record)) => match record.as_value() {
            Some(Value::String(s)) => s.clone(),
            _ => "anthropic".to_string(),
        },
        _ => "anthropic".to_string(),
    };

    let mut kernel = Kernel::new(model);

    loop {
        // Phase 1: Kernel reads prompt and prepares CompletionRequest.
        let request = kernel.initiate_completion(&mut bridge)?;

        // Serialize request and write to tools/completions/complete/{account}.
        let request_json = serde_json::to_value(&request).map_err(|e| e.to_string())?;
        let request_value = structfs_serde_store::json_to_value(request_json);
        let complete_path = Path::parse(&format!("tools/completions/complete/{default_account}"))
            .map_err(|e| e.to_string())?;
        bridge
            .write(&complete_path, Record::parsed(request_value))
            .map_err(|e| e.to_string())?;

        // Read response events from tools/completions/complete/{account}/response.
        let response_path = Path::parse(&format!(
            "tools/completions/complete/{default_account}/response"
        ))
        .map_err(|e| e.to_string())?;
        let response = bridge
            .read(&response_path)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "no completion response".to_string())?;

        let events = deserialize_events(response)?;

        // Phase 2: Kernel accumulates events into content blocks.
        let mut noop_emit = |_event: AgentEvent| {};
        let content = kernel.consume_events(events, &mut noop_emit)?;

        // Phase 3: Write assistant message and extract tool calls.
        let tool_calls = kernel.complete_turn(&mut bridge, &content)?;

        if tool_calls.is_empty() {
            return Ok(());
        }

        // Execute tools via ToolStore paths. Denied tools produce an error
        // result, not a fatal abort — the conversation must continue.
        let mut results = Vec::new();
        for tc in &tool_calls {
            // Emit tool_call_start event.
            let start_event = serde_json::json!({"type": "tool_call_start", "name": tc.name});
            let start_value = structfs_serde_store::json_to_value(start_event);
            bridge
                .write(&path!("events/emit"), Record::parsed(start_value))
                .ok();

            // Write tool input to tools/{wire_name}.
            let input_value = structfs_serde_store::json_to_value(tc.input.clone());
            let tool_path =
                Path::parse(&format!("tools/{}", tc.name)).map_err(|e| e.to_string())?;

            let result_str = match bridge.write(&tool_path, Record::parsed(input_value)) {
                Ok(_) => {
                    // Read result from tools/{wire_name}/result.
                    let result_path = Path::parse(&format!("tools/{}/result", tc.name))
                        .map_err(|e| e.to_string())?;
                    match bridge.read(&result_path) {
                        Ok(Some(record)) => {
                            let val = record.as_value().cloned().unwrap_or(Value::Null);
                            let json = structfs_serde_store::value_to_json(val);
                            serde_json::to_string(&json).unwrap_or_default()
                        }
                        Ok(None) => format!("error: no result for tool {}", tc.name),
                        Err(e) => format!("error: {e}"),
                    }
                }
                Err(e) => {
                    // Tool denied or failed — use the error as the result.
                    e.to_string()
                }
            };

            // Emit tool_call_result event.
            let end_event = serde_json::json!({"type": "tool_call_result", "name": tc.name, "result": &result_str});
            let end_value = structfs_serde_store::json_to_value(end_event);
            bridge
                .write(&path!("events/emit"), Record::parsed(end_value))
                .ok();

            results.push(ToolResult {
                tool_use_id: tc.id.clone(),
                content: serde_json::Value::String(result_str),
            });
        }

        // Write tool results to history so the next turn sees them.
        let results_json = ox_kernel::serialize_tool_results(&results);
        let results_value = structfs_serde_store::json_to_value(results_json);
        bridge
            .write(&path!("history/append"), Record::parsed(results_value))
            .map_err(|e| e.to_string())?;
    }
}

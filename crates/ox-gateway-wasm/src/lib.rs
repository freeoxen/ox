//! Gateway codec Block — the sans-IO codec core as a Wasm guest.
//!
//! Same ABI as agent.wasm: `run()` exported, all I/O through the three
//! host StructFS imports. The Block's contract is one job per run:
//!
//!   read  codec/job     — {"op": ..., ...op-specific fields}
//!   write codec/result  — op-specific output, or {"error": "..."}
//!
//! Ops:
//!   decode_request   {dialect, body}            → CompletionRequest JSON
//!   encode_response  {dialect, events, meta}    → response body JSON
//!   encode_stream    {dialect, events, meta}    → {"frames": [..], "finish": [..]}
//!   translate_request{dialect, request}         → upstream body JSON
//!
//! `encode_stream` runs the full stateful SseEncoder over the event list —
//! the whole per-request encoder lifecycle happens inside the Block, so
//! ordering state never crosses the ABI.

use ox_codec::ResponseMeta;
use ox_kernel::{CompletionRequest, StreamEvent};

#[link(wasm_import_module = "ox")]
unsafe extern "C" {
    fn store_read(path_ptr: i32, path_len: i32) -> i32;
    fn store_write(path_ptr: i32, path_len: i32, data_ptr: i32, data_len: i32) -> i32;
    fn store_result(buf_ptr: i32);
}

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

fn host_write(path: &str, data: &str) -> Result<(), String> {
    let n = unsafe {
        store_write(
            path.as_ptr() as i32,
            path.len() as i32,
            data.as_ptr() as i32,
            data.len() as i32,
        )
    };
    if n >= 0 {
        if n > 0 {
            let mut buf = vec![0u8; n as usize];
            unsafe { store_result(buf.as_mut_ptr() as i32) };
        }
        Ok(())
    } else {
        let mut buf = vec![0u8; (-n) as usize];
        unsafe { store_result(buf.as_mut_ptr() as i32) };
        Err(String::from_utf8(buf).unwrap_or_else(|_| "unknown error".into()))
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn run() -> i32 {
    let outcome = match execute() {
        Ok(result) => host_write("codec/result", &result.to_string()),
        Err(e) => host_write(
            "codec/result",
            &serde_json::json!({ "error": e }).to_string(),
        ),
    };
    match outcome {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

fn execute() -> Result<serde_json::Value, String> {
    let job_str = host_read("codec/job")?
        .ok_or_else(|| "no job at codec/job".to_string())?;
    let job: serde_json::Value =
        serde_json::from_str(&job_str).map_err(|e| format!("bad job JSON: {e}"))?;
    let op = job["op"].as_str().ok_or("job missing op")?;
    let dialect = job["dialect"].as_str().unwrap_or("anthropic");

    match op {
        "decode_request" => {
            let body = job.get("body").ok_or("decode_request needs body")?;
            let req = match dialect {
                "openai" => ox_codec::openai::decode_request(body),
                _ => ox_codec::anthropic::decode_request(body),
            }
            .map_err(|e| e.to_string())?;
            serde_json::to_value(&req).map_err(|e| e.to_string())
        }
        "encode_response" => {
            let (events, meta) = events_and_meta(&job)?;
            Ok(match dialect {
                "openai" => ox_codec::openai::encode_response(&events, &meta),
                _ => ox_codec::anthropic::encode_response(&events, &meta),
            })
        }
        "encode_stream" => {
            let (events, meta) = events_and_meta(&job)?;
            let mut enc = ox_codec::SseEncoder::new(dialect, meta);
            let mut frames: Vec<String> = Vec::new();
            for ev in &events {
                frames.extend(enc.encode_sse(ev));
            }
            let finish = enc.finish();
            Ok(serde_json::json!({ "frames": frames, "finish": finish }))
        }
        "translate_request" => {
            let request: CompletionRequest =
                serde_json::from_value(job["request"].clone())
                    .map_err(|e| format!("bad request: {e}"))?;
            Ok(match dialect {
                "openai" => ox_codec::openai::translate_request(&request),
                _ => ox_codec::anthropic::translate_request(&request),
            })
        }
        other => Err(format!("unknown op: {other}")),
    }
}

fn events_and_meta(job: &serde_json::Value) -> Result<(Vec<StreamEvent>, ResponseMeta), String> {
    let events: Vec<StreamEvent> = serde_json::from_value(job["events"].clone())
        .map_err(|e| format!("bad events: {e}"))?;
    let meta = ResponseMeta {
        id: job["meta"]["id"].as_str().unwrap_or("").to_string(),
        model: job["meta"]["model"].as_str().unwrap_or("").to_string(),
        created: job["meta"]["created"].as_u64().unwrap_or(0),
    };
    Ok((events, meta))
}

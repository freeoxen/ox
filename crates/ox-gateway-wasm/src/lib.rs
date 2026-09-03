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

fn host_write(path: &str, data: &str) -> Result<String, String> {
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
            String::from_utf8(buf).map_err(|e| e.to_string())
        } else {
            Ok(String::new())
        }
    } else {
        let mut buf = vec![0u8; (-n) as usize];
        unsafe { store_result(buf.as_mut_ptr() as i32) };
        Err(String::from_utf8(buf).unwrap_or_else(|_| "unknown error".into()))
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn run() -> i32 {
    // Wire mode: block/wire_config means this instance owns one HTTP
    // exchange — decode, dispatch, drain, encode, and error envelopes.
    if let Ok(Some(cfg)) = host_read("block/wire_config") {
        return wire::run(&cfg);
    }
    // Broker mode: a block/config present in the namespace means this
    // instance drives one completion end-to-end.
    if let Ok(Some(cfg)) = host_read("block/config") {
        return broker::run(&cfg);
    }
    // Stats mode: aggregate the usage ledger into one summary. Codec mode
    // otherwise.
    if let Ok(Some(cfg)) = host_read("block/stats_config") {
        return stats::run(&cfg);
    }
    let outcome = match execute() {
        Ok(result) => host_write("codec/result", &result.to_string()),
        Err(e) => host_write(
            "codec/result",
            &serde_json::json!({ "error": e }).to_string(),
        ),
    };
    match outcome {
        Ok(_) => 0,
        Err(_) => 1,
    }
}

fn execute() -> Result<serde_json::Value, String> {
    let job_str = host_read("codec/job")?.ok_or_else(|| "no job at codec/job".to_string())?;
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
            let request: CompletionRequest = serde_json::from_value(job["request"].clone())
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
    let events: Vec<StreamEvent> =
        serde_json::from_value(job["events"].clone()).map_err(|e| format!("bad events: {e}"))?;
    let meta = ResponseMeta {
        id: job["meta"]["id"].as_str().unwrap_or("").to_string(),
        model: job["meta"]["model"].as_str().unwrap_or("").to_string(),
        created: job["meta"]["created"].as_u64().unwrap_or(0),
    };
    Ok((events, meta))
}

mod broker {
    //! The broker Block: the per-request dispatch state machine as pure
    //! path reads/writes. This is the native `dispatch::drive` ported to
    //! the sync guest ABI — resolution via gate/* and secret/*, upstream
    //! dispatch and drain via upstream/*, progress reported through the
    //! completion broker's push/status/usage sub-paths, records appended
    //! to gateway/usage and gateway/traffic.

    use super::{host_read, host_write};
    use ox_codec::UsageInfo;
    use ox_kernel::{CompletionRequest, StreamEvent};
    use ox_types::AccountConfig;
    use ox_types::api_key::ApiKey;
    use ox_types::provider::ProviderConfig;

    pub fn run(cfg_str: &str) -> i32 {
        match drive(cfg_str) {
            Ok(()) => 0,
            Err(_) => 1,
        }
    }

    struct Ctx {
        base: String,
        traffic: bool,
        request: CompletionRequest,
        started_at_ms: u64,
        events: Vec<StreamEvent>,
        upstream_body: Option<serde_json::Value>,
        account: String,
        model_id: String,
        upstream_dialect: String,
    }

    fn read_json(path: &str) -> Result<Option<serde_json::Value>, String> {
        Ok(match host_read(path)? {
            Some(s) => {
                Some(serde_json::from_str(&s).map_err(|e| format!("bad JSON at {path}: {e}"))?)
            }
            None => None,
        })
    }

    fn write_json(path: &str, value: &serde_json::Value) -> Result<(), String> {
        host_write(path, &value.to_string()).map(|_| ())
    }

    fn now_ms() -> u64 {
        // The host serves time at the Isotope-conventional /sys path; the
        // guest has no clock of its own.
        read_json("sys/time/now_unix_ms")
            .ok()
            .flatten()
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    }

    fn drive(cfg_str: &str) -> Result<(), String> {
        let cfg: serde_json::Value =
            serde_json::from_str(cfg_str).map_err(|e| format!("bad block/config: {e}"))?;
        let base = cfg["inflight"]
            .as_str()
            .ok_or("config missing inflight")?
            .to_string();
        let traffic = cfg["traffic"].as_bool().unwrap_or(false);

        let request: CompletionRequest = serde_json::from_value(
            read_json(&format!("{base}/request"))?.ok_or("no request at inflight")?,
        )
        .map_err(|e| format!("bad request: {e}"))?;

        let mut ctx = Ctx {
            base,
            traffic,
            started_at_ms: now_ms(),
            account: "(unknown)".into(),
            model_id: request.model.clone(),
            upstream_dialect: String::new(),
            request,
            events: Vec::new(),
            upstream_body: None,
        };

        match run_request(&mut ctx) {
            Ok(()) => Ok(()),
            Err(reason) => {
                fail(&ctx, &reason);
                // The request terminated Failed — the Block itself
                // completed its job, so exit 0 after recording.
                Ok(())
            }
        }
    }

    fn run_request(ctx: &mut Ctx) -> Result<(), String> {
        // --- resolution --------------------------------------------------
        let (account, model_id) = match ctx.request.model.split_once('/') {
            Some((a, m)) => (a.to_string(), m.to_string()),
            None => {
                let role = read_json(&format!("gate/completions/{}", ctx.request.model))?
                    .ok_or_else(|| format!("no role named '{}'", ctx.request.model))?;
                let account = role["account"]
                    .as_str()
                    .ok_or("invalid CompletionRole")?
                    .to_string();
                let model_id = role["model_id"]
                    .as_str()
                    .ok_or("invalid CompletionRole")?
                    .to_string();
                (account, model_id)
            }
        };
        ctx.account = account.clone();
        ctx.model_id = model_id.clone();

        let acct: AccountConfig = serde_json::from_value(
            read_json(&format!("gate/accounts/{account}"))?
                .ok_or_else(|| format!("no account named '{account}'"))?,
        )
        .map_err(|e| format!("invalid AccountConfig: {e}"))?;
        let provider: ProviderConfig = serde_json::from_value(
            read_json(&format!("gate/providers/{}", acct.provider))?
                .ok_or_else(|| format!("no provider named '{}'", acct.provider))?,
        )
        .map_err(|e| format!("invalid ProviderConfig: {e}"))?;
        ctx.upstream_dialect = provider.dialect.clone();

        let key: Option<ApiKey> = read_json(&format!("secret/keys/{account}"))?
            .and_then(|v| serde_json::from_value(v).ok());
        let key = match key {
            Some(k) => k,
            None if !provider.resolved_auth().requires_key() => ApiKey::new(""),
            None => {
                return Err(format!(
                    "no API key for account '{account}' — add one to ~/.ox/keys.json or set OX_GATE__ACCOUNTS__{}__KEY",
                    account.to_uppercase(),
                ));
            }
        };

        // --- streaming status -------------------------------------------
        write_json(
            &format!("{}/status", ctx.base),
            &serde_json::json!({
                "state": "streaming",
                "account": account,
                "model_id": model_id,
                "started_at_ms": ctx.started_at_ms,
            }),
        )?;

        // --- upstream dispatch ------------------------------------------
        let upstream_req = ox_codec::upstream::build_upstream_request_json(
            &provider,
            &key,
            &ctx.request,
            &model_id,
        );
        ctx.upstream_body = Some(upstream_req["request"]["body"].clone());
        let rel = host_write("upstream", &upstream_req.to_string())
            .map_err(|e| format!("upstream dispatch failed: {e}"))?;
        // Mounted stores return mount-relative paths (e.g. "outstanding/0").
        let handle = format!("upstream/{}", rel.trim_start_matches("upstream/"));

        // --- drain -------------------------------------------------------
        let mut in_band_error: Option<String> = None;
        let mut next = 0usize;
        let outcome = loop {
            let events: Vec<StreamEvent> = match read_json(&format!("{handle}/events/from/{next}"))
            {
                Ok(v) => serde_json::from_value(v.unwrap_or(serde_json::json!([])))
                    .map_err(|e| format!("bad events: {e}"))?,
                Err(e) => break Err(format!("upstream drain failed: {e}")),
            };
            if !events.is_empty() {
                push_events(ctx, &events, &mut in_band_error)?;
                next += events.len();
            }
            let status = match read_json(&format!("{handle}")) {
                Ok(Some(s)) => s,
                Ok(None) => break Err("upstream inflight vanished".to_string()),
                Err(e) => break Err(format!("upstream status read failed: {e}")),
            };
            match status["state"].as_str() {
                Some("streaming") => continue,
                Some("complete") | Some("failed") => {
                    // Close-out drain for events racing the terminal flip.
                    if let Ok(Some(tail)) = read_json(&format!("{handle}/events/from/{next}")) {
                        let tail: Vec<StreamEvent> =
                            serde_json::from_value(tail).map_err(|e| format!("bad tail: {e}"))?;
                        if !tail.is_empty() {
                            push_events(ctx, &tail, &mut in_band_error)?;
                        }
                    }
                    if status["state"] == "failed" {
                        break Err(status["reason"]
                            .as_str()
                            .unwrap_or("upstream failed")
                            .to_string());
                    }
                    break Ok(());
                }
                _ => break Err("unknown upstream status".to_string()),
            }
        };
        // GC the upstream handle regardless of outcome.
        let _ = host_write(&handle, "null");
        outcome?;
        if let Some(reason) = in_band_error {
            return Err(reason);
        }

        // --- terminal clean path ----------------------------------------
        let usage = UsageInfo::from_events(&ctx.events);
        let completed_at_ms = now_ms();
        write_json(
            &format!("{}/usage", ctx.base),
            &serde_json::to_value(&usage).map_err(|e| e.to_string())?,
        )?;
        write_json(
            &format!("{}/status", ctx.base),
            &serde_json::json!({
                "state": "complete",
                "account": account,
                "model_id": model_id,
                "completed_at_ms": completed_at_ms,
            }),
        )?;

        let record = serde_json::json!({
            "id": new_id(ctx.started_at_ms),
            "account": account,
            "model_id": model_id,
            "dialect": inbound_dialect(&ctx.request),
            "upstream_dialect": ctx.upstream_dialect,
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "cache_creation_input_tokens": usage.cache_creation_input_tokens,
            "cache_read_input_tokens": usage.cache_read_input_tokens,
            "started_at_ms": ctx.started_at_ms,
            "completed_at_ms": completed_at_ms,
            "estimated_cost_usd": ox_types::pricing::estimate_cost(
                &model_id,
                usage.input_tokens,
                usage.output_tokens,
            ),
        });
        write_json("gateway/usage/append", &record)?;

        write_traffic(
            ctx,
            Some(&usage),
            completed_at_ms,
            serde_json::json!({
                "state": "complete",
                "account": account,
                "model_id": model_id,
                "completed_at_ms": completed_at_ms,
            }),
        );
        Ok(())
    }

    fn push_events(
        ctx: &mut Ctx,
        events: &[StreamEvent],
        in_band: &mut Option<String>,
    ) -> Result<(), String> {
        for ev in events {
            if let StreamEvent::Error { message } = ev {
                *in_band = Some(message.clone());
            }
        }
        write_json(
            &format!("{}/push", ctx.base),
            &serde_json::to_value(events).map_err(|e| e.to_string())?,
        )?;
        ctx.events.extend(events.iter().cloned());
        Ok(())
    }

    fn fail(ctx: &Ctx, reason: &str) {
        let failed_at_ms = now_ms();
        let status = serde_json::json!({
            "state": "failed",
            "account": ctx.account,
            "model_id": ctx.model_id,
            "reason": reason,
            "failed_at_ms": failed_at_ms,
        });
        let _ = write_json(&format!("{}/status", ctx.base), &status);
        write_traffic(ctx, None, failed_at_ms, status);
    }

    fn write_traffic(
        ctx: &Ctx,
        usage: Option<&UsageInfo>,
        completed_at_ms: u64,
        status: serde_json::Value,
    ) {
        if !ctx.traffic {
            return;
        }
        // The status the Block itself wrote — re-reading the inflight races
        // the route's GC once the terminal status lands.
        let record = serde_json::json!({
            "kind": "completion",
            "started_at_ms": ctx.started_at_ms,
            "completed_at_ms": completed_at_ms,
            "request": serde_json::to_value(&ctx.request).unwrap_or_default(),
            "upstream_body": ctx.upstream_body,
            "events": serde_json::to_value(&ctx.events).unwrap_or_default(),
            "status": status,
            "usage": usage.map(|u| serde_json::to_value(u).unwrap_or_default()),
        });
        let _ = write_json("gateway/traffic/append", &record);
    }

    /// The wire Block stamps the inbound dialect on the request it queues;
    /// requests that arrive without one (raw ox-native posts, older
    /// records) fall back to the model-string heuristic.
    fn inbound_dialect(request: &CompletionRequest) -> String {
        request
            .extra
            .get("ox_inbound_dialect")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| ox_codec::upstream::detect_inbound_dialect(&request.model))
    }

    fn new_id(started_at_ms: u64) -> String {
        // Time-derived, matching the native scheme's shape. The guest has
        // no counter across instances; the ms clock plus the request's
        // start time gives adequate uniqueness for a log id.
        format!("{:016x}-{:08x}", started_at_ms, now_ms() as u32)
    }
}

mod wire {
    //! The wire Block: one HTTP exchange end-to-end. This is the native
    //! route handler ported to the sync guest ABI — decode the inbound
    //! wire body, mint the response identity, queue the completion, drain
    //! it, and produce either a buffered response body or a stream of
    //! wire frames, with dialect-shaped error envelopes throughout.

    use super::{host_read, host_write};
    use ox_codec::{ResponseMeta, SseEncoder};
    use ox_kernel::StreamEvent;

    pub fn run(cfg_str: &str) -> i32 {
        match drive(cfg_str) {
            Ok(()) => 0,
            Err(_) => 1,
        }
    }

    fn read_json(path: &str) -> Result<Option<serde_json::Value>, String> {
        Ok(match host_read(path)? {
            Some(s) => {
                Some(serde_json::from_str(&s).map_err(|e| format!("bad JSON at {path}: {e}"))?)
            }
            None => None,
        })
    }

    fn write_json(path: &str, value: &serde_json::Value) -> Result<(), String> {
        host_write(path, &value.to_string()).map(|_| ())
    }

    fn now_ms() -> u64 {
        read_json("sys/time/now_unix_ms")
            .ok()
            .flatten()
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    }

    fn error_head(wire: &str, dialect: &str, status: u16, message: &str) {
        let _ = write_json(
            &format!("{wire}/head"),
            &serde_json::json!({
                "mode": "error",
                "status": status,
                "body": ox_codec::wire::error_body(dialect, status, message),
            }),
        );
        let _ = write_json(&format!("{wire}/done"), &serde_json::json!(true));
    }

    fn drive(cfg_str: &str) -> Result<(), String> {
        let cfg: serde_json::Value =
            serde_json::from_str(cfg_str).map_err(|e| format!("bad wire_config: {e}"))?;
        let wire = cfg["wire"]
            .as_str()
            .ok_or("wire_config missing wire")?
            .to_string();
        let dialect = cfg["dialect"].as_str().unwrap_or("anthropic").to_string();

        let inbound = read_json(&format!("{wire}/inbound"))?.ok_or("no inbound record")?;
        let body = inbound["body"].clone();

        // --- decode ------------------------------------------------------
        let req = match dialect.as_str() {
            "openai" => ox_codec::openai::decode_request(&body),
            _ => ox_codec::anthropic::decode_request(&body),
        };
        let mut req = match req {
            Ok(r) => r,
            Err(e) => {
                error_head(&wire, &dialect, 400, &e.to_string());
                return Ok(());
            }
        };
        // Provenance for usage records: which dialect this exchange arrived
        // in. Stamped unconditionally so a client-sent value can't spoof it;
        // the translate allow-lists keep it out of the upstream body.
        req.extra
            .insert("ox_inbound_dialect".into(), serde_json::json!(dialect));
        let streaming = req.stream;

        // --- response identity ------------------------------------------
        let now = now_ms();
        let prefix = if dialect == "openai" {
            "chatcmpl-"
        } else {
            "msg_"
        };
        let meta = ResponseMeta {
            id: format!("{prefix}{:x}", now.wrapping_mul(1_000_003) ^ 0x9e37_79b9),
            model: req.model.clone(),
            created: now / 1000,
        };

        // --- queue -------------------------------------------------------
        let req_value = serde_json::to_value(&req).map_err(|e| e.to_string())?;
        let rel = match host_write("gateway/completions", &req_value.to_string()) {
            Ok(p) => p,
            Err(e) => {
                error_head(&wire, &dialect, 500, &e);
                return Ok(());
            }
        };
        let base = format!(
            "gateway/completions/{}",
            rel.trim_start_matches("gateway/completions/")
        );

        // --- drain + encode ---------------------------------------------
        let outcome = if streaming {
            stream_response(&wire, &dialect, &base, meta)
        } else {
            buffered_response(&wire, &dialect, &base, meta)
        };
        // GC the completion handle; the wire handle is the edge's to GC.
        let _ = host_write(&base, "null");
        let _ = write_json(&format!("{wire}/done"), &serde_json::json!(true));
        outcome
    }

    /// Read one drain step: events since `next`, then status. Terminal
    /// includes the close-out tail, mirroring the native drains.
    fn drain_step(
        base: &str,
        next: &mut usize,
        sink: &mut Vec<StreamEvent>,
    ) -> Result<Option<serde_json::Value>, String> {
        let events: Vec<StreamEvent> = serde_json::from_value(
            read_json(&format!("{base}/events/from/{next}"))?.unwrap_or(serde_json::json!([])),
        )
        .map_err(|e| format!("bad events: {e}"))?;
        *next += events.len();
        sink.extend(events);

        let status = read_json(base)?.ok_or("inflight vanished mid-drain")?;
        let state = status["state"].as_str().unwrap_or("");
        if state == "complete" || state == "failed" {
            let tail: Vec<StreamEvent> = serde_json::from_value(
                read_json(&format!("{base}/events/from/{next}"))?.unwrap_or(serde_json::json!([])),
            )
            .map_err(|e| format!("bad tail: {e}"))?;
            *next += tail.len();
            sink.extend(tail);
            return Ok(Some(status));
        }
        Ok(None)
    }

    fn stream_response(
        wire: &str,
        dialect: &str,
        base: &str,
        meta: ResponseMeta,
    ) -> Result<(), String> {
        write_json(
            &format!("{wire}/head"),
            &serde_json::json!({ "mode": "stream" }),
        )?;
        let mut enc = SseEncoder::new(dialect, meta);
        let mut next = 0usize;
        let mut emitted = 0usize;
        loop {
            let mut batch: Vec<StreamEvent> = Vec::new();
            let terminal = drain_step(base, &mut next, &mut batch)?;
            let mut frames: Vec<serde_json::Value> = Vec::new();
            for ev in &batch {
                for f in enc.encode_sse(ev) {
                    frames.push(serde_json::json!(f));
                }
            }
            emitted += batch.len();
            let _ = emitted;
            match terminal {
                None => {
                    if !frames.is_empty() {
                        write_json(
                            &format!("{wire}/frames/push"),
                            &serde_json::Value::Array(frames),
                        )?;
                    }
                }
                Some(status) => {
                    if status["state"] == "failed" {
                        let reason = status["reason"]
                            .as_str()
                            .unwrap_or("upstream failed")
                            .to_string();
                        for f in enc.encode_sse(&StreamEvent::Error { message: reason }) {
                            frames.push(serde_json::json!(f));
                        }
                    } else {
                        for f in enc.finish() {
                            frames.push(serde_json::json!(f));
                        }
                    }
                    if !frames.is_empty() {
                        write_json(
                            &format!("{wire}/frames/push"),
                            &serde_json::Value::Array(frames),
                        )?;
                    }
                    return Ok(());
                }
            }
        }
    }

    fn buffered_response(
        wire: &str,
        dialect: &str,
        base: &str,
        meta: ResponseMeta,
    ) -> Result<(), String> {
        let mut events: Vec<StreamEvent> = Vec::new();
        let mut next = 0usize;
        let status = loop {
            if let Some(status) = drain_step(base, &mut next, &mut events)? {
                break status;
            }
        };
        if status["state"] == "failed" {
            let reason = status["reason"].as_str().unwrap_or("upstream failed");
            error_head(wire, dialect, 500, reason);
            return Ok(());
        }
        let body = match dialect {
            "openai" => ox_codec::openai::encode_response(&events, &meta),
            _ => ox_codec::anthropic::encode_response(&events, &meta),
        };
        write_json(
            &format!("{wire}/head"),
            &serde_json::json!({ "mode": "json", "status": 200, "body": body }),
        )
    }
}

mod stats {
    //! Stats mode: one instance per stats request. Reads the usage ledger
    //! and the in-flight listing through the assembly-wired namespace,
    //! computes the dashboard aggregates, writes the summary onto the
    //! telemetry handle. The edge route never sees a usage record.

    use super::{host_read, host_write};
    use ox_types::UsageRecord;
    use std::collections::BTreeMap;

    const HOUR_MS: u64 = 3_600_000;
    const DAY_MS: u64 = 86_400_000;
    const RECENT_LIMIT: usize = 20;

    fn read_json(path: &str) -> Result<Option<serde_json::Value>, String> {
        match host_read(path)? {
            Some(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(|e| format!("bad JSON at {path}: {e}")),
            None => Ok(None),
        }
    }

    pub fn run(cfg_str: &str) -> i32 {
        match drive(cfg_str) {
            Ok(()) => 0,
            Err(_) => 1,
        }
    }

    #[derive(Default)]
    struct Totals {
        requests: u64,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_input_tokens: u64,
        cache_creation_input_tokens: u64,
        estimated_cost_usd: Option<f64>,
        priced_requests: u64,
    }

    impl Totals {
        fn add(&mut self, r: &UsageRecord) {
            self.requests += 1;
            self.input_tokens += r.input_tokens as u64;
            self.output_tokens += r.output_tokens as u64;
            self.cache_read_input_tokens += r.cache_read_input_tokens as u64;
            self.cache_creation_input_tokens += r.cache_creation_input_tokens as u64;
            if let Some(c) = r.estimated_cost_usd {
                *self.estimated_cost_usd.get_or_insert(0.0) += c;
                self.priced_requests += 1;
            }
        }

        fn json(&self) -> serde_json::Value {
            serde_json::json!({
                "requests": self.requests,
                "input_tokens": self.input_tokens,
                "output_tokens": self.output_tokens,
                "cache_read_input_tokens": self.cache_read_input_tokens,
                "cache_creation_input_tokens": self.cache_creation_input_tokens,
                "estimated_cost_usd": self.estimated_cost_usd,
                "priced_requests": self.priced_requests,
            })
        }
    }

    fn drive(cfg_str: &str) -> Result<(), String> {
        let cfg: serde_json::Value =
            serde_json::from_str(cfg_str).map_err(|e| format!("bad stats_config: {e}"))?;
        let handle = cfg["telemetry"]
            .as_str()
            .ok_or("stats_config missing telemetry")?
            .to_string();

        // Absent ledger (nothing recorded yet) is zeros, not an error;
        // a refused or failing read still propagates.
        let records: Vec<UsageRecord> = match read_json("gateway/usage")? {
            Some(v) => serde_json::from_value(v).map_err(|e| format!("bad usage ledger: {e}"))?,
            None => Vec::new(),
        };

        let in_flight = read_json("gateway/completions/outstanding")?
            .and_then(|v| v["items"].as_array().map(|a| a.len() as u64))
            .unwrap_or(0);

        let now_ms = read_json("sys/time/now_unix_ms")?
            .and_then(|v| v.as_u64())
            .ok_or("no clock at sys/time/now_unix_ms")?;
        // "Today" starts at the caller's local midnight when the request
        // carries a timezone offset (minutes east of UTC); UTC otherwise.
        let tz_offset_ms: i64 = read_json(&format!("{handle}/params"))?
            .and_then(|p| p["tz_offset_min"].as_i64())
            .unwrap_or(0)
            * 60_000;
        let start_of_today_ms = {
            let local = now_ms as i64 + tz_offset_ms;
            (local - local.rem_euclid(DAY_MS as i64) - tz_offset_ms).max(0) as u64
        };
        let window_start = (now_ms - 23 * HOUR_MS) - ((now_ms - 23 * HOUR_MS) % HOUR_MS);

        let mut totals = Totals::default();
        let mut today = Totals::default();
        let mut by_model: BTreeMap<(String, String), Totals> = BTreeMap::new();
        let mut by_hour: BTreeMap<u64, (u64, u64, u64)> = BTreeMap::new();
        for i in 0..24 {
            by_hour.insert(window_start + i * HOUR_MS, (0, 0, 0));
        }

        for r in &records {
            totals.add(r);
            if r.completed_at_ms >= start_of_today_ms {
                today.add(r);
            }
            by_model
                .entry((r.account.clone(), r.model_id.clone()))
                .or_default()
                .add(r);
            if r.completed_at_ms >= window_start {
                let bucket = r.completed_at_ms - (r.completed_at_ms % HOUR_MS);
                if let Some(b) = by_hour.get_mut(&bucket) {
                    b.0 += 1;
                    b.1 += r.input_tokens as u64;
                    b.2 += r.output_tokens as u64;
                }
            }
        }

        let mut model_rows: Vec<((String, String), Totals)> = by_model.into_iter().collect();
        model_rows.sort_by(|a, b| {
            (b.1.input_tokens + b.1.output_tokens).cmp(&(a.1.input_tokens + a.1.output_tokens))
        });
        let by_model: Vec<serde_json::Value> = model_rows
            .into_iter()
            .map(|((account, model_id), t)| {
                let mut row = t.json();
                row["account"] = serde_json::json!(account);
                row["model_id"] = serde_json::json!(model_id);
                row
            })
            .collect();

        let by_hour: Vec<serde_json::Value> = by_hour
            .into_iter()
            .map(|(start, (req, inp, out))| {
                serde_json::json!({
                    "hour_start_ms": start,
                    "requests": req,
                    "input_tokens": inp,
                    "output_tokens": out,
                })
            })
            .collect();

        let mut recent = records;
        recent.sort_by(|a, b| b.completed_at_ms.cmp(&a.completed_at_ms));
        recent.truncate(RECENT_LIMIT);
        let recent = serde_json::to_value(&recent).map_err(|e| format!("recent encode: {e}"))?;

        let summary = serde_json::json!({
            "generated_at_ms": now_ms,
            "in_flight": in_flight,
            "totals": totals.json(),
            "today": today.json(),
            "by_model": by_model,
            "by_hour": by_hour,
            "recent": recent,
        });
        host_write(&format!("{handle}/summary"), &summary.to_string()).map(|_| ())
    }
}

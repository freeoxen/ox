//! Per-request upstream dispatch task.
//!
//! Spawned by `CompletionBrokerStore::write` when a `CompletionRequest`
//! lands at the broker root. Resolves the model → role → account →
//! provider → API key chain via substrate reads, builds an HttpRequest,
//! drives the SseHttpExecutor stream, pushes events into the shared
//! Inflight buffer, and writes a UsageRecord on terminal Complete.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ox_broker::ClientHandle;
use ox_kernel::{CompletionRequest, PathComponent};
use ox_path::oxpath;
use ox_types::CompletionRole;
use structfs_core_store::{Path, Record};
use structfs_http::types::HttpRequest;
use structfs_serde_store::{from_value, to_value};

use crate::codec::UsageInfo;
use crate::completion_broker::inflight::{CompletionStatus, Inflight};
use crate::upstream_store::{UpstreamRequest, UpstreamStatus};
use crate::{AccountConfig, ApiKey, AuthScheme, ProviderConfig, UsageRecord};

pub(super) async fn per_request_task(
    inflight: Arc<Inflight>,
    substrate: ClientHandle,
    upstream: ClientHandle,
    usage_writer: ClientHandle,
    traffic_writer: Option<ClientHandle>,
) {
    let started_at_ms = now_ms();
    let upstream_body = drive(&inflight, &substrate, &upstream, &usage_writer).await;

    // Traffic log: one complete record per request at terminal, success or
    // failure. Read back from the inflight state so the record reflects
    // exactly what drains observed. The Arc keeps the state alive even if
    // the route GC'd the handle already.
    let Some(traffic) = traffic_writer else { return };
    let record = {
        let state = inflight.state.lock().await;
        serde_json::json!({
            "kind": "completion",
            "started_at_ms": started_at_ms,
            "completed_at_ms": now_ms(),
            "request": serde_json::to_value(&state.request).unwrap_or_default(),
            "upstream_body": upstream_body,
            "events": serde_json::to_value(&state.events).unwrap_or_default(),
            "status": serde_json::to_value(&state.status).unwrap_or_default(),
            "usage": serde_json::to_value(&state.usage).unwrap_or_default(),
        })
    };
    let value = structfs_serde_store::json_to_value(record);
    let _ = traffic.write(&oxpath!("append"), Record::parsed(value)).await;
}

/// Drive one request to a terminal status. Returns the upstream request
/// body when the request got far enough to build one.
async fn drive(
    inflight: &Arc<Inflight>,
    substrate: &ClientHandle,
    upstream: &ClientHandle,
    usage_writer: &ClientHandle,
) -> Option<serde_json::Value> {
    let request: CompletionRequest = {
        let state = inflight.state.lock().await;
        state.request.clone()
    };

    let role = match resolve_model(&request.model, substrate).await {
        Ok(r) => r,
        Err(reason) => {
            mark_failed(inflight, "(unknown)".into(), request.model.clone(), reason).await;
            return None;
        }
    };

    let (account_cfg, provider, api_key) = match resolve_account(&role, substrate).await {
        Ok(t) => t,
        Err(reason) => {
            mark_failed(inflight, role.account.clone(), role.model_id.clone(), reason).await;
            return None;
        }
    };
    // `account_cfg` is resolved for completeness; no per-account fields are
    // consumed at this layer yet.
    let _ = account_cfg;

    // Flip to Streaming and notify waiters.
    let started_at_ms = now_ms();
    {
        let mut state = inflight.state.lock().await;
        state.status = CompletionStatus::Streaming {
            account: role.account.clone(),
            model_id: role.model_id.clone(),
            started_at_ms,
        };
    }
    inflight.notify.notify_waiters();

    // Build the upstream HTTP request via the dialect codec.
    let http_request = match build_http_request(&provider, &api_key, &request, &role.model_id) {
        Ok(r) => r,
        Err(reason) => {
            mark_failed(inflight, role.account.clone(), role.model_id.clone(), reason).await;
            return None;
        }
    };
    let upstream_body = http_request.body.clone();

    // Hand the request to the upstream mount and drain it back through
    // blocking substrate reads. The broker holds no sockets: this loop is
    // pure paths, which is exactly the surface the phase-3 broker Block
    // will run against.
    let handle_rel = match upstream
        .write_typed(
            &Path::try_from_components(Vec::new()).expect("empty path is valid"),
            &UpstreamRequest {
                dialect: provider.dialect.clone(),
                request: http_request,
            },
        )
        .await
    {
        Ok(p) => p,
        Err(e) => {
            mark_failed(
                inflight,
                role.account.clone(),
                role.model_id.clone(),
                format!("upstream dispatch failed: {e}"),
            )
            .await;
            return upstream_body;
        }
    };

    // An in-band error frame (upstream 200 + `event: error`) still gets
    // pushed so streaming drains relay it, but the task must finish Failed —
    // flipping Complete would hand non-streaming clients a 200 with
    // truncated content and write a usage record for a failed request.
    let mut in_band_error: Option<String> = None;
    let mut next: usize = 0;
    let terminal = loop {
        let sub = Path::parse(&format!("events/from/{next}"))
            .expect("events/from/{n} components are valid");
        let events: Vec<ox_types::StreamEvent> = match upstream
            .read_typed(&handle_rel.join(&sub))
            .await
        {
            Ok(v) => v.unwrap_or_default(),
            Err(e) => break Err(format!("upstream drain failed: {e}")),
        };
        if !events.is_empty() {
            let mut state = inflight.state.lock().await;
            for ev in &events {
                if let ox_types::StreamEvent::Error { message } = ev {
                    in_band_error = Some(message.clone());
                }
                state.events.push(ev.clone());
            }
            drop(state);
            inflight.notify.notify_waiters();
            next += events.len();
        }
        let status: UpstreamStatus = match upstream.read_typed(&handle_rel).await {
            Ok(Some(s)) => s,
            Ok(None) => break Err("upstream inflight vanished".to_string()),
            Err(e) => break Err(format!("upstream status read failed: {e}")),
        };
        if status.is_terminal() {
            // Close-out drain: events landing between the events-read and
            // the terminal flip.
            let sub = Path::parse(&format!("events/from/{next}"))
                .expect("valid path");
            if let Ok(Some(tail)) = upstream
                .read_typed::<Vec<ox_types::StreamEvent>>(&handle_rel.join(&sub))
                .await
            {
                if !tail.is_empty() {
                    let mut state = inflight.state.lock().await;
                    for ev in &tail {
                        if let ox_types::StreamEvent::Error { message } = ev {
                            in_band_error = Some(message.clone());
                        }
                        state.events.push(ev.clone());
                    }
                    drop(state);
                    inflight.notify.notify_waiters();
                }
            }
            break Ok(status);
        }
    };
    // GC the upstream handle regardless of outcome.
    let _ = upstream
        .write(&handle_rel, Record::parsed(structfs_core_store::Value::Null))
        .await;

    match terminal {
        Err(reason) => {
            mark_failed(inflight, role.account.clone(), role.model_id.clone(), reason).await;
            return upstream_body;
        }
        Ok(UpstreamStatus::Failed { reason }) => {
            mark_failed(inflight, role.account.clone(), role.model_id.clone(), reason).await;
            return upstream_body;
        }
        Ok(_) => {}
    }
    if let Some(reason) = in_band_error {
        mark_failed(inflight, role.account.clone(), role.model_id.clone(), reason).await;
        return upstream_body;
    }

    // Terminal clean path: compute usage, flip to Complete, notify, append
    // usage record.
    let completed_at_ms = now_ms();
    let usage = {
        let state = inflight.state.lock().await;
        UsageInfo::from_events(&state.events)
    };
    {
        let mut state = inflight.state.lock().await;
        state.status = CompletionStatus::Complete {
            account: role.account.clone(),
            model_id: role.model_id.clone(),
            completed_at_ms,
        };
        state.usage = Some(usage.clone());
    }
    inflight.notify.notify_waiters();

    // Append usage record — best-effort. If the write fails the completion
    // is already marked Complete; usage observability is downstream of that.
    let record = UsageRecord {
        id: new_id(),
        account: role.account.clone(),
        model_id: role.model_id.clone(),
        dialect: detect_inbound_dialect(&request.model),
        upstream_dialect: provider.dialect.clone(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        started_at_ms,
        completed_at_ms,
        estimated_cost_usd: estimate_cost(&role.model_id, usage.input_tokens, usage.output_tokens),
    };
    if let Ok(value) = to_value(&record) {
        let append_path = oxpath!("append");
        let _ = usage_writer.write(&append_path, Record::parsed(value)).await;
    }
    upstream_body
}

/// Resolve the model string to a `CompletionRole`.
///
/// Slash-form (`account/model_id`) is parsed directly. Otherwise the
/// string is treated as a named role looked up at `gate/completions/{name}`
/// in the substrate.
async fn resolve_model(model: &str, substrate: &ClientHandle) -> Result<CompletionRole, String> {
    if let Some((account, model_id)) = model.split_once('/') {
        return Ok(CompletionRole {
            account: account.to_string(),
            model_id: model_id.to_string(),
        });
    }
    let name_comp = PathComponent::try_new(model).map_err(|e| e.to_string())?;
    let path = oxpath!("gate", "completions", name_comp);
    let record = substrate
        .read(&path)
        .await
        .map_err(|e| format!("substrate read failed: {e}"))?
        .ok_or_else(|| format!("no role named '{model}'"))?;
    let value = record
        .as_value()
        .ok_or_else(|| "role record not parsed".to_string())?
        .clone();
    from_value(value).map_err(|e| format!("invalid CompletionRole: {e}"))
}

/// Resolve account → provider → API key via three substrate reads.
async fn resolve_account(
    role: &CompletionRole,
    substrate: &ClientHandle,
) -> Result<(AccountConfig, ProviderConfig, ApiKey), String> {
    let acct_comp = PathComponent::try_new(&role.account).map_err(|e| e.to_string())?;
    let acct_path = oxpath!("gate", "accounts", acct_comp);
    let acct: AccountConfig = read_typed(substrate, &acct_path)
        .await?
        .ok_or_else(|| format!("no account named '{}'", role.account))?;

    let prov_comp = PathComponent::try_new(&acct.provider).map_err(|e| e.to_string())?;
    let prov_path = oxpath!("gate", "providers", prov_comp);
    let provider: ProviderConfig = read_typed(substrate, &prov_path)
        .await?
        .ok_or_else(|| format!("no provider named '{}'", acct.provider))?;

    // Keyless providers (auth = "none": LM Studio, Ollama, other local
    // endpoints) are a first-class configuration — only require a key when
    // the provider's resolved auth scheme actually uses one.
    let key_comp = PathComponent::try_new(&role.account).map_err(|e| e.to_string())?;
    let key_path = oxpath!("secret", "keys", key_comp);
    let key: Option<ApiKey> = read_typed(substrate, &key_path).await?;
    let key = match key {
        Some(k) => k,
        None if !provider.resolved_auth().requires_key() => ApiKey::new(""),
        None => {
            return Err(format!(
                "no API key for account '{}' — add one to ~/.ox/keys.json or set OX_GATE__ACCOUNTS__{}__KEY",
                role.account,
                role.account.to_uppercase(),
            ));
        }
    };

    Ok((acct, provider, key))
}

async fn read_typed<T: serde::de::DeserializeOwned>(
    substrate: &ClientHandle,
    path: &Path,
) -> Result<Option<T>, String> {
    let record = substrate.read(path).await.map_err(|e| e.to_string())?;
    match record {
        Some(r) => {
            let value = r.as_value().ok_or_else(|| "not parsed".to_string())?.clone();
            from_value(value).map(Some).map_err(|e| e.to_string())
        }
        None => Ok(None),
    }
}

fn build_http_request(
    provider: &ProviderConfig,
    api_key: &ApiKey,
    request: &CompletionRequest,
    upstream_model_id: &str,
) -> Result<HttpRequest, String> {
    // Shared with the broker Block — one builder, no drift. The JSON is
    // the serde form of UpstreamRequest; deserialize the request half.
    let json = crate::codec::upstream::build_upstream_request_json(
        provider,
        api_key,
        request,
        upstream_model_id,
    );
    serde_json::from_value(json["request"].clone()).map_err(|e| e.to_string())
}

async fn mark_failed(inflight: &Arc<Inflight>, account: String, model_id: String, reason: String) {
    let mut state = inflight.state.lock().await;
    state.status = CompletionStatus::Failed {
        account,
        model_id,
        reason,
        failed_at_ms: now_ms(),
    };
    drop(state);
    inflight.notify.notify_waiters();
}

/// Detect the inbound dialect from the model string.
///
/// This is best-effort metadata for the usage ledger. The gateway's axum
/// handler knows the true dialect (it decoded the body), but that
/// information doesn't reach this layer. We approximate from model id
/// conventions: OpenAI clients use "gpt-*" / "o1-*" / "o3-*"; Anthropic
/// clients use "claude-*". Does not affect dispatch behavior.
fn detect_inbound_dialect(model: &str) -> String {
    crate::codec::upstream::detect_inbound_dialect(model)
}

fn estimate_cost(model: &str, input_tokens: u32, output_tokens: u32) -> Option<f64> {
    let p = crate::pricing::model_pricing(model)?;
    let in_m = input_tokens as f64 / 1_000_000.0;
    let out_m = output_tokens as f64 / 1_000_000.0;
    Some(in_m * p.input_per_mtok + out_m * p.output_per_mtok)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Generate a unique id for a usage record. Uses timestamp + a fast
/// counter rather than pulling in a ULID/UUID crate.
fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:016x}-{:08x}", now_ms(), seq)
}

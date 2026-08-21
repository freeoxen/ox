//! GET /v1/models — aggregated across all accounts in the gate.

use axum::{
    extract::State,
    http::HeaderMap,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use ox_broker::ClientHandle;
use ox_kernel::PathComponent;
use ox_path::oxpath;
use ox_types::ModelInfo;
use serde_json::{json, Value};

pub fn router(client: ClientHandle) -> Router {
    Router::new()
        .route("/v1/models", get(list_models))
        .with_state(client)
}

async fn list_models(headers: HeaderMap, State(client): State<ClientHandle>) -> impl IntoResponse {
    // Read the gate's snapshot for the accounts + providers list.
    let snapshot_record = match client.read(&oxpath!("gate", "snapshot", "state")).await {
        Ok(Some(r)) => r,
        _ => return Json(empty_list_for_dialect(&headers)).into_response(),
    };
    let snapshot_value = match snapshot_record.as_value() {
        Some(v) => v.clone(),
        None => return Json(empty_list_for_dialect(&headers)).into_response(),
    };
    let json_val = structfs_serde_store::value_to_json(snapshot_value);

    let accounts = match json_val.get("accounts").and_then(|v| v.as_object()) {
        Some(a) => a.clone(),
        None => return Json(empty_list_for_dialect(&headers)).into_response(),
    };

    // For each account, look up its provider, then read that provider's
    // catalog. We read each account's provider and fetch that provider's
    // model list. Emit one entry per (account, model) so callers can
    // address the model via the slash form.
    let mut items: Vec<(String, ModelInfo)> = Vec::new();
    for (account_name, account_val) in &accounts {
        let provider_name = match account_val.get("provider").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => continue,
        };
        let account_comp = match PathComponent::try_new(account_name.as_str()) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let provider_comp = match PathComponent::try_new(provider_name) {
            Ok(c) => c,
            Err(_) => continue,
        };
        // The catalog_refresh subscription lands catalogs at
        // config/gate/accounts/{name}/models; gate/providers/{p}/models only
        // holds catalogs written directly into the GateStore instance.
        // Prefer the refreshed per-account catalog, fall back to the gate's.
        let account_models_path = oxpath!("config", "gate", "accounts", account_comp, "models");
        let provider_models_path = oxpath!("gate", "providers", provider_comp, "models");
        let models: Vec<ModelInfo> = match client.read_typed(&account_models_path).await {
            Ok(Some(m)) if !Vec::is_empty(&m) => m,
            _ => match client.read_typed(&provider_models_path).await {
                Ok(Some(m)) => m,
                _ => continue,
            },
        };
        for m in models {
            items.push((account_name.clone(), m));
        }
    }

    if wants_anthropic(&headers) {
        let data: Vec<Value> = items
            .iter()
            .map(|(acct, m)| {
                json!({
                    "id": format!("{}/{}", acct, m.id),
                    "display_name": m.display_name,
                    "type": "model",
                })
            })
            .collect();
        Json(json!({ "data": data })).into_response()
    } else {
        let data: Vec<Value> = items
            .iter()
            .map(|(acct, m)| {
                json!({
                    "id": format!("{}/{}", acct, m.id),
                    "object": "model",
                    "created": 0,
                    "owned_by": acct,
                })
            })
            .collect();
        Json(json!({ "object": "list", "data": data })).into_response()
    }
}

fn wants_anthropic(headers: &HeaderMap) -> bool {
    headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.contains("anthropic"))
        .unwrap_or(false)
}

fn empty_list_for_dialect(headers: &HeaderMap) -> Value {
    if wants_anthropic(headers) {
        json!({ "data": [] })
    } else {
        json!({ "object": "list", "data": [] })
    }
}

//! GET /stats — dumb edge over the telemetry mount: write a request,
//! blocking-read the summary the stats Block computes. The aggregation
//! logic lives in the Block; this handler shuttles JSON.
//!
//! GET /dashboard — a self-contained HTML page that renders the summary
//! (inline CSS/JS, no external assets; the daemon is loopback-only and
//! the page must work offline). Static asset, served at the edge.

use axum::http::StatusCode;
use axum::{
    extract::State,
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use ox_broker::ClientHandle;
use structfs_core_store::{path, Record};

use crate::handle::InflightGc;

pub fn router(client: ClientHandle) -> Router {
    Router::new()
        .route("/stats", get(get_stats))
        .route("/dashboard", get(get_dashboard))
        .with_state(client)
}

async fn get_stats(
    State(client): State<ClientHandle>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    // Pass the caller's timezone through; the Block owns what it means.
    let tz_offset_min = query
        .get("tz_offset_min")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let params =
        structfs_serde_store::json_to_value(serde_json::json!({ "tz_offset_min": tz_offset_min }));
    let rel = match client
        .write(&path!("gateway/telemetry"), Record::parsed(params))
        .await
    {
        Ok(p) => p,
        Err(e) => return stats_error(e.to_string()),
    };
    let handle = path!("gateway/telemetry").join(&rel);

    // The GC guard covers client disconnects while parked on the blocking
    // read — same lifecycle as the completion drains.
    let gc = InflightGc::new(client.clone(), handle.clone());
    let summary = client.read(&handle.join(&path!("summary"))).await;
    gc.gc_now().await;

    match summary {
        Ok(Some(rec)) => {
            let json = rec
                .as_value()
                .cloned()
                .map(structfs_serde_store::value_to_json)
                .unwrap_or(serde_json::Value::Null);
            Json(json).into_response()
        }
        Ok(None) => stats_error("stats block wrote no summary".into()),
        Err(e) => stats_error(e.to_string()),
    }
}

fn stats_error(message: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({"error": {"message": message}})),
    )
        .into_response()
}

async fn get_dashboard() -> impl IntoResponse {
    Html(include_str!("dashboard.html"))
}

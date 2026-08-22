//! GET /stats — usage-ledger aggregates as JSON. GET /dashboard — a
//! self-contained HTML page that renders them (inline CSS/JS, no external
//! assets; the daemon is loopback-only and the page must work offline).

use axum::{
    extract::State,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use ox_broker::ClientHandle;
use ox_gate::UsageRecord;
use ox_path::oxpath;
use serde::Serialize;
use std::collections::BTreeMap;

pub fn router(client: ClientHandle) -> Router {
    Router::new()
        .route("/stats", get(get_stats))
        .route("/dashboard", get(get_dashboard))
        .with_state(client)
}

#[derive(Serialize, Default, Clone)]
struct Totals {
    requests: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
    /// Sum over records that carry a cost estimate; None when no record does.
    estimated_cost_usd: Option<f64>,
    /// How many records contributed to the cost sum — lets the UI say
    /// "$0.42 (3 of 90 priced)" instead of implying a complete figure.
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
}

#[derive(Serialize)]
struct ModelRow {
    account: String,
    model_id: String,
    #[serde(flatten)]
    totals: Totals,
}

#[derive(Serialize)]
struct HourBucket {
    hour_start_ms: u64,
    requests: u64,
    input_tokens: u64,
    output_tokens: u64,
}

#[derive(Serialize)]
struct Stats {
    generated_at_ms: u64,
    in_flight: u64,
    totals: Totals,
    today: Totals,
    by_model: Vec<ModelRow>,
    /// Last 24 hours in hour buckets, oldest first, empty hours included.
    by_hour: Vec<HourBucket>,
    /// Most recent records, newest first.
    recent: Vec<UsageRecord>,
}

const HOUR_MS: u64 = 3_600_000;
const RECENT_LIMIT: usize = 20;

async fn get_stats(State(client): State<ClientHandle>) -> impl IntoResponse {
    let records: Vec<UsageRecord> = client
        .read_typed(&oxpath!("gateway", "usage"))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();

    let in_flight = client
        .read(&oxpath!("gateway", "completions", "outstanding"))
        .await
        .ok()
        .flatten()
        .and_then(|rec| rec.as_value().cloned())
        .and_then(|v| match v {
            structfs_core_store::Value::Map(m) => m.get("items").cloned(),
            _ => None,
        })
        .map(|items| match items {
            structfs_core_store::Value::Array(a) => a.len() as u64,
            _ => 0,
        })
        .unwrap_or(0);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let start_of_today_ms = now_ms - (now_ms % 86_400_000);
    let window_start = (now_ms - 23 * HOUR_MS) - ((now_ms - 23 * HOUR_MS) % HOUR_MS);

    let mut totals = Totals::default();
    let mut today = Totals::default();
    let mut by_model: BTreeMap<(String, String), Totals> = BTreeMap::new();
    let mut by_hour: BTreeMap<u64, HourBucket> = BTreeMap::new();
    for i in 0..24 {
        let start = window_start + i * HOUR_MS;
        by_hour.insert(
            start,
            HourBucket { hour_start_ms: start, requests: 0, input_tokens: 0, output_tokens: 0 },
        );
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
            let bucket_start = r.completed_at_ms - (r.completed_at_ms % HOUR_MS);
            if let Some(b) = by_hour.get_mut(&bucket_start) {
                b.requests += 1;
                b.input_tokens += r.input_tokens as u64;
                b.output_tokens += r.output_tokens as u64;
            }
        }
    }

    let mut by_model: Vec<ModelRow> = by_model
        .into_iter()
        .map(|((account, model_id), totals)| ModelRow { account, model_id, totals })
        .collect();
    by_model.sort_by(|a, b| {
        (b.totals.input_tokens + b.totals.output_tokens)
            .cmp(&(a.totals.input_tokens + a.totals.output_tokens))
    });

    let mut recent = records;
    recent.sort_by(|a, b| b.completed_at_ms.cmp(&a.completed_at_ms));
    recent.truncate(RECENT_LIMIT);

    Json(Stats {
        generated_at_ms: now_ms,
        in_flight,
        totals,
        today,
        by_model,
        by_hour: by_hour.into_values().collect(),
        recent,
    })
}

async fn get_dashboard() -> impl IntoResponse {
    Html(include_str!("dashboard.html"))
}

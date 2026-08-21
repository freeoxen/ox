//! Axum routers for each inbound dialect.

pub mod anthropic;
pub mod models;
pub mod openai;
pub mod ox_native;

use axum::Router;
use ox_broker::ClientHandle;
use ox_gate::codec::ResponseMeta;
use std::sync::atomic::{AtomicU64, Ordering};

/// Mint the wire identity for one response. The id is unique per daemon
/// lifetime (unix-nanos plus a process-local counter, hex-encoded) — enough
/// for clients that key logs and caches on it, without pulling in a
/// randomness dependency for a loopback-only service.
pub(crate) fn response_meta(prefix: &str, model: &str) -> ResponseMeta {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    ResponseMeta {
        id: format!("{prefix}{:x}{:04x}", now.as_nanos() as u64, seq & 0xffff),
        model: model.to_string(),
        created: now.as_secs(),
    }
}

pub fn build_router(client: ClientHandle) -> Router {
    Router::new()
        .merge(anthropic::router(client.clone()))
        .merge(openai::router(client.clone()))
        .merge(models::router(client.clone()))
        .merge(ox_native::router(client))
}

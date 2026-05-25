//! Axum routers for each inbound dialect.

pub mod anthropic;
pub mod openai;

use axum::Router;
use ox_broker::ClientHandle;

pub fn build_router(client: ClientHandle) -> Router {
    Router::new()
        .merge(anthropic::router(client.clone()))
        .merge(openai::router(client))
    // models, ox_native join here in later tasks
}

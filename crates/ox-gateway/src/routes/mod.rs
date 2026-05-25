//! Axum routers for each inbound dialect.

pub mod anthropic;
pub mod models;
pub mod openai;
pub mod ox_native;

use axum::Router;
use ox_broker::ClientHandle;

pub fn build_router(client: ClientHandle) -> Router {
    Router::new()
        .merge(anthropic::router(client.clone()))
        .merge(openai::router(client.clone()))
        .merge(models::router(client.clone()))
        .merge(ox_native::router(client))
}

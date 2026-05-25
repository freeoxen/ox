//! Axum routers for each inbound dialect.
//!
//! `build_router` composes all dialect routers into one axum::Router.
//! Subsequent tasks add `openai`, `models`, `ox_native`.

pub mod anthropic;

use axum::Router;
use ox_broker::ClientHandle;

pub fn build_router(client: ClientHandle) -> Router {
    Router::new().merge(anthropic::router(client))
    // openai, models, ox_native routers join here in later tasks
}

//! Axum routers. The dialect endpoints are the dumb http-in edge over the
//! wire/ mount — all dialect logic runs in the wire Block. Everything else
//! — models, stats, dashboard, ox-native, count_tokens — is native edge:
//! host-side reads of substrate paths, not gateway logic.

pub mod models;
pub mod ox_native;
pub mod http_in;
pub mod stats;

use axum::Router;
use ox_broker::ClientHandle;

pub fn build_router(client: ClientHandle) -> Router {
    Router::new()
        .merge(http_in::router(client.clone()))
        .route(
            "/v1/messages/count_tokens",
            axum::routing::post(|| async {
                crate::error::anthropic_error(
                    axum::http::StatusCode::NOT_IMPLEMENTED,
                    "count_tokens not yet implemented",
                )
            }),
        )
        .merge(models::router(client.clone()))
        .merge(stats::router(client.clone()))
        .merge(ox_native::router(client))
}

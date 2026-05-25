//! Anthropic Messages API routes — `POST /v1/messages` (streaming + non-streaming)
//! and `POST /v1/messages/count_tokens` (501 stub; deferred per spec).

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use ox_broker::ClientHandle;
use ox_gate::codec::anthropic as codec;
use ox_gate::codec::CodecError;
use ox_gate::completion_broker::CompletionStatus;
use serde_json::Value;
use structfs_core_store::path;

use crate::error::anthropic_error;
use crate::handle;

pub fn router(client: ClientHandle) -> Router {
    Router::new()
        .route("/v1/messages", post(post_messages))
        .route("/v1/messages/count_tokens", post(post_count_tokens))
        .with_state(client)
}

async fn post_messages(
    State(client): State<ClientHandle>,
    Json(body): Json<Value>,
) -> Response {
    let req = match codec::decode_request(&body) {
        Ok(r) => r,
        Err(e) => return anthropic_error(StatusCode::BAD_REQUEST, codec_error_message(&e)).into_response(),
    };
    let streaming = req.stream;

    let handle_path = match client.write_typed(&path!("gateway/completions"), &req).await {
        Ok(p) => p,
        Err(e) => return anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    if streaming {
        handle::stream_response(client, handle_path, "anthropic".into()).into_response()
    } else {
        match handle::buffer_response(client, handle_path).await {
            Ok((CompletionStatus::Complete { .. }, events)) => {
                Json(codec::encode_response(&events)).into_response()
            }
            Ok((CompletionStatus::Failed { reason, .. }, _)) => {
                anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, reason).into_response()
            }
            Ok(_) => anthropic_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "unexpected non-terminal status after drain",
            )
            .into_response(),
            Err(e) => anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        }
    }
}

async fn post_count_tokens(
    State(_client): State<ClientHandle>,
    Json(_body): Json<Value>,
) -> Response {
    // v1 stub: count_tokens is documented in the spec as out-of-scope for v1.
    // Reach back into this when there's a reason to forward to upstream.
    anthropic_error(StatusCode::NOT_IMPLEMENTED, "count_tokens not yet implemented")
        .into_response()
}

fn codec_error_message(e: &CodecError) -> String {
    e.to_string()
}

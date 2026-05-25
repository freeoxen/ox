//! OpenAI Chat Completions route — `POST /v1/chat/completions` (streaming + non-streaming).

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use ox_broker::ClientHandle;
use ox_gate::codec::openai as codec;
use ox_gate::codec::CodecError;
use ox_gate::completion_broker::CompletionStatus;
use serde_json::Value;
use structfs_core_store::path;

use crate::error::openai_error;
use crate::handle;

pub fn router(client: ClientHandle) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(post_chat_completions))
        .with_state(client)
}

async fn post_chat_completions(
    State(client): State<ClientHandle>,
    Json(body): Json<Value>,
) -> Response {
    let req = match codec::decode_request(&body) {
        Ok(r) => r,
        Err(e) => {
            return openai_error(
                StatusCode::BAD_REQUEST,
                codec_error_message(&e),
                None,
            )
            .into_response()
        }
    };
    let streaming = req.stream;

    let handle_path = match client.write_typed(&path!("gateway/completions"), &req).await {
        Ok(p) => p,
        Err(e) => {
            return openai_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                e.to_string(),
                None,
            )
            .into_response()
        }
    };

    if streaming {
        handle::stream_response(client, handle_path, "openai".into()).into_response()
    } else {
        match handle::buffer_response(client, handle_path).await {
            Ok((CompletionStatus::Complete { .. }, events)) => {
                Json(codec::encode_response(&events)).into_response()
            }
            Ok((CompletionStatus::Failed { reason, .. }, _)) => {
                openai_error(StatusCode::INTERNAL_SERVER_ERROR, reason, None).into_response()
            }
            Ok(_) => openai_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "unexpected non-terminal status after drain",
                None,
            )
            .into_response(),
            Err(e) => openai_error(StatusCode::INTERNAL_SERVER_ERROR, e, None).into_response(),
        }
    }
}

fn codec_error_message(e: &CodecError) -> String {
    e.to_string()
}

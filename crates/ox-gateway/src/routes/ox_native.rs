//! ox-native completion route — `POST /completions`.
//!
//! Raw CompletionRequest in, raw StreamEvent stream out. No dialect
//! translation: clients that already speak ox-types use this directly.

use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use bytes::Bytes;
use futures::stream::Stream;
use ox_broker::ClientHandle;
use ox_gate::completion_broker::CompletionStatus;
use ox_kernel::CompletionRequest;
use ox_types::StreamEvent;
use std::convert::Infallible;
use structfs_core_store::path;

use crate::handle;

pub fn router(client: ClientHandle) -> Router {
    Router::new()
        .route("/completions", post(post_completions))
        .with_state(client)
}

async fn post_completions(
    State(client): State<ClientHandle>,
    Json(mut req): Json<CompletionRequest>,
) -> Response {
    let streaming = req.stream;
    // Same provenance stamp the wire Block applies: usage records name
    // the inbound dialect, and this route is the "ox" dialect.
    req.extra
        .insert("ox_inbound_dialect".into(), serde_json::json!("ox"));
    let record = match structfs_serde_store::to_value(&req) {
        Ok(value) => structfs_core_store::Record::parsed(value),
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
    };
    let gc = match handle::InflightGc::open(client.clone(), path!("gateway/completions"), record)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };
    let handle_path = gc.path().clone();

    if streaming {
        ox_native_sse_response(client, handle_path, gc).into_response()
    } else {
        match handle::buffer_response(client, handle_path).await {
            Ok((status, events)) => Json(serde_json::json!({
                "status": status,
                "events": events,
            }))
            .into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        }
    }
}

/// SSE response that yields one frame per StreamEvent, JSON-encoded.
/// No dialect translation; uses StreamEvent's own serde serialization.
fn ox_native_sse_response(
    client: ClientHandle,
    handle_path: structfs_core_store::Path,
    gc: handle::InflightGc,
) -> Response {
    let stream = ox_native_sse_stream(client, handle_path, gc);
    let body = axum::body::Body::from_stream(stream);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .expect("valid response builder inputs")
}

fn ox_native_sse_stream(
    client: ClientHandle,
    handle_path: structfs_core_store::Path,
    gc: handle::InflightGc,
) -> impl Stream<Item = Result<Bytes, Infallible>> + Send + 'static {
    async_stream::stream! {
        // Same Drop-guard as the dialect routes: axum drops this stream on
        // client disconnect, so GC must not rely on code after the loop.
        let gc = gc;
        let mut next: usize = 0;
        loop {
            let events_path = handle_path.join(&handle::events_from_subpath(next));
            let events: Vec<StreamEvent> = client
                .read_typed(&events_path)
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            for ev in &events {
                let json = serde_json::to_string(ev).unwrap_or_default();
                yield Ok::<_, Infallible>(Bytes::from(format!("data: {}\n\n", json)));
            }
            next += events.len();

            let status: Option<CompletionStatus> = client.read_typed(&handle_path).await.ok().flatten();
            let Some(status) = status else { break };
            if !status.is_terminal() {
                continue;
            }
            // Close-out drain, same as handle.rs: events appended between the
            // events-read and the status-read would otherwise be dropped.
            let events_path = handle_path.join(&handle::events_from_subpath(next));
            let tail: Vec<StreamEvent> = client
                .read_typed(&events_path)
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            for ev in &tail {
                let json = serde_json::to_string(ev).unwrap_or_default();
                yield Ok::<_, Infallible>(Bytes::from(format!("data: {}\n\n", json)));
            }
            if let CompletionStatus::Failed { reason, .. } = status {
                let frame = format!(
                    "event: error\ndata: {}\n\n",
                    serde_json::json!({ "message": reason })
                );
                yield Ok(Bytes::from(frame));
            }
            break;
        }
        gc.gc_now().await;
    }
}

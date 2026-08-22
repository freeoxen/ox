//! http-in — the dumb HTTP⇄paths edge (Isotope phase 4).
//!
//! These handlers contain no dialect knowledge: they write the inbound
//! wire body to the wire/ mount, blocking-read the head the wire Block
//! writes, and shuttle the response back — a JSON body verbatim, or a
//! frame stream drained from the wire handle. All decode/encode/error
//! shaping happens inside the Block.

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use bytes::Bytes;
use ox_broker::ClientHandle;
use serde_json::Value;
use structfs_core_store::{path, Path, Record};

use crate::handle::InflightGc;

pub fn router(client: ClientHandle) -> Router {
    Router::new()
        .route("/v1/messages", post(anthropic_edge))
        .route("/v1/chat/completions", post(openai_edge))
        .with_state(client)
}

async fn anthropic_edge(State(client): State<ClientHandle>, Json(body): Json<Value>) -> Response {
    edge(client, "anthropic", body).await
}

async fn openai_edge(State(client): State<ClientHandle>, Json(body): Json<Value>) -> Response {
    edge(client, "openai", body).await
}

async fn edge(client: ClientHandle, dialect: &'static str, body: Value) -> Response {
    let inbound = structfs_serde_store::json_to_value(serde_json::json!({
        "dialect": dialect,
        "body": body,
    }));
    let rel = match client.write(&path!("wire"), Record::parsed(inbound)).await {
        Ok(p) => p,
        Err(e) => {
            // The one error the edge must shape itself: the wire mount was
            // unreachable, so no Block ever saw the request.
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": {"message": e.to_string()}})),
            )
                .into_response();
        }
    };
    let handle = path!("wire").join(&rel);

    let head: Value = match client.read(&handle.join(&path!("head"))).await {
        Ok(Some(rec)) => rec
            .as_value()
            .cloned()
            .map(structfs_serde_store::value_to_json)
            .unwrap_or_default(),
        other => {
            let _ = client
                .write(&handle, Record::parsed(structfs_core_store::Value::Null))
                .await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": {"message": format!("wire head read failed: {other:?}")}})),
            )
                .into_response();
        }
    };

    match head["mode"].as_str() {
        Some("json") | Some("error") => {
            let status = StatusCode::from_u16(head["status"].as_u64().unwrap_or(200) as u16)
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let body = head["body"].clone();
            let _ = client
                .write(&handle, Record::parsed(structfs_core_store::Value::Null))
                .await;
            (status, Json(body)).into_response()
        }
        Some("stream") => stream_frames(client, handle),
        other => {
            let _ = client
                .write(&handle, Record::parsed(structfs_core_store::Value::Null))
                .await;
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": {"message": format!("unknown wire head mode: {other:?}")}})),
            )
                .into_response()
        }
    }
}

/// Drain wire frames into the SSE response body. The frames are already
/// complete wire SSE blocks — the edge writes them verbatim. The GC guard
/// covers client disconnects, same as the completion drains.
fn stream_frames(client: ClientHandle, handle: Path) -> Response {
    let stream = async_stream::stream! {
        let gc = InflightGc::new(client.clone(), handle.clone());
        let mut next = 0usize;
        loop {
            let sub = Path::parse(&format!("frames/from/{next}"))
                .expect("frames/from/{n} components are valid");
            let frames: Vec<String> = match client.read_typed(&handle.join(&sub)).await {
                Ok(Some(v)) => v,
                Ok(None) => break,
                Err(_) => break,
            };
            for f in &frames {
                yield Ok::<_, std::convert::Infallible>(Bytes::from(f.clone()));
            }
            next += frames.len();
            let done: bool = client
                .read_typed(&handle.join(&path!("done")))
                .await
                .ok()
                .flatten()
                .unwrap_or(false);
            if done {
                // Close-out drain for frames racing the done flag.
                let sub = Path::parse(&format!("frames/from/{next}")).expect("valid path");
                if let Ok(Some(tail)) = client.read_typed::<Vec<String>>(&handle.join(&sub)).await {
                    for f in &tail {
                        yield Ok(Bytes::from(f.clone()));
                    }
                }
                break;
            }
        }
        gc.gc_now().await;
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"))
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
        .body(Body::from_stream(stream))
        .expect("building SSE response is infallible")
}

//! Shared drain helpers for streaming and non-streaming gateway routes.
//!
//! Both helpers consume the `outstanding/{N}/events/from/{S}` blocking
//! read on `gateway/completions`. The stream variant yields encoded SSE
//! frames as bytes arrive; the buffer variant accumulates events until
//! terminal, then hands the full vec back to the caller for non-streaming
//! response encoding.
//!
//! The `SseEncoder` returns already-formatted wire SSE strings (e.g.
//! `"event: content_block_start\ndata: {...}\n\n"`), so `stream_response`
//! writes them directly into the response body rather than wrapping them
//! in axum's `Event` abstraction, which would add an extra `data:` prefix.

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use bytes::Bytes;
use futures::stream::Stream;
use ox_broker::ClientHandle;
use ox_gate::codec::{ResponseMeta, SseEncoder};
use ox_gate::completion_broker::CompletionStatus;
use ox_types::StreamEvent;
use structfs_core_store::{Path, Record, Value};

/// GC's the inflight handle even when the drain future never runs to
/// completion. axum drops the response stream (and cancels the buffered
/// handler) the moment the client disconnects, so a GC write placed after
/// the drain loop would never execute — every abandoned request would leak
/// its inflight entry (status + full event buffer) forever. The guard's
/// Drop spawns the `write(Null)` instead; the normal path disarms it after
/// GC'ing inline.
struct InflightGc {
    client: ClientHandle,
    handle: Path,
    armed: bool,
}

impl InflightGc {
    fn new(client: ClientHandle, handle: Path) -> Self {
        Self { client, handle, armed: true }
    }

    async fn gc_now(mut self) {
        self.armed = false;
        let _ = self.client.write(&self.handle, Record::parsed(Value::Null)).await;
    }
}

impl Drop for InflightGc {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let client = self.client.clone();
        let handle = self.handle.clone();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = client.write(&handle, Record::parsed(Value::Null)).await;
            });
        }
    }
}

/// Stream events from `handle` as raw SSE bytes.
///
/// `handle` is the broker-relative path returned by
/// `client.write_typed(&path!("gateway/completions"), &req)` — i.e. the
/// full `gateway/completions/outstanding/{N}` prefix.
///
/// Each `StreamEvent` is encoded via `SseEncoder::new(&dialect)` so the
/// wire shape matches the inbound API. On terminal status the encoder's
/// `finish()` frames are flushed, the handle is GC'd with `write(Null)`,
/// and the stream closes.
pub fn stream_response(
    client: ClientHandle,
    handle: Path,
    dialect: String,
    meta: ResponseMeta,
) -> Response {
    let raw_stream = raw_sse_stream(client, handle, dialect, meta);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"))
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
        .body(Body::from_stream(raw_stream))
        .expect("building SSE response is infallible")
}

fn raw_sse_stream(
    client: ClientHandle,
    handle: Path,
    dialect: String,
    meta: ResponseMeta,
) -> impl Stream<Item = Result<Bytes, std::convert::Infallible>> {
    async_stream::stream! {
        let gc = InflightGc::new(client.clone(), handle.clone());
        let mut encoder = SseEncoder::new(&dialect, meta);
        let mut next: usize = 0;
        loop {
            let events_path = handle.join(&events_from_subpath(next));
            let events: Vec<StreamEvent> = match client.read_typed(&events_path).await {
                Ok(Some(v)) => v,
                Ok(None) => Vec::new(),
                Err(e) => {
                    let ev = StreamEvent::Error { message: e.to_string() };
                    for frame in encoder.encode_sse(&ev) {
                        yield Ok(Bytes::from(frame));
                    }
                    break;
                }
            };

            for ev in &events {
                for frame in encoder.encode_sse(ev) {
                    yield Ok(Bytes::from(frame));
                }
            }
            next += events.len();

            let status: Option<CompletionStatus> = client.read_typed(&handle).await.ok().flatten();
            let Some(status) = status else { continue };
            if !status.is_terminal() {
                continue;
            }
            // Events appended between the events-read above and this status
            // read would otherwise be dropped. No events land after the
            // terminal flip, so one more drain gets everything.
            let events_path = handle.join(&events_from_subpath(next));
            let tail: Vec<StreamEvent> =
                client.read_typed(&events_path).await.ok().flatten().unwrap_or_default();
            for ev in &tail {
                for frame in encoder.encode_sse(ev) {
                    yield Ok(Bytes::from(frame));
                }
            }
            match status {
                CompletionStatus::Failed { reason, .. } => {
                    let ev = StreamEvent::Error { message: reason };
                    for frame in encoder.encode_sse(&ev) {
                        yield Ok(Bytes::from(frame));
                    }
                }
                _ => {
                    for frame in encoder.finish() {
                        yield Ok(Bytes::from(frame));
                    }
                }
            }
            break;
        }
        gc.gc_now().await;
    }
}

/// Non-streaming drain. Blocks until a terminal `CompletionStatus` is
/// observed, accumulating all `StreamEvent`s along the way.
///
/// Returns `(status, events)`. The caller encodes the full event buffer
/// via `codec::*::encode_response` for the appropriate dialect.
///
/// GC's the handle (writes `Null`) before returning.
pub async fn buffer_response(
    client: ClientHandle,
    handle: Path,
) -> Result<(CompletionStatus, Vec<StreamEvent>), String> {
    let gc = InflightGc::new(client.clone(), handle.clone());
    let mut next: usize = 0;
    let mut all: Vec<StreamEvent> = Vec::new();
    loop {
        let events_path = handle.join(&events_from_subpath(next));
        let events: Vec<StreamEvent> = client
            .read_typed(&events_path)
            .await
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        next += events.len();
        all.extend(events);

        let status: CompletionStatus = client
            .read_typed(&handle)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "inflight vanished mid-drain".to_string())?;
        if status.is_terminal() {
            // Same close-out drain as the streaming path: pick up events that
            // landed between the events-read and the status-read.
            let events_path = handle.join(&events_from_subpath(next));
            let tail: Vec<StreamEvent> = client
                .read_typed(&events_path)
                .await
                .map_err(|e| e.to_string())?
                .unwrap_or_default();
            all.extend(tail);
            gc.gc_now().await;
            return Ok((status, all));
        }
    }
}

/// Build the `events/from/{seq}` path segment that hangs off a handle.
///
/// Numeric strings are valid `PathComponent`s (pure-digit rule in
/// `structfs_core_store::Path`), so we can form this at runtime.
pub(crate) fn events_from_subpath(seq: usize) -> Path {
    Path::try_from_components(vec![
        "events".to_string(),
        "from".to_string(),
        seq.to_string(),
    ])
    .expect("events/from/{seq} components are valid PathComponents")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_from_subpath_produces_three_components() {
        let p = events_from_subpath(42);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].as_str(), "events");
        assert_eq!(p[1].as_str(), "from");
        assert_eq!(p[2].as_str(), "42");
    }

    #[test]
    fn events_from_subpath_join_with_handle_makes_full_path() {
        // Simulate what stream_response does: handle = "gateway/completions/outstanding/0"
        let handle = Path::parse("gateway/completions/outstanding/0").unwrap();
        let full = handle.join(&events_from_subpath(7));
        assert_eq!(full.to_string(), "gateway/completions/outstanding/0/events/from/7");
    }
}

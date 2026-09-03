//! Drain helpers over `gateway/completions` handles: the disconnect-safe
//! GC guard and the non-streaming accumulate-to-terminal drain, shared by
//! the ox-native routes and the http-in edge.

use ox_broker::ClientHandle;
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
pub struct InflightGc {
    client: ClientHandle,
    handle: Path,
    armed: bool,
}

impl InflightGc {
    pub fn new(client: ClientHandle, handle: Path) -> Self {
        Self {
            client,
            handle,
            armed: true,
        }
    }

    pub async fn gc_now(mut self) {
        self.armed = false;
        let _ = self
            .client
            .write(&self.handle, Record::parsed(Value::Null))
            .await;
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
        // handle = "gateway/completions/outstanding/0", as the drains build it
        let handle = Path::parse("gateway/completions/outstanding/0").unwrap();
        let full = handle.join(&events_from_subpath(7));
        assert_eq!(
            full.to_string(),
            "gateway/completions/outstanding/0/events/from/7"
        );
    }
}

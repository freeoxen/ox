//! UpstreamStore — the SSE executor behind a StructFS mount.
//!
//! Isotope phase 2: the completion broker stops holding the executor and
//! instead writes upstream requests to this mount and drains events with
//! blocking reads. The broker's namespace becomes pure paths, which is the
//! precondition for running it as a Wasm Block (phase 3) — a Block can
//! read `upstream/outstanding/{n}/events/from/{s}` but cannot own a socket.
//!
//! Path layout (mirrors the completion broker's own conventions):
//!   write /                                {dialect, request: HttpRequest} → outstanding/{n}
//!   read  outstanding/{n}                  status: {state: streaming|complete|failed, reason?}
//!   read  outstanding/{n}/events/from/{s}  Vec<StreamEvent> — BLOCKING
//!   write outstanding/{n} null             GC

use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use ox_broker::async_store::BoxFuture;
use serde::{Deserialize, Serialize};
use structfs_core_store::{
    DetachedReader, DetachedWriter, Error as StoreError, Path, Record, Value,
};
use structfs_http::types::HttpRequest;
use tokio::runtime::Handle as TokioHandle;
use tokio::sync::{Mutex, Notify};

use crate::transport::SseHttpExecutor;
use ox_types::StreamEvent;

/// The write payload: which dialect parser to run and the request to send.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamRequest {
    pub dialect: String,
    pub request: HttpRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum UpstreamStatus {
    Streaming,
    Complete,
    Failed { reason: String },
}

impl UpstreamStatus {
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Streaming)
    }
}

struct UpstreamInflight {
    state: Mutex<UpstreamState>,
    notify: Notify,
}

struct UpstreamState {
    events: Vec<StreamEvent>,
    status: UpstreamStatus,
}

pub struct UpstreamStore<E: SseHttpExecutor> {
    executor: Arc<E>,
    handles: HashMap<u64, Arc<UpstreamInflight>>,
    tasks: HashMap<u64, tokio::task::JoinHandle<()>>,
    next_id: u64,
    runtime: TokioHandle,
}

impl<E: SseHttpExecutor> Drop for UpstreamStore<E> {
    fn drop(&mut self) {
        for task in self.tasks.values() {
            task.abort();
        }
    }
}

impl<E: SseHttpExecutor> UpstreamStore<E> {
    pub fn new(executor: Arc<E>, runtime: TokioHandle) -> Self {
        Self {
            executor,
            handles: HashMap::new(),
            tasks: HashMap::new(),
            next_id: 0,
            runtime,
        }
    }

    fn parse_handle_path(path: &Path) -> Option<(u64, Option<String>)> {
        if path.len() < 2 || &path[0] != "outstanding" {
            return None;
        }
        let id: u64 = path[1].parse().ok()?;
        let sub = if path.len() > 2 {
            Some(
                (2..path.len())
                    .map(|i| &path[i])
                    .collect::<Vec<_>>()
                    .join("/"),
            )
        } else {
            None
        };
        Some((id, sub))
    }
}

impl<E: SseHttpExecutor> DetachedReader for UpstreamStore<E> {
    fn read_detached(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let Some((id, sub)) = Self::parse_handle_path(from) else {
            return Box::pin(async move { Ok(None) });
        };
        let inflight = match self.handles.get(&id) {
            Some(arc) => arc.clone(),
            None => return Box::pin(async move { Ok(None) }),
        };

        Box::pin(async move {
            match sub.as_deref() {
                None => {
                    let state = inflight.state.lock().await;
                    let value = structfs_serde_store::to_value(&state.status)
                        .map_err(|e| StoreError::store("upstream", "read", e.to_string()))?;
                    Ok(Some(Record::parsed(value)))
                }
                // Blocking drain. The Notified future is created and enabled
                // BEFORE the state check — the producer signals with
                // notify_waiters (no permit), so check-then-await would lose
                // a notification landing between the lock drop and the first
                // poll and park forever.
                Some(s) if s.starts_with("events/from/") => {
                    let seq: usize = s.trim_start_matches("events/from/").parse().map_err(
                        |e: std::num::ParseIntError| {
                            StoreError::store("upstream", "read", e.to_string())
                        },
                    )?;
                    loop {
                        let notified = inflight.notify.notified();
                        tokio::pin!(notified);
                        notified.as_mut().enable();
                        {
                            let state = inflight.state.lock().await;
                            if state.events.len() > seq || state.status.is_terminal() {
                                let start = seq.min(state.events.len());
                                let tail = state.events[start..].to_vec();
                                let value = structfs_serde_store::to_value(&tail).map_err(|e| {
                                    StoreError::store("upstream", "read", e.to_string())
                                })?;
                                return Ok(Some(Record::parsed(value)));
                            }
                        }
                        notified.await;
                    }
                }
                _ => Ok(None),
            }
        })
    }
}

impl<E: SseHttpExecutor> DetachedWriter for UpstreamStore<E> {
    fn write_detached(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
        let to = to.clone();

        // GC
        if let Some((id, None)) = Self::parse_handle_path(&to) {
            if matches!(data.as_value(), Some(Value::Null)) {
                let inflight = self.handles.remove(&id);
                let task = self.tasks.remove(&id);
                if let Some(task) = &task {
                    task.abort();
                }
                return Box::pin(async move {
                    let join_error = match task {
                        Some(task) => task.await.err().filter(|error| !error.is_cancelled()),
                        None => None,
                    };
                    if let Some(inflight) = inflight {
                        inflight.state.lock().await.status = UpstreamStatus::Failed {
                            reason: "upstream handle cancelled".into(),
                        };
                        inflight.notify.notify_waiters();
                    }
                    match join_error {
                        Some(error) => Err(StoreError::store("upstream", "gc", error.to_string())),
                        None => Ok(to),
                    }
                });
            }
            return Box::pin(async move {
                Err(StoreError::store(
                    "upstream",
                    "write",
                    "cannot overwrite an outstanding handle; write null to delete",
                ))
            });
        }

        if !to.is_empty() {
            return Box::pin(async move {
                Err(StoreError::store(
                    "upstream",
                    "write",
                    "write an UpstreamRequest to the root",
                ))
            });
        }

        let value = match data.as_value() {
            Some(v) => v.clone(),
            None => {
                return Box::pin(async move {
                    Err(StoreError::store(
                        "upstream",
                        "write",
                        "expected parsed record",
                    ))
                });
            }
        };
        let req: UpstreamRequest = match structfs_serde_store::from_value(value) {
            Ok(r) => r,
            Err(e) => {
                return Box::pin(async move {
                    Err(StoreError::store(
                        "upstream",
                        "write",
                        format!("invalid UpstreamRequest: {e}"),
                    ))
                });
            }
        };

        let id = self.next_id;
        self.next_id += 1;
        let inflight = Arc::new(UpstreamInflight {
            state: Mutex::new(UpstreamState {
                events: Vec::new(),
                status: UpstreamStatus::Streaming,
            }),
            notify: Notify::new(),
        });
        self.handles.insert(id, inflight.clone());

        let executor = self.executor.clone();
        let task = self.runtime.spawn(async move {
            let mut stream = executor.execute(req.request, req.dialect).await;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(ev) => {
                        let mut state = inflight.state.lock().await;
                        state.events.push(ev);
                        drop(state);
                        inflight.notify.notify_waiters();
                    }
                    Err(reason) => {
                        let mut state = inflight.state.lock().await;
                        state.status = UpstreamStatus::Failed { reason };
                        drop(state);
                        inflight.notify.notify_waiters();
                        return;
                    }
                }
            }
            let mut state = inflight.state.lock().await;
            state.status = UpstreamStatus::Complete;
            drop(state);
            inflight.notify.notify_waiters();
        });
        self.tasks.insert(id, task);

        Box::pin(async move {
            Path::try_from_components(vec!["outstanding".to_string(), id.to_string()])
                .map_err(|e| StoreError::store("upstream", "write", e.to_string()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion_broker::mock::MockSseExecutor;
    use ox_broker::BrokerStore;
    use std::time::Duration;
    use structfs_core_store::path;

    async fn mount(executor: Arc<MockSseExecutor>) -> ox_broker::ClientHandle {
        let broker = BrokerStore::new(Duration::from_secs(5));
        let store = UpstreamStore::new(executor, tokio::runtime::Handle::current());
        broker.mount_async(path!("upstream"), store).await;
        // Keep the broker alive for the test's duration by leaking its
        // client-side handle scope; the returned handle owns the channels.
        broker.client()
    }

    fn request() -> UpstreamRequest {
        UpstreamRequest {
            dialect: "anthropic".into(),
            request: HttpRequest::post("https://example.test/v1/messages"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drains_events_to_terminal() {
        let executor = Arc::new(MockSseExecutor::new());
        executor.push_immediate(StreamEvent::TextDelta { text: "a".into() });
        executor.push_immediate(StreamEvent::TextDelta { text: "b".into() });
        let client = mount(executor).await;

        let handle = client
            .write_typed(&path!("upstream"), &request())
            .await
            .expect("write");
        let handle = path!("upstream").join(&handle);

        let mut next = 0usize;
        let mut texts = Vec::new();
        loop {
            let sub = Path::parse(&format!("events/from/{next}")).unwrap();
            let events: Vec<StreamEvent> = client
                .read_typed(&handle.join(&sub))
                .await
                .expect("drain read")
                .unwrap_or_default();
            next += events.len();
            for ev in events {
                if let StreamEvent::TextDelta { text } = ev {
                    texts.push(text);
                }
            }
            let status: UpstreamStatus = client
                .read_typed(&handle)
                .await
                .expect("status read")
                .expect("status present");
            if status.is_terminal() {
                assert_eq!(status, UpstreamStatus::Complete);
                break;
            }
        }
        assert_eq!(texts, vec!["a".to_string(), "b".to_string()]);

        // GC removes the handle.
        client
            .write(&handle, Record::parsed(Value::Null))
            .await
            .expect("gc");
        let gone: Option<UpstreamStatus> = client.read_typed(&handle).await.expect("read");
        assert!(gone.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn transport_error_flips_failed() {
        let executor = Arc::new(MockSseExecutor::new());
        executor.push_immediate(StreamEvent::TextDelta { text: "x".into() });
        executor.push_error("boom");
        let client = mount(executor).await;

        let handle = client
            .write_typed(&path!("upstream"), &request())
            .await
            .expect("write");
        let handle = path!("upstream").join(&handle);

        // Drain until terminal.
        let status = loop {
            let status: UpstreamStatus = client
                .read_typed(&handle)
                .await
                .expect("status")
                .expect("present");
            if status.is_terminal() {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(
            status,
            UpstreamStatus::Failed {
                reason: "boom".into()
            }
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn out_of_range_seq_reads_empty_after_terminal() {
        let executor = Arc::new(MockSseExecutor::new());
        executor.push_immediate(StreamEvent::MessageStop);
        let client = mount(executor).await;

        let handle = client
            .write_typed(&path!("upstream"), &request())
            .await
            .expect("write");
        let handle = path!("upstream").join(&handle);

        // Wait for terminal, then read a stale cursor far past the end.
        loop {
            let status: UpstreamStatus = client.read_typed(&handle).await.unwrap().unwrap();
            if status.is_terminal() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let sub = Path::parse("events/from/999").unwrap();
        let tail: Vec<StreamEvent> = client
            .read_typed(&handle.join(&sub))
            .await
            .expect("clamped read")
            .unwrap_or_default();
        assert!(tail.is_empty());
    }
}

#[cfg(test)]
mod cancellation_regressions {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use structfs_core_store::path;

    struct PendingExecutor {
        entered: Arc<Notify>,
        dropped: Arc<AtomicBool>,
    }
    struct DropProbe(Arc<AtomicBool>);
    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    #[async_trait::async_trait]
    impl SseHttpExecutor for PendingExecutor {
        async fn execute(
            &self,
            _: HttpRequest,
            _: String,
        ) -> futures::stream::BoxStream<'static, Result<StreamEvent, String>> {
            let entered = self.entered.clone();
            let probe = DropProbe(self.dropped.clone());
            Box::pin(futures::stream::once(async move {
                let _probe = probe;
                entered.notify_one();
                std::future::pending().await
            }))
        }
    }
    #[tokio::test]
    async fn gc_joins_upstream_producer_and_wakes_parked_read() {
        let entered = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let executor = Arc::new(PendingExecutor {
            entered: entered.clone(),
            dropped: dropped.clone(),
        });
        let mut store = UpstreamStore::new(executor, TokioHandle::current());
        let req = UpstreamRequest {
            dialect: "anthropic".into(),
            request: HttpRequest::post("https://example.test"),
        };
        let handle = store
            .write_detached(
                &path!(""),
                Record::parsed(structfs_serde_store::to_value(&req).unwrap()),
            )
            .await
            .unwrap();
        entered.notified().await;
        let parked = store.read_detached(&handle.join(&path!("events/from/0")));
        store
            .write_detached(&handle, Record::parsed(Value::Null))
            .await
            .unwrap();
        assert!(
            dropped.load(Ordering::SeqCst),
            "GC must join producer, not just remove its lookup entry"
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(1), parked)
                .await
                .unwrap()
                .unwrap()
                .is_some()
        );
    }
}

//! `CompletionBrokerStore` — substrate-mediated LLM completion dispatch,
//! modeled on structfs_http::HttpBrokerStore but generalized for streaming.
//!
//! Path layout (mirrors HttpBrokerStore's `outstanding/{N}` convention):
//!   write /                                CompletionRequest → outstanding/{N}
//!   read  outstanding/{N}                  CompletionStatus
//!   read  outstanding/{N}/request          original CompletionRequest
//!   read  outstanding/{N}/events/from/{S}  Vec<StreamEvent> — BLOCKING
//!   read  outstanding/{N}/events/count     usize — non-blocking buffer length
//!   read  outstanding/{N}/usage            UsageInfo (None until Complete)
//!   write outstanding/{N} null             GC

mod cancel;
mod inflight;

#[cfg(any(test, feature = "test-utils"))]
pub mod mock;

pub use cancel::CancelHandle;
pub use inflight::CompletionStatus;
#[allow(unused_imports)]
pub(crate) use inflight::{Inflight, InflightState};

use std::collections::HashMap;
use std::sync::Arc;

use ox_broker::async_store::{AsyncReader, AsyncWriter, BoxFuture};
use structfs_core_store::{Error as StoreError, Path, Record, Value};
use tokio::runtime::Handle as TokioHandle;

// Used in the AsyncWriter impl for deserializing the inbound record.
#[allow(unused_imports)]
use structfs_serde_store;

pub type RequestId = u64;

/// Streaming completion broker: the substrate store holding per-request
/// state (queue, events, status, usage). All completion *logic* lives in
/// the broker Block the runner spawns; this store is only the mechanics.
pub struct CompletionBrokerStore {
    /// In-memory in-flight tracker. Per-request state has its own Notify.
    /// No outer Mutex needed — AsyncReader/AsyncWriter give us &mut self.
    pub(crate) handles: HashMap<RequestId, Arc<Inflight>>,

    /// Cancellation for each request's Block run; GC triggers it so a
    /// Block parked on a blocking read unwinds instead of leaking.
    pub(crate) cancels: HashMap<RequestId, CancelHandle>,

    pub(crate) next_request_id: RequestId,

    /// Tokio handle for spawning per-request Block runs.
    pub(crate) runtime: TokioHandle,

    /// Block runner: the store hands each new request's inflight id to
    /// this callback, which runs the broker Block (wasm) against the
    /// substrate. Injected by the binary because the wasm artifact and
    /// its harness live there, not in ox-gate.
    pub(crate) runner: BlockRunner,
}

/// Per-request Block entry point: (inflight id, cancellation for the run).
pub type BlockRunner = Arc<dyn Fn(RequestId, CancelHandle) + Send + Sync>;

impl CompletionBrokerStore {
    pub fn new(runtime: TokioHandle, runner: BlockRunner) -> Self {
        Self {
            handles: HashMap::new(),
            cancels: HashMap::new(),
            next_request_id: 0,
            runtime,
            runner,
        }
    }

    /// Parse `outstanding/{id}[/sub/...]` path. Returns the request id
    /// and any sub-path as a `/`-joined string.
    pub(crate) fn parse_handle_path(path: &Path) -> Option<(RequestId, Option<String>)> {
        if path.is_empty() || path[0].as_str() != "outstanding" {
            return None;
        }
        if path.len() == 1 {
            return None;
        }
        let id: RequestId = path[1].as_str().parse().ok()?;
        let sub = if path.len() > 2 {
            Some(
                (2..path.len())
                    .map(|i| path[i].as_str())
                    .collect::<Vec<_>>()
                    .join("/"),
            )
        } else {
            None
        };
        Some((id, sub))
    }
}

impl AsyncReader for CompletionBrokerStore {
    fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        // Root descriptor map.
        if from.is_empty() {
            let mut map = std::collections::BTreeMap::new();
            map.insert(
                "outstanding".to_string(),
                Value::String("outstanding".into()),
            );
            map.insert("docs".to_string(), Value::String("docs".into()));
            return Box::pin(async move { Ok(Some(Record::parsed(Value::Map(map)))) });
        }

        // /docs
        if from.len() == 1 && from[0].as_str() == "docs" {
            return Box::pin(async move { Ok(Some(Record::parsed(docs_value()))) });
        }

        // /outstanding listing
        if from.len() == 1 && from[0].as_str() == "outstanding" {
            let items: Vec<Value> = self
                .handles
                .keys()
                .map(|id| Value::String(format!("outstanding/{id}")))
                .collect();
            let mut map = std::collections::BTreeMap::new();
            map.insert("items".to_string(), Value::Array(items));
            return Box::pin(async move { Ok(Some(Record::parsed(Value::Map(map)))) });
        }

        // /outstanding/{N}[/sub]
        let (id, sub) = match Self::parse_handle_path(from) {
            Some(t) => t,
            None => return Box::pin(async move { Ok(None) }),
        };

        // Clone the Arc out of the map; the borrow on self ends when this
        // function returns, freeing the actor lock for other reads/writes.
        let inflight = match self.handles.get(&id) {
            Some(arc) => arc.clone(),
            None => return Box::pin(async move { Ok(None) }),
        };

        Box::pin(async move {
            match sub.as_deref() {
                // outstanding/{N} — current status (non-blocking)
                None => {
                    let state = inflight.state.lock().await;
                    let value = structfs_serde_store::to_value(&state.status).map_err(|e| {
                        StoreError::store("completion_broker", "read", e.to_string())
                    })?;
                    Ok(Some(Record::parsed(value)))
                }
                // outstanding/{N}/request — original CompletionRequest
                Some("request") => {
                    let state = inflight.state.lock().await;
                    let value = structfs_serde_store::to_value(&state.request).map_err(|e| {
                        StoreError::store("completion_broker", "read", e.to_string())
                    })?;
                    Ok(Some(Record::parsed(value)))
                }
                // outstanding/{N}/usage — UsageInfo (None until Complete)
                Some("usage") => {
                    let state = inflight.state.lock().await;
                    match &state.usage {
                        Some(u) => {
                            let value = structfs_serde_store::to_value(u).map_err(|e| {
                                StoreError::store("completion_broker", "read", e.to_string())
                            })?;
                            Ok(Some(Record::parsed(value)))
                        }
                        None => Ok(None),
                    }
                }
                // outstanding/{N}/events/count — buffer length (non-blocking)
                Some("events/count") => {
                    let state = inflight.state.lock().await;
                    Ok(Some(Record::parsed(Value::Integer(
                        state.events.len() as i64
                    ))))
                }
                // outstanding/{N}/events/from/{S} — blocking drain from index S.
                //
                // The Notified future is created and enabled BEFORE the state
                // check. Push/status writes signal with notify_waiters(), which
                // stores no permit — a plain check-then-notified().await form
                // loses any notification that lands between the lock drop and
                // the await's first poll, and with no later notification the
                // read hangs forever.
                Some(s) if s.starts_with("events/from/") => {
                    let seq: usize = s.trim_start_matches("events/from/").parse().map_err(
                        |e: std::num::ParseIntError| {
                            StoreError::store("completion_broker", "read", e.to_string())
                        },
                    )?;
                    loop {
                        let notified = inflight.notify.notified();
                        tokio::pin!(notified);
                        notified.as_mut().enable();
                        {
                            let state = inflight.state.lock().await;
                            if state.events.len() > seq || state.status.is_terminal() {
                                // Terminal short-circuits the length check, so
                                // clamp: a client-supplied seq past the end must
                                // read as empty, not panic the actor.
                                let start = seq.min(state.events.len());
                                let tail = state.events[start..].to_vec();
                                let value = structfs_serde_store::to_value(&tail).map_err(|e| {
                                    StoreError::store("completion_broker", "read", e.to_string())
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

fn docs_value() -> Value {
    let json = serde_json::json!({
        "title": "CompletionBrokerStore",
        "paths": {
            "write /": "Queue CompletionRequest → outstanding/{N}",
            "read outstanding/{N}": "CompletionStatus",
            "read outstanding/{N}/request": "Original CompletionRequest",
            "read outstanding/{N}/events/from/{S}": "Vec<StreamEvent> from index S (BLOCKING)",
            "read outstanding/{N}/events/count": "usize current buffer length",
            "read outstanding/{N}/usage": "UsageInfo (None until Complete)",
            "write outstanding/{N} null": "Delete handle"
        }
    });
    structfs_serde_store::json_to_value(json)
}

/// `AsyncWriter` for `CompletionBrokerStore`.
///
/// Two legal write shapes:
///
/// 1. Root (`/`) with a `CompletionRequest` record — inserts an `Inflight`
///    handle, spawns the per-request dispatch task, returns `outstanding/{N}`.
///
/// 2. `outstanding/{N}` with `Value::Null` — GC: removes the handle and
///    returns the same path.
///
/// All other paths and non-null writes to existing handles are errors.
impl AsyncWriter for CompletionBrokerStore {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
        let to = to.clone();

        // GC: write null to outstanding/{N}
        if let Some((id, None)) = Self::parse_handle_path(&to) {
            let value_is_null = matches!(data.as_value(), Some(Value::Null));
            if value_is_null {
                self.handles.remove(&id);
                // Unpark the Block run so it tears down instead of holding
                // its downstream handles until the upstream next emits.
                if let Some(cancel) = self.cancels.remove(&id) {
                    cancel.cancel();
                }
                return Box::pin(async move { Ok(to) });
            }
            return Box::pin(async move {
                Err(StoreError::store(
                    "completion_broker",
                    "write",
                    "cannot overwrite an outstanding handle; write null to delete",
                ))
            });
        }

        // Queue: write CompletionRequest to root
        if to.is_empty() {
            let value = match data.as_value() {
                Some(v) => v.clone(),
                None => {
                    return Box::pin(async move {
                        Err(StoreError::store(
                            "completion_broker",
                            "write",
                            "expected parsed record",
                        ))
                    });
                }
            };
            let request: ox_kernel::CompletionRequest =
                match structfs_serde_store::from_value(value) {
                    Ok(r) => r,
                    Err(e) => {
                        return Box::pin(async move {
                            Err(StoreError::store(
                                "completion_broker",
                                "write",
                                format!("invalid CompletionRequest: {e}"),
                            ))
                        });
                    }
                };

            let id = self.next_request_id;
            self.next_request_id += 1;

            self.handles.insert(id, Inflight::new(request));
            let cancel = CancelHandle::new();
            self.cancels.insert(id, cancel.clone());

            // The Block does resolution, dispatch, drain, and record
            // emission through the substrate. It runs on the blocking
            // pool — wasm execution plus blocking substrate reads must
            // not park an async worker.
            let runner = self.runner.clone();
            self.runtime.spawn_blocking(move || runner(id, cancel));

            let path = Path::try_from_components(vec!["outstanding".to_string(), id.to_string()])
                .map_err(|e| StoreError::store("completion_broker", "write", e.to_string()));
            return Box::pin(async move { path });
        }

        // Block-facing sub-path writes: the broker Block reports progress
        // through these instead of holding the Inflight in memory.
        //   push   — append a batch of StreamEvents and wake drains
        //   status — set the CompletionStatus (wakes drains)
        //   usage  — set the computed UsageInfo
        if let Some((id, Some(sub))) = Self::parse_handle_path(&to) {
            let inflight = match self.handles.get(&id) {
                Some(arc) => arc.clone(),
                None => {
                    return Box::pin(async move {
                        Err(StoreError::store(
                            "completion_broker",
                            "write",
                            format!("no outstanding handle {id}"),
                        ))
                    });
                }
            };
            let value = match data.as_value() {
                Some(v) => v.clone(),
                None => {
                    return Box::pin(async move {
                        Err(StoreError::store(
                            "completion_broker",
                            "write",
                            "expected parsed record",
                        ))
                    });
                }
            };
            return Box::pin(async move {
                match sub.as_str() {
                    "push" => {
                        let events: Vec<ox_types::StreamEvent> =
                            structfs_serde_store::from_value(value).map_err(|e| {
                                StoreError::store("completion_broker", "write", e.to_string())
                            })?;
                        let mut state = inflight.state.lock().await;
                        state.events.extend(events);
                        drop(state);
                        inflight.notify.notify_waiters();
                        Ok(to)
                    }
                    "status" => {
                        let status: CompletionStatus = structfs_serde_store::from_value(value)
                            .map_err(|e| {
                                StoreError::store("completion_broker", "write", e.to_string())
                            })?;
                        let mut state = inflight.state.lock().await;
                        state.status = status;
                        drop(state);
                        inflight.notify.notify_waiters();
                        Ok(to)
                    }
                    "usage" => {
                        let usage: crate::codec::UsageInfo =
                            structfs_serde_store::from_value(value).map_err(|e| {
                                StoreError::store("completion_broker", "write", e.to_string())
                            })?;
                        let mut state = inflight.state.lock().await;
                        state.usage = Some(usage);
                        drop(state);
                        inflight.notify.notify_waiters();
                        Ok(to)
                    }
                    other => Err(StoreError::store(
                        "completion_broker",
                        "write",
                        format!("unknown outstanding sub-path: {other}"),
                    )),
                }
            });
        }

        Box::pin(async move {
            Err(StoreError::store(
                "completion_broker",
                "write",
                format!("unexpected write path: {to}"),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;

    #[test]
    fn parse_handle_path_basic() {
        assert_eq!(
            CompletionBrokerStore::parse_handle_path(&path!("outstanding/42")),
            Some((42, None))
        );
    }

    #[test]
    fn parse_handle_path_with_subpath() {
        assert_eq!(
            CompletionBrokerStore::parse_handle_path(&path!("outstanding/7/events/from/3")),
            Some((7, Some("events/from/3".into())))
        );
    }

    #[test]
    fn parse_handle_path_root_returns_none() {
        assert_eq!(CompletionBrokerStore::parse_handle_path(&path!("")), None);
    }

    #[test]
    fn parse_handle_path_outstanding_only_returns_none() {
        assert_eq!(
            CompletionBrokerStore::parse_handle_path(&path!("outstanding")),
            None
        );
    }

    #[test]
    fn parse_handle_path_nonnumeric_id_returns_none() {
        assert_eq!(
            CompletionBrokerStore::parse_handle_path(&path!("outstanding/abc")),
            None
        );
    }
}
#[cfg(test)]
mod mechanics_tests {
    //! Store mechanics only: queue → runner invocation → sub-path writes →
    //! blocking drain → GC. Completion logic (resolution, upstream dispatch)
    //! lives in the broker Block; ox-gateway's parity suites cover it.

    use super::*;
    use ox_broker::BrokerStore;
    use ox_kernel::CompletionRequest;
    use ox_path::oxpath;
    use ox_types::StreamEvent;
    use std::time::Duration;
    use structfs_core_store::path;
    use structfs_serde_store::to_value;

    fn request() -> CompletionRequest {
        CompletionRequest {
            model: "anthropic/claude-sonnet-4-20250514".into(),
            max_tokens: 10,
            system: String::new(),
            messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
            tools: vec![],
            stream: true,
            extra: Default::default(),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queue_invokes_runner_and_drains_to_terminal() {
        let broker = BrokerStore::new(Duration::from_secs(2));
        let client = broker.client();

        // Fake runner standing in for the broker Block: reads the request
        // back, pushes events, and writes terminal status through the same
        // sub-paths the Block uses.
        let runner_client = client.clone();
        let runtime = tokio::runtime::Handle::current();
        let store = CompletionBrokerStore::new(
            runtime.clone(),
            Arc::new(move |id, _cancel| {
                let base = format!("gateway/completions/outstanding/{id}");
                runtime.block_on(async {
                    let req: CompletionRequest = runner_client
                        .read_typed(&Path::parse(&format!("{base}/request")).unwrap())
                        .await
                        .unwrap()
                        .expect("queued request must be readable");
                    assert_eq!(req.max_tokens, 10);
                    runner_client
                        .write_typed(
                            &Path::parse(&format!("{base}/push")).unwrap(),
                            &vec![
                                StreamEvent::TextDelta { text: "hi".into() },
                                StreamEvent::MessageStop,
                            ],
                        )
                        .await
                        .unwrap();
                    runner_client
                        .write_typed(
                            &Path::parse(&format!("{base}/status")).unwrap(),
                            &CompletionStatus::Complete {
                                account: "anthropic".into(),
                                model_id: "claude-sonnet-4-20250514".into(),
                                completed_at_ms: 0,
                            },
                        )
                        .await
                        .unwrap();
                });
            }),
        );
        broker
            .mount_async(oxpath!("gateway", "completions"), store)
            .await;

        let handle_path = client
            .write_typed(&path!("gateway/completions"), &request())
            .await
            .unwrap();
        assert!(handle_path.to_string().contains("outstanding"));

        // Blocking drain: parks until the runner's push lands.
        let events: Vec<StreamEvent> = client
            .read_typed(&path!("gateway/completions/outstanding/0/events/from/0"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[1], StreamEvent::MessageStop));

        // Terminal status is visible non-blockingly once written.
        let status: CompletionStatus = loop {
            let s: CompletionStatus = client
                .read_typed(&path!("gateway/completions/outstanding/0"))
                .await
                .unwrap()
                .unwrap();
            if s.is_terminal() {
                break s;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert!(matches!(status, CompletionStatus::Complete { .. }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn write_null_to_outstanding_gc_removes_handle() {
        let mut store =
            CompletionBrokerStore::new(tokio::runtime::Handle::current(), Arc::new(|_, _| {}));
        let value = to_value(&request()).unwrap();
        let handle_path = store
            .write(&path!(""), Record::parsed(value))
            .await
            .unwrap();
        assert_eq!(store.handles.len(), 1);

        let gc_result = store
            .write(&handle_path, Record::parsed(Value::Null))
            .await
            .unwrap();
        assert_eq!(gc_result, handle_path);
        assert_eq!(store.handles.len(), 0);
    }
}

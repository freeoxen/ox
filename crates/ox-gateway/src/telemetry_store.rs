//! TelemetryStore — one stats request as substrate paths.
//!
//! The /stats edge writes a request record here and blocking-reads the
//! summary the stats Block computes; the aggregation logic never runs on
//! the edge. Same request-handle shape as WireStore.
//!
//! Path layout:
//!   write /                          params (any record) → outstanding/{k}
//!   read  outstanding/{k}/params     the params record (Block reads this)
//!   write outstanding/{k}/summary    computed stats JSON (Block)
//!   read  outstanding/{k}/summary    BLOCKING until the Block writes it (edge)
//!   write outstanding/{k} null       GC (edge; cancels a still-running Block)

use std::collections::HashMap;
use std::sync::Arc;

use ox_broker::async_store::BoxFuture;
use ox_gate::completion_broker::CancelHandle;
use structfs_core_store::{
    DetachedReader, DetachedWriter, Error as StoreError, Path, Record, Value,
};
use tokio::sync::{Mutex, Notify};

pub type TelemetryRunner =
    Arc<dyn Fn(u64, CancelHandle) -> BoxFuture<Result<(), String>> + Send + Sync>;

struct TelemetryInflight {
    state: Mutex<TelemetryState>,
    notify: Notify,
    cancel: CancelHandle,
}

#[derive(Default)]
struct TelemetryState {
    params: Option<Value>,
    summary: Option<Value>,
    error: Option<String>,
}

pub struct TelemetryStore {
    handles: HashMap<u64, Arc<TelemetryInflight>>,
    next_id: u64,
    runtime: tokio::runtime::Handle,
    runner: TelemetryRunner,
}

impl TelemetryStore {
    pub fn new(runtime: tokio::runtime::Handle, runner: TelemetryRunner) -> Self {
        Self {
            handles: HashMap::new(),
            next_id: 0,
            runtime,
            runner,
        }
    }

    fn parse(path: &Path) -> Option<(u64, Option<String>)> {
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

impl DetachedReader for TelemetryStore {
    fn read_detached(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let Some((id, sub)) = Self::parse(from) else {
            return Box::pin(async move { Ok(None) });
        };
        let inflight = match self.handles.get(&id) {
            Some(arc) => arc.clone(),
            None => return Box::pin(async move { Ok(None) }),
        };
        Box::pin(async move {
            match sub.as_deref() {
                Some("params") => {
                    let state = inflight.state.lock().await;
                    Ok(state.params.clone().map(Record::parsed))
                }
                // Blocking: the edge parks here until the Block delivers.
                // Enable-before-check, as everywhere.
                Some("summary") => loop {
                    let notified = inflight.notify.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    {
                        let state = inflight.state.lock().await;
                        if let Some(error) = &state.error {
                            return Err(StoreError::store("telemetry", "read", error.clone()));
                        }
                        if let Some(summary) = &state.summary {
                            return Ok(Some(Record::parsed(summary.clone())));
                        }
                    }
                    if inflight.cancel.is_cancelled() {
                        return Err(StoreError::store(
                            "telemetry",
                            "read",
                            "stats run cancelled before writing a summary",
                        ));
                    }
                    notified.await;
                },
                _ => Ok(None),
            }
        })
    }
}

impl DetachedWriter for TelemetryStore {
    fn write_detached(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
        let to = to.clone();

        // GC
        if let Some((id, None)) = Self::parse(&to) {
            if matches!(data.as_value(), Some(Value::Null)) {
                if let Some(inflight) = self.handles.remove(&id) {
                    inflight.cancel.cancel();
                    // Wake a parked summary read so it errors instead of
                    // waiting on a Block that will never write.
                    inflight.notify.notify_waiters();
                }
                return Box::pin(async move { Ok(to) });
            }
            return Box::pin(async move {
                Err(StoreError::store(
                    "telemetry",
                    "write",
                    "cannot overwrite a telemetry handle; write null to delete",
                ))
            });
        }

        // Block-facing sub-path write.
        if let Some((id, Some(sub))) = Self::parse(&to) {
            let inflight = match self.handles.get(&id) {
                Some(arc) => arc.clone(),
                None => {
                    return Box::pin(async move {
                        Err(StoreError::store(
                            "telemetry",
                            "write",
                            format!("no telemetry handle {id}"),
                        ))
                    });
                }
            };
            let value = match data.as_value() {
                Some(v) => v.clone(),
                None => {
                    return Box::pin(async move {
                        Err(StoreError::store(
                            "telemetry",
                            "write",
                            "expected parsed record",
                        ))
                    });
                }
            };
            return Box::pin(async move {
                match sub.as_str() {
                    "summary" => {
                        let mut state = inflight.state.lock().await;
                        state.summary = Some(value);
                        drop(state);
                        inflight.notify.notify_waiters();
                        Ok(to)
                    }
                    other => Err(StoreError::store(
                        "telemetry",
                        "write",
                        format!("unknown telemetry sub-path: {other}"),
                    )),
                }
            });
        }

        // Root: enqueue one stats request and hand it to the runner.
        if !to.is_empty() {
            return Box::pin(async move {
                Err(StoreError::store(
                    "telemetry",
                    "write",
                    "write params to the root",
                ))
            });
        }
        let value = match data.as_value() {
            Some(v) => v.clone(),
            None => {
                return Box::pin(async move {
                    Err(StoreError::store(
                        "telemetry",
                        "write",
                        "expected parsed record",
                    ))
                });
            }
        };
        let id = self.next_id;
        self.next_id += 1;
        let cancel = CancelHandle::new();
        let inflight = Arc::new(TelemetryInflight {
            state: Mutex::new(TelemetryState {
                params: Some(value),
                ..Default::default()
            }),
            notify: Notify::new(),
            cancel: cancel.clone(),
        });
        self.handles.insert(id, inflight.clone());

        let runner = self.runner.clone();
        self.runtime.spawn(async move {
            let outcome = runner(id, cancel).await;
            let mut state = inflight.state.lock().await;
            if state.summary.is_none() {
                state.error = Some(
                    outcome
                        .err()
                        .unwrap_or_else(|| "stats exited without a summary".into()),
                );
            }
            drop(state);
            inflight.notify.notify_waiters();
        });

        Box::pin(async move {
            Path::try_from_components(vec!["outstanding".to_string(), id.to_string()])
                .map_err(|e| StoreError::store("telemetry", "write", e.to_string()))
        })
    }
}

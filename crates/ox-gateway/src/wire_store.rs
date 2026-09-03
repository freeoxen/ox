//! WireStore — one HTTP exchange as substrate paths.
//!
//! Isotope phase 4: the http-in edge writes each inbound wire body here and
//! blocking-reads the response back; the codec Block (wire mode) does all
//! dialect work in between. The axum handlers know nothing about dialects,
//! codecs, or the completion lifecycle — they shuttle bytes.
//!
//! Path layout:
//!   write /                          {dialect, body} → outstanding/{k}
//!   read  outstanding/{k}/inbound    the {dialect, body} record (Block reads this)
//!   write outstanding/{k}/head       {mode: json|stream|error, status?, body?} (Block)
//!   read  outstanding/{k}/head       BLOCKING until the Block writes it (edge)
//!   write outstanding/{k}/frames/push  [wire frame strings] (Block, streaming)
//!   write outstanding/{k}/done       terminates the frame stream (Block)
//!   read  outstanding/{k}/frames/from/{j}  Vec<String> — BLOCKING (edge)
//!   read  outstanding/{k}/done       bool — non-blocking (edge)
//!   write outstanding/{k} null       GC (edge)

use std::collections::HashMap;
use std::sync::Arc;

use ox_broker::async_store::{AsyncReader, AsyncWriter, BoxFuture};
use ox_gate::completion_broker::CancelHandle;
use structfs_core_store::{Error as StoreError, Path, Record, Value};
use tokio::sync::{Mutex, Notify};

/// Per-exchange wire Block entry point: (handle id, cancellation for the
/// run — triggered when the edge GC's the handle, e.g. client disconnect).
pub type WireRunner = Arc<dyn Fn(u64, CancelHandle) + Send + Sync>;

struct WireInflight {
    state: Mutex<WireState>,
    notify: Notify,
    cancel: CancelHandle,
}

#[derive(Default)]
struct WireState {
    inbound: Option<Value>,
    head: Option<Value>,
    frames: Vec<String>,
    done: bool,
}

pub struct WireStore {
    handles: HashMap<u64, Arc<WireInflight>>,
    next_id: u64,
    runtime: tokio::runtime::Handle,
    runner: WireRunner,
}

impl WireStore {
    pub fn new(runtime: tokio::runtime::Handle, runner: WireRunner) -> Self {
        Self {
            handles: HashMap::new(),
            next_id: 0,
            runtime,
            runner,
        }
    }

    fn parse(path: &Path) -> Option<(u64, Option<String>)> {
        if path.len() < 2 || path[0].as_str() != "outstanding" {
            return None;
        }
        let id: u64 = path[1].as_str().parse().ok()?;
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

impl AsyncReader for WireStore {
    fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let Some((id, sub)) = Self::parse(from) else {
            return Box::pin(async move { Ok(None) });
        };
        let inflight = match self.handles.get(&id) {
            Some(arc) => arc.clone(),
            None => return Box::pin(async move { Ok(None) }),
        };
        Box::pin(async move {
            match sub.as_deref() {
                Some("inbound") => {
                    let state = inflight.state.lock().await;
                    Ok(state.inbound.clone().map(Record::parsed))
                }
                // Blocking: the edge waits here until the Block decides the
                // response shape. Enable-before-check, as everywhere.
                Some("head") => loop {
                    let notified = inflight.notify.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    {
                        let state = inflight.state.lock().await;
                        if let Some(head) = &state.head {
                            return Ok(Some(Record::parsed(head.clone())));
                        }
                        if state.done {
                            // Block exited without writing a head — surface
                            // as an error head rather than parking forever.
                            return Ok(Some(Record::parsed(structfs_serde_store::json_to_value(
                                serde_json::json!({
                                    "mode": "error",
                                    "status": 500,
                                    "body": {"error": {"message": "wire block wrote no head"}},
                                }),
                            ))));
                        }
                    }
                    notified.await;
                },
                Some("done") => {
                    let state = inflight.state.lock().await;
                    Ok(Some(Record::parsed(Value::Bool(state.done))))
                }
                Some(s) if s.starts_with("frames/from/") => {
                    let seq: usize = s.trim_start_matches("frames/from/").parse().map_err(
                        |e: std::num::ParseIntError| {
                            StoreError::store("wire", "read", e.to_string())
                        },
                    )?;
                    loop {
                        let notified = inflight.notify.notified();
                        tokio::pin!(notified);
                        notified.as_mut().enable();
                        {
                            let state = inflight.state.lock().await;
                            if state.frames.len() > seq || state.done {
                                let start = seq.min(state.frames.len());
                                let tail: Vec<Value> = state.frames[start..]
                                    .iter()
                                    .map(|f| Value::String(f.clone()))
                                    .collect();
                                return Ok(Some(Record::parsed(Value::Array(tail))));
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

impl AsyncWriter for WireStore {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
        let to = to.clone();

        // GC
        if let Some((id, None)) = Self::parse(&to) {
            if matches!(data.as_value(), Some(Value::Null)) {
                if let Some(inflight) = self.handles.remove(&id) {
                    // Unpark the wire Block if it's still draining: it
                    // unwinds through its error path and GC's the
                    // completion handle it holds.
                    inflight.cancel.cancel();
                }
                return Box::pin(async move { Ok(to) });
            }
            return Box::pin(async move {
                Err(StoreError::store(
                    "wire",
                    "write",
                    "cannot overwrite a wire handle; write null to delete",
                ))
            });
        }

        // Block-facing sub-path writes.
        if let Some((id, Some(sub))) = Self::parse(&to) {
            let inflight = match self.handles.get(&id) {
                Some(arc) => arc.clone(),
                None => {
                    return Box::pin(async move {
                        Err(StoreError::store(
                            "wire",
                            "write",
                            format!("no wire handle {id}"),
                        ))
                    });
                }
            };
            let value = match data.as_value() {
                Some(v) => v.clone(),
                None => {
                    return Box::pin(async move {
                        Err(StoreError::store("wire", "write", "expected parsed record"))
                    });
                }
            };
            return Box::pin(async move {
                match sub.as_str() {
                    "head" => {
                        let mut state = inflight.state.lock().await;
                        state.head = Some(value);
                        drop(state);
                        inflight.notify.notify_waiters();
                        Ok(to)
                    }
                    "frames/push" => {
                        let frames: Vec<String> = match value {
                            Value::Array(items) => items
                                .into_iter()
                                .filter_map(|v| match v {
                                    Value::String(s) => Some(s),
                                    _ => None,
                                })
                                .collect(),
                            _ => {
                                return Err(StoreError::store(
                                    "wire",
                                    "write",
                                    "frames/push expects an array of strings",
                                ));
                            }
                        };
                        let mut state = inflight.state.lock().await;
                        state.frames.extend(frames);
                        drop(state);
                        inflight.notify.notify_waiters();
                        Ok(to)
                    }
                    "done" => {
                        let mut state = inflight.state.lock().await;
                        state.done = true;
                        drop(state);
                        inflight.notify.notify_waiters();
                        Ok(to)
                    }
                    other => Err(StoreError::store(
                        "wire",
                        "write",
                        format!("unknown wire sub-path: {other}"),
                    )),
                }
            });
        }

        // Root: enqueue one HTTP exchange and hand it to the wire runner.
        if !to.is_empty() {
            return Box::pin(async move {
                Err(StoreError::store(
                    "wire",
                    "write",
                    "write the inbound record to the root",
                ))
            });
        }
        let value = match data.as_value() {
            Some(v) => v.clone(),
            None => {
                return Box::pin(async move {
                    Err(StoreError::store("wire", "write", "expected parsed record"))
                });
            }
        };
        let id = self.next_id;
        self.next_id += 1;
        let cancel = CancelHandle::new();
        let inflight = Arc::new(WireInflight {
            state: Mutex::new(WireState {
                inbound: Some(value),
                ..Default::default()
            }),
            notify: Notify::new(),
            cancel: cancel.clone(),
        });
        self.handles.insert(id, inflight);

        let runner = self.runner.clone();
        self.runtime.spawn_blocking(move || runner(id, cancel));

        Box::pin(async move {
            Path::try_from_components(vec!["outstanding".to_string(), id.to_string()])
                .map_err(|e| StoreError::store("wire", "write", e.to_string()))
        })
    }
}

//! `DispatchingStore` — the subscription protocol's runtime.
//!
//! The spec sketches `DispatchingStore { inner: Arc<dyn Store>, ... }`
//! where `inner` is a single underlying Store. Our broker doesn't have
//! a single Store — it routes per-prefix to mounted servers via mpsc
//! channels (see `BrokerInner`). So this dispatcher is parameterized by
//! two abstractions instead:
//!
//! - `Arc<dyn AsyncWriter>` — applies a write through whatever substrate
//!   sits below us (the broker's write path in production; a mock in
//!   tests).
//! - `Arc<dyn SnapshotReader>` — reads a single path so we can compute
//!   `before`/`after` for the `PathChange`. Production: a wrapper around
//!   the broker's `submit_read` path. Tests: a mock with controllable
//!   reads.
//!
//! The dispatcher is wired into the broker by F4. Until then, this module
//! is testable in isolation.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use structfs_core_store::{Error as StoreError, Path, Reader, Record};
use tracing::error;

use crate::async_store::BoxFuture;
use crate::subscription::{
    AsyncWriter, PathChange, SubCtx, Subscription, SubscriptionRegistry,
};

/// Read access for the dispatcher. The dispatcher uses this to compute
/// `before`/`after` snapshots and to hand a fresh `Reader` to handlers.
///
/// Two methods rather than one because the SubCtx wants a `&mut dyn Reader`
/// pinned at the post-write state, which is most naturally expressed by
/// "give me a fresh Reader" — but the dispatcher also needs to pull a
/// single value to build the PathChange's before/after. Both shapes are
/// trivially implementable by a substrate that already has read access.
pub trait SnapshotReader: Send + Sync {
    /// Build a fresh boxed `Reader` representing the current state.
    /// Used to populate `SubCtx::snapshot`.
    fn snapshot(&self) -> Box<dyn Reader>;

    /// Read a single path now. Used to compute `before` and `after` for
    /// the `PathChange` passed to handlers.
    fn read_path(&self, path: &Path) -> Result<Option<Record>, StoreError>;
}

/// Production `SpawnHandle` — calls `tokio::spawn`.
pub struct TokioSpawnHandle;

impl crate::subscription::SpawnHandle for TokioSpawnHandle {
    fn spawn(&self, task: BoxFuture<()>) -> tokio::task::AbortHandle {
        tokio::spawn(task).abort_handle()
    }
}

/// The dispatcher itself. Holds the substrate write/read surface, the
/// subscription registry, the spawn handle, and the cascade depth bound.
///
/// Construct via `DispatchingStore::new`. Apply writes via `write` —
/// each write is a depth-0 entry into the cascade; subscription-returned
/// writes recurse with depth+1. When `depth >= cascade_bound`, the write
/// is dropped with an error log; the original `write()` still returns Ok.
pub struct DispatchingStore {
    substrate: Arc<dyn AsyncWriter>,
    reader: Arc<dyn SnapshotReader>,
    subs: Arc<SubscriptionRegistry>,
    spawn: Arc<dyn crate::subscription::SpawnHandle>,
    cascade_bound: usize,
}

impl DispatchingStore {
    pub fn new(
        substrate: Arc<dyn AsyncWriter>,
        reader: Arc<dyn SnapshotReader>,
        subs: Arc<SubscriptionRegistry>,
        spawn: Arc<dyn crate::subscription::SpawnHandle>,
        cascade_bound: usize,
    ) -> Self {
        Self {
            substrate,
            reader,
            subs,
            spawn,
            cascade_bound,
        }
    }

    /// Apply `record` at `path`, then dispatch matching subscriptions.
    /// Public entry point — depth 0.
    pub async fn write(self: &Arc<Self>, path: &Path, record: Record) -> Result<Path, StoreError> {
        self.write_at_depth(path.clone(), record, 0).await
    }

    /// Recursive core. `depth` is the cascade depth; root entries are 0.
    fn write_at_depth<'a>(
        self: &'a Arc<Self>,
        path: Path,
        record: Record,
        depth: usize,
    ) -> BoxFuture<Result<Path, StoreError>> {
        let me = self.clone();
        Box::pin(async move {
            // Read `before` (best-effort — surface read errors as None).
            let before = me.reader.read_path(&path).ok().flatten();

            // Apply the substrate write. Errors here propagate.
            let written_path = me.substrate.write(path.clone(), record).await?;

            // Read `after` for the PathChange.
            let after = me.reader.read_path(&path).ok().flatten();

            let change = PathChange {
                path: path.clone(),
                before,
                after,
            };

            // Build the back-channel writer (the dispatcher itself,
            // reentered at depth 0 — spawned writes are new logical events).
            let back_writer: Arc<dyn AsyncWriter> = Arc::new(SelfWriter { inner: me.clone() });

            // Find matching subscriptions.
            let matched = me.subs.matching(&path);

            // Collect writes to recurse on, in registration order.
            let mut queued: Vec<crate::subscription::Write> = Vec::new();
            for sub in matched {
                let mut snapshot = me.reader.snapshot();
                let ctx = SubCtx {
                    snapshot: snapshot.as_mut(),
                    change: &change,
                    spawn: me.spawn.as_ref(),
                    writer: back_writer.clone(),
                };

                // Catch panics. AssertUnwindSafe because Subscription
                // doesn't bound UnwindSafe and we'd rather contain a
                // panicking handler than refuse to compile.
                //
                // catch_unwind requires the closure be UnwindSafe; the
                // `&dyn Subscription` and the SubCtx mutable references
                // are not UnwindSafe by default — AssertUnwindSafe is
                // the documented escape hatch for "I'm fine if this
                // panics; sibling state is independent."
                let sub_for_log = sub.clone();
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| sub.handle(ctx)));
                match result {
                    Ok(writes) => queued.extend(writes),
                    Err(panic) => {
                        let payload = panic_payload(&panic);
                        error!(
                            subscription_id = %sub_for_log.id().0,
                            path = %path,
                            "subscription handler panicked: {}",
                            payload
                        );
                        // Siblings still run; original write returns Ok.
                    }
                }
            }

            // Apply queued writes in FIFO order. Cascade-bound is depth+1
            // because these are *child* writes of the current depth.
            for write in queued {
                if depth + 1 >= me.cascade_bound {
                    error!(
                        cascade_bound = me.cascade_bound,
                        path = %write.path,
                        "cascade bound reached; dropping write"
                    );
                    continue;
                }
                // Sibling failure: log and continue. Original `write`
                // still returns Ok per spec line 147.
                if let Err(e) = me
                    .write_at_depth(write.path.clone(), write.record, depth + 1)
                    .await
                {
                    error!(path = %write.path, "cascade write failed: {}", e);
                }
            }

            Ok(written_path)
        })
    }
}

fn panic_payload(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = panic.downcast_ref::<String>() {
        return s.clone();
    }
    "<non-string panic payload>".to_string()
}

/// Back-channel writer: forwards to the dispatcher's `write` (depth 0).
/// Spawned tasks hold this as `Arc<dyn AsyncWriter>` and write back into
/// the dispatcher; their writes re-enter the protocol as new events.
struct SelfWriter {
    inner: Arc<DispatchingStore>,
}

impl AsyncWriter for SelfWriter {
    fn write(&self, path: Path, record: Record) -> BoxFuture<Result<Path, StoreError>> {
        let inner = self.inner.clone();
        Box::pin(async move { inner.write(&path, record).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use ox_path::oxpath;
    use structfs_core_store::Value;

    use crate::subscription::{
        AsyncWriter as SubAsyncWriter, PathPattern, SpawnHandle, SubscriptionId, Write,
    };

    // ---------- Mock substrate ----------

    /// In-memory key/value store with shared state behind a Mutex so
    /// tests can assert against the same data the dispatcher writes to.
    /// Optionally rejects writes to a configured "bad path" for
    /// `sibling_failure`.
    struct MockSubstrate {
        data: Arc<Mutex<BTreeMap<String, Value>>>,
        reject_path: Option<String>,
    }

    impl MockSubstrate {
        fn new() -> (Arc<Self>, Arc<Mutex<BTreeMap<String, Value>>>) {
            let data = Arc::new(Mutex::new(BTreeMap::new()));
            let me = Arc::new(Self {
                data: data.clone(),
                reject_path: None,
            });
            (me, data)
        }
        fn with_reject(reject: &str) -> (Arc<Self>, Arc<Mutex<BTreeMap<String, Value>>>) {
            let data = Arc::new(Mutex::new(BTreeMap::new()));
            let me = Arc::new(Self {
                data: data.clone(),
                reject_path: Some(reject.to_string()),
            });
            (me, data)
        }
    }

    impl SubAsyncWriter for MockSubstrate {
        fn write(&self, path: Path, record: Record) -> BoxFuture<Result<Path, StoreError>> {
            let key = path.to_string();
            if let Some(bad) = &self.reject_path {
                if &key == bad {
                    return Box::pin(async move {
                        Err(StoreError::store("mock", "write", "rejected"))
                    });
                }
            }
            if let Some(v) = record.as_value() {
                self.data.lock().unwrap().insert(key, v.clone());
            }
            Box::pin(async move { Ok(path) })
        }
    }

    /// `SnapshotReader` over the same shared map. `snapshot()` returns a
    /// cheap clone of the data behind a fresh `Reader`.
    struct MockReader {
        data: Arc<Mutex<BTreeMap<String, Value>>>,
    }

    struct FrozenReader {
        snapshot: BTreeMap<String, Value>,
    }

    impl Reader for FrozenReader {
        fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
            Ok(self
                .snapshot
                .get(&from.to_string())
                .map(|v| Record::parsed(v.clone())))
        }
    }

    impl SnapshotReader for MockReader {
        fn snapshot(&self) -> Box<dyn Reader> {
            Box::new(FrozenReader {
                snapshot: self.data.lock().unwrap().clone(),
            })
        }
        fn read_path(&self, path: &Path) -> Result<Option<Record>, StoreError> {
            Ok(self
                .data
                .lock()
                .unwrap()
                .get(&path.to_string())
                .map(|v| Record::parsed(v.clone())))
        }
    }

    // ---------- Mock spawn ----------

    /// Test `SpawnHandle` that actually spawns the future via `tokio::spawn`
    /// (so the future runs and AbortHandle is real) but ALSO records the
    /// spawned AbortHandle for assertion. The future itself is consumed
    /// by tokio — tests assert via side effects (writes landing) and via
    /// the recorded handles for supersession checks (N3 will use the
    /// `is_aborted()` test).
    pub struct MockSpawn {
        handles: Mutex<Vec<tokio::task::AbortHandle>>,
    }

    impl MockSpawn {
        pub fn new() -> Self {
            Self {
                handles: Mutex::new(Vec::new()),
            }
        }
        /// Recorded handles. Used by N3's supersession tests to call
        /// `is_aborted()` on the prior handle after a second trigger fires.
        #[allow(dead_code)]
        pub fn handles(&self) -> Vec<tokio::task::AbortHandle> {
            self.handles.lock().unwrap().clone()
        }
    }

    impl SpawnHandle for MockSpawn {
        fn spawn(&self, task: BoxFuture<()>) -> tokio::task::AbortHandle {
            let handle = tokio::spawn(task).abort_handle();
            self.handles.lock().unwrap().push(handle.clone());
            handle
        }
    }

    // ---------- Test subscription helpers ----------

    /// A subscription whose `handle` runs a closure to produce returned
    /// writes. Closure is `Fn(SubCtx<'_>) -> Vec<Write>`, sharable.
    type HandlerFn = Box<dyn Fn(&PathChange, Arc<dyn SubAsyncWriter>, &dyn SpawnHandle) -> Vec<Write>
        + Send
        + Sync>;

    struct ClosureSub {
        id: SubscriptionId,
        watches: Vec<PathPattern>,
        handler: HandlerFn,
    }

    impl Subscription for ClosureSub {
        fn id(&self) -> &SubscriptionId {
            &self.id
        }
        fn watches(&self) -> &[PathPattern] {
            &self.watches
        }
        fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write> {
            (self.handler)(ctx.change, ctx.writer, ctx.spawn)
        }
    }

    fn closure_sub(
        id: &str,
        watches: Vec<PathPattern>,
        handler: HandlerFn,
    ) -> Arc<dyn Subscription> {
        Arc::new(ClosureSub {
            id: SubscriptionId(id.to_string()),
            watches,
            handler,
        })
    }

    // ---------- Test dispatcher builder ----------

    fn build(
        subs: SubscriptionRegistry,
        cascade_bound: usize,
    ) -> (
        Arc<DispatchingStore>,
        Arc<Mutex<BTreeMap<String, Value>>>,
        Arc<MockSpawn>,
    ) {
        let (substrate, data) = MockSubstrate::new();
        let reader: Arc<dyn SnapshotReader> = Arc::new(MockReader { data: data.clone() });
        let spawn = Arc::new(MockSpawn::new());
        let dispatcher = Arc::new(DispatchingStore::new(
            substrate,
            reader,
            Arc::new(subs),
            spawn.clone(),
            cascade_bound,
        ));
        (dispatcher, data, spawn)
    }

    fn build_with_reject(
        subs: SubscriptionRegistry,
        cascade_bound: usize,
        reject: &str,
    ) -> (
        Arc<DispatchingStore>,
        Arc<Mutex<BTreeMap<String, Value>>>,
        Arc<MockSpawn>,
    ) {
        let (substrate, data) = MockSubstrate::with_reject(reject);
        let reader: Arc<dyn SnapshotReader> = Arc::new(MockReader { data: data.clone() });
        let spawn = Arc::new(MockSpawn::new());
        let dispatcher = Arc::new(DispatchingStore::new(
            substrate,
            reader,
            Arc::new(subs),
            spawn.clone(),
            cascade_bound,
        ));
        (dispatcher, data, spawn)
    }

    // ---------- Tests ----------

    #[tokio::test]
    async fn basic_dispatch() {
        // Sub watches `p`, returns one write to `p2`.
        let mut reg = SubscriptionRegistry::new();
        reg.register(closure_sub(
            "A",
            vec![PathPattern::Exact(oxpath!("p"))],
            Box::new(|_change, _writer, _spawn| {
                vec![Write {
                    path: oxpath!("p2"),
                    record: Record::parsed(Value::Integer(99)),
                }]
            }),
        ));
        let (disp, data, _spawn) = build(reg, 64);

        disp.write(&oxpath!("p"), Record::parsed(Value::Integer(1)))
            .await
            .unwrap();

        let map = data.lock().unwrap();
        assert_eq!(map.get("p"), Some(&Value::Integer(1)));
        assert_eq!(map.get("p2"), Some(&Value::Integer(99)));
    }

    #[tokio::test]
    async fn cascade_depth() {
        // Sub watches `p`, returns a write back to `p` with a different
        // value each call (so we see how deep we got via the final
        // landing value). The sub fires on every depth, so we cascade
        // until cascade_bound is reached.
        //
        // cascade_bound = 4 means depths 0..3 land, depth 4 is dropped.
        // - depth 0 (caller): writes value=0, sub returns value=1
        // - depth 1: writes value=1, sub returns value=2
        // - depth 2: writes value=2, sub returns value=3
        // - depth 3: writes value=3, sub returns value=4 → bound, dropped
        // Final value at p = 3.
        let counter = Arc::new(Mutex::new(0u32));
        let counter2 = counter.clone();

        let mut reg = SubscriptionRegistry::new();
        reg.register(closure_sub(
            "self-cascade",
            vec![PathPattern::Exact(oxpath!("p"))],
            Box::new(move |_change, _writer, _spawn| {
                let mut c = counter2.lock().unwrap();
                *c += 1;
                let next = *c;
                vec![Write {
                    path: oxpath!("p"),
                    record: Record::parsed(Value::Integer(next as i64)),
                }]
            }),
        ));
        let (disp, data, _spawn) = build(reg, 4);

        disp.write(&oxpath!("p"), Record::parsed(Value::Integer(0)))
            .await
            .unwrap();

        // Sub fired at depths 0, 1, 2, 3 → counter = 4 (it produced
        // values 1,2,3,4). Value 4 was at depth 4, dropped by bound.
        // Last accepted write was value=3 at depth 3.
        assert_eq!(*counter.lock().unwrap(), 4);
        let map = data.lock().unwrap();
        assert_eq!(map.get("p"), Some(&Value::Integer(3)));
    }

    #[tokio::test]
    async fn panic_isolation() {
        let mut reg = SubscriptionRegistry::new();
        // First sub panics.
        reg.register(closure_sub(
            "panicker",
            vec![PathPattern::Exact(oxpath!("p"))],
            Box::new(|_c, _w, _s| {
                panic!("intentional panic in handler");
            }),
        ));
        // Second sub writes p2.
        reg.register(closure_sub(
            "writer",
            vec![PathPattern::Exact(oxpath!("p"))],
            Box::new(|_c, _w, _s| {
                vec![Write {
                    path: oxpath!("p2"),
                    record: Record::parsed(Value::Integer(99)),
                }]
            }),
        ));
        let (disp, data, _spawn) = build(reg, 64);

        let result = disp
            .write(&oxpath!("p"), Record::parsed(Value::Integer(1)))
            .await;
        assert!(result.is_ok(), "panicking sub must not fail original write");

        let map = data.lock().unwrap();
        assert_eq!(map.get("p"), Some(&Value::Integer(1)));
        assert_eq!(map.get("p2"), Some(&Value::Integer(99)));
    }

    #[tokio::test]
    async fn write_ordering() {
        // Sub returns [A, B]. Storage observes A, then B (so B "wins"
        // at any shared path; or both land at distinct paths in order).
        let mut reg = SubscriptionRegistry::new();
        reg.register(closure_sub(
            "A-then-B",
            vec![PathPattern::Exact(oxpath!("trigger"))],
            Box::new(|_c, _w, _s| {
                vec![
                    Write {
                        path: oxpath!("shared"),
                        record: Record::parsed(Value::String("A".to_string())),
                    },
                    Write {
                        path: oxpath!("shared"),
                        record: Record::parsed(Value::String("B".to_string())),
                    },
                ]
            }),
        ));
        let (disp, data, _spawn) = build(reg, 64);

        disp.write(&oxpath!("trigger"), Record::parsed(Value::Integer(0)))
            .await
            .unwrap();

        let map = data.lock().unwrap();
        // B applied last, so it wins.
        assert_eq!(map.get("shared"), Some(&Value::String("B".to_string())));
    }

    #[tokio::test]
    async fn pattern_boundary() {
        // Prefix("config/gate/accounts") MUST NOT match "config/gate/accounts_other/foo".
        let fired = Arc::new(Mutex::new(0u32));
        let fired2 = fired.clone();
        let mut reg = SubscriptionRegistry::new();
        reg.register(closure_sub(
            "boundary",
            vec![PathPattern::Prefix(oxpath!("config", "gate", "accounts"))],
            Box::new(move |_c, _w, _s| {
                *fired2.lock().unwrap() += 1;
                vec![]
            }),
        ));
        let (disp, _data, _spawn) = build(reg, 64);

        disp.write(
            &oxpath!("config", "gate", "accounts_other", "foo"),
            Record::parsed(Value::Integer(1)),
        )
        .await
        .unwrap();

        assert_eq!(*fired.lock().unwrap(), 0, "must not fire on adjacent name");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_lifecycle() {
        // Sub spawns a task that sleeps 50ms then writes back via the
        // back-channel writer. `write()` returns immediately. After
        // ~150ms the spawned write has landed.
        let mut reg = SubscriptionRegistry::new();
        reg.register(closure_sub(
            "spawner",
            vec![PathPattern::Exact(oxpath!("trigger"))],
            Box::new(|_change, writer, spawn| {
                let writer = writer.clone();
                let _h = spawn.spawn(Box::pin(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    let _ = writer
                        .write(oxpath!("delayed"), Record::parsed(Value::Integer(42)))
                        .await;
                }));
                vec![] // no synchronous writes
            }),
        ));
        let (disp, data, _spawn) = build(reg, 64);

        let t0 = std::time::Instant::now();
        disp.write(&oxpath!("trigger"), Record::parsed(Value::Integer(0)))
            .await
            .unwrap();
        let elapsed = t0.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(40),
            "write should return immediately, took {:?}",
            elapsed
        );

        // Delayed value not yet present.
        assert!(data.lock().unwrap().get("delayed").is_none());

        // Wait for the spawned task to land its write.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert_eq!(
            data.lock().unwrap().get("delayed"),
            Some(&Value::Integer(42))
        );
    }

    #[tokio::test]
    async fn sibling_failure() {
        // Sub A returns a write to a path the substrate rejects.
        // Sub B returns a write to an OK path. Both should run; A's
        // failure logs but original write succeeds; B's write lands.
        let mut reg = SubscriptionRegistry::new();
        reg.register(closure_sub(
            "bad",
            vec![PathPattern::Exact(oxpath!("trigger"))],
            Box::new(|_c, _w, _s| {
                vec![Write {
                    path: oxpath!("rejected"),
                    record: Record::parsed(Value::Integer(1)),
                }]
            }),
        ));
        reg.register(closure_sub(
            "good",
            vec![PathPattern::Exact(oxpath!("trigger"))],
            Box::new(|_c, _w, _s| {
                vec![Write {
                    path: oxpath!("ok"),
                    record: Record::parsed(Value::Integer(2)),
                }]
            }),
        ));
        let (disp, data, _spawn) = build_with_reject(reg, 64, "rejected");

        let result = disp
            .write(&oxpath!("trigger"), Record::parsed(Value::Integer(0)))
            .await;
        assert!(result.is_ok(), "sibling failure must not fail original");
        let map = data.lock().unwrap();
        assert!(map.get("rejected").is_none(), "rejected path stays unwritten");
        assert_eq!(map.get("ok"), Some(&Value::Integer(2)));
    }
}

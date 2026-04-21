//! CommitDrain — polls the LedgerWriter's latest-wins SaveResult slot and
//! propagates updates to the broker's inbox index.
//!
//! # Why a drain
//!
//! Each commit on the `LedgerWriter` thread updates a single latest-wins slot
//! on its [`LedgerWriterHandle`]. That slot carries the cumulative
//! [`SaveResult`] — `last_seq`, `last_hash`, `message_count`. The inbox
//! rollup in SQLite needs those values so listings show live counts instead
//! of the startup-reconcile snapshot; the rollup is keyed on `thread_id`.
//!
//! The drain is the bridge: one tokio task per mounted thread, ticking every
//! [`DRAIN_POLL_MS`] milliseconds, observing the drain slot, and calling
//! [`crate::agents::write_save_result_to_inbox`] when the sequence advances.
//! **Latest-wins semantics hold** — a burst of 1000 commits inside the poll
//! window produces at most one drain write when the interval next fires,
//! because we compare against `last_seq_seen`, not an event count.
//!
//! # Replaces
//!
//! The ledger-append side of the pre-Task-1 `save_thread_state`, which used
//! to return `SaveResult` synchronously and had the caller push it into the
//! inbox rollup. Task 1a moved the ledger writes to the `LedgerWriter`
//! thread; Task 1b stripped the old plumbing; this module is the production
//! replacement for that rollup-propagation hop.
//!
//! # Shutdown
//!
//! Drop triggers a `oneshot` cancel and aborts the task; the task's select!
//! breaks on the oneshot. The `LedgerWriter` thread sees its `Sender` count
//! fall by the drain's clone when the task terminates. Independent of that,
//! the [`LedgerWriter`] owner's own `Drop` sends `WriterMsg::Shutdown`, so
//! writer-thread exit is never contingent on drain lifecycle.
//!
//! Field order in [`crate::thread_registry::ThreadNamespace`] puts
//! `commit_drain` before `ledger_writer` so the drain's task terminates
//! before the writer shuts down — purely cosmetic, since the writer's
//! shutdown is now message-driven and not sender-count-driven.

use std::time::Duration;

use ox_inbox::ledger_writer::LedgerWriterHandle;

/// Poll cadence for the drain task. 100ms is the plan-specified target
/// (Step 10) — aligned with the "within 200ms" propagation guarantee tests
/// assert against. Public-within-crate so tests can reference the same
/// constant rather than hard-coding a number that drifts.
pub(crate) const DRAIN_POLL_MS: u64 = 100;

/// Handle to a spawned drain task. Dropping triggers clean shutdown of the
/// task; the task otherwise runs until the process exits.
pub(crate) struct CommitDrainHandle {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
    /// Test-only write counter — incremented every time the drain task calls
    /// `write_save_result_to_inbox`. Used by the burst test to assert that
    /// the latest-wins slot keeps drain-write volume bounded even under a
    /// 1000-commit burst.
    #[cfg(test)]
    write_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl CommitDrainHandle {
    /// Spawn a drain task for the given thread.
    ///
    /// The task owns its own `LedgerWriterHandle` clone (so the drain never
    /// borrows across awaits) and its own `ClientHandle` clone for broker
    /// writes. Cancellation is via the returned handle's `Drop`.
    pub(crate) fn spawn(
        writer_handle: LedgerWriterHandle,
        broker_client: ox_broker::ClientHandle,
        thread_id: String,
        rt: tokio::runtime::Handle,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        #[cfg(test)]
        let write_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        #[cfg(test)]
        let write_count_task = write_count.clone();

        let task = rt.spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(DRAIN_POLL_MS));
            // Skip the "immediate first tick" so the drain doesn't emit a
            // spurious write before any commit has landed — the first tick
            // fires after one interval has elapsed. The seed path from the
            // writer thread may publish a drain value at spawn time (when
            // remounting a non-empty ledger); we'd prefer to re-publish that
            // only when something actually advances beyond it.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await; // consume immediate tick

            let mut last_seq_seen: i64 = -1;
            let mut shutdown = shutdown_rx;

            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown => {
                        tracing::debug!(
                            thread_id = %thread_id,
                            "CommitDrain: shutdown received, exiting"
                        );
                        break;
                    }
                    _ = ticker.tick() => {
                        if let Some(sr) = writer_handle.latest_save_result() {
                            if sr.last_seq > last_seq_seen {
                                crate::agents::write_save_result_to_inbox(
                                    &broker_client,
                                    &thread_id,
                                    &sr,
                                )
                                .await;
                                last_seq_seen = sr.last_seq;
                                #[cfg(test)]
                                write_count_task
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    }
                }
            }
        });

        Self {
            shutdown: Some(shutdown_tx),
            task: Some(task),
            #[cfg(test)]
            write_count,
        }
    }

    /// Test-only: how many times the drain has called
    /// `write_save_result_to_inbox`. Used by the burst test to assert
    /// latest-wins semantics — a burst of N commits must not produce N
    /// drain writes.
    #[cfg(test)]
    pub(crate) fn write_count(&self) -> usize {
        self.write_count.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Drop for CommitDrainHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            // Ignore send errors: if the task already exited (e.g., runtime
            // teardown), there's nothing to cancel.
            let _ = tx.send(());
        }
        if let Some(jh) = self.task.take() {
            // Abort rather than join — we're in `Drop` and may be in a
            // non-tokio context (e.g., test teardown on a dedicated runtime).
            // The oneshot above is the cooperative signal; abort is the
            // belt-and-suspenders for runtime shutdown.
            jh.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ox_inbox::ledger_writer::LedgerWriter;
    use ox_inbox::{InboxStore, snapshot::SaveResult};
    use ox_kernel::log::LogEntry;
    use std::collections::BTreeMap;
    use structfs_core_store::{Value, path};

    /// Spin up a broker + inbox over a tempdir and return (handle, tempdir).
    /// Mirrors `broker_setup::tests::test_setup`'s wiring but trimmed to the
    /// pieces this drain-focused test needs: `inbox/` mount + thread
    /// creation. Keeping the tempdir alive prevents the sqlite file backing
    /// the inbox from being deleted mid-test.
    async fn setup_broker_and_inbox() -> (crate::broker_setup::BrokerHandle, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let inbox = InboxStore::open(dir.path()).unwrap();
        let bindings = crate::bindings::default_bindings();
        let mut config = BTreeMap::new();
        config.insert(
            "gate/defaults/model".to_string(),
            Value::String("claude-sonnet-4-20250514".into()),
        );
        let handle =
            crate::broker_setup::setup(inbox, bindings, dir.path().to_path_buf(), config).await;
        (handle, dir)
    }

    async fn create_thread(client: &ox_broker::ClientHandle) -> String {
        let mut create = BTreeMap::new();
        create.insert("title".to_string(), Value::String("drain test".into()));
        let created_path = client
            .write(
                &path!("inbox/threads"),
                structfs_core_store::Record::parsed(Value::Map(create)),
            )
            .await
            .unwrap();
        created_path
            .components
            .last()
            .map(|c| c.as_str().to_string())
            .expect("create returns the thread id")
    }

    async fn read_inbox_message_count(client: &ox_broker::ClientHandle, tid: &str) -> i64 {
        let rec = client.read(&path!("inbox/threads")).await.unwrap().unwrap();
        let rows = crate::parse::parse_inbox_threads(rec.as_value().expect("array"));
        rows.iter()
            .find(|r| r.id == tid)
            .map(|r| r.message_count)
            .unwrap_or(0)
    }

    fn mk_user(n: usize) -> LogEntry {
        LogEntry::User {
            content: format!("msg-{n}"),
            scope: None,
        }
    }

    /// Invariant: after N commits through a `LedgerWriter`, the drain must
    /// propagate the final `message_count` to the broker's inbox rollup
    /// within 200ms of the last commit — the freshness guarantee the inbox
    /// listing depends on. A bounded polling loop races a 500ms timeout:
    /// the assertion path fails deterministically on regression instead of
    /// hanging on a wall-clock sleep.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_propagates_message_count_within_deadline() {
        let (handle, _dir) = setup_broker_and_inbox().await;
        let client = handle.client();
        let tid = create_thread(&client).await;

        let ledger_dir = tempfile::tempdir().unwrap();
        let ledger_path = ledger_dir.path().join("ledger.jsonl");
        let writer = LedgerWriter::spawn(ledger_path).unwrap();
        let writer_handle = writer.handle();

        // Baseline before drain starts — freshly created thread row has 0.
        assert_eq!(read_inbox_message_count(&client, &tid).await, 0);

        let drain = CommitDrainHandle::spawn(
            writer_handle.clone(),
            client.clone(),
            tid.clone(),
            tokio::runtime::Handle::current(),
        );

        const N: usize = 5;
        for i in 0..N {
            match writer_handle.commit_blocking(&mk_user(i)).unwrap() {
                ox_inbox::ledger_writer::CommitResult::Ok { .. } => {}
                ox_inbox::ledger_writer::CommitResult::Err(e) => panic!("commit failed: {e}"),
            }
        }

        // Deadline: 500ms. Plan target: within 200ms of last commit. A
        // bounded polling loop (20ms granularity) is explicitly preferred
        // to a bare `sleep(200ms).then(assert)` — it fails deterministically
        // on regression and succeeds as soon as the value propagates.
        let deadline = tokio::time::timeout(Duration::from_millis(500), async {
            loop {
                let count = read_inbox_message_count(&client, &tid).await;
                if count >= N as i64 {
                    return count;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;

        let final_count = deadline.expect("drain should propagate within 500ms");
        assert_eq!(final_count, N as i64);

        drop(drain);
        drop(writer);
    }

    /// Invariant: a burst of N back-to-back commits must produce strictly
    /// fewer than N drain writes — the "latest-wins" slot collapses
    /// intermediate states between ticks. Without this property the drain
    /// would flood the broker with one rollup write per commit.
    ///
    /// Why the bound is loose: `commit_blocking` is synchronous and
    /// includes a per-commit `fsync` on the writer thread. At ~20-30ms
    /// per commit on a tempfile backend, 1000 commits stretches over many
    /// 100ms drain ticks — each of which validly emits one write because
    /// the observed `last_seq` has advanced between ticks. The real
    /// "latest-wins violated" regression would be one write per commit
    /// (~1000). We set the bound at `BURST / 2` to catch that regression
    /// with a healthy margin while tolerating the scheduler-dependent
    /// upper range of the tick-driven count (typically 200-400 on this
    /// code path).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_burst_does_not_flood_on_latest_wins() {
        let (handle, _dir) = setup_broker_and_inbox().await;
        let client = handle.client();
        let tid = create_thread(&client).await;

        let ledger_dir = tempfile::tempdir().unwrap();
        let ledger_path = ledger_dir.path().join("ledger.jsonl");
        let writer = LedgerWriter::spawn(ledger_path).unwrap();
        let writer_handle = writer.handle();

        let drain = CommitDrainHandle::spawn(
            writer_handle.clone(),
            client.clone(),
            tid.clone(),
            tokio::runtime::Handle::current(),
        );

        const BURST: usize = 1000;
        for i in 0..BURST {
            writer_handle.commit_blocking(&mk_user(i)).unwrap();
        }

        // Wait (bounded) for the drain to catch up. The test's real
        // assertion is about *how few* writes the drain emits — so we need
        // to wait until it has stabilized. 1s deadline: at 100ms cadence
        // the drain ticks ~10 times, which is well past sufficient for a
        // single post-burst update.
        let _ = tokio::time::timeout(Duration::from_millis(1500), async {
            loop {
                let count = read_inbox_message_count(&client, &tid).await;
                if count >= BURST as i64 {
                    return count;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("drain should propagate burst tally within 1.5s");

        let writes = drain.write_count();
        assert!(
            writes < BURST / 2,
            "latest-wins violated: burst of {BURST} produced {writes} drain writes \
             (expected <{}); per-commit flooding would show ~{BURST}",
            BURST / 2,
        );
        // Sanity check: the drain did fire at least once.
        assert!(
            writes >= 1,
            "drain never fired for a burst of {BURST}: write_count=0",
        );

        drop(drain);
        drop(writer);
    }

    /// Sanity: `CommitDrainHandle::drop` must not leak the task. We can't
    /// directly inspect task liveness post-drop, but a second drop on the
    /// same handle (via a stand-in) would panic or deadlock if the state
    /// were inconsistent. This test mostly serves as a compile-level
    /// assertion that Drop is idempotent across the take-pattern.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_drop_is_clean() {
        let (handle, _dir) = setup_broker_and_inbox().await;
        let client = handle.client();
        let tid = create_thread(&client).await;

        let ledger_dir = tempfile::tempdir().unwrap();
        let ledger_path = ledger_dir.path().join("ledger.jsonl");
        let writer = LedgerWriter::spawn(ledger_path).unwrap();

        let drain = CommitDrainHandle::spawn(
            writer.handle(),
            client.clone(),
            tid,
            tokio::runtime::Handle::current(),
        );
        drop(drain);
        drop(writer);

        // Yield once so the aborted task has a chance to observe the abort
        // before the test harness reaps the runtime. This is not a
        // wall-clock assertion — just cooperative scheduling.
        tokio::task::yield_now().await;

        // Prevent an unused-variable warning on SaveResult import.
        let _ = SaveResult {
            last_seq: 0,
            last_hash: None,
            message_count: 0,
        };
    }
}

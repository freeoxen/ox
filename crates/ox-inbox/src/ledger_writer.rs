//! LedgerWriter — dedicated OS thread per thread-dir that turns
//! `SharedLog::append` into a durable, hash-chained ledger write.
//!
//! # Contract
//!
//! - **One writer per ledger file.** Serializes appends; the hash chain is an
//!   inherently sequential primitive and a single writer is the natural fit.
//! - **Sync commit semantics.** `commit_blocking(&entry)` returns only after
//!   `write_all` + `File::sync_data` have both succeeded. This is the point
//!   at which the entry can appear in `SharedLog::entries()`.
//! - **No tokio runtime required.** The writer is a plain OS thread; callers
//!   block on a sync channel. That's deliberate — `SharedLog::append` is
//!   reachable from Wasm host imports, sync tests, and the broker's
//!   `block_in_place` bridge alike. A dedicated thread is contextless.
//!
//! # Channels
//!
//! - **Input** (`mpsc::Sender<CommitRequest>`) — callers submit entries.
//! - **Per-request ack** (`sync_channel(0)`) — the writer resolves it after
//!   `sync_data()`. Scope is one commit.
//! - **Drain** (latest-wins slot) — after each commit the writer publishes a
//!   `SaveResult` for the inbox-index freshness task. No back-pressure:
//!   consumers read the latest value; missed deltas are acceptable because
//!   `SaveResult` is a cumulative snapshot, not an event stream.
//!
//! # Shutdown
//!
//! When the last `LedgerWriterHandle` drops, the input channel closes. The
//! writer thread sees `RecvError`, drains any in-flight coalesced batch, and
//! exits. The `LedgerWriter` owner that was returned from `spawn` joins the
//! thread on its own `Drop` to avoid detached threads under the crash
//! harness's `App`-drop path.

use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::thread;
use std::time::Duration;

use ox_kernel::log::LogEntry;
use structfs_core_store::Error as StoreError;

use crate::ledger::{self, LedgerEntry, entry_hash};
use crate::snapshot::{SaveResult, count_messages_in_ledger, is_message_entry};

/// How long the writer waits after receiving a commit for additional commits
/// to coalesce before calling `sync_data`. A small window amortizes fsync
/// cost across bursty writes during streaming. 5ms is a conservative default;
/// a real benchmark (plan Step 1) should revisit this before the nightly
/// soak runs.
const COALESCE_WINDOW: Duration = Duration::from_millis(5);

/// A commit submitted to the writer thread. The `ack` channel carries the
/// eventual [`CommitResult`] back to the caller.
pub struct CommitRequest {
    pub msg: LogEntry,
    pub ack: mpsc::SyncSender<CommitResult>,
}

/// Outcome of a single commit (one entry).
#[derive(Debug, Clone)]
pub enum CommitResult {
    Ok {
        last_seq: u64,
        last_hash: String,
        message_count: u64,
    },
    Err(LedgerIoError),
}

/// An I/O error reported from the writer thread.
#[derive(Debug, Clone)]
pub struct LedgerIoError {
    pub message: String,
}

impl std::fmt::Display for LedgerIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ledger io error: {}", self.message)
    }
}

impl std::error::Error for LedgerIoError {}

impl From<LedgerIoError> for StoreError {
    fn from(err: LedgerIoError) -> Self {
        StoreError::store("LedgerWriter", "commit", err.message)
    }
}

/// Implement the kernel's `Durability` trait so `SharedLog::with_durability`
/// can accept a `LedgerWriterHandle` without creating a cross-crate type
/// dependency in the other direction.
impl ox_kernel::log::Durability for LedgerWriterHandle {
    fn commit(&self, entry: &LogEntry) -> Result<(), StoreError> {
        match self.commit_blocking(entry)? {
            CommitResult::Ok { .. } => Ok(()),
            CommitResult::Err(e) => Err(e.into()),
        }
    }
}

/// Handle distributed to callers. Holds its own `Sender` clone plus a
/// shared-state `Arc` for the drain slot.
#[derive(Clone)]
pub struct LedgerWriterHandle {
    tx: mpsc::Sender<CommitRequest>,
    drain: Arc<DrainState>,
}

struct DrainState {
    last_seq: AtomicU64,
    last_hash: Mutex<Option<String>>,
    message_count: AtomicU64,
    has_value: std::sync::atomic::AtomicBool,
}

impl LedgerWriterHandle {
    /// Submit an entry and block until it is durable. Returns the
    /// `CommitResult` reported by the writer thread.
    pub fn commit_blocking(&self, entry: &LogEntry) -> Result<CommitResult, StoreError> {
        let (ack_tx, ack_rx) = mpsc::sync_channel(0);
        let request = CommitRequest {
            msg: entry.clone(),
            ack: ack_tx,
        };
        self.tx
            .send(request)
            .map_err(|_| StoreError::store("LedgerWriter", "commit", "writer thread has exited"))?;
        ack_rx.recv().map_err(|_| {
            StoreError::store(
                "LedgerWriter",
                "commit",
                "writer dropped ack before responding",
            )
        })
    }

    /// Non-blocking peek at the latest published `SaveResult`. `None` until
    /// the writer has produced at least one commit (or seeded head on spawn).
    pub fn latest_save_result(&self) -> Option<SaveResult> {
        if !self.drain.has_value.load(Ordering::Acquire) {
            return None;
        }
        let last_seq = self.drain.last_seq.load(Ordering::Acquire) as i64;
        let last_hash = self.drain.last_hash.lock().unwrap().clone();
        let message_count = self.drain.message_count.load(Ordering::Acquire) as i64;
        Some(SaveResult {
            last_seq,
            last_hash,
            message_count,
        })
    }
}

/// Owns the writer thread. **Must outlive all handles it hands out.**
///
/// The owner's Drop joins the thread; the thread exits when the last
/// `Sender` clone drops. Outstanding handles hold their own clones, so if a
/// handle outlives the owner, `drop` hangs forever. In practice the owner is
/// `ThreadNamespace`, which keeps the SharedLog (the only place handles are
/// installed) above the writer in its field-declaration order — so SharedLog
/// drops first, releases the handle, then the writer drops and joins cleanly.
pub struct LedgerWriter {
    /// The owner's own Sender clone. Dropped in `Drop` so the writer thread
    /// can exit once all external handles are also gone.
    tx: mpsc::Sender<CommitRequest>,
    /// Drain state shared with handles. Held here so the spawn path can seed
    /// it from the on-disk ledger before any handle is returned.
    drain: Arc<DrainState>,
    thread: Option<thread::JoinHandle<()>>,
}

impl LedgerWriter {
    /// Spawn a writer for the ledger at `ledger_path`. On startup the writer
    /// seeds its head state and message counter by reading the existing
    /// ledger (if any), so commits continue the hash chain correctly.
    pub fn spawn(ledger_path: PathBuf) -> Result<Self, LedgerIoError> {
        let (tx, rx) = mpsc::channel::<CommitRequest>();
        let drain = Arc::new(DrainState {
            last_seq: AtomicU64::new(0),
            last_hash: Mutex::new(None),
            message_count: AtomicU64::new(0),
            has_value: std::sync::atomic::AtomicBool::new(false),
        });
        let thread_drain = drain.clone();
        let thread = thread::Builder::new()
            .name(format!(
                "ledger-writer-{}",
                ledger_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("ledger"),
            ))
            .spawn(move || writer_thread(ledger_path, rx, thread_drain))
            .map_err(|e| LedgerIoError {
                message: format!("spawn writer thread: {e}"),
            })?;

        Ok(Self {
            tx,
            drain,
            thread: Some(thread),
        })
    }

    /// Cheap-cloneable handle to install on `SharedLog`.
    pub fn handle(&self) -> LedgerWriterHandle {
        LedgerWriterHandle {
            tx: self.tx.clone(),
            drain: self.drain.clone(),
        }
    }
}

impl Drop for LedgerWriter {
    fn drop(&mut self) {
        // Drop our own Sender so the writer thread sees `RecvError` once the
        // last external handle has also been released. External handles that
        // outlive this owner will hang the join — see the struct docs.
        let (dummy, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.tx, dummy));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn writer_thread(ledger_path: PathBuf, rx: mpsc::Receiver<CommitRequest>, drain: Arc<DrainState>) {
    // Seed head state from disk.
    let mut head = match ledger::read_last_entry(&ledger_path) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(
                path = %ledger_path.display(),
                error = %e,
                "LedgerWriter: failed to read last entry on startup"
            );
            None
        }
    };
    let mut message_count = match count_messages_in_ledger(&ledger_path) {
        Ok(n) => n as u64,
        Err(e) => {
            tracing::error!(
                path = %ledger_path.display(),
                error = %e,
                "LedgerWriter: failed to count messages on startup"
            );
            0
        }
    };

    // Seed the drain slot so consumers can read a valid starting point without
    // racing against the first commit.
    if let Some(h) = head.as_ref() {
        drain.last_seq.store(h.seq, Ordering::Release);
        *drain.last_hash.lock().unwrap() = Some(h.hash.clone());
        drain.message_count.store(message_count, Ordering::Release);
        drain.has_value.store(true, Ordering::Release);
    }

    loop {
        let first = match rx.recv() {
            Ok(req) => req,
            Err(_) => {
                tracing::debug!(
                    path = %ledger_path.display(),
                    "LedgerWriter: input channel closed, exiting"
                );
                return;
            }
        };

        let mut batch = vec![first];
        // Brief coalesce window: drain any commits that arrive within
        // COALESCE_WINDOW so a single `sync_data` covers them.
        let deadline = std::time::Instant::now() + COALESCE_WINDOW;
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
                Ok(req) => batch.push(req),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        let batch_len = batch.len();
        let start = std::time::Instant::now();
        match commit_batch(&ledger_path, &mut head, &mut message_count, &batch) {
            Ok(results) => {
                let sync_us = start.elapsed().as_micros() as u64;
                tracing::debug!(
                    entries = batch_len,
                    sync_us,
                    last_seq = head.as_ref().map(|h| h.seq).unwrap_or(0),
                    "LedgerCommit"
                );
                // Publish drain value (latest-wins).
                if let Some(h) = head.as_ref() {
                    drain.last_seq.store(h.seq, Ordering::Release);
                    *drain.last_hash.lock().unwrap() = Some(h.hash.clone());
                }
                drain.message_count.store(message_count, Ordering::Release);
                drain.has_value.store(true, Ordering::Release);
                // Resolve each caller's ack.
                for (req, result) in batch.into_iter().zip(results.into_iter()) {
                    let _ = req.ack.send(result);
                }
            }
            Err(err) => {
                tracing::error!(
                    path = %ledger_path.display(),
                    error = %err,
                    entries = batch_len,
                    "LedgerCommit failed"
                );
                let payload = LedgerIoError {
                    message: err.clone(),
                };
                for req in batch {
                    let _ = req.ack.send(CommitResult::Err(payload.clone()));
                }
            }
        }
    }
}

/// Append each entry in `batch` to the ledger with a single `sync_data` at
/// the end. Updates `head` and `message_count` in place.
fn commit_batch(
    ledger_path: &std::path::Path,
    head: &mut Option<LedgerEntry>,
    message_count: &mut u64,
    batch: &[CommitRequest],
) -> Result<Vec<CommitResult>, String> {
    use std::fs::OpenOptions;
    use std::io::Write;

    if let Some(parent) = ledger_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {e}"))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(ledger_path)
        .map_err(|e| format!("open ledger for append: {e}"))?;

    let mut results = Vec::with_capacity(batch.len());
    for req in batch {
        let msg_json = serde_json::to_value(&req.msg).map_err(|e| format!("serialize: {e}"))?;
        let seq = head.as_ref().map_or(0, |h| h.seq + 1);
        let parent = head.as_ref().map(|h| h.hash.clone());
        let hash = entry_hash(&msg_json);

        let line = serde_json::json!({
            "seq": seq,
            "hash": hash,
            "parent": parent,
            "msg": msg_json,
        });
        let line_str = serde_json::to_string(&line).map_err(|e| format!("serialize line: {e}"))?;
        writeln!(file, "{line_str}").map_err(|e| format!("write_all: {e}"))?;

        if is_message_entry(&msg_json) {
            *message_count += 1;
        }

        let entry = LedgerEntry {
            seq,
            hash: hash.clone(),
            parent,
            msg: msg_json,
        };
        *head = Some(entry);
        results.push(CommitResult::Ok {
            last_seq: seq,
            last_hash: hash,
            message_count: *message_count,
        });
    }

    file.sync_data().map_err(|e| format!("sync_data: {e}"))?;
    Ok(results)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn mk_user(content: &str) -> LogEntry {
        LogEntry::User {
            content: content.into(),
            scope: None,
        }
    }

    fn mk_turn_start() -> LogEntry {
        LogEntry::TurnStart { scope: None }
    }

    #[test]
    fn commit_single_entry_is_durable_before_ack() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let writer = LedgerWriter::spawn(path.clone()).unwrap();
        let handle = writer.handle();

        let result = handle.commit_blocking(&mk_user("hello")).unwrap();
        match result {
            CommitResult::Ok {
                last_seq,
                message_count,
                ..
            } => {
                assert_eq!(last_seq, 0);
                assert_eq!(message_count, 1);
            }
            CommitResult::Err(e) => panic!("commit failed: {e}"),
        }

        // The file on disk must contain the entry *before* this point,
        // because commit_blocking returns only after sync_data has succeeded.
        let entries = ledger::read_ledger(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].seq, 0);
    }

    #[test]
    fn commit_maintains_hash_chain() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let writer = LedgerWriter::spawn(path.clone()).unwrap();
        let handle = writer.handle();

        handle.commit_blocking(&mk_user("a")).unwrap();
        handle.commit_blocking(&mk_user("b")).unwrap();
        handle.commit_blocking(&mk_user("c")).unwrap();

        let entries = ledger::read_ledger(&path).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries[0].parent.is_none());
        assert_eq!(entries[1].parent.as_deref(), Some(entries[0].hash.as_str()));
        assert_eq!(entries[2].parent.as_deref(), Some(entries[1].hash.as_str()));
        assert_eq!(entries[0].seq, 0);
        assert_eq!(entries[1].seq, 1);
        assert_eq!(entries[2].seq, 2);
    }

    #[test]
    fn commit_increments_message_count_only_for_user_and_assistant() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let writer = LedgerWriter::spawn(path.clone()).unwrap();
        let handle = writer.handle();

        handle.commit_blocking(&mk_user("first")).unwrap();
        handle.commit_blocking(&mk_turn_start()).unwrap();
        let result = handle.commit_blocking(&mk_user("second")).unwrap();
        match result {
            CommitResult::Ok { message_count, .. } => {
                assert_eq!(
                    message_count, 2,
                    "user + user = 2; turn_start doesn't count"
                );
            }
            CommitResult::Err(e) => panic!("commit failed: {e}"),
        }
    }

    #[test]
    fn writer_seeds_from_existing_ledger_on_spawn() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ledger.jsonl");
        // Seed the ledger with two entries via the first writer.
        {
            let writer = LedgerWriter::spawn(path.clone()).unwrap();
            let handle = writer.handle();
            handle.commit_blocking(&mk_user("a")).unwrap();
            handle.commit_blocking(&mk_user("b")).unwrap();
        }
        // Fresh writer; the next commit must continue seq=2 with parent
        // pointing at the prior hash.
        let writer = LedgerWriter::spawn(path.clone()).unwrap();
        let handle = writer.handle();
        let result = handle.commit_blocking(&mk_user("c")).unwrap();
        match result {
            CommitResult::Ok {
                last_seq,
                last_hash,
                message_count,
            } => {
                assert_eq!(last_seq, 2);
                assert_eq!(message_count, 3);
                // Verify the file content too.
                let entries = ledger::read_ledger(&path).unwrap();
                assert_eq!(entries.len(), 3);
                assert_eq!(entries[2].seq, 2);
                assert_eq!(entries[2].hash, last_hash);
                assert_eq!(entries[2].parent.as_deref(), Some(entries[1].hash.as_str()));
            }
            CommitResult::Err(e) => panic!("commit failed: {e}"),
        }
    }

    #[test]
    fn drop_writer_exits_cleanly() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let writer = LedgerWriter::spawn(path).unwrap();
        let handle = writer.handle();
        handle.commit_blocking(&mk_user("x")).unwrap();
        drop(handle);
        drop(writer);
        // Drop should have joined the writer thread; test exits cleanly if so.
    }

    #[test]
    fn latest_save_result_reflects_last_commit() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let writer = LedgerWriter::spawn(path).unwrap();
        let handle = writer.handle();

        assert!(handle.latest_save_result().is_none());
        handle.commit_blocking(&mk_user("a")).unwrap();
        let sr = handle
            .latest_save_result()
            .expect("drain should publish after first commit");
        assert_eq!(sr.last_seq, 0);
        assert_eq!(sr.message_count, 1);
    }
}

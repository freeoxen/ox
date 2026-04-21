//! ThreadRegistry — an AsyncStore that owns per-thread store lifecycle.
//!
//! Mounted at `threads/` in the broker. Routes `{id}/{store}/{path}` internally,
//! lazy-mounts thread stores from disk on first access.

use std::collections::HashMap;
use std::path::PathBuf;

use ox_broker::async_store::{AsyncReader, AsyncWriter, BoxFuture};
use ox_context::SystemProvider;
use ox_gate::GateStore;
use ox_history::HistoryView;
use ox_kernel::log::{LogStore, SharedLog};
use ox_ui::ApprovalStore;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Store, Writer};

use crate::agents::SYSTEM_PROMPT;

// ---------------------------------------------------------------------------
// ThreadNamespace — per-thread store collection
// ---------------------------------------------------------------------------

/// Per-thread store collection holding 5 sync stores and 1 async store.
///
/// Field order is a cosmetic convention, not a correctness requirement:
/// `LedgerWriter`'s `Drop` sends an explicit `Shutdown` message so the writer
/// thread exits regardless of whether `log` (or any other handle holder) has
/// dropped first. Keeping `log` before `ledger_writer` is still preferred so
/// in-flight commits from `LogStore::write` land before the writer sees the
/// shutdown, but reordering is no longer a deadlock hazard. `commit_drain`
/// is placed **before** `ledger_writer` so its tokio task terminates before
/// the writer thread shuts down — cleaner shutdown telemetry, not load-
/// bearing for correctness.
pub struct ThreadNamespace {
    system: SystemProvider,
    history: HistoryView,
    log: LogStore,
    tools: ox_tools::ToolStore,
    pub gate: GateStore,
    pub approval: ApprovalStore,
    /// Per-thread drain task that observes the `LedgerWriter`'s latest-wins
    /// `SaveResult` slot and writes through to the broker's inbox rollup.
    /// `None` when there's no broker client at mount time (tests, early
    /// construction) or no ledger writer (e.g., `new_default()`).
    commit_drain: Option<crate::commit_drain::CommitDrainHandle>,
    /// Per-thread durable ledger writer. Present after `from_thread_dir`;
    /// `None` for `new_default()` (used in contexts that don't persist).
    ledger_writer: Option<ox_inbox::ledger_writer::LedgerWriter>,
}

impl ThreadNamespace {
    /// Fresh stores with default values. No durability is installed — callers
    /// that need per-append persistence should use [`Self::from_thread_dir`].
    pub fn new_default() -> Self {
        let shared_log = SharedLog::new();
        Self {
            system: SystemProvider::new(SYSTEM_PROMPT.to_string()),
            history: HistoryView::new(shared_log.clone()),
            log: LogStore::from_shared(shared_log),
            tools: ox_tools::ToolStore::empty(),
            gate: GateStore::new(),
            approval: ApprovalStore::new(),
            commit_drain: None,
            ledger_writer: None,
        }
    }

    /// Attach a `CommitDrain` task that propagates the `LedgerWriter`'s
    /// latest-wins `SaveResult` into the broker's inbox rollup.
    ///
    /// Idempotent: if a drain is already attached, this is a no-op (we don't
    /// leak tasks if `ensure_mounted` runs twice for a transient reason). A
    /// no-op also occurs when there's no `LedgerWriter` (e.g., namespace
    /// built via `new_default()` for a test context that doesn't persist) —
    /// the drain has nothing to observe in that case.
    pub fn attach_commit_drain(
        &mut self,
        broker_client: ox_broker::ClientHandle,
        thread_id: String,
        rt: tokio::runtime::Handle,
    ) {
        if self.commit_drain.is_some() {
            return;
        }
        let Some(writer) = self.ledger_writer.as_ref() else {
            return;
        };
        let drain = crate::commit_drain::CommitDrainHandle::spawn(
            writer.handle(),
            broker_client,
            thread_id,
            rt,
        );
        self.commit_drain = Some(drain);
    }

    /// Shared reference to this thread's `SharedLog`. Used by
    /// `from_thread_dir` to install a durability handle after replay.
    fn shared_log(&self) -> SharedLog {
        self.log.shared().clone()
    }

    /// Create from an on-disk thread directory by restoring a snapshot.
    ///
    /// **Mount lifecycle** (Task 2 of the plan pins this sequence):
    ///   1. Build the namespace with no durability installed.
    ///   2. Replay `ledger.jsonl` into the in-memory `SharedLog` via
    ///      `snapshot::restore`. Per P11 this routes through `log/append`, so
    ///      **durability must not be active yet** — otherwise replay would
    ///      double-write every entry.
    ///   3. Spawn the `LedgerWriter` for this ledger path. It seeds its head
    ///      state from the file on disk, matching what replay just loaded.
    ///   4. Install the writer's handle on `SharedLog`. Subsequent appends
    ///      commit-then-publish.
    pub fn from_thread_dir(thread_dir: &std::path::Path) -> Self {
        let mut ns = Self::new_default();
        let has_context = thread_dir.join("context.json").exists();
        let ledger_path = thread_dir.join("ledger.jsonl");
        let has_ledger = ledger_path.exists();
        let mut loaded = false;

        // Ensure a default `view.json` exists for this thread dir. One-shot,
        // not per-turn — Task 1b moved this off the `save_config_snapshot`
        // hot path. Log on failure; remount should not hard-abort just
        // because view.json couldn't be written.
        if let Err(e) = ox_inbox::snapshot::write_default_view_if_missing(thread_dir) {
            tracing::warn!(
                path = %thread_dir.display(),
                error = %e,
                "write_default_view_if_missing failed"
            );
        }

        // Restore context.json (system prompt, gate config). `snapshot::restore`
        // also replays the ledger if `context.json` is present; we invoke it
        // here for that combined path, and fall through to a ledger-only
        // replay below when `context.json` is missing.
        if has_context {
            match ox_inbox::snapshot::restore(
                &mut ns,
                thread_dir,
                &ox_inbox::snapshot::PARTICIPATING_MOUNTS,
            ) {
                Ok(()) => {
                    loaded = true;
                }
                Err(e) => {
                    tracing::error!(
                        path = %thread_dir.display(),
                        error = %e,
                        "failed to restore thread snapshot"
                    );
                    let error_msg = serde_json::json!({
                        "type": "error",
                        "message": format!("Failed to restore thread: {e}"),
                    });
                    let val = structfs_serde_store::json_to_value(error_msg);
                    ns.log
                        .write(
                            &structfs_core_store::path!("append"),
                            structfs_core_store::Record::parsed(val),
                        )
                        .ok();
                }
            }
        } else if has_ledger {
            // With per-append durability (Task 1a) a thread can have a ledger
            // long before any `save_config_snapshot` boundary writes
            // `context.json`. Replay directly from the ledger in that case.
            match ox_inbox::ledger::read_ledger(&ledger_path) {
                Ok(entries) => {
                    for entry in &entries {
                        let value = structfs_serde_store::json_to_value(entry.msg.clone());
                        if let Err(e) = ns.log.write(
                            &structfs_core_store::path!("append"),
                            structfs_core_store::Record::parsed(value),
                        ) {
                            tracing::warn!(
                                path = %ledger_path.display(),
                                seq = entry.seq,
                                error = %e,
                                "ledger replay: failed to replay entry"
                            );
                        }
                    }
                    loaded = !entries.is_empty();
                }
                Err(e) => {
                    tracing::error!(
                        path = %ledger_path.display(),
                        error = %e,
                        "failed to read ledger on remount"
                    );
                }
            }
        }

        // Try JSONL files if neither context.json nor ledger.jsonl carried state.
        if !loaded {
            if let Ok(entries) = std::fs::read_dir(thread_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "jsonl") && path != ledger_path {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            for line in content.lines() {
                                if line.is_empty() {
                                    continue;
                                }
                                if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                                    loaded = true;
                                    let value = structfs_serde_store::json_to_value(json);
                                    let append_path = ox_path::oxpath!("history", "append");
                                    ns.write(&append_path, Record::parsed(value)).ok();
                                }
                            }
                        }
                    }
                }
            }
        }

        if !loaded && !has_context && !has_ledger {
            tracing::warn!(
                path = %thread_dir.display(),
                "no saved state found in thread directory"
            );
        }

        // Reconstruct session token totals from restored log entries.
        ns.history.reconstruct_session_usage();

        // Install the durable ledger writer AFTER replay. Any entry written
        // through `log/append` from this point forward will be committed to
        // `ledger.jsonl` before becoming visible in `SharedLog`.
        match ox_inbox::ledger_writer::LedgerWriter::spawn(ledger_path) {
            Ok(writer) => {
                let handle: std::sync::Arc<dyn ox_kernel::log::Durability> =
                    std::sync::Arc::new(writer.handle());
                ns.shared_log().with_durability(handle);
                ns.ledger_writer = Some(writer);
            }
            Err(e) => {
                tracing::error!(
                    path = %thread_dir.display(),
                    error = %e,
                    "LedgerWriter: failed to spawn; per-append durability disabled for this thread"
                );
                // Post-Task-1b, the ledger is the LedgerWriter's exclusive
                // responsibility — there is no fallback path. A failed spawn
                // means this thread's log will only live in memory for this
                // process lifetime. Surface-the-degradation is a later step
                // (plan Step 7: `LedgerDegraded` banner).
            }
        }

        ns
    }

    /// Route a path to one of the 5 sync stores (NOT approval).
    /// Returns the target store and the remaining sub-path.
    fn route(&mut self, path: &Path) -> Option<(&mut dyn Store, Path)> {
        if path.is_empty() {
            return None;
        }
        let prefix = path.components[0].as_str();
        let sub = Path::from_components(path.components[1..].to_vec());
        match prefix {
            "system" => Some((&mut self.system as &mut dyn Store, sub)),
            "history" => Some((&mut self.history as &mut dyn Store, sub)),
            "log" => Some((&mut self.log as &mut dyn Store, sub)),
            "tools" => Some((&mut self.tools as &mut dyn Store, sub)),
            "gate" => Some((&mut self.gate as &mut dyn Store, sub)),
            _ => None,
        }
    }
}

impl Reader for ThreadNamespace {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        match self.route(from) {
            Some((store, sub)) => store.read(&sub),
            None => Ok(None),
        }
    }
}

impl Writer for ThreadNamespace {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        match self.route(to) {
            Some((store, sub)) => store.write(&sub, data),
            None => Err(StoreError::NoRoute { path: to.clone() }),
        }
    }
}

// ---------------------------------------------------------------------------
// ThreadRegistry — AsyncReader + AsyncWriter, routes by thread ID
// ---------------------------------------------------------------------------

/// Registry of per-thread namespaces with lazy mount from disk.
pub struct ThreadRegistry {
    threads: HashMap<String, ThreadNamespace>,
    inbox_root: PathBuf,
    broker_client: Option<ox_broker::ClientHandle>,
}

impl ThreadRegistry {
    pub fn new(inbox_root: PathBuf) -> Self {
        Self {
            threads: HashMap::new(),
            inbox_root,
            broker_client: None,
        }
    }

    pub fn set_broker_client(&mut self, client: ox_broker::ClientHandle) {
        self.broker_client = Some(client);
    }

    /// Ensure a thread is mounted, lazy-loading from disk if needed.
    fn ensure_mounted(&mut self, thread_id: &str) -> &mut ThreadNamespace {
        if !self.threads.contains_key(thread_id) {
            let thread_dir = self.inbox_root.join("threads").join(thread_id);
            let mut ns = if thread_dir.exists() {
                ThreadNamespace::from_thread_dir(&thread_dir)
            } else {
                ThreadNamespace::new_default()
            };

            // Wire config handle into GateStore if broker client is available
            if let Some(client) = &self.broker_client {
                let config_client = client.scoped("config");
                let config_adapter = ox_broker::SyncClientAdapter::new(
                    config_client,
                    tokio::runtime::Handle::current(),
                );
                let read_only = ox_store_util::ReadOnly::new(config_adapter);
                let thread_overrides = ox_store_util::LocalConfig::new();
                let cascade = ox_store_util::Cascade::new(thread_overrides, read_only);
                ns.gate = GateStore::new().with_config(Box::new(cascade));

                // Spawn the CommitDrain in the same broker-gated branch —
                // no client, no rollup write-through, so the drain would
                // have nothing useful to do. `attach_commit_drain` is a
                // no-op when there's no ledger writer (the `new_default`
                // case), so this is safe to call unconditionally within
                // this branch.
                ns.attach_commit_drain(
                    client.clone(),
                    thread_id.to_string(),
                    tokio::runtime::Handle::current(),
                );
            }

            self.threads.insert(thread_id.to_string(), ns);
        }
        self.threads.get_mut(thread_id).expect("just inserted")
    }

    /// Split the first path component as the thread ID, returning (thread_id, sub_path).
    fn split_thread_path(path: &Path) -> Option<(String, Path)> {
        if path.is_empty() {
            return None;
        }
        let thread_id = path.components[0].clone();
        let sub = Path::from_components(path.components[1..].to_vec());
        Some((thread_id, sub))
    }

    /// Check if a sub-path starts with "approval", returning the approval sub-path.
    fn is_approval_path(sub: &Path) -> Option<Path> {
        if sub.is_empty() {
            return None;
        }
        if sub.components[0] == "approval" {
            Some(Path::from_components(sub.components[1..].to_vec()))
        } else {
            None
        }
    }
}

impl AsyncReader for ThreadRegistry {
    fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let Some((thread_id, sub)) = Self::split_thread_path(from) else {
            return Box::pin(std::future::ready(Ok(None)));
        };
        let ns = self.ensure_mounted(&thread_id);

        if let Some(approval_sub) = Self::is_approval_path(&sub) {
            ns.approval.read(&approval_sub)
        } else {
            let result = ns.read(&sub);
            Box::pin(std::future::ready(result))
        }
    }
}

impl AsyncWriter for ThreadRegistry {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
        let Some((thread_id, sub)) = Self::split_thread_path(to) else {
            return Box::pin(std::future::ready(Err(StoreError::NoRoute {
                path: to.clone(),
            })));
        };
        let ns = self.ensure_mounted(&thread_id);

        if let Some(approval_sub) = Self::is_approval_path(&sub) {
            // Log approval events to the structured log
            let action = approval_sub.components.first().map(|s| s.as_str());
            match action {
                Some("request") => {
                    if let Some(val) = data.as_value() {
                        let json = structfs_serde_store::value_to_json(val.clone());
                        let tool_name = json
                            .get("tool_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        // Extract a display preview from the structured tool_input
                        let tool_input = json.get("tool_input").cloned().unwrap_or_default();
                        let input_preview = tool_input
                            .get("path")
                            .or_else(|| tool_input.get("command"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let entry = serde_json::json!({
                            "type": "approval_requested",
                            "tool_name": tool_name,
                            "input_preview": input_preview,
                        });
                        let log_val = structfs_serde_store::json_to_value(entry);
                        ns.log
                            .write(
                                &structfs_core_store::path!("append"),
                                structfs_core_store::Record::parsed(log_val),
                            )
                            .ok();
                    }
                }
                Some("response") => {
                    // Read tool_name from pending BEFORE routing (which clears it)
                    let tool_name = ns.approval.pending_tool_name().unwrap_or_default();
                    if let Some(val) = data.as_value() {
                        let json = structfs_serde_store::value_to_json(val.clone());
                        let decision = json
                            .get("decision")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let entry = serde_json::json!({
                            "type": "approval_resolved",
                            "tool_name": tool_name,
                            "decision": decision,
                        });
                        let log_val = structfs_serde_store::json_to_value(entry);
                        ns.log
                            .write(
                                &structfs_core_store::path!("append"),
                                structfs_core_store::Record::parsed(log_val),
                            )
                            .ok();
                    }
                }
                _ => {}
            }
            ns.approval.write(&approval_sub, data)
        } else {
            let result = ns.write(&sub, data);
            Box::pin(std::future::ready(result))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use structfs_core_store::{Value, path};
    use structfs_serde_store::json_to_value;

    /// Poll a BoxFuture that should resolve immediately.
    fn futures_or_poll<T>(mut fut: Pin<Box<dyn Future<Output = T> + Send>>) -> T {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
        static VTABLE: RawWakerVTable =
            RawWakerVTable::new(|p| RawWaker::new(p, &VTABLE), |_| {}, |_| {}, |_| {});
        let raw = RawWaker::new(std::ptr::null(), &VTABLE);
        let waker = unsafe { Waker::from_raw(raw) };
        let mut cx = Context::from_waker(&waker);
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(val) => val,
            Poll::Pending => panic!("future unexpectedly pending"),
        }
    }

    #[test]
    fn lazy_mount_from_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = ThreadRegistry::new(dir.path().to_path_buf());

        // Reading from a nonexistent thread creates a default namespace
        let path = Path::parse("t_new/system").unwrap();
        let result = futures_or_poll(reg.read(&path)).unwrap();
        let record = result.expect("should return system prompt");
        match record.as_value().unwrap() {
            Value::String(s) => assert_eq!(s, SYSTEM_PROMPT),
            other => panic!("expected string, got {:?}", other),
        }
    }

    #[test]
    fn lazy_mount_from_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let inbox_root = dir.path().to_path_buf();
        let thread_dir = inbox_root.join("threads").join("t_snap");
        std::fs::create_dir_all(&thread_dir).unwrap();

        // Build a namespace and install a real LedgerWriter on its
        // `SharedLog` so `history/append` commits are durable to
        // `ledger.jsonl` — that's the production lazy-mount path. Then
        // write `context.json` via `save_config_snapshot`.
        let mut ns = ThreadNamespace::new_default();
        let ledger_path = thread_dir.join("ledger.jsonl");
        let writer =
            ox_inbox::ledger_writer::LedgerWriter::spawn(ledger_path).expect("spawn ledger writer");
        let handle: std::sync::Arc<dyn ox_kernel::log::Durability> =
            std::sync::Arc::new(writer.handle());
        ns.shared_log().with_durability(handle);

        let msg = serde_json::json!({"role": "user", "content": "hello from snapshot"});
        ns.write(&path!("history/append"), Record::parsed(json_to_value(msg)))
            .unwrap();

        ox_inbox::snapshot::save_config_snapshot(
            &mut ns,
            &thread_dir,
            "t_snap",
            "Snapshot test",
            &[],
            1712345678,
            &ox_inbox::snapshot::PARTICIPATING_MOUNTS,
        )
        .unwrap();

        // Drop the helper namespace (releasing its durability handle) and
        // then its writer so the ledger is flushed and joined before the
        // fresh registry re-reads it.
        drop(ns);
        drop(writer);

        // Fresh registry — should lazy-mount from disk
        let mut reg = ThreadRegistry::new(inbox_root);
        let count_path = Path::parse("t_snap/history/count").unwrap();
        let result = futures_or_poll(reg.read(&count_path)).unwrap();
        let record = result.expect("should return count");
        match record.as_value().unwrap() {
            Value::Integer(n) => assert_eq!(*n, 1),
            other => panic!("expected integer, got {:?}", other),
        }
    }

    #[test]
    fn routes_to_correct_store() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = ThreadRegistry::new(dir.path().to_path_buf());

        // Write a message to history
        let append_path = Path::parse("t_a/history/append").unwrap();
        let msg = serde_json::json!({"role": "user", "content": "test msg"});
        futures_or_poll(reg.write(&append_path, Record::parsed(json_to_value(msg)))).unwrap();

        // Read history count
        let count_path = Path::parse("t_a/history/count").unwrap();
        let result = futures_or_poll(reg.read(&count_path)).unwrap();
        match result.unwrap().as_value().unwrap() {
            Value::Integer(n) => assert_eq!(*n, 1),
            other => panic!("expected integer 1, got {:?}", other),
        }

        // Read model id from gate store (defaults namespace)
        let model_path = Path::parse("t_a/gate/defaults/model").unwrap();
        let result = futures_or_poll(reg.read(&model_path)).unwrap();
        match result.unwrap().as_value().unwrap() {
            Value::String(s) => assert_eq!(s, "claude-sonnet-4-20250514"),
            other => panic!("expected model string, got {:?}", other),
        }
    }

    #[test]
    fn multiple_threads_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = ThreadRegistry::new(dir.path().to_path_buf());

        // Write 2 messages to t_a
        for i in 0..2 {
            let path = Path::parse("t_a/history/append").unwrap();
            let msg = serde_json::json!({"role": "user", "content": format!("msg {i}")});
            futures_or_poll(reg.write(&path, Record::parsed(json_to_value(msg)))).unwrap();
        }

        // Write 1 message to t_b
        let path = Path::parse("t_b/history/append").unwrap();
        let msg = serde_json::json!({"role": "user", "content": "only one"});
        futures_or_poll(reg.write(&path, Record::parsed(json_to_value(msg)))).unwrap();

        // Verify counts are separate
        let count_a = futures_or_poll(reg.read(&Path::parse("t_a/history/count").unwrap()))
            .unwrap()
            .unwrap();
        match count_a.as_value().unwrap() {
            Value::Integer(n) => assert_eq!(*n, 2),
            other => panic!("expected 2, got {:?}", other),
        }

        let count_b = futures_or_poll(reg.read(&Path::parse("t_b/history/count").unwrap()))
            .unwrap()
            .unwrap();
        match count_b.as_value().unwrap() {
            Value::Integer(n) => assert_eq!(*n, 1),
            other => panic!("expected 1, got {:?}", other),
        }
    }

    #[test]
    fn empty_path_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = ThreadRegistry::new(dir.path().to_path_buf());

        let empty = Path::from_components(vec![]);
        let result = futures_or_poll(reg.read(&empty)).unwrap();
        assert!(result.is_none());
    }
}

//! ThreadRegistry — an AsyncStore that owns per-thread store lifecycle.
//!
//! Mounted at `threads/` in the broker. Routes `{id}/{store}/{path}` internally,
//! lazy-mounts thread stores from disk on first access.

use std::collections::HashMap;
use std::path::PathBuf;

use ox_broker::async_store::{AsyncReader, AsyncWriter, BoxFuture};
use ox_context::{SystemProvider, ToolsProvider};
use ox_gate::GateStore;
use ox_history::HistoryProvider;
use ox_ui::ApprovalStore;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Store, Writer};

use crate::agents::SYSTEM_PROMPT;

// ---------------------------------------------------------------------------
// ThreadNamespace — per-thread store collection
// ---------------------------------------------------------------------------

/// Per-thread store collection holding 5 sync stores and 1 async store.
pub struct ThreadNamespace {
    system: SystemProvider,
    history: HistoryProvider,
    tools: ToolsProvider,
    pub gate: GateStore,
    pub approval: ApprovalStore,
}

impl ThreadNamespace {
    /// Fresh stores with default values.
    pub fn new_default() -> Self {
        Self {
            system: SystemProvider::new(SYSTEM_PROMPT.to_string()),
            history: HistoryProvider::new(),
            tools: ToolsProvider::new(vec![]),
            gate: GateStore::new(),
            approval: ApprovalStore::new(),
        }
    }

    /// Create from an on-disk thread directory by restoring a snapshot.
    pub fn from_thread_dir(thread_dir: &std::path::Path) -> Self {
        let mut ns = Self::new_default();

        // Restore from context.json + ledger.jsonl
        ox_inbox::snapshot::restore(
            &mut ns,
            thread_dir,
            &ox_inbox::snapshot::PARTICIPATING_MOUNTS,
        )
        .ok();

        // Legacy: replay raw JSONL if no context.json existed
        let ledger_path = thread_dir.join("ledger.jsonl");
        if !thread_dir.join("context.json").exists() {
            // Look for any .jsonl file that might contain messages
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
                                    let value = structfs_serde_store::json_to_value(json);
                                    let append_path =
                                        Path::parse("history/append").expect("valid path");
                                    ns.write(&append_path, Record::parsed(value)).ok();
                                }
                            }
                        }
                    }
                }
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
            let ns = if thread_dir.exists() {
                ThreadNamespace::from_thread_dir(&thread_dir)
            } else {
                ThreadNamespace::new_default()
            };

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

        // Build a namespace, write a message, save to disk
        let mut ns = ThreadNamespace::new_default();
        let msg = serde_json::json!({"role": "user", "content": "hello from snapshot"});
        ns.write(&path!("history/append"), Record::parsed(json_to_value(msg)))
            .unwrap();

        ox_inbox::snapshot::save(
            &mut ns,
            &thread_dir,
            "t_snap",
            "Snapshot test",
            &[],
            1712345678,
            &ox_inbox::snapshot::PARTICIPATING_MOUNTS,
        )
        .unwrap();

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

        // Read model id from gate store
        let model_path = Path::parse("t_a/gate/model").unwrap();
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

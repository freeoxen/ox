//! Snapshot coordinator — config-state persistence for thread directories.
//!
//! This module's sole write responsibility is `context.json`: a snapshot of
//! the non-ledger stores (`system`, `gate`) plus thread metadata. The ledger
//! itself is owned by [`crate::ledger_writer::LedgerWriter`], which commits
//! each `SharedLog::append` synchronously and is the only writer of
//! `ledger.jsonl`. The old `save()` function wrote both; that conflated
//! responsibility is now split.
//!
//! `restore` still reads both files because remount must rehydrate both the
//! config snapshot and the log projection.

use crate::ledger;
use crate::thread_dir::{self, ContextFile};
use ox_kernel::PathComponent;
use ox_path::oxpath;
use std::collections::BTreeMap;
use std::path::Path;
use structfs_core_store::Record;
use structfs_serde_store::{json_to_value, value_to_json};

/// Mounts that participate in context.json snapshots.
/// History is excluded — it lives in the ledger, not context.json.
/// Model config is excluded — it's managed by ConfigStore.
pub const PARTICIPATING_MOUNTS: [&str; 2] = ["system", "gate"];

/// Result of a ledger commit — reported by [`crate::ledger_writer::LedgerWriter`]
/// so the inbox index can track live `last_seq`, `last_hash`, and
/// `message_count` within a session. Preserved here because the ledger
/// writer publishes it via `latest_save_result()` and callers serialize it
/// into the broker's inbox rollup.
#[derive(Debug, Clone)]
pub struct SaveResult {
    pub last_seq: i64,
    pub last_hash: Option<String>,
    /// Count of `user` + `assistant` entries after this save. The
    /// indexer writes this through to SQLite so inbox listings show
    /// real message counts (not raw log-entry counts) and stay fresh
    /// within a session.
    pub message_count: i64,
}

/// Write `context.json` for a thread directory from participating-mount
/// snapshot state. Does **not** touch `ledger.jsonl` — per-append durability
/// is the [`LedgerWriter`](crate::ledger_writer::LedgerWriter)'s job.
///
/// - Reads `{mount}/snapshot/state` for each participating mount → writes
///   `context.json`.
/// - Preserves `created_at` from any existing `context.json`.
pub fn save_config_snapshot(
    namespace: &mut dyn structfs_core_store::Store,
    thread_dir: &Path,
    thread_id: &str,
    title: &str,
    labels: &[String],
    updated_at: i64,
    mounts: &[&str],
) -> Result<(), String> {
    tracing::info!(thread_id, path = %thread_dir.display(), "saving thread config snapshot");
    std::fs::create_dir_all(thread_dir).map_err(|e| e.to_string())?;

    // 1. Read snapshot states from participating mounts
    let mut stores = BTreeMap::new();
    for &mount in mounts {
        let mount_comp = PathComponent::try_new(mount.to_string()).map_err(|e| e.to_string())?;
        let path = oxpath!(mount_comp, "snapshot", "state");
        if let Ok(Some(record)) = namespace.read(&path) {
            if let Some(value) = record.as_value() {
                stores.insert(mount.to_string(), value_to_json(value.clone()));
            }
        }
    }

    // 2. Read existing context for created_at, or use updated_at for new threads
    let created_at = thread_dir::read_context(thread_dir)
        .ok()
        .flatten()
        .map(|c| c.created_at)
        .unwrap_or(updated_at);

    // 3. Write context.json
    let ctx = ContextFile {
        version: 1,
        thread_id: thread_id.to_string(),
        title: title.to_string(),
        labels: labels.to_vec(),
        created_at,
        updated_at,
        stores,
    };
    thread_dir::write_context(thread_dir, &ctx)?;

    Ok(())
}

/// Write a default `view.json` into `thread_dir` iff one is not already
/// present. Intended to run once, on thread-mount construction
/// (`ThreadNamespace::from_thread_dir`), not per turn.
pub fn write_default_view_if_missing(thread_dir: &Path) -> Result<(), String> {
    if thread_dir.join("view.json").exists() {
        return Ok(());
    }
    thread_dir::write_default_view(thread_dir)
}

/// True if a raw log entry (as serde_json::Value) is a user or
/// assistant message — the two types that count toward conversational
/// message totals.
///
/// Single typed deserialization through [`ox_kernel::log::LogEntry`]
/// — no positional prefilter. The exhaustive agreement test in this
/// module asserts that any new variant added to `LogEntry` requires
/// a deliberate decision about whether it counts as a message,
/// because the test's expectation helper is itself an exhaustive
/// `match`.
pub(crate) fn is_message_entry(msg: &serde_json::Value) -> bool {
    use ox_kernel::log::LogEntry;
    matches!(
        serde_json::from_value::<LogEntry>(msg.clone()),
        Ok(LogEntry::User { .. }) | Ok(LogEntry::Assistant { .. })
    )
}

/// Count user/assistant entries in a ledger file. Zero if the file
/// doesn't exist yet.
pub(crate) fn count_messages_in_ledger(ledger_path: &Path) -> Result<usize, String> {
    if !ledger_path.exists() {
        return Ok(0);
    }
    let entries = ledger::read_ledger(ledger_path)?;
    Ok(entries.iter().filter(|e| is_message_entry(&e.msg)).count())
}

/// Restore namespace state from a thread directory.
///
/// - Reads context.json → writes `{mount}/snapshot/state` for each store
/// - Reads ledger.jsonl → replays all messages through `history/append`
pub fn restore(
    namespace: &mut dyn structfs_core_store::Store,
    thread_dir: &Path,
    mounts: &[&str],
) -> Result<(), String> {
    // 1. Restore context (non-history stores)
    let ctx = thread_dir::read_context(thread_dir)?.ok_or("no context.json in thread directory")?;

    tracing::info!(thread_id = %ctx.thread_id, path = %thread_dir.display(), "restoring thread snapshot");

    for &mount in mounts {
        if let Some(state_json) = ctx.stores.get(mount) {
            let mount_comp =
                PathComponent::try_new(mount.to_string()).map_err(|e| e.to_string())?;
            let path = oxpath!(mount_comp, "snapshot", "state");
            let value = json_to_value(state_json.clone());
            namespace
                .write(&path, Record::parsed(value))
                .map_err(|e| e.to_string())?;
        }
    }

    // 2. Replay ledger through log/append
    let ledger_path = thread_dir.join("ledger.jsonl");
    let entries = ledger::read_ledger(&ledger_path)?;
    let log_path = structfs_core_store::Path::parse("log/append").map_err(|e| e.to_string())?;

    for entry in &entries {
        let value = json_to_value(entry.msg.clone());
        namespace
            .write(&log_path, Record::parsed(value))
            .map_err(|e| e.to_string())?;
    }

    tracing::info!(thread_id = %ctx.thread_id, message_count = entries.len(), "thread snapshot restored");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_context::{Namespace, SystemProvider};
    use ox_gate::GateStore;
    use ox_history::HistoryView;
    use ox_kernel::log::SharedLog;
    use structfs_core_store::{Reader, Writer, path};

    fn build_namespace() -> Namespace {
        let shared_log = SharedLog::new();
        let mut ns = Namespace::new();
        ns.mount(
            "system",
            Box::new(SystemProvider::new("You are helpful.".to_string())),
        );
        ns.mount("tools", Box::new(ox_tools::ToolStore::empty()));
        ns.mount("history", Box::new(HistoryView::new(shared_log.clone())));
        ns.mount(
            "log",
            Box::new(ox_kernel::log::LogStore::from_shared(shared_log)),
        );
        ns.mount("gate", Box::new(GateStore::new()));
        ns
    }

    /// Build a namespace whose `SharedLog` has a real `LedgerWriter`
    /// installed — so `log/append` writes are durable to `ledger.jsonl`.
    /// Returns the namespace, the writer (must outlive its handle — keep
    /// it alive for the test body), and the ledger file path.
    fn build_namespace_with_ledger_writer(
        ledger_path: std::path::PathBuf,
    ) -> (Namespace, crate::ledger_writer::LedgerWriter) {
        let shared_log = SharedLog::new();
        let writer = crate::ledger_writer::LedgerWriter::spawn(ledger_path)
            .expect("spawn ledger writer for test");
        let handle: std::sync::Arc<dyn ox_kernel::log::Durability> =
            std::sync::Arc::new(writer.handle());
        shared_log.with_durability(handle);

        let mut ns = Namespace::new();
        ns.mount(
            "system",
            Box::new(SystemProvider::new("You are helpful.".to_string())),
        );
        ns.mount("tools", Box::new(ox_tools::ToolStore::empty()));
        ns.mount("history", Box::new(HistoryView::new(shared_log.clone())));
        ns.mount(
            "log",
            Box::new(ox_kernel::log::LogStore::from_shared(shared_log)),
        );
        ns.mount("gate", Box::new(GateStore::new()));
        (ns, writer)
    }

    #[test]
    fn save_config_snapshot_creates_context_json() {
        let dir = tempfile::tempdir().unwrap();
        let thread_dir = dir.path().join("t_test1");
        let mut ns = build_namespace();

        save_config_snapshot(
            &mut ns,
            &thread_dir,
            "t_test1",
            "Test thread",
            &["backend".to_string()],
            1712345678,
            &PARTICIPATING_MOUNTS,
        )
        .unwrap();

        assert!(thread_dir.join("context.json").exists());
        // view.json is no longer written by save_config_snapshot — that
        // responsibility moved to ThreadNamespace::from_thread_dir.
        assert!(!thread_dir.join("view.json").exists());

        let ctx = crate::thread_dir::read_context(&thread_dir)
            .unwrap()
            .unwrap();
        assert_eq!(ctx.thread_id, "t_test1");
        assert_eq!(ctx.title, "Test thread");
        assert!(ctx.stores.contains_key("system"));
        assert!(ctx.stores.contains_key("gate"));
        assert!(!ctx.stores.contains_key("model")); // model now in gate store
        assert!(!ctx.stores.contains_key("tools"));
        assert!(!ctx.stores.contains_key("history"));
    }

    #[test]
    fn save_config_snapshot_does_not_touch_ledger() {
        // Regression guard for Task 1b: the function renamed from `save` to
        // `save_config_snapshot` must not create or mutate `ledger.jsonl`.
        let dir = tempfile::tempdir().unwrap();
        let thread_dir = dir.path().join("t_nl");
        let mut ns = build_namespace();

        // Append a message in memory — under `build_namespace` there's no
        // LedgerWriter attached, so the ledger file stays absent. That's
        // fine: the test's purpose is to assert save_config_snapshot on
        // its own doesn't create one.
        let msg = serde_json::json!({"role": "user", "content": "hi"});
        ns.write(&path!("history/append"), Record::parsed(json_to_value(msg)))
            .unwrap();

        save_config_snapshot(
            &mut ns,
            &thread_dir,
            "t_nl",
            "No ledger",
            &[],
            1712345678,
            &PARTICIPATING_MOUNTS,
        )
        .unwrap();

        assert!(thread_dir.join("context.json").exists());
        assert!(
            !thread_dir.join("ledger.jsonl").exists(),
            "save_config_snapshot must not create ledger.jsonl"
        );
    }

    #[test]
    fn write_default_view_if_missing_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let thread_dir = dir.path().join("t_view");
        std::fs::create_dir_all(&thread_dir).unwrap();

        write_default_view_if_missing(&thread_dir).unwrap();
        let view_path = thread_dir.join("view.json");
        assert!(view_path.exists());
        let first = std::fs::read_to_string(&view_path).unwrap();

        // Mutate the file, then call again — the second call must leave the
        // edited content alone (idempotent: "if missing" means exactly that).
        std::fs::write(&view_path, "custom content").unwrap();
        write_default_view_if_missing(&thread_dir).unwrap();
        let second = std::fs::read_to_string(&view_path).unwrap();
        assert_eq!(second, "custom content");
        // And a sanity check that the initial write produced a real view.
        assert!(first.contains("include"));
    }

    #[test]
    fn save_and_restore_roundtrip() {
        // Roundtrip covers: save_config_snapshot writes context.json, the
        // ledger writer commits log entries durably, and `restore` replays
        // both back into a fresh namespace.
        let dir = tempfile::tempdir().unwrap();
        let thread_dir = dir.path().join("t_test2");
        std::fs::create_dir_all(&thread_dir).unwrap();
        let ledger_path = thread_dir.join("ledger.jsonl");
        let (mut ns, writer) = build_namespace_with_ledger_writer(ledger_path);

        let msg = serde_json::json!({"role": "user", "content": "hello"});
        ns.write(&path!("history/append"), Record::parsed(json_to_value(msg)))
            .unwrap();

        save_config_snapshot(
            &mut ns,
            &thread_dir,
            "t_test2",
            "Roundtrip test",
            &[],
            1712345678,
            &PARTICIPATING_MOUNTS,
        )
        .unwrap();

        // Writer's Drop signals shutdown, so drop order here is cosmetic —
        // the writer thread exits on the Shutdown message regardless of
        // how many external handles `ns` is holding.
        drop(writer);
        drop(ns);

        let mut ns2 = build_namespace();
        restore(&mut ns2, &thread_dir, &PARTICIPATING_MOUNTS).unwrap();

        // Verify system prompt
        let record = ns2.read(&path!("system")).unwrap().unwrap();
        match record.as_value().unwrap() {
            structfs_core_store::Value::String(s) => assert_eq!(s, "You are helpful."),
            _ => panic!("expected string"),
        }

        // Verify model (now read from gate store defaults)
        let record = ns2.read(&path!("gate/defaults/model")).unwrap().unwrap();
        match record.as_value().unwrap() {
            structfs_core_store::Value::String(s) => assert_eq!(s, "claude-sonnet-4-20250514"),
            _ => panic!("expected string"),
        }

        // Verify history (1 message)
        let record = ns2.read(&path!("history/count")).unwrap().unwrap();
        match record.as_value().unwrap() {
            structfs_core_store::Value::Integer(n) => assert_eq!(*n, 1),
            _ => panic!("expected integer"),
        }
    }

    #[test]
    fn context_json_excludes_api_keys() {
        use ox_store_util::LocalConfig;
        use structfs_core_store::Value;

        let dir = tempfile::tempdir().unwrap();
        let thread_dir = dir.path().join("t_keys");

        // Inject an API key via the config handle (keys no longer live on AccountEntry)
        let mut config = LocalConfig::new();
        config.set(
            "gate/accounts/anthropic/key",
            Value::String("sk-secret".into()),
        );
        let gate = ox_gate::GateStore::new().with_config(Box::new(config));

        let mut ns = Namespace::new();
        ns.mount(
            "system",
            Box::new(SystemProvider::new("You are helpful.".to_string())),
        );
        ns.mount("tools", Box::new(ox_tools::ToolStore::empty()));
        ns.mount("history", Box::new(HistoryView::new(SharedLog::new())));
        ns.mount("gate", Box::new(gate));

        save_config_snapshot(
            &mut ns,
            &thread_dir,
            "t_keys",
            "Keys test",
            &[],
            1712345678,
            &PARTICIPATING_MOUNTS,
        )
        .unwrap();

        // Read context.json as raw string and verify no API keys
        let content = std::fs::read_to_string(thread_dir.join("context.json")).unwrap();
        assert!(
            !content.contains("sk-secret"),
            "API key must not appear in context.json"
        );
    }

    #[test]
    fn audit_log_entries_persist_and_restore() {
        // With per-append durability the ledger is populated by
        // LedgerWriter, not save_config_snapshot — so drive a real writer
        // and then verify `restore` rehydrates the full audit trail.
        let dir = tempfile::tempdir().unwrap();
        let thread_dir = dir.path().join("t_audit");
        std::fs::create_dir_all(&thread_dir).unwrap();
        let ledger_path = thread_dir.join("ledger.jsonl");
        let (mut ns, writer) = build_namespace_with_ledger_writer(ledger_path.clone());

        // Write a mix of entry types through history/append (converts to LogEntry)
        let user_msg = serde_json::json!({"role": "user", "content": "hello"});
        ns.write(
            &path!("history/append"),
            Record::parsed(json_to_value(user_msg)),
        )
        .unwrap();

        // Write audit entries directly to log/append
        let turn_start = serde_json::json!({"type": "turn_start", "scope": "root"});
        ns.write(
            &path!("log/append"),
            Record::parsed(json_to_value(turn_start)),
        )
        .unwrap();

        let approval_req = serde_json::json!({
            "type": "approval_requested",
            "tool_name": "shell",
            "input_preview": "ls -la"
        });
        ns.write(
            &path!("log/append"),
            Record::parsed(json_to_value(approval_req)),
        )
        .unwrap();

        let approval_res = serde_json::json!({
            "type": "approval_resolved",
            "tool_name": "shell",
            "decision": "allow_once"
        });
        ns.write(
            &path!("log/append"),
            Record::parsed(json_to_value(approval_res)),
        )
        .unwrap();

        let error_entry = serde_json::json!({
            "type": "error",
            "message": "something broke"
        });
        ns.write(
            &path!("log/append"),
            Record::parsed(json_to_value(error_entry)),
        )
        .unwrap();

        // Verify log has all 5 entries
        let log_count = ns.read(&path!("log/count")).unwrap().unwrap();
        assert_eq!(
            log_count.as_value().unwrap(),
            &structfs_core_store::Value::Integer(5)
        );

        // Save context.json
        save_config_snapshot(
            &mut ns,
            &thread_dir,
            "t_audit",
            "Audit test",
            &[],
            1712345678,
            &PARTICIPATING_MOUNTS,
        )
        .unwrap();

        // Writer's Drop signals shutdown; drop order is cosmetic.
        drop(writer);
        drop(ns);

        // Verify ledger has 5 entries (written via LedgerWriter's durability path)
        let ledger_entries = crate::ledger::read_ledger(&ledger_path).unwrap();
        assert_eq!(ledger_entries.len(), 5);
        assert_eq!(ledger_entries[0].msg["type"], "user");
        assert_eq!(ledger_entries[1].msg["type"], "turn_start");
        assert_eq!(ledger_entries[2].msg["type"], "approval_requested");
        assert_eq!(ledger_entries[3].msg["type"], "approval_resolved");
        assert_eq!(ledger_entries[4].msg["type"], "error");

        // Restore into fresh namespace
        let mut ns2 = build_namespace();
        restore(&mut ns2, &thread_dir, &PARTICIPATING_MOUNTS).unwrap();

        // Verify all 5 entries restored in log
        let log_count2 = ns2.read(&path!("log/count")).unwrap().unwrap();
        assert_eq!(
            log_count2.as_value().unwrap(),
            &structfs_core_store::Value::Integer(5)
        );

        // Verify history projection still works (only user message)
        let hist_count = ns2.read(&path!("history/count")).unwrap().unwrap();
        assert_eq!(
            hist_count.as_value().unwrap(),
            &structfs_core_store::Value::Integer(1)
        );
    }

    /// Exhaustive expectation for [`is_message_entry`]. Adding a new
    /// variant to [`ox_kernel::log::LogEntry`] will not compile here
    /// until the author decides whether it counts as a "message" —
    /// the same decision must be made in production. The pair is the
    /// compiler-enforced seam.
    fn expected_is_message(entry: &ox_kernel::log::LogEntry) -> bool {
        use ox_kernel::log::LogEntry;
        match entry {
            LogEntry::User { .. } | LogEntry::Assistant { .. } => true,
            LogEntry::ToolCall { .. }
            | LogEntry::ToolResult { .. }
            | LogEntry::Meta { .. }
            | LogEntry::TurnStart { .. }
            | LogEntry::TurnEnd { .. }
            | LogEntry::CompletionEnd { .. }
            | LogEntry::ApprovalRequested { .. }
            | LogEntry::ApprovalResolved { .. }
            | LogEntry::Error { .. }
            | LogEntry::TurnAborted { .. }
            | LogEntry::ToolAborted { .. } => false,
        }
    }

    #[test]
    fn is_message_entry_matches_exhaustive_expectation() {
        use ox_kernel::log::LogEntry;
        let samples: Vec<LogEntry> = vec![
            LogEntry::User {
                content: "x".into(),
                scope: None,
            },
            LogEntry::Assistant {
                content: vec![],
                source: None,
                scope: None,
                completion_id: 0,
            },
            LogEntry::ToolCall {
                id: "1".into(),
                name: "t".into(),
                input: serde_json::json!({}),
                scope: None,
            },
            LogEntry::ToolResult {
                id: "1".into(),
                output: serde_json::json!({}),
                is_error: false,
                scope: None,
            },
            LogEntry::Meta {
                data: serde_json::json!({}),
            },
            LogEntry::TurnStart { scope: None },
            LogEntry::TurnEnd {
                scope: None,
                model: None,
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            LogEntry::CompletionEnd {
                scope: "s".into(),
                model: "m".into(),
                completion_id: 0,
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            LogEntry::ApprovalRequested {
                tool_name: "t".into(),
                input_preview: "".into(),
            },
            LogEntry::ApprovalResolved {
                tool_name: "t".into(),
                decision: ox_types::Decision::AllowOnce,
            },
            LogEntry::Error {
                message: "x".into(),
                scope: None,
            },
            LogEntry::TurnAborted {
                reason: ox_kernel::log::TurnAbortReason::CrashDuringStream,
            },
            LogEntry::ToolAborted {
                tool_use_id: "t1".into(),
                reason: ox_kernel::log::ToolAbortReason::CrashDuringDispatch,
            },
        ];
        for entry in &samples {
            let json = serde_json::to_value(entry).unwrap();
            assert_eq!(
                is_message_entry(&json),
                expected_is_message(entry),
                "disagreement on variant: {entry:?}",
            );
        }
    }
}

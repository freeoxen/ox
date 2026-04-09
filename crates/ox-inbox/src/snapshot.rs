//! Snapshot coordinator — saves/restores namespace state to/from thread directories.

use crate::ledger;
use crate::thread_dir::{self, ContextFile};
use std::collections::BTreeMap;
use std::path::Path;
use structfs_core_store::Record;
use structfs_serde_store::{json_to_value, value_to_json};

/// Mounts that participate in context.json snapshots.
/// History is excluded — it lives in the ledger, not context.json.
/// Model config is excluded — it's managed by ConfigStore.
pub const PARTICIPATING_MOUNTS: [&str; 2] = ["system", "gate"];

/// Result from a save operation — used to update SQLite cache.
#[derive(Debug, Clone)]
pub struct SaveResult {
    pub last_seq: i64,
    pub last_hash: Option<String>,
}

/// Save namespace state to a thread directory.
///
/// - Reads `{mount}/snapshot/state` for each participating mount → writes context.json
/// - Reads `history/messages` → appends new messages to ledger.jsonl (incremental)
/// - Creates view.json if it doesn't exist
pub fn save(
    namespace: &mut dyn structfs_core_store::Store,
    thread_dir: &Path,
    thread_id: &str,
    title: &str,
    labels: &[String],
    updated_at: i64,
    mounts: &[&str],
) -> Result<SaveResult, String> {
    std::fs::create_dir_all(thread_dir).map_err(|e| e.to_string())?;

    // 1. Read snapshot states from participating mounts
    let mut stores = BTreeMap::new();
    for &mount in mounts {
        let path = structfs_core_store::Path::parse(&format!("{mount}/snapshot/state"))
            .map_err(|e| e.to_string())?;
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

    // 4. Write default view.json if it doesn't exist
    if !thread_dir.join("view.json").exists() {
        thread_dir::write_default_view(thread_dir)?;
    }

    // 5. Append new messages to ledger (incremental)
    let ledger_path = thread_dir.join("ledger.jsonl");
    let last_entry = ledger::read_last_entry(&ledger_path)?;
    let existing_count = last_entry.as_ref().map_or(0, |e| e.seq + 1);

    // Read all messages from history
    let messages_path =
        structfs_core_store::Path::parse("history/messages").map_err(|e| e.to_string())?;
    let messages = match namespace.read(&messages_path) {
        Ok(Some(record)) => {
            let json = value_to_json(record.as_value().ok_or("expected value")?.clone());
            json.as_array().cloned().unwrap_or_default()
        }
        _ => Vec::new(),
    };

    // Append only messages beyond what's already in the ledger
    let mut prev = last_entry;
    let mut last_seq: i64 = prev.as_ref().map_or(-1, |e| e.seq as i64);
    let mut last_hash: Option<String> = prev.as_ref().map(|e| e.hash.clone());

    for msg in messages.iter().skip(existing_count as usize) {
        let entry = ledger::append_entry(&ledger_path, msg, prev.as_ref())?;
        last_seq = entry.seq as i64;
        last_hash = Some(entry.hash.clone());
        prev = Some(entry);
    }

    Ok(SaveResult {
        last_seq,
        last_hash,
    })
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

    for &mount in mounts {
        if let Some(state_json) = ctx.stores.get(mount) {
            let path = structfs_core_store::Path::parse(&format!("{mount}/snapshot/state"))
                .map_err(|e| e.to_string())?;
            let value = json_to_value(state_json.clone());
            namespace
                .write(&path, Record::parsed(value))
                .map_err(|e| e.to_string())?;
        }
    }

    // 2. Replay ledger through history/append
    let ledger_path = thread_dir.join("ledger.jsonl");
    let entries = ledger::read_ledger(&ledger_path)?;
    let history_path =
        structfs_core_store::Path::parse("history/append").map_err(|e| e.to_string())?;

    for entry in &entries {
        let value = json_to_value(entry.msg.clone());
        namespace
            .write(&history_path, Record::parsed(value))
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_context::{Namespace, SystemProvider, ToolsProvider};
    use ox_gate::GateStore;
    use ox_history::HistoryProvider;
    use structfs_core_store::{Reader, Writer, path};

    fn build_namespace() -> Namespace {
        let mut ns = Namespace::new();
        ns.mount(
            "system",
            Box::new(SystemProvider::new("You are helpful.".to_string())),
        );
        ns.mount("tools", Box::new(ToolsProvider::new(vec![])));
        ns.mount("history", Box::new(HistoryProvider::new()));
        ns.mount("gate", Box::new(GateStore::new()));
        ns
    }

    #[test]
    fn save_creates_context_and_view() {
        let dir = tempfile::tempdir().unwrap();
        let thread_dir = dir.path().join("t_test1");
        let mut ns = build_namespace();

        save(
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
        assert!(thread_dir.join("view.json").exists());

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
    fn save_and_restore_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let thread_dir = dir.path().join("t_test2");
        let mut ns = build_namespace();

        let msg = serde_json::json!({"role": "user", "content": "hello"});
        ns.write(&path!("history/append"), Record::parsed(json_to_value(msg)))
            .unwrap();

        save(
            &mut ns,
            &thread_dir,
            "t_test2",
            "Roundtrip test",
            &[],
            1712345678,
            &PARTICIPATING_MOUNTS,
        )
        .unwrap();

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
    fn save_appends_ledger_entries() {
        let dir = tempfile::tempdir().unwrap();
        let thread_dir = dir.path().join("t_test3");
        let mut ns = build_namespace();

        let msg1 = serde_json::json!({"role": "user", "content": "first"});
        ns.write(
            &path!("history/append"),
            Record::parsed(json_to_value(msg1)),
        )
        .unwrap();
        let msg2 = serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "reply"}]});
        ns.write(
            &path!("history/append"),
            Record::parsed(json_to_value(msg2)),
        )
        .unwrap();

        save(
            &mut ns,
            &thread_dir,
            "t_test3",
            "Ledger test",
            &[],
            1712345678,
            &PARTICIPATING_MOUNTS,
        )
        .unwrap();

        let entries = crate::ledger::read_ledger(&thread_dir.join("ledger.jsonl")).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].seq, 0);
        assert_eq!(entries[1].seq, 1);
        assert_eq!(entries[1].parent.as_deref(), Some(entries[0].hash.as_str()));
    }

    #[test]
    fn incremental_save_appends_new_messages_only() {
        let dir = tempfile::tempdir().unwrap();
        let thread_dir = dir.path().join("t_test4");
        let mut ns = build_namespace();

        let msg1 = serde_json::json!({"role": "user", "content": "first"});
        ns.write(
            &path!("history/append"),
            Record::parsed(json_to_value(msg1)),
        )
        .unwrap();
        save(
            &mut ns,
            &thread_dir,
            "t_test4",
            "Incremental test",
            &[],
            1712345678,
            &PARTICIPATING_MOUNTS,
        )
        .unwrap();

        let entries = crate::ledger::read_ledger(&thread_dir.join("ledger.jsonl")).unwrap();
        assert_eq!(entries.len(), 1);

        let msg2 = serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "reply"}]});
        ns.write(
            &path!("history/append"),
            Record::parsed(json_to_value(msg2)),
        )
        .unwrap();
        let msg3 = serde_json::json!({"role": "user", "content": "follow-up"});
        ns.write(
            &path!("history/append"),
            Record::parsed(json_to_value(msg3)),
        )
        .unwrap();
        save(
            &mut ns,
            &thread_dir,
            "t_test4",
            "Incremental test",
            &[],
            1712345678,
            &PARTICIPATING_MOUNTS,
        )
        .unwrap();

        let entries = crate::ledger::read_ledger(&thread_dir.join("ledger.jsonl")).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[2].parent.as_deref(), Some(entries[1].hash.as_str()));
    }

    #[test]
    fn full_lifecycle_save_mutate_restore() {
        let dir = tempfile::tempdir().unwrap();
        let thread_dir = dir.path().join("t_lifecycle");
        let mut ns = build_namespace();

        // Build initial state
        let msg1 = serde_json::json!({"role": "user", "content": "hello"});
        ns.write(
            &path!("history/append"),
            Record::parsed(json_to_value(msg1)),
        )
        .unwrap();
        let msg2 = serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "hi there"}]});
        ns.write(
            &path!("history/append"),
            Record::parsed(json_to_value(msg2)),
        )
        .unwrap();

        // Save
        let result = save(
            &mut ns,
            &thread_dir,
            "t_lifecycle",
            "Lifecycle test",
            &["test".to_string()],
            1712345678,
            &PARTICIPATING_MOUNTS,
        )
        .unwrap();
        assert_eq!(result.last_seq, 1);
        assert!(result.last_hash.is_some());

        // Verify thread directory structure
        assert!(thread_dir.join("context.json").exists());
        assert!(thread_dir.join("ledger.jsonl").exists());
        assert!(thread_dir.join("view.json").exists());

        // Restore into fresh namespace
        let mut ns2 = build_namespace();
        restore(&mut ns2, &thread_dir, &PARTICIPATING_MOUNTS).unwrap();

        // Verify all state restored
        let record = ns2.read(&path!("system")).unwrap().unwrap();
        match record.as_value().unwrap() {
            structfs_core_store::Value::String(s) => assert_eq!(s, "You are helpful."),
            _ => panic!("expected string"),
        }

        let record = ns2.read(&path!("history/count")).unwrap().unwrap();
        match record.as_value().unwrap() {
            structfs_core_store::Value::Integer(n) => assert_eq!(*n, 2),
            _ => panic!("expected integer"),
        }

        // Verify ledger has proper hash chain
        let entries = crate::ledger::read_ledger(&thread_dir.join("ledger.jsonl")).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].parent.is_none());
        assert_eq!(entries[1].parent.as_deref(), Some(entries[0].hash.as_str()));
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
        ns.mount("tools", Box::new(ToolsProvider::new(vec![])));
        ns.mount("history", Box::new(HistoryProvider::new()));
        ns.mount("gate", Box::new(gate));

        save(
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
}

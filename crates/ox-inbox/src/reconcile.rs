//! Startup reconciliation — ensure SQLite index matches thread directories.

use crate::ledger;
use crate::thread_dir;
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::Path;

/// Reconcile SQLite index with thread directories.
///
/// 1. Remove index entries for missing directories
/// 2. Index new directories not in SQLite
/// 3. Verify last_hash for existing entries, re-derive on mismatch
pub fn reconcile(conn: &Connection, threads_dir: &Path) -> Result<(), String> {
    // 1. Get all indexed thread IDs
    let indexed: HashSet<String> = {
        let mut stmt = conn
            .prepare("SELECT id FROM threads")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.filter_map(|r| r.ok()).collect()
    };

    // 2. Get all thread directories on disk
    let on_disk: HashSet<String> = if threads_dir.exists() {
        std::fs::read_dir(threads_dir)
            .map_err(|e| e.to_string())?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                if entry.file_type().ok()?.is_dir() {
                    let name = entry.file_name().into_string().ok()?;
                    // Only count directories with context.json (new format)
                    if entry.path().join("context.json").exists() {
                        Some(name)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect()
    } else {
        HashSet::new()
    };

    // 3. Remove orphan index entries (in SQLite but not on disk)
    for id in indexed.difference(&on_disk) {
        // Only remove if the thread directory is completely gone
        // (not just missing context.json — could be legacy format)
        let dir = threads_dir.join(id);
        if !dir.exists() {
            conn.execute("DELETE FROM threads WHERE id = ?1", [id])
                .map_err(|e| e.to_string())?;
            conn.execute("DELETE FROM labels WHERE thread_id = ?1", [id])
                .map_err(|e| e.to_string())?;
            conn.execute("DELETE FROM tasks WHERE thread_id = ?1", [id])
                .map_err(|e| e.to_string())?;
        }
    }

    // 4. Index new directories (on disk but not in SQLite)
    for id in on_disk.difference(&indexed) {
        let dir = threads_dir.join(id);
        if let Ok(Some(ctx)) = thread_dir::read_context(&dir) {
            conn.execute(
                "INSERT OR IGNORE INTO threads (id, title, created_at, updated_at, last_seq, last_hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    ctx.thread_id,
                    ctx.title,
                    ctx.created_at,
                    ctx.updated_at,
                    -1i64,
                    Option::<String>::None,
                ],
            ).map_err(|e| e.to_string())?;

            // Derive last_seq/last_hash from ledger
            let ledger_path = dir.join("ledger.jsonl");
            if let Ok(Some(last)) = ledger::read_last_entry(&ledger_path) {
                conn.execute(
                    "UPDATE threads SET last_seq = ?1, last_hash = ?2 WHERE id = ?3",
                    rusqlite::params![last.seq as i64, last.hash, ctx.thread_id],
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }

    // 5. Verify hash consistency for indexed threads that exist on disk
    {
        let mut stmt = conn
            .prepare("SELECT id, last_hash FROM threads")
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, Option<String>)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();

        for (id, cached_hash) in rows {
            let dir = threads_dir.join(&id);
            let ledger_path = dir.join("ledger.jsonl");
            if !ledger_path.exists() {
                continue;
            }
            let actual_hash = ledger::read_last_entry(&ledger_path)
                .ok()
                .flatten()
                .map(|e| e.hash);

            if actual_hash != cached_hash {
                // Re-derive from directory
                if let Ok(Some(last)) = ledger::read_last_entry(&ledger_path) {
                    conn.execute(
                        "UPDATE threads SET last_seq = ?1, last_hash = ?2 WHERE id = ?3",
                        rusqlite::params![last.seq as i64, last.hash, id],
                    )
                    .map_err(|e| e.to_string())?;
                }
                // Update title/timestamps from context.json if available
                if let Ok(Some(ctx)) = thread_dir::read_context(&dir) {
                    conn.execute(
                        "UPDATE threads SET title = ?1, updated_at = ?2 WHERE id = ?3",
                        rusqlite::params![ctx.title, ctx.updated_at, id],
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
        }
    }

    // 6. Sweep stale active states — any thread that was "running" or
    //    "blocked_on_approval" at startup was interrupted by a previous exit.
    conn.execute(
        "UPDATE threads SET thread_state = 'interrupted' WHERE thread_state IN ('running', 'blocked_on_approval')",
        [],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;

    #[test]
    fn reconcile_indexes_new_directory() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("inbox.db");
        let threads_dir = dir.path().join("threads");
        std::fs::create_dir_all(&threads_dir).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        schema::initialize(&conn).unwrap();

        // Create a thread directory with context.json
        let thread_dir = threads_dir.join("t_new");
        std::fs::create_dir_all(&thread_dir).unwrap();
        let ctx = crate::thread_dir::ContextFile {
            version: 1,
            thread_id: "t_new".to_string(),
            title: "New thread".to_string(),
            labels: vec![],
            created_at: 100,
            updated_at: 200,
            stores: std::collections::BTreeMap::new(),
        };
        crate::thread_dir::write_context(&thread_dir, &ctx).unwrap();

        // Add a ledger entry
        let msg = serde_json::json!({"role": "user", "content": "hello"});
        crate::ledger::append_entry(&thread_dir.join("ledger.jsonl"), &msg, None).unwrap();

        // Reconcile
        reconcile(&conn, &threads_dir).unwrap();

        // Verify it was indexed
        let title: String = conn
            .query_row("SELECT title FROM threads WHERE id = 't_new'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(title, "New thread");

        let last_seq: i64 = conn
            .query_row(
                "SELECT last_seq FROM threads WHERE id = 't_new'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(last_seq, 0);
    }

    #[test]
    fn reconcile_removes_orphan_index_entries() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("inbox.db");
        let threads_dir = dir.path().join("threads");
        std::fs::create_dir_all(&threads_dir).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        schema::initialize(&conn).unwrap();

        // Insert an index entry with no corresponding directory
        let now = 100i64;
        conn.execute(
            "INSERT INTO threads (id, title, created_at, updated_at) VALUES ('t_gone', 'Gone', ?1, ?2)",
            [now, now],
        )
        .unwrap();

        reconcile(&conn, &threads_dir).unwrap();

        // Verify it was removed
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM threads WHERE id = 't_gone'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn reconcile_sweeps_stale_running_to_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("inbox.db");
        let threads_dir = dir.path().join("threads");
        std::fs::create_dir_all(&threads_dir).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        schema::initialize(&conn).unwrap();

        let now = 100i64;
        // Create threads in various states
        for (id, state) in [
            ("t_run", "running"),
            ("t_block", "blocked_on_approval"),
            ("t_idle", "waiting_for_input"),
            ("t_err", "errored"),
        ] {
            // Create matching directory so they aren't removed as orphans
            let tdir = threads_dir.join(id);
            std::fs::create_dir_all(&tdir).unwrap();
            let ctx = thread_dir::ContextFile {
                version: 1,
                thread_id: id.to_string(),
                title: "test".to_string(),
                labels: vec![],
                created_at: now,
                updated_at: now,
                stores: std::collections::BTreeMap::new(),
            };
            thread_dir::write_context(&tdir, &ctx).unwrap();

            conn.execute(
                "INSERT INTO threads (id, title, thread_state, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, "test", state, now, now],
            )
            .unwrap();
        }

        reconcile(&conn, &threads_dir).unwrap();

        let get_state = |id: &str| -> String {
            conn.query_row(
                "SELECT thread_state FROM threads WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
        };

        // Running and blocked should become interrupted
        assert_eq!(get_state("t_run"), "interrupted");
        assert_eq!(get_state("t_block"), "interrupted");
        // Idle and errored should be unchanged
        assert_eq!(get_state("t_idle"), "waiting_for_input");
        assert_eq!(get_state("t_err"), "errored");
    }

    #[test]
    fn reconcile_fixes_stale_hash() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("inbox.db");
        let threads_dir = dir.path().join("threads");
        std::fs::create_dir_all(&threads_dir).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        schema::initialize(&conn).unwrap();

        // Create thread in SQLite with wrong hash
        let now = 100i64;
        conn.execute(
            "INSERT INTO threads (id, title, created_at, updated_at, last_seq, last_hash) VALUES ('t_stale', 'Stale', ?1, ?2, 0, 'wronghash1234567')",
            [now, now],
        )
        .unwrap();

        // Create matching directory with different ledger content
        let thread_dir = threads_dir.join("t_stale");
        std::fs::create_dir_all(&thread_dir).unwrap();
        let ctx = crate::thread_dir::ContextFile {
            version: 1,
            thread_id: "t_stale".to_string(),
            title: "Updated title".to_string(),
            labels: vec![],
            created_at: 100,
            updated_at: 300,
            stores: std::collections::BTreeMap::new(),
        };
        crate::thread_dir::write_context(&thread_dir, &ctx).unwrap();

        let msg = serde_json::json!({"role": "user", "content": "actual"});
        let entry =
            crate::ledger::append_entry(&thread_dir.join("ledger.jsonl"), &msg, None).unwrap();

        // Reconcile
        reconcile(&conn, &threads_dir).unwrap();

        // Verify hash was corrected
        let (last_hash, title): (String, String) = conn
            .query_row(
                "SELECT last_hash, title FROM threads WHERE id = 't_stale'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(last_hash, entry.hash);
        assert_eq!(title, "Updated title");
    }
}

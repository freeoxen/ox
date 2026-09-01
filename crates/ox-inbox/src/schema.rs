use rusqlite::Connection;

pub fn initialize(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS threads (
            id            TEXT PRIMARY KEY,
            title         TEXT NOT NULL,
            parent_id     TEXT REFERENCES threads(id),
            inbox_state   TEXT NOT NULL DEFAULT 'inbox',
            thread_state  TEXT NOT NULL DEFAULT 'running',
            block_reason  TEXT,
            created_at    INTEGER NOT NULL,
            updated_at    INTEGER NOT NULL,
            token_count   INTEGER NOT NULL DEFAULT 0,
            last_seq      INTEGER NOT NULL DEFAULT -1,
            last_hash     TEXT,
            message_count INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS labels (
            thread_id     TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
            label         TEXT NOT NULL,
            PRIMARY KEY (thread_id, label)
        );

        CREATE TABLE IF NOT EXISTS tasks (
            id            TEXT PRIMARY KEY,
            thread_id     TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
            title         TEXT NOT NULL,
            status        TEXT NOT NULL DEFAULT 'pending',
            created_at    INTEGER NOT NULL,
            updated_at    INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_threads_inbox_state ON threads(inbox_state);
        CREATE INDEX IF NOT EXISTS idx_threads_thread_state ON threads(thread_state);
        CREATE INDEX IF NOT EXISTS idx_threads_updated_at ON threads(updated_at);
        CREATE INDEX IF NOT EXISTS idx_threads_parent_id ON threads(parent_id);
        CREATE INDEX IF NOT EXISTS idx_labels_label ON labels(label);
        CREATE INDEX IF NOT EXISTS idx_tasks_thread_id ON tasks(thread_id);

        CREATE TABLE IF NOT EXISTS worker_creates (
            create_id TEXT PRIMARY KEY, request_hash TEXT NOT NULL,
            thread_id TEXT NOT NULL UNIQUE, record_json BLOB NOT NULL,
            state TEXT NOT NULL DEFAULT 'accepted', result_path TEXT,
            accepted_seq INTEGER NOT NULL,
            accepted_at INTEGER NOT NULL, applied_at INTEGER
        );
        CREATE TABLE IF NOT EXISTS worker_inputs (
            message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL,
            request_hash TEXT NOT NULL, record_json BLOB NOT NULL,
            state TEXT NOT NULL DEFAULT 'accepted', result_path TEXT,
            accepted_seq INTEGER NOT NULL,
            accepted_at INTEGER NOT NULL, applied_at INTEGER
        );
        CREATE TABLE IF NOT EXISTS worker_decisions (
            approval_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL,
            request_hash TEXT NOT NULL, record_json BLOB NOT NULL,
            state TEXT NOT NULL DEFAULT 'accepted', result_path TEXT,
            accepted_seq INTEGER NOT NULL,
            accepted_at INTEGER NOT NULL, applied_at INTEGER
        );
        CREATE TABLE IF NOT EXISTS worker_cancels (
            cancel_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL,
            request_hash TEXT NOT NULL, record_json BLOB NOT NULL,
            state TEXT NOT NULL DEFAULT 'accepted', result_path TEXT,
            accepted_seq INTEGER NOT NULL,
            accepted_at INTEGER NOT NULL, applied_at INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_worker_inputs_state ON worker_inputs(state, accepted_at);
        CREATE INDEX IF NOT EXISTS idx_worker_decisions_state ON worker_decisions(state, accepted_at);
        CREATE INDEX IF NOT EXISTS idx_worker_cancels_state ON worker_cancels(state, accepted_at);
        CREATE INDEX IF NOT EXISTS idx_worker_creates_accepted_seq ON worker_creates(accepted_seq);
        CREATE INDEX IF NOT EXISTS idx_worker_inputs_accepted_seq ON worker_inputs(accepted_seq);
        CREATE INDEX IF NOT EXISTS idx_worker_decisions_accepted_seq ON worker_decisions(accepted_seq);
        CREATE INDEX IF NOT EXISTS idx_worker_cancels_accepted_seq ON worker_cancels(accepted_seq);

        -- Local orchestration state for remote ox workers. These rows record
        -- local intent and observations; the worker's ordinary thread ledger
        -- remains the authoritative remote conversation history.
        CREATE TABLE IF NOT EXISTS remote_nodes (
            node_id             TEXT PRIMARY KEY,
            node_attempt_id     TEXT NOT NULL,
            provider            TEXT NOT NULL,
            vm_name             TEXT NOT NULL UNIQUE,
            ssh_host            TEXT,
            ssh_port            INTEGER NOT NULL,
            ssh_user            TEXT,
            ssh_dest            TEXT,
            identity_path       TEXT NOT NULL,
            known_hosts_path    TEXT NOT NULL,
            worker_socket_path  TEXT NOT NULL,
            desired_state       TEXT NOT NULL,
            observed_state      TEXT NOT NULL,
            cleanup_state       TEXT NOT NULL,
            image_digest        TEXT,
            request_hash        TEXT NOT NULL,
            created_at          INTEGER NOT NULL,
            updated_at          INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS remote_conversations (
            conversation_id     TEXT PRIMARY KEY,
            node_id             TEXT NOT NULL,
            node_attempt_id     TEXT NOT NULL,
            worker_thread_id    TEXT,
            create_id           TEXT NOT NULL UNIQUE,
            title               TEXT NOT NULL,
            initial_prompt      TEXT NOT NULL,
            parent_thread_id    TEXT,
            placement           TEXT NOT NULL,
            desired_state       TEXT NOT NULL,
            observed_state      TEXT NOT NULL,
            cleanup_state       TEXT NOT NULL,
            request_hash        TEXT NOT NULL,
            created_at          INTEGER NOT NULL,
            updated_at          INTEGER NOT NULL,
            FOREIGN KEY (node_id) REFERENCES remote_nodes(node_id),
            UNIQUE (node_id, node_attempt_id, worker_thread_id)
        );

        CREATE TABLE IF NOT EXISTS remote_operations (
            operation_id        TEXT PRIMARY KEY,
            operation_kind      TEXT NOT NULL,
            node_id             TEXT,
            node_attempt_id     TEXT,
            conversation_id     TEXT,
            request_hash        TEXT NOT NULL,
            intent_json         BLOB NOT NULL,
            state               TEXT NOT NULL DEFAULT 'pending',
            result_json         BLOB,
            lease_owner         TEXT,
            lease_until         INTEGER,
            lease_epoch         INTEGER NOT NULL DEFAULT 0,
            created_at          INTEGER NOT NULL,
            updated_at          INTEGER NOT NULL,
            FOREIGN KEY (conversation_id)
                REFERENCES remote_conversations(conversation_id),
            FOREIGN KEY (node_id) REFERENCES remote_nodes(node_id)
        );

        CREATE TABLE IF NOT EXISTS remote_cached_ledger_entries (
            conversation_id     TEXT NOT NULL
                REFERENCES remote_conversations(conversation_id) ON DELETE CASCADE,
            seq                 INTEGER NOT NULL,
            hash                TEXT NOT NULL,
            parent_hash         TEXT,
            message_json        BLOB NOT NULL,
            PRIMARY KEY (conversation_id, seq),
            UNIQUE (conversation_id, hash)
        );

        CREATE TABLE IF NOT EXISTS remote_ledger_cursors (
            conversation_id     TEXT PRIMARY KEY
                REFERENCES remote_conversations(conversation_id) ON DELETE CASCADE,
            last_seq            INTEGER NOT NULL DEFAULT -1,
            last_hash           TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_remote_nodes_state
            ON remote_nodes(desired_state, observed_state, cleanup_state);
        CREATE INDEX IF NOT EXISTS idx_remote_conversations_node
            ON remote_conversations(node_id, node_attempt_id);
        CREATE INDEX IF NOT EXISTS idx_remote_conversations_state
            ON remote_conversations(desired_state, observed_state, cleanup_state);
        CREATE INDEX IF NOT EXISTS idx_remote_operations_state
            ON remote_operations(state, created_at);
        CREATE INDEX IF NOT EXISTS idx_remote_operations_node
            ON remote_operations(node_id, node_attempt_id);
        ",
    )?;

    // Task 9 persists node intent before provider creation, so provider-returned
    // addressing must be nullable. Rebuild only the short-lived remote table
    // created by the earlier development schema; local intent is preserved.
    let provider_fields_not_null: bool = conn
        .prepare("PRAGMA table_info(remote_nodes)")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(3)?))
        })?
        .filter_map(Result::ok)
        .any(|(name, not_null)| matches!(name.as_str(), "ssh_host" | "ssh_dest") && not_null != 0);
    if provider_fields_not_null {
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE remote_nodes_v2 (
                node_id TEXT PRIMARY KEY, node_attempt_id TEXT NOT NULL,
                provider TEXT NOT NULL, vm_name TEXT NOT NULL UNIQUE,
                ssh_host TEXT, ssh_port INTEGER NOT NULL, ssh_user TEXT,
                ssh_dest TEXT, identity_path TEXT NOT NULL,
                known_hosts_path TEXT NOT NULL, worker_socket_path TEXT NOT NULL,
                desired_state TEXT NOT NULL, observed_state TEXT NOT NULL,
                cleanup_state TEXT NOT NULL, image_digest TEXT,
                request_hash TEXT NOT NULL, created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
             );
             INSERT INTO remote_nodes_v2 SELECT * FROM remote_nodes;
             DROP TABLE remote_nodes;
             ALTER TABLE remote_nodes_v2 RENAME TO remote_nodes;
             CREATE INDEX IF NOT EXISTS idx_remote_nodes_state
                ON remote_nodes(desired_state, observed_state, cleanup_state);
             COMMIT;",
        )?;
    }
    if conn
        .prepare("SELECT lease_epoch FROM remote_operations LIMIT 0")
        .is_err()
    {
        conn.execute_batch(
            "ALTER TABLE remote_operations ADD COLUMN lease_epoch INTEGER NOT NULL DEFAULT 0;",
        )?;
    }

    // Migrate: add columns if missing (for databases created before this version)
    let has_last_seq: bool = conn.prepare("SELECT last_seq FROM threads LIMIT 0").is_ok();
    if !has_last_seq {
        conn.execute_batch(
            "ALTER TABLE threads ADD COLUMN last_seq INTEGER NOT NULL DEFAULT -1;
             ALTER TABLE threads ADD COLUMN last_hash TEXT;",
        )?;
    }
    // message_count is a later addition than last_seq / last_hash. It
    // counts user+assistant entries (real conversational messages), as
    // opposed to last_seq which counts every log entry including
    // turn_start/end, tool_call, completion_end, etc. Reconcile
    // backfills from the ledger at startup.
    let has_message_count: bool = conn
        .prepare("SELECT message_count FROM threads LIMIT 0")
        .is_ok();
    if !has_message_count {
        conn.execute_batch(
            "ALTER TABLE threads ADD COLUMN message_count INTEGER NOT NULL DEFAULT 0;",
        )?;
    }

    // -- Search tables (unified messages + FTS5) --------------------------------

    conn.execute_batch(
        "
        PRAGMA journal_mode=WAL;

        CREATE TABLE IF NOT EXISTS messages (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            thread_id  TEXT NOT NULL,
            role       TEXT NOT NULL,
            content    TEXT NOT NULL,
            entry_type TEXT NOT NULL,
            context    TEXT NOT NULL DEFAULT '',
            seq        INTEGER NOT NULL DEFAULT 0,
            hash       TEXT,
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS index_state (
            thread_id TEXT PRIMARY KEY,
            last_seq  INTEGER NOT NULL DEFAULT 0
        );
        ",
    )?;

    // Migrate: add context column if missing (databases from before consolidation)
    let has_context: bool = conn.prepare("SELECT context FROM messages LIMIT 0").is_ok();
    if !has_context {
        conn.execute_batch("ALTER TABLE messages ADD COLUMN context TEXT NOT NULL DEFAULT '';")
            .ok();
    }

    // Drop legacy inputs table if it exists (consolidated into messages)
    conn.execute_batch(
        "DROP TABLE IF EXISTS inputs;
         DROP TABLE IF EXISTS inputs_fts;
         DROP TRIGGER IF EXISTS inputs_ai;
         DROP TRIGGER IF EXISTS inputs_ad;",
    )
    .ok();

    // FTS5 virtual table for messages
    let has_messages_fts: bool = conn
        .prepare("SELECT rowid FROM messages_fts LIMIT 0")
        .is_ok();
    if !has_messages_fts {
        conn.execute_batch(
            "
            CREATE VIRTUAL TABLE messages_fts USING fts5(
                content, content=messages, content_rowid=id
            );
            CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
                INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
            END;
            CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content) VALUES ('delete', old.id, old.content);
            END;
            ",
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_db_has_message_count_column() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        // A SELECT on the column should succeed without error.
        assert!(
            conn.prepare("SELECT message_count FROM threads LIMIT 0")
                .is_ok()
        );
    }

    #[test]
    fn migrates_legacy_threads_table_missing_message_count() {
        // Simulate a DB from before `message_count` landed: create the
        // schema by hand without that column, then run initialize and
        // verify the migration added it with a safe default.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE threads (
                id            TEXT PRIMARY KEY,
                title         TEXT NOT NULL,
                parent_id     TEXT,
                inbox_state   TEXT NOT NULL DEFAULT 'inbox',
                thread_state  TEXT NOT NULL DEFAULT 'running',
                block_reason  TEXT,
                created_at    INTEGER NOT NULL,
                updated_at    INTEGER NOT NULL,
                token_count   INTEGER NOT NULL DEFAULT 0,
                last_seq      INTEGER NOT NULL DEFAULT -1,
                last_hash     TEXT
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO threads (id, title, created_at, updated_at) VALUES ('t_old', 'legacy', 1, 2)",
            [],
        )
        .unwrap();

        // Pre-initialize: the column does NOT exist.
        assert!(
            conn.prepare("SELECT message_count FROM threads LIMIT 0")
                .is_err()
        );

        initialize(&conn).unwrap();

        // Post-initialize: the column exists and the pre-existing row
        // got the 0 default.
        let count: i64 = conn
            .query_row(
                "SELECT message_count FROM threads WHERE id = 't_old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "legacy rows must default to 0");

        // Schema is now current: inserts can include the new column.
        conn.execute(
            "INSERT INTO threads (id, title, created_at, updated_at, message_count) \
             VALUES ('t_new', 'fresh', 1, 2, 42)",
            [],
        )
        .unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT message_count FROM threads WHERE id = 't_new'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 42);
    }

    #[test]
    fn initialize_is_idempotent() {
        // Re-running initialize on a fully-migrated DB must be a no-op.
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        initialize(&conn).unwrap();
        initialize(&conn).unwrap();
        assert!(
            conn.prepare("SELECT message_count FROM threads LIMIT 0")
                .is_ok()
        );
    }

    #[test]
    fn migration_persists_across_on_disk_reopen() {
        // In-memory SQLite differs from on-disk in locking + WAL
        // behavior. Run a real file-backed migration to prove the
        // ALTER TABLE survives close/reopen and data written after the
        // migration is readable on a fresh connection.
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("inbox.db");

        // First boot: fresh DB, run migration, insert a row with the
        // new column populated, close.
        {
            let conn = Connection::open(&db_path).unwrap();
            initialize(&conn).unwrap();
            conn.execute(
                "INSERT INTO threads (id, title, created_at, updated_at, message_count) \
                 VALUES ('t_disk', 'disk test', 1, 2, 17)",
                [],
            )
            .unwrap();
        }

        // Second boot: reopen the on-disk file, run migration again
        // (must be idempotent), verify the column + value persisted.
        {
            let conn = Connection::open(&db_path).unwrap();
            initialize(&conn).unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT message_count FROM threads WHERE id = 't_disk'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 17);
        }
    }

    #[test]
    fn migrates_current_pre_remote_schema_additively_and_idempotently() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("ox.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            initialize(&conn).unwrap();
            conn.execute(
                "INSERT INTO threads (id, title, created_at, updated_at) VALUES ('existing', 'keep me', 1, 2)",
                [],
            )
            .unwrap();
            // The immediately preceding repository schema had every table
            // initialized above except these additive remote tables.
            conn.execute_batch(
                "DROP TABLE remote_cached_ledger_entries;
                 DROP TABLE remote_ledger_cursors;
                 DROP TABLE remote_operations;
                 DROP TABLE remote_conversations;
                 DROP TABLE remote_nodes;",
            )
            .unwrap();
        }
        {
            let conn = Connection::open(&db_path).unwrap();
            initialize(&conn).unwrap();
            initialize(&conn).unwrap();
            let title: String = conn
                .query_row("SELECT title FROM threads WHERE id='existing'", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(title, "keep me");
            for table in [
                "remote_nodes",
                "remote_conversations",
                "remote_operations",
                "remote_cached_ledger_entries",
                "remote_ledger_cursors",
            ] {
                let exists: bool = conn
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                        [table],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert!(exists, "missing additive table {table}");
            }
        }
    }

    #[test]
    fn migrates_task8_remote_shape_without_losing_related_rows() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        conn.execute_batch(
            "INSERT INTO remote_nodes VALUES (
                'node-1','attempt-1','exe.dev','oxnode1','203.0.113.1',22,NULL,
                'route@203.0.113.1','/tmp/id','/tmp/known','/tmp/worker.sock',
                'active','ready','none','sha256:image','hash-node',1,1
             );
             INSERT INTO remote_conversations VALUES (
                'conversation-1','node-1','attempt-1','t_remote','create-1',
                'title','prompt',NULL,'fresh_node','active','running','none',
                'hash-conversation',1,1
             );
             INSERT INTO remote_operations (
                operation_id,operation_kind,node_id,node_attempt_id,conversation_id,
                request_hash,intent_json,state,created_at,updated_at
             ) VALUES (
                'operation-1','send_message','node-1','attempt-1','conversation-1',
                'hash-operation',x'7b7d','applied',1,1
             );
             CREATE TABLE remote_nodes_task8 (
                node_id TEXT PRIMARY KEY, node_attempt_id TEXT NOT NULL,
                provider TEXT NOT NULL, vm_name TEXT NOT NULL UNIQUE,
                ssh_host TEXT NOT NULL, ssh_port INTEGER NOT NULL, ssh_user TEXT,
                ssh_dest TEXT NOT NULL, identity_path TEXT NOT NULL,
                known_hosts_path TEXT NOT NULL, worker_socket_path TEXT NOT NULL,
                desired_state TEXT NOT NULL, observed_state TEXT NOT NULL,
                cleanup_state TEXT NOT NULL, image_digest TEXT,
                request_hash TEXT NOT NULL, created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
             );
             INSERT INTO remote_nodes_task8 SELECT * FROM remote_nodes;
             DROP TABLE remote_nodes;
             ALTER TABLE remote_nodes_task8 RENAME TO remote_nodes;
             ALTER TABLE remote_operations DROP COLUMN lease_epoch;",
        )
        .unwrap();

        initialize(&conn).unwrap();
        initialize(&conn).unwrap();
        for (table, expected) in [
            ("remote_nodes", 1_i64),
            ("remote_conversations", 1),
            ("remote_operations", 1),
        ] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, expected, "lost rows from {table}");
        }
        let provider_nullability: Vec<(String, i64)> = conn
            .prepare("PRAGMA table_info(remote_nodes)")
            .unwrap()
            .query_map([], |row| Ok((row.get(1)?, row.get(3)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for field in ["ssh_host", "ssh_dest"] {
            assert_eq!(
                provider_nullability
                    .iter()
                    .find(|(name, _)| name == field)
                    .map(|(_, not_null)| *not_null),
                Some(0)
            );
        }
        assert!(
            conn.prepare("SELECT lease_epoch FROM remote_operations LIMIT 0")
                .is_ok()
        );
        let relation: String = conn
            .query_row(
                "SELECT c.node_id FROM remote_conversations c JOIN remote_nodes n ON n.node_id=c.node_id WHERE c.conversation_id='conversation-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(relation, "node-1");
    }
}

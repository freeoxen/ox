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
        ",
    )?;

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
}

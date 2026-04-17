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
            last_hash     TEXT
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

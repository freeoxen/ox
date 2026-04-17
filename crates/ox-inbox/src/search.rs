//! Search engine — FTS5-backed full-text search over the unified messages table.
//!
//! All functions take a bare `&Connection` (caller holds the mutex).
//! Integrated into InboxStore via the StructFS read/write dispatch.

use crate::ledger::LedgerEntry;
use rusqlite::Connection;
use structfs_core_store::Value;

// ---------------------------------------------------------------------------
// Record an input (inserts into messages with entry_type='input')
// ---------------------------------------------------------------------------

pub fn record_input(
    conn: &Connection,
    text: &str,
    thread_id: &str,
    context: &str,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO messages (thread_id, role, content, entry_type, context) \
         VALUES (?1, 'user', ?2, 'input', ?3)",
        rusqlite::params![thread_id, text, context],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Index ledger entries
// ---------------------------------------------------------------------------

/// Extract displayable text content from a ledger entry message.
fn extract_text(msg: &serde_json::Value) -> Option<(String, String)> {
    let entry_type = msg.get("type").and_then(|v| v.as_str())?;
    match entry_type {
        "user" => {
            let content = msg.get("content").and_then(|v| v.as_str())?;
            Some(("user".into(), content.to_string()))
        }
        "assistant" => {
            let content = msg.get("content")?;
            let text = if let Some(s) = content.as_str() {
                s.to_string()
            } else if let Some(arr) = content.as_array() {
                arr.iter()
                    .filter_map(|block| {
                        if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                            block.get("text").and_then(|v| v.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                return None;
            };
            if text.is_empty() {
                None
            } else {
                Some(("assistant".into(), text))
            }
        }
        "tool_result" => {
            let output = msg.get("output").and_then(|v| v.as_str())?;
            if output.is_empty() {
                None
            } else {
                Some(("tool_result".into(), output.to_string()))
            }
        }
        _ => None,
    }
}

/// Index ledger entries for a thread, starting from `from_seq`.
pub fn index_ledger_entries(
    conn: &Connection,
    thread_id: &str,
    entries: &[LedgerEntry],
    from_seq: u64,
) -> Result<(), String> {
    for entry in entries {
        if entry.seq < from_seq {
            continue;
        }
        let entry_type = entry.msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if let Some((role, text)) = extract_text(&entry.msg) {
            conn.execute(
                "INSERT INTO messages (thread_id, role, content, entry_type, seq, hash) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![thread_id, role, text, entry_type, entry.seq, entry.hash],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    if let Some(last) = entries.last() {
        conn.execute(
            "INSERT INTO index_state (thread_id, last_seq) VALUES (?1, ?2) \
             ON CONFLICT(thread_id) DO UPDATE SET last_seq = ?2",
            rusqlite::params![thread_id, last.seq],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn get_index_state(conn: &Connection, thread_id: &str) -> Option<u64> {
    conn.query_row(
        "SELECT last_seq FROM index_state WHERE thread_id = ?1",
        [thread_id],
        |row| row.get::<_, i64>(0),
    )
    .ok()
    .map(|n| n as u64)
}

// ---------------------------------------------------------------------------
// FTS5 query sanitization
// ---------------------------------------------------------------------------

/// Sanitize a user query for FTS5 MATCH.
///
/// Each word is quoted individually and joined with AND. This means
/// "foo bar" matches "foo baz bar" (all words present, any order).
/// Special characters like `*`, `(`, `)` are treated as literals
/// because each token is wrapped in double quotes.
fn sanitize_fts_query(query: &str) -> String {
    let words: Vec<String> = query
        .split_whitespace()
        .map(|w| {
            let escaped = w.replace('"', "\"\"");
            format!("\"{escaped}\"")
        })
        .collect();
    if words.is_empty() {
        "\"\"".into()
    } else {
        words.join(" AND ")
    }
}

// ---------------------------------------------------------------------------
// Search queries
// ---------------------------------------------------------------------------

/// FTS5 search over user inputs (entry_type='input').
pub fn search_inputs(conn: &Connection, query: &str, limit: usize) -> Result<Vec<Value>, String> {
    let fts_query = sanitize_fts_query(query);
    let mut stmt = conn
        .prepare(
            "SELECT m.id, m.content, m.thread_id, m.context, m.seq, m.created_at \
             FROM messages_fts f JOIN messages m ON f.rowid = m.id \
             WHERE messages_fts MATCH ?1 AND m.entry_type = 'input' \
             ORDER BY rank LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map(rusqlite::params![fts_query, limit as i64], |row| {
            Ok(input_row_to_value(row))
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// FTS5 search over all messages.
pub fn search_messages(conn: &Connection, query: &str, limit: usize) -> Result<Vec<Value>, String> {
    let fts_query = sanitize_fts_query(query);
    let mut stmt = conn
        .prepare(
            "SELECT m.id, m.thread_id, m.role, m.content, m.entry_type, m.seq, m.hash, \
                    m.created_at, snippet(messages_fts, 0, '>>>', '<<<', '...', 32) as snip \
             FROM messages_fts f JOIN messages m ON f.rowid = m.id \
             WHERE messages_fts MATCH ?1 \
             ORDER BY rank LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map(rusqlite::params![fts_query, limit as i64], |row| {
            Ok(message_row_to_value(row))
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// FTS5 search returning matching thread_ids (for inbox search integration).
/// Runs separately from the LIKE query so FTS5 errors don't kill title search.
pub fn search_thread_ids_by_content(conn: &Connection, query: &str, limit: usize) -> Vec<String> {
    let fts_query = sanitize_fts_query(query);
    let mut stmt = match conn.prepare(
        "SELECT DISTINCT m.thread_id \
         FROM messages_fts f JOIN messages m ON f.rowid = m.id \
         WHERE messages_fts MATCH ?1 \
         LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    match stmt.query_map(rusqlite::params![fts_query, limit as i64], |row| {
        row.get::<_, String>(0)
    }) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    }
}

/// Recent user inputs ordered by newest first.
pub fn recent_inputs(conn: &Connection, limit: usize) -> Result<Vec<Value>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, content, thread_id, context, seq, created_at \
             FROM messages WHERE entry_type = 'input' ORDER BY id DESC LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([limit as i64], |row| Ok(input_row_to_value(row)))
        .map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Rebuild from ledger files
// ---------------------------------------------------------------------------

pub fn rebuild_index(conn: &Connection, threads_dir: &std::path::Path) -> Result<(), String> {
    conn.execute_batch("DELETE FROM messages; DELETE FROM index_state;")
        .map_err(|e| e.to_string())?;

    let entries = std::fs::read_dir(threads_dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let thread_id = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };
        let ledger_path = path.join("ledger.jsonl");
        if !ledger_path.exists() {
            continue;
        }
        if let Ok(ledger_entries) = crate::ledger::read_ledger(&ledger_path) {
            index_ledger_entries(conn, &thread_id, &ledger_entries, 0).ok();
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Value conversion helpers
// ---------------------------------------------------------------------------

fn input_row_to_value(row: &rusqlite::Row<'_>) -> Value {
    let mut map = std::collections::BTreeMap::new();
    if let Ok(id) = row.get::<_, i64>(0) {
        map.insert("id".into(), Value::Integer(id));
    }
    if let Ok(text) = row.get::<_, String>(1) {
        map.insert("text".into(), Value::String(text));
    }
    if let Ok(tid) = row.get::<_, String>(2) {
        map.insert("thread_id".into(), Value::String(tid));
    }
    if let Ok(ctx) = row.get::<_, String>(3) {
        map.insert("context".into(), Value::String(ctx));
    }
    if let Ok(Some(s)) = row.get::<_, Option<i64>>(4) {
        map.insert("seq".into(), Value::Integer(s));
    }
    if let Ok(ts) = row.get::<_, i64>(5) {
        map.insert("created_at".into(), Value::Integer(ts));
    }
    Value::Map(map)
}

fn message_row_to_value(row: &rusqlite::Row<'_>) -> Value {
    let mut map = std::collections::BTreeMap::new();
    if let Ok(id) = row.get::<_, i64>(0) {
        map.insert("id".into(), Value::Integer(id));
    }
    if let Ok(tid) = row.get::<_, String>(1) {
        map.insert("thread_id".into(), Value::String(tid));
    }
    if let Ok(role) = row.get::<_, String>(2) {
        map.insert("role".into(), Value::String(role));
    }
    if let Ok(content) = row.get::<_, String>(3) {
        map.insert("content".into(), Value::String(content));
    }
    if let Ok(et) = row.get::<_, String>(4) {
        map.insert("entry_type".into(), Value::String(et));
    }
    if let Ok(seq) = row.get::<_, i64>(5) {
        map.insert("seq".into(), Value::Integer(seq));
    }
    if let Ok(Some(h)) = row.get::<_, Option<String>>(6) {
        map.insert("hash".into(), Value::String(h));
    }
    if let Ok(ts) = row.get::<_, i64>(7) {
        map.insert("created_at".into(), Value::Integer(ts));
    }
    if let Ok(snip) = row.get::<_, String>(8) {
        map.insert("snippet".into(), Value::String(snip));
    }
    Value::Map(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::schema::initialize(&conn).unwrap();
        conn
    }

    #[test]
    fn record_and_search_input() {
        let conn = test_conn();
        record_input(&conn, "fix the auth middleware", "t_1", "compose").unwrap();
        record_input(&conn, "add pagination to API", "t_2", "compose").unwrap();

        let results = search_inputs(&conn, "auth", 10).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            Value::Map(m) => {
                assert_eq!(
                    m.get("text"),
                    Some(&Value::String("fix the auth middleware".into()))
                );
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn recent_inputs_ordered() {
        let conn = test_conn();
        record_input(&conn, "first", "t_1", "compose").unwrap();
        record_input(&conn, "second", "t_1", "reply").unwrap();
        record_input(&conn, "third", "t_2", "compose").unwrap();

        let results = recent_inputs(&conn, 2).unwrap();
        assert_eq!(results.len(), 2);
        match &results[0] {
            Value::Map(m) => assert_eq!(m.get("text"), Some(&Value::String("third".into()))),
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn index_and_search_messages() {
        let conn = test_conn();
        let entries = vec![
            LedgerEntry {
                seq: 0,
                hash: "abc".into(),
                parent: None,
                msg: serde_json::json!({"type": "user", "content": "explain authentication"}),
            },
            LedgerEntry {
                seq: 1,
                hash: "def".into(),
                parent: Some("abc".into()),
                msg: serde_json::json!({"type": "assistant", "content": [{"type": "text", "text": "Authentication is the process of verifying identity."}]}),
            },
        ];
        index_ledger_entries(&conn, "t_1", &entries, 0).unwrap();

        let results = search_messages(&conn, "authentication", 10).unwrap();
        assert_eq!(results.len(), 2);

        let state = get_index_state(&conn, "t_1");
        assert_eq!(state, Some(1));
    }

    #[test]
    fn incremental_indexing() {
        let conn = test_conn();
        let entries = vec![
            LedgerEntry {
                seq: 0,
                hash: "a".into(),
                parent: None,
                msg: serde_json::json!({"type": "user", "content": "hello"}),
            },
            LedgerEntry {
                seq: 1,
                hash: "b".into(),
                parent: Some("a".into()),
                msg: serde_json::json!({"type": "user", "content": "world"}),
            },
        ];
        index_ledger_entries(&conn, "t_1", &entries, 0).unwrap();

        let entries2 = vec![LedgerEntry {
            seq: 2,
            hash: "c".into(),
            parent: Some("b".into()),
            msg: serde_json::json!({"type": "user", "content": "again"}),
        }];
        index_ledger_entries(&conn, "t_1", &entries2, 2).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn fts_query_sanitized() {
        // Words joined with AND — "foo bar" matches "foo baz bar"
        assert_eq!(sanitize_fts_query("fix bug"), "\"fix\" AND \"bug\"");
        // Special chars treated as literals
        assert_eq!(sanitize_fts_query("auth*"), "\"auth*\"");
        // Quotes escaped
        assert_eq!(
            sanitize_fts_query("say \"hello\""),
            "\"say\" AND \"\"\"hello\"\"\""
        );
    }

    #[test]
    fn fuzzy_match_words_any_order() {
        let conn = test_conn();
        record_input(&conn, "foo baz bar", "t_1", "compose").unwrap();
        record_input(&conn, "completely unrelated", "t_2", "compose").unwrap();

        // "foo bar" should match "foo baz bar" (both words present)
        let results = search_inputs(&conn, "foo bar", 10).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0] {
            Value::Map(m) => {
                assert_eq!(m.get("text"), Some(&Value::String("foo baz bar".into())));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn search_thread_ids_returns_distinct() {
        let conn = test_conn();
        let entries = vec![
            LedgerEntry {
                seq: 0,
                hash: "a".into(),
                parent: None,
                msg: serde_json::json!({"type": "user", "content": "authentication flow"}),
            },
            LedgerEntry {
                seq: 1,
                hash: "b".into(),
                parent: Some("a".into()),
                msg: serde_json::json!({"type": "user", "content": "more authentication"}),
            },
        ];
        index_ledger_entries(&conn, "t_1", &entries, 0).unwrap();

        let ids = search_thread_ids_by_content(&conn, "authentication", 10);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], "t_1");
    }

    #[test]
    fn inputs_and_messages_in_same_table() {
        let conn = test_conn();
        // Record an input
        record_input(&conn, "fix auth", "t_1", "compose").unwrap();
        // Index a ledger entry
        let entries = vec![LedgerEntry {
            seq: 0,
            hash: "a".into(),
            parent: None,
            msg: serde_json::json!({"type": "user", "content": "explain auth"}),
        }];
        index_ledger_entries(&conn, "t_1", &entries, 0).unwrap();

        // Total messages: 2 (1 input + 1 ledger)
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);

        // recent_inputs only returns the input, not the ledger entry
        let recent = recent_inputs(&conn, 10).unwrap();
        assert_eq!(recent.len(), 1);

        // search_messages finds both
        let results = search_messages(&conn, "auth", 10).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn extract_text_user() {
        let msg = serde_json::json!({"type": "user", "content": "hello world"});
        let (role, text) = extract_text(&msg).unwrap();
        assert_eq!(role, "user");
        assert_eq!(text, "hello world");
    }

    #[test]
    fn extract_text_assistant_blocks() {
        let msg = serde_json::json!({"type": "assistant", "content": [
            {"type": "text", "text": "part one"},
            {"type": "tool_use", "id": "t1", "name": "read_file"},
            {"type": "text", "text": "part two"},
        ]});
        let (role, text) = extract_text(&msg).unwrap();
        assert_eq!(role, "assistant");
        assert_eq!(text, "part one\npart two");
    }

    #[test]
    fn extract_text_skips_non_textual() {
        let msg = serde_json::json!({"type": "turn_start", "scope": "root"});
        assert!(extract_text(&msg).is_none());
    }
}

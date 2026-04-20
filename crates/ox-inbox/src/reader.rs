use crate::model::{InboxState, TaskInfo, ThreadMetadata, ThreadState};
use crate::pagination;
use rusqlite::Connection;
use std::sync::Mutex;
use structfs_core_store::{Error as StoreError, Path, Record, Value};

fn err(op: &'static str, msg: impl std::fmt::Display) -> StoreError {
    StoreError::store("InboxStore", op, msg.to_string())
}

pub fn read_dispatch(
    db: &Mutex<Connection>,
    last_search_result: &Option<crate::SearchCache>,
    from: &Path,
) -> Result<Option<Record>, StoreError> {
    let segments: Vec<&String> = from.iter().collect();
    match segments.as_slice() {
        [root] if root.as_str() == "threads" => list_threads(db, "inbox"),
        [root] if root.as_str() == "done" => list_threads(db, "done"),
        [root] if root.as_str() == "labels" => list_all_labels(db),
        [root, id] if root.as_str() == "threads" => get_thread(db, id),
        [root, name] if root.as_str() == "labels" => threads_by_label(db, name),
        [root, state] if root.as_str() == "by_state" => threads_by_state(db, state),
        [root, query] if root.as_str() == "search" => search_threads(db, query),
        [root, id, sub] if root.as_str() == "threads" && sub.as_str() == "children" => {
            list_children(db, id)
        }
        [root, id, sub] if root.as_str() == "threads" && sub.as_str() == "tasks" => {
            list_tasks(db, id)
        }
        // --- Paginated search results (StructFS pagination protocol) ---
        // search/results/{handle}
        [a, b, id] if a.as_str() == "search" && b.as_str() == "results" => {
            paginate_search_results(last_search_result, id, None, 20)
        }
        // search/results/{handle}/limit/{n}
        [a, b, id, c, lim]
            if a.as_str() == "search" && b.as_str() == "results" && c.as_str() == "limit" =>
        {
            let limit: usize = lim.parse().unwrap_or(20);
            paginate_search_results(last_search_result, id, None, limit)
        }
        // search/results/{handle}/after/{cursor}/limit/{n}
        [a, b, id, c, cursor, d, lim]
            if a.as_str() == "search"
                && b.as_str() == "results"
                && c.as_str() == "after"
                && d.as_str() == "limit" =>
        {
            let limit: usize = lim.parse().unwrap_or(20);
            paginate_search_results(last_search_result, id, Some(cursor.as_str()), limit)
        }
        // Recent inputs: inputs/recent or inputs/recent/{limit}
        [a, b] if a.as_str() == "inputs" && b.as_str() == "recent" => {
            let conn = db.lock().map_err(|e| err("read", e))?;
            let results = crate::search::recent_inputs(&conn, 50).map_err(|e| err("read", e))?;
            Ok(Some(Record::parsed(Value::Array(results))))
        }
        [a, b, lim] if a.as_str() == "inputs" && b.as_str() == "recent" => {
            let limit: usize = lim.parse().unwrap_or(50);
            let conn = db.lock().map_err(|e| err("read", e))?;
            let results = crate::search::recent_inputs(&conn, limit).map_err(|e| err("read", e))?;
            Ok(Some(Record::parsed(Value::Array(results))))
        }
        _ => Ok(None),
    }
}

/// Paginate a cached search result set using StructFS cursor-based pagination.
fn paginate_search_results(
    cache: &Option<crate::SearchCache>,
    handle: &str,
    after_cursor: Option<&str>,
    limit: usize,
) -> Result<Option<Record>, StoreError> {
    match cache {
        Some(cached) if cached.handle == handle => {
            let base_path = format!("search/results/{handle}");
            // Use "id" as cursor field for threads, "id" for inputs too
            let page = pagination::paginate(&cached.items, &base_path, "id", after_cursor, limit);
            Ok(Some(Record::parsed(page.to_value())))
        }
        _ => Ok(None),
    }
}

fn row_to_metadata(row: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadMetadata> {
    let inbox_str: String = row.get(3)?;
    let thread_str: String = row.get(4)?;
    Ok(ThreadMetadata {
        id: row.get(0)?,
        title: row.get(1)?,
        parent_id: row.get(2)?,
        inbox_state: InboxState::parse(&inbox_str).unwrap_or(InboxState::Inbox),
        thread_state: ThreadState::parse(&thread_str).unwrap_or(ThreadState::Running),
        block_reason: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        token_count: row.get(8)?,
        labels: Vec::new(),
        last_seq: row.get(9)?,
        last_hash: row.get(10)?,
        message_count: row.get(11)?,
    })
}

fn query_threads(
    conn: &Connection,
    where_clause: &str,
    params: &[&dyn rusqlite::types::ToSql],
) -> Result<Vec<ThreadMetadata>, StoreError> {
    let sql = format!(
        "SELECT id, title, parent_id, inbox_state, thread_state, block_reason, \
         created_at, updated_at, token_count, last_seq, last_hash, message_count \
         FROM threads WHERE {} ORDER BY updated_at DESC",
        where_clause
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| err("read", e))?;
    let mut threads: Vec<ThreadMetadata> = stmt
        .query_map(params, row_to_metadata)
        .map_err(|e| err("read", e))?
        .collect::<Result<_, _>>()
        .map_err(|e| err("read", e))?;

    if !threads.is_empty() {
        batch_load_labels(conn, &mut threads)?;
    }
    Ok(threads)
}

fn batch_load_labels(conn: &Connection, threads: &mut [ThreadMetadata]) -> Result<(), StoreError> {
    // Build a single query for all thread IDs
    let placeholders: Vec<String> = (1..=threads.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "SELECT thread_id, label FROM labels WHERE thread_id IN ({}) ORDER BY thread_id, label",
        placeholders.join(", ")
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| err("read", e))?;
    let ids: Vec<&str> = threads.iter().map(|t| t.id.as_str()).collect();
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = ids
        .iter()
        .map(|id| id as &dyn rusqlite::types::ToSql)
        .collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| err("read", e))?;

    // Build a map of thread_id -> labels
    let mut label_map: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for row in rows {
        let (thread_id, label) = row.map_err(|e| err("read", e))?;
        label_map.entry(thread_id).or_default().push(label);
    }

    // Assign labels to threads
    for thread in threads {
        if let Some(labels) = label_map.remove(&thread.id) {
            thread.labels = labels;
        }
    }
    Ok(())
}

fn threads_to_record(threads: Vec<ThreadMetadata>) -> Result<Option<Record>, StoreError> {
    let values: Vec<Value> = threads.into_iter().map(|t| t.to_value()).collect();
    Ok(Some(Record::parsed(Value::Array(values))))
}

fn list_threads(db: &Mutex<Connection>, inbox_state: &str) -> Result<Option<Record>, StoreError> {
    let conn = db.lock().map_err(|e| err("read", e))?;
    let threads = query_threads(&conn, "inbox_state = ?1", &[&inbox_state])?;
    threads_to_record(threads)
}

fn get_thread(db: &Mutex<Connection>, id: &str) -> Result<Option<Record>, StoreError> {
    let conn = db.lock().map_err(|e| err("read", e))?;
    let mut threads = query_threads(&conn, "id = ?1", &[&id])?;
    match threads.len() {
        0 => Ok(None),
        _ => Ok(Some(Record::parsed(threads.remove(0).to_value()))),
    }
}

fn list_children(db: &Mutex<Connection>, parent_id: &str) -> Result<Option<Record>, StoreError> {
    let conn = db.lock().map_err(|e| err("read", e))?;
    let threads = query_threads(&conn, "parent_id = ?1", &[&parent_id])?;
    threads_to_record(threads)
}

fn list_tasks(db: &Mutex<Connection>, thread_id: &str) -> Result<Option<Record>, StoreError> {
    let conn = db.lock().map_err(|e| err("read", e))?;
    let mut stmt = conn
        .prepare(
            "SELECT id, thread_id, title, status, created_at, updated_at \
             FROM tasks WHERE thread_id = ?1 ORDER BY created_at ASC",
        )
        .map_err(|e| err("read", e))?;
    let tasks: Vec<TaskInfo> = stmt
        .query_map([thread_id], |row| {
            Ok(TaskInfo {
                id: row.get(0)?,
                thread_id: row.get(1)?,
                title: row.get(2)?,
                status: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })
        .map_err(|e| err("read", e))?
        .collect::<Result<_, _>>()
        .map_err(|e| err("read", e))?;
    let values: Vec<Value> = tasks.into_iter().map(|t| t.to_value()).collect();
    Ok(Some(Record::parsed(Value::Array(values))))
}

fn list_all_labels(db: &Mutex<Connection>) -> Result<Option<Record>, StoreError> {
    let conn = db.lock().map_err(|e| err("read", e))?;
    let mut stmt = conn
        .prepare("SELECT DISTINCT label FROM labels ORDER BY label")
        .map_err(|e| err("read", e))?;
    let labels: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .map_err(|e| err("read", e))?
        .collect::<Result<_, _>>()
        .map_err(|e| err("read", e))?;
    let values: Vec<Value> = labels.into_iter().map(Value::String).collect();
    Ok(Some(Record::parsed(Value::Array(values))))
}

fn threads_by_label(db: &Mutex<Connection>, label: &str) -> Result<Option<Record>, StoreError> {
    let conn = db.lock().map_err(|e| err("read", e))?;
    let threads = query_threads(
        &conn,
        "inbox_state = 'inbox' AND id IN (SELECT thread_id FROM labels WHERE label = ?1)",
        &[&label],
    )?;
    threads_to_record(threads)
}

fn threads_by_state(db: &Mutex<Connection>, state: &str) -> Result<Option<Record>, StoreError> {
    let conn = db.lock().map_err(|e| err("read", e))?;
    let threads = query_threads(
        &conn,
        "inbox_state = 'inbox' AND thread_state = ?1",
        &[&state],
    )?;
    threads_to_record(threads)
}

/// Search threads by title, label, and content, returning Values with match metadata.
///
/// Called from InboxStore::execute_search for the "threads" scope.
pub(crate) fn search_threads_to_values(
    conn: &Connection,
    query: &str,
) -> Result<Vec<Value>, StoreError> {
    if query.is_empty() {
        // Empty query returns all inbox threads (unfiltered)
        let threads = query_threads(conn, "inbox_state = 'inbox'", &[])?;
        return Ok(threads.into_iter().map(|t| t.to_value()).collect());
    }

    let pattern = format!("%{}%", escape_like(query));

    // FTS5 content search — returns thread_ids with snippets
    let content_matches = crate::search::search_thread_ids_with_snippets(conn, query, 50);
    let content_ids: std::collections::HashSet<String> =
        content_matches.iter().map(|(id, _)| id.clone()).collect();
    let snippet_map: std::collections::HashMap<String, String> =
        content_matches.into_iter().collect();

    // Title/label LIKE search
    let title_match_ids: std::collections::HashSet<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT id FROM threads WHERE inbox_state = 'inbox' \
                 AND title LIKE ?1 ESCAPE '\\'",
            )
            .map_err(|e| err("read", e))?;
        stmt.query_map([&pattern as &dyn rusqlite::types::ToSql], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|e| err("read", e))?
        .filter_map(|r| r.ok())
        .collect()
    };

    let label_match_ids: std::collections::HashSet<String> = {
        let mut stmt = conn
            .prepare("SELECT DISTINCT thread_id FROM labels WHERE label LIKE ?1 ESCAPE '\\'")
            .map_err(|e| err("read", e))?;
        stmt.query_map([&pattern as &dyn rusqlite::types::ToSql], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|e| err("read", e))?
        .filter_map(|r| r.ok())
        .collect()
    };

    // Merge all matching thread IDs
    let mut all_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    all_ids.extend(content_ids.iter().cloned());
    all_ids.extend(title_match_ids.iter().cloned());
    all_ids.extend(label_match_ids.iter().cloned());

    if all_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Fetch full thread metadata for matching IDs
    let placeholders: String = all_ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let where_clause = format!("inbox_state = 'inbox' AND id IN ({placeholders})");
    let id_vec: Vec<String> = all_ids.into_iter().collect();
    let params: Vec<&dyn rusqlite::types::ToSql> = id_vec
        .iter()
        .map(|s| s as &dyn rusqlite::types::ToSql)
        .collect();
    let threads = query_threads(conn, &where_clause, &params)?;

    // Annotate with match_source and snippet
    let values: Vec<Value> = threads
        .into_iter()
        .map(|t| {
            let mut val = t.to_value();
            if let Value::Map(ref mut map) = val {
                let in_title = title_match_ids.contains(&t.id);
                let in_label = label_match_ids.contains(&t.id);
                let in_content = content_ids.contains(&t.id);
                let source = match (in_title, in_label, in_content) {
                    (true, _, true) | (_, true, true) => "multiple",
                    (true, _, _) => "title",
                    (_, true, _) => "label",
                    (_, _, true) => "content",
                    _ => "title",
                };
                map.insert(
                    "match_source".to_string(),
                    Value::String(source.to_string()),
                );
                if let Some(snippet) = snippet_map.get(&t.id) {
                    map.insert("snippet".to_string(), Value::String(snippet.clone()));
                }
            }
            val
        })
        .collect();

    Ok(values)
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn search_threads(db: &Mutex<Connection>, query: &str) -> Result<Option<Record>, StoreError> {
    let conn = db.lock().map_err(|e| err("read", e))?;
    let pattern = format!("%{}%", escape_like(query));

    // Run FTS5 content search separately so a malformed query can't kill title/label search.
    let content_thread_ids = crate::search::search_thread_ids_by_content(&conn, query, 50);

    if content_thread_ids.is_empty() {
        // No content matches — just search titles and labels
        let threads = query_threads(
            &conn,
            "inbox_state = 'inbox' AND (title LIKE ?1 ESCAPE '\\' OR id IN \
             (SELECT thread_id FROM labels WHERE label LIKE ?1 ESCAPE '\\'))",
            &[&pattern as &dyn rusqlite::types::ToSql],
        )?;
        threads_to_record(threads)
    } else {
        // Merge content matches with title/label matches
        let placeholders: String = content_thread_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let where_clause = format!(
            "inbox_state = 'inbox' AND (title LIKE ?1 ESCAPE '\\' OR id IN \
             (SELECT thread_id FROM labels WHERE label LIKE ?1 ESCAPE '\\') OR id IN ({placeholders}))"
        );
        let mut params: Vec<&dyn rusqlite::types::ToSql> = vec![&pattern];
        for id in &content_thread_ids {
            params.push(id);
        }
        let threads = query_threads(&conn, &where_clause, &params)?;
        threads_to_record(threads)
    }
}

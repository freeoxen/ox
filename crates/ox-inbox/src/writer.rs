use crate::model::{InboxState, ThreadState};
use ox_kernel::oxpath;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::Path as FsPath;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use structfs_core_store::{Error as StoreError, Path, Record, Value};

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn err(op: &'static str, msg: impl std::fmt::Display) -> StoreError {
    StoreError::store("InboxStore", op, msg.to_string())
}

pub fn write_dispatch(
    db: &Mutex<Connection>,
    threads_dir: &FsPath,
    to: &Path,
    data: Record,
) -> Result<Path, StoreError> {
    let segments: Vec<&String> = to.iter().collect();
    match segments.as_slice() {
        [root] if root.as_str() == "threads" => create_thread(db, threads_dir, &data),
        [root, id] if root.as_str() == "threads" => update_thread(db, id, &data),
        [root, id, sub] if root.as_str() == "threads" && sub.as_str() == "labels" => {
            set_labels(db, id, &data)
        }
        [root, id, sub] if root.as_str() == "threads" && sub.as_str() == "tasks" => {
            create_task(db, id, &data)
        }
        [root, id, sub, task_id] if root.as_str() == "threads" && sub.as_str() == "tasks" => {
            update_task(db, id, task_id, &data)
        }
        _ => Err(err("write", format!("unrecognized path: {}", to))),
    }
}

fn require_map<'a>(
    data: &'a Record,
    op: &'static str,
) -> Result<&'a BTreeMap<String, Value>, StoreError> {
    match data.as_value() {
        Some(Value::Map(map)) => Ok(map),
        _ => Err(err(op, "expected a Map value")),
    }
}

fn get_str<'a>(map: &'a BTreeMap<String, Value>, key: &str) -> Option<&'a str> {
    match map.get(key) {
        Some(Value::String(s)) => Some(s.as_str()),
        _ => None,
    }
}

fn create_thread(
    db: &Mutex<Connection>,
    threads_dir: &FsPath,
    data: &Record,
) -> Result<Path, StoreError> {
    let map = require_map(data, "create_thread")?;
    let title = get_str(map, "title").ok_or_else(|| err("create_thread", "title is required"))?;
    let parent_id = get_str(map, "parent_id");
    let id = format!("t_{}", uuid::Uuid::new_v4().as_simple());
    let now = now_epoch();

    let conn = db.lock().map_err(|e| err("create_thread", e))?;
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| err("create_thread", e))?;

    tx.execute(
        "INSERT INTO threads (id, title, parent_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![id, title, parent_id, now, now],
    )
    .map_err(|e| err("create_thread", e))?;

    if let Some(Value::Array(labels)) = map.get("labels") {
        insert_labels(&tx, &id, labels)?;
    }

    tx.commit().map_err(|e| err("create_thread", e))?;

    // Create thread directory for JSONL storage (outside tx — fs isn't transactional)
    let thread_dir = threads_dir.join(&id);
    std::fs::create_dir_all(&thread_dir).map_err(|e| err("create_thread", e))?;

    Ok(oxpath!("threads", id))
}

fn insert_labels(conn: &Connection, thread_id: &str, labels: &[Value]) -> Result<(), StoreError> {
    for label in labels {
        if let Value::String(l) = label {
            conn.execute(
                "INSERT INTO labels (thread_id, label) VALUES (?1, ?2)",
                rusqlite::params![thread_id, l],
            )
            .map_err(|e| err("insert_labels", e))?;
        }
    }
    Ok(())
}

fn update_thread(db: &Mutex<Connection>, id: &str, data: &Record) -> Result<Path, StoreError> {
    let map = require_map(data, "update_thread")?;

    // Validate state values before touching the database
    if let Some(Value::String(s)) = map.get("inbox_state") {
        if InboxState::parse(s.as_str()).is_none() {
            return Err(err("update_thread", format!("invalid inbox_state: {}", s)));
        }
    }
    if let Some(Value::String(s)) = map.get("thread_state") {
        if ThreadState::parse(s.as_str()).is_none() {
            return Err(err("update_thread", format!("invalid thread_state: {}", s)));
        }
    }

    // Build a single UPDATE with all provided fields
    let now = now_epoch();
    let mut sets: Vec<String> = vec!["updated_at = ?1".to_string()];
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(now)];

    let string_fields = ["title", "inbox_state", "thread_state", "block_reason"];
    for field in string_fields {
        if let Some(Value::String(v)) = map.get(field) {
            params.push(Box::new(v.clone()));
            sets.push(format!("{} = ?{}", field, params.len()));
        }
    }
    if let Some(Value::Integer(n)) = map.get("token_count") {
        params.push(Box::new(*n));
        sets.push(format!("token_count = ?{}", params.len()));
    }
    if let Some(Value::Integer(n)) = map.get("last_seq") {
        params.push(Box::new(*n));
        sets.push(format!("last_seq = ?{}", params.len()));
    }
    if let Some(v) = map.get("last_hash") {
        match v {
            Value::String(s) => {
                params.push(Box::new(s.clone()));
                sets.push(format!("last_hash = ?{}", params.len()));
            }
            Value::Null => {
                params.push(Box::new(rusqlite::types::Null));
                sets.push(format!("last_hash = ?{}", params.len()));
            }
            _ => {}
        }
    }

    params.push(Box::new(id.to_string()));
    let sql = format!(
        "UPDATE threads SET {} WHERE id = ?{}",
        sets.join(", "),
        params.len()
    );

    let conn = db.lock().map_err(|e| err("update_thread", e))?;
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    conn.execute(&sql, param_refs.as_slice())
        .map_err(|e| err("update_thread", e))?;

    let id = id.to_string();
    Ok(oxpath!("threads", id))
}

fn set_labels(db: &Mutex<Connection>, id: &str, data: &Record) -> Result<Path, StoreError> {
    let labels = match data.as_value() {
        Some(Value::Array(arr)) => arr,
        _ => return Err(err("set_labels", "expected an Array value")),
    };

    let conn = db.lock().map_err(|e| err("set_labels", e))?;
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| err("set_labels", e))?;

    tx.execute(
        "DELETE FROM labels WHERE thread_id = ?1",
        rusqlite::params![id],
    )
    .map_err(|e| err("set_labels", e))?;

    insert_labels(&tx, id, labels)?;

    let now = now_epoch();
    tx.execute(
        "UPDATE threads SET updated_at = ?1 WHERE id = ?2",
        rusqlite::params![now, id],
    )
    .map_err(|e| err("set_labels", e))?;

    tx.commit().map_err(|e| err("set_labels", e))?;

    let id = id.to_string();
    Ok(oxpath!("threads", id, "labels"))
}

fn create_task(db: &Mutex<Connection>, thread_id: &str, data: &Record) -> Result<Path, StoreError> {
    let map = require_map(data, "create_task")?;
    let title = get_str(map, "title").ok_or_else(|| err("create_task", "title is required"))?;
    let id = format!("k_{}", uuid::Uuid::new_v4().as_simple());
    let now = now_epoch();

    let conn = db.lock().map_err(|e| err("create_task", e))?;
    conn.execute(
        "INSERT INTO tasks (id, thread_id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![id, thread_id, title, now, now],
    )
    .map_err(|e| err("create_task", e))?;

    let thread_id = thread_id.to_string();
    Ok(oxpath!("threads", thread_id, "tasks", id))
}

fn update_task(
    db: &Mutex<Connection>,
    thread_id: &str,
    task_id: &str,
    data: &Record,
) -> Result<Path, StoreError> {
    let map = require_map(data, "update_task")?;
    let conn = db.lock().map_err(|e| err("update_task", e))?;

    // Verify the task belongs to the thread specified in the path
    let actual_thread_id: String = conn
        .query_row(
            "SELECT thread_id FROM tasks WHERE id = ?1",
            rusqlite::params![task_id],
            |row| row.get(0),
        )
        .map_err(|e| err("update_task", e))?;
    if actual_thread_id != thread_id {
        return Err(err(
            "update_task",
            format!("task {} does not belong to thread {}", task_id, thread_id),
        ));
    }

    // Build single UPDATE
    let now = now_epoch();
    let mut sets: Vec<String> = vec!["updated_at = ?1".to_string()];
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(now)];

    for field in ["title", "status"] {
        if let Some(Value::String(v)) = map.get(field) {
            params.push(Box::new(v.clone()));
            sets.push(format!("{} = ?{}", field, params.len()));
        }
    }

    params.push(Box::new(task_id.to_string()));
    let sql = format!(
        "UPDATE tasks SET {} WHERE id = ?{}",
        sets.join(", "),
        params.len()
    );
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    conn.execute(&sql, param_refs.as_slice())
        .map_err(|e| err("update_task", e))?;

    let thread_id = thread_id.to_string();
    let task_id = task_id.to_string();
    Ok(oxpath!("threads", thread_id, "tasks", task_id))
}

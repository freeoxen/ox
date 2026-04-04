pub(crate) mod jsonl;
pub mod model;
mod reader;
mod schema;
mod writer;

use rusqlite::Connection;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Mutex;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Writer};

#[allow(unused_imports)]
use structfs_core_store::Value;

pub struct InboxStore {
    db: Mutex<Connection>,
    threads_dir: PathBuf,
}

impl InboxStore {
    pub fn open(root: &FsPath) -> Result<Self, StoreError> {
        std::fs::create_dir_all(root)
            .map_err(|e| StoreError::store("InboxStore", "open", e.to_string()))?;
        let db_path = root.join("inbox.db");
        let threads_dir = root.join("threads");
        std::fs::create_dir_all(&threads_dir)
            .map_err(|e| StoreError::store("InboxStore", "open", e.to_string()))?;

        let conn = Connection::open(&db_path)
            .map_err(|e| StoreError::store("InboxStore", "open", e.to_string()))?;
        schema::initialize(&conn)
            .map_err(|e| StoreError::store("InboxStore", "open", e.to_string()))?;

        Ok(Self {
            db: Mutex::new(conn),
            threads_dir,
        })
    }
}

impl Reader for InboxStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        reader::read_dispatch(&self.db, &self.threads_dir, from)
    }
}

impl Writer for InboxStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        writer::write_dispatch(&self.db, &self.threads_dir, to, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> (InboxStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = InboxStore::open(dir.path()).unwrap();
        (store, dir)
    }

    #[test]
    fn schema_creates_all_tables() {
        let (store, _dir) = test_store();
        let db = store.db.lock().unwrap();
        let tables: Vec<String> = db
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(tables.contains(&"threads".to_string()));
        assert!(tables.contains(&"labels".to_string()));
        assert!(tables.contains(&"tasks".to_string()));
    }

    #[test]
    fn open_creates_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("nested/inbox");
        let _store = InboxStore::open(&root).unwrap();
        assert!(root.join("inbox.db").exists());
        assert!(root.join("threads").is_dir());
    }

    #[test]
    fn create_thread_via_write() {
        let (mut store, _dir) = test_store();
        let mut map = std::collections::BTreeMap::new();
        map.insert(
            "title".to_string(),
            Value::String("My new thread".to_string()),
        );
        let record = Record::parsed(Value::Map(map));
        let path = store
            .write(&structfs_core_store::path!("threads"), record)
            .unwrap();
        assert_eq!(path.len(), 2);
        assert_eq!(path.iter().next().unwrap(), "threads");
        let thread_id = path.iter().nth(1).unwrap().clone();
        let db = store.db.lock().unwrap();
        let title: String = db
            .query_row(
                "SELECT title FROM threads WHERE id = ?1",
                [&thread_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title, "My new thread");
    }

    #[test]
    fn create_thread_with_labels() {
        let (mut store, _dir) = test_store();
        let mut map = std::collections::BTreeMap::new();
        map.insert("title".to_string(), Value::String("Labeled".to_string()));
        map.insert(
            "labels".to_string(),
            Value::Array(vec![
                Value::String("backend".to_string()),
                Value::String("urgent".to_string()),
            ]),
        );
        let path = store
            .write(
                &structfs_core_store::path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .unwrap();
        let thread_id = path.iter().nth(1).unwrap().clone();
        let db = store.db.lock().unwrap();
        let labels: Vec<String> = db
            .prepare("SELECT label FROM labels WHERE thread_id = ?1 ORDER BY label")
            .unwrap()
            .query_map([&thread_id], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(labels, vec!["backend", "urgent"]);
    }

    #[test]
    fn create_thread_with_parent() {
        let (mut store, _dir) = test_store();
        let mut map = std::collections::BTreeMap::new();
        map.insert("title".to_string(), Value::String("Parent".to_string()));
        let path = store
            .write(
                &structfs_core_store::path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .unwrap();
        let parent_id = path.iter().nth(1).unwrap().clone();

        let mut map = std::collections::BTreeMap::new();
        map.insert("title".to_string(), Value::String("Child".to_string()));
        map.insert("parent_id".to_string(), Value::String(parent_id.clone()));
        let child_path = store
            .write(
                &structfs_core_store::path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .unwrap();
        let child_id = child_path.iter().nth(1).unwrap().clone();

        let db = store.db.lock().unwrap();
        let found_parent: String = db
            .query_row(
                "SELECT parent_id FROM threads WHERE id = ?1",
                [&child_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(found_parent, parent_id);
    }

    #[test]
    fn update_thread_metadata() {
        let (mut store, _dir) = test_store();
        let mut map = std::collections::BTreeMap::new();
        map.insert("title".to_string(), Value::String("Original".to_string()));
        let path = store
            .write(
                &structfs_core_store::path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .unwrap();
        let id = path.iter().nth(1).unwrap().clone();

        let mut update = std::collections::BTreeMap::new();
        update.insert("title".to_string(), Value::String("Updated".to_string()));
        update.insert("inbox_state".to_string(), Value::String("done".to_string()));
        update.insert(
            "thread_state".to_string(),
            Value::String("completed".to_string()),
        );
        let update_path = Path::parse(&format!("threads/{}", id)).unwrap();
        store
            .write(&update_path, Record::parsed(Value::Map(update)))
            .unwrap();

        let db = store.db.lock().unwrap();
        let (title, inbox_state, thread_state): (String, String, String) = db
            .query_row(
                "SELECT title, inbox_state, thread_state FROM threads WHERE id = ?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "Updated");
        assert_eq!(inbox_state, "done");
        assert_eq!(thread_state, "completed");
    }

    #[test]
    fn update_thread_rejects_invalid_state() {
        let (mut store, _dir) = test_store();
        let mut map = std::collections::BTreeMap::new();
        map.insert("title".to_string(), Value::String("Thread".to_string()));
        let path = store
            .write(
                &structfs_core_store::path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .unwrap();
        let id = path.iter().nth(1).unwrap().clone();

        let mut update = std::collections::BTreeMap::new();
        update.insert(
            "thread_state".to_string(),
            Value::String("bogus".to_string()),
        );
        let update_path = Path::parse(&format!("threads/{}", id)).unwrap();
        let result = store.write(&update_path, Record::parsed(Value::Map(update)));
        assert!(result.is_err());
    }

    #[test]
    fn set_labels_replaces_existing() {
        let (mut store, _dir) = test_store();
        let mut map = std::collections::BTreeMap::new();
        map.insert("title".to_string(), Value::String("Thread".to_string()));
        map.insert(
            "labels".to_string(),
            Value::Array(vec![Value::String("old".to_string())]),
        );
        let path = store
            .write(
                &structfs_core_store::path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .unwrap();
        let id = path.iter().nth(1).unwrap().clone();

        let labels_path = Path::parse(&format!("threads/{}/labels", id)).unwrap();
        store
            .write(
                &labels_path,
                Record::parsed(Value::Array(vec![
                    Value::String("new1".to_string()),
                    Value::String("new2".to_string()),
                ])),
            )
            .unwrap();

        let db = store.db.lock().unwrap();
        let labels: Vec<String> = db
            .prepare("SELECT label FROM labels WHERE thread_id = ?1 ORDER BY label")
            .unwrap()
            .query_map([&id], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(labels, vec!["new1", "new2"]);
    }

    #[test]
    fn list_inbox_threads() {
        let (mut store, _dir) = test_store();
        for title in ["Thread A", "Thread B"] {
            let mut map = std::collections::BTreeMap::new();
            map.insert("title".to_string(), Value::String(title.to_string()));
            store
                .write(
                    &structfs_core_store::path!("threads"),
                    Record::parsed(Value::Map(map)),
                )
                .unwrap();
        }
        let result = store
            .read(&structfs_core_store::path!("threads"))
            .unwrap()
            .unwrap();
        let Value::Array(threads) = result.as_value().unwrap() else {
            panic!("expected array")
        };
        assert_eq!(threads.len(), 2);
    }

    #[test]
    fn get_single_thread() {
        let (mut store, _dir) = test_store();
        let mut map = std::collections::BTreeMap::new();
        map.insert("title".to_string(), Value::String("Solo".to_string()));
        let path = store
            .write(
                &structfs_core_store::path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .unwrap();
        let id = path.iter().nth(1).unwrap().clone();
        let read_path = Path::parse(&format!("threads/{}", id)).unwrap();
        let result = store.read(&read_path).unwrap().unwrap();
        let value = result.as_value().unwrap();
        let Value::Map(map) = value else {
            panic!("expected map")
        };
        assert_eq!(map.get("title"), Some(&Value::String("Solo".to_string())));
        assert_eq!(
            map.get("thread_state"),
            Some(&Value::String("running".to_string()))
        );
    }

    #[test]
    fn done_threads_separate_from_inbox() {
        let (mut store, _dir) = test_store();
        // Create and archive one
        let mut map = std::collections::BTreeMap::new();
        map.insert("title".to_string(), Value::String("Archived".to_string()));
        let path = store
            .write(
                &structfs_core_store::path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .unwrap();
        let id = path.iter().nth(1).unwrap().clone();
        let mut update = std::collections::BTreeMap::new();
        update.insert("inbox_state".to_string(), Value::String("done".to_string()));
        store
            .write(
                &Path::parse(&format!("threads/{}", id)).unwrap(),
                Record::parsed(Value::Map(update)),
            )
            .unwrap();
        // Create one active
        let mut map = std::collections::BTreeMap::new();
        map.insert("title".to_string(), Value::String("Active".to_string()));
        store
            .write(
                &structfs_core_store::path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .unwrap();

        let inbox = store
            .read(&structfs_core_store::path!("threads"))
            .unwrap()
            .unwrap();
        let Value::Array(arr) = inbox.as_value().unwrap() else {
            panic!()
        };
        assert_eq!(arr.len(), 1);
        let done = store
            .read(&structfs_core_store::path!("done"))
            .unwrap()
            .unwrap();
        let Value::Array(arr) = done.as_value().unwrap() else {
            panic!()
        };
        assert_eq!(arr.len(), 1);
    }

    #[test]
    fn read_messages_via_reader() {
        let (mut store, _dir) = test_store();
        let mut map = std::collections::BTreeMap::new();
        map.insert("title".to_string(), Value::String("Chat".to_string()));
        let path = store
            .write(
                &structfs_core_store::path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .unwrap();
        let id = path.iter().nth(1).unwrap().clone();
        let msg_path = Path::parse(&format!("threads/{}/messages", id)).unwrap();
        for content in ["Hello", "World"] {
            let msg = structfs_serde_store::json_to_value(serde_json::json!({
                "role": "user",
                "content": content
            }));
            store.write(&msg_path, Record::parsed(msg)).unwrap();
        }
        let result = store.read(&msg_path).unwrap().unwrap();
        let Value::Array(msgs) = result.as_value().unwrap() else {
            panic!("expected array")
        };
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn filter_by_label() {
        let (mut store, _dir) = test_store();
        for (title, labels) in [
            ("Backend", vec!["backend"]),
            ("Frontend", vec!["frontend"]),
            ("Full", vec!["backend", "frontend"]),
        ] {
            let mut map = std::collections::BTreeMap::new();
            map.insert("title".to_string(), Value::String(title.to_string()));
            map.insert(
                "labels".to_string(),
                Value::Array(
                    labels
                        .into_iter()
                        .map(|l| Value::String(l.to_string()))
                        .collect(),
                ),
            );
            store
                .write(
                    &structfs_core_store::path!("threads"),
                    Record::parsed(Value::Map(map)),
                )
                .unwrap();
        }
        let result = store
            .read(&structfs_core_store::path!("labels/backend"))
            .unwrap()
            .unwrap();
        let Value::Array(threads) = result.as_value().unwrap() else {
            panic!()
        };
        assert_eq!(threads.len(), 2);

        let result = store
            .read(&structfs_core_store::path!("labels"))
            .unwrap()
            .unwrap();
        let Value::Array(labels) = result.as_value().unwrap() else {
            panic!()
        };
        assert_eq!(labels.len(), 2);
    }

    #[test]
    fn filter_by_state() {
        let (mut store, _dir) = test_store();
        let mut map = std::collections::BTreeMap::new();
        map.insert("title".to_string(), Value::String("Running".to_string()));
        store
            .write(
                &structfs_core_store::path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .unwrap();

        let mut map = std::collections::BTreeMap::new();
        map.insert("title".to_string(), Value::String("Done agent".to_string()));
        let path = store
            .write(
                &structfs_core_store::path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .unwrap();
        let id = path.iter().nth(1).unwrap().clone();
        let mut update = std::collections::BTreeMap::new();
        update.insert(
            "thread_state".to_string(),
            Value::String("completed".to_string()),
        );
        store
            .write(
                &Path::parse(&format!("threads/{}", id)).unwrap(),
                Record::parsed(Value::Map(update)),
            )
            .unwrap();

        let result = store
            .read(&structfs_core_store::path!("by_state/running"))
            .unwrap()
            .unwrap();
        let Value::Array(arr) = result.as_value().unwrap() else {
            panic!()
        };
        assert_eq!(arr.len(), 1);
        let result = store
            .read(&structfs_core_store::path!("by_state/completed"))
            .unwrap()
            .unwrap();
        let Value::Array(arr) = result.as_value().unwrap() else {
            panic!()
        };
        assert_eq!(arr.len(), 1);
    }

    #[test]
    fn search_by_title() {
        let (mut store, _dir) = test_store();
        for title in ["Refactor auth", "Add pagination", "Fix auth bug"] {
            let mut map = std::collections::BTreeMap::new();
            map.insert("title".to_string(), Value::String(title.to_string()));
            store
                .write(
                    &structfs_core_store::path!("threads"),
                    Record::parsed(Value::Map(map)),
                )
                .unwrap();
        }
        let result = store
            .read(&structfs_core_store::path!("search/auth"))
            .unwrap()
            .unwrap();
        let Value::Array(threads) = result.as_value().unwrap() else {
            panic!()
        };
        assert_eq!(threads.len(), 2);
    }

    #[test]
    fn list_children_of_thread() {
        let (mut store, _dir) = test_store();
        let mut map = std::collections::BTreeMap::new();
        map.insert("title".to_string(), Value::String("Parent".to_string()));
        let path = store
            .write(
                &structfs_core_store::path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .unwrap();
        let parent_id = path.iter().nth(1).unwrap().clone();
        for title in ["Child A", "Child B"] {
            let mut map = std::collections::BTreeMap::new();
            map.insert("title".to_string(), Value::String(title.to_string()));
            map.insert("parent_id".to_string(), Value::String(parent_id.clone()));
            store
                .write(
                    &structfs_core_store::path!("threads"),
                    Record::parsed(Value::Map(map)),
                )
                .unwrap();
        }
        let children_path = Path::parse(&format!("threads/{}/children", parent_id)).unwrap();
        let result = store.read(&children_path).unwrap().unwrap();
        let Value::Array(children) = result.as_value().unwrap() else {
            panic!()
        };
        assert_eq!(children.len(), 2);
    }

    #[test]
    fn create_and_list_tasks() {
        let (mut store, _dir) = test_store();
        let mut map = std::collections::BTreeMap::new();
        map.insert("title".to_string(), Value::String("Thread".to_string()));
        let path = store
            .write(
                &structfs_core_store::path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .unwrap();
        let thread_id = path.iter().nth(1).unwrap().clone();
        let tasks_path = Path::parse(&format!("threads/{}/tasks", thread_id)).unwrap();
        for title in ["Read file", "Edit code"] {
            let mut map = std::collections::BTreeMap::new();
            map.insert("title".to_string(), Value::String(title.to_string()));
            store
                .write(&tasks_path, Record::parsed(Value::Map(map)))
                .unwrap();
        }
        let result = store.read(&tasks_path).unwrap().unwrap();
        let Value::Array(tasks) = result.as_value().unwrap() else {
            panic!()
        };
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn update_task_status() {
        let (mut store, _dir) = test_store();
        let mut map = std::collections::BTreeMap::new();
        map.insert("title".to_string(), Value::String("Thread".to_string()));
        let path = store
            .write(
                &structfs_core_store::path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .unwrap();
        let thread_id = path.iter().nth(1).unwrap().clone();
        let tasks_path = Path::parse(&format!("threads/{}/tasks", thread_id)).unwrap();
        let mut task_map = std::collections::BTreeMap::new();
        task_map.insert("title".to_string(), Value::String("My task".to_string()));
        let task_path = store
            .write(&tasks_path, Record::parsed(Value::Map(task_map)))
            .unwrap();
        let task_id = task_path.iter().nth(3).unwrap().clone();

        let update_path = Path::parse(&format!("threads/{}/tasks/{}", thread_id, task_id)).unwrap();
        let mut update = std::collections::BTreeMap::new();
        update.insert("status".to_string(), Value::String("completed".to_string()));
        store
            .write(&update_path, Record::parsed(Value::Map(update)))
            .unwrap();

        let result = store.read(&tasks_path).unwrap().unwrap();
        let Value::Array(tasks) = result.as_value().unwrap() else {
            panic!()
        };
        assert_eq!(tasks.len(), 1);
        let Value::Map(task_map) = &tasks[0] else {
            panic!()
        };
        assert_eq!(
            task_map.get("status"),
            Some(&Value::String("completed".to_string()))
        );
    }

    #[test]
    fn full_lifecycle_integration() {
        let (mut store, _dir) = test_store();

        // 1. Create thread with label
        let mut map = std::collections::BTreeMap::new();
        map.insert(
            "title".to_string(),
            Value::String("Refactor auth middleware".to_string()),
        );
        map.insert(
            "labels".to_string(),
            Value::Array(vec![Value::String("backend".to_string())]),
        );
        let path = store
            .write(
                &structfs_core_store::path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .unwrap();
        let id = path.iter().nth(1).unwrap().clone();

        // 2. Append messages
        let msg_path = Path::parse(&format!("threads/{}/messages", id)).unwrap();
        store
            .write(
                &msg_path,
                Record::parsed(structfs_serde_store::json_to_value(
                    serde_json::json!({"role": "user", "content": "Refactor the auth middleware"}),
                )),
            )
            .unwrap();
        store
            .write(
                &msg_path,
                Record::parsed(structfs_serde_store::json_to_value(
                    serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "Reading code..."}]}),
                )),
            )
            .unwrap();

        // 3. Create task
        let tasks_path = Path::parse(&format!("threads/{}/tasks", id)).unwrap();
        let mut task = std::collections::BTreeMap::new();
        task.insert(
            "title".to_string(),
            Value::String("Read auth.rs".to_string()),
        );
        store
            .write(&tasks_path, Record::parsed(Value::Map(task)))
            .unwrap();

        // 4. Update state to blocked
        let update_path = Path::parse(&format!("threads/{}", id)).unwrap();
        let mut update = std::collections::BTreeMap::new();
        update.insert(
            "thread_state".to_string(),
            Value::String("blocked_on_approval".to_string()),
        );
        update.insert(
            "block_reason".to_string(),
            Value::String("shell \"cargo test\"".to_string()),
        );
        store
            .write(&update_path, Record::parsed(Value::Map(update)))
            .unwrap();

        // 5. Verify by_state filter
        let blocked = store
            .read(&structfs_core_store::path!("by_state/blocked_on_approval"))
            .unwrap()
            .unwrap();
        let Value::Array(arr) = blocked.as_value().unwrap() else {
            panic!()
        };
        assert_eq!(arr.len(), 1);

        // 6. Verify messages
        let messages = store.read(&msg_path).unwrap().unwrap();
        let Value::Array(msgs) = messages.as_value().unwrap() else {
            panic!()
        };
        assert_eq!(msgs.len(), 2);

        // 7. Mark done
        let mut done_update = std::collections::BTreeMap::new();
        done_update.insert("inbox_state".to_string(), Value::String("done".to_string()));
        store
            .write(&update_path, Record::parsed(Value::Map(done_update)))
            .unwrap();

        // 8. Verify inbox/done separation
        let inbox = store
            .read(&structfs_core_store::path!("threads"))
            .unwrap()
            .unwrap();
        let Value::Array(arr) = inbox.as_value().unwrap() else {
            panic!()
        };
        assert_eq!(arr.len(), 0);
        let done = store
            .read(&structfs_core_store::path!("done"))
            .unwrap()
            .unwrap();
        let Value::Array(arr) = done.as_value().unwrap() else {
            panic!()
        };
        assert_eq!(arr.len(), 1);
    }

    #[test]
    fn append_and_read_messages() {
        let (mut store, dir) = test_store();
        let mut map = std::collections::BTreeMap::new();
        map.insert(
            "title".to_string(),
            Value::String("Chat thread".to_string()),
        );
        let path = store
            .write(
                &structfs_core_store::path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .unwrap();
        let id = path.iter().nth(1).unwrap().clone();

        let msg = structfs_serde_store::json_to_value(serde_json::json!({
            "role": "user",
            "content": "Hello agent"
        }));
        let msg_path = Path::parse(&format!("threads/{}/messages", id)).unwrap();
        store.write(&msg_path, Record::parsed(msg)).unwrap();

        let jsonl_path = dir
            .path()
            .join("threads")
            .join(&id)
            .join(format!("{}.jsonl", id));
        let content = std::fs::read_to_string(&jsonl_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["role"], "user");
        assert_eq!(parsed["content"], "Hello agent");
    }
}

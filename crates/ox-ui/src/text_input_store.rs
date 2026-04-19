//! TextInputStore — broker-side canonical store for text input state.
//!
//! Holds the current input content and cursor position. Accepts structured
//! edit sequences through the StructFS write protocol, enabling batched
//! edits from paste, autocomplete, and keystroke buffering.
//!
//! Read paths:
//! - `""` → full snapshot `{ content, cursor }`
//! - `"content"` → `Value::String`
//! - `"cursor"` → `Value::Integer`
//!
//! Write paths:
//! - `"edit"` → apply an `EditSequence` (with generation check)
//! - `"replace"` → wholesale replace content + cursor, bump generation
//! - `"clear"` → reset to empty

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

// ---------------------------------------------------------------------------
// Edit types
// ---------------------------------------------------------------------------

/// A single edit operation within an edit sequence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edit {
    pub op: EditOp,
    pub at: usize,
    pub source: EditSource,
    pub ts_ms: u64,
}

/// The kind of edit: insert text or delete a range.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum EditOp {
    Insert { text: String },
    Delete { len: usize },
}

/// How the edit originated.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditSource {
    Key,
    Paste,
    Completion,
    Replace,
}

/// A batch of edits with a generation counter for staleness detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditSequence {
    pub edits: Vec<Edit>,
    pub generation: u64,
}

/// Payload for the `replace` write path.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReplacePayload {
    content: String,
    cursor: usize,
}

// ---------------------------------------------------------------------------
// TextInputStore
// ---------------------------------------------------------------------------

/// Broker-side canonical text input state.
pub struct TextInputStore {
    content: String,
    cursor: usize,
    generation: u64,
}

impl TextInputStore {
    pub fn new() -> Self {
        TextInputStore {
            content: String::new(),
            cursor: 0,
            generation: 0,
        }
    }

    /// Apply a single edit, clamping positions to valid bounds.
    fn apply_edit(&mut self, edit: &Edit) {
        let at = edit.at.min(self.content.len());
        match &edit.op {
            EditOp::Insert { text } => {
                self.content.insert_str(at, text);
                self.cursor = at + text.len();
            }
            EditOp::Delete { len } => {
                let end = (at + len).min(self.content.len());
                self.content.drain(at..end);
                self.cursor = at.min(self.content.len());
            }
        }
    }

    pub fn content_and_cursor(&self) -> (String, usize) {
        (self.content.clone(), self.cursor)
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Reset buffer to empty. Does not bump generation.
    pub fn clear(&mut self) {
        self.content.clear();
        self.cursor = 0;
    }

    fn snapshot(&self) -> Value {
        let mut map = BTreeMap::new();
        map.insert("content".to_string(), Value::String(self.content.clone()));
        map.insert("cursor".to_string(), Value::Integer(self.cursor as i64));
        Value::Map(map)
    }
}

impl Default for TextInputStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

impl Reader for TextInputStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        if from.is_empty() {
            return Ok(Some(Record::parsed(self.snapshot())));
        }
        match from.components[0].as_str() {
            "content" => Ok(Some(Record::parsed(Value::String(self.content.clone())))),
            "cursor" => Ok(Some(Record::parsed(Value::Integer(self.cursor as i64)))),
            _ => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

impl Writer for TextInputStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let action = if to.is_empty() {
            return Err(StoreError::store(
                "text_input",
                "write",
                "write to root not supported",
            ));
        } else {
            to.components[0].as_str()
        };

        match action {
            "edit" => {
                let value = data.as_value().ok_or_else(|| {
                    StoreError::store("text_input", "edit", "write data must contain a value")
                })?;
                let seq: EditSequence =
                    structfs_serde_store::from_value(value.clone()).map_err(|e| {
                        StoreError::store("text_input", "edit", format!("bad edit sequence: {e}"))
                    })?;
                // Stale generation — ignore silently
                if seq.generation < self.generation {
                    return Ok(Path::parse("content").unwrap());
                }
                for edit in &seq.edits {
                    self.apply_edit(edit);
                }
                Ok(Path::parse("content").unwrap())
            }
            "replace" => {
                let value = data.as_value().ok_or_else(|| {
                    StoreError::store("text_input", "replace", "write data must contain a value")
                })?;
                let payload: ReplacePayload = structfs_serde_store::from_value(value.clone())
                    .map_err(|e| {
                        StoreError::store(
                            "text_input",
                            "replace",
                            format!("bad replace payload: {e}"),
                        )
                    })?;
                self.content = payload.content;
                self.cursor = payload.cursor.min(self.content.len());
                self.generation += 1;
                Ok(Path::parse("content").unwrap())
            }
            "clear" => {
                self.content.clear();
                self.cursor = 0;
                Ok(Path::parse("content").unwrap())
            }
            _ => Err(StoreError::store(
                "text_input",
                "write",
                format!("unknown path: {action}"),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;

    fn make_edit(op: EditOp, at: usize, source: EditSource) -> Edit {
        Edit {
            op,
            at,
            source,
            ts_ms: 0,
        }
    }

    fn insert(text: &str, at: usize) -> Edit {
        make_edit(
            EditOp::Insert {
                text: text.to_string(),
            },
            at,
            EditSource::Key,
        )
    }

    fn delete(at: usize, len: usize) -> Edit {
        make_edit(EditOp::Delete { len }, at, EditSource::Key)
    }

    fn write_edit_seq(store: &mut TextInputStore, edits: Vec<Edit>, generation: u64) {
        let seq = EditSequence { edits, generation };
        let value = structfs_serde_store::to_value(&seq).unwrap();
        store.write(&path!("edit"), Record::parsed(value)).unwrap();
    }

    fn write_replace(store: &mut TextInputStore, content: &str, cursor: usize) {
        let payload = ReplacePayload {
            content: content.to_string(),
            cursor,
        };
        let value = structfs_serde_store::to_value(&payload).unwrap();
        store
            .write(&path!("replace"), Record::parsed(value))
            .unwrap();
    }

    fn read_content(store: &mut TextInputStore) -> String {
        match store
            .read(&path!("content"))
            .unwrap()
            .unwrap()
            .as_value()
            .unwrap()
        {
            Value::String(s) => s.clone(),
            other => panic!("expected String, got {other:?}"),
        }
    }

    fn read_cursor(store: &mut TextInputStore) -> i64 {
        match store
            .read(&path!("cursor"))
            .unwrap()
            .unwrap()
            .as_value()
            .unwrap()
        {
            Value::Integer(i) => *i,
            other => panic!("expected Integer, got {other:?}"),
        }
    }

    // -- Insert tests --

    #[test]
    fn insert_at_beginning() {
        let mut store = TextInputStore::new();
        write_edit_seq(&mut store, vec![insert("hello", 0)], 0);
        assert_eq!(read_content(&mut store), "hello");
        assert_eq!(read_cursor(&mut store), 5);
    }

    #[test]
    fn insert_at_end() {
        let mut store = TextInputStore::new();
        write_edit_seq(&mut store, vec![insert("he", 0)], 0);
        write_edit_seq(&mut store, vec![insert("llo", 2)], 0);
        assert_eq!(read_content(&mut store), "hello");
        assert_eq!(read_cursor(&mut store), 5);
    }

    #[test]
    fn insert_at_middle() {
        let mut store = TextInputStore::new();
        write_edit_seq(&mut store, vec![insert("hllo", 0)], 0);
        write_edit_seq(&mut store, vec![insert("e", 1)], 0);
        assert_eq!(read_content(&mut store), "hello");
        assert_eq!(read_cursor(&mut store), 2);
    }

    // -- Delete tests --

    #[test]
    fn delete_at_beginning() {
        let mut store = TextInputStore::new();
        write_edit_seq(&mut store, vec![insert("hello", 0)], 0);
        write_edit_seq(&mut store, vec![delete(0, 1)], 0);
        assert_eq!(read_content(&mut store), "ello");
        assert_eq!(read_cursor(&mut store), 0);
    }

    #[test]
    fn delete_at_middle() {
        let mut store = TextInputStore::new();
        write_edit_seq(&mut store, vec![insert("hello", 0)], 0);
        write_edit_seq(&mut store, vec![delete(2, 1)], 0);
        assert_eq!(read_content(&mut store), "helo");
        assert_eq!(read_cursor(&mut store), 2);
    }

    #[test]
    fn delete_at_end() {
        let mut store = TextInputStore::new();
        write_edit_seq(&mut store, vec![insert("hello", 0)], 0);
        write_edit_seq(&mut store, vec![delete(4, 1)], 0);
        assert_eq!(read_content(&mut store), "hell");
        assert_eq!(read_cursor(&mut store), 4);
    }

    // -- Multi-edit sequence --

    #[test]
    fn edit_sequence_multiple_ops() {
        let mut store = TextInputStore::new();
        let edits = vec![
            insert("hello world", 0),
            delete(5, 6), // remove " world"
            insert("!", 5),
        ];
        write_edit_seq(&mut store, edits, 0);
        assert_eq!(read_content(&mut store), "hello!");
        assert_eq!(read_cursor(&mut store), 6);
    }

    // -- Generation / staleness --

    #[test]
    fn stale_generation_is_ignored() {
        let mut store = TextInputStore::new();
        write_edit_seq(&mut store, vec![insert("hello", 0)], 0);
        // Bump generation via replace
        write_replace(&mut store, "world", 5);
        // generation is now 1; sending generation=0 should be ignored
        write_edit_seq(&mut store, vec![insert("STALE", 0)], 0);
        assert_eq!(read_content(&mut store), "world");
    }

    #[test]
    fn current_generation_is_accepted() {
        let mut store = TextInputStore::new();
        write_replace(&mut store, "hello", 5); // generation bumps to 1
        write_edit_seq(&mut store, vec![insert("!", 5)], 1);
        assert_eq!(read_content(&mut store), "hello!");
    }

    // -- Replace --

    #[test]
    fn replace_overwrites_content_and_cursor() {
        let mut store = TextInputStore::new();
        write_edit_seq(&mut store, vec![insert("hello", 0)], 0);
        write_replace(&mut store, "new content", 3);
        assert_eq!(read_content(&mut store), "new content");
        assert_eq!(read_cursor(&mut store), 3);
    }

    #[test]
    fn replace_clamps_cursor() {
        let mut store = TextInputStore::new();
        write_replace(&mut store, "hi", 100);
        assert_eq!(read_cursor(&mut store), 2);
    }

    // -- Clear --

    #[test]
    fn clear_resets_everything() {
        let mut store = TextInputStore::new();
        write_edit_seq(&mut store, vec![insert("hello", 0)], 0);
        store
            .write(&path!("clear"), Record::parsed(Value::Null))
            .unwrap();
        assert_eq!(read_content(&mut store), "");
        assert_eq!(read_cursor(&mut store), 0);
    }

    // -- Boundary / clamping --

    #[test]
    fn insert_at_out_of_bounds_clamps() {
        let mut store = TextInputStore::new();
        write_edit_seq(&mut store, vec![insert("hi", 0)], 0);
        // Insert at position 100 — should clamp to end (2)
        write_edit_seq(&mut store, vec![insert("!", 100)], 0);
        assert_eq!(read_content(&mut store), "hi!");
        assert_eq!(read_cursor(&mut store), 3);
    }

    #[test]
    fn delete_at_out_of_bounds_clamps() {
        let mut store = TextInputStore::new();
        write_edit_seq(&mut store, vec![insert("hi", 0)], 0);
        // Delete starting at 100 — clamps to end, deletes nothing
        write_edit_seq(&mut store, vec![delete(100, 5)], 0);
        assert_eq!(read_content(&mut store), "hi");
    }

    #[test]
    fn delete_len_exceeds_content_clamps() {
        let mut store = TextInputStore::new();
        write_edit_seq(&mut store, vec![insert("hello", 0)], 0);
        // Delete 100 chars starting at 2 — should only delete to end
        write_edit_seq(&mut store, vec![delete(2, 100)], 0);
        assert_eq!(read_content(&mut store), "he");
        assert_eq!(read_cursor(&mut store), 2);
    }

    // -- Empty content edge cases --

    #[test]
    fn delete_on_empty_is_noop() {
        let mut store = TextInputStore::new();
        write_edit_seq(&mut store, vec![delete(0, 1)], 0);
        assert_eq!(read_content(&mut store), "");
        assert_eq!(read_cursor(&mut store), 0);
    }

    #[test]
    fn insert_empty_string() {
        let mut store = TextInputStore::new();
        write_edit_seq(&mut store, vec![insert("", 0)], 0);
        assert_eq!(read_content(&mut store), "");
        assert_eq!(read_cursor(&mut store), 0);
    }

    // -- Read snapshot --

    #[test]
    fn read_root_returns_snapshot() {
        let mut store = TextInputStore::new();
        write_edit_seq(&mut store, vec![insert("hello", 0)], 0);
        let record = store.read(&path!("")).unwrap().unwrap();
        let val = record.as_value().unwrap();
        match val {
            Value::Map(m) => {
                assert_eq!(m.get("content"), Some(&Value::String("hello".to_string())));
                assert_eq!(m.get("cursor"), Some(&Value::Integer(5)));
            }
            _ => panic!("expected Map"),
        }
    }

    #[test]
    fn read_unknown_path_returns_none() {
        let mut store = TextInputStore::new();
        assert!(store.read(&path!("nonexistent")).unwrap().is_none());
    }

    // -- Error cases --

    #[test]
    fn write_to_root_returns_error() {
        let mut store = TextInputStore::new();
        let result = store.write(&path!(""), Record::parsed(Value::Null));
        assert!(result.is_err());
    }

    #[test]
    fn write_unknown_path_returns_error() {
        let mut store = TextInputStore::new();
        let result = store.write(&path!("bogus"), Record::parsed(Value::Null));
        assert!(result.is_err());
    }

    // -- Paste source --

    #[test]
    fn paste_insert_works() {
        let mut store = TextInputStore::new();
        let edits = vec![Edit {
            op: EditOp::Insert {
                text: "pasted text".to_string(),
            },
            at: 0,
            source: EditSource::Paste,
            ts_ms: 1000,
        }];
        write_edit_seq(&mut store, edits, 0);
        assert_eq!(read_content(&mut store), "pasted text");
        assert_eq!(read_cursor(&mut store), 11);
    }
}

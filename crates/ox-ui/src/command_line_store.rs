//! CommandLineStore — reusable vim-style command-line state machine.
//!
//! A buffer paired with an open/closed flag. On submit, the buffer text
//! is moved into [`CommandLineStore::pending_submit`] and the prompt
//! closes. A consumer (event loop) drains the pending text and routes
//! it through the command pipeline — typically a write to `command/exec`.
//!
//! This deferred-effect shape matches [`UiStore`]'s `pending_action`
//! pattern: the store records intent synchronously; the caller performs
//! the async side-effect on the next tick. Keeping dispatch out of the
//! Writer call is load-bearing: when this store is composed inside
//! another (like [`UiStore`]), dispatching from its handler would block
//! the parent's server task, which any write routed back to the same
//! server would then deadlock against. Pending-effect sidesteps that
//! without sleeps or spawn-and-pray.
//!
//! Screen-agnostic and UI-agnostic: mount under any path, embed as a
//! sub-store, or exercise standalone in tests.
//!
//! Read paths:
//! - `""` — snapshot `{open, content, cursor, pending_submit}`
//! - `"open"` — Bool
//! - `"content"` — String
//! - `"cursor"` — Integer
//! - `"pending_submit"` — String or Null
//!
//! Write paths:
//! - `"open"` — set open=true, clear buffer
//! - `"close"` — set open=false, clear buffer (pending_submit untouched)
//! - `"submit"` — move buffer content into pending_submit, then close
//! - `"clear_pending_submit"` — consumer ack, drops pending_submit
//! - `"edit"` / `"replace"` / `"clear"` — delegate to the inner [`TextInputStore`]
//!
//! [`UiStore`]: crate::UiStore
//! [`TextInputStore`]: crate::text_input_store::TextInputStore

use std::collections::BTreeMap;

use ox_path::oxpath;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

use crate::text_input_store::TextInputStore;

// ---------------------------------------------------------------------------
// CommandLineStore
// ---------------------------------------------------------------------------

pub struct CommandLineStore {
    open: bool,
    buffer: TextInputStore,
    /// Committed text waiting for a consumer to route to `command/exec`.
    /// `None` in the steady state; `Some` between submit and the next
    /// tick of the consumer.
    pending_submit: Option<String>,
}

impl CommandLineStore {
    pub fn new() -> Self {
        CommandLineStore {
            open: false,
            buffer: TextInputStore::new(),
            pending_submit: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn content(&self) -> &str {
        self.buffer.content()
    }

    pub fn cursor(&self) -> usize {
        self.buffer.cursor()
    }

    pub fn pending_submit(&self) -> Option<&str> {
        self.pending_submit.as_deref()
    }

    fn snapshot(&self) -> Value {
        let mut map = BTreeMap::new();
        map.insert("open".to_string(), Value::Bool(self.open));
        map.insert(
            "content".to_string(),
            Value::String(self.buffer.content().to_string()),
        );
        map.insert(
            "cursor".to_string(),
            Value::Integer(self.buffer.cursor() as i64),
        );
        map.insert(
            "pending_submit".to_string(),
            self.pending_submit
                .as_ref()
                .map(|s| Value::String(s.clone()))
                .unwrap_or(Value::Null),
        );
        Value::Map(map)
    }

    fn do_open(&mut self) -> Result<Path, StoreError> {
        self.open = true;
        self.buffer.clear();
        Ok(oxpath!("open"))
    }

    fn do_close(&mut self) -> Result<Path, StoreError> {
        self.open = false;
        self.buffer.clear();
        Ok(oxpath!("open"))
    }

    fn do_submit(&mut self) -> Result<Path, StoreError> {
        let content = self.buffer.content().to_string();
        if content.trim().is_empty() {
            // Empty submit: vim behavior — silently close, no pending.
            self.open = false;
            self.buffer.clear();
            return Ok(oxpath!("open"));
        }
        self.pending_submit = Some(content);
        self.open = false;
        self.buffer.clear();
        Ok(oxpath!("pending_submit"))
    }

    fn do_clear_pending_submit(&mut self) -> Result<Path, StoreError> {
        self.pending_submit = None;
        Ok(oxpath!("pending_submit"))
    }
}

impl Default for CommandLineStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

impl Reader for CommandLineStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        if from.is_empty() {
            return Ok(Some(Record::parsed(self.snapshot())));
        }
        match from.components[0].as_str() {
            "open" => Ok(Some(Record::parsed(Value::Bool(self.open)))),
            "content" | "cursor" => self.buffer.read(from),
            "pending_submit" => Ok(Some(Record::parsed(
                self.pending_submit
                    .as_ref()
                    .map(|s| Value::String(s.clone()))
                    .unwrap_or(Value::Null),
            ))),
            _ => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

impl Writer for CommandLineStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        if to.is_empty() {
            return Err(StoreError::store(
                "command_line",
                "write",
                "write to root not supported",
            ));
        }
        match to.components[0].as_str() {
            "open" => self.do_open(),
            "close" => self.do_close(),
            "submit" => self.do_submit(),
            "clear_pending_submit" => self.do_clear_pending_submit(),
            "edit" | "replace" | "clear" => self.buffer.write(to, data),
            other => Err(StoreError::store(
                "command_line",
                "write",
                format!("unknown path: {other}"),
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
    use crate::text_input_store::{Edit, EditOp, EditSequence, EditSource};
    use structfs_core_store::path;

    fn write_insert(store: &mut CommandLineStore, text: &str, at: usize, generation: u64) {
        let seq = EditSequence {
            edits: vec![Edit {
                op: EditOp::Insert {
                    text: text.to_string(),
                },
                at,
                source: EditSource::Key,
                ts_ms: 0,
            }],
            generation,
        };
        let v = structfs_serde_store::to_value(&seq).unwrap();
        store.write(&path!("edit"), Record::parsed(v)).unwrap();
    }

    fn read_open(store: &mut CommandLineStore) -> bool {
        match store
            .read(&path!("open"))
            .unwrap()
            .unwrap()
            .as_value()
            .unwrap()
        {
            Value::Bool(b) => *b,
            other => panic!("expected Bool, got {other:?}"),
        }
    }

    fn read_pending(store: &mut CommandLineStore) -> Value {
        store
            .read(&path!("pending_submit"))
            .unwrap()
            .unwrap()
            .as_value()
            .cloned()
            .unwrap()
    }

    // -- Open / close --

    #[test]
    fn new_store_is_closed_with_empty_buffer() {
        let mut s = CommandLineStore::new();
        assert!(!read_open(&mut s));
        assert_eq!(s.content(), "");
        assert_eq!(s.cursor(), 0);
        assert!(s.pending_submit().is_none());
    }

    #[test]
    fn open_flips_flag_and_clears_buffer() {
        let mut s = CommandLineStore::new();
        write_insert(&mut s, "stale", 0, 0);
        assert_eq!(s.content(), "stale");
        s.write(&path!("open"), Record::parsed(Value::Null))
            .unwrap();
        assert!(read_open(&mut s));
        assert_eq!(s.content(), "");
    }

    #[test]
    fn close_flips_flag_and_clears_buffer() {
        let mut s = CommandLineStore::new();
        s.write(&path!("open"), Record::parsed(Value::Null))
            .unwrap();
        write_insert(&mut s, "q", 0, 0);
        s.write(&path!("close"), Record::parsed(Value::Null))
            .unwrap();
        assert!(!read_open(&mut s));
        assert_eq!(s.content(), "");
    }

    #[test]
    fn close_does_not_drop_pending_submit() {
        // A consumer might observe the pending text after submit even
        // though the prompt itself has closed.
        let mut s = CommandLineStore::new();
        s.write(&path!("open"), Record::parsed(Value::Null))
            .unwrap();
        write_insert(&mut s, "quit", 0, 0);
        s.write(&path!("submit"), Record::parsed(Value::Null))
            .unwrap();
        // close after submit preserves pending_submit
        s.write(&path!("close"), Record::parsed(Value::Null))
            .unwrap();
        assert_eq!(s.pending_submit(), Some("quit"));
    }

    // -- Edit delegates to inner buffer --

    #[test]
    fn edit_writes_flow_through_to_buffer() {
        let mut s = CommandLineStore::new();
        write_insert(&mut s, "hello", 0, 0);
        assert_eq!(s.content(), "hello");
        assert_eq!(s.cursor(), 5);
    }

    // -- Snapshot --

    #[test]
    fn root_read_returns_full_snapshot() {
        let mut s = CommandLineStore::new();
        s.write(&path!("open"), Record::parsed(Value::Null))
            .unwrap();
        write_insert(&mut s, "quit", 0, 0);
        let v = s.read(&path!("")).unwrap().unwrap();
        let map = match v.as_value().unwrap() {
            Value::Map(m) => m.clone(),
            other => panic!("expected Map, got {other:?}"),
        };
        assert_eq!(map.get("open"), Some(&Value::Bool(true)));
        assert_eq!(map.get("content"), Some(&Value::String("quit".into())));
        assert_eq!(map.get("cursor"), Some(&Value::Integer(4)));
        assert_eq!(map.get("pending_submit"), Some(&Value::Null));
    }

    // -- Submit: deferred effect, no dispatcher --

    #[test]
    fn submit_moves_buffer_to_pending_and_closes() {
        let mut s = CommandLineStore::new();
        s.write(&path!("open"), Record::parsed(Value::Null))
            .unwrap();
        write_insert(&mut s, "quit", 0, 0);
        s.write(&path!("submit"), Record::parsed(Value::Null))
            .unwrap();

        assert!(!read_open(&mut s));
        assert_eq!(s.content(), "");
        assert_eq!(read_pending(&mut s), Value::String("quit".into()));
    }

    #[test]
    fn submit_on_empty_buffer_is_silent_noop() {
        let mut s = CommandLineStore::new();
        s.write(&path!("open"), Record::parsed(Value::Null))
            .unwrap();
        // Buffer is empty. Submitting should close without staging anything.
        s.write(&path!("submit"), Record::parsed(Value::Null))
            .unwrap();
        assert!(!read_open(&mut s));
        assert!(s.pending_submit().is_none());
    }

    #[test]
    fn clear_pending_submit_drops_the_staged_text() {
        let mut s = CommandLineStore::new();
        s.write(&path!("open"), Record::parsed(Value::Null))
            .unwrap();
        write_insert(&mut s, "quit", 0, 0);
        s.write(&path!("submit"), Record::parsed(Value::Null))
            .unwrap();
        assert_eq!(s.pending_submit(), Some("quit"));

        s.write(&path!("clear_pending_submit"), Record::parsed(Value::Null))
            .unwrap();
        assert!(s.pending_submit().is_none());
    }

    // -- Unknown write paths --

    #[test]
    fn unknown_write_path_errors() {
        let mut s = CommandLineStore::new();
        let err = s
            .write(&path!("bogus"), Record::parsed(Value::Null))
            .unwrap_err();
        assert!(format!("{err}").contains("unknown path"));
    }

    #[test]
    fn write_to_root_errors() {
        let mut s = CommandLineStore::new();
        let err = s
            .write(&path!(""), Record::parsed(Value::Null))
            .unwrap_err();
        assert!(format!("{err}").contains("root"));
    }
}

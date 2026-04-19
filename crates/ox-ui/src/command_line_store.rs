//! CommandLineStore — reusable vim-style command-line state machine.
//!
//! A buffer paired with an open/closed flag. On submit, writes the raw
//! buffer text to `command/exec` — the [`CommandStore`] owns parsing and
//! resolution, so this store stays ignorant of command grammar and of
//! the command registry. Two surfaces (bindings and command line) feed
//! the same downstream pipeline.
//!
//! Screen-agnostic and UI-agnostic: mount under any path, embed as a
//! sub-store, or exercise standalone in tests. The only wiring needed is
//! a [`Dispatcher`] that routes writes to the broker.
//!
//! Read paths:
//! - `""` — snapshot `{open, content, cursor}`
//! - `"open"` — Bool
//! - `"content"` — String
//! - `"cursor"` — Integer
//!
//! Write paths:
//! - `"open"` — set open=true, clear buffer
//! - `"close"` — set open=false, clear buffer
//! - `"submit"` — send buffer to `command/exec`, then close
//! - `"edit"` / `"replace"` / `"clear"` — delegate to the inner [`TextInputStore`]
//!
//! [`CommandStore`]: crate::CommandStore
//! [`Dispatcher`]: crate::command::Dispatcher
//! [`TextInputStore`]: crate::text_input_store::TextInputStore

use std::collections::BTreeMap;

use ox_path::oxpath;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

use crate::command::Dispatcher;
use crate::text_input_store::TextInputStore;

// ---------------------------------------------------------------------------
// CommandLineStore
// ---------------------------------------------------------------------------

pub struct CommandLineStore {
    open: bool,
    buffer: TextInputStore,
    dispatcher: Option<Dispatcher>,
}

impl CommandLineStore {
    pub fn new() -> Self {
        CommandLineStore {
            open: false,
            buffer: TextInputStore::new(),
            dispatcher: None,
        }
    }

    pub fn set_dispatcher(&mut self, dispatcher: Dispatcher) {
        self.dispatcher = Some(dispatcher);
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
        let dispatcher = self.dispatcher.as_mut().ok_or_else(|| {
            StoreError::store("command_line", "submit", "no dispatcher configured")
        })?;
        let content = self.buffer.content().to_string();
        let target = oxpath!("command", "exec");
        // Dispatch first so parse errors leave the buffer intact for the
        // user to correct. Only clear on successful dispatch.
        dispatcher(&target, Record::parsed(Value::String(content)))?;
        self.open = false;
        self.buffer.clear();
        Ok(oxpath!("open"))
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
    use std::sync::{Arc, Mutex};
    use structfs_core_store::path;

    type DispatchLog = Arc<Mutex<Vec<(String, Value)>>>;

    fn mock_dispatcher() -> (Dispatcher, DispatchLog) {
        let log: DispatchLog = Arc::new(Mutex::new(Vec::new()));
        let log_clone = log.clone();
        let dispatcher: Dispatcher = Box::new(move |p, r| {
            let v = r.as_value().cloned().unwrap_or(Value::Null);
            log_clone.lock().unwrap().push((p.to_string(), v));
            Ok(p.clone())
        });
        (dispatcher, log)
    }

    fn failing_dispatcher() -> Dispatcher {
        Box::new(|_, _| Err(StoreError::store("test", "dispatch", "nope")))
    }

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

    // -- Open / close --

    #[test]
    fn new_store_is_closed_with_empty_buffer() {
        let mut s = CommandLineStore::new();
        assert!(!read_open(&mut s));
        assert_eq!(s.content(), "");
        assert_eq!(s.cursor(), 0);
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
    }

    // -- Submit --

    #[test]
    fn submit_writes_raw_buffer_to_command_exec_and_closes() {
        let mut s = CommandLineStore::new();
        let (dispatcher, log) = mock_dispatcher();
        s.set_dispatcher(dispatcher);

        s.write(&path!("open"), Record::parsed(Value::Null))
            .unwrap();
        write_insert(&mut s, "quit", 0, 0);
        s.write(&path!("submit"), Record::parsed(Value::Null))
            .unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, "command/exec");
        assert_eq!(log[0].1, Value::String("quit".into()));

        assert!(!read_open(&mut s));
        assert_eq!(s.content(), "");
    }

    #[test]
    fn submit_without_dispatcher_errors() {
        let mut s = CommandLineStore::new();
        write_insert(&mut s, "quit", 0, 0);
        let err = s
            .write(&path!("submit"), Record::parsed(Value::Null))
            .unwrap_err();
        assert!(format!("{err}").contains("dispatcher"));
    }

    #[test]
    fn submit_preserves_buffer_on_dispatch_error() {
        // If dispatch fails (e.g., parse error downstream), the user's
        // text should remain so they can edit and retry.
        let mut s = CommandLineStore::new();
        s.set_dispatcher(failing_dispatcher());
        s.write(&path!("open"), Record::parsed(Value::Null))
            .unwrap();
        write_insert(&mut s, "oops typo", 0, 0);

        let err = s.write(&path!("submit"), Record::parsed(Value::Null));
        assert!(err.is_err());
        // Buffer + open flag preserved for user to correct
        assert!(read_open(&mut s));
        assert_eq!(s.content(), "oops typo");
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

//! UiStore — in-memory state machine for TUI state.
//!
//! Reads return current field values. Writes are commands that transition
//! state atomically with txn deduplication.

use std::collections::BTreeMap;

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer, path};

use crate::command::{Command, TxnLog};

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Which screen is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Inbox,
    Thread,
}

/// Editing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
}

/// Context for insert mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertContext {
    Compose,
    Reply,
    Search,
}

// ---------------------------------------------------------------------------
// UiStore
// ---------------------------------------------------------------------------

/// Holds all TUI state. Implements StructFS Reader and Writer.
pub struct UiStore {
    screen: Screen,
    active_thread: Option<String>,
    mode: Mode,
    insert_context: Option<InsertContext>,
    selected_row: usize,
    row_count: usize,
    scroll: usize,
    scroll_max: usize,
    viewport_height: usize,
    input: String,
    cursor: usize,
    modal: Option<Value>,
    status: Option<String>,
    /// Pending application-level action for the TUI to handle.
    /// Set by commands like send_input/quit/open_selected that
    /// require App-level logic the store can't perform.
    pending_action: Option<String>,
    search_chips: Vec<String>,
    search_live_query: String,
    txn_log: TxnLog,
}

impl UiStore {
    /// Create a new UiStore with default state.
    pub fn new() -> Self {
        UiStore {
            screen: Screen::Inbox,
            active_thread: None,
            mode: Mode::Normal,
            insert_context: None,
            selected_row: 0,
            row_count: 0,
            scroll: 0,
            scroll_max: 0,
            viewport_height: 0,
            input: String::new(),
            cursor: 0,
            modal: None,
            status: None,
            pending_action: None,
            search_chips: Vec::new(),
            search_live_query: String::new(),
            txn_log: TxnLog::new(),
        }
    }

    // -- Helpers for reading state as Value --

    fn screen_value(&self) -> Value {
        Value::String(
            match self.screen {
                Screen::Inbox => "inbox",
                Screen::Thread => "thread",
            }
            .to_string(),
        )
    }

    fn mode_value(&self) -> Value {
        Value::String(
            match self.mode {
                Mode::Normal => "normal",
                Mode::Insert => "insert",
            }
            .to_string(),
        )
    }

    fn insert_context_value(&self) -> Value {
        match self.insert_context {
            Some(InsertContext::Compose) => Value::String("compose".to_string()),
            Some(InsertContext::Reply) => Value::String("reply".to_string()),
            Some(InsertContext::Search) => Value::String("search".to_string()),
            None => Value::Null,
        }
    }

    fn active_thread_value(&self) -> Value {
        match &self.active_thread {
            Some(s) => Value::String(s.clone()),
            None => Value::Null,
        }
    }

    fn status_value(&self) -> Value {
        match &self.status {
            Some(s) => Value::String(s.clone()),
            None => Value::Null,
        }
    }

    fn pending_action_value(&self) -> Value {
        match &self.pending_action {
            Some(s) => Value::String(s.clone()),
            None => Value::Null,
        }
    }

    fn modal_value(&self) -> Value {
        match &self.modal {
            Some(v) => v.clone(),
            None => Value::Null,
        }
    }

    fn search_chips_value(&self) -> Value {
        Value::Array(self.search_chips.iter().map(|s| Value::String(s.clone())).collect())
    }

    fn search_active(&self) -> bool {
        !self.search_chips.is_empty() || !self.search_live_query.is_empty()
    }

    fn all_fields_map(&self) -> Value {
        let mut map = BTreeMap::new();
        map.insert("screen".to_string(), self.screen_value());
        map.insert("active_thread".to_string(), self.active_thread_value());
        map.insert("mode".to_string(), self.mode_value());
        map.insert("insert_context".to_string(), self.insert_context_value());
        map.insert(
            "selected_row".to_string(),
            Value::Integer(self.selected_row as i64),
        );
        map.insert(
            "row_count".to_string(),
            Value::Integer(self.row_count as i64),
        );
        map.insert("scroll".to_string(), Value::Integer(self.scroll as i64));
        map.insert(
            "viewport_height".to_string(),
            Value::Integer(self.viewport_height as i64),
        );
        map.insert(
            "scroll_max".to_string(),
            Value::Integer(self.scroll_max as i64),
        );
        map.insert("input".to_string(), Value::String(self.input.clone()));
        map.insert("cursor".to_string(), Value::Integer(self.cursor as i64));
        map.insert("modal".to_string(), self.modal_value());
        map.insert("status".to_string(), self.status_value());
        map.insert("pending_action".to_string(), self.pending_action_value());
        map.insert("search_chips".to_string(), self.search_chips_value());
        map.insert("search_live_query".to_string(), Value::String(self.search_live_query.clone()));
        map.insert("search_active".to_string(), Value::Bool(self.search_active()));
        Value::Map(map)
    }

    // -- Helpers for parsing insert context --

    fn parse_insert_context(s: &str) -> Result<InsertContext, StoreError> {
        match s {
            "compose" => Ok(InsertContext::Compose),
            "reply" => Ok(InsertContext::Reply),
            "search" => Ok(InsertContext::Search),
            _ => Err(StoreError::store(
                "ui",
                "enter_insert",
                "unknown insert context",
            )),
        }
    }
}

impl Default for UiStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

impl Reader for UiStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = if from.is_empty() {
            ""
        } else {
            from.components[0].as_str()
        };
        let value = match key {
            "" => self.all_fields_map(),
            "screen" => self.screen_value(),
            "active_thread" => self.active_thread_value(),
            "mode" => self.mode_value(),
            "insert_context" => self.insert_context_value(),
            "selected_row" => Value::Integer(self.selected_row as i64),
            "row_count" => Value::Integer(self.row_count as i64),
            "scroll" => Value::Integer(self.scroll as i64),
            "scroll_max" => Value::Integer(self.scroll_max as i64),
            "viewport_height" => Value::Integer(self.viewport_height as i64),
            "input" => Value::String(self.input.clone()),
            "cursor" => Value::Integer(self.cursor as i64),
            "modal" => self.modal_value(),
            "status" => self.status_value(),
            "pending_action" => self.pending_action_value(),
            "search_chips" => self.search_chips_value(),
            "search_live_query" => Value::String(self.search_live_query.clone()),
            "search_active" => Value::Bool(self.search_active()),
            _ => return Ok(None),
        };
        Ok(Some(Record::parsed(value)))
    }
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

impl Writer for UiStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let command = if to.is_empty() {
            ""
        } else {
            to.components[0].as_str()
        };
        let value = data
            .as_value()
            .ok_or_else(|| StoreError::store("ui", "write", "write data must contain a value"))?;

        let cmd = Command::parse(value)?;

        // Txn deduplication
        if let Some(ref txn) = cmd.txn {
            if self.txn_log.is_duplicate(txn) {
                // Already processed — return silently
                return Ok(path!(""));
            }
        }

        match command {
            "select_next" => {
                if self.selected_row + 1 < self.row_count {
                    self.selected_row += 1;
                }
                Ok(path!("selected_row"))
            }
            "select_prev" => {
                if self.selected_row > 0 {
                    self.selected_row -= 1;
                }
                Ok(path!("selected_row"))
            }
            "open" => {
                let thread_id = cmd.get_str("thread_id").ok_or_else(|| {
                    StoreError::store("ui", "open", "missing required field: thread_id")
                })?;
                self.active_thread = Some(thread_id.to_string());
                self.screen = Screen::Thread;
                self.scroll = 0;
                Ok(path!("screen"))
            }
            "close" => {
                self.active_thread = None;
                self.screen = Screen::Inbox;
                self.mode = Mode::Normal;
                self.insert_context = None;
                self.scroll = 0;
                self.scroll_max = 0;
                Ok(path!("screen"))
            }
            "enter_insert" => {
                let context_str = cmd.get_str("context").ok_or_else(|| {
                    StoreError::store("ui", "enter_insert", "missing required field: context")
                })?;
                let ctx = Self::parse_insert_context(context_str)?;
                self.mode = Mode::Insert;
                self.insert_context = Some(ctx);
                self.input.clear();
                self.cursor = 0;
                Ok(path!("mode"))
            }
            "exit_insert" => {
                self.mode = Mode::Normal;
                self.insert_context = None;
                Ok(path!("mode"))
            }
            "set_input" => {
                if let Some(text) = cmd.get_str("text") {
                    self.input = text.to_string();
                }
                if let Some(pos) = cmd.get_int("cursor") {
                    let pos = pos.max(0) as usize;
                    self.cursor = pos.min(self.input.len());
                } else {
                    // Clamp existing cursor if input changed
                    self.cursor = self.cursor.min(self.input.len());
                }
                Ok(path!("input"))
            }
            "clear_input" => {
                self.input.clear();
                self.cursor = 0;
                Ok(path!("input"))
            }
            // Scroll commands: names match VISUAL direction.
            // scroll_up = viewport moves up = see older = scroll value increases
            // scroll_down = viewport moves down = see newer = scroll value decreases
            "scroll_up" => {
                if self.scroll < self.scroll_max {
                    self.scroll += 1;
                }
                Ok(path!("scroll"))
            }
            "scroll_down" => {
                self.scroll = self.scroll.saturating_sub(1);
                Ok(path!("scroll"))
            }
            "scroll_to_top" => {
                self.scroll = self.scroll_max;
                Ok(path!("scroll"))
            }
            "scroll_to_bottom" => {
                self.scroll = 0;
                Ok(path!("scroll"))
            }
            "scroll_page_up" => {
                self.scroll = (self.scroll + self.viewport_height).min(self.scroll_max);
                Ok(path!("scroll"))
            }
            "scroll_page_down" => {
                self.scroll = self.scroll.saturating_sub(self.viewport_height);
                Ok(path!("scroll"))
            }
            "scroll_half_page_up" => {
                let half = self.viewport_height / 2;
                self.scroll = (self.scroll + half).min(self.scroll_max);
                Ok(path!("scroll"))
            }
            "scroll_half_page_down" => {
                let half = self.viewport_height / 2;
                self.scroll = self.scroll.saturating_sub(half);
                Ok(path!("scroll"))
            }
            "set_scroll_max" => {
                let max = cmd
                    .get_int("max")
                    .ok_or_else(|| StoreError::store("ui", "set_scroll_max", "missing max"))?;
                self.scroll_max = max.max(0) as usize;
                if self.scroll > self.scroll_max {
                    self.scroll = self.scroll_max;
                }
                Ok(path!("scroll_max"))
            }
            "set_viewport_height" => {
                let h = cmd.get_int("height").ok_or_else(|| {
                    StoreError::store("ui", "set_viewport_height", "missing height")
                })?;
                self.viewport_height = h.max(0) as usize;
                Ok(path!("viewport_height"))
            }
            "select_first" => {
                self.selected_row = 0;
                Ok(path!("selected_row"))
            }
            "select_last" => {
                if self.row_count > 0 {
                    self.selected_row = self.row_count - 1;
                }
                Ok(path!("selected_row"))
            }
            "set_row_count" => {
                let count = cmd.get_int("count").ok_or_else(|| {
                    StoreError::store("ui", "set_row_count", "missing required field: count")
                })?;
                self.row_count = count.max(0) as usize;
                if self.row_count > 0 && self.selected_row >= self.row_count {
                    self.selected_row = self.row_count - 1;
                } else if self.row_count == 0 {
                    self.selected_row = 0;
                }
                Ok(path!("row_count"))
            }
            "show_modal" => {
                self.modal = cmd.fields.get("modal").cloned();
                Ok(path!("modal"))
            }
            "dismiss_modal" => {
                self.modal = None;
                Ok(path!("modal"))
            }
            "set_status" => {
                self.status = cmd.get_str("text").map(|s| s.to_string());
                Ok(path!("status"))
            }
            // -- Text editing commands --
            "insert_char" => {
                let ch = cmd
                    .get_str("char")
                    .ok_or_else(|| StoreError::store("ui", "insert_char", "missing char"))?;
                let at = cmd
                    .get_int("at")
                    .map(|n| (n.max(0) as usize).min(self.input.len()))
                    .unwrap_or(self.cursor);
                self.input.insert_str(at, ch);
                self.cursor = at + ch.len();
                Ok(path!("input"))
            }
            "delete_char" => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.input.remove(self.cursor);
                }
                Ok(path!("input"))
            }
            "search_insert_char" => {
                let ch = cmd
                    .get_str("char")
                    .ok_or_else(|| StoreError::store("ui", "search_insert_char", "missing char"))?;
                self.search_live_query.push_str(ch);
                Ok(path!("search_live_query"))
            }
            "search_delete_char" => {
                self.search_live_query.pop();
                Ok(path!("search_live_query"))
            }
            "search_clear" => {
                self.search_live_query.clear();
                Ok(path!("search_live_query"))
            }
            "search_save_chip" => {
                let trimmed = self.search_live_query.trim().to_string();
                if !trimmed.is_empty() {
                    self.search_chips.push(trimmed);
                }
                self.search_live_query.clear();
                Ok(path!("search_chips"))
            }
            "search_dismiss_chip" => {
                let idx = cmd
                    .get_int("index")
                    .ok_or_else(|| StoreError::store("ui", "search_dismiss_chip", "missing index"))?;
                let idx = idx as usize;
                if idx < self.search_chips.len() {
                    self.search_chips.remove(idx);
                }
                Ok(path!("search_chips"))
            }
            // -- Application-level actions (set pending for TUI to handle) --
            "send_input" | "quit" | "open_selected" | "archive_selected" => {
                self.pending_action = Some(command.to_string());
                Ok(path!("pending_action"))
            }
            "clear_pending_action" => {
                self.pending_action = None;
                Ok(path!("pending_action"))
            }
            _ => Err(StoreError::store("ui", "write", "unknown command")),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn cmd_map(pairs: &[(&str, Value)]) -> Record {
        let mut map = BTreeMap::new();
        for (k, v) in pairs {
            map.insert(k.to_string(), v.clone());
        }
        Record::parsed(Value::Map(map))
    }

    fn empty_cmd() -> Record {
        Record::parsed(Value::Map(BTreeMap::new()))
    }

    fn read_str(store: &mut UiStore, key: &str) -> Value {
        let p = path!(key);
        store.read(&p).unwrap().unwrap().as_value().unwrap().clone()
    }

    #[test]
    fn initial_state() {
        let mut store = UiStore::new();
        assert_eq!(
            read_str(&mut store, "screen"),
            Value::String("inbox".into())
        );
        assert_eq!(read_str(&mut store, "mode"), Value::String("normal".into()));
        assert_eq!(read_str(&mut store, "selected_row"), Value::Integer(0));
    }

    #[test]
    fn read_all_returns_map() {
        let mut store = UiStore::new();
        let val = read_str(&mut store, "");
        match val {
            Value::Map(m) => {
                assert!(m.contains_key("screen"));
                assert!(m.contains_key("mode"));
                assert!(m.contains_key("selected_row"));
                assert!(m.contains_key("input"));
                assert!(m.contains_key("modal"));
                assert!(m.contains_key("status"));
            }
            _ => panic!("expected Map"),
        }
    }

    #[test]
    fn read_unknown_returns_none() {
        let mut store = UiStore::new();
        let p = path!("nonexistent");
        assert!(store.read(&p).unwrap().is_none());
    }

    #[test]
    fn select_next_and_prev() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("set_row_count"),
                cmd_map(&[("count", Value::Integer(5))]),
            )
            .unwrap();
        store.write(&path!("select_next"), empty_cmd()).unwrap();
        assert_eq!(read_str(&mut store, "selected_row"), Value::Integer(1));
        store.write(&path!("select_next"), empty_cmd()).unwrap();
        assert_eq!(read_str(&mut store, "selected_row"), Value::Integer(2));
        store.write(&path!("select_prev"), empty_cmd()).unwrap();
        assert_eq!(read_str(&mut store, "selected_row"), Value::Integer(1));
    }

    #[test]
    fn select_clamps_to_bounds() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("set_row_count"),
                cmd_map(&[("count", Value::Integer(3))]),
            )
            .unwrap();
        // Can't go below 0
        store.write(&path!("select_prev"), empty_cmd()).unwrap();
        assert_eq!(read_str(&mut store, "selected_row"), Value::Integer(0));
        // Go to max
        store.write(&path!("select_next"), empty_cmd()).unwrap();
        store.write(&path!("select_next"), empty_cmd()).unwrap();
        assert_eq!(read_str(&mut store, "selected_row"), Value::Integer(2));
        // Can't go past row_count-1
        store.write(&path!("select_next"), empty_cmd()).unwrap();
        assert_eq!(read_str(&mut store, "selected_row"), Value::Integer(2));
    }

    #[test]
    fn set_row_count_clamps_selection() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("set_row_count"),
                cmd_map(&[("count", Value::Integer(10))]),
            )
            .unwrap();
        // Move to row 8
        for _ in 0..8 {
            store.write(&path!("select_next"), empty_cmd()).unwrap();
        }
        assert_eq!(read_str(&mut store, "selected_row"), Value::Integer(8));
        // Shrink to 5 rows — selection should clamp to 4
        store
            .write(
                &path!("set_row_count"),
                cmd_map(&[("count", Value::Integer(5))]),
            )
            .unwrap();
        assert_eq!(read_str(&mut store, "selected_row"), Value::Integer(4));
    }

    #[test]
    fn open_and_close_thread() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("open"),
                cmd_map(&[("thread_id", Value::String("t_001".into()))]),
            )
            .unwrap();
        assert_eq!(
            read_str(&mut store, "screen"),
            Value::String("thread".into())
        );
        assert_eq!(
            read_str(&mut store, "active_thread"),
            Value::String("t_001".into())
        );

        store.write(&path!("close"), empty_cmd()).unwrap();
        assert_eq!(
            read_str(&mut store, "screen"),
            Value::String("inbox".into())
        );
        assert_eq!(read_str(&mut store, "active_thread"), Value::Null);
    }

    #[test]
    fn enter_and_exit_insert() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("enter_insert"),
                cmd_map(&[("context", Value::String("compose".into()))]),
            )
            .unwrap();
        assert_eq!(read_str(&mut store, "mode"), Value::String("insert".into()));
        assert_eq!(
            read_str(&mut store, "insert_context"),
            Value::String("compose".into())
        );

        store.write(&path!("exit_insert"), empty_cmd()).unwrap();
        assert_eq!(read_str(&mut store, "mode"), Value::String("normal".into()));
        assert_eq!(read_str(&mut store, "insert_context"), Value::Null);
    }

    #[test]
    fn enter_insert_clears_input() {
        let mut store = UiStore::new();
        // Set some input first
        store
            .write(
                &path!("set_input"),
                cmd_map(&[
                    ("text", Value::String("leftover".into())),
                    ("cursor", Value::Integer(5)),
                ]),
            )
            .unwrap();
        assert_eq!(
            read_str(&mut store, "input"),
            Value::String("leftover".into())
        );

        // Enter insert — should clear
        store
            .write(
                &path!("enter_insert"),
                cmd_map(&[("context", Value::String("reply".into()))]),
            )
            .unwrap();
        assert_eq!(read_str(&mut store, "input"), Value::String("".into()));
        assert_eq!(read_str(&mut store, "cursor"), Value::Integer(0));
    }

    #[test]
    fn set_and_clear_input() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("set_input"),
                cmd_map(&[
                    ("text", Value::String("hello".into())),
                    ("cursor", Value::Integer(3)),
                ]),
            )
            .unwrap();
        assert_eq!(read_str(&mut store, "input"), Value::String("hello".into()));
        assert_eq!(read_str(&mut store, "cursor"), Value::Integer(3));

        store.write(&path!("clear_input"), empty_cmd()).unwrap();
        assert_eq!(read_str(&mut store, "input"), Value::String("".into()));
        assert_eq!(read_str(&mut store, "cursor"), Value::Integer(0));
    }

    #[test]
    fn set_input_clamps_cursor() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("set_input"),
                cmd_map(&[
                    ("text", Value::String("hi".into())),
                    ("cursor", Value::Integer(100)),
                ]),
            )
            .unwrap();
        // Cursor should be clamped to input length (2)
        assert_eq!(read_str(&mut store, "cursor"), Value::Integer(2));
    }

    #[test]
    fn duplicate_txn_is_idempotent() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("set_row_count"),
                cmd_map(&[("count", Value::Integer(5))]),
            )
            .unwrap();

        let txn_cmd = cmd_map(&[("txn", Value::String("txn_1".into()))]);
        store.write(&path!("select_next"), txn_cmd.clone()).unwrap();
        assert_eq!(read_str(&mut store, "selected_row"), Value::Integer(1));

        // Same txn again — should not advance
        let txn_cmd2 = cmd_map(&[("txn", Value::String("txn_1".into()))]);
        store.write(&path!("select_next"), txn_cmd2).unwrap();
        assert_eq!(read_str(&mut store, "selected_row"), Value::Integer(1));
    }

    #[test]
    fn unknown_command_returns_error() {
        let mut store = UiStore::new();
        let result = store.write(&path!("bogus"), empty_cmd());
        assert!(result.is_err());
    }

    #[test]
    fn open_without_thread_id_returns_error() {
        let mut store = UiStore::new();
        let result = store.write(&path!("open"), empty_cmd());
        assert!(result.is_err());
    }

    // --- Text editing commands ---

    #[test]
    fn insert_char_at_cursor() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("insert_char"),
                cmd_map(&[
                    ("char", Value::String("h".into())),
                    ("at", Value::Integer(0)),
                ]),
            )
            .unwrap();
        assert_eq!(read_str(&mut store, "input"), Value::String("h".into()));
        assert_eq!(read_str(&mut store, "cursor"), Value::Integer(1));

        store
            .write(
                &path!("insert_char"),
                cmd_map(&[
                    ("char", Value::String("i".into())),
                    ("at", Value::Integer(1)),
                ]),
            )
            .unwrap();
        assert_eq!(read_str(&mut store, "input"), Value::String("hi".into()));
        assert_eq!(read_str(&mut store, "cursor"), Value::Integer(2));
    }

    #[test]
    fn delete_char_at_cursor() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("set_input"),
                cmd_map(&[
                    ("text", Value::String("hello".into())),
                    ("cursor", Value::Integer(3)),
                ]),
            )
            .unwrap();

        store.write(&path!("delete_char"), empty_cmd()).unwrap();
        assert_eq!(read_str(&mut store, "input"), Value::String("helo".into()));
        assert_eq!(read_str(&mut store, "cursor"), Value::Integer(2));
    }

    #[test]
    fn delete_char_at_zero_is_noop() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("set_input"),
                cmd_map(&[
                    ("text", Value::String("hi".into())),
                    ("cursor", Value::Integer(0)),
                ]),
            )
            .unwrap();

        store.write(&path!("delete_char"), empty_cmd()).unwrap();
        assert_eq!(read_str(&mut store, "input"), Value::String("hi".into()));
        assert_eq!(read_str(&mut store, "cursor"), Value::Integer(0));
    }

    // --- Pending action commands ---

    #[test]
    fn send_input_sets_pending_action() {
        let mut store = UiStore::new();
        store.write(&path!("send_input"), empty_cmd()).unwrap();
        assert_eq!(
            read_str(&mut store, "pending_action"),
            Value::String("send_input".into())
        );
    }

    #[test]
    fn quit_sets_pending_action() {
        let mut store = UiStore::new();
        store.write(&path!("quit"), empty_cmd()).unwrap();
        assert_eq!(
            read_str(&mut store, "pending_action"),
            Value::String("quit".into())
        );
    }

    #[test]
    fn clear_pending_action() {
        let mut store = UiStore::new();
        store.write(&path!("send_input"), empty_cmd()).unwrap();
        store
            .write(&path!("clear_pending_action"), empty_cmd())
            .unwrap();
        assert_eq!(read_str(&mut store, "pending_action"), Value::Null);
    }

    // --- Scroll bounds ---

    #[test]
    fn scroll_clamps_to_max() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("set_scroll_max"),
                cmd_map(&[("max", Value::Integer(5))]),
            )
            .unwrap();

        // scroll_up = visual up = increases scroll value
        for _ in 0..10 {
            store.write(&path!("scroll_up"), empty_cmd()).unwrap();
        }
        assert_eq!(read_str(&mut store, "scroll"), Value::Integer(5));
    }

    #[test]
    fn set_scroll_max_clamps_current() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("set_scroll_max"),
                cmd_map(&[("max", Value::Integer(10))]),
            )
            .unwrap();
        for _ in 0..8 {
            store.write(&path!("scroll_up"), empty_cmd()).unwrap();
        }
        assert_eq!(read_str(&mut store, "scroll"), Value::Integer(8));

        // Shrink max — scroll clamps
        store
            .write(
                &path!("set_scroll_max"),
                cmd_map(&[("max", Value::Integer(3))]),
            )
            .unwrap();
        assert_eq!(read_str(&mut store, "scroll"), Value::Integer(3));
    }

    // --- Vim navigation ---

    #[test]
    fn scroll_to_top_and_bottom() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("set_scroll_max"),
                cmd_map(&[("max", Value::Integer(50))]),
            )
            .unwrap();

        store.write(&path!("scroll_to_top"), empty_cmd()).unwrap();
        assert_eq!(read_str(&mut store, "scroll"), Value::Integer(50));

        store
            .write(&path!("scroll_to_bottom"), empty_cmd())
            .unwrap();
        assert_eq!(read_str(&mut store, "scroll"), Value::Integer(0));
    }

    #[test]
    fn page_and_half_page_scroll() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("set_scroll_max"),
                cmd_map(&[("max", Value::Integer(100))]),
            )
            .unwrap();
        store
            .write(
                &path!("set_viewport_height"),
                cmd_map(&[("height", Value::Integer(20))]),
            )
            .unwrap();

        // Full page up (visual up = increases scroll)
        store.write(&path!("scroll_page_up"), empty_cmd()).unwrap();
        assert_eq!(read_str(&mut store, "scroll"), Value::Integer(20));

        // Half page up
        store
            .write(&path!("scroll_half_page_up"), empty_cmd())
            .unwrap();
        assert_eq!(read_str(&mut store, "scroll"), Value::Integer(30));

        // Half page down (visual down = decreases scroll)
        store
            .write(&path!("scroll_half_page_down"), empty_cmd())
            .unwrap();
        assert_eq!(read_str(&mut store, "scroll"), Value::Integer(20));

        // Full page down
        store
            .write(&path!("scroll_page_down"), empty_cmd())
            .unwrap();
        assert_eq!(read_str(&mut store, "scroll"), Value::Integer(0));
    }

    #[test]
    fn select_first_and_last() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("set_row_count"),
                cmd_map(&[("count", Value::Integer(10))]),
            )
            .unwrap();

        store.write(&path!("select_last"), empty_cmd()).unwrap();
        assert_eq!(read_str(&mut store, "selected_row"), Value::Integer(9));

        store.write(&path!("select_first"), empty_cmd()).unwrap();
        assert_eq!(read_str(&mut store, "selected_row"), Value::Integer(0));
    }

    #[test]
    fn page_scroll_clamps_to_max() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("set_scroll_max"),
                cmd_map(&[("max", Value::Integer(10))]),
            )
            .unwrap();
        store
            .write(
                &path!("set_viewport_height"),
                cmd_map(&[("height", Value::Integer(20))]),
            )
            .unwrap();

        // Page up with viewport > max — clamps to max
        store.write(&path!("scroll_page_up"), empty_cmd()).unwrap();
        assert_eq!(read_str(&mut store, "scroll"), Value::Integer(10));
    }

    // --- Search commands ---

    #[test]
    fn search_insert_char() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("search_insert_char"),
                cmd_map(&[("char", Value::String("h".into()))]),
            )
            .unwrap();
        assert_eq!(
            read_str(&mut store, "search_live_query"),
            Value::String("h".into())
        );
        store
            .write(
                &path!("search_insert_char"),
                cmd_map(&[("char", Value::String("i".into()))]),
            )
            .unwrap();
        assert_eq!(
            read_str(&mut store, "search_live_query"),
            Value::String("hi".into())
        );
    }

    #[test]
    fn search_delete_char() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("search_insert_char"),
                cmd_map(&[("char", Value::String("a".into()))]),
            )
            .unwrap();
        store
            .write(
                &path!("search_insert_char"),
                cmd_map(&[("char", Value::String("b".into()))]),
            )
            .unwrap();
        store
            .write(&path!("search_delete_char"), empty_cmd())
            .unwrap();
        assert_eq!(
            read_str(&mut store, "search_live_query"),
            Value::String("a".into())
        );
    }

    #[test]
    fn search_delete_char_empty_is_noop() {
        let mut store = UiStore::new();
        store
            .write(&path!("search_delete_char"), empty_cmd())
            .unwrap();
        assert_eq!(
            read_str(&mut store, "search_live_query"),
            Value::String("".into())
        );
    }

    #[test]
    fn search_clear() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("search_insert_char"),
                cmd_map(&[("char", Value::String("x".into()))]),
            )
            .unwrap();
        store
            .write(&path!("search_clear"), empty_cmd())
            .unwrap();
        assert_eq!(
            read_str(&mut store, "search_live_query"),
            Value::String("".into())
        );
    }

    #[test]
    fn search_save_chip() {
        let mut store = UiStore::new();
        for ch in ['b', 'u', 'g'] {
            store
                .write(
                    &path!("search_insert_char"),
                    cmd_map(&[("char", Value::String(ch.to_string()))]),
                )
                .unwrap();
        }
        store
            .write(&path!("search_save_chip"), empty_cmd())
            .unwrap();
        assert_eq!(
            read_str(&mut store, "search_chips"),
            Value::Array(vec![Value::String("bug".into())])
        );
        assert_eq!(
            read_str(&mut store, "search_live_query"),
            Value::String("".into())
        );
    }

    #[test]
    fn search_save_chip_trims_whitespace() {
        let mut store = UiStore::new();
        for ch in [' ', 'a', ' '] {
            store
                .write(
                    &path!("search_insert_char"),
                    cmd_map(&[("char", Value::String(ch.to_string()))]),
                )
                .unwrap();
        }
        store
            .write(&path!("search_save_chip"), empty_cmd())
            .unwrap();
        assert_eq!(
            read_str(&mut store, "search_chips"),
            Value::Array(vec![Value::String("a".into())])
        );
    }

    #[test]
    fn search_save_chip_empty_is_noop() {
        let mut store = UiStore::new();
        store
            .write(&path!("search_save_chip"), empty_cmd())
            .unwrap();
        assert_eq!(
            read_str(&mut store, "search_chips"),
            Value::Array(vec![])
        );
    }

    #[test]
    fn search_dismiss_chip() {
        let mut store = UiStore::new();
        for word in ["alpha", "beta"] {
            for ch in word.chars() {
                store
                    .write(
                        &path!("search_insert_char"),
                        cmd_map(&[("char", Value::String(ch.to_string()))]),
                    )
                    .unwrap();
            }
            store
                .write(&path!("search_save_chip"), empty_cmd())
                .unwrap();
        }
        store
            .write(
                &path!("search_dismiss_chip"),
                cmd_map(&[("index", Value::Integer(0))]),
            )
            .unwrap();
        assert_eq!(
            read_str(&mut store, "search_chips"),
            Value::Array(vec![Value::String("beta".into())])
        );
    }

    #[test]
    fn search_dismiss_chip_out_of_bounds_is_noop() {
        let mut store = UiStore::new();
        store
            .write(
                &path!("search_dismiss_chip"),
                cmd_map(&[("index", Value::Integer(99))]),
            )
            .unwrap();
        assert_eq!(
            read_str(&mut store, "search_chips"),
            Value::Array(vec![])
        );
    }

    #[test]
    fn search_active_derived() {
        let mut store = UiStore::new();
        assert_eq!(read_str(&mut store, "search_active"), Value::Bool(false));

        store
            .write(
                &path!("search_insert_char"),
                cmd_map(&[("char", Value::String("x".into()))]),
            )
            .unwrap();
        assert_eq!(read_str(&mut store, "search_active"), Value::Bool(true));

        store.write(&path!("search_clear"), empty_cmd()).unwrap();
        assert_eq!(read_str(&mut store, "search_active"), Value::Bool(false));

        store
            .write(
                &path!("search_insert_char"),
                cmd_map(&[("char", Value::String("y".into()))]),
            )
            .unwrap();
        store
            .write(&path!("search_save_chip"), empty_cmd())
            .unwrap();
        assert_eq!(read_str(&mut store, "search_active"), Value::Bool(true));
    }

    #[test]
    fn search_fields_in_all_map() {
        let mut store = UiStore::new();
        let val = read_str(&mut store, "");
        match val {
            Value::Map(m) => {
                assert!(m.contains_key("search_chips"));
                assert!(m.contains_key("search_live_query"));
                assert!(m.contains_key("search_active"));
            }
            _ => panic!("expected Map"),
        }
    }
}

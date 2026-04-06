# Core Stores (Plan C2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the stores that replace the TUI's ad-hoc state management (App struct, AppEvent channels, ThreadView mirrors) with StructFS-native stores accessible through the BrokerStore.

**Architecture:** Each store implements synchronous Reader/Writer traits. Reads return current state. Writes are commands — the path determines the action, the value carries preconditions and a txn ID for deduplication. Stores are independently testable without the broker. InputStore bridges to the broker for cross-store command dispatch.

**Tech Stack:** Rust, structfs-core-store (Path, Record, Value, Error), structfs-serde-store (json_to_value, value_to_json), ox-broker (ClientHandle, BrokerStore), tokio (for InputStore integration tests)

**Spec:** `docs/superpowers/specs/2026-04-06-structfs-tui-design.md`

---

## File Structure

| File | Responsibility |
|------|---------------|
| `crates/ox-ui/Cargo.toml` | New crate: UI stores shared between CLI and web |
| `crates/ox-ui/src/lib.rs` | Module exports |
| `crates/ox-ui/src/command.rs` | Command protocol: parse preconditions + txn from write values, dedup |
| `crates/ox-ui/src/ui_store.rs` | UiStore: in-memory state machine for screen, mode, selection, input |
| `crates/ox-ui/src/input_store.rs` | InputStore: key-to-command translator with binding table |
| `crates/ox-ui/src/approval_store.rs` | ApprovalStore: per-thread approval request/response state |
| `crates/ox-history/src/turn.rs` | Turn state: streaming, thinking, tool, tokens |
| `crates/ox-history/src/lib.rs` | (modify) Wire turn state into HistoryProvider read/write paths |
| `Cargo.toml` (workspace root) | Add ox-ui to workspace members |

---

### Task 1: ox-ui Crate Scaffold and Command Protocol

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/ox-ui/Cargo.toml`
- Create: `crates/ox-ui/src/lib.rs`
- Create: `crates/ox-ui/src/command.rs`

The command protocol is the contract between stores. Every write
carries an optional precondition and txn ID. The store validates
the precondition and deduplicates by txn. This module is shared
by UiStore, InputStore, and any future command-driven store.

- [ ] **Step 1: Create crate directory and Cargo.toml**

```toml
# crates/ox-ui/Cargo.toml
[package]
name = "ox-ui"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
homepage.workspace = true
authors.workspace = true
description = "UI stores for ox — state machines driven by StructFS command protocol"

[dependencies]
structfs-core-store = { workspace = true }
structfs-serde-store = { workspace = true }

[dev-dependencies]
ox-broker = { path = "../ox-broker" }
tokio = { version = "1", features = ["sync", "time", "rt", "macros", "rt-multi-thread"] }
```

- [ ] **Step 2: Add to workspace**

Add `"crates/ox-ui"` to the `members` list in root `Cargo.toml`.

- [ ] **Step 3: Write command.rs with tests**

```rust
//! Command protocol for store writes.
//!
//! Every command write carries a Value::Map with optional fields:
//! - "txn": String — unique transaction ID for deduplication
//! - "from" or other precondition fields — validated by the store
//!
//! The path determines the action. The value carries context.

use std::collections::VecDeque;

use structfs_core_store::{Error as StoreError, Value};

/// Maximum number of txn IDs to remember for deduplication.
const TXN_HISTORY_SIZE: usize = 256;

/// Parsed command fields extracted from a write value.
pub struct Command {
    /// Transaction ID for deduplication (if present).
    pub txn: Option<String>,
    /// All fields from the command value.
    pub fields: std::collections::BTreeMap<String, Value>,
}

impl Command {
    /// Parse a command from a write value.
    ///
    /// Accepts Value::Map with optional "txn" field.
    /// Returns error if value is not a Map.
    pub fn parse(value: &Value) -> Result<Self, StoreError> {
        let map = match value {
            Value::Map(m) => m,
            _ => {
                return Err(StoreError::store(
                    "command",
                    "parse",
                    "command value must be a Map",
                ))
            }
        };
        let txn = map
            .get("txn")
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            });
        Ok(Command {
            txn,
            fields: map.clone(),
        })
    }

    /// Get a string field from the command.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.fields.get(key).and_then(|v| match v {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        })
    }

    /// Get an integer field from the command.
    pub fn get_int(&self, key: &str) -> Option<i64> {
        self.fields.get(key).and_then(|v| match v {
            Value::Integer(i) => Some(*i),
            _ => None,
        })
    }
}

/// Tracks recently seen txn IDs for deduplication.
pub struct TxnLog {
    seen: VecDeque<String>,
}

impl TxnLog {
    pub fn new() -> Self {
        TxnLog {
            seen: VecDeque::new(),
        }
    }

    /// Check if a txn has been seen before. If not, record it.
    /// Returns true if the txn is a duplicate.
    pub fn is_duplicate(&mut self, txn: &str) -> bool {
        if self.seen.iter().any(|s| s == txn) {
            return true;
        }
        self.seen.push_back(txn.to_string());
        if self.seen.len() > TXN_HISTORY_SIZE {
            self.seen.pop_front();
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn parse_command_with_txn() {
        let mut map = BTreeMap::new();
        map.insert("txn".to_string(), Value::String("abc123".to_string()));
        map.insert("from".to_string(), Value::String("t_001".to_string()));
        let cmd = Command::parse(&Value::Map(map)).unwrap();
        assert_eq!(cmd.txn.as_deref(), Some("abc123"));
        assert_eq!(cmd.get_str("from"), Some("t_001"));
    }

    #[test]
    fn parse_command_without_txn() {
        let map = BTreeMap::new();
        let cmd = Command::parse(&Value::Map(map)).unwrap();
        assert_eq!(cmd.txn, None);
    }

    #[test]
    fn parse_rejects_non_map() {
        let result = Command::parse(&Value::String("bad".to_string()));
        assert!(result.is_err());
    }

    #[test]
    fn txn_dedup_detects_duplicates() {
        let mut log = TxnLog::new();
        assert!(!log.is_duplicate("txn_1"));
        assert!(log.is_duplicate("txn_1"));
        assert!(!log.is_duplicate("txn_2"));
    }

    #[test]
    fn txn_dedup_evicts_oldest() {
        let mut log = TxnLog::new();
        for i in 0..TXN_HISTORY_SIZE {
            assert!(!log.is_duplicate(&format!("txn_{}", i)));
        }
        // txn_0 should still be in the log (size = 256, we added 256)
        assert!(log.is_duplicate("txn_0"));
        // Add one more to evict txn_0
        assert!(!log.is_duplicate("txn_overflow"));
        assert!(!log.is_duplicate("txn_0")); // evicted, no longer duplicate
    }
}
```

- [ ] **Step 4: Write lib.rs**

```rust
//! UI stores for ox — state machines driven by StructFS command protocol.
//!
//! Stores are synchronous Reader/Writer implementations. Reads return
//! current state. Writes are commands validated by the command protocol
//! (preconditions + txn deduplication).

pub mod command;
```

- [ ] **Step 5: Verify and test**

Run: `cargo check -p ox-ui && cargo test -p ox-ui`
Expected: clean build, 5 tests pass

- [ ] **Step 6: Commit**

```
git add Cargo.toml crates/ox-ui/
git commit -m 'feat(ox-ui): new crate with command protocol for store writes

Command protocol parses preconditions and txn IDs from write values.
TxnLog tracks recent txn IDs for bounded deduplication (256 entries).
Shared by all command-driven stores (UiStore, InputStore, etc.).'
```

---

### Task 2: UiStore — In-Memory State Machine

**Files:**
- Create: `crates/ox-ui/src/ui_store.rs`
- Modify: `crates/ox-ui/src/lib.rs`

UiStore holds all TUI state. Reads return current values. Writes are
commands that transition state atomically. The store owns its invariants:
clamping selection to valid range, enforcing valid mode transitions.

The store does NOT know about threads, messages, or any domain concepts.
It manages: which screen is active, what mode the user is in, where the
cursor is, and what's in the input buffer. Domain stores (InboxStore,
HistoryProvider) hold domain data.

- [ ] **Step 1: Write ui_store.rs with data model and reads**

```rust
//! UiStore — in-memory state machine for TUI state.
//!
//! Reads return current values. Writes are commands.
//! The store owns its invariants (clamping, valid transitions).

use std::collections::BTreeMap;

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

use crate::command::{Command, TxnLog};

/// The screen the TUI is showing.
#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    Inbox,
    Thread,
}

/// The input mode.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    Insert,
}

/// Context for insert mode.
#[derive(Debug, Clone, PartialEq)]
pub enum InsertContext {
    Compose,
    Reply,
    Search,
}

pub struct UiStore {
    screen: Screen,
    active_thread: Option<String>,
    mode: Mode,
    insert_context: Option<InsertContext>,
    selected_row: usize,
    /// Total number of rows (set externally so selection can be clamped).
    row_count: usize,
    scroll: usize,
    input: String,
    cursor: usize,
    modal: Option<Value>,
    status: Option<String>,
    txn_log: TxnLog,
}

impl UiStore {
    pub fn new() -> Self {
        UiStore {
            screen: Screen::Inbox,
            active_thread: None,
            mode: Mode::Normal,
            insert_context: None,
            selected_row: 0,
            row_count: 0,
            scroll: 0,
            input: String::new(),
            cursor: 0,
            modal: None,
            status: None,
            txn_log: TxnLog::new(),
        }
    }

    fn read_field(&self, field: &str) -> Option<Value> {
        match field {
            "screen" => Some(Value::String(match &self.screen {
                Screen::Inbox => "inbox".to_string(),
                Screen::Thread => "thread".to_string(),
            })),
            "active_thread" => Some(match &self.active_thread {
                Some(id) => Value::String(id.clone()),
                None => Value::Null,
            }),
            "mode" => Some(Value::String(match &self.mode {
                Mode::Normal => "normal".to_string(),
                Mode::Insert => "insert".to_string(),
            })),
            "insert_context" => Some(match &self.insert_context {
                Some(InsertContext::Compose) => Value::String("compose".to_string()),
                Some(InsertContext::Reply) => Value::String("reply".to_string()),
                Some(InsertContext::Search) => Value::String("search".to_string()),
                None => Value::Null,
            }),
            "selected_row" => Some(Value::Integer(self.selected_row as i64)),
            "row_count" => Some(Value::Integer(self.row_count as i64)),
            "scroll" => Some(Value::Integer(self.scroll as i64)),
            "input" => Some(Value::String(self.input.clone())),
            "cursor" => Some(Value::Integer(self.cursor as i64)),
            "modal" => Some(self.modal.clone().unwrap_or(Value::Null)),
            "status" => Some(match &self.status {
                Some(s) => Value::String(s.clone()),
                None => Value::Null,
            }),
            _ => None,
        }
    }
}

impl Reader for UiStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let path_str = from.to_string();
        if path_str.is_empty() {
            // Read all state as a map
            let mut map = BTreeMap::new();
            for field in &[
                "screen", "active_thread", "mode", "insert_context",
                "selected_row", "row_count", "scroll", "input", "cursor",
                "modal", "status",
            ] {
                if let Some(val) = self.read_field(field) {
                    map.insert(field.to_string(), val);
                }
            }
            return Ok(Some(Record::parsed(Value::Map(map))));
        }
        Ok(self.read_field(&path_str).map(Record::parsed))
    }
}
```

- [ ] **Step 2: Add command handling (Writer impl)**

Add to `ui_store.rs`:

```rust
impl UiStore {
    // ... existing methods ...

    /// Process a command write. Returns Ok(path) on success.
    fn handle_command(
        &mut self,
        action: &str,
        cmd: &Command,
    ) -> Result<Path, StoreError> {
        match action {
            "select_next" => {
                if self.row_count > 0 && self.selected_row < self.row_count - 1 {
                    self.selected_row += 1;
                }
                Ok(Path::parse("selected_row").unwrap())
            }
            "select_prev" => {
                if self.selected_row > 0 {
                    self.selected_row -= 1;
                }
                Ok(Path::parse("selected_row").unwrap())
            }
            "open" => {
                let thread_id = cmd.get_str("thread_id").ok_or_else(|| {
                    StoreError::store("ui", "open", "missing thread_id field")
                })?;
                self.active_thread = Some(thread_id.to_string());
                self.screen = Screen::Thread;
                self.scroll = 0;
                Ok(Path::parse("screen").unwrap())
            }
            "close" => {
                self.active_thread = None;
                self.screen = Screen::Inbox;
                self.mode = Mode::Normal;
                self.insert_context = None;
                Ok(Path::parse("screen").unwrap())
            }
            "enter_insert" => {
                let context = cmd.get_str("context").ok_or_else(|| {
                    StoreError::store("ui", "enter_insert", "missing context field")
                })?;
                let ctx = match context {
                    "compose" => InsertContext::Compose,
                    "reply" => InsertContext::Reply,
                    "search" => InsertContext::Search,
                    other => {
                        return Err(StoreError::store(
                            "ui", "enter_insert",
                            format!("unknown insert context: {}", other),
                        ))
                    }
                };
                self.mode = Mode::Insert;
                self.insert_context = Some(ctx);
                self.input.clear();
                self.cursor = 0;
                Ok(Path::parse("mode").unwrap())
            }
            "exit_insert" => {
                self.mode = Mode::Normal;
                self.insert_context = None;
                Ok(Path::parse("mode").unwrap())
            }
            "set_input" => {
                if let Some(text) = cmd.get_str("text") {
                    self.input = text.to_string();
                }
                if let Some(pos) = cmd.get_int("cursor") {
                    self.cursor = (pos as usize).min(self.input.len());
                }
                Ok(Path::parse("input").unwrap())
            }
            "clear_input" => {
                self.input.clear();
                self.cursor = 0;
                Ok(Path::parse("input").unwrap())
            }
            "scroll_up" => {
                self.scroll = self.scroll.saturating_sub(1);
                Ok(Path::parse("scroll").unwrap())
            }
            "scroll_down" => {
                self.scroll = self.scroll.saturating_add(1);
                Ok(Path::parse("scroll").unwrap())
            }
            "set_row_count" => {
                if let Some(n) = cmd.get_int("count") {
                    self.row_count = n.max(0) as usize;
                    if self.row_count > 0 {
                        self.selected_row = self.selected_row.min(self.row_count - 1);
                    } else {
                        self.selected_row = 0;
                    }
                }
                Ok(Path::parse("row_count").unwrap())
            }
            "show_modal" => {
                self.modal = self.fields_to_modal_value(cmd);
                Ok(Path::parse("modal").unwrap())
            }
            "dismiss_modal" => {
                self.modal = None;
                Ok(Path::parse("modal").unwrap())
            }
            "set_status" => {
                self.status = cmd.get_str("text").map(|s| s.to_string());
                Ok(Path::parse("status").unwrap())
            }
            _ => Err(StoreError::store(
                "ui", "write",
                format!("unknown command: {}", action),
            )),
        }
    }

    fn fields_to_modal_value(&self, cmd: &Command) -> Option<Value> {
        cmd.fields.get("modal").cloned()
    }
}

impl Writer for UiStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let action = to.to_string();
        let value = data.as_value().ok_or_else(|| {
            StoreError::store("ui", "write", "write data must contain a value")
        })?;
        let cmd = Command::parse(value)?;

        // Txn deduplication
        if let Some(txn) = &cmd.txn {
            if self.txn_log.is_duplicate(txn) {
                // Duplicate txn — silently succeed (idempotent)
                return Ok(to.clone());
            }
        }

        self.handle_command(&action, &cmd)
    }
}
```

- [ ] **Step 3: Write tests**

Add to `ui_store.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;

    fn cmd_map(fields: &[(&str, Value)]) -> Record {
        let mut map = BTreeMap::new();
        for (k, v) in fields {
            map.insert(k.to_string(), v.clone());
        }
        Record::parsed(Value::Map(map))
    }

    fn txn(id: &str) -> (&str, Value) {
        ("txn", Value::String(id.to_string()))
    }

    fn s(val: &str) -> Value {
        Value::String(val.to_string())
    }

    // --- Read tests ---

    #[test]
    fn initial_state() {
        let mut store = UiStore::new();
        let screen = store.read(&path!("screen")).unwrap().unwrap();
        assert_eq!(screen.as_value().unwrap(), &s("inbox"));

        let mode = store.read(&path!("mode")).unwrap().unwrap();
        assert_eq!(mode.as_value().unwrap(), &s("normal"));

        let row = store.read(&path!("selected_row")).unwrap().unwrap();
        assert_eq!(row.as_value().unwrap(), &Value::Integer(0));
    }

    #[test]
    fn read_all_returns_map() {
        let mut store = UiStore::new();
        let all = store.read(&Path::from_components(vec![])).unwrap().unwrap();
        let map = match all.as_value().unwrap() {
            Value::Map(m) => m,
            _ => panic!("expected map"),
        };
        assert!(map.contains_key("screen"));
        assert!(map.contains_key("mode"));
        assert!(map.contains_key("selected_row"));
    }

    #[test]
    fn read_unknown_returns_none() {
        let mut store = UiStore::new();
        assert!(store.read(&path!("nonexistent")).unwrap().is_none());
    }

    // --- Selection commands ---

    #[test]
    fn select_next_and_prev() {
        let mut store = UiStore::new();
        store.write(&path!("set_row_count"), cmd_map(&[
            ("count", Value::Integer(5)),
            txn("t1"),
        ])).unwrap();

        store.write(&path!("select_next"), cmd_map(&[txn("t2")])).unwrap();
        assert_eq!(store.selected_row, 1);

        store.write(&path!("select_next"), cmd_map(&[txn("t3")])).unwrap();
        assert_eq!(store.selected_row, 2);

        store.write(&path!("select_prev"), cmd_map(&[txn("t4")])).unwrap();
        assert_eq!(store.selected_row, 1);
    }

    #[test]
    fn select_clamps_to_bounds() {
        let mut store = UiStore::new();
        store.write(&path!("set_row_count"), cmd_map(&[
            ("count", Value::Integer(3)),
            txn("t1"),
        ])).unwrap();

        // Can't go below 0
        store.write(&path!("select_prev"), cmd_map(&[txn("t2")])).unwrap();
        assert_eq!(store.selected_row, 0);

        // Go to end
        store.write(&path!("select_next"), cmd_map(&[txn("t3")])).unwrap();
        store.write(&path!("select_next"), cmd_map(&[txn("t4")])).unwrap();
        assert_eq!(store.selected_row, 2);

        // Can't go past end
        store.write(&path!("select_next"), cmd_map(&[txn("t5")])).unwrap();
        assert_eq!(store.selected_row, 2);
    }

    #[test]
    fn set_row_count_clamps_selection() {
        let mut store = UiStore::new();
        store.write(&path!("set_row_count"), cmd_map(&[
            ("count", Value::Integer(10)),
            txn("t1"),
        ])).unwrap();
        store.selected_row = 8;

        // Shrink — selection must clamp
        store.write(&path!("set_row_count"), cmd_map(&[
            ("count", Value::Integer(3)),
            txn("t2"),
        ])).unwrap();
        assert_eq!(store.selected_row, 2);
    }

    // --- Screen navigation ---

    #[test]
    fn open_and_close_thread() {
        let mut store = UiStore::new();

        store.write(&path!("open"), cmd_map(&[
            ("thread_id", s("t_abc")),
            txn("t1"),
        ])).unwrap();
        assert_eq!(store.screen, Screen::Thread);
        assert_eq!(store.active_thread.as_deref(), Some("t_abc"));

        store.write(&path!("close"), cmd_map(&[txn("t2")])).unwrap();
        assert_eq!(store.screen, Screen::Inbox);
        assert_eq!(store.active_thread, None);
    }

    // --- Mode transitions ---

    #[test]
    fn enter_and_exit_insert() {
        let mut store = UiStore::new();

        store.write(&path!("enter_insert"), cmd_map(&[
            ("context", s("compose")),
            txn("t1"),
        ])).unwrap();
        assert_eq!(store.mode, Mode::Insert);
        assert_eq!(store.insert_context, Some(InsertContext::Compose));

        store.write(&path!("exit_insert"), cmd_map(&[txn("t2")])).unwrap();
        assert_eq!(store.mode, Mode::Normal);
        assert_eq!(store.insert_context, None);
    }

    #[test]
    fn enter_insert_clears_input() {
        let mut store = UiStore::new();
        store.input = "leftover".to_string();
        store.cursor = 5;

        store.write(&path!("enter_insert"), cmd_map(&[
            ("context", s("reply")),
            txn("t1"),
        ])).unwrap();
        assert!(store.input.is_empty());
        assert_eq!(store.cursor, 0);
    }

    // --- Input buffer ---

    #[test]
    fn set_and_clear_input() {
        let mut store = UiStore::new();

        store.write(&path!("set_input"), cmd_map(&[
            ("text", s("hello")),
            ("cursor", Value::Integer(3)),
            txn("t1"),
        ])).unwrap();
        assert_eq!(store.input, "hello");
        assert_eq!(store.cursor, 3);

        store.write(&path!("clear_input"), cmd_map(&[txn("t2")])).unwrap();
        assert!(store.input.is_empty());
        assert_eq!(store.cursor, 0);
    }

    #[test]
    fn set_input_clamps_cursor() {
        let mut store = UiStore::new();
        store.write(&path!("set_input"), cmd_map(&[
            ("text", s("hi")),
            ("cursor", Value::Integer(999)),
            txn("t1"),
        ])).unwrap();
        assert_eq!(store.cursor, 2); // clamped to input length
    }

    // --- Txn deduplication ---

    #[test]
    fn duplicate_txn_is_idempotent() {
        let mut store = UiStore::new();
        store.write(&path!("set_row_count"), cmd_map(&[
            ("count", Value::Integer(5)),
            txn("setup"),
        ])).unwrap();

        store.write(&path!("select_next"), cmd_map(&[txn("dup")])).unwrap();
        assert_eq!(store.selected_row, 1);

        // Same txn — should not advance again
        store.write(&path!("select_next"), cmd_map(&[txn("dup")])).unwrap();
        assert_eq!(store.selected_row, 1);
    }

    // --- Error cases ---

    #[test]
    fn unknown_command_returns_error() {
        let mut store = UiStore::new();
        let result = store.write(&path!("fly_to_moon"), cmd_map(&[txn("t1")]));
        assert!(result.is_err());
    }

    #[test]
    fn open_without_thread_id_returns_error() {
        let mut store = UiStore::new();
        let result = store.write(&path!("open"), cmd_map(&[txn("t1")]));
        assert!(result.is_err());
    }
}
```

- [ ] **Step 4: Update lib.rs**

```rust
pub mod command;
pub mod ui_store;

pub use command::{Command, TxnLog};
pub use ui_store::UiStore;
```

- [ ] **Step 5: Verify and test**

Run: `cargo check -p ox-ui && cargo test -p ox-ui`
Expected: clean build, all tests pass (~17 tests: 5 command + 12 UiStore)

- [ ] **Step 6: Commit**

```
git add crates/ox-ui/
git commit -m 'feat(ox-ui): add UiStore — in-memory state machine with command protocol

UiStore manages screen, mode, selection, scroll, input buffer, modal,
and status state. Reads return current values. Writes are commands
with txn deduplication and precondition support. State transitions
are atomic: open/close thread, enter/exit insert mode, selection
clamping, input buffer management.'
```

---

### Task 3: HistoryProvider Turn State Extensions

**Files:**
- Create: `crates/ox-history/src/turn.rs`
- Modify: `crates/ox-history/src/lib.rs`

Extend HistoryProvider with per-turn transient state for real-time
streaming. Turn state accumulates during a turn and is committed
to the message list when the turn ends.

- [ ] **Step 1: Write turn.rs**

```rust
//! Per-turn transient state for real-time streaming.
//!
//! Accumulates during a turn. Committed to the message list when
//! the agent writes to "commit". All turn state clears on commit.

use std::collections::BTreeMap;
use structfs_core_store::Value;

/// Transient state for the current in-progress turn.
#[derive(Debug, Default)]
pub struct TurnState {
    /// Accumulated streaming text (assistant response being built).
    pub streaming: String,
    /// Whether the agent is currently mid-turn.
    pub thinking: bool,
    /// Current tool call, if any: (tool_name, status).
    pub tool: Option<(String, String)>,
    /// Token usage for this turn: (input_tokens, output_tokens).
    pub tokens: (u32, u32),
}

impl TurnState {
    pub fn new() -> Self {
        TurnState::default()
    }

    /// Clear all turn state (called on commit).
    pub fn clear(&mut self) {
        self.streaming.clear();
        self.thinking = false;
        self.tool = None;
        self.tokens = (0, 0);
    }

    /// Whether there is any in-progress content.
    pub fn is_active(&self) -> bool {
        self.thinking || !self.streaming.is_empty() || self.tool.is_some()
    }

    /// Read a turn sub-path.
    pub fn read(&self, sub_path: &str) -> Option<Value> {
        match sub_path {
            "streaming" => Some(Value::String(self.streaming.clone())),
            "thinking" => Some(Value::Bool(self.thinking)),
            "tool" => Some(match &self.tool {
                Some((name, status)) => {
                    let mut map = BTreeMap::new();
                    map.insert("name".to_string(), Value::String(name.clone()));
                    map.insert("status".to_string(), Value::String(status.clone()));
                    Value::Map(map)
                }
                None => Value::Null,
            }),
            "tokens" => {
                let mut map = BTreeMap::new();
                map.insert("in".to_string(), Value::Integer(self.tokens.0 as i64));
                map.insert("out".to_string(), Value::Integer(self.tokens.1 as i64));
                Value::Map(map)
            }
            _ => None,
        }
    }

    /// Write to a turn sub-path.
    pub fn write(&mut self, sub_path: &str, value: &Value) -> bool {
        match sub_path {
            "streaming" => {
                if let Value::String(text) = value {
                    self.streaming.push_str(text);
                    return true;
                }
                false
            }
            "thinking" => {
                if let Value::Bool(b) = value {
                    self.thinking = *b;
                    return true;
                }
                false
            }
            "tool" => {
                match value {
                    Value::Map(map) => {
                        let name = map.get("name").and_then(|v| match v {
                            Value::String(s) => Some(s.clone()),
                            _ => None,
                        });
                        let status = map.get("status").and_then(|v| match v {
                            Value::String(s) => Some(s.clone()),
                            _ => None,
                        });
                        if let (Some(n), Some(s)) = (name, status) {
                            self.tool = Some((n, s));
                            return true;
                        }
                        false
                    }
                    Value::Null => {
                        self.tool = None;
                        true
                    }
                    _ => false,
                }
            }
            "tokens" => {
                if let Value::Map(map) = value {
                    let in_tokens = map.get("in").and_then(|v| match v {
                        Value::Integer(i) => Some(*i as u32),
                        _ => None,
                    });
                    let out_tokens = map.get("out").and_then(|v| match v {
                        Value::Integer(i) => Some(*i as u32),
                        _ => None,
                    });
                    if let (Some(i), Some(o)) = (in_tokens, out_tokens) {
                        self.tokens = (i, o);
                        return true;
                    }
                    false
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_accumulates() {
        let mut turn = TurnState::new();
        turn.write("streaming", &Value::String("Hello".to_string()));
        turn.write("streaming", &Value::String(" world".to_string()));
        assert_eq!(turn.read("streaming"), Some(Value::String("Hello world".to_string())));
    }

    #[test]
    fn thinking_toggles() {
        let mut turn = TurnState::new();
        assert_eq!(turn.read("thinking"), Some(Value::Bool(false)));
        turn.write("thinking", &Value::Bool(true));
        assert_eq!(turn.read("thinking"), Some(Value::Bool(true)));
    }

    #[test]
    fn tool_state() {
        let mut turn = TurnState::new();
        assert_eq!(turn.read("tool"), Some(Value::Null));

        let mut tool_map = BTreeMap::new();
        tool_map.insert("name".to_string(), Value::String("bash".to_string()));
        tool_map.insert("status".to_string(), Value::String("running".to_string()));
        turn.write("tool", &Value::Map(tool_map));

        let val = turn.read("tool").unwrap();
        if let Value::Map(m) = val {
            assert_eq!(m.get("name").unwrap(), &Value::String("bash".to_string()));
        } else {
            panic!("expected map");
        }
    }

    #[test]
    fn tokens_tracking() {
        let mut turn = TurnState::new();
        let mut map = BTreeMap::new();
        map.insert("in".to_string(), Value::Integer(100));
        map.insert("out".to_string(), Value::Integer(50));
        turn.write("tokens", &Value::Map(map));
        assert_eq!(turn.tokens, (100, 50));
    }

    #[test]
    fn clear_resets_everything() {
        let mut turn = TurnState::new();
        turn.write("streaming", &Value::String("text".to_string()));
        turn.write("thinking", &Value::Bool(true));
        turn.clear();
        assert_eq!(turn.streaming, "");
        assert!(!turn.thinking);
        assert!(!turn.is_active());
    }

    #[test]
    fn is_active_reflects_state() {
        let mut turn = TurnState::new();
        assert!(!turn.is_active());
        turn.write("thinking", &Value::Bool(true));
        assert!(turn.is_active());
    }
}
```

- [ ] **Step 2: Wire turn state into HistoryProvider**

In `crates/ox-history/src/lib.rs`, add the following changes:

1. Add `mod turn;` and `pub use turn::TurnState;` at the top.
2. Add `turn: TurnState` field to `HistoryProvider`.
3. Initialize `turn: TurnState::new()` in `HistoryProvider::new()`.
4. In `Reader::read`:
   - For paths starting with `"turn/"`, delegate to `self.turn.read(sub_path)`.
   - For `"messages"`, if `self.turn.is_active()`, append a partial assistant
     message built from `turn.streaming` to the message array before returning.
5. In `Writer::write`:
   - For paths starting with `"turn/"`, delegate to `self.turn.write(sub_path, value)`.
   - For `"commit"`: if turn has streaming content, construct an assistant message
     from the accumulated text, append it to `self.messages`, call `self.turn.clear()`.

- [ ] **Step 3: Add HistoryProvider turn tests**

Add to the existing test module in `lib.rs`:

```rust
#[test]
fn turn_streaming_visible_in_messages() {
    let mut hp = HistoryProvider::new();
    // Add a user message first
    let user_msg = serde_json::json!({"role": "user", "content": "hello"});
    let user_val = json_to_value(&user_msg);
    hp.write(&path!("append"), Record::parsed(user_val)).unwrap();

    // Start streaming
    hp.turn.write("streaming", &Value::String("Hi there".to_string()));
    hp.turn.write("thinking", &Value::Bool(true));

    // Messages should include the in-progress turn
    let messages = hp.read(&path!("messages")).unwrap().unwrap();
    let arr = match messages.as_value().unwrap() {
        Value::Array(a) => a,
        _ => panic!("expected array"),
    };
    assert_eq!(arr.len(), 2); // user + partial assistant
}

#[test]
fn commit_finalizes_turn() {
    let mut hp = HistoryProvider::new();
    let user_msg = serde_json::json!({"role": "user", "content": "hello"});
    let user_val = json_to_value(&user_msg);
    hp.write(&path!("append"), Record::parsed(user_val)).unwrap();

    // Stream content
    hp.turn.write("streaming", &Value::String("Response text".to_string()));

    // Commit
    hp.write(&path!("commit"), Record::parsed(Value::Null)).unwrap();

    // Turn should be clear
    assert!(!hp.turn.is_active());

    // Message should be committed
    let count = hp.read(&path!("count")).unwrap().unwrap();
    assert_eq!(count.as_value().unwrap(), &Value::Integer(2));
}

#[test]
fn turn_read_paths() {
    let mut hp = HistoryProvider::new();
    hp.turn.write("thinking", &Value::Bool(true));

    let val = hp.read(&path!("turn/thinking")).unwrap().unwrap();
    assert_eq!(val.as_value().unwrap(), &Value::Bool(true));
}
```

- [ ] **Step 4: Verify and test**

Run: `cargo test -p ox-history`
Expected: all existing tests pass + 3 new turn tests pass

- [ ] **Step 5: Commit**

```
git add crates/ox-history/
git commit -m 'feat(ox-history): add turn state for real-time streaming

TurnState accumulates streaming text, thinking flag, tool call info,
and token counts during a turn. Commit finalizes the turn into a
committed message. Messages read path includes in-progress turn
content as a partial assistant message.'
```

---

### Task 4: InputStore — Key Binding Translation

**Files:**
- Create: `crates/ox-ui/src/input_store.rs`
- Modify: `crates/ox-ui/src/lib.rs`

InputStore translates raw key events into command writes. It holds
a binding table, readable for help screens. It uses a ClientHandle
to read UI context (mode, screen) and write commands to target stores.

The sync Writer impl uses `tokio::task::block_in_place` to bridge
to the async broker — the same pattern as the Wasm host bridge.

- [ ] **Step 1: Write input_store.rs**

```rust
//! InputStore — translates key events into command writes.
//!
//! Holds a binding table mapping (mode, key) → (target_path, description).
//! Writes to `input/{mode}/{key}` trigger command dispatch through a
//! ClientHandle. Reads return the binding table for help/discoverability.

use std::collections::BTreeMap;

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

use crate::command::TxnLog;

/// A single key binding: which key in which mode triggers which command.
#[derive(Debug, Clone)]
pub struct Binding {
    pub mode: String,
    pub key: String,
    pub target: Path,
    pub description: String,
}

/// Callback for dispatching commands. Receives the target path and
/// command value, and performs the write (typically through a broker
/// ClientHandle).
///
/// Boxed to avoid generic parameter on InputStore, keeping it
/// object-safe and storable.
pub type CommandDispatcher = Box<dyn FnMut(&Path, Record) -> Result<Path, StoreError> + Send>;

pub struct InputStore {
    bindings: Vec<Binding>,
    dispatcher: Option<CommandDispatcher>,
    txn_counter: u64,
    txn_log: TxnLog,
}

impl InputStore {
    pub fn new(bindings: Vec<Binding>) -> Self {
        InputStore {
            bindings,
            dispatcher: None,
            txn_counter: 0,
            txn_log: TxnLog::new(),
        }
    }

    /// Set the command dispatcher (called after mounting in broker).
    pub fn set_dispatcher(&mut self, dispatcher: CommandDispatcher) {
        self.dispatcher = dispatcher;
    }

    fn next_txn(&mut self) -> String {
        self.txn_counter += 1;
        format!("input_{}", self.txn_counter)
    }

    fn bindings_for_mode(&self, mode: &str) -> Vec<Value> {
        self.bindings
            .iter()
            .filter(|b| b.mode == mode)
            .map(|b| {
                let mut map = BTreeMap::new();
                map.insert("key".to_string(), Value::String(b.key.clone()));
                map.insert("target".to_string(), Value::String(b.target.to_string()));
                map.insert(
                    "description".to_string(),
                    Value::String(b.description.clone()),
                );
                Value::Map(map)
            })
            .collect()
    }

    fn find_binding(&self, mode: &str, key: &str) -> Option<&Binding> {
        self.bindings
            .iter()
            .find(|b| b.mode == mode && b.key == key)
    }
}

impl Reader for InputStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let path_str = from.to_string();
        match path_str.as_str() {
            "normal" => Ok(Some(Record::parsed(Value::Array(
                self.bindings_for_mode("normal"),
            )))),
            "insert" => Ok(Some(Record::parsed(Value::Array(
                self.bindings_for_mode("insert"),
            )))),
            "approval" => Ok(Some(Record::parsed(Value::Array(
                self.bindings_for_mode("approval"),
            )))),
            _ => Ok(None),
        }
    }
}

impl Writer for InputStore {
    fn write(&mut self, to: &Path, _data: Record) -> Result<Path, StoreError> {
        // Path format: "{mode}/{key}"
        let path_str = to.to_string();
        let parts: Vec<&str> = path_str.splitn(2, '/').collect();
        if parts.len() != 2 {
            return Err(StoreError::store(
                "input",
                "write",
                format!("expected path format 'mode/key', got '{}'", path_str),
            ));
        }
        let (mode, key) = (parts[0], parts[1]);

        let binding = self.find_binding(mode, key).ok_or_else(|| {
            StoreError::store(
                "input",
                "write",
                format!("no binding for mode='{}' key='{}'", mode, key),
            )
        })?;
        let target = binding.target.clone();

        let txn = self.next_txn();
        let mut cmd_map = BTreeMap::new();
        cmd_map.insert("txn".to_string(), Value::String(txn));

        let dispatcher = self.dispatcher.as_mut().ok_or_else(|| {
            StoreError::store("input", "write", "no dispatcher configured")
        })?;
        dispatcher(&target, Record::parsed(Value::Map(cmd_map)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;
    use std::sync::{Arc, Mutex};

    fn test_bindings() -> Vec<Binding> {
        vec![
            Binding {
                mode: "normal".to_string(),
                key: "j".to_string(),
                target: path!("ui/select_next"),
                description: "Move selection down".to_string(),
            },
            Binding {
                mode: "normal".to_string(),
                key: "k".to_string(),
                target: path!("ui/select_prev"),
                description: "Move selection up".to_string(),
            },
            Binding {
                mode: "insert".to_string(),
                key: "Esc".to_string(),
                target: path!("ui/exit_insert"),
                description: "Exit insert mode".to_string(),
            },
        ]
    }

    #[test]
    fn read_bindings_by_mode() {
        let mut store = InputStore::new(test_bindings());
        let normal = store.read(&path!("normal")).unwrap().unwrap();
        let arr = match normal.as_value().unwrap() {
            Value::Array(a) => a,
            _ => panic!("expected array"),
        };
        assert_eq!(arr.len(), 2); // j, k

        let insert = store.read(&path!("insert")).unwrap().unwrap();
        let arr = match insert.as_value().unwrap() {
            Value::Array(a) => a,
            _ => panic!("expected array"),
        };
        assert_eq!(arr.len(), 1); // Esc
    }

    #[test]
    fn write_dispatches_to_target() {
        let mut store = InputStore::new(test_bindings());

        let dispatched: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let dispatched_clone = dispatched.clone();
        store.set_dispatcher(Box::new(move |path, data| {
            let txn = data.as_value().and_then(|v| match v {
                Value::Map(m) => m.get("txn").and_then(|t| match t {
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                }),
                _ => None,
            }).unwrap_or_default();
            dispatched_clone.lock().unwrap().push((path.to_string(), txn));
            Ok(path.clone())
        }));

        store.write(&path!("normal/j"), Record::parsed(Value::Null)).unwrap();

        let log = dispatched.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, "ui/select_next");
        assert!(log[0].1.starts_with("input_"));
    }

    #[test]
    fn write_unknown_binding_returns_error() {
        let mut store = InputStore::new(test_bindings());
        store.set_dispatcher(Box::new(|path, _| Ok(path.clone())));

        let result = store.write(&path!("normal/z"), Record::parsed(Value::Null));
        assert!(result.is_err());
    }

    #[test]
    fn write_without_dispatcher_returns_error() {
        let mut store = InputStore::new(test_bindings());
        let result = store.write(&path!("normal/j"), Record::parsed(Value::Null));
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Update lib.rs**

```rust
pub mod command;
pub mod input_store;
pub mod ui_store;

pub use command::{Command, TxnLog};
pub use input_store::{Binding, InputStore};
pub use ui_store::UiStore;
```

- [ ] **Step 3: Verify and test**

Run: `cargo test -p ox-ui`
Expected: all tests pass (~25 total)

- [ ] **Step 4: Commit**

```
git add crates/ox-ui/
git commit -m 'feat(ox-ui): add InputStore — key binding translation with dispatch

InputStore holds a binding table mapping (mode, key) to target paths.
Reads return bindings by mode for help/discoverability. Writes to
input/{mode}/{key} dispatch commands to target stores via a pluggable
CommandDispatcher. Each dispatched command carries an auto-generated
txn ID.'
```

---

### Task 5: ApprovalStore — Per-Thread Approval State

**Files:**
- Create: `crates/ox-ui/src/approval_store.rs`
- Modify: `crates/ox-ui/src/lib.rs`

ApprovalStore holds the state for one thread's approval flow.
The agent writes a request. The TUI reads pending. The user writes
a response. The agent polls for the response.

The actual blocking behavior (agent waits for user decision) is a
C3 integration concern — the host effects layer manages it. The
store is just state.

- [ ] **Step 1: Write approval_store.rs**

```rust
//! ApprovalStore — per-thread approval request/response state.
//!
//! The agent writes to "request" to post an approval request.
//! The TUI reads "pending" to discover requests.
//! The user writes to "response" to post a decision.
//! The agent reads "response" to get the decision.

use std::collections::BTreeMap;

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

/// An approval request from the agent.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub tool_name: String,
    pub input_preview: String,
}

pub struct ApprovalStore {
    pending: Option<ApprovalRequest>,
    response: Option<String>,
}

impl ApprovalStore {
    pub fn new() -> Self {
        ApprovalStore {
            pending: None,
            response: None,
        }
    }
}

impl Reader for ApprovalStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let path_str = from.to_string();
        match path_str.as_str() {
            "pending" => Ok(Some(Record::parsed(match &self.pending {
                Some(req) => {
                    let mut map = BTreeMap::new();
                    map.insert(
                        "tool_name".to_string(),
                        Value::String(req.tool_name.clone()),
                    );
                    map.insert(
                        "input_preview".to_string(),
                        Value::String(req.input_preview.clone()),
                    );
                    Value::Map(map)
                }
                None => Value::Null,
            }))),
            "response" => Ok(Some(Record::parsed(match &self.response {
                Some(decision) => Value::String(decision.clone()),
                None => Value::Null,
            }))),
            _ => Ok(None),
        }
    }
}

impl Writer for ApprovalStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let path_str = to.to_string();
        let value = data.as_value().ok_or_else(|| {
            StoreError::store("approval", "write", "write data must contain a value")
        })?;

        match path_str.as_str() {
            "request" => {
                let map = match value {
                    Value::Map(m) => m,
                    _ => {
                        return Err(StoreError::store(
                            "approval",
                            "request",
                            "request must be a Map with tool_name and input_preview",
                        ))
                    }
                };
                let tool_name = map
                    .get("tool_name")
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        StoreError::store("approval", "request", "missing tool_name")
                    })?;
                let input_preview = map
                    .get("input_preview")
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();

                self.pending = Some(ApprovalRequest {
                    tool_name,
                    input_preview,
                });
                self.response = None;
                Ok(to.clone())
            }
            "response" => {
                let decision = match value {
                    Value::String(s) => s.clone(),
                    _ => {
                        return Err(StoreError::store(
                            "approval",
                            "response",
                            "response must be a String decision",
                        ))
                    }
                };
                self.response = Some(decision);
                self.pending = None;
                Ok(to.clone())
            }
            _ => Err(StoreError::store(
                "approval",
                "write",
                format!("unknown path: {}", path_str),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;

    #[test]
    fn initial_state_has_no_pending() {
        let mut store = ApprovalStore::new();
        let pending = store.read(&path!("pending")).unwrap().unwrap();
        assert_eq!(pending.as_value().unwrap(), &Value::Null);
    }

    #[test]
    fn request_creates_pending() {
        let mut store = ApprovalStore::new();
        let mut map = BTreeMap::new();
        map.insert("tool_name".to_string(), Value::String("bash".to_string()));
        map.insert(
            "input_preview".to_string(),
            Value::String("ls -la".to_string()),
        );
        store
            .write(&path!("request"), Record::parsed(Value::Map(map)))
            .unwrap();

        let pending = store.read(&path!("pending")).unwrap().unwrap();
        let m = match pending.as_value().unwrap() {
            Value::Map(m) => m,
            _ => panic!("expected map"),
        };
        assert_eq!(
            m.get("tool_name").unwrap(),
            &Value::String("bash".to_string())
        );
    }

    #[test]
    fn response_clears_pending() {
        let mut store = ApprovalStore::new();
        let mut map = BTreeMap::new();
        map.insert("tool_name".to_string(), Value::String("bash".to_string()));
        store
            .write(&path!("request"), Record::parsed(Value::Map(map)))
            .unwrap();

        store
            .write(
                &path!("response"),
                Record::parsed(Value::String("allow_once".to_string())),
            )
            .unwrap();

        // Pending is cleared
        let pending = store.read(&path!("pending")).unwrap().unwrap();
        assert_eq!(pending.as_value().unwrap(), &Value::Null);

        // Response is available
        let resp = store.read(&path!("response")).unwrap().unwrap();
        assert_eq!(
            resp.as_value().unwrap(),
            &Value::String("allow_once".to_string())
        );
    }

    #[test]
    fn request_clears_previous_response() {
        let mut store = ApprovalStore::new();

        // First cycle
        let mut map = BTreeMap::new();
        map.insert("tool_name".to_string(), Value::String("bash".to_string()));
        store
            .write(&path!("request"), Record::parsed(Value::Map(map)))
            .unwrap();
        store
            .write(
                &path!("response"),
                Record::parsed(Value::String("allow_once".to_string())),
            )
            .unwrap();

        // Second request clears old response
        let mut map2 = BTreeMap::new();
        map2.insert("tool_name".to_string(), Value::String("write".to_string()));
        store
            .write(&path!("request"), Record::parsed(Value::Map(map2)))
            .unwrap();

        let resp = store.read(&path!("response")).unwrap().unwrap();
        assert_eq!(resp.as_value().unwrap(), &Value::Null);
    }
}
```

- [ ] **Step 2: Update lib.rs**

```rust
pub mod approval_store;
pub mod command;
pub mod input_store;
pub mod ui_store;

pub use approval_store::ApprovalStore;
pub use command::{Command, TxnLog};
pub use input_store::{Binding, InputStore};
pub use ui_store::UiStore;
```

- [ ] **Step 3: Verify and test**

Run: `cargo test -p ox-ui`
Expected: all tests pass (~29 total)

- [ ] **Step 4: Run full workspace check**

Run: `cargo check && cargo test -p ox-ui -p ox-history`
Expected: clean build, all ox-ui and ox-history tests pass

- [ ] **Step 5: Commit**

```
git add crates/ox-ui/
git commit -m 'feat(ox-ui): add ApprovalStore — per-thread approval request/response

ApprovalStore holds pending approval requests and user responses.
Agent writes request (tool_name, input_preview), TUI reads pending,
user writes response (decision string). Request clears on response,
response clears on new request. Blocking behavior is a C3 concern.'
```

---

## Summary

| Task | What | Tests |
|------|------|-------|
| 1 | ox-ui crate scaffold + command protocol | 5 |
| 2 | UiStore — state machine with commands | ~12 |
| 3 | HistoryProvider turn state | ~9 |
| 4 | InputStore — binding table + dispatch | 4 |
| 5 | ApprovalStore — request/response state | 4 |

**Total: ~34 tests across 5 commits.**

After Plan C2, we have:
- **UiStore**: replaces App struct's 20+ state fields
- **HistoryProvider turn state**: replaces ThreadView + AppEvent streaming
- **InputStore**: replaces handle_normal_key / handle_insert_key / handle_approval_key
- **ApprovalStore**: replaces pending_approval + AppControl channel

Plan C3 wires these into the broker and rewrites the TUI event loop.

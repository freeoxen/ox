# Typed StructFS Boundary Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace stringly-typed StructFS boundary communication with serde-derived types, then restructure the 1,295-line event loop into per-screen handlers.

**Architecture:** A new `ox-types` leaf crate holds all shared types (Screen, Mode, UiCommand, TurnState, etc.) with serde derives. `ClientHandle` and `SyncClientAdapter` gain `write_typed`/`read_typed` methods that handle `to_value`/`from_value` internally. UiStore's Reader/Writer, TurnState's read/write, and all call sites switch to typed APIs. Finally, the event loop decomposes into per-screen shell handlers.

**Tech Stack:** Rust, serde, structfs-core-store, structfs-serde-store, ox-broker, ox-ui, ox-history, ox-cli

**Spec:** `docs/superpowers/specs/2026-04-11-typed-structfs-boundary-design.md`

---

### Task 1: Create `ox-types` crate with UI enums

**Files:**
- Create: `crates/ox-types/Cargo.toml`
- Create: `crates/ox-types/src/lib.rs`
- Create: `crates/ox-types/src/ui.rs`
- Modify: `Cargo.toml` (workspace root, add member)

- [ ] **Step 1: Create crate directory and Cargo.toml**

```bash
mkdir -p crates/ox-types/src
```

Write `crates/ox-types/Cargo.toml`:

```toml
[package]
name = "ox-types"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
homepage.workspace = true
authors.workspace = true
description = "Shared types for the ox agent framework — pure data, serde-ready"
publish = false

[dependencies]
serde = { version = "1", features = ["derive"] }
```

- [ ] **Step 2: Add to workspace members**

In `Cargo.toml` (workspace root), add `"crates/ox-types"` to the `members` array.

- [ ] **Step 3: Write the UI enum types**

Write `crates/ox-types/src/ui.rs`:

```rust
//! UI state types shared across all ox platform shells.

use serde::{Deserialize, Serialize};

/// Which screen is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Screen {
    Inbox,
    Thread,
    Settings,
}

impl Default for Screen {
    fn default() -> Self {
        Screen::Inbox
    }
}

/// Editing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Normal,
    Insert,
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Normal
    }
}

/// Context for insert mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertContext {
    Compose,
    Reply,
    Search,
    Command,
}

/// Application-level pending action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingAction {
    SendInput,
    Quit,
    OpenSelected,
    ArchiveSelected,
}
```

- [ ] **Step 4: Write the lib.rs re-exports**

Write `crates/ox-types/src/lib.rs`:

```rust
//! Shared types for the ox agent framework.
//!
//! Pure data — no behavior, no Store impls. Leaf of the dependency tree.

pub mod ui;

pub use ui::{InsertContext, Mode, PendingAction, Screen};
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo check -p ox-types`
Expected: compiles with no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-types/ Cargo.toml
git commit -m "feat: create ox-types crate with UI state enums"
```

---

### Task 2: Add snapshot and command types to `ox-types`

**Files:**
- Create: `crates/ox-types/src/snapshot.rs`
- Create: `crates/ox-types/src/command.rs`
- Modify: `crates/ox-types/src/lib.rs`

- [ ] **Step 1: Write snapshot types**

Write `crates/ox-types/src/snapshot.rs`:

```rust
//! Typed snapshots for reading store state across the StructFS boundary.

use serde::{Deserialize, Serialize};

use crate::ui::{InsertContext, Mode, PendingAction, Screen};

/// Complete UI state snapshot — the read contract for UiStore.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiSnapshot {
    pub screen: Screen,
    pub mode: Mode,
    pub active_thread: Option<String>,
    pub insert_context: Option<InsertContext>,
    pub selected_row: usize,
    pub scroll: usize,
    pub scroll_max: usize,
    pub viewport_height: usize,
    pub input: InputSnapshot,
    pub pending_action: Option<PendingAction>,
    pub search: SearchSnapshot,
}

/// Text input state snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InputSnapshot {
    pub content: String,
    pub cursor: usize,
}

/// Search state snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchSnapshot {
    pub chips: Vec<String>,
    pub live_query: String,
    pub active: bool,
}
```

- [ ] **Step 2: Write command enum**

Write `crates/ox-types/src/command.rs`:

```rust
//! Typed commands for writing to UiStore across the StructFS boundary.

use serde::{Deserialize, Serialize};

use crate::ui::InsertContext;

/// Typed command for UiStore writes.
///
/// Serializes with `#[serde(tag = "command")]` so the command name is a field
/// in the resulting map, e.g. `{"command": "open", "thread_id": "abc"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum UiCommand {
    SelectNext,
    SelectPrev,
    SelectFirst,
    SelectLast,
    Open { thread_id: String },
    Close,
    GoToSettings,
    GoToInbox,
    EnterInsert { context: InsertContext },
    ExitInsert,
    SetInput { content: String, cursor: usize },
    ClearInput,
    ScrollUp,
    ScrollDown,
    ScrollToTop,
    ScrollToBottom,
    ScrollPageUp,
    ScrollPageDown,
    ScrollHalfPageUp,
    ScrollHalfPageDown,
    SetScrollMax { max: usize },
    SetViewportHeight { height: usize },
    SetRowCount { count: usize },
    SendInput,
    Quit,
    OpenSelected,
    ArchiveSelected,
    ClearPendingAction,
    SearchInsertChar { char: char },
    SearchDeleteChar,
    SearchClear,
    SearchSaveChip,
    SearchDismissChip { index: usize },
}
```

- [ ] **Step 3: Update lib.rs with new modules and re-exports**

Replace `crates/ox-types/src/lib.rs`:

```rust
//! Shared types for the ox agent framework.
//!
//! Pure data — no behavior, no Store impls. Leaf of the dependency tree.

pub mod command;
pub mod snapshot;
pub mod ui;

pub use command::UiCommand;
pub use snapshot::{InputSnapshot, SearchSnapshot, UiSnapshot};
pub use ui::{InsertContext, Mode, PendingAction, Screen};
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p ox-types`
Expected: compiles with no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-types/src/
git commit -m "feat(ox-types): add UiSnapshot, UiCommand, and sub-types"
```

---

### Task 3: Add turn state and approval types to `ox-types`

**Files:**
- Create: `crates/ox-types/src/turn.rs`
- Create: `crates/ox-types/src/approval.rs`
- Modify: `crates/ox-types/src/lib.rs`

- [ ] **Step 1: Write turn state types**

Write `crates/ox-types/src/turn.rs`:

```rust
//! Turn sub-types — pure data used by TurnState and agent workers.
//!
//! TurnState itself lives in ox-history (it has StructFS read/write behavior).
//! These sub-types are pure data shared across crates.

use serde::{Deserialize, Serialize};

/// Status of a currently executing tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStatus {
    pub name: String,
    pub status: String,
}

/// Token usage for a turn.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}
```

- [ ] **Step 2: Write approval types**

Write `crates/ox-types/src/approval.rs`:

```rust
//! Approval request types for tool call policy enforcement.

use serde::{Deserialize, Serialize};

/// An approval request from the agent for a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub tool_name: String,
    pub input_preview: String,
}
```

- [ ] **Step 3: Update lib.rs**

Replace `crates/ox-types/src/lib.rs`:

```rust
//! Shared types for the ox agent framework.
//!
//! Pure data — no behavior, no Store impls. Leaf of the dependency tree.

pub mod approval;
pub mod command;
pub mod snapshot;
pub mod turn;
pub mod ui;

pub use approval::ApprovalRequest;
pub use command::UiCommand;
pub use snapshot::{InputSnapshot, SearchSnapshot, UiSnapshot};
pub use turn::{TokenUsage, ToolStatus};
pub use ui::{InsertContext, Mode, PendingAction, Screen};
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p ox-types`
Expected: compiles with no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-types/src/
git commit -m "feat(ox-types): add TokenUsage, ToolStatus, ApprovalRequest"
```

---

### Task 4: Add serde round-trip tests for all ox-types

**Files:**
- Create: `crates/ox-types/tests/serde_roundtrip.rs`
- Modify: `crates/ox-types/Cargo.toml` (add dev-dep on serde_json)

- [ ] **Step 1: Add serde_json dev-dependency**

In `crates/ox-types/Cargo.toml`, add:

```toml
[dev-dependencies]
serde_json = "1"
```

- [ ] **Step 2: Write round-trip tests**

Write `crates/ox-types/tests/serde_roundtrip.rs`:

```rust
use ox_types::*;

/// Helper: serialize to JSON and back, assert equality.
fn roundtrip<T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + PartialEq>(
    value: &T,
) {
    let json = serde_json::to_value(value).expect("serialize");
    let back: T = serde_json::from_value(json).expect("deserialize");
    assert_eq!(*value, back);
}

#[test]
fn screen_roundtrip() {
    roundtrip(&Screen::Inbox);
    roundtrip(&Screen::Thread);
    roundtrip(&Screen::Settings);
}

#[test]
fn screen_serializes_as_snake_case() {
    let json = serde_json::to_value(Screen::Settings).unwrap();
    assert_eq!(json, serde_json::json!("settings"));
}

#[test]
fn mode_roundtrip() {
    roundtrip(&Mode::Normal);
    roundtrip(&Mode::Insert);
}

#[test]
fn insert_context_roundtrip() {
    roundtrip(&InsertContext::Compose);
    roundtrip(&InsertContext::Reply);
    roundtrip(&InsertContext::Search);
    roundtrip(&InsertContext::Command);
}

#[test]
fn pending_action_roundtrip() {
    roundtrip(&PendingAction::SendInput);
    roundtrip(&PendingAction::Quit);
    roundtrip(&PendingAction::OpenSelected);
    roundtrip(&PendingAction::ArchiveSelected);
}

#[test]
fn pending_action_serializes_as_snake_case() {
    let json = serde_json::to_value(PendingAction::SendInput).unwrap();
    assert_eq!(json, serde_json::json!("send_input"));
}

#[test]
fn ui_snapshot_default() {
    let snap = UiSnapshot::default();
    assert_eq!(snap.screen, Screen::Inbox);
    assert_eq!(snap.mode, Mode::Normal);
    assert!(snap.active_thread.is_none());
    assert!(snap.pending_action.is_none());
}

#[test]
fn ui_snapshot_roundtrip() {
    let snap = UiSnapshot {
        screen: Screen::Thread,
        mode: Mode::Insert,
        active_thread: Some("t_abc".to_string()),
        insert_context: Some(InsertContext::Reply),
        selected_row: 3,
        scroll: 10,
        scroll_max: 50,
        viewport_height: 20,
        input: InputSnapshot {
            content: "hello".to_string(),
            cursor: 5,
        },
        pending_action: Some(PendingAction::SendInput),
        search: SearchSnapshot {
            chips: vec!["bug".to_string()],
            live_query: "fix".to_string(),
            active: true,
        },
    };
    let json = serde_json::to_value(&snap).expect("serialize");
    let back: UiSnapshot = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back.screen, Screen::Thread);
    assert_eq!(back.active_thread.as_deref(), Some("t_abc"));
    assert_eq!(back.input.content, "hello");
    assert_eq!(back.search.chips, vec!["bug"]);
}

#[test]
fn ui_command_tagged_serialization() {
    let cmd = UiCommand::Open {
        thread_id: "t_123".to_string(),
    };
    let json = serde_json::to_value(&cmd).unwrap();
    assert_eq!(json["command"], "open");
    assert_eq!(json["thread_id"], "t_123");
}

#[test]
fn ui_command_unit_variant() {
    let cmd = UiCommand::ScrollUp;
    let json = serde_json::to_value(&cmd).unwrap();
    assert_eq!(json["command"], "scroll_up");
}

#[test]
fn ui_command_enter_insert_roundtrip() {
    let cmd = UiCommand::EnterInsert {
        context: InsertContext::Compose,
    };
    let json = serde_json::to_value(&cmd).unwrap();
    let back: UiCommand = serde_json::from_value(json).unwrap();
    match back {
        UiCommand::EnterInsert { context } => assert_eq!(context, InsertContext::Compose),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn ui_command_search_dismiss_chip_roundtrip() {
    let cmd = UiCommand::SearchDismissChip { index: 2 };
    let json = serde_json::to_value(&cmd).unwrap();
    assert_eq!(json["command"], "search_dismiss_chip");
    assert_eq!(json["index"], 2);
    let back: UiCommand = serde_json::from_value(json).unwrap();
    match back {
        UiCommand::SearchDismissChip { index } => assert_eq!(index, 2),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn tool_status_roundtrip() {
    let ts = ToolStatus {
        name: "bash".to_string(),
        status: "running".to_string(),
    };
    roundtrip(&ts);
}

#[test]
fn token_usage_default() {
    let tu = TokenUsage::default();
    assert_eq!(tu.input_tokens, 0);
    assert_eq!(tu.output_tokens, 0);
}

#[test]
fn token_usage_roundtrip() {
    let tu = TokenUsage {
        input_tokens: 100,
        output_tokens: 50,
    };
    roundtrip(&tu);
}

#[test]
fn approval_request_roundtrip() {
    let req = ApprovalRequest {
        tool_name: "write_file".to_string(),
        input_preview: "{\"path\": \"foo.rs\"}".to_string(),
    };
    let json = serde_json::to_value(&req).expect("serialize");
    let back: ApprovalRequest = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back.tool_name, "write_file");
    assert_eq!(back.input_preview, "{\"path\": \"foo.rs\"}");
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p ox-types`
Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-types/
git commit -m "test(ox-types): add serde round-trip tests for all types"
```

---

### Task 5: Add `write_typed`/`read_typed` to `ClientHandle`

**Files:**
- Modify: `crates/ox-broker/Cargo.toml` (add structfs-serde-store, serde deps)
- Modify: `crates/ox-broker/src/client.rs`

- [ ] **Step 1: Add dependencies to ox-broker**

In `crates/ox-broker/Cargo.toml`, add to `[dependencies]`:

```toml
serde = { version = "1", features = ["derive"] }
structfs-serde-store = { workspace = true }
```

- [ ] **Step 2: Write the failing test**

Add to the bottom of `crates/ox-broker/src/client.rs` (inside a `#[cfg(test)] mod tests` block, or if one exists, add to it). If there's no test module in client.rs, check if tests are in `lib.rs` and add there instead. The test needs a running broker, so it goes in `crates/ox-broker/src/lib.rs` test module where broker setup infrastructure exists.

Add these tests at the end of the existing `#[cfg(test)]` module in `crates/ox-broker/src/lib.rs`:

```rust
    #[tokio::test]
    async fn write_typed_then_read_typed() {
        use serde::{Deserialize, Serialize};

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        struct Greeting {
            message: String,
            count: u32,
        }

        let broker = BrokerStore::default();
        let store = test_support::MemoryStore::new();
        let _h = broker.mount(path!("data"), store).await;
        let client = broker.client();

        let greeting = Greeting {
            message: "hello".to_string(),
            count: 42,
        };
        client
            .write_typed(&path!("data/greeting"), &greeting)
            .await
            .unwrap();

        let back: Option<Greeting> = client
            .read_typed(&path!("data/greeting"))
            .await
            .unwrap();
        assert_eq!(back, Some(greeting));
    }

    #[tokio::test]
    async fn read_typed_returns_none_for_missing() {
        use serde::Deserialize;

        #[derive(Debug, Deserialize)]
        struct Anything {
            _x: String,
        }

        let broker = BrokerStore::default();
        let store = test_support::MemoryStore::new();
        let _h = broker.mount(path!("data"), store).await;
        let client = broker.client();

        let result: Option<Anything> = client
            .read_typed(&path!("data/nonexistent"))
            .await
            .unwrap();
        assert!(result.is_none());
    }
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p ox-broker write_typed`
Expected: FAIL — `write_typed` and `read_typed` don't exist yet.

- [ ] **Step 4: Implement `write_typed` and `read_typed` on ClientHandle**

In `crates/ox-broker/src/client.rs`, add these methods to the `impl ClientHandle` block (after the existing `write` method):

```rust
    /// Write a typed value by serializing it to a StructFS Value via serde.
    pub async fn write_typed<T: serde::Serialize>(
        &self,
        to: &Path,
        value: &T,
    ) -> Result<Path, StoreError> {
        let v = structfs_serde_store::to_value(value)
            .map_err(|e| StoreError::store("broker", "write_typed", &e.to_string()))?;
        self.write(to, Record::parsed(v)).await
    }

    /// Read a typed value by deserializing a StructFS Value via serde.
    ///
    /// Returns `Ok(None)` if the path does not exist. Returns `Ok(None)` if
    /// the record has no value. Returns an error if deserialization fails.
    pub async fn read_typed<T: serde::de::DeserializeOwned>(
        &self,
        from: &Path,
    ) -> Result<Option<T>, StoreError> {
        match self.read(from).await? {
            Some(record) => match record.as_value() {
                Some(value) => {
                    let typed = structfs_serde_store::from_value(value.clone())
                        .map_err(|e| StoreError::store("broker", "read_typed", &e.to_string()))?;
                    Ok(Some(typed))
                }
                None => Ok(None),
            },
            None => Ok(None),
        }
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ox-broker write_typed`
Expected: both tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-broker/
git commit -m "feat(ox-broker): add write_typed/read_typed to ClientHandle"
```

---

### Task 6: Add `write_typed`/`read_typed` to `SyncClientAdapter`

**Files:**
- Modify: `crates/ox-broker/src/sync_adapter.rs`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `crates/ox-broker/src/sync_adapter.rs`:

```rust
    #[test]
    fn sync_adapter_write_typed_then_read_typed() {
        use serde::{Deserialize, Serialize};

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        struct Item {
            name: String,
            value: i32,
        }

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        let (broker, _handles) = rt.block_on(async {
            let broker = BrokerStore::default();
            let store = crate::test_support::MemoryStore::new();
            let h = broker.mount(path!("data"), store).await;
            (broker, vec![h])
        });

        let client = broker.client().scoped("data");
        let mut adapter = SyncClientAdapter::new(client, rt.handle().clone());

        let item = Item {
            name: "widget".to_string(),
            value: 99,
        };
        adapter.write_typed(&path!("item"), &item).unwrap();

        let back: Option<Item> = adapter.read_typed(&path!("item")).unwrap();
        assert_eq!(back, Some(item));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p ox-broker sync_adapter_write_typed`
Expected: FAIL — methods don't exist yet.

- [ ] **Step 3: Implement `write_typed` and `read_typed` on SyncClientAdapter**

In `crates/ox-broker/src/sync_adapter.rs`, add these methods to the `impl SyncClientAdapter` block (before the `Reader` impl):

```rust
    /// Write a typed value (sync version).
    pub fn write_typed<T: serde::Serialize>(
        &mut self,
        to: &Path,
        value: &T,
    ) -> Result<Path, StoreError> {
        let v = structfs_serde_store::to_value(value)
            .map_err(|e| StoreError::store("broker", "write_typed", &e.to_string()))?;
        self.write(to, Record::parsed(v))
    }

    /// Read a typed value (sync version).
    pub fn read_typed<T: serde::de::DeserializeOwned>(
        &mut self,
        from: &Path,
    ) -> Result<Option<T>, StoreError> {
        match self.read(from)? {
            Some(record) => match record.as_value() {
                Some(value) => {
                    let typed = structfs_serde_store::from_value(value.clone())
                        .map_err(|e| StoreError::store("broker", "read_typed", &e.to_string()))?;
                    Ok(Some(typed))
                }
                None => Ok(None),
            },
            None => Ok(None),
        }
    }
```

Add `use structfs_core_store::Record;` to the imports at the top of the file if not already present.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ox-broker sync_adapter_write_typed`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-broker/src/sync_adapter.rs
git commit -m "feat(ox-broker): add write_typed/read_typed to SyncClientAdapter"
```

---

### Task 7: UiStore imports enums from `ox-types`, removes local definitions

**Files:**
- Modify: `crates/ox-ui/Cargo.toml` (add ox-types dep)
- Modify: `crates/ox-ui/src/ui_store.rs`
- Modify: `crates/ox-ui/src/lib.rs`

- [ ] **Step 1: Add ox-types dependency to ox-ui**

In `crates/ox-ui/Cargo.toml`, add to `[dependencies]`:

```toml
ox-types = { path = "../ox-types" }
```

- [ ] **Step 2: Replace local enum definitions in ui_store.rs with imports**

In `crates/ox-ui/src/ui_store.rs`, remove the local enum definitions (lines 17-39, the `Screen`, `Mode`, and `InsertContext` enums) and replace with imports:

```rust
use ox_types::{InsertContext, Mode, PendingAction, Screen};
```

- [ ] **Step 3: Change `pending_action` field from `Option<String>` to `Option<PendingAction>`**

In `crates/ox-ui/src/ui_store.rs`, change the field in the `UiStore` struct:

```rust
    pending_action: Option<PendingAction>,
```

Update the `new()` constructor — this is already `None`, so no change needed.

Update `pending_action_value()`:

```rust
    fn pending_action_value(&self) -> Value {
        match &self.pending_action {
            Some(pa) => structfs_serde_store::to_value(pa)
                .unwrap_or(Value::Null),
            None => Value::Null,
        }
    }
```

Update the Writer's pending action handling (the `"send_input" | "quit" | "open_selected" | "archive_selected"` arm at line 522):

```rust
            "send_input" => {
                self.pending_action = Some(PendingAction::SendInput);
                Ok(path!("pending_action"))
            }
            "quit" => {
                self.pending_action = Some(PendingAction::Quit);
                Ok(path!("pending_action"))
            }
            "open_selected" => {
                self.pending_action = Some(PendingAction::OpenSelected);
                Ok(path!("pending_action"))
            }
            "archive_selected" => {
                self.pending_action = Some(PendingAction::ArchiveSelected);
                Ok(path!("pending_action"))
            }
```

- [ ] **Step 4: Re-export types from ox-ui lib.rs**

In `crates/ox-ui/src/lib.rs`, add re-exports so downstream crates that currently import from `ox_ui` still work:

```rust
pub use ox_types::{InsertContext, Mode, PendingAction, Screen};
```

- [ ] **Step 5: Remove the `parse_insert_context` helper**

In `crates/ox-ui/src/ui_store.rs`, delete the `parse_insert_context` method and update the `enter_insert` arm in the Writer to deserialize the context directly:

```rust
            "enter_insert" => {
                let context_str = cmd.get_str("context").ok_or_else(|| {
                    StoreError::store("ui", "enter_insert", "missing required field: context")
                })?;
                let ctx: InsertContext = serde_json::from_value(
                    serde_json::Value::String(context_str.to_string())
                ).map_err(|_| {
                    StoreError::store("ui", "enter_insert", "unknown insert context")
                })?;
                self.mode = Mode::Insert;
                self.insert_context = Some(ctx);
                let _ = self
                    .text_input_store
                    .write(&path!("clear"), Record::parsed(Value::Null));
                Ok(path!("mode"))
            }
```

- [ ] **Step 6: Run existing tests**

Run: `cargo test -p ox-ui`
Expected: all existing tests pass. The wire format (`Value::String("inbox")`, etc.) is unchanged because serde's `rename_all = "snake_case"` produces the same strings.

- [ ] **Step 7: Run full workspace check**

Run: `cargo check`
Expected: full workspace compiles. No downstream breakage since the enums have the same public API.

- [ ] **Step 8: Commit**

```bash
git add crates/ox-ui/ crates/ox-types/
git commit -m "refactor(ox-ui): import Screen, Mode, InsertContext, PendingAction from ox-types"
```

---

### Task 8: UiStore Reader uses `UiSnapshot` via `to_value`

**Files:**
- Modify: `crates/ox-ui/src/ui_store.rs`

- [ ] **Step 1: Write a test that reads a full snapshot and checks typed deserialization**

Add to the test module in `crates/ox-ui/src/ui_store.rs`:

```rust
    #[test]
    fn full_read_deserializes_as_ui_snapshot() {
        let mut store = UiStore::new();
        // Set some state
        store
            .write(
                &path!("open"),
                cmd_map(&[("thread_id", Value::String("t_1".into()))]),
            )
            .unwrap();

        let record = store.read(&path!("")).unwrap().unwrap();
        let value = record.as_value().unwrap();
        let snapshot: ox_types::UiSnapshot =
            structfs_serde_store::from_value(value.clone()).unwrap();
        assert_eq!(snapshot.screen, ox_types::Screen::Thread);
        assert_eq!(snapshot.active_thread.as_deref(), Some("t_1"));
        assert_eq!(snapshot.mode, ox_types::Mode::Normal);
    }
```

- [ ] **Step 2: Run to verify it passes (the current manual map should produce a compatible shape)**

Run: `cargo test -p ox-ui full_read_deserializes`
Expected: this may pass or fail depending on whether the current manual map field names match UiSnapshot's serde field names. If it fails, the next step fixes it.

- [ ] **Step 3: Replace `all_fields_map` with `snapshot()` + `to_value`**

In `crates/ox-ui/src/ui_store.rs`, replace the `all_fields_map` method (lines 165-212) with:

```rust
    fn snapshot(&mut self) -> ox_types::UiSnapshot {
        let (content, cursor) = self.text_input_store.content_and_cursor();
        ox_types::UiSnapshot {
            screen: self.screen,
            mode: self.mode,
            active_thread: self.active_thread.clone(),
            insert_context: self.insert_context,
            selected_row: self.selected_row,
            scroll: self.scroll,
            scroll_max: self.scroll_max,
            viewport_height: self.viewport_height,
            input: ox_types::InputSnapshot { content, cursor },
            pending_action: self.pending_action,
            search: ox_types::SearchSnapshot {
                chips: self.search_chips.clone(),
                live_query: self.search_live_query.clone(),
                active: self.search_active(),
            },
        }
    }
```

Update the Reader's empty-path case to use it:

```rust
            "" => {
                let snapshot = self.snapshot();
                structfs_serde_store::to_value(&snapshot)
                    .map_err(|e| StoreError::store("ui", "read", &e.to_string()))?
            }
```

- [ ] **Step 4: Add `content_and_cursor` method to TextInputStore**

In `crates/ox-ui/src/text_input_store.rs`, add a public accessor:

```rust
    /// Return current content and cursor position.
    pub fn content_and_cursor(&self) -> (String, usize) {
        (self.content.clone(), self.cursor)
    }
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ox-ui`
Expected: all tests pass. The snapshot produces the same Value shape as the old manual map.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-ui/src/
git commit -m "refactor(ox-ui): UiStore Reader uses UiSnapshot via to_value"
```

---

### Task 9: UiStore Writer uses `UiCommand` via `from_value`

**Files:**
- Modify: `crates/ox-ui/src/ui_store.rs`

- [ ] **Step 1: Write a test that writes a typed UiCommand and verifies state**

Add to the test module in `crates/ox-ui/src/ui_store.rs`:

```rust
    #[test]
    fn typed_command_open() {
        let mut store = UiStore::new();
        let cmd = ox_types::UiCommand::Open {
            thread_id: "t_42".to_string(),
        };
        let value = structfs_serde_store::to_value(&cmd).unwrap();
        store
            .write(&path!(""), Record::parsed(value))
            .unwrap();
        assert_eq!(
            read_str(&mut store, "screen"),
            Value::String("thread".into())
        );
        assert_eq!(
            read_str(&mut store, "active_thread"),
            Value::String("t_42".into())
        );
    }

    #[test]
    fn typed_command_enter_insert() {
        let mut store = UiStore::new();
        let cmd = ox_types::UiCommand::EnterInsert {
            context: ox_types::InsertContext::Compose,
        };
        let value = structfs_serde_store::to_value(&cmd).unwrap();
        store
            .write(&path!(""), Record::parsed(value))
            .unwrap();
        assert_eq!(
            read_str(&mut store, "mode"),
            Value::String("insert".into())
        );
        assert_eq!(
            read_str(&mut store, "insert_context"),
            Value::String("compose".into())
        );
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p ox-ui typed_command`
Expected: FAIL — the Writer currently routes by path component, not by command tag in the value.

- [ ] **Step 3: Update Writer to accept UiCommand at the root path**

In `crates/ox-ui/src/ui_store.rs`, in the `Writer::write` method, add a branch before the existing `match command` that handles writes to the root path `""` as typed commands:

```rust
        // Try typed UiCommand for writes to root path
        if command.is_empty() || command == "" {
            // Attempt to parse as UiCommand
            if let Ok(ui_cmd) = structfs_serde_store::from_value::<ox_types::UiCommand>(value.clone()) {
                return self.handle_ui_command(ui_cmd);
            }
            // Fall through to legacy Command parsing below
        }
```

Then add the `handle_ui_command` method to `impl UiStore`:

```rust
    fn handle_ui_command(&mut self, cmd: ox_types::UiCommand) -> Result<Path, StoreError> {
        use ox_types::UiCommand;
        match cmd {
            UiCommand::SelectNext => {
                if self.selected_row + 1 < self.row_count {
                    self.selected_row += 1;
                }
                Ok(path!("selected_row"))
            }
            UiCommand::SelectPrev => {
                if self.selected_row > 0 {
                    self.selected_row -= 1;
                }
                Ok(path!("selected_row"))
            }
            UiCommand::SelectFirst => {
                self.selected_row = 0;
                Ok(path!("selected_row"))
            }
            UiCommand::SelectLast => {
                if self.row_count > 0 {
                    self.selected_row = self.row_count - 1;
                }
                Ok(path!("selected_row"))
            }
            UiCommand::Open { thread_id } => {
                self.active_thread = Some(thread_id);
                self.screen = Screen::Thread;
                self.scroll = 0;
                Ok(path!("screen"))
            }
            UiCommand::Close => {
                self.active_thread = None;
                self.screen = Screen::Inbox;
                self.mode = Mode::Normal;
                self.insert_context = None;
                self.scroll = 0;
                self.scroll_max = 0;
                Ok(path!("screen"))
            }
            UiCommand::GoToSettings => {
                self.screen = Screen::Settings;
                self.mode = Mode::Normal;
                Ok(path!("screen"))
            }
            UiCommand::GoToInbox => {
                self.screen = Screen::Inbox;
                self.mode = Mode::Normal;
                Ok(path!("screen"))
            }
            UiCommand::EnterInsert { context } => {
                self.mode = Mode::Insert;
                self.insert_context = Some(context);
                let _ = self
                    .text_input_store
                    .write(&path!("clear"), Record::parsed(Value::Null));
                Ok(path!("mode"))
            }
            UiCommand::ExitInsert => {
                self.mode = Mode::Normal;
                self.insert_context = None;
                Ok(path!("mode"))
            }
            UiCommand::SetInput { content, cursor } => {
                let mut replace_map = BTreeMap::new();
                let cursor_clamped = cursor.min(content.len());
                replace_map.insert("content".to_string(), Value::String(content));
                replace_map.insert("cursor".to_string(), Value::Integer(cursor_clamped as i64));
                self.text_input_store
                    .write(&path!("replace"), Record::parsed(Value::Map(replace_map)))
            }
            UiCommand::ClearInput => self
                .text_input_store
                .write(&path!("clear"), Record::parsed(Value::Null)),
            UiCommand::ScrollUp => {
                if self.scroll < self.scroll_max {
                    self.scroll += 1;
                }
                Ok(path!("scroll"))
            }
            UiCommand::ScrollDown => {
                self.scroll = self.scroll.saturating_sub(1);
                Ok(path!("scroll"))
            }
            UiCommand::ScrollToTop => {
                self.scroll = self.scroll_max;
                Ok(path!("scroll"))
            }
            UiCommand::ScrollToBottom => {
                self.scroll = 0;
                Ok(path!("scroll"))
            }
            UiCommand::ScrollPageUp => {
                self.scroll = (self.scroll + self.viewport_height).min(self.scroll_max);
                Ok(path!("scroll"))
            }
            UiCommand::ScrollPageDown => {
                self.scroll = self.scroll.saturating_sub(self.viewport_height);
                Ok(path!("scroll"))
            }
            UiCommand::ScrollHalfPageUp => {
                let half = self.viewport_height / 2;
                self.scroll = (self.scroll + half).min(self.scroll_max);
                Ok(path!("scroll"))
            }
            UiCommand::ScrollHalfPageDown => {
                let half = self.viewport_height / 2;
                self.scroll = self.scroll.saturating_sub(half);
                Ok(path!("scroll"))
            }
            UiCommand::SetScrollMax { max } => {
                self.scroll_max = max;
                if self.scroll > self.scroll_max {
                    self.scroll = self.scroll_max;
                }
                Ok(path!("scroll_max"))
            }
            UiCommand::SetViewportHeight { height } => {
                self.viewport_height = height;
                Ok(path!("viewport_height"))
            }
            UiCommand::SetRowCount { count } => {
                self.row_count = count;
                if self.row_count > 0 && self.selected_row >= self.row_count {
                    self.selected_row = self.row_count - 1;
                } else if self.row_count == 0 {
                    self.selected_row = 0;
                }
                Ok(path!("row_count"))
            }
            UiCommand::SendInput => {
                self.pending_action = Some(PendingAction::SendInput);
                Ok(path!("pending_action"))
            }
            UiCommand::Quit => {
                self.pending_action = Some(PendingAction::Quit);
                Ok(path!("pending_action"))
            }
            UiCommand::OpenSelected => {
                self.pending_action = Some(PendingAction::OpenSelected);
                Ok(path!("pending_action"))
            }
            UiCommand::ArchiveSelected => {
                self.pending_action = Some(PendingAction::ArchiveSelected);
                Ok(path!("pending_action"))
            }
            UiCommand::ClearPendingAction => {
                self.pending_action = None;
                Ok(path!("pending_action"))
            }
            UiCommand::SearchInsertChar { char: ch } => {
                self.search_live_query.push(ch);
                Ok(path!("search_live_query"))
            }
            UiCommand::SearchDeleteChar => {
                self.search_live_query.pop();
                Ok(path!("search_live_query"))
            }
            UiCommand::SearchClear => {
                self.search_live_query.clear();
                Ok(path!("search_live_query"))
            }
            UiCommand::SearchSaveChip => {
                let trimmed = self.search_live_query.trim().to_string();
                if !trimmed.is_empty() {
                    self.search_chips.push(trimmed);
                }
                self.search_live_query.clear();
                Ok(path!("search_chips"))
            }
            UiCommand::SearchDismissChip { index } => {
                if index < self.search_chips.len() {
                    self.search_chips.remove(index);
                }
                Ok(path!("search_chips"))
            }
        }
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-ui`
Expected: all tests pass — both old string-path tests and new typed command tests.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-ui/src/ui_store.rs
git commit -m "feat(ox-ui): UiStore Writer accepts typed UiCommand via from_value"
```

---

### Task 10: TurnState uses `ox-types` sub-types, gets serde derives

TurnState stays in ox-history (it has StructFS read/write behavior). Its sub-types (`ToolStatus`, `TokenUsage`) come from ox-types. The manual `BTreeMap` construction in `read`/`write` is replaced with `to_value`/`from_value`.

**Files:**
- Modify: `crates/ox-history/Cargo.toml` (add ox-types, serde deps)
- Modify: `crates/ox-history/src/turn.rs`
- Modify: `crates/ox-history/src/lib.rs`

- [ ] **Step 1: Add ox-types and serde dependencies**

In `crates/ox-history/Cargo.toml`, add to `[dependencies]`:

```toml
ox-types = { path = "../ox-types" }
serde = { version = "1", features = ["derive"] }
```

- [ ] **Step 2: Update TurnState to use ox-types sub-types and serde**

In `crates/ox-history/src/turn.rs`:

1. Add serde derive to `TurnState`:
   ```rust
   use serde::{Serialize, Deserialize};
   use ox_types::{ToolStatus, TokenUsage};
   ```

2. Replace the struct definition — change `tool: Option<(String, String)>` to `tool: Option<ToolStatus>` and `tokens: (u32, u32)` to `tokens: TokenUsage`:

   ```rust
   #[derive(Debug, Default, Clone, Serialize, Deserialize)]
   pub struct TurnState {
       pub streaming: String,
       pub thinking: bool,
       pub tool: Option<ToolStatus>,
       pub tokens: TokenUsage,
   }
   ```

3. Update `clear()` to use `TokenUsage::default()`:
   ```rust
   pub fn clear(&mut self) {
       self.streaming.clear();
       self.thinking = false;
       self.tool = None;
       self.tokens = TokenUsage::default();
   }
   ```

4. Replace the `read` method's manual `Value::Map` construction with `to_value`:
   ```rust
   pub fn read(&self, sub_path: &str) -> Option<Value> {
       match sub_path {
           "streaming" => Some(Value::String(self.streaming.clone())),
           "thinking" => Some(Value::Bool(self.thinking)),
           "tool" => Some(match &self.tool {
               Some(ts) => structfs_serde_store::to_value(ts).unwrap_or(Value::Null),
               None => Value::Null,
           }),
           "tokens" => structfs_serde_store::to_value(&self.tokens).ok(),
           _ => None,
       }
   }
   ```

5. Replace the `write` method's manual `Value::Map` destructuring with `from_value`:
   ```rust
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
           "tool" => match value {
               Value::Null => {
                   self.tool = None;
                   true
               }
               _ => match structfs_serde_store::from_value::<ToolStatus>(value.clone()) {
                   Ok(ts) => {
                       self.tool = Some(ts);
                       true
                   }
                   Err(_) => false,
               },
           },
           "tokens" => match structfs_serde_store::from_value::<TokenUsage>(value.clone()) {
               Ok(tu) => {
                   self.tokens = tu;
                   true
               }
               Err(_) => false,
           },
           _ => false,
       }
   }
   ```

- [ ] **Step 3: Update lib.rs exports**

In `crates/ox-history/src/lib.rs`, add re-exports for the sub-types:

```rust
mod turn;
pub use turn::TurnState;
pub use ox_types::{TokenUsage, ToolStatus};
```

- [ ] **Step 4: Run existing tests**

Run: `cargo test -p ox-history`
Expected: all existing tests pass. The `Value::Map` shapes produced by `to_value` match the previous manual construction (same field names, same value types).

- [ ] **Step 5: Run full workspace check**

Run: `cargo check`
Expected: compiles. Downstream crates that use `ox_history::TurnState` still work — the struct is the same, just with serde derives and typed sub-fields.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-history/ 
git commit -m "refactor(ox-history): TurnState uses ox-types sub-types with serde-driven read/write"
```

---

### Task 11: ApprovalStore uses `ox-types::ApprovalRequest`

**Files:**
- Modify: `crates/ox-ui/src/approval_store.rs`
- Modify: `crates/ox-ui/src/lib.rs`

- [ ] **Step 1: Replace local ApprovalRequest with ox-types import**

In `crates/ox-ui/src/approval_store.rs`, remove the local `ApprovalRequest` struct (lines 13-17) and replace with:

```rust
use ox_types::ApprovalRequest;
```

- [ ] **Step 2: Update AsyncReader to use `to_value` for pending**

In the `AsyncReader` impl, find the `"pending"` read path and update it to use `to_value`:

```rust
            "pending" => {
                let value = match &self.pending {
                    Some(req) => structfs_serde_store::to_value(req)
                        .unwrap_or(Value::Null),
                    None => Value::Null,
                };
                Box::pin(async move { Ok(Some(Record::parsed(value))) })
            }
```

- [ ] **Step 3: Update AsyncWriter to use `from_value` for request**

In the `AsyncWriter` impl, find the `"request"` write path and update it to use `from_value`:

```rust
            "request" => {
                let req: ApprovalRequest = structfs_serde_store::from_value(value.clone())
                    .map_err(|e| StoreError::store("approval", "request", &e.to_string()))?;
                self.pending = Some(req);
                // ... rest stays the same (deferred channel setup)
            }
```

- [ ] **Step 4: Update lib.rs re-export**

In `crates/ox-ui/src/lib.rs`, add:

```rust
pub use ox_types::ApprovalRequest;
```

(This ensures anything importing `ox_ui::ApprovalRequest` still works.)

- [ ] **Step 5: Run tests**

Run: `cargo test -p ox-ui`
Expected: all approval_store tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-ui/src/
git commit -m "refactor(ox-ui): ApprovalStore uses ox_types::ApprovalRequest"
```

---

### Task 12: `fetch_view_state` uses `read_typed` and typed structs

**Files:**
- Modify: `crates/ox-cli/Cargo.toml` (add ox-types dep)
- Modify: `crates/ox-cli/src/view_state.rs`
- Modify: `crates/ox-cli/src/types.rs` (add ThreadData, ConfigSnapshot)

- [ ] **Step 1: Add ox-types dependency to ox-cli**

In `crates/ox-cli/Cargo.toml`, add to `[dependencies]`:

```toml
ox-types = { path = "../ox-types" }
```

- [ ] **Step 2: Update ViewState struct to use typed fields**

In `crates/ox-cli/src/view_state.rs`, replace the string-typed fields with ox-types enums. Change:

```rust
    pub screen: String,
    pub mode: String,
    // ...
    pub pending_action: Option<String>,
    // ...
    pub insert_context: Option<String>,
```

To:

```rust
    pub ui: ox_types::UiSnapshot,
```

And update `ThreadData` / remaining fields accordingly. The full new `ViewState`:

```rust
pub struct ViewState<'a> {
    // -- Broker-sourced (owned, typed) -----------------------------------
    pub ui: ox_types::UiSnapshot,

    /// Inbox threads (only populated on inbox screen).
    pub inbox_threads: Vec<InboxThread>,
    /// Messages for the active thread.
    pub messages: Vec<ChatMessage>,
    /// Turn state for the active thread.
    pub turn: ox_history::TurnState,
    /// Pending approval for the active thread.
    pub approval_pending: Option<ox_types::ApprovalRequest>,

    // -- Config ----------------------------------------------------------
    pub model: String,
    pub provider: String,

    // -- App-borrowed (references) ---------------------------------------
    pub input_history: &'a [String],
    pub approval_selected: usize,
    pub pending_customize: &'a Option<CustomizeState>,
    pub key_hints: Vec<(String, String)>,
    pub show_shortcuts: bool,
    pub editor_mode: crate::editor::EditorMode,
    pub editor_command_buffer: String,
}
```

- [ ] **Step 3: Rewrite `fetch_view_state` to use `read_typed`**

Replace the body of `fetch_view_state` with typed reads:

```rust
pub async fn fetch_view_state<'a>(
    client: &ClientHandle,
    app: &'a App,
    dialog: &'a crate::event_loop::DialogState,
    editor_mode: crate::editor::EditorMode,
    editor_command_buffer: &str,
) -> ViewState<'a> {
    // Read UiStore state as typed snapshot
    let ui: ox_types::UiSnapshot = client
        .read_typed(&oxpath!("ui"))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();

    // Conditional reads based on screen
    let mut inbox_threads = Vec::new();
    let mut messages = Vec::new();
    let mut turn = ox_history::TurnState::default();
    let mut approval_pending: Option<ox_types::ApprovalRequest> = None;

    match ui.screen {
        ox_types::Screen::Inbox => {
            if let Ok(Some(record)) = client.read(&oxpath!("inbox", "threads")).await {
                if let Some(val) = record.as_value() {
                    inbox_threads = parse_inbox_threads(val);
                }
            }
        }
        ox_types::Screen::Thread => {
            if let Some(tid) = &ui.active_thread {
                // Read committed messages
                let msg_path = ox_path::oxpath!("threads", tid, "history", "messages");
                if let Ok(Some(record)) = client.read(&msg_path).await {
                    if let Some(Value::Array(arr)) = record.as_value() {
                        messages = parse_chat_messages(arr);
                    }
                }

                // Read turn state fields
                let thinking_path = ox_path::oxpath!("threads", tid, "history", "turn", "thinking");
                if let Ok(Some(record)) = client.read(&thinking_path).await {
                    if let Some(Value::Bool(b)) = record.as_value() {
                        turn.thinking = *b;
                    }
                }

                let tool_path = ox_path::oxpath!("threads", tid, "history", "turn", "tool");
                if let Ok(Some(val)) = client.read_typed::<ox_types::ToolStatus>(&tool_path).await {
                    turn.tool = Some(val);
                }

                let tokens_path = ox_path::oxpath!("threads", tid, "history", "turn", "tokens");
                if let Ok(Some(val)) = client.read_typed::<ox_types::TokenUsage>(&tokens_path).await {
                    turn.tokens = val;
                }

                // Read approval
                let approval_path = ox_path::oxpath!("threads", tid, "approval", "pending");
                if let Ok(Some(val)) = client.read_typed::<ox_types::ApprovalRequest>(&approval_path).await {
                    approval_pending = Some(val);
                }
            }
        }
        ox_types::Screen::Settings => {}
    }

    // Read config
    let model = match client.read(&oxpath!("config", "gate", "defaults", "model")).await {
        Ok(Some(r)) => match r.as_value() {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    };
    let provider = match client.read(&oxpath!("config", "gate", "defaults", "account")).await {
        Ok(Some(r)) => match r.as_value() {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    };

    let mode_str = match ui.mode {
        ox_types::Mode::Normal => "normal",
        ox_types::Mode::Insert => "insert",
    };
    let screen_str = match ui.screen {
        ox_types::Screen::Inbox => "inbox",
        ox_types::Screen::Thread => "thread",
        ox_types::Screen::Settings => "settings",
    };
    let key_hints = read_key_hints(client, mode_str, screen_str).await;

    ViewState {
        ui,
        inbox_threads,
        messages,
        turn,
        approval_pending,
        model,
        provider,
        input_history: &app.input_history,
        approval_selected: dialog.approval_selected,
        pending_customize: &dialog.pending_customize,
        key_hints,
        show_shortcuts: dialog.show_shortcuts,
        editor_mode,
        editor_command_buffer: editor_command_buffer.to_string(),
    }
}
```

- [ ] **Step 4: Update all draw functions and event loop code that reads from ViewState**

This is a mechanical replacement throughout `crates/ox-cli/src/`. Every reference to `vs.screen`, `vs.mode`, `vs.pending_action`, `vs.insert_context`, `vs.active_thread`, etc. changes to read from `vs.ui.screen`, `vs.ui.mode`, `vs.ui.pending_action`, `vs.ui.insert_context`, `vs.ui.active_thread`, and comparisons change from string to enum:

- `vs.screen == "inbox"` → `vs.ui.screen == ox_types::Screen::Inbox`
- `vs.mode == "insert"` → `vs.ui.mode == ox_types::Mode::Insert`
- `vs.pending_action.as_deref() == Some("send_input")` → `vs.ui.pending_action == Some(ox_types::PendingAction::SendInput)`
- `vs.insert_context.as_deref() == Some("compose")` → `vs.ui.insert_context == Some(ox_types::InsertContext::Compose)`
- `vs.active_thread` stays the same (it's `Option<String>` in both)
- `vs.thinking` → `vs.turn.thinking`
- `vs.tool_status` → `vs.turn.tool.as_ref().map(|t| (t.name.clone(), t.status.clone()))`
- `vs.turn_tokens` → `(vs.turn.tokens.input_tokens, vs.turn.tokens.output_tokens)`
- `vs.approval_pending` stays the same type
- `vs.scroll` → `vs.ui.scroll as u16`
- `vs.scroll_max` → `vs.ui.scroll_max as u16`
- `vs.input` → `vs.ui.input.content.clone()`
- `vs.cursor` → `vs.ui.input.cursor`
- `vs.selected_row` → `vs.ui.selected_row`
- `vs.search_chips` → `vs.ui.search.chips`
- `vs.search_active` → `vs.ui.search.active`

Work through each file that references ViewState fields: `event_loop.rs`, `tui.rs`, `thread_view.rs`, `inbox_view.rs`, `tab_bar.rs`, `dialogs.rs`.

- [ ] **Step 5: Verify it compiles**

Run: `cargo check -p ox-cli`
Expected: compiles with no errors.

- [ ] **Step 6: Run full test suite**

Run: `cargo test -p ox-cli`
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/ox-cli/
git commit -m "refactor(ox-cli): fetch_view_state uses read_typed and typed ViewState"
```

---

### Task 13: Event loop uses typed enums for dispatch

**Files:**
- Modify: `crates/ox-cli/src/event_loop.rs`

This task is the mechanical consequence of Task 12 — the ViewState fields are now typed, so every string comparison in the event loop becomes an enum comparison.

- [ ] **Step 1: Update pending_action handling**

In the event loop's pending action section, change:

```rust
        if let Some(action) = &pending_action {
            match action.as_str() {
                "send_input" => { ... }
                "quit" => return Ok(()),
                "open_selected" => { ... }
                "archive_selected" => { ... }
                _ => {}
            }
```

To:

```rust
        if let Some(action) = &pending_action {
            match action {
                ox_types::PendingAction::SendInput => { ... }
                ox_types::PendingAction::Quit => return Ok(()),
                ox_types::PendingAction::OpenSelected => { ... }
                ox_types::PendingAction::ArchiveSelected => { ... }
            }
```

- [ ] **Step 2: Update mode/screen extraction**

The variables extracted from ViewState change type:

```rust
        let pending_action: Option<ox_types::PendingAction> = vs.ui.pending_action;
        let screen_owned: ox_types::Screen = vs.ui.screen;
        let mode_owned: ox_types::Mode = vs.ui.mode;
        let insert_context_owned: Option<ox_types::InsertContext> = vs.ui.insert_context;
```

- [ ] **Step 3: Update all string comparisons**

Throughout the event loop, replace:

- `screen_owned == "settings"` → `screen_owned == ox_types::Screen::Settings`
- `screen_owned == "inbox"` → `screen_owned == ox_types::Screen::Inbox`
- `mode_owned == "insert"` → `mode_owned == ox_types::Mode::Insert`
- `mode_owned == "normal"` → `mode_owned == ox_types::Mode::Normal`
- `mode == "insert"` → `mode == ox_types::Mode::Insert` (local variable)
- `mode == "normal"` → `mode == ox_types::Mode::Normal`
- `insert_context_owned.as_deref() == Some("compose")` → `insert_context_owned == Some(ox_types::InsertContext::Compose)`
- `insert_context_owned.as_deref() == Some("reply")` → `insert_context_owned == Some(ox_types::InsertContext::Reply)`
- `insert_context_owned.as_deref() == Some("search")` → `insert_context_owned == Some(ox_types::InsertContext::Search)`
- `insert_context_owned.as_deref() == Some("command")` → `insert_context_owned == Some(ox_types::InsertContext::Command)`

- [ ] **Step 4: Update InputStore dispatch**

The `input/key` write currently passes mode and screen as strings. This still needs to work with the InputStore's string-based binding resolution. Convert the enums to strings for this specific call:

```rust
        let mode_str = match mode_owned {
            ox_types::Mode::Normal => "normal",
            ox_types::Mode::Insert => "insert",
        };
        let screen_str = match screen_owned {
            ox_types::Screen::Inbox => "inbox",
            ox_types::Screen::Thread => "thread",
            ox_types::Screen::Settings => "settings",
        };

        let result = client
            .write(
                &oxpath!("input", "key"),
                cmd!("mode" => mode_str, "key" => key_str.clone(), "screen" => screen_str),
            )
            .await;
```

(InputStore binding resolution is a future task to type — this keeps it working for now.)

- [ ] **Step 5: Verify it compiles and tests pass**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: compiles and all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/event_loop.rs
git commit -m "refactor(ox-cli): event loop uses typed enums instead of string comparisons"
```

---

### Task 14: Call sites use `write_typed` + `UiCommand`

**Files:**
- Modify: `crates/ox-cli/src/event_loop.rs`
- Modify: `crates/ox-cli/src/editor.rs`
- Modify: `crates/ox-cli/src/key_handlers.rs`
- Modify: `crates/ox-cli/src/bindings.rs`

- [ ] **Step 1: Replace `cmd!` writes to `ui/` paths with `write_typed` to `ui`**

Search all files in `crates/ox-cli/src/` for `client.write(&oxpath!("ui",` patterns and replace with typed writes. Examples:

```rust
// Before
client.write(&oxpath!("ui", "scroll_up"), cmd!()).await;
// After
client.write_typed(&oxpath!("ui"), &UiCommand::ScrollUp).await;

// Before
client.write(&oxpath!("ui", "open"), cmd!("thread_id" => tid)).await;
// After
client.write_typed(&oxpath!("ui"), &UiCommand::Open { thread_id: tid.to_string() }).await;

// Before
client.write(&oxpath!("ui", "enter_insert"), cmd!("context" => "compose")).await;
// After
client.write_typed(&oxpath!("ui"), &UiCommand::EnterInsert { context: InsertContext::Compose }).await;

// Before
client.write(&oxpath!("ui", "set_row_count"), cmd!("count" => row_count)).await;
// After
client.write_typed(&oxpath!("ui"), &UiCommand::SetRowCount { count: row_count as usize }).await;

// Before
client.write(&oxpath!("ui", "clear_pending_action"), cmd!()).await;
// After
client.write_typed(&oxpath!("ui"), &UiCommand::ClearPendingAction).await;
```

Add `use ox_types::{UiCommand, InsertContext};` at the top of each modified file.

- [ ] **Step 2: Update bindings.rs action invocations**

In `crates/ox-cli/src/bindings.rs`, the binding table uses `Action::Invoke { command: String, args }`. These are dispatched through `InputStore` → `CommandStore` → broker, not directly through `write_typed`. Leave these as-is for now — they'll be migrated when InputStore is typed (future work).

- [ ] **Step 3: Verify it compiles and tests pass**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: compiles and all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-cli/src/
git commit -m "refactor(ox-cli): call sites use write_typed with UiCommand"
```

---

### Task 15: Agent worker call sites use `write_typed`

**Files:**
- Modify: `crates/ox-cli/src/agents.rs`

- [ ] **Step 1: Replace manual `BTreeMap` + `Value::Map` construction with `write_typed`**

In `crates/ox-cli/src/agents.rs`, find all places where agent workers write to `history/turn/tokens`, `history/turn/tool`, etc. using manually constructed `Value::Map`. Replace with `write_typed`.

Token usage write:

```rust
// Before
let mut tmap = BTreeMap::new();
tmap.insert("in".to_string(), Value::Integer(usage.input_tokens as i64));
tmap.insert("out".to_string(), Value::Integer(usage.output_tokens as i64));
self.rt_handle
    .block_on(self.scoped_client.write(
        &path!("history/turn/tokens"),
        Record::parsed(Value::Map(tmap)),
    ))
    .ok();

// After
self.rt_handle
    .block_on(self.scoped_client.write_typed(
        &oxpath!("history", "turn", "tokens"),
        &ox_types::TokenUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
        },
    ))
    .ok();
```

Tool status writes (search for `turn/tool` writes):

```rust
// Before (manually constructed map with "name" and "status" keys)
// After
adapter.write_typed(
    &oxpath!("history", "turn", "tool"),
    &ox_types::ToolStatus {
        name: tool_name,
        status: status_str,
    },
).ok();
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p ox-cli`
Expected: compiles with no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/ox-cli/src/agents.rs
git commit -m "refactor(ox-cli): agent workers use write_typed for turn state"
```

---

### Task 16: Extract screen handlers from event loop

**Files:**
- Create: `crates/ox-cli/src/shell.rs`
- Create: `crates/ox-cli/src/inbox_shell.rs`
- Create: `crates/ox-cli/src/thread_shell.rs`
- Create: `crates/ox-cli/src/settings_shell.rs`
- Modify: `crates/ox-cli/src/event_loop.rs`
- Modify: `crates/ox-cli/src/main.rs` (add modules)

- [ ] **Step 1: Create the shell types and Outcome enum**

Write `crates/ox-cli/src/shell.rs`:

```rust
//! Shell types — platform-local state and dispatch for the TUI.

use crate::app::App;

/// What a screen handler returns.
pub(crate) enum Outcome {
    /// Key wasn't handled.
    Ignored,
    /// State was updated, redraw next frame.
    Handled,
    /// Quit the application.
    Quit,
    /// A cross-cutting action that needs App-level resources.
    Action(AppAction),
}

/// Cross-cutting actions that require App-level resources.
pub(crate) enum AppAction {
    Compose { text: String },
    Reply { thread_id: String, text: String },
    ArchiveThread { thread_id: String },
}

/// Platform-local rendering state for all screens.
pub(crate) struct ShellState {
    pub inbox: crate::inbox_shell::InboxShell,
    pub thread: crate::thread_shell::ThreadShell,
    pub settings: crate::settings_shell::SettingsShell,
}

impl ShellState {
    pub fn new() -> Self {
        Self {
            inbox: crate::inbox_shell::InboxShell::new(),
            thread: crate::thread_shell::ThreadShell::new(),
            settings: crate::settings_shell::SettingsShell::new(),
        }
    }
}
```

- [ ] **Step 2: Create InboxShell**

Write `crates/ox-cli/src/inbox_shell.rs`:

Extract inbox-specific key handling from event_loop.rs — the search chip dismissal, inbox navigation that's currently guarded by `screen == "inbox"`. Each handler method takes `&mut self`, the typed `UiSnapshot`, and the broker client, and returns `Outcome`.

```rust
//! InboxShell — TUI-local state and key handling for the inbox screen.

use ox_broker::ClientHandle;
use ox_types::UiSnapshot;

use crate::shell::Outcome;

pub(crate) struct InboxShell;

impl InboxShell {
    pub fn new() -> Self {
        Self
    }

    pub async fn handle_key(
        &mut self,
        key: &str,
        ui: &UiSnapshot,
        client: &ClientHandle,
    ) -> Outcome {
        // Search chip dismissal, inbox-specific navigation
        // Moved from event_loop.rs inbox-guarded sections
        Outcome::Ignored
    }
}
```

(The actual body is extracted from event_loop.rs — the steps below fill it in.)

- [ ] **Step 3: Create ThreadShell**

Write `crates/ox-cli/src/thread_shell.rs`:

Extract thread-specific key handling — editor dispatch, approval dialog, thread navigation.

```rust
//! ThreadShell — TUI-local state and key handling for the thread screen.

use ox_broker::ClientHandle;
use ox_types::UiSnapshot;

use crate::app::App;
use crate::editor::InputSession;
use crate::shell::Outcome;
use crate::text_input_view::TextInputView;

pub(crate) struct ThreadShell {
    pub input_session: InputSession,
    pub text_input_view: TextInputView,
}

impl ThreadShell {
    pub fn new() -> Self {
        Self {
            input_session: InputSession::new(),
            text_input_view: TextInputView::new(),
        }
    }

    pub async fn handle_key(
        &mut self,
        key: &str,
        ui: &UiSnapshot,
        app: &mut App,
        client: &ClientHandle,
        terminal_width: u16,
    ) -> Outcome {
        // Editor sub-mode dispatch, approval handling
        // Moved from event_loop.rs thread-guarded sections
        Outcome::Ignored
    }
}
```

- [ ] **Step 4: Create SettingsShell**

Write `crates/ox-cli/src/settings_shell.rs`:

Extract the ~500 lines of settings key handling from event_loop.rs.

```rust
//! SettingsShell — TUI-local state and key handling for the settings screen.

use ox_broker::ClientHandle;
use ox_types::UiSnapshot;

use crate::app::App;
use crate::settings_state::SettingsState;
use crate::shell::Outcome;

pub(crate) struct SettingsShell {
    pub state: SettingsState,
}

impl SettingsShell {
    pub fn new() -> Self {
        Self {
            state: SettingsState::new(),
        }
    }

    pub fn new_wizard() -> Self {
        Self {
            state: SettingsState::new_wizard(),
        }
    }

    pub async fn handle_key(
        &mut self,
        key: &str,
        ui: &UiSnapshot,
        app: &mut App,
        client: &ClientHandle,
    ) -> Outcome {
        // All settings key handling moved from event_loop.rs
        Outcome::Ignored
    }
}
```

- [ ] **Step 5: Move key handling code from event_loop.rs into screen handlers**

This is the largest mechanical step. For each screen handler, cut the relevant sections from event_loop.rs and paste into the handler's `handle_key` method:

- **SettingsShell**: the `screen == "settings"` blocks (~lines 287-1016 of event_loop.rs)
- **ThreadShell**: the editor dispatch, ESC interception, approval dialog (~lines 1038-1113)
- **InboxShell**: search chip dismissal (~lines 1018-1030)

Update return types: where event_loop used `continue`, return `Outcome::Handled`. Where it wrote to broker and continued, return `Outcome::Handled`. Where it needed `app.do_compose`, return `Outcome::Action(AppAction::Compose { ... })`.

- [ ] **Step 6: Rewrite event loop as router**

Replace the key dispatch section of event_loop.rs with:

```rust
                    let outcome = match screen_owned {
                        ox_types::Screen::Settings => {
                            shell.settings.handle_key(&key_str, &ui, app, client).await
                        }
                        ox_types::Screen::Inbox => {
                            shell.inbox.handle_key(&key_str, &ui, client).await
                        }
                        ox_types::Screen::Thread => {
                            shell.thread.handle_key(
                                &key_str, &ui, app, client, terminal_width,
                            ).await
                        }
                    };

                    match outcome {
                        Outcome::Quit => return Ok(()),
                        Outcome::Action(action) => {
                            execute_app_action(app, client, action).await;
                        }
                        _ => {}
                    }
```

- [ ] **Step 7: Add module declarations to main.rs**

In `crates/ox-cli/src/main.rs`, add:

```rust
mod inbox_shell;
mod settings_shell;
mod shell;
mod thread_shell;
```

- [ ] **Step 8: Verify it compiles and tests pass**

Run: `cargo check -p ox-cli && cargo test -p ox-cli`
Expected: compiles and all tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/ox-cli/src/
git commit -m "refactor(ox-cli): extract screen handlers from event loop into shell modules"
```

---

### Task 17: Run quality gates

**Files:** None — verification only.

- [ ] **Step 1: Run the full quality gate script**

Run: `./scripts/quality_gates.sh`
Expected: all 14 gates pass.

- [ ] **Step 2: If any gate fails, fix and commit the fix**

Address any formatting (gate 1-2), clippy (gate 3), check (gate 4), test (gate 5-6), or build (gate 7-10) failures.

- [ ] **Step 3: Final commit if needed**

```bash
git add -A
git commit -m "fix: address quality gate issues from typed StructFS boundary migration"
```

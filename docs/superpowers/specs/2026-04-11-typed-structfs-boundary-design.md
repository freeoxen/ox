# Typed StructFS Boundary Design

**Date:** 2026-04-11
**Status:** Approved
**Scope:** ox-types crate, UiStore typed protocol, TurnState typed protocol, event loop restructure

## Problem

The StructFS boundary between stores and consumers destroys type information unnecessarily. UiStore has `Screen`, `Mode`, `InsertContext` enums internally but serializes them to `Value::String`, and consumers manually destructure `Value::Map` with string key lookups. The result:

- `fetch_view_state` is ~370 lines of manual `Value::Map` picking
- `event_loop.rs` is 1,295 lines matching on strings (`mode == "insert"`, `screen == "settings"`)
- `TurnState::read`/`write` manually construct `BTreeMap` + `Value::Map` for structured data
- Command arguments pass through as `cmd!("context" => "compose")` — stringly typed
- Adding a new screen, mode, or command has no compiler enforcement

The infrastructure for typed round-trips (`structfs_serde_store::to_value`/`from_value` with standard serde derives) already exists but is not used for UI state.

## Architecture Principle

**Types survive the StructFS boundary.** Every struct that crosses the boundary is a `#[derive(Serialize, Deserialize)]` type that gets `to_value`'d on the write side and `from_value`'d on the read side. No manual `Value::Map` destructuring. No string matching. The types go in, the types come out.

**The shared core is cross-platform.** ox targets TUI (`ox-cli`), web (`ox-web`), and eventually mobile. The UI state machine in `ox-ui` is the shared core — the Elm Architecture's `Model` and `update`. Each platform provides the `view` function. The StructFS boundary is the message bus between the platform-agnostic core and platform-specific shells.

```
ox-types          Pure data: Screen, Mode, UiCommand, TurnState, etc.
ox-ui             Shared core: UiStore, InputStore, ApprovalStore
                  Platform-agnostic behavior. No rendering.

ox-cli            TUI shell: ratatui rendering, crossterm events
ox-web            Web shell: Svelte rendering, DOM events
```

## Design

### 1. `ox-types` Crate

A new leaf crate containing all shared types that cross the StructFS boundary. Pure data — no behavior, no Store impls.

**Dependencies:** `serde` only. Leaf of the dependency tree — every crate can depend on it.

**Types:**

```rust
// -- UI state enums --

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Screen { Inbox, Thread, Settings }
// Default: Screen::Inbox

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode { Normal, Insert }
// Default: Mode::Normal

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertContext { Compose, Reply, Search, Command }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingAction { SendInput, Quit, OpenSelected, ArchiveSelected }

// -- UI snapshots (read contract) --

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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InputSnapshot {
    pub content: String,
    pub cursor: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchSnapshot {
    pub chips: Vec<String>,
    pub live_query: String,
    pub active: bool,
}

// -- UI commands (write contract) --

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

// -- Turn state (agent-produced) --

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnState {
    pub streaming: String,
    pub thinking: bool,
    pub tool: Option<ToolStatus>,
    pub tokens: TokenUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStatus {
    pub name: String,
    pub status: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

// -- Approval --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub tool_name: String,
    pub input_preview: String,
}
```

### 2. Typed Broker API

`ClientHandle` and `SyncClientAdapter` gain typed read/write methods. The serialization boundary becomes invisible infrastructure.

```rust
// On ClientHandle
async fn write_typed<T: Serialize>(&self, to: &Path, value: &T) -> Result<Path, StoreError> {
    let v = structfs_serde_store::to_value(value)?;
    self.write(to, Record::parsed(v)).await
}

async fn read_typed<T: DeserializeOwned>(&self, from: &Path) -> Result<Option<T>, StoreError> {
    match self.read(from).await? {
        Some(record) => {
            let value = record.into_value()?;
            Ok(Some(structfs_serde_store::from_value(value)?))
        }
        None => Ok(None),
    }
}

// Same for SyncClientAdapter
fn write_typed<T: Serialize>(&mut self, to: &Path, value: &T) -> Result<Path, StoreError> { ... }
fn read_typed<T: DeserializeOwned>(&mut self, from: &Path) -> Result<Option<T>, StoreError> { ... }
```

Call sites go from:

```rust
// Before
client.write(&oxpath!("ui", "enter_insert"), cmd!("context" => "compose")).await;

// After
client.write_typed(&oxpath!("ui"), &UiCommand::EnterInsert {
    context: InsertContext::Compose,
}).await;
```

Read sites go from:

```rust
// Before: ~100 lines of manual Value::Map destructuring
let ui_state = match client.read(&path!("ui")).await { ... };
let screen = match ui_state.get("screen") { Some(Value::String(s)) => s.clone(), ... };

// After
let ui: UiSnapshot = client.read_typed(&oxpath!("ui")).await?.unwrap_or_default();
// ui.screen is Screen::Inbox, not "inbox"
```

### 3. UiStore Protocol Changes

**Reader:** `all_fields_map()` (50 lines of manual `BTreeMap` construction) becomes `to_value(&self.snapshot())` where `snapshot()` returns a `UiSnapshot` struct literal. Individual field reads stay for targeted reads but use `to_value(&self.screen)` instead of hand-building `Value::String`.

**Writer:** The existing `Command::parse` step stays — it extracts the txn field for dedup and yields the inner value. Then `from_value::<UiCommand>` deserializes that inner value instead of matching on a string command name. The match becomes exhaustive on `UiCommand` variants.

**Internal fields change:**
- `pending_action: Option<String>` → `Option<PendingAction>`
- Enums imported from `ox-types` instead of locally defined

### 4. TurnState Protocol Changes

`ToolStatus` and `TokenUsage` move to `ox-types` (pure data sub-types). `TurnState` stays in `ox-history` (it has StructFS read/write behavior) but gains serde derives and imports its sub-types from `ox-types`.

`TurnState::read` replaces manual `Value::Map` construction with `to_value`. `TurnState::write` replaces manual destructuring with `from_value`. The 120 lines of manual map building/parsing collapse to serde calls.

Field changes:
- `tool: Option<(String, String)>` → `Option<ToolStatus>`
- `tokens: (u32, u32)` → `TokenUsage`

Agent worker writes in `agents.rs` switch from manual `BTreeMap` construction to `write_typed`:

```rust
// Before
let mut tmap = BTreeMap::new();
tmap.insert("in".to_string(), Value::Integer(usage.input_tokens as i64));
tmap.insert("out".to_string(), Value::Integer(usage.output_tokens as i64));
adapter.write(&oxpath!("history", "turn", "tokens"), Record::parsed(Value::Map(tmap))).ok();

// After
adapter.write_typed(&oxpath!("history", "turn", "tokens"), &TokenUsage {
    input_tokens: usage.input_tokens,
    output_tokens: usage.output_tokens,
}).ok();
```

### 5. ApprovalStore Protocol Changes

`ApprovalRequest` moves from `ox-ui` to `ox-types` with serde derives. `ApprovalStore`'s `AsyncReader`/`AsyncWriter` use `to_value`/`from_value` instead of manual `Value::Map` construction.

### 6. `ViewState` Restructure

`ViewState` fields change from strings to typed enums:

```rust
pub struct ViewState<'a> {
    pub ui: UiSnapshot,                    // typed, from read_typed
    pub thread_data: Option<ThreadData>,   // typed sub-structs
    pub inbox_threads: Vec<InboxThread>,
    pub shell: &'a ShellState,             // platform-local borrows
    pub config: ConfigSnapshot,
    pub key_hints: Vec<KeyHint>,
}

pub struct ThreadData {
    pub messages: Vec<ChatMessage>,
    pub turn: TurnState,
    pub approval: Option<ApprovalRequest>,
}
```

`fetch_view_state` shrinks from ~370 lines to ~60. The `match ui.screen` for conditional reads uses enum matching instead of string comparison.

### 7. Event Loop Restructure

Each screen gets its own handler. The event loop becomes a router.

**Shell state** — platform-local rendering state, keyed by screen:

```rust
struct ShellState {
    inbox: InboxShell,
    thread: ThreadShell,
    settings: SettingsShell,
}
```

**Outcome enum** — what a screen handler returns:

```rust
enum Outcome {
    Ignored,
    Handled,
    Quit,
    Action(AppAction),
}

enum AppAction {
    Compose { text: String },
    Reply { thread_id: String, text: String },
    ArchiveThread { thread_id: String },
}
```

Screen transitions are `UiCommand` writes to the broker (the shared core owns the state machine), not local enum swaps. Next frame, `ui.screen` reflects the transition.

**The event loop** becomes ~50 lines of routing:

```rust
let ui: UiSnapshot = client.read_typed(&oxpath!("ui")).await?.unwrap_or_default();

// ... draw ...

match ui.screen {
    Screen::Inbox => shell.inbox.handle_key(key, &ui, client).await,
    Screen::Thread => shell.thread.handle_key(key, &ui, client).await,
    Screen::Settings => shell.settings.handle_key(key, &ui, client).await,
}
```

**Where the 1,295 lines go:**
- Settings key handling (~500 lines) → `SettingsShell::handle_key`
- Thread + editor + approval (~300 lines) → `ThreadShell::handle_key`
- Inbox + search (~80 lines) → `InboxShell::handle_key`
- Mouse handling (~100 lines) → split across shell handlers
- Event loop router (~50 lines) → stays

## Migration Path

Each step compiles and runs independently.

1. **Create `ox-types` crate** with all shared types, serde derives. Add to workspace.
2. **Add `write_typed`/`read_typed`** to `ClientHandle` and `SyncClientAdapter`. New methods, no breakage.
3. **UiStore uses `ox-types`** — import enums, `UiSnapshot` for Reader, `UiCommand` for Writer. Remove local enum definitions.
4. **TurnState uses `ox-types`** — import types, replace manual `Value::Map` read/write with serde.
5. **ApprovalStore uses `ox-types`** — import `ApprovalRequest`, add serde to read/write paths.
6. **`fetch_view_state` uses `read_typed`** — replace manual destructuring. `ViewState` fields become typed.
7. **Event loop uses typed enums** — mechanical replacement of string comparisons with enum matches.
8. **Call sites use `write_typed` + `UiCommand`** — replace `cmd!` macro usage for UI commands.
9. **Extract screen handlers** — settings, thread, inbox key handling into separate modules.
10. **Agent worker call sites use `write_typed`** — replace manual `BTreeMap` construction.

## Testing

- Existing `TurnState` tests continue to pass — same `Value` shapes, serde-driven internally.
- Existing editor snapshot tests unaffected — rendering layer unchanged.
- UiStore tests updated to use typed commands instead of string commands.
- New unit tests for `UiCommand` serde round-trip (each variant serializes and deserializes correctly).
- New unit tests for `UiSnapshot` serde round-trip.
- Existing integration behavior unchanged — the wire format is the same (serde produces identical `Value::Map` shapes).

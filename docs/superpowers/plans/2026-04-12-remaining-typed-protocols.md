# Remaining Typed Protocols Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate all raw `Value::Map`/`Value::String`/`Value::Null` construction at StructFS store boundaries — every write and read uses typed structs with `write_typed`/`read_typed` or `to_value`/`from_value`.

**Architecture:** Define small Serialize/Deserialize structs in `ox-types` for each protocol (inbox commands, config commands, history commands, key hints, set-input). Call sites switch from manual `BTreeMap`/`Value` construction to typed struct construction + `write_typed`. Stores that consume `Value::Map` continue to work unchanged — serde produces the same maps.

**Tech Stack:** Rust, serde, structfs-core-store, structfs-serde-store, ox-types, ox-broker

---

## File Structure

| File | Responsibility |
|------|----------------|
| `crates/ox-types/src/inbox.rs` (create) | `CreateThread`, `UpdateThread`, `ArchiveThread` structs |
| `crates/ox-types/src/config.rs` (create) | `ConfigSignal` enum (Save/Delete), typed config reads |
| `crates/ox-types/src/history.rs` (create) | `HistoryAppend`, `StreamingDelta`, `TurnClear` |
| `crates/ox-types/src/editor.rs` (create) | `SetInput` struct |
| `crates/ox-types/src/key_hint.rs` (create) | `KeyHint` struct for binding reads |
| `crates/ox-types/src/lib.rs` (modify) | Re-export new modules |
| `crates/ox-cli/src/policy_check.rs` (modify) | Use `write_typed` with `ApprovalRequest` |
| `crates/ox-cli/src/view_state.rs` (modify) | Use `read_typed::<String>`, `read_typed::<Vec<KeyHint>>` |
| `crates/ox-cli/src/agents.rs` (modify) | Use typed inbox/history/tool-schema writes |
| `crates/ox-cli/src/app.rs` (modify) | Use typed `UpdateThread` |
| `crates/ox-cli/src/event_loop.rs` (modify) | Use typed `ArchiveThread` |
| `crates/ox-cli/src/settings_shell.rs` (modify) | Use typed `ConfigSignal` for save/delete |
| `crates/ox-cli/src/editor.rs` (modify) | Use `write_typed` for `SetInput` and `EditSequence` |
| `crates/ox-cli/src/broker_setup.rs` (modify) | Use `write_typed` with `InputKeyEvent` |

---

### Task 1: Approval request writes (policy_check.rs)

The `ApprovalRequest` type already exists in `ox-types`. The call site manually builds a `BTreeMap` instead of using it.

**Files:**
- Modify: `crates/ox-cli/src/policy_check.rs:62-77`

- [ ] **Step 1: Replace manual BTreeMap with `write_typed` using `ApprovalRequest`**

In `crates/ox-cli/src/policy_check.rs`, replace the `handle_ask` method's request construction (lines 63-77):

```rust
// BEFORE (lines 63-77):
let mut request = BTreeMap::new();
request.insert("tool_name".to_string(), Value::String(tool.to_string()));
request.insert(
    "input_preview".to_string(),
    Value::String(input_preview.to_string()),
);

let approval_client = self.scoped_client.with_timeout(std::time::Duration::MAX);
let result = self.rt_handle.block_on(approval_client.write(
    &structfs_core_store::path!("approval/request"),
    Record::parsed(Value::Map(request)),
));

// AFTER:
let req = ox_types::ApprovalRequest {
    tool_name: tool.to_string(),
    input_preview: input_preview.to_string(),
};

let approval_client = self.scoped_client.with_timeout(std::time::Duration::MAX);
let result = self.rt_handle.block_on(approval_client.write_typed(
    &structfs_core_store::path!("approval/request"),
    &req,
));
```

Remove unused imports: `BTreeMap` (if no longer needed), `Value` (if no longer needed), `Record` (if no longer needed). Keep only what's still used — check the `record_to_json` method which uses `Record`.

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p ox-cli 2>&1 | head -30`
Expected: no errors related to policy_check.rs

- [ ] **Step 3: Run existing tests**

Run: `cargo test -p ox-cli 2>&1 | tail -20`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/ox-cli/src/policy_check.rs
git commit -m "refactor: use typed ApprovalRequest in policy_check write"
```

---

### Task 2: Config reads — model/provider (view_state.rs)

Replace manual `Value::String` destructuring with `read_typed::<String>`.

**Files:**
- Modify: `crates/ox-cli/src/view_state.rs:120-134`

- [ ] **Step 1: Replace manual destructuring with `read_typed::<String>`**

In `crates/ox-cli/src/view_state.rs`, replace lines 120-134:

```rust
// BEFORE:
let model = match client.read(&path!("config/gate/defaults/model")).await {
    Ok(Some(r)) => match r.as_value() {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    },
    _ => String::new(),
};
let provider = match client.read(&path!("config/gate/defaults/account")).await {
    Ok(Some(r)) => match r.as_value() {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    },
    _ => String::new(),
};

// AFTER:
let model = client
    .read_typed::<String>(&path!("config/gate/defaults/model"))
    .await
    .ok()
    .flatten()
    .unwrap_or_default();
let provider = client
    .read_typed::<String>(&path!("config/gate/defaults/account"))
    .await
    .ok()
    .flatten()
    .unwrap_or_default();
```

Remove `Value` from imports if no longer needed in the function scope (but check `read_key_hints` below which still uses it).

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p ox-cli 2>&1 | head -30`
Expected: no errors

- [ ] **Step 3: Commit**

```bash
git add crates/ox-cli/src/view_state.rs
git commit -m "refactor: use read_typed for config string reads in view_state"
```

---

### Task 3: Key hints read — define `KeyHint` type (ox-types + view_state.rs)

Replace manual `Value::Array`/`Value::Map` destructuring for key bindings with a typed struct.

**Files:**
- Create: `crates/ox-types/src/key_hint.rs`
- Modify: `crates/ox-types/src/lib.rs`
- Modify: `crates/ox-cli/src/view_state.rs:176-208`

- [ ] **Step 1: Define `KeyHint` in ox-types**

Create `crates/ox-types/src/key_hint.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyHint {
    pub key: String,
    pub description: String,
}
```

- [ ] **Step 2: Re-export from ox-types lib.rs**

Add to `crates/ox-types/src/lib.rs`:

```rust
pub mod key_hint;
// ...
pub use key_hint::*;
```

- [ ] **Step 3: Verify ox-types compiles**

Run: `cargo check -p ox-types 2>&1 | head -20`
Expected: no errors

- [ ] **Step 4: Replace manual key_hints destructuring in view_state.rs**

Replace the `read_key_hints` function (lines 176-208):

```rust
async fn read_key_hints(client: &ClientHandle, mode: &str, screen: &str) -> Vec<(String, String)> {
    let bindings_path =
        structfs_core_store::Path::parse(&format!("input/bindings/{mode}/{screen}"))
            .unwrap_or_else(|_| path!("input/bindings"));
    let hints: Vec<ox_types::KeyHint> = client
        .read_typed(&bindings_path)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();

    let mut result = Vec::new();
    let mut seen_keys = std::collections::HashSet::new();
    for hint in hints {
        if seen_keys.insert(hint.key.clone()) {
            result.push((hint.key, hint.description));
        }
    }
    result
}
```

Remove `Value` import from `view_state.rs` if it's now unused.

- [ ] **Step 5: Verify it compiles**

Run: `cargo check -p ox-cli 2>&1 | head -30`
Expected: no errors

- [ ] **Step 6: Run tests**

Run: `cargo test -p ox-cli 2>&1 | tail -20`
Expected: all tests pass

- [ ] **Step 7: Commit**

```bash
git add crates/ox-types/src/key_hint.rs crates/ox-types/src/lib.rs crates/ox-cli/src/view_state.rs
git commit -m "refactor: typed KeyHint for binding reads in view_state"
```

---

### Task 4: Tool schema writes (agents.rs)

Already uses `to_value` — just switch to `write_typed`.

**Files:**
- Modify: `crates/ox-cli/src/agents.rs:262-267`

- [ ] **Step 1: Replace `to_value` + `Record::parsed` with `write_typed`**

```rust
// BEFORE (lines 262-267):
if let Ok(val) = structfs_serde_store::to_value(&tool_store.tool_schemas_for_model()) {
    adapter
        .write(&path!("tools/schemas"), Record::parsed(val))
        .ok();
}

// AFTER:
adapter
    .write_typed(&path!("tools/schemas"), &tool_store.tool_schemas_for_model())
    .ok();
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p ox-cli 2>&1 | head -30`
Expected: no errors

- [ ] **Step 3: Commit**

```bash
git add crates/ox-cli/src/agents.rs
git commit -m "refactor: use write_typed for tool schema writes"
```

---

### Task 5: Inbox protocol types (ox-types + agents.rs + app.rs + event_loop.rs)

Define typed structs for thread creation, state updates, and archiving. These serialize to the same `Value::Map` that `ox-inbox`'s writer expects.

**Files:**
- Create: `crates/ox-types/src/inbox.rs`
- Modify: `crates/ox-types/src/lib.rs`
- Modify: `crates/ox-cli/src/agents.rs:123-133, 395-407`
- Modify: `crates/ox-cli/src/app.rs:135-150`
- Modify: `crates/ox-cli/src/event_loop.rs:187-201`

- [ ] **Step 1: Define inbox protocol types in ox-types**

Create `crates/ox-types/src/inbox.rs`:

```rust
use serde::{Deserialize, Serialize};

/// Write to `threads` to create a new thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateThread {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

/// Write to `threads/{id}` to update thread metadata.
/// All fields are optional — only provided fields are updated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateThread {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inbox_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}
```

- [ ] **Step 2: Re-export from ox-types lib.rs**

Add to `crates/ox-types/src/lib.rs`:

```rust
pub mod inbox;
// ...
pub use inbox::*;
```

- [ ] **Step 3: Verify ox-types compiles**

Run: `cargo check -p ox-types 2>&1 | head -20`
Expected: no errors

- [ ] **Step 4: Replace thread creation in agents.rs:123-133**

```rust
// BEFORE:
let mut map = std::collections::BTreeMap::new();
map.insert(
    "title".to_string(),
    structfs_core_store::Value::String(title.to_string()),
);
let path = self
    .inbox
    .write(&path!("threads"), Record::parsed(Value::Map(map)))
    .map_err(|e| e.to_string())?;

// AFTER:
let create = ox_types::CreateThread {
    title: title.to_string(),
    parent_id: None,
};
let val = structfs_serde_store::to_value(&create).map_err(|e| e.to_string())?;
let path = self
    .inbox
    .write(&path!("threads"), Record::parsed(val))
    .map_err(|e| e.to_string())?;
```

Note: We can't use `write_typed` here because `self.inbox` is an `InboxStore` (implements `Writer`), not a `ClientHandle`/`SyncClientAdapter`. The `write_typed` method is on `ClientHandle`/`SyncClientAdapter` only. So we use `to_value` + `Record::parsed`.

- [ ] **Step 5: Replace thread state update in agents.rs:395-407**

```rust
// BEFORE:
let mut update = BTreeMap::new();
update.insert("id".to_string(), Value::String(thread_id.clone()));
update.insert(
    "thread_state".to_string(),
    Value::String(new_state.to_string()),
);
update.insert("updated_at".to_string(), Value::Integer(now));
rt_handle
    .block_on(broker_client.write(
        &ox_path::oxpath!("inbox", "threads"),
        Record::parsed(Value::Map(update)),
    ))
    .ok();

// AFTER:
let update = ox_types::UpdateThread {
    id: Some(thread_id.clone()),
    thread_state: Some(new_state.to_string()),
    inbox_state: None,
    updated_at: Some(now),
};
rt_handle
    .block_on(broker_client.write_typed(
        &ox_path::oxpath!("inbox", "threads"),
        &update,
    ))
    .ok();
```

- [ ] **Step 6: Replace thread state update in app.rs:135-150**

```rust
// BEFORE:
pub fn update_thread_state(&mut self, thread_id: &str, state: &str) {
    let tid = thread_id.to_string();
    let update_path = ox_path::oxpath!("threads", tid);
    let mut map = std::collections::BTreeMap::new();
    map.insert(
        "thread_state".to_string(),
        structfs_core_store::Value::String(state.to_string()),
    );
    self.pool
        .inbox()
        .write(
            &update_path,
            structfs_core_store::Record::parsed(structfs_core_store::Value::Map(map)),
        )
        .ok();
}

// AFTER:
pub fn update_thread_state(&mut self, thread_id: &str, state: &str) {
    let tid = thread_id.to_string();
    let update_path = ox_path::oxpath!("threads", tid);
    let update = ox_types::UpdateThread {
        id: None,
        thread_state: Some(state.to_string()),
        inbox_state: None,
        updated_at: None,
    };
    let val = structfs_serde_store::to_value(&update).unwrap();
    self.pool
        .inbox()
        .write(&update_path, structfs_core_store::Record::parsed(val))
        .ok();
}
```

- [ ] **Step 7: Replace archive in event_loop.rs:187-201**

```rust
// BEFORE:
let mut map = std::collections::BTreeMap::new();
map.insert(
    "inbox_state".to_string(),
    structfs_core_store::Value::String("done".to_string()),
);
app.pool
    .inbox()
    .write(
        &update_path,
        structfs_core_store::Record::parsed(
            structfs_core_store::Value::Map(map),
        ),
    )
    .ok();

// AFTER:
let archive = ox_types::UpdateThread {
    id: None,
    thread_state: None,
    inbox_state: Some("done".to_string()),
    updated_at: None,
};
let val = structfs_serde_store::to_value(&archive).unwrap();
app.pool
    .inbox()
    .write(
        &update_path,
        structfs_core_store::Record::parsed(val),
    )
    .ok();
```

- [ ] **Step 8: Clean up unused imports**

Remove `BTreeMap` and `Value` imports from each modified file where they are no longer used.

- [ ] **Step 9: Verify it compiles**

Run: `cargo check -p ox-cli 2>&1 | head -30`
Expected: no errors

- [ ] **Step 10: Run tests**

Run: `cargo test -p ox-cli 2>&1 | tail -20`
Expected: all tests pass

- [ ] **Step 11: Commit**

```bash
git add crates/ox-types/src/inbox.rs crates/ox-types/src/lib.rs crates/ox-cli/src/agents.rs crates/ox-cli/src/app.rs crates/ox-cli/src/event_loop.rs
git commit -m "refactor: typed inbox protocol — CreateThread and UpdateThread"
```

---

### Task 6: Config signal writes (settings_shell.rs)

Replace `Record::parsed(Value::Null)` for save/delete signals with a typed enum.

**Files:**
- Create: `crates/ox-types/src/config.rs`
- Modify: `crates/ox-types/src/lib.rs`
- Modify: `crates/ox-cli/src/settings_shell.rs:317-320, 398-412, 429-432, 624-627`

- [ ] **Step 1: Define ConfigSignal in ox-types**

Create `crates/ox-types/src/config.rs`:

```rust
use serde::{Deserialize, Serialize};

/// Signal written to `config/save` or config field paths to trigger persistence or deletion.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "signal", rename_all = "snake_case")]
pub enum ConfigSignal {
    /// Persist current config to disk.
    Save,
    /// Delete the value at the written path.
    Delete,
}
```

- [ ] **Step 2: Re-export from ox-types lib.rs**

Add to `crates/ox-types/src/lib.rs`:

```rust
pub mod config;
// ...
pub use config::*;
```

- [ ] **Step 3: Verify ox-types compiles**

Run: `cargo check -p ox-types 2>&1 | head -20`
Expected: no errors

- [ ] **Step 4: Replace save signals in settings_shell.rs**

There are 4 instances of `Record::parsed(Value::Null)` for `config/save`. Replace each:

```rust
// BEFORE (lines 317-320, 429-432, 624-627, and one more):
client
    .write(&oxpath!("config", "save"), Record::parsed(Value::Null))
    .await
    .ok();

// AFTER:
client
    .write_typed(&oxpath!("config", "save"), &ox_types::ConfigSignal::Save)
    .await
    .ok();
```

- [ ] **Step 5: Replace delete signals in settings_shell.rs**

There are 3 instances of `Record::parsed(Value::Null)` for account field deletion (lines 398-412):

```rust
// BEFORE:
client
    .write(&provider_path, Record::parsed(Value::Null))
    .await
    .ok();
client
    .write(&ep_path, Record::parsed(Value::Null))
    .await
    .ok();
client
    .write(&key_path, Record::parsed(Value::Null))
    .await
    .ok();

// AFTER:
client
    .write_typed(&provider_path, &ox_types::ConfigSignal::Delete)
    .await
    .ok();
client
    .write_typed(&ep_path, &ox_types::ConfigSignal::Delete)
    .await
    .ok();
client
    .write_typed(&key_path, &ox_types::ConfigSignal::Delete)
    .await
    .ok();
```

**Important:** The ConfigStore's Writer impl must be updated to accept `ConfigSignal::Save` and `ConfigSignal::Delete` in addition to (or instead of) `Value::Null`. Check how the config store handles these writes before proceeding. If the config store matches on `Value::Null`, it will need to also match on the serialized `ConfigSignal` map form, or we need a different approach.

**Alternative if config store can't be changed:** If the ConfigStore explicitly checks for `Value::Null`, leave the save/delete signals as `Record::parsed(Value::Null)` for now and skip this task. Document it as a future change that requires coordinating config store and call sites.

- [ ] **Step 6: Verify it compiles**

Run: `cargo check -p ox-cli 2>&1 | head -30`
Expected: no errors. If ConfigStore rejects the new format, revert to `Value::Null` and skip.

- [ ] **Step 7: Run tests**

Run: `cargo test -p ox-cli 2>&1 | tail -20`
Expected: all tests pass

- [ ] **Step 8: Commit**

```bash
git add crates/ox-types/src/config.rs crates/ox-types/src/lib.rs crates/ox-cli/src/settings_shell.rs
git commit -m "refactor: typed ConfigSignal for config save/delete"
```

---

### Task 7: Config reads in agents.rs worker

Replace manual `Value::String` destructuring for gate config reads.

**Files:**
- Modify: `crates/ox-cli/src/agents.rs:270-299`

- [ ] **Step 1: Replace manual config reads with `read_typed::<String>`**

```rust
// BEFORE (lines 270-299):
let default_account = match adapter.read(&path!("gate/defaults/account")) {
    Ok(Some(r)) => match r.as_value() {
        Some(Value::String(s)) => s.clone(),
        _ => "anthropic".to_string(),
    },
    _ => "anthropic".to_string(),
};
let provider = match adapter.read(&ox_path::oxpath!(
    "gate", "accounts", default_account, "provider"
)) {
    Ok(Some(r)) => match r.as_value() {
        Some(Value::String(s)) => s.clone(),
        _ => "anthropic".to_string(),
    },
    _ => "anthropic".to_string(),
};
let api_key_for_transport = match adapter.read(&ox_path::oxpath!(
    "gate", "accounts", default_account, "key"
)) {
    Ok(Some(r)) => match r.as_value() {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    },
    _ => String::new(),
};

// AFTER:
let default_account = adapter
    .read_typed::<String>(&path!("gate/defaults/account"))
    .ok()
    .flatten()
    .unwrap_or_else(|| "anthropic".to_string());
let provider = adapter
    .read_typed::<String>(&ox_path::oxpath!(
        "gate", "accounts", default_account, "provider"
    ))
    .ok()
    .flatten()
    .unwrap_or_else(|| "anthropic".to_string());
let api_key_for_transport = adapter
    .read_typed::<String>(&ox_path::oxpath!(
        "gate", "accounts", default_account, "key"
    ))
    .ok()
    .flatten()
    .unwrap_or_default();
```

- [ ] **Step 2: Also replace config read in settings_shell.rs:282-291**

```rust
// BEFORE:
let current_default = client
    .read(&oxpath!("config", "gate", "defaults", "account"))
    .await
    .ok()
    .flatten()
    .and_then(|r| match r.as_value() {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    })
    .unwrap_or_default();

// AFTER:
let current_default = client
    .read_typed::<String>(&oxpath!("config", "gate", "defaults", "account"))
    .await
    .ok()
    .flatten()
    .unwrap_or_default();
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p ox-cli 2>&1 | head -30`
Expected: no errors

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-cli 2>&1 | tail -20`
Expected: all tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/agents.rs crates/ox-cli/src/settings_shell.rs
git commit -m "refactor: use read_typed for config string reads"
```

---

### Task 8: History protocol writes (agents.rs)

Type the history append, streaming delta, and turn clear writes.

**Files:**
- Modify: `crates/ox-cli/src/agents.rs:46-52, 338-341, 369-372, 377-379`

- [ ] **Step 1: Replace streaming delta write**

The streaming write passes `Value::String(text)` to `history/turn/streaming`. The `TurnState::write` method for "streaming" expects `Value::String`. Since `write_typed` would serialize a `String` to `Value::String`, this works directly:

```rust
// BEFORE (line 48-52):
handle
    .block_on(scoped.write(
        &path!("history/turn/streaming"),
        Record::parsed(Value::String(text.clone())),
    ))
    .ok();

// AFTER:
handle
    .block_on(scoped.write_typed(
        &path!("history/turn/streaming"),
        &text,
    ))
    .ok();
```

Note: `write_typed` on `ClientHandle` serializes `&String` → `Value::String(...)`, which is exactly what `TurnState::write("streaming", ...)` expects. This works.

- [ ] **Step 2: Replace history append writes**

The history append writes `json_to_value(user_json)` where `user_json` is a `serde_json::Value`. We can use `write_typed` with the `serde_json::Value` directly since `serde_json::Value` implements `Serialize`:

```rust
// BEFORE (line 338-342):
let user_json = serde_json::json!({"role": "user", "content": input});
if let Err(e) = adapter.write(
    &path!("history/append"),
    Record::parsed(json_to_value(user_json)),
) {

// AFTER:
let user_json = serde_json::json!({"role": "user", "content": input});
if let Err(e) = adapter.write_typed(
    &path!("history/append"),
    &user_json,
) {
```

Same for the error message append (lines 369-372):

```rust
// BEFORE:
let msg = serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": format!("error: {e}")}]});
adapter
    .write(&path!("history/append"), Record::parsed(json_to_value(msg)))
    .ok();

// AFTER:
let msg = serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": format!("error: {e}")}]});
adapter
    .write_typed(&path!("history/append"), &msg)
    .ok();
```

- [ ] **Step 3: Replace turn clear write**

```rust
// BEFORE (line 377-379):
adapter
    .write(&path!("history/turn/clear"), Record::parsed(Value::Null))
    .ok();

// AFTER — use a unit type or empty struct:
// Value::Null is the expected signal for clear. write_typed with () serializes
// to Value::Null via serde, which is what TurnState expects.
adapter
    .write_typed(&path!("history/turn/clear"), &())
    .ok();
```

Note: `serde` serializes `()` to `Value::Null` via `structfs_serde_store::to_value`. The TurnState handler for "clear" checks for `Value::Null`. Verify this works by checking what `to_value(&())` produces — if it doesn't produce `Value::Null`, use `Record::parsed(Value::Null)` (keep the original).

- [ ] **Step 4: Clean up unused imports**

Remove `json_to_value` from imports in `agents.rs` if no longer used. Remove `Value`, `Record`, `BTreeMap` imports if no longer needed (check all usages in the file first).

- [ ] **Step 5: Verify it compiles**

Run: `cargo check -p ox-cli 2>&1 | head -30`
Expected: no errors

- [ ] **Step 6: Run tests**

Run: `cargo test -p ox-cli 2>&1 | tail -20`
Expected: all tests pass

- [ ] **Step 7: Commit**

```bash
git add crates/ox-cli/src/agents.rs
git commit -m "refactor: typed history protocol writes — streaming, append, clear"
```

---

### Task 9: Editor writes — SetInput and EditSequence (editor.rs)

Type the `set_input` map construction and the `EditSequence` write.

**Files:**
- Create: `crates/ox-types/src/editor.rs`
- Modify: `crates/ox-types/src/lib.rs`
- Modify: `crates/ox-cli/src/editor.rs:176-211`

- [ ] **Step 1: Define SetInput in ox-types**

Create `crates/ox-types/src/editor.rs`:

```rust
use serde::{Deserialize, Serialize};

/// Payload for the `ui/set_input` write path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetInput {
    pub text: String,
    pub cursor: usize,
}
```

- [ ] **Step 2: Re-export from ox-types lib.rs**

Add to `crates/ox-types/src/lib.rs`:

```rust
pub mod editor;
// ...
pub use editor::*;
```

- [ ] **Step 3: Replace write_set_input in editor.rs**

```rust
// BEFORE (lines 176-192):
async fn write_set_input(client: &ox_broker::ClientHandle, text: &str, cursor: usize) {
    let mut map = std::collections::BTreeMap::new();
    map.insert(
        "text".to_string(),
        structfs_core_store::Value::String(text.to_string()),
    );
    map.insert(
        "cursor".to_string(),
        structfs_core_store::Value::Integer(cursor as i64),
    );
    let _ = client
        .write(
            &oxpath!("ui", "set_input"),
            structfs_core_store::Record::parsed(structfs_core_store::Value::Map(map)),
        )
        .await;
}

// AFTER:
async fn write_set_input(client: &ox_broker::ClientHandle, text: &str, cursor: usize) {
    let input = ox_types::SetInput {
        text: text.to_string(),
        cursor,
    };
    let _ = client
        .write_typed(&oxpath!("ui", "set_input"), &input)
        .await;
}
```

- [ ] **Step 4: Replace flush_pending_edits EditSequence write**

```rust
// BEFORE (lines 199-211):
if !input_session.pending_edits.is_empty() {
    let seq = EditSequence {
        edits: std::mem::take(&mut input_session.pending_edits),
        generation: input_session.generation,
    };
    let value = structfs_serde_store::to_value(&seq).unwrap();
    let _ = client
        .write(
            &oxpath!("ui", "input", "edit"),
            structfs_core_store::Record::parsed(value),
        )
        .await;
}

// AFTER:
if !input_session.pending_edits.is_empty() {
    let seq = EditSequence {
        edits: std::mem::take(&mut input_session.pending_edits),
        generation: input_session.generation,
    };
    let _ = client
        .write_typed(&oxpath!("ui", "input", "edit"), &seq)
        .await;
}
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo check -p ox-cli 2>&1 | head -30`
Expected: no errors

- [ ] **Step 6: Commit**

```bash
git add crates/ox-types/src/editor.rs crates/ox-types/src/lib.rs crates/ox-cli/src/editor.rs
git commit -m "refactor: typed SetInput and EditSequence writes in editor"
```

---

### Task 10: Input key dispatch (broker_setup.rs test)

Replace raw `BTreeMap` construction with typed `InputKeyEvent`.

**Files:**
- Modify: `crates/ox-cli/src/broker_setup.rs:197-204`

- [ ] **Step 1: Replace raw key event map with `InputKeyEvent`**

```rust
// BEFORE (lines 197-204):
let mut event = BTreeMap::new();
event.insert("mode".to_string(), Value::String("normal".to_string()));
event.insert("key".to_string(), Value::String("j".to_string()));
event.insert("screen".to_string(), Value::String("inbox".to_string()));
client
    .write(&path!("input/key"), Record::parsed(Value::Map(event)))
    .await
    .unwrap();

// AFTER:
let event = ox_types::InputKeyEvent {
    mode: ox_types::Mode::Normal,
    key: "j".to_string(),
    screen: ox_types::Screen::Inbox,
};
client
    .write_typed(&path!("input/key"), &event)
    .await
    .unwrap();
```

Check for any other raw key event construction in broker_setup.rs tests and apply the same pattern.

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p ox-cli 2>&1 | head -30`
Expected: no errors

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-cli 2>&1 | tail -20`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/ox-cli/src/broker_setup.rs
git commit -m "refactor: typed InputKeyEvent in broker_setup tests"
```

---

### Task 11: Command invocation write (editor.rs)

Replace `json_to_value` for command invocation with `write_typed`.

**Files:**
- Modify: `crates/ox-cli/src/editor.rs` (find `command/invoke` write)

- [ ] **Step 1: Find and replace command invocation write**

The `CommandInvocation` type already exists in `ox-ui`. The call site builds it as `serde_json::Value` and converts via `json_to_value`. Switch to `write_typed`:

```rust
// BEFORE (approximately):
let inv = serde_json::json!({ "command": cmd, "args": args_map });
let inv_value = structfs_serde_store::json_to_value(inv);
client
    .write(&oxpath!("command", "invoke"), Record::parsed(inv_value))
    .await;

// AFTER:
// If CommandInvocation is already constructed, just use write_typed:
client
    .write_typed(&oxpath!("command", "invoke"), &invocation)
    .await;
```

Read the actual code at the call site to determine the exact replacement. The `CommandInvocation` struct from `ox_ui::command_def` has `command: String` and `args: BTreeMap<String, serde_json::Value>`.

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p ox-cli 2>&1 | head -30`
Expected: no errors

- [ ] **Step 3: Commit**

```bash
git add crates/ox-cli/src/editor.rs
git commit -m "refactor: typed CommandInvocation write"
```

---

### Task 12: Thread registry history writes (thread_registry.rs)

Replace `json_to_value` for history snapshot restoration.

**Files:**
- Modify: `crates/ox-cli/src/thread_registry.rs` (find `history/append` writes)

- [ ] **Step 1: Find and replace history append writes**

These are `json_to_value(msg)` → `Record::parsed(value)` for history restoration from snapshots:

```rust
// BEFORE:
let value = json_to_value(msg);
ns.write(&append_path, Record::parsed(value)).ok();

// AFTER — use to_value since ns is a Namespace (Writer), not ClientHandle:
let value = structfs_serde_store::to_value(&msg).unwrap();
ns.write(&append_path, Record::parsed(value)).ok();
```

Actually, since `msg` is a `serde_json::Value` and `to_value` on a `serde_json::Value` produces the same thing as `json_to_value`, this is equivalent. But it removes the `json_to_value` import. If there's no `write_typed` on `Namespace`, keep using `to_value` + `Record::parsed`.

For test code in the same file, apply the same pattern.

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p ox-cli 2>&1 | head -30`
Expected: no errors

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-cli 2>&1 | tail -20`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/ox-cli/src/thread_registry.rs
git commit -m "refactor: typed history append writes in thread_registry"
```

---

### Task 13: Final cleanup and audit

Remove any remaining raw `Value::Map`/`Value::String`/`Value::Null` construction at write boundaries.

**Files:**
- Audit all files in `crates/ox-cli/src/`

- [ ] **Step 1: Search for remaining raw Value construction at write boundaries**

Run: `grep -rn 'Value::Map\|Value::String\|Value::Null' crates/ox-cli/src/ | grep -v '// \|test\|\.rs:.*//\|assert'`

Review each hit. Some `Value` usage is legitimate (e.g., in match arms reading values). Only flag writes that construct values for `Record::parsed()`.

- [ ] **Step 2: Search for remaining `json_to_value` usage**

Run: `grep -rn 'json_to_value' crates/ox-cli/src/`

Each remaining usage should be justified (e.g., the data is genuinely dynamic JSON, not a known struct).

- [ ] **Step 3: Run full quality gates**

Run: `./scripts/quality_gates.sh`
Expected: all 14 gates pass

- [ ] **Step 4: Commit any final cleanup**

```bash
git add -A
git commit -m "refactor: final cleanup — all store protocols typed"
```

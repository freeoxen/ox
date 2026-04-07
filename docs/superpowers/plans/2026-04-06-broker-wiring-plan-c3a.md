# Broker Wiring + Default Bindings (Plan C3a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the BrokerStore into ox-cli with all stores mounted and the full default key binding table defined, proving the input→state→render pipeline works end-to-end without touching the existing event loop.

**Architecture:** BrokerSetup creates a BrokerStore, mounts UiStore (at `ui/`), InputStore (at `input/`), and InboxStore (at `inbox/`). The default bindings module translates the current handle_normal_key/handle_insert_key/handle_approval_key logic into declarative Binding structs. An integration test proves the full chain: key event → InputStore dispatch → UiStore state change — through the broker, matching the current TUI behavior.

**Tech Stack:** Rust, ox-broker (BrokerStore, ClientHandle), ox-ui (UiStore, InputStore, Binding, Action, ActionField, ApprovalStore), ox-inbox (InboxStore), tokio, structfs-core-store

**Spec:** `docs/superpowers/specs/2026-04-06-structfs-tui-design.md` §Event Loop, §Command Protocol

---

## File Structure

| File | Responsibility |
|------|---------------|
| `crates/ox-cli/Cargo.toml` | Add ox-broker, ox-ui, tokio deps |
| `crates/ox-cli/src/bindings.rs` | Default key binding table — declarative encoding of current TUI keymaps |
| `crates/ox-cli/src/broker_setup.rs` | BrokerSetup — create broker, mount stores, return client |
| `crates/ox-cli/src/lib.rs` or module declarations | Wire new modules into crate |

---

### Task 1: Add Dependencies

**Files:**
- Modify: `crates/ox-cli/Cargo.toml`

- [ ] **Step 1: Add ox-broker, ox-ui, and tokio dependencies**

Add to the `[dependencies]` section:

```toml
ox-broker = { path = "../ox-broker" }
ox-ui = { path = "../ox-ui" }
tokio = { workspace = true }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p ox-cli`
Expected: clean build (new deps available but unused)

- [ ] **Step 3: Commit**

```
git add crates/ox-cli/Cargo.toml
git commit -m 'build(ox-cli): add ox-broker, ox-ui, tokio dependencies'
```

---

### Task 2: Default Key Bindings

**Files:**
- Create: `crates/ox-cli/src/bindings.rs`
- Modify: `crates/ox-cli/src/main.rs` (add `mod bindings;`)

This translates the imperative key handling in tui.rs into a declarative
binding table. Each entry in the current `handle_normal_key`,
`handle_insert_key`, and `handle_approval_key` functions becomes a
`Binding` struct with context-aware activation.

Key encoding convention: crossterm `KeyCode` variants are encoded as
strings: `"j"`, `"k"`, `"Enter"`, `"Esc"`, `"Backspace"`, `"Up"`,
`"Down"`, `"Left"`, `"Right"`, `"Ctrl+c"`, `"Ctrl+s"`, `"Ctrl+u"`,
`"Ctrl+a"`, `"Ctrl+e"`, `"Ctrl+t"`, `"Ctrl+Enter"`, `"/"`, `"d"`,
`"i"`, `"q"`, `"y"`, `"n"`, `"s"`, `"a"`.

For keys that behave differently based on screen, we create
screen-specific bindings (e.g., `j` on inbox → select_next, `j` on
thread → scroll_down).

- [ ] **Step 1: Write bindings.rs**

```rust
//! Default key binding table for the ox TUI.
//!
//! Encodes the current handle_normal_key / handle_insert_key /
//! handle_approval_key logic as declarative Binding structs.
//! The TUI event loop writes key events to InputStore, which
//! resolves bindings and dispatches commands.

use ox_ui::{Action, ActionField, Binding, BindingContext};
use structfs_core_store::Path;

/// Build the default binding table.
///
/// This is the single source of truth for keyboard shortcuts.
/// Runtime modifications (bind/unbind/macro) layer on top.
pub fn default_bindings() -> Vec<Binding> {
    let mut bindings = Vec::new();
    normal_mode_bindings(&mut bindings);
    insert_mode_bindings(&mut bindings);
    approval_mode_bindings(&mut bindings);
    bindings
}

fn p(s: &str) -> Path {
    Path::parse(s).expect("binding target must be valid path")
}

fn cmd(target: &str) -> Action {
    Action::Command {
        target: p(target),
        fields: vec![],
    }
}

fn cmd_with(target: &str, fields: Vec<ActionField>) -> Action {
    Action::Command {
        target: p(target),
        fields,
    }
}

fn bind(mode: &str, key: &str, action: Action, desc: &str) -> Binding {
    Binding {
        context: BindingContext {
            mode: mode.to_string(),
            key: key.to_string(),
            screen: None,
        },
        action,
        description: desc.to_string(),
    }
}

fn bind_screen(
    mode: &str,
    key: &str,
    screen: &str,
    action: Action,
    desc: &str,
) -> Binding {
    Binding {
        context: BindingContext {
            mode: mode.to_string(),
            key: key.to_string(),
            screen: Some(screen.to_string()),
        },
        action,
        description: desc.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Normal mode
// ---------------------------------------------------------------------------

fn normal_mode_bindings(out: &mut Vec<Binding>) {
    // -- Navigation (screen-specific) --
    out.push(bind_screen("normal", "j", "inbox", cmd("ui/select_next"), "Move selection down"));
    out.push(bind_screen("normal", "Down", "inbox", cmd("ui/select_next"), "Move selection down"));
    out.push(bind_screen("normal", "k", "inbox", cmd("ui/select_prev"), "Move selection up"));
    out.push(bind_screen("normal", "Up", "inbox", cmd("ui/select_prev"), "Move selection up"));

    out.push(bind_screen("normal", "j", "thread", cmd("ui/scroll_up"), "Scroll up"));
    out.push(bind_screen("normal", "Down", "thread", cmd("ui/scroll_up"), "Scroll up"));
    out.push(bind_screen("normal", "k", "thread", cmd("ui/scroll_down"), "Scroll down"));
    out.push(bind_screen("normal", "Up", "thread", cmd("ui/scroll_down"), "Scroll down"));

    // -- Screen transitions --
    out.push(bind_screen(
        "normal", "Ctrl+c", "thread",
        cmd("ui/close"),
        "Back to inbox",
    ));
    out.push(bind_screen(
        "normal", "Ctrl+c", "inbox",
        cmd("ui/quit"),
        "Quit",
    ));
    out.push(bind_screen(
        "normal", "Esc", "thread",
        cmd("ui/close"),
        "Back to inbox",
    ));
    out.push(bind_screen(
        "normal", "q", "thread",
        cmd("ui/close"),
        "Back to inbox",
    ));
    out.push(bind_screen(
        "normal", "q", "inbox",
        cmd("ui/quit"),
        "Quit",
    ));
    out.push(bind("normal", "Ctrl+t", cmd("ui/close"), "Back to inbox"));

    // -- Enter insert mode (screen-specific context) --
    out.push(bind_screen(
        "normal", "i", "inbox",
        cmd_with("ui/enter_insert", vec![
            ActionField::Static {
                key: "context".to_string(),
                value: structfs_core_store::Value::String("compose".to_string()),
            },
        ]),
        "Compose new thread",
    ));
    out.push(bind_screen(
        "normal", "i", "thread",
        cmd_with("ui/enter_insert", vec![
            ActionField::Static {
                key: "context".to_string(),
                value: structfs_core_store::Value::String("reply".to_string()),
            },
        ]),
        "Reply in thread",
    ));
    out.push(bind_screen(
        "normal", "/", "inbox",
        cmd_with("ui/enter_insert", vec![
            ActionField::Static {
                key: "context".to_string(),
                value: structfs_core_store::Value::String("search".to_string()),
            },
        ]),
        "Search",
    ));

    // -- Thread actions --
    out.push(bind_screen("normal", "Enter", "inbox", cmd("ui/open_selected"), "Open thread"));
    out.push(bind_screen("normal", "d", "inbox", cmd("ui/archive_selected"), "Archive thread"));

    // -- Approval quick keys (thread only) --
    out.push(bind_screen("normal", "y", "thread", cmd_with("approval/response", vec![
        ActionField::Static {
            key: "decision".to_string(),
            value: structfs_core_store::Value::String("allow_once".to_string()),
        },
    ]), "Allow once"));
    out.push(bind_screen("normal", "n", "thread", cmd_with("approval/response", vec![
        ActionField::Static {
            key: "decision".to_string(),
            value: structfs_core_store::Value::String("deny_once".to_string()),
        },
    ]), "Deny once"));
    out.push(bind_screen("normal", "s", "thread", cmd_with("approval/response", vec![
        ActionField::Static {
            key: "decision".to_string(),
            value: structfs_core_store::Value::String("allow_session".to_string()),
        },
    ]), "Allow for session"));
    out.push(bind_screen("normal", "a", "thread", cmd_with("approval/response", vec![
        ActionField::Static {
            key: "decision".to_string(),
            value: structfs_core_store::Value::String("allow_always".to_string()),
        },
    ]), "Allow always"));
}

// ---------------------------------------------------------------------------
// Insert mode
// ---------------------------------------------------------------------------

fn insert_mode_bindings(out: &mut Vec<Binding>) {
    out.push(bind("insert", "Ctrl+s", cmd("ui/send_input"), "Send"));
    out.push(bind("insert", "Ctrl+Enter", cmd("ui/send_input"), "Send"));
    out.push(bind("insert", "Esc", cmd("ui/exit_insert"), "Exit insert mode"));
    out.push(bind("insert", "Ctrl+u", cmd("ui/clear_input"), "Clear line"));
}

// ---------------------------------------------------------------------------
// Approval mode
// ---------------------------------------------------------------------------

fn approval_mode_bindings(out: &mut Vec<Binding>) {
    out.push(bind("approval", "j", cmd("ui/select_next"), "Next option"));
    out.push(bind("approval", "Down", cmd("ui/select_next"), "Next option"));
    out.push(bind("approval", "k", cmd("ui/select_prev"), "Previous option"));
    out.push(bind("approval", "Up", cmd("ui/select_prev"), "Previous option"));

    out.push(bind("approval", "y", cmd_with("approval/response", vec![
        ActionField::Static {
            key: "decision".to_string(),
            value: structfs_core_store::Value::String("allow_once".to_string()),
        },
    ]), "Allow once"));
    out.push(bind("approval", "n", cmd_with("approval/response", vec![
        ActionField::Static {
            key: "decision".to_string(),
            value: structfs_core_store::Value::String("deny_once".to_string()),
        },
    ]), "Deny once"));
    out.push(bind("approval", "s", cmd_with("approval/response", vec![
        ActionField::Static {
            key: "decision".to_string(),
            value: structfs_core_store::Value::String("allow_session".to_string()),
        },
    ]), "Allow for session"));
    out.push(bind("approval", "a", cmd_with("approval/response", vec![
        ActionField::Static {
            key: "decision".to_string(),
            value: structfs_core_store::Value::String("allow_always".to_string()),
        },
    ]), "Allow always"));
    out.push(bind("approval", "d", cmd_with("approval/response", vec![
        ActionField::Static {
            key: "decision".to_string(),
            value: structfs_core_store::Value::String("deny_always".to_string()),
        },
    ]), "Deny always"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bindings_not_empty() {
        let bindings = default_bindings();
        assert!(!bindings.is_empty());
    }

    #[test]
    fn j_has_screen_specific_bindings() {
        let bindings = default_bindings();
        let j_inbox: Vec<_> = bindings.iter().filter(|b| {
            b.context.mode == "normal"
                && b.context.key == "j"
                && b.context.screen.as_deref() == Some("inbox")
        }).collect();
        let j_thread: Vec<_> = bindings.iter().filter(|b| {
            b.context.mode == "normal"
                && b.context.key == "j"
                && b.context.screen.as_deref() == Some("thread")
        }).collect();
        assert_eq!(j_inbox.len(), 1, "j should have exactly one inbox binding");
        assert_eq!(j_thread.len(), 1, "j should have exactly one thread binding");
    }

    #[test]
    fn all_three_modes_have_bindings() {
        let bindings = default_bindings();
        assert!(bindings.iter().any(|b| b.context.mode == "normal"));
        assert!(bindings.iter().any(|b| b.context.mode == "insert"));
        assert!(bindings.iter().any(|b| b.context.mode == "approval"));
    }

    #[test]
    fn bindings_have_descriptions() {
        let bindings = default_bindings();
        for b in &bindings {
            assert!(!b.description.is_empty(), "binding {:?} has empty description", b.context);
        }
    }
}
```

- [ ] **Step 2: Add module declaration**

In `crates/ox-cli/src/main.rs`, add `mod bindings;` with the other module declarations.

- [ ] **Step 3: Verify and test**

Run: `cargo check -p ox-cli && cargo test -p ox-cli -- bindings`
Expected: clean build, 4 bindings tests pass

- [ ] **Step 4: Commit**

```
git add crates/ox-cli/src/bindings.rs crates/ox-cli/src/main.rs
git commit -m 'feat(ox-cli): default key binding table for TUI

Declarative encoding of current handle_normal_key, handle_insert_key,
and handle_approval_key as Binding structs. Screen-specific bindings
for context-dependent keys (j/k: select vs scroll). Three modes:
normal, insert, approval. Single source of truth for keyboard
shortcuts — runtime modifications layer on top.'
```

---

### Task 3: BrokerSetup — Mount Stores

**Files:**
- Create: `crates/ox-cli/src/broker_setup.rs`
- Modify: `crates/ox-cli/src/main.rs` (add `mod broker_setup;`)

BrokerSetup creates the BrokerStore and mounts all stores. It returns
a `BrokerHandle` containing the broker and client for the TUI.

- [ ] **Step 1: Write broker_setup.rs**

```rust
//! BrokerSetup — create the BrokerStore and mount all stores.
//!
//! This is the single point where the store namespace is assembled.
//! The TUI event loop and agent workers interact through client handles.

use ox_broker::BrokerStore;
use ox_inbox::InboxStore;
use ox_ui::{ApprovalStore, InputStore, UiStore, Binding};
use structfs_core_store::path;
use tokio::task::JoinHandle;

/// Handles returned from broker setup.
pub struct BrokerHandle {
    pub broker: BrokerStore,
    /// Server task handles (kept alive for the broker's lifetime).
    _servers: Vec<JoinHandle<()>>,
}

impl BrokerHandle {
    pub fn client(&self) -> ox_broker::ClientHandle {
        self.broker.client()
    }
}

/// Create and wire the BrokerStore with all stores mounted.
///
/// Mounts:
/// - `ui/` → UiStore (in-memory state machine)
/// - `input/` → InputStore (key binding translation)
/// - `inbox/` → InboxStore (SQLite-backed thread index)
pub async fn setup(
    inbox: InboxStore,
    bindings: Vec<Binding>,
) -> BrokerHandle {
    let broker = BrokerStore::default();
    let mut servers = Vec::new();

    // Mount UiStore
    servers.push(broker.mount(path!("ui"), UiStore::new()).await);

    // Mount InputStore with broker-connected dispatcher
    let dispatch_client = broker.client();
    let mut input = InputStore::new(bindings);
    input.set_dispatcher(Box::new(move |target, data| {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(dispatch_client.write(target, data))
        })
    }));
    servers.push(broker.mount(path!("input"), input).await);

    // Mount InboxStore
    servers.push(broker.mount(path!("inbox"), inbox).await);

    BrokerHandle {
        broker,
        _servers: servers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_ui::Binding;
    use structfs_core_store::{path, Value, Record};
    use std::collections::BTreeMap;

    fn test_inbox() -> InboxStore {
        let dir = tempfile::tempdir().unwrap();
        InboxStore::open(dir.path()).unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn broker_setup_mounts_all_stores() {
        let bindings = crate::bindings::default_bindings();
        let handle = setup(test_inbox(), bindings).await;
        let client = handle.client();

        // UiStore is mounted — read initial state
        let screen = client.read(&path!("ui/screen")).await.unwrap().unwrap();
        assert_eq!(
            screen.as_value().unwrap(),
            &Value::String("inbox".to_string())
        );

        // InputStore is mounted — read bindings
        let bindings = client.read(&path!("input/bindings/normal")).await.unwrap().unwrap();
        match bindings.as_value().unwrap() {
            Value::Array(a) => assert!(!a.is_empty()),
            _ => panic!("expected array"),
        }

        // InboxStore is mounted — read threads (empty initially)
        let threads = client.read(&path!("inbox/threads")).await.unwrap();
        assert!(threads.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn key_dispatch_through_broker() {
        let bindings = crate::bindings::default_bindings();
        let handle = setup(test_inbox(), bindings).await;
        let client = handle.client();

        // Set row count so selection can advance
        let mut count_cmd = BTreeMap::new();
        count_cmd.insert("count".to_string(), Value::Integer(5));
        client
            .write(&path!("ui/set_row_count"), Record::parsed(Value::Map(count_cmd)))
            .await
            .unwrap();

        // Dispatch "j" on inbox screen
        let mut event = BTreeMap::new();
        event.insert("mode".to_string(), Value::String("normal".to_string()));
        event.insert("key".to_string(), Value::String("j".to_string()));
        event.insert("screen".to_string(), Value::String("inbox".to_string()));
        client
            .write(&path!("input/key"), Record::parsed(Value::Map(event)))
            .await
            .unwrap();

        // Verify UiStore state changed
        let row = client.read(&path!("ui/selected_row")).await.unwrap().unwrap();
        assert_eq!(row.as_value().unwrap(), &Value::Integer(1));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn screen_specific_binding_routes_correctly() {
        let bindings = crate::bindings::default_bindings();
        let handle = setup(test_inbox(), bindings).await;
        let client = handle.client();

        // Open a thread so we're on the thread screen
        let mut open_cmd = BTreeMap::new();
        open_cmd.insert("thread_id".to_string(), Value::String("t_test".to_string()));
        client
            .write(&path!("ui/open"), Record::parsed(Value::Map(open_cmd)))
            .await
            .unwrap();

        // Now "j" on thread screen should scroll, not select
        let mut event = BTreeMap::new();
        event.insert("mode".to_string(), Value::String("normal".to_string()));
        event.insert("key".to_string(), Value::String("j".to_string()));
        event.insert("screen".to_string(), Value::String("thread".to_string()));
        client
            .write(&path!("input/key"), Record::parsed(Value::Map(event)))
            .await
            .unwrap();

        // Scroll should have changed (scroll_up from 0 stays 0 via saturating_sub)
        // but selected_row should NOT have changed (still 0)
        let row = client.read(&path!("ui/selected_row")).await.unwrap().unwrap();
        assert_eq!(row.as_value().unwrap(), &Value::Integer(0));
    }
}
```

- [ ] **Step 2: Add module declaration**

In `crates/ox-cli/src/main.rs`, add `mod broker_setup;` with the other module declarations.

- [ ] **Step 3: Verify and test**

Run: `cargo test -p ox-cli -- broker_setup`
Expected: 3 tests pass

- [ ] **Step 4: Run full workspace check**

Run: `cargo check && cargo test -p ox-cli`
Expected: clean build, all ox-cli tests pass

- [ ] **Step 5: Commit**

```
git add crates/ox-cli/src/broker_setup.rs crates/ox-cli/src/main.rs
git commit -m 'feat(ox-cli): BrokerSetup mounts UiStore + InputStore + InboxStore

BrokerSetup creates the BrokerStore and mounts all stores. InputStore
gets a block_in_place dispatcher that writes through the broker.
Integration tests prove: all stores reachable, key dispatch changes
UiStore state, screen-specific bindings route correctly.'
```

---

## Summary

| Task | What | Tests |
|------|------|-------|
| 1 | Add ox-broker, ox-ui, tokio deps to ox-cli | 0 (build check) |
| 2 | Default key binding table (all 3 modes) | 4 |
| 3 | BrokerSetup + integration tests | 3 |

**Total: 7 tests across 3 commits.**

After Plan C3a, we have:
- The complete default binding table — single source of truth for keyboard shortcuts
- BrokerStore with UiStore + InputStore + InboxStore mounted
- Proven end-to-end: key event → InputStore → BrokerStore → UiStore state change
- Screen-specific binding resolution working through the broker

Plan C3b rewrites the TUI event loop to use the broker client for state
reads and InputStore for key dispatch, replacing the imperative
handle_*_key functions with broker writes.

# Async Event Loop (Plan C3b) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the sync TUI event loop with an async loop that dispatches key events through the BrokerStore, while keeping draw functions and agent handling unchanged.

**Architecture:** The main function creates a tokio runtime and `block_on`s the async event loop. Simple state commands (navigation, mode, scroll) dispatch through InputStore → BrokerStore → UiStore. Complex application commands (send, open thread, archive, quit) are detected by target path and handled by existing App methods. UiStore state syncs back to App fields after each broker write so draw functions work unchanged. Crossterm polling uses `block_in_place` for the sync/async bridge.

**Tech Stack:** Rust, tokio (runtime, block_in_place), crossterm (event polling), ratatui (rendering), ox-broker (ClientHandle), ox-ui (InputStore key dispatch), structfs-core-store

**Spec:** `docs/superpowers/specs/2026-04-06-structfs-tui-design.md` §Event Loop

---

## File Structure

| File | Responsibility |
|------|---------------|
| `crates/ox-cli/src/key_encode.rs` | Encode crossterm KeyEvent → string key name for InputStore |
| `crates/ox-cli/src/state_sync.rs` | Read UiStore state from broker, sync to App fields |
| `crates/ox-cli/src/tui.rs` | (modify) Add async event loop alongside existing sync loop |
| `crates/ox-cli/src/main.rs` | (modify) Create tokio runtime, wire BrokerSetup, call async loop |

---

### Task 1: Key Encoding

**Files:**
- Create: `crates/ox-cli/src/key_encode.rs`
- Modify: `crates/ox-cli/src/main.rs` (add `mod key_encode;`)

Translates crossterm `KeyEvent` into the string key names that
InputStore's binding table uses: `"j"`, `"Ctrl+c"`, `"Enter"`, etc.

- [ ] **Step 1: Write key_encode.rs**

```rust
//! Encode crossterm key events as string names for InputStore dispatch.

use crossterm::event::{KeyCode, KeyModifiers};

/// Encode a crossterm key event as a string key name.
///
/// Convention:
/// - Letters: `"j"`, `"k"`, `"q"`, `"i"`
/// - Special: `"Enter"`, `"Esc"`, `"Backspace"`, `"Up"`, `"Down"`, `"Left"`, `"Right"`
/// - With Ctrl: `"Ctrl+c"`, `"Ctrl+s"`, `"Ctrl+Enter"`
/// - Digits: `"1"` through `"9"`
/// - Punctuation: `"/"`, `"d"`, etc.
pub fn encode_key(modifiers: KeyModifiers, code: KeyCode) -> Option<String> {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);

    match code {
        KeyCode::Char(c) if ctrl => Some(format!("Ctrl+{}", c)),
        KeyCode::Enter if ctrl => Some("Ctrl+Enter".to_string()),
        KeyCode::Char(c) => Some(c.to_string()),
        KeyCode::Enter => Some("Enter".to_string()),
        KeyCode::Esc => Some("Esc".to_string()),
        KeyCode::Backspace => Some("Backspace".to_string()),
        KeyCode::Up => Some("Up".to_string()),
        KeyCode::Down => Some("Down".to_string()),
        KeyCode::Left => Some("Left".to_string()),
        KeyCode::Right => Some("Right".to_string()),
        KeyCode::Tab => Some("Tab".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_char() {
        assert_eq!(encode_key(KeyModifiers::NONE, KeyCode::Char('j')), Some("j".to_string()));
    }

    #[test]
    fn ctrl_char() {
        assert_eq!(
            encode_key(KeyModifiers::CONTROL, KeyCode::Char('c')),
            Some("Ctrl+c".to_string())
        );
    }

    #[test]
    fn special_keys() {
        assert_eq!(encode_key(KeyModifiers::NONE, KeyCode::Enter), Some("Enter".to_string()));
        assert_eq!(encode_key(KeyModifiers::NONE, KeyCode::Esc), Some("Esc".to_string()));
        assert_eq!(encode_key(KeyModifiers::NONE, KeyCode::Up), Some("Up".to_string()));
    }

    #[test]
    fn ctrl_enter() {
        assert_eq!(
            encode_key(KeyModifiers::CONTROL, KeyCode::Enter),
            Some("Ctrl+Enter".to_string())
        );
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(encode_key(KeyModifiers::NONE, KeyCode::F(1)), None);
    }
}
```

- [ ] **Step 2: Add module to main.rs**

Add `mod key_encode;` to the module declarations in `main.rs`.

- [ ] **Step 3: Verify and test**

Run: `cargo test -p ox-cli -- key_encode`
Expected: 5 tests pass

- [ ] **Step 4: Commit**

```
git add crates/ox-cli/src/key_encode.rs crates/ox-cli/src/main.rs
git commit -m 'feat(ox-cli): key encoding for crossterm → InputStore bridge'
```

---

### Task 2: State Sync — Broker → App Fields

**Files:**
- Create: `crates/ox-cli/src/state_sync.rs`
- Modify: `crates/ox-cli/src/main.rs` (add `mod state_sync;`)

Reads UiStore state from the broker and updates App fields so
draw functions continue to work unchanged. This is the bridge
between the broker world and the existing rendering code.

- [ ] **Step 1: Write state_sync.rs**

```rust
//! State sync: read UiStore via broker, update App fields.
//!
//! Bridge between broker-managed state and App's fields that the
//! draw functions read. Called after every broker write.

use crate::app::{App, InputMode, InsertContext};
use ox_broker::ClientHandle;
use structfs_core_store::{path, Value};

/// Read UiStore state from the broker and sync to App fields.
///
/// Updates: mode, insert_context, active_thread, selected_row,
/// scroll, input, cursor. Does NOT touch thread_views, search,
/// event channels, or agent state.
pub async fn sync_ui_to_app(client: &ClientHandle, app: &mut App) {
    // Read all UiStore state in one call
    let state = match client.read(&path!("ui")).await {
        Ok(Some(record)) => match record.as_value() {
            Some(Value::Map(m)) => m.clone(),
            _ => return,
        },
        _ => return,
    };

    // Mode + insert context
    let mode_str = state.get("mode").and_then(|v| match v {
        Value::String(s) => Some(s.as_str()),
        _ => None,
    });
    let ctx_str = state.get("insert_context").and_then(|v| match v {
        Value::String(s) => Some(s.as_str()),
        _ => None,
    });
    match mode_str {
        Some("insert") => {
            let ctx = match ctx_str {
                Some("compose") => InsertContext::Compose,
                Some("reply") => InsertContext::Reply,
                Some("search") => InsertContext::Search,
                _ => InsertContext::Compose,
            };
            app.mode = InputMode::Insert(ctx);
        }
        _ => {
            app.mode = InputMode::Normal;
        }
    }

    // Active thread
    app.active_thread = state.get("active_thread").and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        _ => None,
    });

    // Selection + scroll
    if let Some(Value::Integer(n)) = state.get("selected_row") {
        app.selected_row = *n as usize;
    }
    if let Some(Value::Integer(n)) = state.get("scroll") {
        app.scroll = *n as u16;
    }

    // Input + cursor
    if let Some(Value::String(s)) = state.get("input") {
        app.input = s.clone();
    }
    if let Some(Value::Integer(n)) = state.get("cursor") {
        app.cursor = *n as usize;
    }
}
```

- [ ] **Step 2: Add module to main.rs**

Add `mod state_sync;` to the module declarations in `main.rs`.

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p ox-cli`
Expected: clean build

- [ ] **Step 4: Commit**

```
git add crates/ox-cli/src/state_sync.rs crates/ox-cli/src/main.rs
git commit -m 'feat(ox-cli): state sync — broker UiStore → App fields bridge'
```

---

### Task 3: Async Event Loop

**Files:**
- Modify: `crates/ox-cli/src/tui.rs`

Add a new `run_async` function alongside the existing `run`. This is
the core change: the event loop reads mode from App (for routing),
encodes key events, and dispatches through the broker. Complex
commands (send, open, archive, quit) are handled by App methods.
Simple state commands go through InputStore → UiStore.

The existing `run` function and all handle_*_key functions are kept
but no longer called from the new path.

- [ ] **Step 1: Add the async event loop function**

Add to `crates/ox-cli/src/tui.rs`, after the existing `run` function (around line 76):

```rust
/// Async event loop that dispatches through the BrokerStore.
///
/// Simple state commands (navigation, mode, scroll) go through
/// InputStore → BrokerStore → UiStore. Complex commands (send,
/// open thread, archive, quit) are handled directly by App methods.
pub async fn run_async(
    app: &mut App,
    client: &ox_broker::ClientHandle,
    theme: &Theme,
    terminal: &mut ratatui::DefaultTerminal,
) -> std::io::Result<()> {
    use crate::key_encode::encode_key;
    use crate::state_sync::sync_ui_to_app;
    use std::collections::BTreeMap;
    use structfs_core_store::{path, Record, Value};

    loop {
        // 1. Sync broker state → App fields
        sync_ui_to_app(client, app).await;

        // 2. Sync inbox row count to UiStore
        let row_count = app.cached_threads.len() as i64;
        let mut rc = BTreeMap::new();
        rc.insert("count".to_string(), Value::Integer(row_count));
        let _ = client
            .write(&path!("ui/set_row_count"), Record::parsed(Value::Map(rc)))
            .await;

        // 3. Draw
        terminal.draw(|frame| draw(frame, app, theme))?;

        // 4. Poll terminal event (blocking — bridge via block_in_place)
        let terminal_event = tokio::task::block_in_place(|| {
            if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                event::read().ok()
            } else {
                None
            }
        });

        // 5. Handle event
        if let Some(evt) = terminal_event {
            match evt {
                Event::Key(key) => {
                    // Customize dialog — bypass broker entirely
                    if app.pending_customize.is_some() {
                        handle_customize_key(app, key.code);
                    }
                    // Approval dialog — full dialog handling stays direct
                    else if app.pending_approval.is_some()
                        && matches!(app.mode, InputMode::Normal)
                    {
                        handle_approval_key(app, key.code, key.modifiers);
                    }
                    // Normal + Insert — dispatch through broker
                    else if let Some(key_str) = encode_key(key.modifiers, key.code) {
                        let mode = match &app.mode {
                            InputMode::Normal => "normal",
                            InputMode::Insert(_) => "insert",
                        };
                        let screen = if app.active_thread.is_some() {
                            "thread"
                        } else {
                            "inbox"
                        };

                        let mut event_map = BTreeMap::new();
                        event_map.insert(
                            "mode".to_string(),
                            Value::String(mode.to_string()),
                        );
                        event_map.insert(
                            "key".to_string(),
                            Value::String(key_str.clone()),
                        );
                        event_map.insert(
                            "screen".to_string(),
                            Value::String(screen.to_string()),
                        );

                        // Try InputStore dispatch
                        let result = client
                            .write(
                                &path!("input/key"),
                                Record::parsed(Value::Map(event_map)),
                            )
                            .await;

                        match result {
                            Ok(returned_path) => {
                                // Check if the dispatched command is an app-level action
                                let path_str = returned_path.to_string();
                                match path_str.as_str() {
                                    p if p.contains("send_input") => app.send_input(),
                                    p if p.contains("open_selected") => {
                                        app.open_selected_thread()
                                    }
                                    p if p.contains("archive_selected") => {
                                        app.archive_selected_thread()
                                    }
                                    p if p.contains("quit") => app.should_quit = true,
                                    _ => {
                                        // State command handled by UiStore — sync will pick it up
                                    }
                                }
                            }
                            Err(_) => {
                                // No binding found — handle text input directly
                                if matches!(app.mode, InputMode::Insert(_)) {
                                    let ctx = match &app.mode {
                                        InputMode::Insert(c) => c.clone(),
                                        _ => unreachable!(),
                                    };
                                    handle_insert_key(
                                        app,
                                        ctx,
                                        key.modifiers,
                                        key.code,
                                    );
                                }
                            }
                        }
                    }
                }
                Event::Mouse(mouse) => handle_mouse(app, mouse.kind, mouse.row),
                _ => {}
            }
        }

        // 6. Drain agent events (unchanged)
        while let Ok(event) = app.event_rx.try_recv() {
            app.handle_event(event);
        }

        // 7. Permission requests (unchanged)
        if app.pending_approval.is_none() && app.pending_customize.is_none() {
            if let Ok(AppControl::PermissionRequest {
                thread_id,
                tool,
                input_preview,
                respond,
            }) = app.control_rx.try_recv()
            {
                app.update_thread_state(&thread_id, "blocked_on_approval");
                app.open_thread(thread_id.clone());
                app.pending_approval = Some(ApprovalState {
                    thread_id,
                    tool,
                    input_preview,
                    selected: 0,
                    respond,
                });
            }
        }

        // 8. Quit
        if app.should_quit {
            return Ok(());
        }
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p ox-cli`
Expected: clean build (run_async is unused for now — Task 4 wires it)

- [ ] **Step 3: Commit**

```
git add crates/ox-cli/src/tui.rs
git commit -m 'feat(ox-cli): async event loop with broker dispatch

run_async dispatches key events through InputStore → BrokerStore →
UiStore. Complex commands (send, open, archive, quit) detected by
returned path and handled by App methods. Unbound keys in insert
mode fall through to direct text editing. Approval and customize
dialogs bypass the broker. State syncs from UiStore to App fields
after each cycle.'
```

---

### Task 4: Wire Async Loop in main()

**Files:**
- Modify: `crates/ox-cli/src/main.rs`

Replace the sync event loop call with the async version. Create a
tokio runtime, set up the broker, and run the async event loop.

- [ ] **Step 1: Update main() to use tokio runtime + broker**

Replace the main function body (starting from `let mut app = ...` around line 75) with:

```rust
    let mut app = app::App::new(
        cli.provider,
        model,
        cli.max_tokens,
        api_key,
        workspace,
        inbox_root.clone(),
        cli.no_policy,
    )
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    let theme = theme::Theme::default();

    // Create tokio runtime for broker
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // Setup broker with stores mounted
    let inbox = ox_inbox::InboxStore::open(&inbox_root)
        .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;
    let broker_bindings = bindings::default_bindings();
    let broker_handle = rt.block_on(broker_setup::setup(inbox, broker_bindings));
    let client = broker_handle.client();

    let mut terminal = ratatui::init();
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture).ok();

    let result = rt.block_on(tui::run_async(&mut app, &client, &theme, &mut terminal));

    crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture).ok();
    ratatui::restore();

    result?;
    Ok(())
```

Note: This opens a SECOND InboxStore instance for the broker. The first is in App::new (via AgentPool). This is acceptable for C3b — both instances connect to the same SQLite DB. Unifying them is a C3c concern.

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p ox-cli`
Expected: clean build

- [ ] **Step 3: Run existing tests**

Run: `cargo test -p ox-cli`
Expected: all tests pass (broker_setup, bindings, key_encode tests)

- [ ] **Step 4: Commit**

```
git add crates/ox-cli/src/main.rs
git commit -m 'feat(ox-cli): wire async event loop with tokio runtime

main() creates a multi-thread tokio runtime, sets up the broker
with UiStore + InputStore + InboxStore mounted, and runs the async
event loop. Simple key commands dispatch through the broker. Complex
commands (send, open, archive) handled by App methods. Agent events
and approval flow unchanged.'
```

---

## Summary

| Task | What | Tests |
|------|------|-------|
| 1 | Key encoding (crossterm → string) | 5 |
| 2 | State sync (broker → App fields) | 0 (compile check) |
| 3 | Async event loop (run_async) | 0 (compile check) |
| 4 | Wire in main() | 0 (integration — existing tests) |

**Total: 5 new tests + all existing tests must pass.**

After Plan C3b:
- Key events flow through BrokerStore for simple state commands
- Complex commands (send, open, archive) still use App methods
- Draw functions unchanged (read from App fields, synced from broker)
- Agent events unchanged (mpsc channels → App.handle_event)
- Approval/customize dialogs unchanged (direct App mutation)

**What this replaces:**
- `handle_normal_key` for navigation/mode commands → InputStore dispatch
- Direct `app.selected_row += 1` → `ui/select_next` through broker
- Direct `app.mode = Insert` → `ui/enter_insert` through broker

**What stays unchanged:**
- `handle_insert_key` for text editing (char insertion, backspace, cursor movement)
- `handle_approval_key` and `handle_customize_key` (dialog state machines)
- `handle_mouse` (direct App mutation)
- All draw functions (read App fields)
- Agent worker lifecycle (OS threads, Namespace, HostStore)

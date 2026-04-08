# App Convergence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate state duplication between App and UiStore, remove the last direct-mutation escape hatch (search), and split tui.rs into focused modules.

**Architecture:** Move search state (chips + live_query) into UiStore so all UI state flows through the broker. Remove duplicate fields (active_thread, mode, input, cursor) from App so ViewState is the single source of truth for rendering and event handling. Split the 1258-line tui.rs into event_loop.rs, key_handlers.rs, dialogs.rs, and a slimmed tui.rs.

**Tech Stack:** Rust, structfs-core-store (Reader/Writer/Value/Record/Path), ox-broker (ClientHandle), ratatui, crossterm

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/ox-ui/src/ui_store.rs` | Modify | Add search fields, read paths, write commands |
| `crates/ox-cli/src/view_state.rs` | Modify | Replace `SearchState` borrow with owned search fields; remove `InputMode` ref; add `insert_context`; add `search_matches` filter fn |
| `crates/ox-cli/src/app.rs` | Modify | Remove `SearchState`, `InputMode`, `InsertContext`, `active_thread`, `mode`, `input`, `cursor`; refactor `send_input_with_text`, `do_compose`, `do_reply` |
| `crates/ox-cli/src/tui.rs` | Modify | Slim down to `draw` + `draw_status_bar` using string matching instead of `InputMode` enums |
| `crates/ox-cli/src/event_loop.rs` | Create | `run_async`, `dispatch_text_edit_owned`, `dispatch_mouse_owned`, `send_approval_response` |
| `crates/ox-cli/src/key_handlers.rs` | Create | `handle_approval_key`, `handle_customize_key`, `infer_args` |
| `crates/ox-cli/src/dialogs.rs` | Create | `draw_approval_dialog`, `draw_customize_dialog`, `build_node_from_customize`, `build_sandbox_from_customize`, constants |
| `crates/ox-cli/src/inbox_view.rs` | Modify | Update `draw_filter_bar` to use owned search fields instead of `&SearchState` |
| `crates/ox-cli/src/main.rs` | Modify | Change `tui::run_async` to `event_loop::run_async` |

---

### Task 1: Add Search State to UiStore

**Files:**
- Modify: `crates/ox-ui/src/ui_store.rs`

- [ ] **Step 1: Write failing tests for search commands**

Add these tests to the existing `mod tests` block at the bottom of `crates/ox-ui/src/ui_store.rs`:

```rust
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
    // Type "bug"
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
    // Add two chips
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
    // Dismiss first chip (index 0)
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

    // Type something → active
    store
        .write(
            &path!("search_insert_char"),
            cmd_map(&[("char", Value::String("x".into()))]),
        )
        .unwrap();
    assert_eq!(read_str(&mut store, "search_active"), Value::Bool(true));

    // Clear → still inactive (no chips)
    store.write(&path!("search_clear"), empty_cmd()).unwrap();
    assert_eq!(read_str(&mut store, "search_active"), Value::Bool(false));

    // Add chip → active
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ox-ui -- search_`
Expected: All 11 new tests FAIL (unknown command / missing read paths).

- [ ] **Step 3: Add search fields and implement commands**

In `crates/ox-ui/src/ui_store.rs`, add two fields to the `UiStore` struct after `pending_action`:

```rust
    search_chips: Vec<String>,
    search_live_query: String,
```

In `UiStore::new()`, initialize them:

```rust
    search_chips: Vec::new(),
    search_live_query: String::new(),
```

Add helper methods to `impl UiStore` (before `fn all_fields_map`):

```rust
    fn search_chips_value(&self) -> Value {
        Value::Array(self.search_chips.iter().map(|s| Value::String(s.clone())).collect())
    }

    fn search_active(&self) -> bool {
        !self.search_chips.is_empty() || !self.search_live_query.is_empty()
    }
```

Add three entries to `fn all_fields_map` (inside the map-building block, after `pending_action`):

```rust
        map.insert("search_chips".to_string(), self.search_chips_value());
        map.insert("search_live_query".to_string(), Value::String(self.search_live_query.clone()));
        map.insert("search_active".to_string(), Value::Bool(self.search_active()));
```

Add three read paths to the `Reader` impl `match key` block (before the `_ => return Ok(None)` arm):

```rust
            "search_chips" => self.search_chips_value(),
            "search_live_query" => Value::String(self.search_live_query.clone()),
            "search_active" => Value::Bool(self.search_active()),
```

Add six write commands to the `Writer` impl `match command` block (before the `"send_input"` arm):

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ox-ui -- search_`
Expected: All 11 new tests PASS.

- [ ] **Step 5: Run full ox-ui tests**

Run: `cargo test -p ox-ui`
Expected: All tests pass (53 existing + 11 new = 64).

- [ ] **Step 6: Commit**

```bash
git add crates/ox-ui/src/ui_store.rs
git commit -m "feat(ox-ui): add search state to UiStore"
```

---

### Task 2: Wire Search Through ViewState and Event Loop

**Files:**
- Modify: `crates/ox-cli/src/view_state.rs`
- Modify: `crates/ox-cli/src/tui.rs`
- Modify: `crates/ox-cli/src/app.rs`
- Modify: `crates/ox-cli/src/inbox_view.rs`

- [ ] **Step 1: Update ViewState to use owned search fields**

In `crates/ox-cli/src/view_state.rs`:

Remove the import of `SearchState` from the `use crate::app::{...}` line (keep `App`, `ChatMessage`, `CustomizeState`, `InputMode`).

Replace the `search` field in `ViewState`:

```rust
    // Replace:
    //   pub search: &'a SearchState,
    // With:
    pub search_chips: Vec<String>,
    pub search_live_query: String,
    pub search_active: bool,
```

In `fetch_view_state`, read search fields from the broker ui_state map (after parsing `pending_action`):

```rust
    let search_chips = match ui_state.get("search_chips") {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let search_live_query = match ui_state.get("search_live_query") {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    let search_active = match ui_state.get("search_active") {
        Some(Value::Bool(b)) => *b,
        _ => false,
    };
```

Update the `ViewState` construction at the bottom of `fetch_view_state` to use the new fields:

```rust
    // Replace:
    //   search: &app.search,
    // With:
    search_chips,
    search_live_query,
    search_active,
```

Add the `search_matches` filter function as a standalone pub fn at the bottom of the file (before `#[cfg(test)]`):

```rust
/// Check whether a thread matches all search chips and the live query.
pub fn search_matches(
    chips: &[String],
    live_query: &str,
    title: &str,
    labels: &[String],
    state: &str,
) -> bool {
    let hay = format!(
        "{} {} {}",
        title.to_lowercase(),
        labels
            .iter()
            .map(|l| l.to_lowercase())
            .collect::<Vec<_>>()
            .join(" "),
        state.to_lowercase()
    );
    for chip in chips {
        if !hay.contains(&chip.to_lowercase()) {
            return false;
        }
    }
    if !live_query.is_empty() && !hay.contains(&live_query.to_lowercase()) {
        return false;
    }
    true
}
```

- [ ] **Step 2: Update inbox_view.rs to use owned search fields**

In `crates/ox-cli/src/inbox_view.rs`:

In `draw_inbox`, replace `vs.search.is_active()` with `vs.search_active`:

```rust
    // Line 15: Replace:
    //   if vs.search.is_active() {
    // With:
    if vs.search_active {
```

In `draw_filter_bar`, replace `vs.search.chips` and `vs.search.live_query`:

```rust
pub fn draw_filter_bar(frame: &mut Frame, vs: &ViewState, theme: &Theme, area: Rect) {
    let mut spans = vec![Span::styled("/ ", theme.tool_name)];
    for (i, chip) in vs.search_chips.iter().enumerate() {
        spans.push(Span::styled(
            format!("[{}: {}] ", i + 1, chip),
            theme.tool_meta,
        ));
    }
    spans.push(Span::styled(&vs.search_live_query, theme.user_text));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
```

- [ ] **Step 3: Update tui.rs to route search through broker**

In `crates/ox-cli/src/tui.rs`:

Replace `vs.search.is_active()` on line 596 with `vs.search_active`:

```rust
    let show_filter = vs.active_thread.is_none() && vs.search_active;
```

Replace the `search_active` extraction (line 96) to read from ViewState:

```rust
    // Replace:
    //   search_active = vs.search.is_active();
    // With:
    search_active = vs.search_active;
```

Replace the chip dismissal block (lines 183-189) to go through broker:

```rust
                        // Search chip dismissal (1-9 in normal mode, inbox, search active)
                        if mode == "normal" && screen == "inbox" && search_active {
                            if let KeyCode::Char(c @ '1'..='9') = key.code {
                                let idx = (c as u8 - b'1') as usize;
                                let mut cmd = BTreeMap::new();
                                cmd.insert("index".to_string(), Value::Integer(idx as i64));
                                let _ = client
                                    .write(
                                        &path!("ui/search_dismiss_chip"),
                                        Record::parsed(Value::Map(cmd)),
                                    )
                                    .await;
                                continue;
                            }
                        }
```

Replace the search fallback (lines 203-206) to go through broker instead of calling `handle_search_key`:

```rust
                            if let InputMode::Insert(ref ctx) = app.mode {
                                if *ctx == InsertContext::Search {
                                    dispatch_search_edit(client, key.modifiers, key.code).await;
                                } else {
```

Add the new `dispatch_search_edit` function (after `dispatch_text_edit_owned`):

```rust
/// Dispatch search text editing through UiStore via the broker.
async fn dispatch_search_edit(
    client: &ox_broker::ClientHandle,
    modifiers: KeyModifiers,
    code: KeyCode,
) {
    use std::collections::BTreeMap;
    use structfs_core_store::{Record, Value, path};

    match (modifiers, code) {
        (_, KeyCode::Enter) => {
            let _ = client
                .write(
                    &path!("ui/search_save_chip"),
                    Record::parsed(Value::Map(BTreeMap::new())),
                )
                .await;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
            let _ = client
                .write(
                    &path!("ui/search_clear"),
                    Record::parsed(Value::Map(BTreeMap::new())),
                )
                .await;
        }
        (_, KeyCode::Backspace) => {
            let _ = client
                .write(
                    &path!("ui/search_delete_char"),
                    Record::parsed(Value::Map(BTreeMap::new())),
                )
                .await;
        }
        (_, KeyCode::Char(c)) => {
            let mut cmd = BTreeMap::new();
            cmd.insert("char".to_string(), Value::String(c.to_string()));
            let _ = client
                .write(
                    &path!("ui/search_insert_char"),
                    Record::parsed(Value::Map(cmd)),
                )
                .await;
        }
        _ => {}
    }
}
```

Delete the `handle_search_key` function (lines 461-471).

- [ ] **Step 4: Remove SearchState from App**

In `crates/ox-cli/src/app.rs`:

Delete the `SearchState` struct and its `impl` block (lines 33-86).

Remove `pub search: SearchState` from the `App` struct.

Remove `search: SearchState::default(),` from `App::new`.

In `send_input_with_text`, remove the `InputMode::Insert(InsertContext::Search)` arm that called `self.search.save_chip()`. Search chip saving is now handled by the broker command in `dispatch_search_edit`. Replace:

```rust
            InputMode::Insert(InsertContext::Search) => {
                self.search.save_chip();
            }
```

With nothing — remove the arm entirely. The send_input pending_action won't fire for search mode because the InputStore binding for `ctrl+enter` in search mode maps to `search_save_chip`, not `send_input`.

- [ ] **Step 5: Verify compilation**

Run: `cargo check -p ox-cli`
Expected: Compiles with no errors. May have warnings about unused `SearchState` — that's fine, we deleted it.

- [ ] **Step 6: Run all tests**

Run: `cargo test -p ox-ui && cargo test -p ox-cli`
Expected: All tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/ox-cli/src/view_state.rs crates/ox-cli/src/tui.rs crates/ox-cli/src/app.rs crates/ox-cli/src/inbox_view.rs
git commit -m "feat(ox-cli): route search state through broker via UiStore"
```

---

### Task 3: Remove Duplicate App Fields

**Files:**
- Modify: `crates/ox-cli/src/app.rs`
- Modify: `crates/ox-cli/src/view_state.rs`
- Modify: `crates/ox-cli/src/tui.rs`

- [ ] **Step 1: Add insert_context to ViewState, remove input_mode**

In `crates/ox-cli/src/view_state.rs`:

Remove `InputMode` from the `use crate::app::{...}` import line.

In `ViewState`, replace:

```rust
    // Remove:
    //   pub input_mode: &'a InputMode,
    // Add:
    pub insert_context: Option<String>,
```

In `fetch_view_state`, parse `insert_context` from the broker ui_state map (after parsing `mode`):

```rust
    let insert_context = match ui_state.get("insert_context") {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    };
```

Update the ViewState construction — remove `input_mode: &app.mode,` and add `insert_context,`.

- [ ] **Step 2: Update tui.rs draw functions to use string matching**

In `crates/ox-cli/src/tui.rs`:

Remove `InputMode` and `InsertContext` from the `use crate::app::{...}` import.

In `fn draw`, replace `InputMode` pattern matches with string matches:

```rust
    // Replace:
    //   let in_insert = matches!(vs.input_mode, InputMode::Insert(_));
    // With:
    let in_insert = vs.mode == "insert";
```

Replace the input box title logic:

```rust
        let ctx_label = match vs.insert_context.as_deref() {
            Some("compose") => " compose ",
            Some("reply") => " reply ",
            Some("search") => " search ",
            _ => "",
        };
```

Replace the cursor positioning logic:

```rust
        if vs.approval_pending.is_none() && vs.pending_customize.is_none() {
            if vs.insert_context.as_deref() != Some("search") {
                frame.set_cursor_position((
                    input_area.x + vs.cursor as u16 + 2,
                    input_area.y + 1,
                ));
            }
        }
```

In `fn draw_status_bar`, replace all `InputMode` pattern matches:

```rust
fn draw_status_bar(frame: &mut Frame, vs: &ViewState, theme: &Theme, area: Rect) {
    let mode_badge = if vs.mode == "insert" {
        Span::styled(" INSERT ", theme.insert_badge)
    } else {
        Span::styled(" NORMAL ", theme.title_badge)
    };

    let context_info = if vs.active_thread.is_some() {
        let (ti, to) = vs.turn_tokens;
        format!(" {}in/{}out", ti, to)
    } else {
        let count = vs.inbox_threads.len();
        format!(" {} thread{}", count, if count == 1 { "" } else { "s" })
    };

    let hints = match (vs.mode.as_str(), vs.insert_context.as_deref(), vs.active_thread.is_some()) {
        ("normal", _, false) => " | i compose | / search | Enter open | d archive | q quit",
        ("normal", _, true) => " | i reply | j/k scroll | q/Esc inbox",
        ("insert", Some("search"), _) => " | Enter chip | Esc cancel",
        ("insert", _, _) => " | ^Enter send | Esc cancel",
        _ => "",
    };

    let status_line = Line::from(vec![
        mode_badge,
        Span::styled(context_info, theme.status),
        Span::styled(hints, theme.status),
    ]);
    frame.render_widget(Paragraph::new(status_line), area);
}
```

- [ ] **Step 3: Refactor App — remove duplicate fields, refactor methods**

In `crates/ox-cli/src/app.rs`:

Delete the `InputMode` and `InsertContext` enums (lines 10-27).

Remove these fields from `App`:

```rust
    // Remove:
    pub active_thread: Option<String>,
    pub mode: InputMode,
    pub input: String,
    pub cursor: usize,
```

Remove from `App::new`:

```rust
    // Remove:
    active_thread: None,
    mode: InputMode::default(),
    input: String::new(),
    cursor: 0,
```

Refactor `send_input_with_text` to take explicit parameters:

```rust
    /// Send input with explicit text and context from ViewState.
    /// Returns Some(thread_id) if a new thread was composed.
    pub fn send_input_with_text(
        &mut self,
        text: String,
        mode: &str,
        insert_context: Option<&str>,
        active_thread: Option<&str>,
    ) -> Option<String> {
        if text.is_empty() {
            return None;
        }
        match (mode, insert_context) {
            ("insert", Some("compose")) | ("normal", None) if active_thread.is_none() => {
                self.do_compose(text)
            }
            ("insert", Some("reply")) | ("normal", _) if active_thread.is_some() => {
                self.do_reply(text, active_thread.unwrap());
                None
            }
            _ => None,
        }
    }
```

Refactor `do_compose` to take input and return thread_id:

```rust
    /// Create a new thread from input text. Returns thread_id on success.
    fn do_compose(&mut self, input: String) -> Option<String> {
        self.input_history.push(input.clone());
        self.history_cursor = self.input_history.len();
        self.input_draft.clear();

        let title: String = input.chars().take(40).collect();
        match self.pool.create_thread(&title) {
            Ok(tid) => {
                self.update_thread_state(&tid, "running");
                self.pool.send_prompt(&tid, input).ok();
                Some(tid)
            }
            Err(e) => {
                eprintln!("failed to create thread: {e}");
                None
            }
        }
    }
```

Refactor `do_reply` to take input and thread_id:

```rust
    /// Send a reply to the given thread.
    fn do_reply(&mut self, input: String, thread_id: &str) {
        self.input_history.push(input.clone());
        self.history_cursor = self.input_history.len();
        self.input_draft.clear();

        self.update_thread_state(thread_id, "running");
        self.pool.send_prompt(thread_id, input).ok();
    }
```

Delete `open_thread` method.

Update `history_up` and `history_down` to take/return input+cursor instead of mutating self.input/self.cursor. These methods need the current input from ViewState:

```rust
    /// Navigate input history up (older). Returns (new_input, new_cursor) or None if no change.
    pub fn history_up(&mut self, current_input: &str) -> Option<(String, usize)> {
        if self.input_history.is_empty() {
            return None;
        }
        if self.history_cursor == self.input_history.len() {
            self.input_draft = current_input.to_string();
        }
        if self.history_cursor > 0 {
            self.history_cursor -= 1;
            let text = self.input_history[self.history_cursor].clone();
            let cursor = text.len();
            Some((text, cursor))
        } else {
            None
        }
    }

    /// Navigate input history down (newer). Returns (new_input, new_cursor) or None if no change.
    pub fn history_down(&mut self) -> Option<(String, usize)> {
        if self.history_cursor < self.input_history.len() {
            self.history_cursor += 1;
            let text = if self.history_cursor == self.input_history.len() {
                self.input_draft.clone()
            } else {
                self.input_history[self.history_cursor].clone()
            };
            let cursor = text.len();
            Some((text, cursor))
        } else {
            None
        }
    }
```

- [ ] **Step 4: Update event loop for removed fields**

In `crates/ox-cli/src/tui.rs`:

Remove `InputMode` and `InsertContext` from all imports in the file. These enums no longer exist.

Add new extraction variables in the ViewState scope block:

```rust
        let mode_owned: String;
        let insert_context_owned: Option<String>;
```

Extract them from ViewState:

```rust
        mode_owned = vs.mode.clone();
        insert_context_owned = vs.insert_context.clone();
```

Update the `send_input` pending_action handler:

```rust
                "send_input" => {
                    let new_tid = app.send_input_with_text(
                        input_text.clone(),
                        &mode_owned,
                        insert_context_owned.as_deref(),
                        active_thread_id.as_deref(),
                    );
                    // Clear input and exit insert mode through broker
                    let _ = client
                        .write(&path!("ui/clear_input"), Record::parsed(Value::Map(BTreeMap::new())))
                        .await;
                    let _ = client
                        .write(&path!("ui/exit_insert"), Record::parsed(Value::Map(BTreeMap::new())))
                        .await;
                    // If compose created a new thread, open it
                    if let Some(tid) = new_tid {
                        let mut cmd = BTreeMap::new();
                        cmd.insert("thread_id".to_string(), Value::String(tid));
                        let _ = client
                            .write(&path!("ui/open"), Record::parsed(Value::Map(cmd)))
                            .await;
                    }
                }
```

Note: `send_input_with_text` now returns `Option<String>` (new thread id from compose). Update its signature in app.rs to return this:

```rust
    pub fn send_input_with_text(...) -> Option<String> {
        // ... match arms return do_compose result or None
    }
```

Update the `open_selected` handler — remove `app.open_thread(id.clone())`:

```rust
                "open_selected" => {
                    if let Some(id) = &selected_thread_id {
                        let mut cmd = BTreeMap::new();
                        cmd.insert("thread_id".to_string(), Value::String(id.clone()));
                        let _ = client
                            .write(&path!("ui/open"), Record::parsed(Value::Map(cmd)))
                            .await;
                    }
                }
```

Delete the `sync_mode_to_broker` function entirely.

Update the approval check to use string mode:

```rust
                    // Replace:
                    //   else if has_approval_pending && matches!(app.mode, InputMode::Normal) {
                    // With:
                    else if has_approval_pending && mode_owned == "normal" {
```

Update the mode string for InputStore dispatch:

```rust
                        // Replace:
                        //   let mode = match &app.mode {
                        //       InputMode::Normal => "normal",
                        //       InputMode::Insert(_) => "insert",
                        //   };
                        // With:
                        let mode = mode_owned.as_str();
```

Update the search fallback:

```rust
                        if result.is_err() {
                            if mode_owned == "insert" {
                                if insert_context_owned.as_deref() == Some("search") {
                                    dispatch_search_edit(client, key.modifiers, key.code).await;
                                } else {
                                    match key.code {
                                        KeyCode::Up => {
                                            if let Some((text, cursor)) = app.history_up(&input_text) {
                                                let mut cmd = BTreeMap::new();
                                                cmd.insert("text".to_string(), Value::String(text));
                                                cmd.insert("cursor".to_string(), Value::Integer(cursor as i64));
                                                let _ = client
                                                    .write(&path!("ui/set_input"), Record::parsed(Value::Map(cmd)))
                                                    .await;
                                            }
                                        }
                                        KeyCode::Down => {
                                            if let Some((text, cursor)) = app.history_down() {
                                                let mut cmd = BTreeMap::new();
                                                cmd.insert("text".to_string(), Value::String(text));
                                                cmd.insert("cursor".to_string(), Value::Integer(cursor as i64));
                                                let _ = client
                                                    .write(&path!("ui/set_input"), Record::parsed(Value::Map(cmd)))
                                                    .await;
                                            }
                                        }
                                        _ => {
                                            dispatch_text_edit_owned(
                                                client,
                                                cursor_pos,
                                                input_len,
                                                key.modifiers,
                                                key.code,
                                            )
                                            .await;
                                        }
                                    }
                                }
                            }
                        }
```

- [ ] **Step 5: Verify compilation**

Run: `cargo check -p ox-cli`
Expected: Compiles. No references to `app.mode`, `app.active_thread`, `app.input`, `app.cursor`, `InputMode`, `InsertContext` (from app.rs), `sync_mode_to_broker`, or `app.open_thread`.

- [ ] **Step 6: Run all tests**

Run: `cargo test -p ox-ui && cargo test -p ox-cli`
Expected: All tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/ox-cli/src/app.rs crates/ox-cli/src/view_state.rs crates/ox-cli/src/tui.rs
git commit -m "refactor(ox-cli): remove duplicate App fields, single source of truth in UiStore"
```

---

### Task 4: Split tui.rs into Focused Modules

**Files:**
- Create: `crates/ox-cli/src/event_loop.rs`
- Create: `crates/ox-cli/src/key_handlers.rs`
- Create: `crates/ox-cli/src/dialogs.rs`
- Modify: `crates/ox-cli/src/tui.rs`
- Modify: `crates/ox-cli/src/main.rs`

- [ ] **Step 1: Create dialogs.rs**

Create `crates/ox-cli/src/dialogs.rs` with the following content extracted from `tui.rs`:

- `EFFECTS`, `SCOPES`, `NETWORKS` constants
- `fn infer_args` (made `pub(crate)`)
- `fn build_node_from_customize` (made `pub(crate)`)
- `fn build_sandbox_from_customize` (made `pub(crate)`)
- `pub(crate) fn draw_approval_dialog`
- `pub(crate) fn draw_customize_dialog`

The file header:

```rust
use crate::app::CustomizeState;
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::APPROVAL_OPTIONS;
```

Move the following functions verbatim (with `pub(crate)` visibility on functions called from other modules):

- `draw_approval_dialog` → `pub(crate) fn draw_approval_dialog`
- `draw_customize_dialog` → `pub(crate) fn draw_customize_dialog`
- `build_node_from_customize` → `pub(crate) fn build_node_from_customize`
- `build_sandbox_from_customize` → `pub(crate) fn build_sandbox_from_customize`
- `infer_args` → `pub(crate) fn infer_args`
- `EFFECTS`, `SCOPES`, `NETWORKS` → `pub(crate) const`

- [ ] **Step 2: Create key_handlers.rs**

Create `crates/ox-cli/src/key_handlers.rs` with content extracted from `tui.rs`:

```rust
use crate::app::{APPROVAL_OPTIONS, App};
use crate::dialogs::infer_args;
use crossterm::event::{KeyCode, KeyModifiers};
use structfs_core_store::{Record, Value};
```

Move these functions with `pub(crate)` visibility:

- `handle_approval_key` → `pub(crate) async fn handle_approval_key`
- `handle_customize_key` → `pub(crate) async fn handle_customize_key`
- `send_approval_response` → `pub(crate) async fn send_approval_response`

- [ ] **Step 3: Create event_loop.rs**

Create `crates/ox-cli/src/event_loop.rs` with the event loop extracted from `tui.rs`:

```rust
use crate::app::App;
use crate::key_handlers::{handle_approval_key, handle_customize_key, send_approval_response};
use crate::theme::Theme;
use crate::view_state::fetch_view_state;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use std::collections::BTreeMap;
use std::time::Duration;
use structfs_core_store::{Record, Value, path};
use structfs_core_store::Writer as StructWriter;
use crate::app::APPROVAL_OPTIONS;
```

Move these functions:

- `run_async` → `pub async fn run_async`
- `dispatch_text_edit_owned` → `async fn dispatch_text_edit_owned`
- `dispatch_mouse_owned` → `async fn dispatch_mouse_owned`
- `dispatch_search_edit` → `async fn dispatch_search_edit`

`run_async` calls `crate::tui::draw` for rendering.

- [ ] **Step 4: Slim down tui.rs**

`crates/ox-cli/src/tui.rs` retains only:

```rust
use crate::app::{ChatMessage, ThreadView};
use crate::theme::Theme;
use crate::view_state::ViewState;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// Main draw function. Takes a ViewState snapshot.
///
/// Returns `(content_height, viewport_height)` for scroll_max calculation.
pub(crate) fn draw(frame: &mut Frame, vs: &ViewState, theme: &Theme) -> (Option<usize>, usize) {
    // ... existing draw body, calling crate::dialogs::draw_approval_dialog
    // and crate::dialogs::draw_customize_dialog for modal overlays
}

fn draw_status_bar(frame: &mut Frame, vs: &ViewState, theme: &Theme, area: Rect) {
    // ... existing draw_status_bar body
}
```

Update modal overlay calls in `draw`:

```rust
    // Replace direct calls:
    //   draw_customize_dialog(frame, customize, theme);
    //   draw_approval_dialog(frame, tool, preview, vs.approval_selected, theme);
    // With:
    crate::dialogs::draw_customize_dialog(frame, customize, theme);
    crate::dialogs::draw_approval_dialog(frame, tool, preview, vs.approval_selected, theme);
```

- [ ] **Step 5: Update main.rs module declarations and entry point**

In `crates/ox-cli/src/main.rs`:

Add new module declarations:

```rust
mod dialogs;
mod event_loop;
mod key_handlers;
```

Change the `run_async` call:

```rust
    // Replace:
    //   let result = rt.block_on(tui::run_async(&mut app, &client, &theme, &mut terminal));
    // With:
    let result = rt.block_on(event_loop::run_async(&mut app, &client, &theme, &mut terminal));
```

- [ ] **Step 6: Verify compilation**

Run: `cargo check -p ox-cli`
Expected: Compiles cleanly.

- [ ] **Step 7: Run all tests**

Run: `cargo test -p ox-ui && cargo test -p ox-cli`
Expected: All tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/ox-cli/src/dialogs.rs crates/ox-cli/src/event_loop.rs crates/ox-cli/src/key_handlers.rs crates/ox-cli/src/tui.rs crates/ox-cli/src/main.rs
git commit -m "refactor(ox-cli): split tui.rs into event_loop, key_handlers, dialogs modules"
```

---

### Task 5: Final Cleanup and Quality Gate

**Files:**
- Modify: `docs/design/rfc/structfs-tui-status.md`

- [ ] **Step 1: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: All 14 gates pass.

- [ ] **Step 2: Run formatter**

Run: `./scripts/fmt.sh`
Expected: No changes (or apply formatting if needed).

- [ ] **Step 3: Update status document**

In `docs/design/rfc/structfs-tui-status.md`, add a new section after the C7 entry under "Phase C":

```markdown
#### Phase 1: App Convergence (complete)
- Search state (chips + live_query) moved into UiStore with 6 new commands
- `SearchState` struct, `handle_search_key` deleted from ox-cli
- `active_thread`, `mode`, `input`, `cursor` removed from App (UiStore is single source of truth)
- `InputMode`, `InsertContext` enums deleted from app.rs (ViewState uses strings)
- `sync_mode_to_broker` deleted
- `open_thread` deleted (broker `ui/open` command only)
- `history_up`/`history_down` take explicit parameters, return new state
- `tui.rs` split: event_loop.rs (~280), key_handlers.rs (~180), dialogs.rs (~280), tui.rs (~140)
- App fields reduced from 13 to 8: pool, model, provider, input_history, history_cursor, input_draft, approval_selected, pending_customize
```

Update the "What's Next" section — remove "Search State in UiStore" from the remaining items.

- [ ] **Step 4: Commit**

```bash
git add docs/design/rfc/structfs-tui-status.md
git commit -m "docs: update status for Phase 1 App Convergence completion"
```

# History Explorer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a full-screen history explorer to ox-cli, accessible via `h` in normal mode on a thread, showing structured message data with drill-down and real-time updates.

**Architecture:** New `History` screen variant flows through the same UiStore/ScreenSnapshot/bindings pipeline as existing screens. The history view reads from the same broker path (`threads/{id}/history/messages`) as the thread view, parsed into a richer `HistoryEntry` struct with duplicate detection. Key bindings support j/k selection, Enter expand/collapse, and Esc/q/h to return.

**Tech Stack:** Rust, ratatui 0.29, crossterm 0.28, structfs (Reader/Writer/Store), ox-types, ox-ui, ox-cli

---

## File Map

### New Files

| File | Responsibility |
|------|---------------|
| `crates/ox-cli/src/history_view.rs` | `draw_history()` — renders the history explorer screen |

### Modified Files

| File | Changes |
|------|---------|
| `crates/ox-types/src/ui.rs` | Add `History` to `Screen` enum |
| `crates/ox-types/src/snapshot.rs` | Add `HistorySnapshot`, `ScreenSnapshot::History` variant, update `editor()` |
| `crates/ox-types/src/command.rs` | Add `HistoryCommand` enum, `UiCommand::History` variant, `GlobalCommand::OpenHistory`/`BackToThread` |
| `crates/ox-ui/src/ui_store.rs` | Add `HistoryState`, `handle_history()`, screen transitions, screen-aware `resolve_path_command` |
| `crates/ox-cli/src/parse.rs` | Add `HistoryEntry`, `HistoryBlock`, `EntryFlags`, `parse_history_entries()` |
| `crates/ox-cli/src/bindings.rs` | Add `h` on thread screen, all history screen bindings |
| `crates/ox-cli/src/view_state.rs` | Add `raw_messages` field, history arm in `fetch_view_state` |
| `crates/ox-cli/src/tui.rs` | Wire `ScreenSnapshot::History` into `draw()` |
| `crates/ox-cli/src/event_loop.rs` | Wire history screen into scroll feedback and key dispatch |
| `crates/ox-cli/src/theme.rs` | Add history-specific styles (duplicate badge, header, role badges) |
| `crates/ox-cli/src/types.rs` | (no changes needed — `HistoryEntry` lives in parse.rs) |

---

### Task 1: ox-types — Screen, Snapshot, and Command Types

**Files:**
- Modify: `crates/ox-types/src/ui.rs:5-10`
- Modify: `crates/ox-types/src/snapshot.rs:1-89`
- Modify: `crates/ox-types/src/command.rs:1-121`

- [ ] **Step 1: Add `History` to `Screen` enum**

In `crates/ox-types/src/ui.rs`, add the variant:

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Screen {
    #[default]
    Inbox,
    Thread,
    Settings,
    History,
}
```

- [ ] **Step 2: Add `HistorySnapshot` and `ScreenSnapshot::History`**

In `crates/ox-types/src/snapshot.rs`, add the struct and variant:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySnapshot {
    pub thread_id: String,
    pub selected_row: usize,
    pub scroll: usize,
    pub scroll_max: usize,
    pub viewport_height: usize,
    /// Message indices currently expanded for detail view.
    pub expanded: Vec<usize>,
}
```

Add `History(HistorySnapshot)` to `ScreenSnapshot`:

```rust
pub enum ScreenSnapshot {
    Inbox(InboxSnapshot),
    Thread(ThreadSnapshot),
    Settings(SettingsSnapshot),
    History(HistorySnapshot),
}
```

Update `UiSnapshot::editor()` to handle the new variant:

```rust
pub fn editor(&self) -> Option<&EditorSnapshot> {
    match &self.screen {
        ScreenSnapshot::Inbox(s) => s.editor.as_ref(),
        ScreenSnapshot::Thread(s) => s.editor.as_ref(),
        ScreenSnapshot::Settings(_) => None,
        ScreenSnapshot::History(_) => None,
    }
}
```

- [ ] **Step 3: Add `HistoryCommand`, `UiCommand::History`, and global transitions**

In `crates/ox-types/src/command.rs`, add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum HistoryCommand {
    SelectNext,
    SelectPrev,
    SelectFirst,
    SelectLast,
    ToggleExpand,
    ExpandAll,
    CollapseAll,
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
}
```

Add to `UiCommand`:

```rust
pub enum UiCommand {
    Global(GlobalCommand),
    Inbox(InboxCommand),
    Thread(ThreadCommand),
    Settings(SettingsCommand),
    History(HistoryCommand),
}
```

Add to `GlobalCommand`:

```rust
pub enum GlobalCommand {
    Quit,
    Open { thread_id: String },
    Close,
    GoToSettings,
    GoToInbox,
    SetStatus { text: String },
    ClearPendingAction,
    OpenHistory { thread_id: String },
    BackToThread { thread_id: String },
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p ox-types 2>&1 | head -20`
Expected: Warnings about unused variants are OK. No errors.

- [ ] **Step 5: Commit**

```
git add crates/ox-types/src/ui.rs crates/ox-types/src/snapshot.rs crates/ox-types/src/command.rs
git commit -m "feat(ox-types): add History screen, HistorySnapshot, HistoryCommand types"
```

---

### Task 2: ox-ui — UiStore History State and Command Handling

**Files:**
- Modify: `crates/ox-ui/src/ui_store.rs`

- [ ] **Step 1: Add `HistoryState` and `ActiveScreen::History`**

After the existing `SettingsState` struct (~line 128), add:

```rust
struct HistoryState {
    thread_id: String,
    selected_row: usize,
    scroll: usize,
    scroll_max: usize,
    viewport_height: usize,
    expanded: std::collections::HashSet<usize>,
}

impl HistoryState {
    fn new(thread_id: String) -> Self {
        HistoryState {
            thread_id,
            selected_row: 0,
            scroll: 0,
            scroll_max: 0,
            viewport_height: 0,
            expanded: std::collections::HashSet::new(),
        }
    }
}
```

Add variant to `ActiveScreen`:

```rust
enum ActiveScreen {
    Inbox(InboxState),
    Thread(ThreadState),
    Settings(SettingsState),
    History(HistoryState),
}
```

- [ ] **Step 2: Add `history_state()` guard helper**

After the `settings_state()` method:

```rust
fn history_state(&mut self) -> Result<&mut HistoryState, StoreError> {
    match &mut self.screen {
        ActiveScreen::History(s) => Ok(s),
        _ => Err(StoreError::store("ui", "history", "not on history screen")),
    }
}
```

- [ ] **Step 3: Update `active_editor_mut()` for History**

```rust
fn active_editor_mut(&mut self) -> Result<&mut EditorState, StoreError> {
    match &mut self.screen {
        ActiveScreen::Inbox(s) => s.editor.as_mut(),
        ActiveScreen::Thread(s) => s.editor.as_mut(),
        ActiveScreen::Settings(_) => None,
        ActiveScreen::History(_) => None,
    }
    .ok_or_else(|| StoreError::store("ui", "editor", "no active editor"))
}
```

- [ ] **Step 4: Update `snapshot()` for History**

In the `snapshot()` method, add the arm:

```rust
ActiveScreen::History(s) => ScreenSnapshot::History(HistorySnapshot {
    thread_id: s.thread_id.clone(),
    selected_row: s.selected_row,
    scroll: s.scroll,
    scroll_max: s.scroll_max,
    viewport_height: s.viewport_height,
    expanded: s.expanded.iter().copied().collect(),
}),
```

- [ ] **Step 5: Update `screen_name()`, `mode_value()`, `insert_context_value()`, `active_thread_value()`**

```rust
// screen_name
ActiveScreen::History(_) => "history",

// mode_value — history is always normal mode
ActiveScreen::History(_) => Mode::Normal,

// insert_context_value
ActiveScreen::History(_) => None,

// active_thread_value — return the history screen's thread_id
ActiveScreen::History(s) => Value::String(s.thread_id.clone()),
```

- [ ] **Step 6: Add `handle_global` arms for `OpenHistory` and `BackToThread`**

In `handle_global`:

```rust
GlobalCommand::OpenHistory { thread_id } => {
    self.screen = ActiveScreen::History(HistoryState::new(thread_id));
    Ok(path!("screen"))
}
GlobalCommand::BackToThread { thread_id } => {
    self.screen = ActiveScreen::Thread(ThreadState::new(thread_id));
    Ok(path!("screen"))
}
```

- [ ] **Step 7: Add `handle_history` method**

After `handle_settings`:

```rust
fn handle_history(&mut self, cmd: HistoryCommand) -> Result<Path, StoreError> {
    let _ = self.history_state()?;

    match cmd {
        HistoryCommand::SelectNext => {
            let s = self.history_state()?;
            s.selected_row += 1;
            // row_count clamping happens at render time since message count is dynamic
            Ok(path!("selected_row"))
        }
        HistoryCommand::SelectPrev => {
            let s = self.history_state()?;
            s.selected_row = s.selected_row.saturating_sub(1);
            Ok(path!("selected_row"))
        }
        HistoryCommand::SelectFirst => {
            let s = self.history_state()?;
            s.selected_row = 0;
            Ok(path!("selected_row"))
        }
        HistoryCommand::SelectLast => {
            let s = self.history_state()?;
            // Set to usize::MAX — render clamps to actual count
            s.selected_row = usize::MAX;
            Ok(path!("selected_row"))
        }
        HistoryCommand::ToggleExpand => {
            let s = self.history_state()?;
            let row = s.selected_row;
            if !s.expanded.remove(&row) {
                s.expanded.insert(row);
            }
            Ok(path!("expanded"))
        }
        HistoryCommand::ExpandAll => {
            let s = self.history_state()?;
            // Insert indices 0..1000 — actual message count clamps at render time.
            // A debugging tool won't have thousands of messages in practice.
            for i in 0..1000 {
                s.expanded.insert(i);
            }
            Ok(path!("expanded"))
        }
        HistoryCommand::CollapseAll => {
            let s = self.history_state()?;
            s.expanded.clear();
            Ok(path!("expanded"))
        }
        HistoryCommand::ScrollUp => {
            let s = self.history_state()?;
            if s.scroll < s.scroll_max {
                s.scroll += 1;
            }
            Ok(path!("scroll"))
        }
        HistoryCommand::ScrollDown => {
            let s = self.history_state()?;
            s.scroll = s.scroll.saturating_sub(1);
            Ok(path!("scroll"))
        }
        HistoryCommand::ScrollToTop => {
            let s = self.history_state()?;
            s.scroll = s.scroll_max;
            Ok(path!("scroll"))
        }
        HistoryCommand::ScrollToBottom => {
            let s = self.history_state()?;
            s.scroll = 0;
            Ok(path!("scroll"))
        }
        HistoryCommand::ScrollPageUp => {
            let s = self.history_state()?;
            s.scroll = (s.scroll + s.viewport_height).min(s.scroll_max);
            Ok(path!("scroll"))
        }
        HistoryCommand::ScrollPageDown => {
            let s = self.history_state()?;
            s.scroll = s.scroll.saturating_sub(s.viewport_height);
            Ok(path!("scroll"))
        }
        HistoryCommand::ScrollHalfPageUp => {
            let s = self.history_state()?;
            let half = s.viewport_height / 2;
            s.scroll = (s.scroll + half).min(s.scroll_max);
            Ok(path!("scroll"))
        }
        HistoryCommand::ScrollHalfPageDown => {
            let s = self.history_state()?;
            let half = s.viewport_height / 2;
            s.scroll = s.scroll.saturating_sub(half);
            Ok(path!("scroll"))
        }
        HistoryCommand::SetScrollMax { max } => {
            let s = self.history_state()?;
            s.scroll_max = max;
            if s.scroll > s.scroll_max {
                s.scroll = s.scroll_max;
            }
            Ok(path!("scroll_max"))
        }
        HistoryCommand::SetViewportHeight { height } => {
            let s = self.history_state()?;
            s.viewport_height = height;
            Ok(path!("viewport_height"))
        }
    }
}
```

- [ ] **Step 8: Update `dispatch_command`**

```rust
fn dispatch_command(&mut self, cmd: UiCommand) -> Result<Path, StoreError> {
    match cmd {
        UiCommand::Global(g) => self.handle_global(g),
        UiCommand::Inbox(i) => self.handle_inbox(i),
        UiCommand::Thread(t) => self.handle_thread(t),
        UiCommand::Settings(s) => self.handle_settings(s),
        UiCommand::History(h) => self.handle_history(h),
    }
}
```

- [ ] **Step 9: Update `resolve_path_command` for screen-aware routing**

The commands `select_next`, `select_prev`, `select_first`, `select_last`, `scroll_up`, `scroll_down`, etc. need to route to `HistoryCommand` when on the history screen. Update the match arms:

```rust
// Selection — route by screen
"select_next" => match &self.screen {
    ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::SelectNext)),
    _ => Ok(UiCommand::Inbox(InboxCommand::SelectNext)),
},
"select_prev" => match &self.screen {
    ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::SelectPrev)),
    _ => Ok(UiCommand::Inbox(InboxCommand::SelectPrev)),
},
"select_first" => match &self.screen {
    ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::SelectFirst)),
    _ => Ok(UiCommand::Inbox(InboxCommand::SelectFirst)),
},
"select_last" => match &self.screen {
    ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::SelectLast)),
    _ => Ok(UiCommand::Inbox(InboxCommand::SelectLast)),
},

// Scroll — route by screen
"scroll_up" => match &self.screen {
    ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::ScrollUp)),
    _ => Ok(UiCommand::Thread(ThreadCommand::ScrollUp)),
},
"scroll_down" => match &self.screen {
    ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::ScrollDown)),
    _ => Ok(UiCommand::Thread(ThreadCommand::ScrollDown)),
},
"scroll_to_top" => match &self.screen {
    ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::ScrollToTop)),
    _ => Ok(UiCommand::Thread(ThreadCommand::ScrollToTop)),
},
"scroll_to_bottom" => match &self.screen {
    ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::ScrollToBottom)),
    _ => Ok(UiCommand::Thread(ThreadCommand::ScrollToBottom)),
},
"scroll_page_up" => match &self.screen {
    ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::ScrollPageUp)),
    _ => Ok(UiCommand::Thread(ThreadCommand::ScrollPageUp)),
},
"scroll_page_down" => match &self.screen {
    ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::ScrollPageDown)),
    _ => Ok(UiCommand::Thread(ThreadCommand::ScrollPageDown)),
},
"scroll_half_page_up" => match &self.screen {
    ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::ScrollHalfPageUp)),
    _ => Ok(UiCommand::Thread(ThreadCommand::ScrollHalfPageUp)),
},
"scroll_half_page_down" => match &self.screen {
    ActiveScreen::History(_) => Ok(UiCommand::History(HistoryCommand::ScrollHalfPageDown)),
    _ => Ok(UiCommand::Thread(ThreadCommand::ScrollHalfPageDown)),
},
```

Add new history-specific commands:

```rust
"open_history" => {
    let thread_id = match &self.screen {
        ActiveScreen::Thread(s) => s.thread_id.clone(),
        _ => return Err(err("open_history requires thread screen")),
    };
    Ok(UiCommand::Global(GlobalCommand::OpenHistory { thread_id }))
}
"back_to_thread" => {
    let thread_id = match &self.screen {
        ActiveScreen::History(s) => s.thread_id.clone(),
        _ => return Err(err("back_to_thread requires history screen")),
    };
    Ok(UiCommand::Global(GlobalCommand::BackToThread { thread_id }))
}
"toggle_expand" => Ok(UiCommand::History(HistoryCommand::ToggleExpand)),
"expand_all" => Ok(UiCommand::History(HistoryCommand::ExpandAll)),
"collapse_all" => Ok(UiCommand::History(HistoryCommand::CollapseAll)),
```

Also update the `exit_insert` and `send_input` matches to handle the History screen (return errors, since history has no editor):

```rust
// exit_insert
ActiveScreen::History(_) => Err(StoreError::store(
    "ui", "path_command", "exit_insert not supported on history screen",
)),

// send_input
ActiveScreen::History(_) => Err(StoreError::store(
    "ui", "path_command", "send_input not supported on history screen",
)),
```

- [ ] **Step 10: Update Reader helpers for history screen**

In the Reader-related helper methods, add History arms:

```rust
// scroll_value (if it exists — check and add)
ActiveScreen::History(s) => Value::Integer(s.scroll as i64),

// scroll_max_value
ActiveScreen::History(s) => Value::Integer(s.scroll_max as i64),

// viewport_height_value
ActiveScreen::History(s) => Value::Integer(s.viewport_height as i64),
```

- [ ] **Step 11: Verify it compiles**

Run: `cargo check -p ox-ui 2>&1 | head -30`
Expected: May have warnings. No errors.

- [ ] **Step 12: Commit**

```
git add crates/ox-ui/src/ui_store.rs
git commit -m "feat(ox-ui): UiStore history screen state and command handling"
```

---

### Task 3: ox-cli — Parse History Entries with Duplicate Detection

**Files:**
- Modify: `crates/ox-cli/src/parse.rs`

- [ ] **Step 1: Write the failing test for `parse_history_entries`**

At the bottom of the `#[cfg(test)] mod tests` block in `parse.rs`, add:

```rust
#[test]
fn parse_history_entries_basic() {
    let user_msg = map(vec![("role", s("user")), ("content", s("hello"))]);
    let text_block = map(vec![("type", s("text")), ("text", s("hi there"))]);
    let assistant_msg = map(vec![
        ("role", s("assistant")),
        ("content", Value::Array(vec![text_block])),
    ]);
    let entries = parse_history_entries(&[user_msg, assistant_msg]);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].index, 0);
    assert_eq!(entries[0].role, "user");
    assert_eq!(entries[0].summary, "hello");
    assert_eq!(entries[0].block_count, 1);
    assert_eq!(entries[0].text_len, 5);
    assert!(!entries[0].flags.duplicate_content);

    assert_eq!(entries[1].index, 1);
    assert_eq!(entries[1].role, "assistant");
    assert_eq!(entries[1].summary, "hi there");
    assert_eq!(entries[1].block_count, 1);
    assert_eq!(entries[1].text_len, 8);
    assert!(!entries[1].flags.duplicate_content);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-cli parse_history_entries_basic 2>&1 | tail -10`
Expected: FAIL — `parse_history_entries` not found.

- [ ] **Step 3: Add the `HistoryEntry`, `HistoryBlock`, `EntryFlags` types and `parse_history_entries`**

Add these structs and the function above the tests module in `parse.rs`:

```rust
// ---------------------------------------------------------------------------
// HistoryEntry — rich parsed message for the history explorer
// ---------------------------------------------------------------------------

/// Parsed message for the history explorer.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub index: usize,
    pub role: String,
    /// Summary line for collapsed view (first ~80 chars of text content).
    pub summary: String,
    /// Number of content blocks.
    pub block_count: usize,
    /// Total text length across all text blocks.
    pub text_len: usize,
    /// Content blocks for expanded view.
    pub blocks: Vec<HistoryBlock>,
    /// Visual indicator flags.
    pub flags: EntryFlags,
}

/// A single content block within a message.
#[derive(Debug, Clone)]
pub struct HistoryBlock {
    pub block_type: String,
    pub text: Option<String>,
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
    pub input_json: Option<String>,
}

/// Flags for visual indicators.
#[derive(Debug, Clone, Default)]
pub struct EntryFlags {
    pub duplicate_content: bool,
    pub duplicate_of: Option<usize>,
}

/// Parse raw StructFS Values into HistoryEntry list with duplicate detection.
pub fn parse_history_entries(values: &[Value]) -> Vec<HistoryEntry> {
    let mut entries = Vec::with_capacity(values.len());

    for (index, val) in values.iter().enumerate() {
        let map = match val {
            Value::Map(m) => m,
            _ => continue,
        };

        let role = match map.get("role") {
            Some(Value::String(s)) => s.clone(),
            _ => continue,
        };

        let content = match map.get("content") {
            Some(v) => v,
            None => continue,
        };

        let (blocks, summary, block_count, text_len) = parse_content_blocks(&role, content);

        entries.push(HistoryEntry {
            index,
            role,
            summary,
            block_count,
            text_len,
            blocks,
            flags: EntryFlags::default(),
        });
    }

    // Duplicate detection: compare text of same-role consecutive messages
    detect_duplicates(&mut entries);

    entries
}

fn parse_content_blocks(
    role: &str,
    content: &Value,
) -> (Vec<HistoryBlock>, String, usize, usize) {
    match content {
        Value::String(s) => {
            let summary = truncate_summary(s, 80);
            let block = HistoryBlock {
                block_type: if role == "assistant" {
                    "text".to_string()
                } else {
                    "text".to_string()
                },
                text: Some(s.clone()),
                tool_name: None,
                tool_use_id: None,
                input_json: None,
            };
            (vec![block], summary, 1, s.len())
        }
        Value::Array(arr) => {
            let mut blocks = Vec::new();
            let mut all_text = String::new();
            let mut summary_parts = Vec::new();

            for item in arr {
                let Value::Map(block_map) = item else {
                    continue;
                };
                let block_type = match block_map.get("type") {
                    Some(Value::String(s)) => s.clone(),
                    _ => continue,
                };

                match block_type.as_str() {
                    "text" => {
                        let text = match block_map.get("text") {
                            Some(Value::String(s)) => s.clone(),
                            _ => String::new(),
                        };
                        all_text.push_str(&text);
                        if summary_parts.is_empty() {
                            summary_parts.push(truncate_summary(&text, 80));
                        }
                        blocks.push(HistoryBlock {
                            block_type: "text".to_string(),
                            text: Some(text),
                            tool_name: None,
                            tool_use_id: None,
                            input_json: None,
                        });
                    }
                    "tool_use" => {
                        let name = match block_map.get("name") {
                            Some(Value::String(s)) => s.clone(),
                            _ => "unknown".to_string(),
                        };
                        let id = match block_map.get("id") {
                            Some(Value::String(s)) => Some(s.clone()),
                            _ => None,
                        };
                        let input = block_map.get("input").map(|v| format_value(v));
                        summary_parts.push(format!("tool_use: {name}"));
                        blocks.push(HistoryBlock {
                            block_type: "tool_use".to_string(),
                            text: None,
                            tool_name: Some(name),
                            tool_use_id: id,
                            input_json: input,
                        });
                    }
                    "tool_result" => {
                        let content_str = match block_map.get("content") {
                            Some(Value::String(s)) => Some(s.clone()),
                            _ => None,
                        };
                        let id = match block_map.get("tool_use_id") {
                            Some(Value::String(s)) => Some(s.clone()),
                            _ => None,
                        };
                        let len = content_str.as_ref().map(|s| s.len()).unwrap_or(0);
                        all_text.push_str(content_str.as_deref().unwrap_or(""));
                        summary_parts.push(format!(
                            "tool_result{}",
                            id.as_ref()
                                .map(|i| format!(": {}...", &i[..i.len().min(12)]))
                                .unwrap_or_default()
                        ));
                        blocks.push(HistoryBlock {
                            block_type: "tool_result".to_string(),
                            text: content_str,
                            tool_name: None,
                            tool_use_id: id,
                            input_json: None,
                        });
                        // Include tool_result text length
                        let _ = len;
                    }
                    _ => {}
                }
            }

            let summary = if summary_parts.is_empty() {
                "(empty)".to_string()
            } else {
                summary_parts.join(" | ")
            };
            let text_len = all_text.len();
            let block_count = blocks.len();
            (blocks, summary, block_count, text_len)
        }
        _ => (Vec::new(), "(empty)".to_string(), 0, 0),
    }
}

/// Truncate a string for summary display.
fn truncate_summary(s: &str, max: usize) -> String {
    let first_line = s.lines().next().unwrap_or(s);
    if first_line.len() > max {
        format!("{}...", &first_line[..max])
    } else {
        first_line.to_string()
    }
}

/// Format a StructFS Value as a compact JSON-like string.
fn format_value(val: &Value) -> String {
    match val {
        Value::String(s) => format!("\"{s}\""),
        Value::Integer(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Null => "null".to_string(),
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(format_value).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Map(map) => {
            let pairs: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("\"{k}\": {}", format_value(v)))
                .collect();
            format!("{{{}}}", pairs.join(", "))
        }
    }
}

/// Detect duplicate messages by comparing text content of same-role entries.
fn detect_duplicates(entries: &mut [HistoryEntry]) {
    for i in 1..entries.len() {
        if entries[i].text_len == 0 {
            continue;
        }
        // Look backwards for same-role messages with matching text
        for j in (0..i).rev() {
            if entries[j].role != entries[i].role {
                continue;
            }
            // Compare the concatenated text of all text blocks
            let text_i = concat_text(&entries[i].blocks);
            let text_j = concat_text(&entries[j].blocks);
            if !text_i.is_empty() && text_i == text_j {
                entries[i].flags.duplicate_content = true;
                entries[i].flags.duplicate_of = Some(j);
            }
            break; // Only compare with the most recent same-role message
        }
    }
}

fn concat_text(blocks: &[HistoryBlock]) -> String {
    let mut s = String::new();
    for b in blocks {
        if let Some(t) = &b.text {
            s.push_str(t);
        }
    }
    s
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ox-cli parse_history_entries_basic 2>&1 | tail -5`
Expected: PASS

- [ ] **Step 5: Write duplicate detection test**

```rust
#[test]
fn parse_history_entries_duplicate_detection() {
    let msg1 = map(vec![
        ("role", s("assistant")),
        ("content", s("hello world")),
    ]);
    let msg2 = map(vec![("role", s("user")), ("content", s("ok"))]);
    let msg3 = map(vec![
        ("role", s("assistant")),
        ("content", s("hello world")),
    ]);
    let entries = parse_history_entries(&[msg1, msg2, msg3]);
    assert_eq!(entries.len(), 3);
    assert!(!entries[0].flags.duplicate_content);
    assert!(!entries[1].flags.duplicate_content);
    assert!(entries[2].flags.duplicate_content);
    assert_eq!(entries[2].flags.duplicate_of, Some(0));
}
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p ox-cli parse_history_entries_duplicate 2>&1 | tail -5`
Expected: PASS

- [ ] **Step 7: Write tool_use/tool_result test**

```rust
#[test]
fn parse_history_entries_tool_blocks() {
    let tool_use_block = map(vec![
        ("type", s("tool_use")),
        ("id", s("toolu_01ABC")),
        ("name", s("read_file")),
        ("input", map(vec![("path", s("/tmp/x"))])),
    ]);
    let assistant_msg = map(vec![
        ("role", s("assistant")),
        ("content", Value::Array(vec![tool_use_block])),
    ]);
    let entries = parse_history_entries(&[assistant_msg]);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].block_count, 1);
    assert_eq!(entries[0].blocks[0].block_type, "tool_use");
    assert_eq!(
        entries[0].blocks[0].tool_name.as_deref(),
        Some("read_file")
    );
    assert_eq!(
        entries[0].blocks[0].tool_use_id.as_deref(),
        Some("toolu_01ABC")
    );
    assert!(entries[0].blocks[0].input_json.is_some());
    assert!(entries[0].summary.contains("tool_use: read_file"));
}
```

- [ ] **Step 8: Run test to verify it passes**

Run: `cargo test -p ox-cli parse_history_entries_tool 2>&1 | tail -5`
Expected: PASS

- [ ] **Step 9: Commit**

```
git add crates/ox-cli/src/parse.rs
git commit -m "feat(ox-cli): parse_history_entries with duplicate detection"
```

---

### Task 4: Key Bindings

**Files:**
- Modify: `crates/ox-cli/src/bindings.rs`

- [ ] **Step 1: Add `h` binding on thread screen and all history screen bindings**

In `normal_mode()`, before the `// Settings` section, add:

```rust
// History explorer
out.push(bind_screen(
    "normal",
    "h",
    "thread",
    invoke("open_history"),
    "History explorer",
));
```

Add a new function `history_mode` and call it from `default_bindings()`:

```rust
pub fn default_bindings() -> Vec<Binding> {
    let mut b = Vec::new();
    normal_mode(&mut b);
    insert_mode(&mut b);
    approval_mode(&mut b);
    history_mode(&mut b);
    b
}
```

```rust
fn history_mode(out: &mut Vec<Binding>) {
    // Navigation
    out.push(bind_screen(
        "normal",
        "j",
        "history",
        invoke("select_next"),
        "Next message",
    ));
    out.push(bind_screen(
        "normal",
        "Down",
        "history",
        invoke("select_next"),
        "Next message",
    ));
    out.push(bind_screen(
        "normal",
        "k",
        "history",
        invoke("select_prev"),
        "Previous message",
    ));
    out.push(bind_screen(
        "normal",
        "Up",
        "history",
        invoke("select_prev"),
        "Previous message",
    ));
    out.push(bind_screen(
        "normal",
        "g",
        "history",
        invoke("select_first"),
        "First message",
    ));
    out.push(bind_screen(
        "normal",
        "G",
        "history",
        invoke("select_last"),
        "Last message",
    ));

    // Expand/collapse
    out.push(bind_screen(
        "normal",
        "Enter",
        "history",
        invoke("toggle_expand"),
        "Toggle expand",
    ));
    out.push(bind_screen(
        "normal",
        " ",
        "history",
        invoke("toggle_expand"),
        "Toggle expand",
    ));
    out.push(bind_screen(
        "normal",
        "e",
        "history",
        invoke("expand_all"),
        "Expand all",
    ));
    out.push(bind_screen(
        "normal",
        "E",
        "history",
        invoke("collapse_all"),
        "Collapse all",
    ));

    // Scrolling
    out.push(bind_screen(
        "normal",
        "Ctrl+d",
        "history",
        invoke("scroll_half_page_down"),
        "Half page down",
    ));
    out.push(bind_screen(
        "normal",
        "Ctrl+u",
        "history",
        invoke("scroll_half_page_up"),
        "Half page up",
    ));
    out.push(bind_screen(
        "normal",
        "Ctrl+f",
        "history",
        invoke("scroll_page_down"),
        "Page down",
    ));
    out.push(bind_screen(
        "normal",
        "Ctrl+b",
        "history",
        invoke("scroll_page_up"),
        "Page up",
    ));

    // Exit
    out.push(bind_screen(
        "normal",
        "Esc",
        "history",
        invoke("back_to_thread"),
        "Back to thread",
    ));
    out.push(bind_screen(
        "normal",
        "q",
        "history",
        invoke("back_to_thread"),
        "Back to thread",
    ));
    out.push(bind_screen(
        "normal",
        "h",
        "history",
        invoke("back_to_thread"),
        "Back to thread",
    ));
    out.push(bind_screen(
        "normal",
        "Ctrl+c",
        "history",
        invoke("back_to_thread"),
        "Back to thread",
    ));
}
```

- [ ] **Step 2: Add a test for the history bindings**

```rust
#[test]
fn h_on_thread_opens_history() {
    let bindings = default_bindings();
    let found: Vec<_> = bindings
        .iter()
        .filter(|b| {
            b.context.mode == "normal"
                && b.context.key == "h"
                && b.context.screen.as_deref() == Some("thread")
        })
        .collect();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].description, "History explorer");
}

#[test]
fn history_screen_has_bindings() {
    let bindings = default_bindings();
    let history_bindings: Vec<_> = bindings
        .iter()
        .filter(|b| b.context.screen.as_deref() == Some("history"))
        .collect();
    // At minimum: j, k, g, G, Enter, Space, e, E, Ctrl+d, Ctrl+u, Ctrl+f, Ctrl+b, Esc, q, h, Ctrl+c, Down, Up
    assert!(
        history_bindings.len() >= 16,
        "expected at least 16 history bindings, got {}",
        history_bindings.len()
    );
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-cli h_on_thread_opens_history history_screen_has_bindings 2>&1 | tail -10`
Expected: PASS

- [ ] **Step 4: Commit**

```
git add crates/ox-cli/src/bindings.rs
git commit -m "feat(ox-cli): key bindings for history explorer"
```

---

### Task 5: ViewState — History Screen Data

**Files:**
- Modify: `crates/ox-cli/src/view_state.rs`

- [ ] **Step 1: Add `raw_messages` field to `ViewState`**

```rust
pub struct ViewState<'a> {
    // ... existing fields ...

    /// Raw StructFS message values for the history explorer.
    pub raw_messages: Vec<Value>,
}
```

- [ ] **Step 2: Add `ScreenSnapshot::History` arm to `fetch_view_state`**

In the match on `&ui.screen`, add after the `ScreenSnapshot::Thread` arm:

```rust
ScreenSnapshot::History(snap) => {
    let tid = &snap.thread_id;
    // Read committed messages (same path as thread)
    let msg_path = ox_path::oxpath!("threads", tid, "history", "messages");
    if let Ok(Some(record)) = client.read(&msg_path).await {
        if let Some(Value::Array(arr)) = record.as_value() {
            raw_messages = arr.clone();
        }
    }

    // Read turn state for live streaming indicator
    let turn_path = ox_path::oxpath!("threads", tid, "history", "turn");
    if let Ok(Some(t)) = client.read_typed::<ox_history::TurnState>(&turn_path).await {
        turn = t;
    }
}
```

Add `let mut raw_messages = Vec::new();` alongside the other `let mut` declarations.

- [ ] **Step 3: Add `raw_messages` to the ViewState construction at the end**

```rust
ViewState {
    // ... existing fields ...
    raw_messages,
}
```

- [ ] **Step 4: Update the `(mode_str, screen_str)` match for key hints**

```rust
ScreenSnapshot::History(_) => ("normal", "history"),
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo check -p ox-cli 2>&1 | head -20`
Expected: May warn about unused `raw_messages` (used in next task). No errors.

- [ ] **Step 6: Commit**

```
git add crates/ox-cli/src/view_state.rs
git commit -m "feat(ox-cli): ViewState history screen data fetching"
```

---

### Task 6: Theme — History-Specific Styles

**Files:**
- Modify: `crates/ox-cli/src/theme.rs`

- [ ] **Step 1: Add history styles to Theme struct**

```rust
pub struct Theme {
    // ... existing fields ...

    /// History explorer: header line.
    pub history_header: Style,
    /// History explorer: selected row.
    pub history_selected: Style,
    /// History explorer: message index number.
    pub history_index: Style,
    /// History explorer: role badge — user.
    pub history_role_user: Style,
    /// History explorer: role badge — assistant.
    pub history_role_assistant: Style,
    /// History explorer: role badge — tool_result (user with tool_result content).
    pub history_role_tool: Style,
    /// History explorer: message summary text.
    pub history_summary: Style,
    /// History explorer: metadata (block count, char count).
    pub history_meta: Style,
    /// History explorer: duplicate badge.
    pub history_duplicate: Style,
    /// History explorer: expanded block type tag.
    pub history_block_tag: Style,
    /// History explorer: expanded block content.
    pub history_block_content: Style,
    /// History explorer: streaming indicator.
    pub history_streaming: Style,
}
```

- [ ] **Step 2: Initialize styles in `default_theme()`**

```rust
history_header: bold,
history_selected: Style::default().add_modifier(Modifier::REVERSED),
history_index: dim,
history_role_user: bold.fg(Color::Green),
history_role_assistant: bold.fg(Color::Blue),
history_role_tool: bold.fg(Color::Yellow),
history_summary: Style::default(),
history_meta: dim,
history_duplicate: bold.fg(Color::Red),
history_block_tag: bold.fg(Color::Cyan),
history_block_content: dim,
history_streaming: dim.fg(Color::Blue),
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p ox-cli 2>&1 | head -10`
Expected: No errors (warnings about unused fields are fine until history_view.rs uses them).

- [ ] **Step 4: Commit**

```
git add crates/ox-cli/src/theme.rs
git commit -m "feat(ox-cli): history explorer theme styles"
```

---

### Task 7: History View — Rendering

**Files:**
- Create: `crates/ox-cli/src/history_view.rs`
- Modify: `crates/ox-cli/src/tui.rs`

- [ ] **Step 1: Create `history_view.rs`**

```rust
use crate::parse::{parse_history_entries, EntryFlags, HistoryBlock, HistoryEntry};
use crate::theme::Theme;
use crate::view_state::ViewState;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};
use structfs_core_store::Value;

/// Render the history explorer screen.
///
/// Returns `(content_height, viewport_height)` for scroll_max feedback.
pub fn draw_history(
    frame: &mut Frame,
    vs: &ViewState,
    theme: &Theme,
    area: Rect,
) -> (usize, usize) {
    let entries = parse_history_entries(&vs.raw_messages);
    let entry_count = entries.len();

    // Extract snapshot state
    let (selected_row, scroll, expanded) = match &vs.ui.screen {
        ox_types::ScreenSnapshot::History(snap) => {
            let sel = snap.selected_row.min(entry_count.saturating_sub(1));
            let expanded_set: std::collections::HashSet<usize> =
                snap.expanded.iter().copied().collect();
            (sel, snap.scroll, expanded_set)
        }
        _ => (0, 0, std::collections::HashSet::new()),
    };

    let thread_id = match &vs.ui.screen {
        ox_types::ScreenSnapshot::History(snap) => &snap.thread_id,
        _ => "",
    };

    let mut lines: Vec<Line> = Vec::new();

    // Header
    lines.push(Line::from(vec![
        Span::styled(" HISTORY ", theme.history_header),
        Span::styled(
            format!(" {} messages", entry_count),
            theme.history_meta,
        ),
        Span::styled(
            format!("  {}", truncate_id(thread_id, 16)),
            theme.history_meta,
        ),
    ]));
    lines.push(Line::from(""));

    // Message list
    for entry in &entries {
        let is_selected = entry.index == selected_row;
        let is_expanded = expanded.contains(&entry.index);

        // Summary line
        let cursor = if is_selected { ">" } else { " " };
        let role_style = match entry.role.as_str() {
            "user" => {
                if entry.blocks.iter().any(|b| b.block_type == "tool_result") {
                    theme.history_role_tool
                } else {
                    theme.history_role_user
                }
            }
            "assistant" => theme.history_role_assistant,
            _ => theme.history_meta,
        };

        let role_label = if entry
            .blocks
            .iter()
            .any(|b| b.block_type == "tool_result")
        {
            "tool_result"
        } else {
            &entry.role
        };

        let mut spans = vec![
            Span::styled(
                cursor.to_string(),
                if is_selected {
                    theme.history_selected
                } else {
                    theme.history_summary
                },
            ),
            Span::styled(format!("#{:<3} ", entry.index), theme.history_index),
            Span::styled(format!("{:<12}", role_label), role_style),
        ];

        // Duplicate badge
        if entry.flags.duplicate_content {
            spans.push(Span::styled(
                format!(
                    "[DUP of #{}] ",
                    entry.flags.duplicate_of.unwrap_or(0)
                ),
                theme.history_duplicate,
            ));
        }

        // Summary text (truncated)
        let summary = if entry.summary.len() > 60 {
            format!("{}...", &entry.summary[..60])
        } else {
            entry.summary.clone()
        };
        spans.push(Span::styled(
            format!("\"{summary}\""),
            if is_selected {
                theme.history_selected
            } else {
                theme.history_summary
            },
        ));

        // Metadata
        spans.push(Span::styled(
            format!(
                " ({} block{}, {} chars)",
                entry.block_count,
                if entry.block_count == 1 { "" } else { "s" },
                entry.text_len
            ),
            theme.history_meta,
        ));

        lines.push(Line::from(spans));

        // Expanded detail
        if is_expanded {
            for block in &entry.blocks {
                render_block(&mut lines, block, theme, area.width as usize);
            }
            lines.push(Line::from(""));
        }
    }

    // Streaming indicator
    if vs.turn.thinking {
        lines.push(Line::from(Span::styled(
            "  [streaming...]",
            theme.history_streaming,
        )));
    }

    // Render with scrolling
    let viewport_width = area.width as usize;
    let content_height = count_wrapped_lines(&lines, viewport_width);
    let viewport_height = area.height as usize;
    let max_scroll = content_height.saturating_sub(viewport_height);

    let computed_scroll = if scroll == 0 {
        max_scroll as u16
    } else {
        (max_scroll as u16).saturating_sub(scroll as u16)
    };

    let text = Text::from(lines);
    let widget = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .scroll((computed_scroll, 0));
    frame.render_widget(widget, area);

    // Scrollbar
    if content_height > viewport_height {
        let scroll_position = max_scroll.saturating_sub(scroll);
        let mut scrollbar_state = ScrollbarState::new(max_scroll).position(scroll_position);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }

    (content_height, viewport_height)
}

fn render_block(lines: &mut Vec<Line>, block: &HistoryBlock, theme: &Theme, _width: usize) {
    match block.block_type.as_str() {
        "text" => {
            let text = block.text.as_deref().unwrap_or("");
            lines.push(Line::from(vec![
                Span::raw("      "),
                Span::styled("[text] ", theme.history_block_tag),
            ]));
            for line in text.lines() {
                lines.push(Line::from(vec![
                    Span::raw("        "),
                    Span::styled(line.to_string(), theme.history_block_content),
                ]));
            }
        }
        "tool_use" => {
            let name = block.tool_name.as_deref().unwrap_or("?");
            let id = block
                .tool_use_id
                .as_ref()
                .map(|i| truncate_id(i, 16))
                .unwrap_or_default();
            lines.push(Line::from(vec![
                Span::raw("      "),
                Span::styled("[tool_use] ", theme.history_block_tag),
                Span::styled(format!("{name} "), theme.history_role_tool),
                Span::styled(format!("id: {id}"), theme.history_meta),
            ]));
            if let Some(input) = &block.input_json {
                for line in input.lines() {
                    lines.push(Line::from(vec![
                        Span::raw("        "),
                        Span::styled(line.to_string(), theme.history_block_content),
                    ]));
                }
            }
        }
        "tool_result" => {
            let id = block
                .tool_use_id
                .as_ref()
                .map(|i| truncate_id(i, 16))
                .unwrap_or_default();
            lines.push(Line::from(vec![
                Span::raw("      "),
                Span::styled("[tool_result] ", theme.history_block_tag),
                Span::styled(format!("id: {id}"), theme.history_meta),
            ]));
            if let Some(text) = &block.text {
                let preview_lines: Vec<&str> = text.lines().take(10).collect();
                let total = text.lines().count();
                for pl in &preview_lines {
                    lines.push(Line::from(vec![
                        Span::raw("        "),
                        Span::styled(pl.to_string(), theme.history_block_content),
                    ]));
                }
                if total > 10 {
                    lines.push(Line::from(vec![
                        Span::raw("        "),
                        Span::styled(
                            format!("... ({} more lines)", total - 10),
                            theme.history_meta,
                        ),
                    ]));
                }
            }
        }
        _ => {}
    }
}

fn truncate_id(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...", &s[..max])
    } else {
        s.to_string()
    }
}

/// Count total rendered lines after word wrapping (same as thread_view).
fn count_wrapped_lines(lines: &[Line], width: usize) -> usize {
    if width == 0 {
        return lines.len();
    }
    lines
        .iter()
        .map(|line| {
            let w: usize = line.spans.iter().map(|s| s.content.len()).sum();
            if w == 0 { 1 } else { w.div_ceil(width) }
        })
        .sum()
}
```

- [ ] **Step 2: Register `history_view` module in `main.rs`**

Check if `crates/ox-cli/src/main.rs` has module declarations. Add:

```rust
mod history_view;
```

(Or in the appropriate `mod` declaration file — check where other view modules like `thread_view`, `inbox_view` are declared.)

- [ ] **Step 3: Wire into `tui.rs` — add the History arm to `draw()`**

In the content area match in `tui.rs` `draw()` function (~line 72), add:

```rust
ScreenSnapshot::History(_snap) => {
    let (ch, vh) = crate::history_view::draw_history(frame, vs, theme, content_area);
    content_height = Some(ch);
    // viewport_height is set from the returned value
    let _ = vh;
}
```

The full match should be:

```rust
match &vs.ui.screen {
    ScreenSnapshot::Settings(_) => {
        crate::settings_view::draw_settings(frame, settings, theme, content_area);
    }
    ScreenSnapshot::Thread(snap) => {
        let view = crate::types::ThreadView {
            messages: vs.messages.clone(),
            thinking: vs.turn.thinking,
        };
        content_height = Some(crate::thread_view::draw_thread(
            frame,
            &view,
            snap.scroll as u16,
            theme,
            content_area,
        ));
    }
    ScreenSnapshot::Inbox(_) => {
        crate::inbox_view::draw_inbox(frame, vs, theme, content_area);
    }
    ScreenSnapshot::History(_) => {
        let (ch, _vh) = crate::history_view::draw_history(frame, vs, theme, content_area);
        content_height = Some(ch);
    }
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p ox-cli 2>&1 | head -20`
Expected: No errors.

- [ ] **Step 5: Commit**

```
git add crates/ox-cli/src/history_view.rs crates/ox-cli/src/tui.rs
git commit -m "feat(ox-cli): history explorer rendering"
```

(If the module declaration is in main.rs, include that file too.)

---

### Task 8: Event Loop — Wire History Screen

**Files:**
- Modify: `crates/ox-cli/src/event_loop.rs`

- [ ] **Step 1: Add scroll feedback for history screen**

In the post-draw section (~line 139), after the thread scroll feedback block, add a similar block for history:

```rust
if matches!(&ui.screen, ScreenSnapshot::History(_)) && viewport_height > 0 {
    let scroll_max = content_height.unwrap_or(0).saturating_sub(viewport_height);
    let _ = client
        .write_typed(
            &oxpath!("ui"),
            &UiCommand::History(HistoryCommand::SetScrollMax { max: scroll_max }),
        )
        .await;
    let _ = client
        .write_typed(
            &oxpath!("ui"),
            &UiCommand::History(HistoryCommand::SetViewportHeight {
                height: viewport_height,
            }),
        )
        .await;
}
```

This requires importing `HistoryCommand` — add it to the use statement at the top:

```rust
use ox_types::{
    GlobalCommand, HistoryCommand, InboxCommand, InputKeyEvent, InsertContext, Mode,
    PendingAction, Screen, ScreenSnapshot, ThreadCommand, UiCommand, UiSnapshot,
};
```

- [ ] **Step 2: Add History screen to the key dispatch path**

In the key event handling (~line 243), the current code checks for `ScreenSnapshot::Thread` specially (for approval handling). The history screen uses normal InputStore dispatch. Add it to the general dispatch path.

After the `else if let ScreenSnapshot::Thread(snap) = &ui.screen {` block, before the final `else` clause, the history screen is already handled by the fallthrough — key events go through `dispatch_key`, which sends to InputStore, which resolves the binding, which writes to UiStore. No special handling needed since history has no approval dialog, no editor, no customize dialog.

However, the `dispatch_key` function's screen-specific handling needs a `ScreenSnapshot::History` arm. In `dispatch_key` (~line 380), add to the match:

```rust
ScreenSnapshot::History(_) => {
    // No screen-specific key handling — all goes through InputStore bindings
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p ox-cli 2>&1 | head -20`
Expected: No errors.

- [ ] **Step 4: Run the full test suite**

Run: `cargo test -p ox-cli 2>&1 | tail -20`
Expected: All tests pass.

- [ ] **Step 5: Commit**

```
git add crates/ox-cli/src/event_loop.rs
git commit -m "feat(ox-cli): wire history screen into event loop"
```

---

### Task 9: Compile Check and Fix-Up

**Files:**
- Various — fix any compilation issues from the previous tasks

- [ ] **Step 1: Full workspace check**

Run: `cargo check 2>&1`

Address any errors. Common issues to expect:
- Missing imports (e.g., `HistoryCommand` in event_loop.rs, `HistorySnapshot` in snapshot.rs)
- Non-exhaustive match arms in code that matches on `ScreenSnapshot` or `Screen` outside the files we modified (check `tab_bar.rs`, `inbox_shell.rs`, etc.)
- Missing module declaration for `history_view`

- [ ] **Step 2: Fix non-exhaustive matches**

Search for match arms on `ScreenSnapshot` in ox-cli:

Run: `grep -rn "ScreenSnapshot" crates/ox-cli/src/ | grep -v "test" | grep -v "history_view"`

For each match, add the `History` arm. Typical patterns:

In `tab_bar.rs` (tab display):
```rust
ScreenSnapshot::History(s) => {
    // Show "history" tab or combine with thread tab
}
```

In `thread_shell.rs` or `inbox_shell.rs` if they check screen type:
```rust
ScreenSnapshot::History(_) => { /* no-op */ }
```

- [ ] **Step 3: Run clippy**

Run: `cargo clippy -p ox-cli 2>&1 | head -30`
Fix any clippy warnings in new code.

- [ ] **Step 4: Format**

Run: `./scripts/fmt.sh`

- [ ] **Step 5: Run quality gates**

Run: `cargo test 2>&1 | tail -20`
Expected: All tests pass across the workspace.

- [ ] **Step 6: Commit**

```
git add -A
git commit -m "fix: compilation fixes for history explorer integration"
```

---

### Task 10: Manual Smoke Test

- [ ] **Step 1: Build and run**

Run: `cargo run -p ox-cli`

- [ ] **Step 2: Test the flow**

1. Create or open a thread (Enter on inbox, or `c` to compose)
2. Send a message
3. Press `Esc` to enter normal mode on the thread
4. Press `h` to open the history explorer
5. Verify: header shows "HISTORY N messages", messages are listed with index, role, summary, metadata
6. Press `j`/`k` to navigate — cursor indicator moves
7. Press `Enter` on a message — content blocks expand inline
8. Press `Enter` again — collapses
9. Press `Esc` or `q` — returns to the thread view (not inbox)
10. Send another message, press `h` again — verify new message appears (real-time updates)

- [ ] **Step 3: Test duplicate detection**

If you can trigger the looping output bug, open the history explorer and verify that duplicate messages show the `[DUP of #N]` badge in red.

- [ ] **Step 4: Commit any fixes**

If any issues were found during smoke testing, fix and commit:
```
git add -A
git commit -m "fix: history explorer smoke test fixes"
```

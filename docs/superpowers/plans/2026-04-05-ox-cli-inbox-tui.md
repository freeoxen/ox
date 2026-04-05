# ox-cli Inbox TUI Rendering (Plan 2b)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add vim-style modal input, inbox thread list rendering, live search with compounding filter chips, and visual differentiation to ox-cli's TUI.

**Architecture:** Add `InputMode` enum (Normal/Insert/Approval) to App state. Split tui.rs into focused modules: tui.rs (event loop + mode dispatch), inbox_view.rs (thread list), thread_view.rs (conversation), tab_bar.rs (tabs). The input box only renders in Insert mode. Search uses live-filtering with numbered chips that compound as AND filters.

**Tech Stack:** Rust (edition 2024), ratatui 0.29, crossterm 0.28

**Spec:** `docs/superpowers/specs/2026-04-05-inbox-tui-design.md`

**Depends on:** Plan 2a (multi-thread state + AgentPool — committed as `9ea9024`)

---

### File Structure

| File | Responsibility |
|------|---------------|
| `crates/ox-cli/src/app.rs` | **MODIFY.** Add InputMode, InsertContext, SearchState, selected_row |
| `crates/ox-cli/src/tui.rs` | **REWRITE.** Event loop with mode-aware key dispatch, top-level draw routing |
| `crates/ox-cli/src/inbox_view.rs` | **NEW.** Inbox thread list rendering — 2-line rows with state dots |
| `crates/ox-cli/src/thread_view.rs` | **NEW.** Conversation rendering (extracted from current tui.rs draw code) |
| `crates/ox-cli/src/tab_bar.rs` | **NEW.** Tab bar widget |
| `crates/ox-cli/src/main.rs` | **MODIFY.** Add new module declarations |

---

### Task 1: InputMode + State Model

**Files:**
- Modify: `crates/ox-cli/src/app.rs`

Add the modal state types and integrate them into App.

- [ ] **Step 1: Add InputMode, InsertContext, SearchState types**

Add to `crates/ox-cli/src/app.rs` after the existing type definitions:

```rust
/// What the user is doing in Insert mode.
#[derive(Debug, Clone, PartialEq)]
pub enum InsertContext {
    /// Composing a new thread from the inbox.
    Compose,
    /// Replying to the active thread.
    Reply,
    /// Searching/filtering the inbox.
    Search,
}

/// The current input mode (vim-style).
#[derive(Debug, Clone, PartialEq)]
pub enum InputMode {
    /// Navigate, scan, manage. No input box visible.
    Normal,
    /// Typing a message or search query. Input box visible.
    Insert(InsertContext),
}

impl Default for InputMode {
    fn default() -> Self {
        InputMode::Normal
    }
}

/// Live search state with compounding filter chips.
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    /// Saved filter chips — each is a search fragment. All compound as AND.
    pub chips: Vec<String>,
    /// The live query being typed (filters in real-time).
    pub live_query: String,
}

impl SearchState {
    /// Save the current live query as a numbered chip.
    pub fn save_chip(&mut self) {
        let q = self.live_query.trim().to_string();
        if !q.is_empty() {
            self.chips.push(q);
            self.live_query.clear();
        }
    }

    /// Dismiss chip by 1-based index.
    pub fn dismiss_chip(&mut self, idx: usize) {
        if idx > 0 && idx <= self.chips.len() {
            self.chips.remove(idx - 1);
        }
    }

    /// Whether any filters are active.
    pub fn is_active(&self) -> bool {
        !self.chips.is_empty() || !self.live_query.is_empty()
    }

    /// Check if a thread matches all active filters (chips + live query).
    /// Matches against title, labels, and state name.
    pub fn matches(&self, title: &str, labels: &[String], state: &str) -> bool {
        let haystack = format!(
            "{} {} {}",
            title.to_lowercase(),
            labels.join(" ").to_lowercase(),
            state.to_lowercase()
        );
        for chip in &self.chips {
            if !haystack.contains(&chip.to_lowercase()) {
                return false;
            }
        }
        if !self.live_query.is_empty() {
            if !haystack.contains(&self.live_query.to_lowercase()) {
                return false;
            }
        }
        true
    }
}
```

- [ ] **Step 2: Add modal state fields to App**

Add to the App struct:

```rust
pub struct App {
    // ... existing fields ...
    pub mode: InputMode,
    pub search: SearchState,
    pub selected_row: usize,  // selected inbox row index
}
```

Initialize in `App::new()`:
```rust
mode: InputMode::Normal,
search: SearchState::default(),
selected_row: 0,
```

- [ ] **Step 3: Add mode transition methods to App**

```rust
impl App {
    /// Enter Insert mode for composing a new thread.
    pub fn enter_compose(&mut self) {
        self.mode = InputMode::Insert(InsertContext::Compose);
    }

    /// Enter Insert mode for replying to the active thread.
    pub fn enter_reply(&mut self) {
        if self.active_thread.is_some() {
            self.mode = InputMode::Insert(InsertContext::Reply);
        }
    }

    /// Enter Insert mode for searching.
    pub fn enter_search(&mut self) {
        self.mode = InputMode::Insert(InsertContext::Search);
    }

    /// Exit Insert mode back to Normal. Draft is preserved.
    pub fn exit_insert(&mut self) {
        self.mode = InputMode::Normal;
    }

    /// Send the current input based on context.
    pub fn send_input(&mut self) {
        match &self.mode {
            InputMode::Insert(InsertContext::Search) => {
                self.search.save_chip();
                // Stay in search insert mode for next fragment
            }
            InputMode::Insert(InsertContext::Compose) => {
                if !self.input.is_empty() {
                    let input = std::mem::take(&mut self.input);
                    self.cursor = 0;
                    self.input_history.push(input.clone());
                    self.history_cursor = self.input_history.len();
                    let title: String = input.chars().take(40).collect();
                    match self.pool.create_thread(&title) {
                        Ok(tid) => {
                            let mut view = ThreadView::default();
                            view.messages.push(ChatMessage::User(input.clone()));
                            view.thinking = true;
                            self.thread_views.insert(tid.clone(), view);
                            self.pool.send_prompt(&tid, input).ok();
                        }
                        Err(e) => eprintln!("failed to create thread: {e}"),
                    }
                    // Stay in inbox, back to Normal
                    self.mode = InputMode::Normal;
                }
            }
            InputMode::Insert(InsertContext::Reply) => {
                if !self.input.is_empty() {
                    if let Some(ref tid) = self.active_thread {
                        let input = std::mem::take(&mut self.input);
                        self.cursor = 0;
                        self.input_history.push(input.clone());
                        self.history_cursor = self.input_history.len();
                        let view = self.thread_views.entry(tid.clone()).or_default();
                        view.messages.push(ChatMessage::User(input.clone()));
                        view.thinking = true;
                        self.scroll = 0;
                        self.pool.send_prompt(tid, input).ok();
                    }
                    self.mode = InputMode::Normal;
                }
            }
            InputMode::Normal => {
                // Normal mode Enter with draft: send based on view context
                if !self.input.is_empty() {
                    if self.active_thread.is_some() {
                        self.mode = InputMode::Insert(InsertContext::Reply);
                        self.send_input();
                    } else {
                        self.mode = InputMode::Insert(InsertContext::Compose);
                        self.send_input();
                    }
                }
            }
            _ => {}
        }
    }
}
```

- [ ] **Step 4: Update the old `submit()` method**

The old `submit()` is replaced by `send_input()`. Remove the old `submit()` method entirely.

- [ ] **Step 5: Verify compilation**

Run: `cargo check -p ox-cli`

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/app.rs
git commit -m "feat(ox-cli): InputMode + SearchState for modal TUI"
```

---

### Task 2: Mode-Aware Key Dispatch (tui.rs rewrite)

**Files:**
- Rewrite: `crates/ox-cli/src/tui.rs`

Replace the current key handling with mode-aware dispatch. The event loop stays the same; only the key handling and draw routing change.

- [ ] **Step 1: Rewrite key handling in tui.rs**

The main event loop (`run()`) stays mostly the same. Replace key dispatch:

```rust
pub fn run(
    app: &mut App,
    theme: &Theme,
    terminal: &mut ratatui::DefaultTerminal,
) -> std::io::Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, app, theme))?;

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    if app.pending_customize.is_some() {
                        handle_customize_key(app, key.code);
                    } else if app.pending_approval.is_some() {
                        handle_approval_key(app, key.code);
                    } else {
                        match &app.mode {
                            InputMode::Normal => handle_normal_key(app, key.modifiers, key.code),
                            InputMode::Insert(ctx) => {
                                handle_insert_key(app, ctx.clone(), key.modifiers, key.code)
                            }
                        }
                    }
                }
                Event::Mouse(mouse) => handle_mouse(app, mouse.kind, mouse.row),
                _ => {}
            }
        }

        while let Ok(event) = app.event_rx.try_recv() {
            app.handle_event(event);
        }

        if app.pending_approval.is_none() && app.pending_customize.is_none() {
            if let Ok(AppControl::PermissionRequest {
                thread_id, tool, input_preview, respond,
            }) = app.control_rx.try_recv() {
                app.open_thread(thread_id.clone());
                app.pending_approval = Some(ApprovalState {
                    thread_id, tool, input_preview, selected: 0, respond,
                });
            }
        }

        if app.should_quit {
            return Ok(());
        }
    }
}
```

- [ ] **Step 2: Implement Normal mode key handler**

```rust
fn handle_normal_key(app: &mut App, modifiers: KeyModifiers, code: KeyCode) {
    match (modifiers, code) {
        // Quit / navigate up
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
            if app.active_thread.is_some() {
                app.go_to_inbox();
            } else {
                app.should_quit = true;
            }
        }
        (_, KeyCode::Esc) => {
            if app.active_thread.is_some() {
                app.go_to_inbox();
            }
        }

        // Enter Insert mode
        (_, KeyCode::Char('i')) => {
            if app.active_thread.is_some() {
                app.enter_reply();
            } else {
                app.enter_compose();
            }
        }
        (_, KeyCode::Char('/')) => {
            if app.active_thread.is_none() {
                app.enter_search();
            }
        }

        // Navigation
        (_, KeyCode::Char('j')) | (_, KeyCode::Down) => {
            if app.active_thread.is_none() {
                app.selected_row = app.selected_row.saturating_add(1);
            } else {
                app.scroll = app.scroll.saturating_add(1);
            }
        }
        (_, KeyCode::Char('k')) | (_, KeyCode::Up) => {
            if app.active_thread.is_none() {
                app.selected_row = app.selected_row.saturating_sub(1);
            } else {
                app.scroll = app.scroll.saturating_add(1); // scroll up = increase offset
            }
        }

        // Inbox actions
        (_, KeyCode::Enter) => {
            if !app.input.is_empty() {
                // Draft exists — send it
                app.send_input();
            } else if app.active_thread.is_none() {
                // Open selected thread
                // Thread ID lookup handled by inbox_view (deferred to Task 3)
                app.open_selected_thread();
            }
        }
        (_, KeyCode::Char('d')) => {
            if app.active_thread.is_none() {
                app.archive_selected_thread();
            }
        }

        // Dismiss search filter chips (1-9)
        (_, KeyCode::Char(c)) if c.is_ascii_digit() && c != '0' && app.active_thread.is_none() => {
            let idx = (c as u8 - b'0') as usize;
            app.search.dismiss_chip(idx);
        }

        // Tab management
        (KeyModifiers::CONTROL, KeyCode::Char('t')) => app.go_to_inbox(),
        (KeyModifiers::CONTROL, KeyCode::Char('w')) => app.close_current_tab(),
        (KeyModifiers::CONTROL, KeyCode::Right) => app.next_tab(),
        (KeyModifiers::CONTROL, KeyCode::Left) => app.prev_tab(),

        // Approval quick keys (in thread view with pending approval)
        (_, KeyCode::Char('y')) if app.active_thread.is_some() => {
            try_quick_approve(app, ApprovalResponse::AllowOnce);
        }
        (_, KeyCode::Char('n')) if app.active_thread.is_some() => {
            try_quick_approve(app, ApprovalResponse::DenyOnce);
        }
        (_, KeyCode::Char('s')) if app.active_thread.is_some() => {
            try_quick_approve(app, ApprovalResponse::AllowSession);
        }
        (_, KeyCode::Char('a')) if app.active_thread.is_some() => {
            try_quick_approve(app, ApprovalResponse::AllowAlways);
        }

        _ => {}
    }
}

fn try_quick_approve(app: &mut App, response: ApprovalResponse) {
    if let Some(approval) = app.pending_approval.take() {
        approval.respond.send(response).ok();
    }
}
```

- [ ] **Step 3: Implement Insert mode key handler**

```rust
fn handle_insert_key(
    app: &mut App,
    ctx: InsertContext,
    modifiers: KeyModifiers,
    code: KeyCode,
) {
    match (modifiers, code) {
        // Send
        (KeyModifiers::CONTROL, KeyCode::Enter) | (KeyModifiers::CONTROL, KeyCode::Char('s')) => {
            app.send_input();
        }
        // Exit Insert mode (draft preserved)
        (_, KeyCode::Esc) => {
            app.exit_insert();
        }
        // Newline (Enter is newline in Insert mode)
        (_, KeyCode::Enter) => {
            if ctx == InsertContext::Search {
                // In search: Enter saves chip
                app.send_input();
            } else {
                app.input.insert(app.cursor, '\n');
                app.cursor += 1;
            }
        }
        // Editing
        (_, KeyCode::Backspace) => {
            if ctx == InsertContext::Search {
                // Backspace in search modifies live_query
                app.search.live_query.pop();
            } else if app.cursor > 0 {
                app.cursor -= 1;
                app.input.remove(app.cursor);
            }
        }
        (_, KeyCode::Left) => {
            if ctx != InsertContext::Search {
                app.cursor = app.cursor.saturating_sub(1);
            }
        }
        (_, KeyCode::Right) => {
            if ctx != InsertContext::Search && app.cursor < app.input.len() {
                app.cursor += 1;
            }
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
            if ctx == InsertContext::Search {
                app.search.live_query.clear();
            } else {
                app.input.clear();
                app.cursor = 0;
            }
        }
        (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
            if ctx != InsertContext::Search {
                app.cursor = 0;
            }
        }
        (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
            if ctx != InsertContext::Search {
                app.cursor = app.input.len();
            }
        }
        // Character input
        (_, KeyCode::Char(c)) => {
            if ctx == InsertContext::Search {
                app.search.live_query.push(c);
            } else {
                app.input.insert(app.cursor, c);
                app.cursor += 1;
            }
        }
        _ => {}
    }
}
```

- [ ] **Step 4: Add helper methods to App for inbox actions**

In `app.rs`, add:

```rust
impl App {
    /// Open the thread at the currently selected inbox row.
    /// Requires mapping selected_row to a thread_id via the filtered list.
    pub fn open_selected_thread(&mut self) {
        if let Some(tid) = self.get_selected_thread_id() {
            self.open_thread(tid);
        }
    }

    /// Archive the thread at the currently selected inbox row.
    pub fn archive_selected_thread(&mut self) {
        if let Some(tid) = self.get_selected_thread_id() {
            // Update inbox state via ox-inbox store
            let mut map = std::collections::BTreeMap::new();
            map.insert(
                "inbox_state".to_string(),
                ox_kernel::Value::String("done".to_string()),
            );
            let update_path =
                ox_kernel::Path::parse(&format!("threads/{}", tid)).unwrap_or_default();
            self.pool
                .inbox()
                .write(&update_path, ox_kernel::Record::parsed(ox_kernel::Value::Map(map)))
                .ok();
        }
    }

    /// Get the thread_id for the currently selected inbox row.
    /// This reads from ox-inbox and filters by SearchState.
    fn get_selected_thread_id(&mut self) -> Option<String> {
        let threads = self.get_visible_threads();
        threads.get(self.selected_row).map(|(id, _)| id.clone())
    }

    /// Get the list of visible threads (filtered by search).
    /// Returns Vec<(thread_id, title)>.
    pub fn get_visible_threads(&mut self) -> Vec<(String, String)> {
        // Read thread list from inbox store
        let record = self
            .pool
            .inbox()
            .read(&ox_kernel::path!("threads"))
            .ok()
            .flatten();
        let Some(record) = record else {
            return vec![];
        };
        let Some(ox_kernel::Value::Array(threads)) = record.as_value() else {
            return vec![];
        };

        let mut result = Vec::new();
        for thread_val in threads {
            let ox_kernel::Value::Map(ref map) = thread_val else {
                continue;
            };
            let id = match map.get("id") {
                Some(ox_kernel::Value::String(s)) => s.clone(),
                _ => continue,
            };
            let title = match map.get("title") {
                Some(ox_kernel::Value::String(s)) => s.clone(),
                _ => "untitled".to_string(),
            };
            let labels: Vec<String> = match map.get("labels") {
                Some(ox_kernel::Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|v| match v {
                        ox_kernel::Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => vec![],
            };
            let state = match map.get("thread_state") {
                Some(ox_kernel::Value::String(s)) => s.clone(),
                _ => "running".to_string(),
            };

            if self.search.matches(&title, &labels, &state) {
                result.push((id, title));
            }
        }
        result
    }
}
```

- [ ] **Step 5: Stub the draw function**

For now, keep the existing `draw()` function working (it will be split in Tasks 3-5). Just make sure it compiles with the new key handlers.

- [ ] **Step 6: Verify compilation**

Run: `cargo check -p ox-cli`

- [ ] **Step 7: Commit**

```bash
git add crates/ox-cli/src/tui.rs crates/ox-cli/src/app.rs
git commit -m "feat(ox-cli): mode-aware key dispatch — Normal + Insert modes"
```

---

### Task 3: Inbox View Rendering

**Files:**
- Create: `crates/ox-cli/src/inbox_view.rs`
- Modify: `crates/ox-cli/src/main.rs` (add module)

- [ ] **Step 1: Create `crates/ox-cli/src/inbox_view.rs`**

```rust
use crate::app::App;
use crate::theme::Theme;
use ox_kernel::Value;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Render the inbox thread list into the given area.
pub fn draw_inbox(frame: &mut Frame, app: &mut App, theme: &Theme, area: Rect) {
    let threads = read_inbox_threads(app);
    let mut lines: Vec<Line> = Vec::new();

    if threads.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  No threads. Press i to start one.",
            theme.assistant_text,
        )));
    }

    for (idx, thread) in threads.iter().enumerate() {
        let is_selected = idx == app.selected_row;
        let bg = if is_selected {
            theme.selected_bg
        } else {
            ratatui::style::Style::default()
        };

        // Line 1: state dot + title + recency
        let state_span = state_span(&thread.state, theme);
        let title_style = if thread.state == "completed" {
            theme.tool_meta
        } else {
            theme.user_text
        };

        lines.push(Line::from(vec![
            state_span,
            Span::raw(" "),
            Span::styled(&thread.title, title_style.patch(bg)),
        ]));

        // Line 2: labels + activity + tasks + tokens
        let mut meta_spans: Vec<Span> = vec![Span::raw("  ")];
        for label in &thread.labels {
            meta_spans.push(Span::styled(
                format!("[{}] ", label),
                theme.tool_name.patch(bg),
            ));
        }
        if !thread.activity.is_empty() {
            meta_spans.push(Span::styled(&thread.activity, theme.tool_meta.patch(bg)));
            meta_spans.push(Span::raw(" "));
        }
        if thread.task_done > 0 || thread.task_total > 0 {
            meta_spans.push(Span::styled(
                format!("☑ {}/{} ", thread.task_done, thread.task_total),
                theme.tool_meta.patch(bg),
            ));
        }
        if thread.token_count > 0 {
            let tok_str = if thread.token_count >= 1000 {
                format!("{:.1}k tok", thread.token_count as f64 / 1000.0)
            } else {
                format!("{} tok", thread.token_count)
            };
            meta_spans.push(Span::styled(tok_str, theme.tool_meta.patch(bg)));
        }
        lines.push(Line::from(meta_spans));
    }

    // Clamp selected_row
    if !threads.is_empty() {
        app.selected_row = app.selected_row.min(threads.len() - 1);
    }

    let text = ratatui::text::Text::from(lines);
    let paragraph = Paragraph::new(text);
    frame.render_widget(paragraph, area);
}

/// Render the search/filter bar (if active).
pub fn draw_filter_bar(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let mut spans: Vec<Span> = vec![Span::styled("/ ", theme.user_prompt)];
    for (i, chip) in app.search.chips.iter().enumerate() {
        spans.push(Span::styled(
            format!("[{}: {}] ", i + 1, chip),
            theme.tool_name,
        ));
    }
    if !app.search.live_query.is_empty() {
        spans.push(Span::styled(&app.search.live_query, theme.user_text));
    }
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

fn state_span<'a>(state: &str, theme: &'a Theme) -> Span<'a> {
    match state {
        "running" => Span::styled("● RUNNING", theme.approval_allow),
        "blocked_on_approval" => Span::styled("● BLOCKED", theme.approval_deny),
        "waiting_for_input" => Span::styled("● WAITING", theme.approval_title),
        "errored" => Span::styled("● ERRORED", theme.error),
        "completed" => Span::styled("● DONE", theme.tool_meta),
        _ => Span::styled("● ???", theme.tool_meta),
    }
}

/// A thread summary extracted from ox-inbox.
struct InboxThread {
    id: String,
    title: String,
    state: String,
    labels: Vec<String>,
    activity: String,
    task_done: u32,
    task_total: u32,
    token_count: i64,
}

/// Read threads from inbox store, filtered by search state.
fn read_inbox_threads(app: &mut App) -> Vec<InboxThread> {
    let record = app
        .pool
        .inbox()
        .read(&ox_kernel::path!("threads"))
        .ok()
        .flatten();
    let Some(record) = record else {
        return vec![];
    };
    let Some(Value::Array(threads)) = record.as_value() else {
        return vec![];
    };

    let mut result = Vec::new();
    for thread_val in threads {
        let Value::Map(ref map) = thread_val else {
            continue;
        };
        let get_str = |key: &str| match map.get(key) {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        let id = get_str("id");
        let title = get_str("title");
        let state = get_str("thread_state");
        if state.is_empty() {
            continue;
        }
        let labels: Vec<String> = match map.get("labels") {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        };
        let token_count = match map.get("token_count") {
            Some(Value::Integer(n)) => *n,
            _ => 0,
        };

        // Check if thread matches search filters
        if !app.search.matches(&title, &labels, &state) {
            continue;
        }

        // Infer activity from ThreadView if we have one
        let activity = if let Some(view) = app.thread_views.get(&id) {
            if view.thinking {
                "streaming...".to_string()
            } else if !view.messages.is_empty() {
                String::new()
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        result.push(InboxThread {
            id,
            title,
            state,
            labels,
            activity,
            task_done: 0,
            task_total: 0,
            token_count,
        });
    }

    // Sort: urgency first, then by state priority
    result.sort_by(|a, b| {
        state_priority(&a.state).cmp(&state_priority(&b.state))
    });

    result
}

fn state_priority(state: &str) -> u8 {
    match state {
        "blocked_on_approval" => 0,
        "errored" => 1,
        "waiting_for_input" => 2,
        "running" => 3,
        "completed" => 4,
        _ => 5,
    }
}
```

- [ ] **Step 2: Add module declaration**

In `crates/ox-cli/src/main.rs`, add `mod inbox_view;`.

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p ox-cli`

- [ ] **Step 4: Commit**

```bash
git add crates/ox-cli/src/inbox_view.rs crates/ox-cli/src/main.rs
git commit -m "feat(ox-cli): inbox view — 2-line thread rows with state/labels/search"
```

---

### Task 4: Thread View + Tab Bar (extracted from tui.rs)

**Files:**
- Create: `crates/ox-cli/src/thread_view.rs`
- Create: `crates/ox-cli/src/tab_bar.rs`
- Modify: `crates/ox-cli/src/main.rs` (add modules)

- [ ] **Step 1: Create `crates/ox-cli/src/thread_view.rs`**

Extract the conversation rendering from the current `draw()` function in tui.rs. This renders the message list for a single thread:

```rust
use crate::app::{ChatMessage, ThreadView};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Wrap};

/// Render a thread's conversation into the given area.
pub fn draw_thread(
    frame: &mut Frame,
    view: &ThreadView,
    scroll: u16,
    theme: &Theme,
    area: Rect,
) {
    let mut lines: Vec<Line> = Vec::new();

    for msg in &view.messages {
        match msg {
            ChatMessage::User(text) => {
                lines.push(Line::from(""));
                for line in text.lines() {
                    lines.push(Line::from(vec![
                        Span::styled("> ", theme.user_prompt),
                        Span::styled(line, theme.user_text),
                    ]));
                }
                lines.push(Line::from(""));
            }
            ChatMessage::AssistantChunk(text) => {
                for line in text.lines() {
                    lines.push(Line::from(Span::styled(line, theme.assistant_text)));
                }
            }
            ChatMessage::ToolCall { name } => {
                lines.push(Line::from(vec![
                    Span::styled(format!("  [{name}] "), theme.tool_name),
                    Span::styled("running...", theme.tool_running),
                ]));
            }
            ChatMessage::ToolResult { name, output } => {
                let line_count = output.lines().count();
                let preview_lines: Vec<&str> = output.lines().take(5).collect();
                lines.push(Line::from(vec![
                    Span::styled(format!("  [{name}] "), theme.tool_name),
                    Span::styled(
                        if line_count > 5 {
                            format!("({line_count} lines)")
                        } else {
                            format!(
                                "({line_count} line{})",
                                if line_count == 1 { "" } else { "s" }
                            )
                        },
                        theme.tool_meta,
                    ),
                ]));
                for pl in &preview_lines {
                    lines.push(Line::from(Span::styled(
                        format!("  | {pl}"),
                        theme.tool_output,
                    )));
                }
                if line_count > 5 {
                    lines.push(Line::from(Span::styled(
                        format!("  | ... ({} more)", line_count - 5),
                        theme.tool_overflow,
                    )));
                }
            }
            ChatMessage::Error(e) => {
                lines.push(Line::from(Span::styled(
                    format!("  error: {e}"),
                    theme.error,
                )));
            }
        }
    }

    // Thinking indicator
    if view.thinking {
        if !matches!(view.messages.last(), Some(ChatMessage::AssistantChunk(_))) {
            lines.push(Line::from(Span::styled("  ...", theme.thinking)));
        }
    }

    let text = Text::from(lines);
    let msg_height = area.height as usize;
    let total_lines = text.lines.len();
    let computed_scroll = if scroll == 0 {
        total_lines.saturating_sub(msg_height) as u16
    } else {
        let max_scroll = total_lines.saturating_sub(msg_height) as u16;
        max_scroll.saturating_sub(scroll)
    };
    let paragraph = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .scroll((computed_scroll, 0));
    frame.render_widget(paragraph, area);
}
```

- [ ] **Step 2: Create `crates/ox-cli/src/tab_bar.rs`**

```rust
use crate::app::App;
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Render the tab bar into the given area.
pub fn draw_tabs(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();

    // Inbox tab
    let thread_count = app.thread_views.len();
    let inbox_label = format!(" ■ Inbox ({}) ", thread_count);
    if app.active_thread.is_none() {
        spans.push(Span::styled(inbox_label, theme.title_badge));
    } else {
        spans.push(Span::styled(inbox_label, theme.title_info));
    }

    // Thread tabs
    for tid in &app.tabs {
        let title: String = tid.chars().take(20).collect();
        let label = format!(" {} ", title);
        if app.active_thread.as_ref() == Some(tid) {
            spans.push(Span::styled(label, theme.title_badge));
        } else {
            spans.push(Span::styled(label, theme.title_info));
        }
    }

    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}
```

- [ ] **Step 3: Add module declarations**

In `crates/ox-cli/src/main.rs`:
```rust
mod tab_bar;
mod thread_view;
```

- [ ] **Step 4: Verify compilation**

Run: `cargo check -p ox-cli`

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/thread_view.rs crates/ox-cli/src/tab_bar.rs crates/ox-cli/src/main.rs
git commit -m "feat(ox-cli): thread_view + tab_bar extracted as modules"
```

---

### Task 5: Wire Up Draw — Compose the Views

**Files:**
- Modify: `crates/ox-cli/src/tui.rs`

Replace the monolithic `draw()` with one that composes tab_bar, inbox_view/thread_view, optional input box, and status bar.

- [ ] **Step 1: Rewrite draw() to compose views**

```rust
fn draw(frame: &mut Frame, app: &mut App, theme: &Theme) {
    let is_insert = matches!(app.mode, InputMode::Insert(_));
    let show_filter_bar = app.active_thread.is_none() && app.search.is_active();

    // Layout: tab bar + optional filter bar + content + optional input + status
    let mut constraints = vec![
        Constraint::Length(1), // tab bar
    ];
    if show_filter_bar {
        constraints.push(Constraint::Length(1)); // filter bar
    }
    constraints.push(Constraint::Min(1)); // content
    if is_insert {
        constraints.push(Constraint::Length(3)); // input box
    }
    constraints.push(Constraint::Length(1)); // status

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(frame.area());

    let mut chunk_idx = 0;

    // Tab bar
    crate::tab_bar::draw_tabs(frame, app, theme, chunks[chunk_idx]);
    chunk_idx += 1;

    // Filter bar (inbox only, when search active)
    if show_filter_bar {
        crate::inbox_view::draw_filter_bar(frame, app, theme, chunks[chunk_idx]);
        chunk_idx += 1;
    }

    // Content area
    let content_area = chunks[chunk_idx];
    chunk_idx += 1;

    if let Some(view) = app.active_view().cloned() {
        // Thread view
        crate::thread_view::draw_thread(frame, &view, app.scroll, theme, content_area);
    } else {
        // Inbox view
        crate::inbox_view::draw_inbox(frame, app, theme, content_area);
    }

    // Input box (Insert mode only)
    if is_insert {
        let input_area = chunks[chunk_idx];
        chunk_idx += 1;

        let border_style = theme.title_badge; // blue accent
        let input_block = Block::default()
            .borders(Borders::TOP)
            .border_style(border_style);
        let display_input = if matches!(app.mode, InputMode::Insert(InsertContext::Search)) {
            format!("> {}", app.search.live_query)
        } else {
            format!("> {}", app.input)
        };
        let input = Paragraph::new(display_input).block(input_block);
        frame.render_widget(input, input_area);

        // Cursor
        if app.pending_approval.is_none() && app.pending_customize.is_none() {
            let cursor_x = if matches!(app.mode, InputMode::Insert(InsertContext::Search)) {
                input_area.x + app.search.live_query.len() as u16 + 2
            } else {
                input_area.x + app.cursor as u16 + 2
            };
            frame.set_cursor_position((cursor_x, input_area.y + 1));
        }
    }

    // Status bar
    let status_area = chunks[chunk_idx];
    let mode_badge = match &app.mode {
        InputMode::Normal => Span::styled(" NORMAL ", theme.title_badge),
        InputMode::Insert(_) => Span::styled(" INSERT ", theme.approval_deny_style()),
    };

    let context_info = if let Some(ref tid) = app.active_thread {
        if let Some(view) = app.thread_views.get(tid) {
            format!(
                " {}in/{}out",
                view.tokens_in, view.tokens_out
            )
        } else {
            String::new()
        }
    } else {
        let count = app.thread_views.len();
        format!(" {} threads", count)
    };

    let hints = match &app.mode {
        InputMode::Normal if app.active_thread.is_none() => {
            " i compose  Enter open  d done  / search  j/k nav"
        }
        InputMode::Normal => " i reply  y approve  Esc inbox",
        InputMode::Insert(InsertContext::Search) => " Enter save chip  Esc done  Ctrl+U clear",
        InputMode::Insert(_) => " Ctrl+Enter send  Esc cancel",
    };

    let status = Line::from(vec![
        mode_badge,
        Span::styled(context_info, theme.status),
        Span::styled(hints, theme.status),
    ]);
    frame.render_widget(Paragraph::new(status), status_area);

    // Modal overlays (unchanged)
    if let Some(ref customize) = app.pending_customize {
        draw_customize_dialog(frame, customize, theme);
    } else if let Some(ref approval) = app.pending_approval {
        draw_approval_dialog(frame, approval, theme);
    }
}
```

Note: `theme.approval_deny_style()` may not exist — use whatever orange/warning style is available in the theme, or add one. The implementer should check `crates/ox-cli/src/theme.rs` for available styles and use the closest match (e.g., `theme.approval_deny` or create a new style for the INSERT badge).

- [ ] **Step 2: Add InsertContext import to tui.rs**

```rust
use crate::app::{App, AppControl, ApprovalResponse, ApprovalState, ChatMessage, InputMode, InsertContext};
```

- [ ] **Step 3: Remove old message rendering code from tui.rs**

Delete the old inline message rendering from `draw()` — it's now in `thread_view.rs`. Keep the approval dialog and customize dialog rendering functions (they stay in tui.rs).

- [ ] **Step 4: Verify compilation**

Run: `cargo check -p ox-cli`

- [ ] **Step 5: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: 14/14 pass

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/
git commit -m "feat(ox-cli): compose views — modal draw with inbox/thread/tabs/status"
```

---

### Summary

| Task | What it builds |
|------|---------------|
| 1 | InputMode + InsertContext + SearchState types, mode transitions, send_input() |
| 2 | Mode-aware key dispatch — Normal (navigate/approve) + Insert (type/search) |
| 3 | Inbox view rendering — 2-line thread rows, filter bar, state sorting |
| 4 | Thread view + tab bar extracted as focused modules |
| 5 | Composed draw — tab bar + filter bar + content + conditional input box + status bar |

After Plan 2b, ox-cli has:
- Vim-style Normal/Insert modes — input box only in Insert
- Inbox view with 2-line rows, state colors, urgency sorting
- Live search with compounding numbered filter chips
- Tab bar showing inbox + open threads
- Status bar with mode badge + context hints
- `tui.rs` split into inbox_view.rs, thread_view.rs, tab_bar.rs

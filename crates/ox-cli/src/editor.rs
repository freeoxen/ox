use crossterm::event::{KeyCode, KeyModifiers};
use ox_ui::text_input_store::{Edit, EditOp, EditSequence, EditSource};

// ---------------------------------------------------------------------------
// InputSession — optimistic local input state
// ---------------------------------------------------------------------------

/// Sub-mode within the text editor (compose/reply input).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorMode {
    /// Typing text — characters are inserted at cursor.
    Insert,
    /// Vim-style navigation — hjkl, w/b, 0/$, i/a to re-enter insert.
    Normal,
    /// Command prompt within the editor (`:` line at bottom).
    Command,
}

pub(crate) struct InputSession {
    pub(crate) content: String,
    pub(crate) cursor: usize,
    pub(crate) pending_edits: Vec<Edit>,
    pub(crate) generation: u64,
    pub(crate) editor_mode: EditorMode,
    /// Buffer for the editor command prompt (`:q`, `:w`, etc.)
    pub(crate) command_buffer: String,
}

impl InputSession {
    pub(crate) fn new() -> Self {
        Self {
            content: String::new(),
            cursor: 0,
            pending_edits: Vec::new(),
            generation: 0,
            editor_mode: EditorMode::Insert,
            command_buffer: String::new(),
        }
    }

    /// Insert text at the current cursor position, push an Edit.
    pub(crate) fn insert(&mut self, text: &str, source: EditSource) {
        let at = self.cursor.min(self.content.len());
        self.content.insert_str(at, text);
        self.cursor = at + text.len();
        self.pending_edits.push(Edit {
            op: EditOp::Insert {
                text: text.to_string(),
            },
            at,
            source,
            ts_ms: now_ms(),
        });
    }

    /// Delete one char before cursor (backspace), push an Edit.
    pub(crate) fn backspace(&mut self) {
        if self.cursor > 0 {
            // Find previous char boundary
            let before = &self.content[..self.cursor];
            let prev_char_start = before
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            let len = self.cursor - prev_char_start;
            self.content.drain(prev_char_start..self.cursor);
            self.cursor = prev_char_start;
            self.pending_edits.push(Edit {
                op: EditOp::Delete { len },
                at: prev_char_start,
                source: EditSource::Key,
                ts_ms: now_ms(),
            });
        }
    }

    /// Clear all content, push a Delete edit for the whole content.
    pub(crate) fn clear(&mut self) {
        if !self.content.is_empty() {
            let len = self.content.len();
            self.pending_edits.push(Edit {
                op: EditOp::Delete { len },
                at: 0,
                source: EditSource::Key,
                ts_ms: now_ms(),
            });
            self.content.clear();
            self.cursor = 0;
        }
    }

    /// Reset session after submission (clear content + bump generation).
    pub(crate) fn reset_after_submit(&mut self) {
        self.content.clear();
        self.cursor = 0;
        self.pending_edits.clear();
        self.generation += 1;
        self.editor_mode = EditorMode::Insert;
    }

    /// Initialize from broker state.
    pub(crate) fn init_from(&mut self, content: String, cursor: usize) {
        self.content = content;
        self.cursor = cursor.min(self.content.len());
        self.pending_edits.clear();
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Submit editor content: flush edits, send message, clear, exit, reset.
/// Returns the new thread ID if a compose created one.
pub(crate) async fn submit_editor_content(
    session: &mut InputSession,
    app: &mut crate::app::App,
    client: &ox_broker::ClientHandle,
) -> Option<String> {
    flush_pending_edits(session, client).await;
    let text = session.content.clone();

    // Read context from broker
    let ctx = client
        .read(&structfs_core_store::path!("ui/insert_context"))
        .await
        .ok()
        .flatten()
        .and_then(|r| match r.as_value() {
            Some(structfs_core_store::Value::String(s)) => Some(s.clone()),
            _ => None,
        });
    let active = client
        .read(&structfs_core_store::path!("ui/active_thread"))
        .await
        .ok()
        .flatten()
        .and_then(|r| match r.as_value() {
            Some(structfs_core_store::Value::String(s)) => Some(s.clone()),
            _ => None,
        });
    let mode = client
        .read(&structfs_core_store::path!("ui/mode"))
        .await
        .ok()
        .flatten()
        .and_then(|r| match r.as_value() {
            Some(structfs_core_store::Value::String(s)) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "insert".to_string());

    let new_tid = app.send_input_with_text(text, &mode, ctx.as_deref(), active.as_deref());

    let _ = client
        .write(&structfs_core_store::path!("ui/clear_input"), cmd!())
        .await;
    let _ = client
        .write(&structfs_core_store::path!("ui/exit_insert"), cmd!())
        .await;
    session.reset_after_submit();

    new_tid
}

/// Flush pending edits to the broker as a single EditSequence write.
pub(crate) async fn flush_pending_edits(
    input_session: &mut InputSession,
    client: &ox_broker::ClientHandle,
) {
    if !input_session.pending_edits.is_empty() {
        let seq = EditSequence {
            edits: std::mem::take(&mut input_session.pending_edits),
            generation: input_session.generation,
        };
        let value = structfs_serde_store::to_value(&seq).unwrap();
        let _ = client
            .write(
                &structfs_core_store::path!("ui/input/edit"),
                structfs_core_store::Record::parsed(value),
            )
            .await;
    }
}

// ---------------------------------------------------------------------------
// Command input execution
// ---------------------------------------------------------------------------

/// Parse command input text and dispatch through the CommandStore.
///
/// Syntax: `command_name` or `command_name key=value key=value`
/// For commands with a single required param, positional: `command_name value`
pub(crate) async fn execute_command_input(input: &str, client: &ox_broker::ClientHandle) {
    use structfs_core_store::path;

    let input = input.trim();
    if input.is_empty() {
        return;
    }

    let mut parts = input.splitn(2, ' ');
    let command_name = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();

    // Build args map from remaining text
    let mut args = serde_json::Map::new();
    if !rest.is_empty() {
        // Try key=value pairs first
        let mut has_kv = false;
        for token in rest.split_whitespace() {
            if let Some((k, v)) = token.split_once('=') {
                args.insert(k.to_string(), serde_json::Value::String(v.to_string()));
                has_kv = true;
            }
        }
        // If no key=value pairs found, treat as a single positional argument.
        // Look up the command to find the first required param name.
        if !has_kv {
            let cmd_path =
                structfs_core_store::Path::parse(&format!("command/commands/{command_name}"))
                    .unwrap_or_else(|_| path!("command/commands"));
            if let Ok(Some(record)) = client.read(&cmd_path).await {
                if let Some(structfs_core_store::Value::Map(def_map)) = record.as_value() {
                    if let Some(structfs_core_store::Value::Array(params)) = def_map.get("params") {
                        // Find the first required param
                        for param in params {
                            if let structfs_core_store::Value::Map(p) = param {
                                let required = matches!(
                                    p.get("required"),
                                    Some(structfs_core_store::Value::Bool(true))
                                );
                                if required {
                                    if let Some(structfs_core_store::Value::String(name)) =
                                        p.get("name")
                                    {
                                        args.insert(
                                            name.clone(),
                                            serde_json::Value::String(rest.to_string()),
                                        );
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Build CommandInvocation and dispatch
    let inv = serde_json::json!({
        "command": command_name,
        "args": args,
    });
    let inv_value = structfs_serde_store::json_to_value(inv);
    let result = client
        .write(
            &path!("command/invoke"),
            structfs_core_store::Record::parsed(inv_value),
        )
        .await;
    if let Err(e) = result {
        let _ = client
            .write(&path!("ui/set_status"), cmd!("text" => format!("{e}")))
            .await;
    }
}

// ---------------------------------------------------------------------------
// Editor sub-mode key handlers
// ---------------------------------------------------------------------------

/// Handle a key in editor-insert mode (typing text).
pub(crate) fn handle_editor_insert_key(
    session: &mut InputSession,
    modifiers: KeyModifiers,
    code: KeyCode,
) {
    match (modifiers, code) {
        (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
            session.cursor = 0;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
            session.cursor = session.content.len();
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
            session.clear();
        }
        (_, KeyCode::Left) => {
            let s = &session.content[..session.cursor];
            session.cursor = s.char_indices().next_back().map(|(i, _)| i).unwrap_or(0);
        }
        (_, KeyCode::Right) => {
            let s = &session.content[session.cursor..];
            session.cursor += s.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
        }
        (_, KeyCode::Backspace) => {
            session.backspace();
        }
        (_, KeyCode::Enter) => {
            session.insert("\n", EditSource::Key);
        }
        (_, KeyCode::Char(c)) => {
            session.insert(&c.to_string(), EditSource::Key);
        }
        _ => {}
    }
}

/// Handle a key in editor-normal mode (vim navigation).
pub(crate) async fn handle_editor_normal_key(
    session: &mut InputSession,
    app: &mut crate::app::App,
    client: &ox_broker::ClientHandle,
    term_width: u16,
    code: KeyCode,
) {
    use crate::text_input_view::{byte_offset_at, cursor_in_lines, wrap_lines};

    match code {
        // -- Mode transitions --
        KeyCode::Char('i') => {
            session.editor_mode = EditorMode::Insert;
        }
        KeyCode::Char('a') => {
            // Append: move cursor right one char, enter insert
            let s = &session.content[session.cursor..];
            session.cursor += s.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
            session.editor_mode = EditorMode::Insert;
        }
        KeyCode::Char('I') => {
            // Insert at beginning of line
            let lines = wrap_lines(&session.content, term_width);
            let (cur_line, _) = cursor_in_lines(&session.content, session.cursor, &lines);
            session.cursor = byte_offset_at(&session.content, &lines, cur_line, 0);
            session.editor_mode = EditorMode::Insert;
        }
        KeyCode::Char('A') => {
            // Append at end of line
            let lines = wrap_lines(&session.content, term_width);
            let (cur_line, _) = cursor_in_lines(&session.content, session.cursor, &lines);
            if cur_line < lines.len() {
                session.cursor = lines[cur_line].end;
            }
            session.editor_mode = EditorMode::Insert;
        }
        KeyCode::Char('o') => {
            // Open line below
            let lines = wrap_lines(&session.content, term_width);
            let (cur_line, _) = cursor_in_lines(&session.content, session.cursor, &lines);
            if cur_line < lines.len() {
                session.cursor = lines[cur_line].end;
            }
            session.insert("\n", EditSource::Key);
            session.editor_mode = EditorMode::Insert;
        }
        KeyCode::Char('O') => {
            // Open line above
            let lines = wrap_lines(&session.content, term_width);
            let (cur_line, _) = cursor_in_lines(&session.content, session.cursor, &lines);
            session.cursor = byte_offset_at(&session.content, &lines, cur_line, 0);
            session.insert("\n", EditSource::Key);
            // Cursor is now after the newline; move back to start of new blank line
            session.cursor -= 1;
            session.editor_mode = EditorMode::Insert;
        }

        // -- Movement --
        KeyCode::Char('h') | KeyCode::Left => {
            let s = &session.content[..session.cursor];
            session.cursor = s.char_indices().next_back().map(|(i, _)| i).unwrap_or(0);
        }
        KeyCode::Char('l') | KeyCode::Right => {
            let s = &session.content[session.cursor..];
            session.cursor += s.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
        }
        KeyCode::Char('j') | KeyCode::Down => {
            let lines = wrap_lines(&session.content, term_width);
            let (cur_line, cur_col) = cursor_in_lines(&session.content, session.cursor, &lines);
            if cur_line + 1 < lines.len() {
                session.cursor = byte_offset_at(&session.content, &lines, cur_line + 1, cur_col);
            } else if let Some((text, cursor)) = app.history_down() {
                flush_pending_edits(session, client).await;
                let _ = client
                    .write(
                        &structfs_core_store::path!("ui/set_input"),
                        cmd!("text" => text.clone(), "cursor" => cursor as i64),
                    )
                    .await;
                session.init_from(text, cursor);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            let lines = wrap_lines(&session.content, term_width);
            let (cur_line, cur_col) = cursor_in_lines(&session.content, session.cursor, &lines);
            if cur_line > 0 {
                session.cursor = byte_offset_at(&session.content, &lines, cur_line - 1, cur_col);
            } else if let Some((text, cursor)) = app.history_up(&session.content) {
                flush_pending_edits(session, client).await;
                let _ = client
                    .write(
                        &structfs_core_store::path!("ui/set_input"),
                        cmd!("text" => text.clone(), "cursor" => cursor as i64),
                    )
                    .await;
                session.init_from(text, cursor);
            }
        }
        KeyCode::Char('0') => {
            // Beginning of line
            let lines = wrap_lines(&session.content, term_width);
            let (cur_line, _) = cursor_in_lines(&session.content, session.cursor, &lines);
            session.cursor = byte_offset_at(&session.content, &lines, cur_line, 0);
        }
        KeyCode::Char('$') => {
            // End of line
            let lines = wrap_lines(&session.content, term_width);
            let (cur_line, _) = cursor_in_lines(&session.content, session.cursor, &lines);
            if cur_line < lines.len() {
                session.cursor = lines[cur_line].end;
            }
        }
        KeyCode::Char('w') => {
            // Next word start
            let rest = &session.content[session.cursor..];
            let mut chars = rest.char_indices();
            // Skip current word (non-whitespace)
            let mut offset = 0;
            for (i, c) in chars.by_ref() {
                if c.is_whitespace() {
                    offset = i;
                    break;
                }
                offset = i + c.len_utf8();
            }
            // Skip whitespace
            for (i, c) in rest[offset..].char_indices() {
                if !c.is_whitespace() {
                    session.cursor += offset + i;
                    return;
                }
            }
            // End of content
            session.cursor = session.content.len();
        }
        KeyCode::Char('b') => {
            // Previous word start
            let before = &session.content[..session.cursor];
            let trimmed = before.trim_end();
            if trimmed.is_empty() {
                session.cursor = 0;
                return;
            }
            // Find start of previous word
            if let Some(pos) = trimmed.rfind(|c: char| c.is_whitespace()) {
                session.cursor = pos + 1;
            } else {
                session.cursor = 0;
            }
        }

        // -- Editing --
        KeyCode::Char('x') => {
            // Delete char under cursor
            let s = &session.content[session.cursor..];
            if let Some(c) = s.chars().next() {
                let len = c.len_utf8();
                session.content.drain(session.cursor..session.cursor + len);
                session.pending_edits.push(Edit {
                    op: EditOp::Delete { len },
                    at: session.cursor,
                    source: EditSource::Key,
                    ts_ms: now_ms(),
                });
                // Clamp cursor
                if session.cursor > 0 && session.cursor >= session.content.len() {
                    let s = &session.content[..session.cursor];
                    session.cursor = s.char_indices().next_back().map(|(i, _)| i).unwrap_or(0);
                }
            }
        }

        // -- Command prompt --
        KeyCode::Char(':') | KeyCode::Char(';') => {
            session.command_buffer.clear();
            session.editor_mode = EditorMode::Command;
        }

        _ => {}
    }
}

/// Handle a key in editor-command mode (`:` prompt within the editor).
pub(crate) async fn handle_editor_command_key(
    session: &mut InputSession,
    app: &mut crate::app::App,
    client: &ox_broker::ClientHandle,
    code: KeyCode,
) {
    match code {
        KeyCode::Esc => {
            session.command_buffer.clear();
            session.editor_mode = EditorMode::Normal;
        }
        KeyCode::Enter => {
            let cmd = session.command_buffer.trim().to_string();
            session.command_buffer.clear();
            session.editor_mode = EditorMode::Normal;

            match cmd.as_str() {
                "q" | "quit" => {
                    // Exit the editor (back to app normal mode)
                    let _ = client
                        .write(&structfs_core_store::path!("ui/clear_input"), cmd!())
                        .await;
                    let _ = client
                        .write(&structfs_core_store::path!("ui/exit_insert"), cmd!())
                        .await;
                    session.reset_after_submit();
                }
                "w" | "write" | "wq" | "x" => {
                    let new_tid = submit_editor_content(session, app, client).await;
                    if let Some(tid) = new_tid {
                        let _ = client
                            .write(
                                &structfs_core_store::path!("ui/open"),
                                cmd!("thread_id" => tid),
                            )
                            .await;
                    }
                }
                other => {
                    // Unknown command — try dispatching through CommandStore
                    execute_command_input(other, client).await;
                }
            }
        }
        KeyCode::Backspace => {
            session.command_buffer.pop();
            if session.command_buffer.is_empty() {
                session.editor_mode = EditorMode::Normal;
            }
        }
        KeyCode::Char(c) => {
            session.command_buffer.push(c);
        }
        _ => {}
    }
}

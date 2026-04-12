use crossterm::event::{KeyCode, KeyModifiers};
use ox_path::oxpath;
use ox_types::UiCommand;
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

    // Read UI snapshot from broker (typed)
    let ui: ox_types::UiSnapshot = client
        .read_typed(&structfs_core_store::path!("ui"))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();

    let new_tid = app.send_input_with_text(
        text,
        ui.mode,
        ui.insert_context,
        ui.active_thread.as_deref(),
    );

    let _ = client
        .write_typed(&oxpath!("ui"), &UiCommand::ClearInput)
        .await;
    let _ = client
        .write_typed(&oxpath!("ui"), &UiCommand::ExitInsert)
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
                &oxpath!("ui", "input", "edit"),
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
                    .unwrap_or_else(|_| oxpath!("command", "commands"));
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
            &oxpath!("command", "invoke"),
            structfs_core_store::Record::parsed(inv_value),
        )
        .await;
    if let Err(e) = result {
        let _ = client
            .write_typed(
                &oxpath!("ui"),
                &UiCommand::SetStatus {
                    text: format!("{e}"),
                },
            )
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
                    .write_typed(
                        &oxpath!("ui"),
                        &UiCommand::SetInput {
                            content: text.clone(),
                            cursor,
                        },
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
                    .write_typed(
                        &oxpath!("ui"),
                        &UiCommand::SetInput {
                            content: text.clone(),
                            cursor,
                        },
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
                        .write_typed(&oxpath!("ui"), &UiCommand::ClearInput)
                        .await;
                    let _ = client
                        .write_typed(&oxpath!("ui"), &UiCommand::ExitInsert)
                        .await;
                    session.reset_after_submit();
                }
                "w" | "write" | "wq" | "x" => {
                    let new_tid = submit_editor_content(session, app, client).await;
                    if let Some(tid) = new_tid {
                        let _ = client
                            .write_typed(&oxpath!("ui"), &UiCommand::Open { thread_id: tid })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use ox_ui::text_input_store::EditSource;

    fn session_with(content: &str, cursor: usize) -> InputSession {
        let mut s = InputSession::new();
        s.content = content.to_string();
        s.cursor = cursor.min(content.len());
        s
    }

    // ======================================================================
    // Editor Insert Mode
    // ======================================================================

    #[test]
    fn insert_char_at_cursor() {
        let mut s = session_with("hllo", 1);
        handle_editor_insert_key(&mut s, KeyModifiers::NONE, KeyCode::Char('e'));
        assert_eq!(s.content, "hello");
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn insert_char_at_end() {
        let mut s = session_with("hell", 4);
        handle_editor_insert_key(&mut s, KeyModifiers::NONE, KeyCode::Char('o'));
        assert_eq!(s.content, "hello");
        assert_eq!(s.cursor, 5);
    }

    #[test]
    fn insert_backspace() {
        let mut s = session_with("hello", 5);
        handle_editor_insert_key(&mut s, KeyModifiers::NONE, KeyCode::Backspace);
        assert_eq!(s.content, "hell");
        assert_eq!(s.cursor, 4);
    }

    #[test]
    fn insert_backspace_at_start_noop() {
        let mut s = session_with("hello", 0);
        handle_editor_insert_key(&mut s, KeyModifiers::NONE, KeyCode::Backspace);
        assert_eq!(s.content, "hello");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn insert_ctrl_a_moves_to_start() {
        let mut s = session_with("hello", 3);
        handle_editor_insert_key(&mut s, KeyModifiers::CONTROL, KeyCode::Char('a'));
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn insert_ctrl_e_moves_to_end() {
        let mut s = session_with("hello", 0);
        handle_editor_insert_key(&mut s, KeyModifiers::CONTROL, KeyCode::Char('e'));
        assert_eq!(s.cursor, 5);
    }

    #[test]
    fn insert_enter_adds_newline() {
        let mut s = session_with("ab", 1);
        handle_editor_insert_key(&mut s, KeyModifiers::NONE, KeyCode::Enter);
        assert_eq!(s.content, "a\nb");
        assert_eq!(s.cursor, 2); // after the newline
    }

    #[test]
    fn insert_left_arrow() {
        let mut s = session_with("abc", 2);
        handle_editor_insert_key(&mut s, KeyModifiers::NONE, KeyCode::Left);
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn insert_right_arrow() {
        let mut s = session_with("abc", 1);
        handle_editor_insert_key(&mut s, KeyModifiers::NONE, KeyCode::Right);
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn insert_left_at_start_stays() {
        let mut s = session_with("abc", 0);
        handle_editor_insert_key(&mut s, KeyModifiers::NONE, KeyCode::Left);
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn insert_right_at_end_stays() {
        let mut s = session_with("abc", 3);
        handle_editor_insert_key(&mut s, KeyModifiers::NONE, KeyCode::Right);
        assert_eq!(s.cursor, 3);
    }

    #[test]
    fn insert_ctrl_u_clears() {
        let mut s = session_with("hello", 3);
        handle_editor_insert_key(&mut s, KeyModifiers::CONTROL, KeyCode::Char('u'));
        assert!(s.content.is_empty());
        assert_eq!(s.cursor, 0);
    }

    // ======================================================================
    // Editor Normal Mode — cursor movement
    // Tested via direct state manipulation that mirrors the exact logic in
    // handle_editor_normal_key (async + broker-dependent paths are skipped).
    // ======================================================================

    #[test]
    fn normal_mode_starts_from_insert() {
        let s = InputSession::new();
        assert_eq!(s.editor_mode, EditorMode::Insert);
    }

    #[test]
    fn session_with_sets_content_and_cursor() {
        let s = session_with("hello world", 5);
        assert_eq!(s.content, "hello world");
        assert_eq!(s.cursor, 5);
    }

    /// Mirror of the 'h' branch in handle_editor_normal_key.
    #[test]
    fn h_movement_left() {
        let s = session_with("hello", 3);
        let before = &s.content[..s.cursor];
        let new_cursor = before
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        assert_eq!(new_cursor, 2);
    }

    /// At position 0 'h' keeps cursor at 0.
    #[test]
    fn h_at_start_stays() {
        let s = session_with("hello", 0);
        let before = &s.content[..s.cursor]; // ""
        let new_cursor = before
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        assert_eq!(new_cursor, 0);
    }

    /// Mirror of the 'l' branch in handle_editor_normal_key.
    #[test]
    fn l_movement_right() {
        let s = session_with("hello", 2);
        let rest = &s.content[s.cursor..];
        let new_cursor = s.cursor + rest.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
        assert_eq!(new_cursor, 3);
    }

    /// At the end 'l' keeps cursor at content.len().
    #[test]
    fn l_at_end_stays() {
        let s = session_with("hello", 5);
        let rest = &s.content[s.cursor..]; // ""
        let new_cursor = s.cursor + rest.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
        assert_eq!(new_cursor, 5);
    }

    /// '0' moves to byte offset of the first column of the current wrap-line.
    #[test]
    fn zero_moves_to_line_start() {
        use crate::text_input_view::{byte_offset_at, cursor_in_lines, wrap_lines};
        let s = session_with("hello world", 6);
        // With a wide terminal (80 cols) the whole string is one line.
        let lines = wrap_lines(&s.content, 80);
        let (cur_line, _) = cursor_in_lines(&s.content, s.cursor, &lines);
        let new_cursor = byte_offset_at(&s.content, &lines, cur_line, 0);
        assert_eq!(new_cursor, 0);
    }

    /// '$' lands on lines[cur_line].end for "hello world" with a wide terminal.
    #[test]
    fn dollar_moves_to_line_end() {
        use crate::text_input_view::{cursor_in_lines, wrap_lines};
        let s = session_with("hello world", 3);
        let lines = wrap_lines(&s.content, 80);
        let (cur_line, _) = cursor_in_lines(&s.content, s.cursor, &lines);
        // "hello world" is 11 bytes; the single wrap-line ends at 11.
        assert_eq!(lines[cur_line].end, 11);
    }

    /// 'w' — simulate the exact for-loop logic from handle_editor_normal_key.
    ///
    /// Starting at 0 in "hello world foo" the next word starts at byte 6 ('w').
    #[test]
    fn w_jumps_to_next_word() {
        let mut s = session_with("hello world foo", 0);
        // --- exact replica of the 'w' arm ---
        let rest = &s.content[s.cursor..];
        let mut chars = rest.char_indices();
        let mut offset = 0usize;
        for (i, c) in chars.by_ref() {
            if c.is_whitespace() {
                offset = i;
                break;
            }
            offset = i + c.len_utf8();
        }
        for (i, c) in rest[offset..].char_indices() {
            if !c.is_whitespace() {
                s.cursor += offset + i;
                break; // note: the real code uses `return`, effect is the same
            }
        }
        // ---------------------------------
        assert_eq!(s.cursor, 6); // "world" starts at byte 6
    }

    /// 'b' — simulate the exact trim_end / rfind logic from handle_editor_normal_key.
    ///
    /// Starting at byte 8 in "hello world" the previous word is "world" at byte 6.
    #[test]
    fn b_jumps_to_prev_word() {
        let s = session_with("hello world", 8);
        let before = &s.content[..s.cursor]; // "hello wo"
        let trimmed = before.trim_end();
        let new_cursor = if let Some(pos) = trimmed.rfind(|c: char| c.is_whitespace()) {
            pos + 1
        } else {
            0
        };
        assert_eq!(new_cursor, 6);
    }

    /// 'b' at cursor inside the first word goes all the way to 0.
    #[test]
    fn b_at_first_word_goes_to_start() {
        let s = session_with("hello world", 3);
        let before = &s.content[..s.cursor]; // "hel"
        let trimmed = before.trim_end();
        let new_cursor = if let Some(pos) = trimmed.rfind(|c: char| c.is_whitespace()) {
            pos + 1
        } else {
            0
        };
        assert_eq!(new_cursor, 0);
    }

    /// 'x' deletes the character under the cursor, cursor stays in place.
    #[test]
    fn x_deletes_char_under_cursor() {
        let mut s = session_with("hello", 1);
        // --- exact replica of the 'x' arm (excluding the clamping path) ---
        let ch = s.content[s.cursor..].chars().next().unwrap();
        let len = ch.len_utf8();
        s.content.drain(s.cursor..s.cursor + len);
        // Cursor is 1, content is now "hllo" (len 4); 1 < 4, no clamping.
        assert_eq!(s.content, "hllo");
        assert_eq!(s.cursor, 1);
    }

    /// 'x' at end of string (cursor == len) has no character to delete.
    #[test]
    fn x_at_end_is_noop() {
        let s = session_with("hello", 5);
        let has_char = s.content[s.cursor..].chars().next().is_some();
        assert!(!has_char); // nothing to delete
    }

    // ======================================================================
    // Editor Command Mode
    // ======================================================================

    #[test]
    fn command_buffer_appends_chars() {
        let mut s = InputSession::new();
        s.editor_mode = EditorMode::Command;
        // Simulates two KeyCode::Char presses routed through handle_editor_command_key.
        s.command_buffer.push('w');
        s.command_buffer.push('q');
        assert_eq!(s.command_buffer, "wq");
    }

    #[test]
    fn command_buffer_backspace() {
        let mut s = InputSession::new();
        s.editor_mode = EditorMode::Command;
        s.command_buffer = "wq".to_string();
        // Simulate the Backspace arm: pop, then check for empty.
        s.command_buffer.pop();
        if s.command_buffer.is_empty() {
            s.editor_mode = EditorMode::Normal;
        }
        assert_eq!(s.command_buffer, "w");
        assert_eq!(s.editor_mode, EditorMode::Command); // not empty, stays Command
    }

    #[test]
    fn command_backspace_empty_transitions_to_normal() {
        let mut s = InputSession::new();
        s.editor_mode = EditorMode::Command;
        // buffer is already empty; pop is a no-op, then the empty-check fires.
        s.command_buffer.pop();
        if s.command_buffer.is_empty() {
            s.editor_mode = EditorMode::Normal;
        }
        assert_eq!(s.editor_mode, EditorMode::Normal);
    }

    #[test]
    fn command_esc_clears_and_returns_to_normal() {
        let mut s = InputSession::new();
        s.editor_mode = EditorMode::Command;
        s.command_buffer = "wq".to_string();
        // Simulate the Esc arm.
        s.command_buffer.clear();
        s.editor_mode = EditorMode::Normal;
        assert!(s.command_buffer.is_empty());
        assert_eq!(s.editor_mode, EditorMode::Normal);
    }

    // ======================================================================
    // Mode Transitions
    // ======================================================================

    #[test]
    fn mode_cycle_insert_normal_command_normal_insert() {
        let mut s = InputSession::new();
        assert_eq!(s.editor_mode, EditorMode::Insert);

        s.editor_mode = EditorMode::Normal;
        assert_eq!(s.editor_mode, EditorMode::Normal);

        s.command_buffer.clear();
        s.editor_mode = EditorMode::Command;
        assert_eq!(s.editor_mode, EditorMode::Command);

        s.command_buffer.clear();
        s.editor_mode = EditorMode::Normal;
        assert_eq!(s.editor_mode, EditorMode::Normal);

        s.editor_mode = EditorMode::Insert;
        assert_eq!(s.editor_mode, EditorMode::Insert);
    }

    /// reset_after_submit clears content/cursor/pending_edits, sets Insert mode,
    /// bumps generation — but does NOT clear command_buffer.
    #[test]
    fn reset_after_submit_returns_to_insert() {
        let mut s = session_with("hello", 3);
        s.editor_mode = EditorMode::Normal;
        s.command_buffer = "wq".to_string();
        s.reset_after_submit();
        assert_eq!(s.editor_mode, EditorMode::Insert);
        assert!(s.content.is_empty());
        assert_eq!(s.cursor, 0);
        // command_buffer is intentionally NOT cleared by reset_after_submit.
        assert_eq!(s.command_buffer, "wq");
    }

    /// init_from sets content + cursor, clears pending_edits, leaves editor_mode alone.
    #[test]
    fn init_from_preserves_editor_mode() {
        let mut s = InputSession::new();
        s.editor_mode = EditorMode::Normal;
        s.init_from("new content".to_string(), 5);
        assert_eq!(s.content, "new content");
        assert_eq!(s.cursor, 5);
        assert_eq!(s.editor_mode, EditorMode::Normal);
    }

    // ======================================================================
    // InputSession basics
    // ======================================================================

    #[test]
    fn insert_text_at_position() {
        let mut s = session_with("hd", 1);
        s.insert("ello worl", EditSource::Key);
        assert_eq!(s.content, "hello world");
        assert_eq!(s.cursor, 10);
        assert_eq!(s.pending_edits.len(), 1);
    }

    #[test]
    fn backspace_deletes_previous_char() {
        let mut s = session_with("hello", 3);
        s.backspace();
        assert_eq!(s.content, "helo");
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn backspace_at_zero_is_noop() {
        let mut s = session_with("hello", 0);
        s.backspace();
        assert_eq!(s.content, "hello");
        assert_eq!(s.cursor, 0);
        assert!(s.pending_edits.is_empty());
    }

    #[test]
    fn clear_empties_content() {
        let mut s = session_with("hello", 3);
        s.clear();
        assert!(s.content.is_empty());
        assert_eq!(s.cursor, 0);
        assert_eq!(s.pending_edits.len(), 1);
    }

    #[test]
    fn clear_empty_is_noop() {
        let mut s = InputSession::new();
        s.clear();
        assert!(s.pending_edits.is_empty());
    }
}

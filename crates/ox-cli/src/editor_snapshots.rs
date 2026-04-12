//! Insta snapshot tests for the editor component.
//!
//! Renders the TextInputView into a ratatui TestBackend and snapshots
//! the terminal buffer. This catches visual regressions in the editor
//! rendering across all states and transitions.

#[cfg(test)]
mod tests {
    use crate::editor::{EditorMode, InputSession};
    use crate::text_input_view::TextInputView;
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    /// Render the TextInputView into a string for snapshot testing.
    /// Returns the rendered buffer as lines of text.
    fn render_editor(content: &str, cursor: usize, title: &str, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut view = TextInputView::new();
        view.set_state(content, cursor);

        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, width, height);
                view.render(frame, area, Style::default(), title);
            })
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut lines = Vec::new();
        for y in 0..buf.area.height {
            let mut line = String::new();
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                line.push_str(cell.symbol());
            }
            // Trim trailing spaces for cleaner snapshots
            let trimmed = line.trim_end();
            lines.push(trimmed.to_string());
        }
        // Remove trailing empty lines
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    /// Render a command line prompt (`:` at bottom) for snapshot testing.
    fn render_command_line(buffer: &str, width: u16) -> String {
        let backend = TestBackend::new(width, 1);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                use ratatui::text::{Line, Span};
                use ratatui::widgets::Paragraph;
                let prompt = Span::raw(":");
                let input = Span::raw(buffer);
                let line = Line::from(vec![prompt, input]);
                frame.render_widget(Paragraph::new(line), Rect::new(0, 0, width, 1));
            })
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut line = String::new();
        for x in 0..buf.area.width {
            let cell = &buf[(x, 0)];
            line.push_str(cell.symbol());
        }
        line.trim_end().to_string()
    }

    // ======================================================================
    // Empty / initial states
    // ======================================================================

    #[test]
    fn empty_editor() {
        let rendered = render_editor("", 0, "", 40, 3);
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn empty_editor_compose_mode() {
        let rendered = render_editor("", 0, " compose ", 40, 3);
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn empty_editor_reply_mode() {
        let rendered = render_editor("", 0, " reply ", 40, 3);
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn empty_editor_normal_indicator() {
        let rendered = render_editor("", 0, " NORMAL ", 40, 3);
        insta::assert_snapshot!(rendered);
    }

    // ======================================================================
    // Content rendering
    // ======================================================================

    #[test]
    fn single_line_content() {
        let rendered = render_editor("hello world", 11, "", 40, 3);
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn cursor_at_start() {
        let rendered = render_editor("hello world", 0, "", 40, 3);
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn cursor_in_middle() {
        let rendered = render_editor("hello world", 5, "", 40, 3);
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn multiline_content() {
        let rendered = render_editor("line one\nline two\nline three", 0, "", 40, 5);
        insta::assert_snapshot!(rendered);
    }

    // ======================================================================
    // Line wrapping
    // ======================================================================

    #[test]
    fn soft_wrap_long_line() {
        let rendered = render_editor("this is a long line that should wrap around", 0, "", 20, 5);
        insta::assert_snapshot!(rendered);
    }

    // ======================================================================
    // Scroll indicator
    // ======================================================================

    #[test]
    fn scroll_indicator_when_overflowing() {
        // 5 lines of content in a 3-line viewport (1 border + 2 visible)
        let content = "line 1\nline 2\nline 3\nline 4\nline 5";
        let rendered = render_editor(content, 0, "", 40, 3);
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn scroll_indicator_cursor_at_bottom() {
        let content = "line 1\nline 2\nline 3\nline 4\nline 5";
        // Cursor at end of last line — should show 5/5
        let rendered = render_editor(content, content.len(), "", 40, 3);
        insta::assert_snapshot!(rendered);
    }

    // ======================================================================
    // Editor state transitions (content after key sequences)
    // ======================================================================

    #[test]
    fn insert_then_esc_to_normal() {
        let mut session = InputSession::new();
        session.content = "hello".to_string();
        session.cursor = 5;
        session.editor_mode = EditorMode::Insert;

        // Press ESC → editor normal
        session.editor_mode = EditorMode::Normal;

        let title = if session.editor_mode == EditorMode::Normal {
            " NORMAL "
        } else {
            ""
        };
        let rendered = render_editor(&session.content, session.cursor, title, 40, 3);
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn normal_mode_after_hjkl() {
        let mut session = InputSession::new();
        session.content = "hello world".to_string();
        session.cursor = 5;
        session.editor_mode = EditorMode::Normal;

        // h moves left
        crate::editor::handle_editor_insert_key(&mut session, KeyModifiers::NONE, KeyCode::Left);
        // Now cursor at 4

        let rendered = render_editor(&session.content, session.cursor, " NORMAL ", 40, 3);
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn insert_text_then_render() {
        let mut session = InputSession::new();
        session.editor_mode = EditorMode::Insert;

        // Type "hello"
        for c in "hello".chars() {
            crate::editor::handle_editor_insert_key(
                &mut session,
                KeyModifiers::NONE,
                KeyCode::Char(c),
            );
        }

        let rendered = render_editor(&session.content, session.cursor, "", 40, 3);
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn backspace_then_render() {
        let mut session = InputSession::new();
        session.content = "hello".to_string();
        session.cursor = 5;
        session.editor_mode = EditorMode::Insert;

        // Backspace twice
        crate::editor::handle_editor_insert_key(
            &mut session,
            KeyModifiers::NONE,
            KeyCode::Backspace,
        );
        crate::editor::handle_editor_insert_key(
            &mut session,
            KeyModifiers::NONE,
            KeyCode::Backspace,
        );

        let rendered = render_editor(&session.content, session.cursor, "", 40, 3);
        insta::assert_snapshot!(rendered);
    }

    // ======================================================================
    // Command line rendering
    // ======================================================================

    #[test]
    fn command_line_empty() {
        let rendered = render_command_line("", 40);
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn command_line_with_q() {
        let rendered = render_command_line("q", 40);
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn command_line_with_wq() {
        let rendered = render_command_line("wq", 40);
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn command_line_with_long_command() {
        let rendered = render_command_line("open thread_abc123", 40);
        insta::assert_snapshot!(rendered);
    }

    // ======================================================================
    // Editor state machine: full transition sequence
    // ======================================================================

    #[test]
    fn full_lifecycle_insert_type_esc_navigate_colon_q() {
        let mut session = InputSession::new();
        assert_eq!(session.editor_mode, EditorMode::Insert);

        // 1. Type some content
        for c in "hello world".chars() {
            crate::editor::handle_editor_insert_key(
                &mut session,
                KeyModifiers::NONE,
                KeyCode::Char(c),
            );
        }
        let step1 = render_editor(&session.content, session.cursor, "", 40, 3);
        insta::assert_snapshot!("lifecycle_1_after_typing", step1);

        // 2. ESC → normal mode
        session.editor_mode = EditorMode::Normal;
        let step2 = render_editor(&session.content, session.cursor, " NORMAL ", 40, 3);
        insta::assert_snapshot!("lifecycle_2_normal_mode", step2);

        // 3. Move cursor left with h (simulate)
        let before = &session.content[..session.cursor];
        session.cursor = before
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        let step3 = render_editor(&session.content, session.cursor, " NORMAL ", 40, 3);
        insta::assert_snapshot!("lifecycle_3_after_h", step3);

        // 4. : → command mode
        session.command_buffer.clear();
        session.editor_mode = EditorMode::Command;
        let step4_cmd = render_command_line("", 40);
        insta::assert_snapshot!("lifecycle_4_command_prompt", step4_cmd);

        // 5. Type "q"
        session.command_buffer.push('q');
        let step5_cmd = render_command_line(&session.command_buffer, 40);
        insta::assert_snapshot!("lifecycle_5_command_q", step5_cmd);
    }
}

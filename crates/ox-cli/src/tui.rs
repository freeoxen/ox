use crate::app::{App, ApprovalResponse, ApprovalState, AppControl, ChatMessage};
use crate::theme::Theme;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use std::time::Duration;

/// Run the TUI event loop. Blocks until the user quits.
pub fn run(
    app: &mut App,
    theme: &Theme,
    terminal: &mut ratatui::DefaultTerminal,
) -> std::io::Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, app, theme))?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if app.pending_approval.is_some() {
                    // Modal: approval dialog key handling
                    handle_approval_key(app, key.code);
                } else {
                    // Normal key handling
                    match (key.modifiers, key.code) {
                        (KeyModifiers::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Esc) => {
                            app.should_quit = true;
                        }
                        (_, KeyCode::Enter) => {
                            app.submit();
                        }
                        (_, KeyCode::Backspace) => {
                            if app.cursor > 0 {
                                app.cursor -= 1;
                                app.input.remove(app.cursor);
                            }
                        }
                        (_, KeyCode::Left) => {
                            app.cursor = app.cursor.saturating_sub(1);
                        }
                        (_, KeyCode::Right) => {
                            if app.cursor < app.input.len() {
                                app.cursor += 1;
                            }
                        }
                        (_, KeyCode::Up) => {
                            app.history_up();
                        }
                        (_, KeyCode::Down) => {
                            app.history_down();
                        }
                        (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
                            app.input.clear();
                            app.cursor = 0;
                        }
                        (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
                            app.cursor = 0;
                        }
                        (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
                            app.cursor = app.input.len();
                        }
                        (_, KeyCode::Char(c)) => {
                            app.input.insert(app.cursor, c);
                            app.cursor += 1;
                        }
                        _ => {}
                    }
                }
            }
        }

        // Drain agent events
        while let Ok(event) = app.event_rx.try_recv() {
            app.handle_event(event);
        }

        // Check for permission requests (non-blocking)
        if app.pending_approval.is_none() {
            if let Ok(AppControl::PermissionRequest {
                tool,
                input_preview,
                respond,
            }) = app.control_rx.try_recv()
            {
                app.pending_approval = Some(ApprovalState {
                    tool,
                    input_preview,
                    selected: 0,
                    respond,
                });
            }
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

fn handle_approval_key(app: &mut App, key: KeyCode) {
    let approval = app.pending_approval.as_mut().unwrap();
    match key {
        KeyCode::Up => {
            approval.selected = approval.selected.saturating_sub(1);
        }
        KeyCode::Down => {
            if approval.selected < ApprovalState::OPTIONS.len() - 1 {
                approval.selected += 1;
            }
        }
        KeyCode::Enter => {
            let response = ApprovalState::OPTIONS[approval.selected].1;
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(response).ok();
        }
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::AllowOnce).ok();
        }
        KeyCode::Char('n') | KeyCode::Char('N') => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::DenyOnce).ok();
        }
        KeyCode::Char('a') | KeyCode::Char('A') => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::AllowAlways).ok();
        }
        _ => {}
    }
}

fn draw(frame: &mut Frame, app: &App, theme: &Theme) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Min(1),    // messages
            Constraint::Length(3), // input
            Constraint::Length(1), // status
        ])
        .split(frame.area());

    // Title bar
    let title = Line::from(vec![
        Span::styled(" ox ", theme.title_badge),
        Span::styled(format!(" {} ({})", app.model, app.provider), theme.title_info),
    ]);
    frame.render_widget(Paragraph::new(title), chunks[0]);

    // Messages
    let mut lines: Vec<Line> = Vec::new();
    for msg in &app.messages {
        match msg {
            ChatMessage::User(text) => {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("> ", theme.user_prompt),
                    Span::styled(text, theme.user_text),
                ]));
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
    if app.thinking {
        if let Some(ChatMessage::AssistantChunk(_)) = app.messages.last() {
            // streaming text visible
        } else {
            lines.push(Line::from(Span::styled("  ...", theme.thinking)));
        }
    }

    let text = Text::from(lines);
    let msg_height = chunks[1].height as usize;
    let total_lines = text.lines.len();
    let scroll = if app.scroll == 0 {
        total_lines.saturating_sub(msg_height) as u16
    } else {
        let max_scroll = total_lines.saturating_sub(msg_height) as u16;
        max_scroll.saturating_sub(app.scroll)
    };
    let messages = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(messages, chunks[1]);

    // Input box
    let input_title = if app.thinking {
        " streaming... "
    } else {
        ""
    };
    let input_block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme.input_border)
        .title(input_title);
    let input = Paragraph::new(format!("> {}", app.input)).block(input_block);
    frame.render_widget(input, chunks[2]);

    // Cursor (only when not in approval mode)
    if app.pending_approval.is_none() {
        frame.set_cursor_position((
            chunks[2].x + app.cursor as u16 + 2,
            chunks[2].y + 1,
        ));
    }

    // Status bar
    let tokens = if app.tokens_in > 0 || app.tokens_out > 0 {
        format!(" | {}in/{}out", app.tokens_in, app.tokens_out)
    } else {
        String::new()
    };
    let policy = {
        let s = &app.policy_stats;
        if s.allowed > 0 || s.denied > 0 || s.asked > 0 {
            format!(" | ok:{} no:{} ask:{}", s.allowed, s.denied, s.asked)
        } else {
            String::new()
        }
    };
    let status_text = if app.pending_approval.is_some() {
        format!("PERMISSION REQUIRED{tokens}{policy}")
    } else if app.thinking {
        format!("streaming...{tokens}{policy}")
    } else {
        format!("idle{tokens}{policy} | Enter send | Esc quit")
    };
    let status = Paragraph::new(Span::styled(format!(" {status_text}"), theme.status));
    frame.render_widget(status, chunks[3]);

    // Approval dialog overlay
    if let Some(ref approval) = app.pending_approval {
        draw_approval_dialog(frame, approval, theme);
    }
}

fn draw_approval_dialog(frame: &mut Frame, approval: &ApprovalState, theme: &Theme) {
    let area = frame.area();
    let dialog_width = 50.min(area.width.saturating_sub(4));
    let dialog_height = 9;
    let x = (area.width.saturating_sub(dialog_width)) / 2;
    let y = (area.height.saturating_sub(dialog_height)) / 2;
    let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

    // Clear the area behind the dialog
    frame.render_widget(Clear, dialog_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.approval_border)
        .title(Span::styled(" Permission Required ", theme.approval_title));

    let inner = block.inner(dialog_area);
    frame.render_widget(block, dialog_area);

    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("[{}] ", approval.tool), theme.approval_tool),
            Span::styled(&approval.input_preview, theme.approval_preview),
        ]),
        Line::from(""),
    ];

    for (i, (label, _)) in ApprovalState::OPTIONS.iter().enumerate() {
        let style = if i == approval.selected {
            theme.approval_selected
        } else {
            theme.approval_option
        };
        let marker = if i == approval.selected { "> " } else { "  " };
        lines.push(Line::from(Span::styled(format!("{marker}{label}"), style)));
    }

    let content = Paragraph::new(Text::from(lines));
    frame.render_widget(content, inner);
}

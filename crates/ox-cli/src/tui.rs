use crate::app::{App, ChatMessage};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use std::time::Duration;

/// Run the TUI event loop. Blocks until the user quits.
pub fn run(app: &mut App, terminal: &mut ratatui::DefaultTerminal) -> std::io::Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, app))?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
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
                        app.scroll = app.scroll.saturating_add(1);
                    }
                    (_, KeyCode::Down) => {
                        app.scroll = app.scroll.saturating_sub(1);
                    }
                    (_, KeyCode::Char(c)) => {
                        app.input.insert(app.cursor, c);
                        app.cursor += 1;
                    }
                    _ => {}
                }
            }
        }

        // Drain agent events
        while let Ok(event) = app.event_rx.try_recv() {
            app.handle_event(event);
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

fn draw(frame: &mut Frame, app: &App) {
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
        Span::styled(
            " ox ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {} ({})", app.model, app.provider)),
    ]);
    frame.render_widget(Paragraph::new(title), chunks[0]);

    // Messages
    let mut lines: Vec<Line> = Vec::new();
    for msg in &app.messages {
        match msg {
            ChatMessage::User(text) => {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled(
                        "> ",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(text),
                ]));
            }
            ChatMessage::AssistantChunk(text) => {
                for line in text.lines() {
                    lines.push(Line::from(Span::raw(line)));
                }
            }
            ChatMessage::ToolCall { name } => {
                lines.push(Line::from(vec![
                    Span::styled(format!("  [{name}] "), Style::default().fg(Color::Yellow)),
                    Span::styled("running...", Style::default().fg(Color::DarkGray)),
                ]));
            }
            ChatMessage::ToolResult { name, output } => {
                let preview: String = output.lines().take(3).collect::<Vec<_>>().join(" | ");
                let truncated = if preview.len() > 120 {
                    format!("{}...", &preview[..120])
                } else {
                    preview
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  [{name}] "), Style::default().fg(Color::Yellow)),
                    Span::styled(truncated, Style::default().fg(Color::DarkGray)),
                ]));
            }
            ChatMessage::Error(e) => {
                lines.push(Line::from(Span::styled(
                    format!("  error: {e}"),
                    Style::default().fg(Color::Red),
                )));
            }
        }
    }

    let text = Text::from(lines);
    let msg_height = chunks[1].height as usize;
    let total_lines = text.lines.len();
    let scroll = if total_lines > msg_height && app.scroll == 0 {
        (total_lines - msg_height) as u16
    } else {
        app.scroll
    };
    let messages = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(messages, chunks[1]);

    // Input box
    let input_block = Block::default()
        .borders(Borders::TOP)
        .title(if app.thinking { " thinking... " } else { "" });
    let input = Paragraph::new(format!("> {}", app.input)).block(input_block);
    frame.render_widget(input, chunks[2]);

    // Cursor
    frame.set_cursor_position((
        chunks[2].x + app.cursor as u16 + 2, // "> " prefix
        chunks[2].y + 1,                       // border
    ));

    // Status bar
    let status_text = if app.thinking {
        "streaming..."
    } else {
        "idle — Enter to send, Esc to quit"
    };
    let status = Paragraph::new(Span::styled(
        format!(" {status_text}"),
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(status, chunks[3]);
}

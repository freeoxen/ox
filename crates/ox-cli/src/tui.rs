use crate::app::{App, ApprovalResponse, ApprovalState, AppControl, ChatMessage};
use crate::theme::Theme;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
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
            match event::read()? {
                Event::Key(key) => {
                    if app.pending_customize.is_some() {
                        handle_customize_key(app, key.code);
                    } else if app.pending_approval.is_some() {
                        handle_approval_key(app, key.code);
                    } else {
                        handle_normal_key(app, key.modifiers, key.code);
                    }
                }
                Event::Mouse(mouse) => {
                    handle_mouse(app, mouse.kind, mouse.row);
                }
                _ => {}
            }
        }

        // Drain agent events
        while let Ok(event) = app.event_rx.try_recv() {
            app.handle_event(event);
        }

        // Check for permission requests
        if app.pending_approval.is_none() && app.pending_customize.is_none() {
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

fn handle_normal_key(app: &mut App, modifiers: KeyModifiers, code: KeyCode) {
    match (modifiers, code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Esc) => {
            app.should_quit = true;
        }
        (_, KeyCode::Enter) => app.submit(),
        (_, KeyCode::Backspace) => {
            if app.cursor > 0 {
                app.cursor -= 1;
                app.input.remove(app.cursor);
            }
        }
        (_, KeyCode::Left) => app.cursor = app.cursor.saturating_sub(1),
        (_, KeyCode::Right) => {
            if app.cursor < app.input.len() {
                app.cursor += 1;
            }
        }
        (_, KeyCode::Up) => app.history_up(),
        (_, KeyCode::Down) => app.history_down(),
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
            app.input.clear();
            app.cursor = 0;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('a')) => app.cursor = 0,
        (KeyModifiers::CONTROL, KeyCode::Char('e')) => app.cursor = app.input.len(),
        (_, KeyCode::Char(c)) => {
            app.input.insert(app.cursor, c);
            app.cursor += 1;
        }
        _ => {}
    }
}

fn handle_approval_key(app: &mut App, key: KeyCode) {
    let approval = app.pending_approval.as_mut().unwrap();
    match key {
        // vim navigation
        KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => {
            approval.selected = approval.selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => {
            if approval.selected < ApprovalState::OPTIONS.len() - 1 {
                approval.selected += 1;
            }
        }
        // number keys for direct selection
        KeyCode::Char(c @ '1'..='6') => {
            let idx = (c as u8 - b'1') as usize;
            if idx < ApprovalState::OPTIONS.len() {
                let response = ApprovalState::OPTIONS[idx].1.clone();
                let approval = app.pending_approval.take().unwrap();
                approval.respond.send(response).ok();
            }
        }
        KeyCode::Enter => {
            let response = ApprovalState::OPTIONS[approval.selected].1.clone();
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(response).ok();
        }
        // customize
        KeyCode::Char('c') | KeyCode::Char('C') => {
            let approval = app.pending_approval.take().unwrap();
            let (arg_key, pattern) = infer_rule_fields(&approval.tool, &approval.input_preview);
            app.pending_customize = Some(crate::app::CustomizeState {
                tool: approval.tool,
                arg_key,
                pattern: pattern.clone(),
                cursor: pattern.len(),
                effect_idx: 0,
                scope_idx: 0,
                focus: 0,
                respond: approval.respond,
                network: true,
                fs_entries: vec![
                    crate::policy::FsEntry {
                        path: ".".into(),
                        read: true,
                        write: true,
                        create: true,
                        delete: true,
                    },
                ],
                fs_sub_focus: 0,
                fs_path_cursor: 0,
            });
        }
        // quick keys
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::AllowOnce).ok();
        }
        KeyCode::Char('s') | KeyCode::Char('S') => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::AllowSession).ok();
        }
        KeyCode::Char('a') | KeyCode::Char('A') => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::AllowAlways).ok();
        }
        KeyCode::Char('n') | KeyCode::Char('N') => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::DenyOnce).ok();
        }
        KeyCode::Char('d') | KeyCode::Char('D') => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::DenyAlways).ok();
        }
        KeyCode::Esc => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::DenyOnce).ok();
        }
        _ => {}
    }
}

fn handle_mouse(app: &mut App, kind: MouseEventKind, row: u16) {
    match kind {
        MouseEventKind::ScrollUp => {
            if app.pending_approval.is_none() && app.pending_customize.is_none() {
                app.scroll = app.scroll.saturating_add(3);
            }
        }
        MouseEventKind::ScrollDown => {
            if app.pending_approval.is_none() && app.pending_customize.is_none() {
                app.scroll = app.scroll.saturating_sub(3);
            }
        }
        MouseEventKind::Down(_) => {
            // Click on approval dialog options
            if let Some(ref mut approval) = app.pending_approval {
                // Approximate: dialog options start at center-ish of screen
                // Each option is one row. The dialog is centered.
                let term_h = crossterm::terminal::size().map(|(_, h)| h).unwrap_or(24);
                let dialog_h = 13u16;
                let dialog_top = term_h.saturating_sub(dialog_h) / 2;
                let first_option_row = dialog_top + 3; // border + header + blank line
                if row >= first_option_row && row < first_option_row + ApprovalState::OPTIONS.len() as u16 {
                    let idx = (row - first_option_row) as usize;
                    approval.selected = idx;
                    // Double-click-ish: select on single click
                    let response = ApprovalState::OPTIONS[idx].1.clone();
                    let approval = app.pending_approval.take().unwrap();
                    approval.respond.send(response).ok();
                }
            }
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
    let input_title = if app.thinking { " streaming... " } else { "" };
    let input_block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme.input_border)
        .title(input_title);
    let input = Paragraph::new(format!("> {}", app.input)).block(input_block);
    frame.render_widget(input, chunks[2]);

    // Cursor
    if app.pending_approval.is_none() && app.pending_customize.is_none() {
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
    let status_text = if app.pending_customize.is_some() {
        format!("CUSTOMIZE — Up/Down fields, Left/Right toggle, Enter save, Esc cancel{tokens}{policy}")
    } else if app.pending_approval.is_some() {
        format!("PERMISSION — y/s/a/n/d or 1-6 or (c)ustomize{tokens}{policy}")
    } else if app.thinking {
        format!("streaming...{tokens}{policy}")
    } else {
        format!("idle{tokens}{policy} | Enter send | Esc quit")
    };
    let status = Paragraph::new(Span::styled(format!(" {status_text}"), theme.status));
    frame.render_widget(status, chunks[3]);

    // Modal overlays
    if let Some(ref customize) = app.pending_customize {
        draw_customize_dialog(frame, customize, theme);
    } else if let Some(ref approval) = app.pending_approval {
        draw_approval_dialog(frame, approval, theme);
    }
}

fn draw_approval_dialog(frame: &mut Frame, approval: &ApprovalState, theme: &Theme) {
    let area = frame.area();
    let dialog_width = 50.min(area.width.saturating_sub(4));
    let dialog_height = 13;
    let x = (area.width.saturating_sub(dialog_width)) / 2;
    let y = (area.height.saturating_sub(dialog_height)) / 2;
    let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

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

    for (i, (label, resp)) in ApprovalState::OPTIONS.iter().enumerate() {
        let is_allow = matches!(
            resp,
            ApprovalResponse::AllowOnce | ApprovalResponse::AllowSession | ApprovalResponse::AllowAlways
        );
        let base_style = if is_allow {
            theme.approval_allow
        } else {
            theme.approval_deny
        };
        let style = if i == approval.selected {
            theme.approval_selected
        } else {
            base_style
        };
        let marker = if i == approval.selected { "> " } else { "  " };
        let num = i + 1;
        lines.push(Line::from(Span::styled(
            format!("{marker}{num}. {label}"),
            style,
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  (c)ustomize rule | Esc deny once",
        theme.approval_option,
    )));

    let content = Paragraph::new(Text::from(lines));
    frame.render_widget(content, inner);
}

const EFFECTS: [&str; 2] = ["allow", "deny"];
const SCOPES: [&str; 3] = ["once", "session", "always"];

fn infer_rule_fields(tool: &str, preview: &str) -> (String, String) {
    match tool {
        "shell" => (
            "command".into(),
            format!(
                "{}*",
                preview.split_whitespace().next().unwrap_or(preview)
            ),
        ),
        "read_file" | "write_file" | "edit_file" => ("path".into(), preview.to_string()),
        _ => (String::new(), "*".into()),
    }
}

fn handle_customize_key(app: &mut App, key: KeyCode) {
    let cust = app.pending_customize.as_mut().unwrap();
    match key {
        KeyCode::Esc => {
            let cust = app.pending_customize.take().unwrap();
            cust.respond.send(ApprovalResponse::DenyOnce).ok();
        }
        // Move between fields: Tab, Shift-Tab, Up/Down
        // Fields: 0=pattern, 1=effect, 2=scope, 3=network, 4..4+N=fs entries, 4+N=add path
        KeyCode::Tab | KeyCode::Down => {
            let max_focus = 4 + cust.fs_entries.len(); // 4+N = "add path"
            cust.focus = if cust.focus >= max_focus { 0 } else { cust.focus + 1 };
            cust.fs_sub_focus = 0;
            cust.fs_path_cursor = 0;
        }
        KeyCode::BackTab | KeyCode::Up => {
            let max_focus = 4 + cust.fs_entries.len();
            cust.focus = if cust.focus == 0 { max_focus } else { cust.focus - 1 };
            cust.fs_sub_focus = 0;
            cust.fs_path_cursor = 0;
        }
        KeyCode::Enter => {
            let cust = app.pending_customize.take().unwrap();
            let response = ApprovalResponse::CustomRule {
                tool: cust.tool,
                arg_key: if cust.arg_key.is_empty() {
                    None
                } else {
                    Some(cust.arg_key)
                },
                arg_pattern: if cust.pattern.is_empty() {
                    None
                } else {
                    Some(cust.pattern)
                },
                effect: EFFECTS[cust.effect_idx].to_string(),
                scope: SCOPES[cust.scope_idx].to_string(),
                sandbox: if cust.fs_entries.is_empty() && cust.network {
                    None // no restrictions = no sandbox
                } else {
                    Some(crate::policy::SandboxConfig {
                        network: cust.network,
                        fs: cust.fs_entries.clone(),
                    })
                },
            };
            cust.respond.send(response).ok();
        }
        // Field-specific keys
        _ => match cust.focus {
            0 => {
                // Pattern: text editing
                match key {
                    KeyCode::Char(c) => {
                        cust.pattern.insert(cust.cursor, c);
                        cust.cursor += 1;
                    }
                    KeyCode::Backspace if cust.cursor > 0 => {
                        cust.cursor -= 1;
                        cust.pattern.remove(cust.cursor);
                    }
                    KeyCode::Left => cust.cursor = cust.cursor.saturating_sub(1),
                    KeyCode::Right if cust.cursor < cust.pattern.len() => cust.cursor += 1,
                    _ => {}
                }
            }
            1 => {
                // Effect: toggle
                if matches!(key, KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') | KeyCode::Char(' ')) {
                    cust.effect_idx = 1 - cust.effect_idx;
                }
            }
            2 => {
                // Scope: cycle once/session/always
                match key {
                    KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                        cust.scope_idx = (cust.scope_idx + 1) % SCOPES.len();
                    }
                    KeyCode::Left | KeyCode::Char('h') => {
                        cust.scope_idx = if cust.scope_idx == 0 { SCOPES.len() - 1 } else { cust.scope_idx - 1 };
                    }
                    _ => {}
                }
            }
            3 => {
                // Network: toggle
                if matches!(key, KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') | KeyCode::Char(' ')) {
                    cust.network = !cust.network;
                }
            }
            f if f >= 4 && f < 4 + cust.fs_entries.len() => {
                // Filesystem entry
                let idx = f - 4;
                match cust.fs_sub_focus {
                    0 => {
                        // Path sub-field: text editing, Space moves to perms
                        match key {
                            KeyCode::Char(' ') => cust.fs_sub_focus = 1,
                            KeyCode::Char(c) => {
                                cust.fs_entries[idx].path.insert(cust.fs_path_cursor, c);
                                cust.fs_path_cursor += 1;
                            }
                            KeyCode::Backspace if cust.fs_path_cursor > 0 => {
                                cust.fs_path_cursor -= 1;
                                cust.fs_entries[idx].path.remove(cust.fs_path_cursor);
                            }
                            KeyCode::Left => cust.fs_path_cursor = cust.fs_path_cursor.saturating_sub(1),
                            KeyCode::Right if cust.fs_path_cursor < cust.fs_entries[idx].path.len() => {
                                cust.fs_path_cursor += 1;
                            }
                            _ => {}
                        }
                    }
                    1..=4 => {
                        // Permission toggles: Space toggles, Left/Right moves between r/w/c/d
                        match key {
                            KeyCode::Char(' ') => {
                                match cust.fs_sub_focus {
                                    1 => cust.fs_entries[idx].read = !cust.fs_entries[idx].read,
                                    2 => cust.fs_entries[idx].write = !cust.fs_entries[idx].write,
                                    3 => cust.fs_entries[idx].create = !cust.fs_entries[idx].create,
                                    4 => cust.fs_entries[idx].delete = !cust.fs_entries[idx].delete,
                                    _ => {}
                                }
                            }
                            KeyCode::Left | KeyCode::Char('h') => {
                                cust.fs_sub_focus = if cust.fs_sub_focus <= 1 { 0 } else { cust.fs_sub_focus - 1 };
                            }
                            KeyCode::Right | KeyCode::Char('l') => {
                                cust.fs_sub_focus = (cust.fs_sub_focus + 1).min(4);
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
                // Remove entry with x (only when on permission toggles, not path editing)
                if matches!(key, KeyCode::Char('x')) && cust.fs_sub_focus > 0 {
                    cust.fs_entries.remove(idx);
                    cust.fs_sub_focus = 0;
                    // Adjust focus if we removed the last entry
                    let max_focus = 4 + cust.fs_entries.len();
                    if cust.focus > max_focus {
                        cust.focus = max_focus;
                    }
                }
            }
            f if f == 4 + cust.fs_entries.len() => {
                // "Add path" action
                if matches!(key, KeyCode::Enter | KeyCode::Char(' ')) {
                    cust.fs_entries.push(crate::policy::FsEntry {
                        path: String::new(),
                        read: true,
                        write: false,
                        create: false,
                        delete: false,
                    });
                    cust.focus = 4 + cust.fs_entries.len() - 1;
                    cust.fs_sub_focus = 0;
                    cust.fs_path_cursor = 0;
                }
            }
            _ => {}
        },
    }
}

fn draw_customize_dialog(frame: &mut Frame, cust: &crate::app::CustomizeState, theme: &Theme) {
    let area = frame.area();
    let dialog_width = 56.min(area.width.saturating_sub(4));
    // Height: 5 fixed fields + 1 separator + 1 network + fs entries + 1 add + 1 help + 2 border
    let dialog_height = (12 + cust.fs_entries.len() as u16).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(dialog_width)) / 2;
    let y = (area.height.saturating_sub(dialog_height)) / 2;
    let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

    frame.render_widget(Clear, dialog_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.approval_border)
        .title(Span::styled(" Customize Rule ", theme.approval_title));

    let inner = block.inner(dialog_area);
    frame.render_widget(block, dialog_area);

    let sel = theme.approval_selected;
    let dim = theme.approval_option;
    let effect_color = if EFFECTS[cust.effect_idx] == "allow" {
        theme.approval_allow
    } else {
        theme.approval_deny
    };
    let net_color = if cust.network {
        theme.approval_allow
    } else {
        theme.approval_deny
    };

    let f = |field: usize, label: &str| -> Span {
        Span::styled(label.to_string(), if cust.focus == field { sel } else { dim })
    };

    let pattern_label = match cust.tool.as_str() {
        "shell" => "  Command: ",
        "read_file" | "write_file" | "edit_file" => "  Path:    ",
        _ => "  Match:   ",
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled("  Tool:    ", dim),
            Span::styled(&cust.tool, theme.approval_tool),
        ]),
        Line::from(vec![f(0, pattern_label), Span::styled(format!("[{}]", cust.pattern), if cust.focus == 0 { sel } else { dim })]),
        Line::from(vec![f(1, "  Effect:  "), Span::styled(format!("< {} >", EFFECTS[cust.effect_idx]), if cust.focus == 1 { sel } else { effect_color })]),
        Line::from(vec![f(2, "  Scope:   "), Span::styled(format!("< {} >", SCOPES[cust.scope_idx]), if cust.focus == 2 { sel } else { dim })]),
        Line::from(Span::styled("  ── Sandbox ──", dim)),
        Line::from(vec![f(3, "  Network: "), Span::styled(format!("< {} >", if cust.network { "allow" } else { "deny" }), if cust.focus == 3 { sel } else { net_color })]),
        Line::from(Span::styled("  Filesystem:", dim)),
    ];

    for (i, entry) in cust.fs_entries.iter().enumerate() {
        let field_idx = 4 + i;
        let is_focused = cust.focus == field_idx;

        let path_style = if is_focused && cust.fs_sub_focus == 0 { sel } else { dim };
        let mut spans = vec![
            Span::styled("    ", dim),
            Span::styled(format!("{:<16}", entry.path), path_style),
            Span::styled(" ", dim),
        ];

        let perms = [
            ("r", entry.read, 1),
            ("w", entry.write, 2),
            ("c", entry.create, 3),
            ("d", entry.delete, 4),
        ];
        for (label, enabled, sub_idx) in &perms {
            let perm_focused = is_focused && cust.fs_sub_focus == *sub_idx;
            let style = if perm_focused {
                sel
            } else if *enabled {
                theme.approval_allow
            } else {
                theme.approval_deny
            };
            let display = if *enabled {
                label.to_uppercase()
            } else {
                "-".to_string()
            };
            spans.push(Span::styled(display, style));
        }

        spans.push(Span::styled(
            if is_focused { "  (x)rm" } else { "" },
            dim,
        ));

        lines.push(Line::from(spans));
    }

    // "Add path" action
    let add_focused = cust.focus == 4 + cust.fs_entries.len();
    lines.push(Line::from(Span::styled(
        "    + add path",
        if add_focused { sel } else { dim },
    )));

    lines.push(Line::from(Span::styled(
        "  Up/Down fields | Space toggle | Enter save | Esc cancel",
        dim,
    )));

    let content = Paragraph::new(Text::from(lines));
    frame.render_widget(content, inner);
}

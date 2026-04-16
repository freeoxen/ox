use crate::theme::Theme;
use crate::types::APPROVAL_OPTIONS;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

pub(crate) const EFFECTS: [&str; 2] = ["allow", "deny"];
pub(crate) const SCOPES: [&str; 3] = ["once", "session", "always"];
pub(crate) const NETWORKS: [&str; 3] = ["deny", "allow", "localhost"];

/// Decompose a tool call into editable arg strings.
pub(crate) fn infer_args(tool: &str, preview: &str) -> Vec<String> {
    match tool {
        "shell" => preview.split_whitespace().map(|s| s.to_string()).collect(),
        "read_file" | "write_file" | "edit_file" => vec![preview.to_string()],
        _ => vec![],
    }
}

/// Build a clash Node from the customize state.
#[allow(dead_code)]
pub(crate) fn build_node_from_customize(
    cust: &crate::types::CustomizeState,
) -> clash::policy::match_tree::Node {
    use clash::policy::match_tree::*;

    let sandbox_ref = if EFFECTS[cust.effect_idx] == "allow"
        && (cust.network_idx != 1 || !cust.fs_rules.is_empty())
    {
        Some(SandboxRef(format!("ox-{}", cust.tool)))
    } else {
        None
    };
    let decision = if EFFECTS[cust.effect_idx] == "allow" {
        Decision::Allow(sandbox_ref)
    } else {
        Decision::Deny
    };
    let leaf = Node::Decision(decision);

    if cust.tool == "shell" {
        // Build ToolName -> arg0 -> arg1 -> ... -> Decision
        let mut current = leaf;
        for (i, arg) in cust.args.iter().enumerate().rev() {
            let pattern = if arg == "*" {
                Pattern::Wildcard
            } else {
                Pattern::Literal(Value::Literal(arg.clone()))
            };
            current = Node::Condition {
                observe: Observable::PositionalArg(i as i32),
                pattern,
                children: vec![current],
                doc: None,
                source: None,
                terminal: false,
            };
        }
        Node::Condition {
            observe: Observable::ToolName,
            pattern: Pattern::Literal(Value::Literal(cust.tool.clone())),
            children: vec![current],
            doc: None,
            source: Some("ox-cli".into()),
            terminal: false,
        }
    } else if let Some(path) = cust.args.first() {
        // File tool: ToolName -> NamedArg("path") -> Decision
        Node::Condition {
            observe: Observable::ToolName,
            pattern: Pattern::Literal(Value::Literal(cust.tool.clone())),
            children: vec![Node::Condition {
                observe: Observable::NamedArg("path".into()),
                pattern: Pattern::Literal(Value::Literal(path.clone())),
                children: vec![leaf],
                doc: None,
                source: None,
                terminal: false,
            }],
            doc: None,
            source: Some("ox-cli".into()),
            terminal: false,
        }
    } else {
        Node::Condition {
            observe: Observable::ToolName,
            pattern: Pattern::Literal(Value::Literal(cust.tool.clone())),
            children: vec![leaf],
            doc: None,
            source: Some("ox-cli".into()),
            terminal: false,
        }
    }
}

/// Build a sandbox from the customize state. Returns None if no restrictions.
#[allow(dead_code)]
pub(crate) fn build_sandbox_from_customize(
    cust: &crate::types::CustomizeState,
) -> Option<(String, clash::policy::sandbox_types::SandboxPolicy)> {
    use clash::policy::sandbox_types::*;

    let network = match cust.network_idx {
        0 => NetworkPolicy::Deny,
        2 => NetworkPolicy::Localhost,
        _ => NetworkPolicy::Allow,
    };

    let rules: Vec<SandboxRule> = cust
        .fs_rules
        .iter()
        .map(|r| {
            let mut caps = Cap::empty();
            if r.read {
                caps |= Cap::READ;
            }
            if r.write {
                caps |= Cap::WRITE;
            }
            if r.create {
                caps |= Cap::CREATE;
            }
            if r.delete {
                caps |= Cap::DELETE;
            }
            if r.execute {
                caps |= Cap::EXECUTE;
            }
            SandboxRule {
                effect: RuleEffect::Allow,
                caps,
                path: r.path.clone(),
                path_match: PathMatch::Subpath,
                follow_worktrees: false,
                doc: None,
            }
        })
        .collect();

    // Skip sandbox if it's fully permissive (all allow, no fs restrictions)
    if matches!(network, NetworkPolicy::Allow) && rules.is_empty() {
        return None;
    }

    let name = format!("ox-{}", cust.tool);
    Some((
        name,
        SandboxPolicy {
            default: Cap::READ | Cap::EXECUTE,
            rules,
            network,
            doc: Some(format!("sandbox for {}", cust.tool)),
        },
    ))
}

pub(crate) fn draw_shortcuts_modal(
    frame: &mut Frame,
    key_hints: &[ox_types::KeyHint],
    mode: &str,
    screen: &str,
    theme: &Theme,
) {
    let area = frame.area();
    let key_style = Style::default().add_modifier(Modifier::BOLD);
    let desc_style = Style::default();
    let footer_style = theme.status;

    // Group hints by command, preserving first-seen order.
    // Within each group, status_hint keys come first (the "recommended" form).
    let mut groups: Vec<ShortcutGroup> = Vec::new();
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for h in key_hints {
        let group_key = if h.command.is_empty() {
            // No command — use description as group key (fallback)
            h.description.clone()
        } else {
            h.command.clone()
        };

        if let Some(&idx) = seen.get(&group_key) {
            if h.status_hint {
                // Promoted: status_hint key becomes primary, old primary becomes alt
                let old_primary = groups[idx].primary_key.clone();
                groups[idx].alt_keys.insert(0, old_primary);
                groups[idx].primary_key = h.key.clone();
                groups[idx].description = h.description.clone();
            } else {
                groups[idx].alt_keys.push(h.key.clone());
            }
        } else {
            seen.insert(group_key, groups.len());
            groups.push(ShortcutGroup {
                primary_key: h.key.clone(),
                description: h.description.clone(),
                alt_keys: Vec::new(),
            });
        }
    }

    // Build "keys" column: "j/Down", "Enter/Space", etc.
    let key_labels: Vec<String> = groups
        .iter()
        .map(|g| {
            let mut keys = vec![g.primary_key.clone()];
            keys.extend(g.alt_keys.iter().cloned());
            keys.join("/")
        })
        .collect();
    let key_col_width = key_labels.iter().map(|k| k.len()).max().unwrap_or(6);

    let content_lines: Vec<Line> = groups
        .iter()
        .zip(key_labels.iter())
        .map(|(g, keys)| {
            Line::from(vec![
                Span::styled(format!("  {keys:<key_col_width$}"), key_style),
                Span::styled(format!("  {}", g.description), desc_style),
            ])
        })
        .collect();

    let line_count = content_lines.len() as u16 + 4; // +2 border +1 blank +1 footer
    let max_width = content_lines.iter().map(|l| l.width()).max().unwrap_or(30) as u16 + 4;
    let dialog_width = max_width.clamp(30, area.width.saturating_sub(4));
    let dialog_height = line_count.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(dialog_width)) / 2;
    let y = (area.height.saturating_sub(dialog_height)) / 2;
    let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

    frame.render_widget(Clear, dialog_area);

    let title = format!(" {mode}/{screen} ");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default())
        .title(Span::styled(title, key_style));
    let inner = block.inner(dialog_area);
    frame.render_widget(block, dialog_area);

    let mut lines = content_lines;
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  ? or Esc to close",
        footer_style,
    )));

    let content = Paragraph::new(Text::from(lines));
    frame.render_widget(content, inner);
}

/// A group of bindings that map to the same command.
struct ShortcutGroup {
    /// The recommended key (first seen, or status_hint key).
    primary_key: String,
    /// Human-readable description.
    description: String,
    /// Alternate keys for the same command.
    alt_keys: Vec<String>,
}

pub(crate) fn draw_approval_dialog(
    frame: &mut Frame,
    tool: &str,
    input_preview: &str,
    selected: usize,
    theme: &Theme,
) {
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
            Span::styled(format!("[{tool}] "), theme.approval_tool),
            Span::styled(input_preview, theme.approval_preview),
        ]),
        Line::from(""),
    ];

    for (i, (label, decision)) in APPROVAL_OPTIONS.iter().enumerate() {
        let base_style = if decision.is_allow() {
            theme.approval_allow
        } else {
            theme.approval_deny
        };
        let style = if i == selected {
            theme.approval_selected
        } else {
            base_style
        };
        let marker = if i == selected { "> " } else { "  " };
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

pub(crate) fn draw_customize_dialog(
    frame: &mut Frame,
    cust: &crate::types::CustomizeState,
    theme: &Theme,
) {
    let area = frame.area();
    let dialog_width = 58.min(area.width.saturating_sub(4));
    let dialog_height = (10 + cust.args.len() as u16 + cust.fs_rules.len() as u16)
        .min(area.height.saturating_sub(4));
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
    let net_color = if cust.network_idx == 1 {
        theme.approval_allow
    } else {
        theme.approval_deny
    };

    let mut lines = vec![Line::from(vec![
        Span::styled("  Tool:  ", dim),
        Span::styled(&cust.tool, theme.approval_tool),
    ])];

    // Arg fields
    let arg_label = if cust.tool == "shell" { "arg" } else { "path" };
    for (i, arg) in cust.args.iter().enumerate() {
        let focused = cust.focus == i;
        let label = if cust.tool == "shell" {
            format!("  {arg_label} {i}: ")
        } else {
            format!("  {arg_label}:   ")
        };
        lines.push(Line::from(vec![
            Span::styled(label, if focused { sel } else { dim }),
            Span::styled(format!("[{arg}]"), if focused { sel } else { dim }),
        ]));
    }
    if cust.tool == "shell" {
        let add_focused = cust.focus == cust.add_arg_field();
        lines.push(Line::from(Span::styled(
            "  + add argument (Space)",
            if add_focused { sel } else { dim },
        )));
    }

    let ef = cust.effect_field();
    let sf = cust.scope_field();
    lines.push(Line::from(vec![
        Span::styled("  Effect:  ", if cust.focus == ef { sel } else { dim }),
        Span::styled(
            format!("< {} >", EFFECTS[cust.effect_idx]),
            if cust.focus == ef { sel } else { effect_color },
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Scope:   ", if cust.focus == sf { sel } else { dim }),
        Span::styled(
            format!("< {} >", SCOPES[cust.scope_idx]),
            if cust.focus == sf { sel } else { dim },
        ),
    ]));

    // Sandbox section
    let nf = cust.network_field();
    lines.push(Line::from(Span::styled("  -- Sandbox --", dim)));
    lines.push(Line::from(vec![
        Span::styled("  Network: ", if cust.focus == nf { sel } else { dim }),
        Span::styled(
            format!("< {} >", NETWORKS[cust.network_idx]),
            if cust.focus == nf { sel } else { net_color },
        ),
    ]));

    let fs_start = cust.fs_start();
    for (i, rule) in cust.fs_rules.iter().enumerate() {
        let is_focused = cust.focus == fs_start + i;
        let path_style = if is_focused && cust.fs_sub_focus == 0 {
            sel
        } else {
            dim
        };
        let mut spans = vec![
            Span::styled("    ", dim),
            Span::styled(format!("{:<14}", rule.path), path_style),
            Span::styled(" ", dim),
        ];
        for (label, enabled, sub_idx) in [
            ("r", rule.read, 1),
            ("w", rule.write, 2),
            ("c", rule.create, 3),
            ("d", rule.delete, 4),
            ("x", rule.execute, 5),
        ] {
            let pf = is_focused && cust.fs_sub_focus == sub_idx;
            let st = if pf {
                sel
            } else if enabled {
                theme.approval_allow
            } else {
                theme.approval_deny
            };
            spans.push(Span::styled(
                if enabled {
                    label.to_uppercase()
                } else {
                    "-".into()
                },
                st,
            ));
        }
        if is_focused && cust.fs_sub_focus > 0 {
            spans.push(Span::styled(" (x)rm", dim));
        }
        lines.push(Line::from(spans));
    }
    let add_fs_focused = cust.focus == cust.add_fs_field();
    lines.push(Line::from(Span::styled(
        "    + add path (Space)",
        if add_fs_focused { sel } else { dim },
    )));

    lines.push(Line::from(Span::styled(
        "  Up/Down | Space toggle | Enter save | Esc cancel",
        dim,
    )));

    let content = Paragraph::new(Text::from(lines));
    frame.render_widget(content, inner);
}

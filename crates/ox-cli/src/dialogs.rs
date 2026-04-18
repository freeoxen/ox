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

/// Decompose a tool call's raw input into editable arg strings.
pub(crate) fn infer_args_from_input(tool: &str, input: &serde_json::Value) -> Vec<String> {
    match tool {
        "shell" => {
            let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            cmd.split_whitespace().map(|s| s.to_string()).collect()
        }
        "read_file" | "write_file" | "edit_file" => {
            let path = input
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            vec![path]
        }
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
            keys.join(" / ")
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
    tool_input: &serde_json::Value,
    selected: usize,
    preview_scroll: usize,
    theme: &Theme,
) {
    let area = frame.area();

    // Build tool-specific preview from structured input.
    let (header, detail) = build_approval_preview(tool, tool_input);

    let prefix = format!("[{tool}] ");
    let header_width = prefix.len() + header.len();
    let detail_width = detail.iter().map(|l| l.text.len() + 2).max().unwrap_or(0);
    let content_width = header_width.max(detail_width);
    let min_width = 50u16;
    let max_width = area.width.saturating_sub(4);
    let dialog_width = (content_width as u16 + 4).clamp(min_width, max_width);
    let inner_width = dialog_width.saturating_sub(2) as usize;

    // Wrap header if needed.
    let first_avail = inner_width.saturating_sub(prefix.len());
    let wrapped_first: Vec<&str> = if first_avail > 0 && header.len() > first_avail {
        header
            .as_bytes()
            .chunks(first_avail)
            .map(|chunk| std::str::from_utf8(chunk).unwrap_or(""))
            .collect()
    } else {
        vec![&header]
    };

    // Reserve room for chrome: header + blank + 6 options + blank + footer + borders.
    let chrome_lines = wrapped_first.len() + 11;
    let max_detail = (area.height as usize).saturating_sub(chrome_lines + 4);
    let total_detail = detail.len();
    let scroll = preview_scroll.min(total_detail.saturating_sub(max_detail.max(1)));
    let shown_end = (scroll + max_detail).min(total_detail);
    let visible_detail = if max_detail > 0 && !detail.is_empty() {
        &detail[scroll..shown_end]
    } else {
        &[]
    };

    let dialog_height =
        (chrome_lines as u16 + visible_detail.len() as u16 + 2).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(dialog_width)) / 2;
    let y = (area.height.saturating_sub(dialog_height)) / 2;
    let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

    frame.render_widget(Clear, dialog_area);

    let mut title_parts = vec![Span::styled(" Permission Required ", theme.approval_title)];
    if total_detail > max_detail {
        title_parts.push(Span::styled(
            format!(
                " {}/{} ",
                scroll + 1,
                total_detail.saturating_sub(max_detail) + 1
            ),
            theme.tool_meta,
        ));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.approval_border)
        .title(Line::from(title_parts));
    let inner = block.inner(dialog_area);
    frame.render_widget(block, dialog_area);

    let mut lines = Vec::new();

    // Header: [tool] primary_info
    if let Some(first) = wrapped_first.first() {
        lines.push(Line::from(vec![
            Span::styled(&prefix, theme.approval_tool),
            Span::styled((*first).to_string(), theme.approval_preview),
        ]));
    }
    let indent = " ".repeat(prefix.len());
    for continuation in wrapped_first.iter().skip(1) {
        lines.push(Line::from(vec![
            Span::raw(indent.clone()),
            Span::styled((*continuation).to_string(), theme.approval_preview),
        ]));
    }

    // Detail lines (scrollable)
    if !detail.is_empty() {
        for pl in visible_detail {
            lines.push(Line::from(Span::styled(
                format!("  {}", pl.text),
                pl.style(theme),
            )));
        }
        if shown_end < total_detail {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} more (j/k scroll)", total_detail - shown_end),
                theme.tool_meta,
            )));
        }
    }

    lines.push(Line::from(""));

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
        "  (c)ustomize rule | j/k scroll | Esc deny once",
        theme.approval_option,
    )));

    let content = Paragraph::new(Text::from(lines));
    frame.render_widget(content, inner);
}

/// A styled line in the approval preview.
struct PreviewLine {
    text: String,
    kind: PreviewLineKind,
}

enum PreviewLineKind {
    Content,
    DiffAdd,
    DiffRemove,
    DiffContext,
    Info,
}

impl PreviewLine {
    fn style(&self, theme: &Theme) -> Style {
        match self.kind {
            PreviewLineKind::Content => theme.approval_preview,
            PreviewLineKind::DiffAdd => theme.approval_allow,
            PreviewLineKind::DiffRemove => theme.approval_deny,
            PreviewLineKind::DiffContext => theme.approval_preview,
            PreviewLineKind::Info => theme.tool_meta,
        }
    }
}

/// Build structured preview from tool input. Returns (header, detail_lines).
fn build_approval_preview(tool: &str, input: &serde_json::Value) -> (String, Vec<PreviewLine>) {
    let get_str = |key: &str| -> &str { input.get(key).and_then(|v| v.as_str()).unwrap_or("") };

    match tool {
        "shell" => (get_str("command").to_string(), Vec::new()),
        "read_file" => (get_str("path").to_string(), Vec::new()),
        "write_file" => {
            let path = get_str("path");
            let content = get_str("content");
            let lines: Vec<PreviewLine> = content
                .lines()
                .map(|l| PreviewLine {
                    text: l.to_string(),
                    kind: PreviewLineKind::Content,
                })
                .collect();
            (path.to_string(), lines)
        }
        "edit_file" => {
            let path = get_str("path");
            let old = get_str("old_string");
            let new = get_str("new_string");
            let diff = compute_unified_diff(old, new);
            (path.to_string(), diff)
        }
        _ => {
            let s = serde_json::to_string_pretty(input).unwrap_or_default();
            let lines: Vec<PreviewLine> = s
                .lines()
                .map(|l| PreviewLine {
                    text: l.to_string(),
                    kind: PreviewLineKind::Content,
                })
                .collect();
            (tool.to_string(), lines)
        }
    }
}

/// Compute a unified diff with context between old and new text.
fn compute_unified_diff(old: &str, new: &str) -> Vec<PreviewLine> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    // LCS-based diff
    let lcs = lcs_table(&old_lines, &new_lines);
    let mut chunks: Vec<PreviewLine> = Vec::new();
    let mut i = old_lines.len();
    let mut j = new_lines.len();

    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old_lines[i - 1] == new_lines[j - 1] {
            chunks.push(PreviewLine {
                text: format!(" {}", old_lines[i - 1]),
                kind: PreviewLineKind::DiffContext,
            });
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || lcs[i][j - 1] >= lcs[i - 1][j]) {
            chunks.push(PreviewLine {
                text: format!("+{}", new_lines[j - 1]),
                kind: PreviewLineKind::DiffAdd,
            });
            j -= 1;
        } else if i > 0 {
            chunks.push(PreviewLine {
                text: format!("-{}", old_lines[i - 1]),
                kind: PreviewLineKind::DiffRemove,
            });
            i -= 1;
        }
    }
    chunks.reverse();

    // Trim to 3 lines of context around changes.
    let is_change: Vec<bool> = chunks
        .iter()
        .map(|c| !matches!(c.kind, PreviewLineKind::DiffContext))
        .collect();
    let context = 3;
    let mut show = vec![false; chunks.len()];
    for (idx, _) in is_change.iter().enumerate().filter(|&(_, &v)| v) {
        let start = idx.saturating_sub(context);
        let end = (idx + context + 1).min(chunks.len());
        for s in &mut show[start..end] {
            *s = true;
        }
    }

    let mut result = Vec::new();
    let mut last_shown = false;
    for (idx, chunk) in chunks.into_iter().enumerate() {
        if show[idx] {
            result.push(chunk);
            last_shown = true;
        } else if last_shown {
            result.push(PreviewLine {
                text: "···".to_string(),
                kind: PreviewLineKind::Info,
            });
            last_shown = false;
        }
    }
    result
}

/// LCS length table for line-level diff.
fn lcs_table(a: &[&str], b: &[&str]) -> Vec<Vec<usize>> {
    let m = a.len();
    let n = b.len();
    let mut table = vec![vec![0usize; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            if a[i - 1] == b[j - 1] {
                table[i][j] = table[i - 1][j - 1] + 1;
            } else {
                table[i][j] = table[i - 1][j].max(table[i][j - 1]);
            }
        }
    }
    table
}

/// Returns `(row, col, url, text)` if a hyperlink should be rendered via OSC 8.
pub(crate) fn draw_usage_dialog(
    frame: &mut Frame,
    model: &str,
    session_tokens: &ox_types::TokenUsage,
    last_run_tokens: &ox_types::TokenUsage,
    per_model_usage: &[(String, ox_types::TokenUsage)],
    pricing_overrides: &std::collections::BTreeMap<String, ox_gate::pricing::ModelPricing>,
    theme: &Theme,
) -> Option<crate::tui::PendingHyperlink> {
    use ox_gate::pricing;

    let area = frame.area();
    let pricing_info = pricing::model_pricing_with_overrides(model, pricing_overrides);
    let url = pricing::pricing_url(model);

    let mut lines: Vec<Line> = Vec::new();

    // Model
    lines.push(Line::from(vec![
        Span::styled("  Model:   ", Style::default()),
        Span::styled(model, Style::default().add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(""));

    // Rates
    if let Some(p) = pricing_info {
        lines.push(Line::from(Span::styled(
            "  Rates (per million tokens)",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(format!(
            "    Input:   ${:.2}/Mtok",
            p.input_per_mtok
        )));
        lines.push(Line::from(format!(
            "    Output:  ${:.2}/Mtok",
            p.output_per_mtok
        )));
        if p.cache_creation_multiplier != 1.0 || p.cache_read_multiplier != 1.0 {
            lines.push(Line::from(format!(
                "    Cache:   {:.0}% write, {:.0}% read",
                p.cache_creation_multiplier * 100.0,
                p.cache_read_multiplier * 100.0,
            )));
        }
        lines.push(Line::from(""));
    }

    // Last query breakdown
    if last_run_tokens.input_tokens > 0 || last_run_tokens.output_tokens > 0 {
        lines.push(Line::from(Span::styled(
            "  Last query",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.extend(usage_section_lines(
            last_run_tokens,
            model,
            pricing_overrides,
        ));
        lines.push(Line::from(""));
    }

    // Session totals
    lines.push(Line::from(Span::styled(
        "  Session total",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    if per_model_usage.len() > 1 {
        // Multi-model: show per-model breakdown with each model's own pricing
        for (m, usage) in per_model_usage {
            lines.push(Line::from(format!("    {m}")));
            lines.extend(usage_section_lines(usage, m, pricing_overrides));
        }
    } else if session_tokens.input_tokens > 0 || session_tokens.output_tokens > 0 {
        lines.extend(usage_section_lines(
            session_tokens,
            model,
            pricing_overrides,
        ));
    } else if pricing_info.is_none() {
        lines.push(Line::from("  (pricing unavailable for this model)"));
    }

    // Source URL — track line index for OSC 8 hyperlink
    let url_line_idx = if !url.is_empty() {
        lines.push(Line::from(""));
        let idx = lines.len();
        let prefix = "  Source: ";
        lines.push(Line::from(vec![
            Span::styled(prefix, theme.status),
            Span::styled(
                url.to_string(),
                theme.status.add_modifier(Modifier::UNDERLINED),
            ),
        ]));
        Some((idx, prefix.len()))
    } else {
        None
    };

    // Footer
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("  Esc to close", theme.status)));

    let content_width = lines.iter().map(|l| l.width()).max().unwrap_or(30) as u16 + 4;
    let dialog_width = content_width.clamp(40, area.width.saturating_sub(4));
    let dialog_height = (lines.len() as u16 + 2).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(dialog_width)) / 2;
    let y = (area.height.saturating_sub(dialog_height)) / 2;
    let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

    frame.render_widget(Clear, dialog_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default())
        .title(Span::styled(
            " Usage & Cost ",
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(dialog_area);
    frame.render_widget(block, dialog_area);

    let content = Paragraph::new(Text::from(lines));
    frame.render_widget(content, inner);

    // Return hyperlink info for OSC 8 post-render
    url_line_idx.map(|(line_idx, prefix_len)| crate::tui::PendingHyperlink {
        row: inner.y + line_idx as u16,
        col: inner.x + prefix_len as u16,
        url: url.to_string(),
        text: url.to_string(),
    })
}

/// Render a token usage section (tokens + cost) as dialog lines.
fn usage_section_lines(
    usage: &ox_types::TokenUsage,
    model: &str,
    overrides: &std::collections::BTreeMap<String, ox_gate::pricing::ModelPricing>,
) -> Vec<Line<'static>> {
    use ox_gate::pricing;

    let mut out = Vec::new();
    let has_cache = usage.cache_creation_input_tokens > 0 || usage.cache_read_input_tokens > 0;

    out.push(Line::from(format!(
        "    Input:   {:>8} tokens",
        format_with_commas(usage.input_tokens)
    )));
    if has_cache {
        if usage.cache_creation_input_tokens > 0 {
            out.push(Line::from(format!(
                "      cache write {:>8}",
                format_with_commas(usage.cache_creation_input_tokens)
            )));
        }
        if usage.cache_read_input_tokens > 0 {
            out.push(Line::from(format!(
                "      cache read  {:>8}",
                format_with_commas(usage.cache_read_input_tokens)
            )));
        }
    }
    out.push(Line::from(format!(
        "    Output:  {:>8} tokens",
        format_with_commas(usage.output_tokens)
    )));

    if let Some(cost) = pricing::estimate_cost_full_with_overrides(
        model,
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_creation_input_tokens,
        usage.cache_read_input_tokens,
        overrides,
    ) {
        out.push(Line::from(vec![
            Span::raw("    Cost:    "),
            Span::styled(
                format!("${:.6}", cost),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    out
}

/// Format a u32 with comma separators: 1234567 → "1,234,567".
fn format_with_commas(n: u32) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result
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

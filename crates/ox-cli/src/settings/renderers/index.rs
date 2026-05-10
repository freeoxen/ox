//! Renderer for `settings/index` — the accordion tree view.
//!
//! All settings browsing happens here. Top-level entries (Accounts,
//! Models) are always shown; children appear inline only when the
//! entry is in the expanded set
//! (`super::super::visible_rows::read_expanded_set`). The cursor
//! identifies the focused row by path; the renderer maps cursor →
//! row index for display selection.
//!
//! Indentation is encoded in the `primary` string ("  alpha") so
//! the existing `View::List` translator covers the tree shape
//! without a new variant. The expanded-state marker is a leading
//! glyph on the primary text; the legacy badge slot still carries
//! the entry's badge string.

use ox_view::{FocusId, ListItem, ModifierSet, Span, Style, View};

use crate::settings::commands::account_model::{AUTH_DISPLAY, resolve_protocol_options};
use crate::settings::commands::edit::read_edit_state;
use crate::settings::registry::{AscendRule, RenderCtx, Renderer, RendererRegistry};
use crate::settings::visible_rows::{self, RowKind};

pub struct IndexRenderer;

impl Renderer for IndexRenderer {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View {
        let rows = visible_rows::enumerate(ctx.data);
        let edit_state = read_edit_state(ctx.data);

        let cursor = read_cursor(ctx.data);
        let selected = cursor
            .as_ref()
            .and_then(|c| visible_rows::position_of(&rows, c));

        // Resolve the Protocol carousel's option list once per frame, only
        // when the focused row actually is a Protocol field. Doing this
        // here (rather than inside the per-row closure) keeps the broker
        // read out of the iteration's borrow scope and avoids paying the
        // resolution cost for every visible row.
        let protocol_options: Vec<String> = selected
            .and_then(|i| rows.get(i))
            .filter(|r| {
                matches!(
                    &r.kind,
                    RowKind::AccountField {
                        field: ox_types::AccountField::Protocol,
                        ..
                    }
                )
            })
            .map(|r| {
                let current = r.label.split(": ").nth(1).unwrap_or("");
                resolve_protocol_options(ctx.data, current)
            })
            .unwrap_or_default();

        let mut items: Vec<ListItem> = rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let indent = "  ".repeat(row.depth);
                let glyph = if row.expandable {
                    if row.expanded { "▾ " } else { "▸ " }
                } else {
                    "  "
                };
                // Selector rows render a flanked carousel only when
                // they're the focused row — that's the visual cue
                // that h/l will cycle. Other rows fall through to
                // the plain label.
                let is_focused = selected.is_some_and(|sel| sel == i);
                if is_focused {
                    if let Some(spans) =
                        selector_carousel_spans(row, &indent, glyph, &protocol_options)
                    {
                        return ListItem {
                            primary: format!("{indent}{glyph}{}", row.label),
                            primary_spans: Some(spans),
                            secondary: row.secondary.clone(),
                            badge: row.badge.clone(),
                            focus: Some(FocusId(row.path.clone())),
                        };
                    }
                }
                let label = decorate_row_label(row, edit_state.as_ref());
                ListItem {
                    primary: format!("{indent}{glyph}{label}"),
                    primary_spans: None,
                    secondary: row.secondary.clone(),
                    badge: row.badge.clone(),
                    focus: Some(FocusId(row.path.clone())),
                }
            })
            .collect();

        let mut selected = selected;

        // Always emit an affordance directly after the expanded Accounts
        // header. When compose mode is active (the buffer is `Some(_)`),
        // it's an inline name prompt reflecting the live buffer; when
        // inactive, it's the static "+ New connection" line. `focus: None`
        // keeps j/k from landing on it, so the affordance is purely
        // decorative — the dispatcher's compose-mode pass handles input
        // routing.
        let buffer: Option<String> =
            crate::settings::renderers::util::read_typed(
                ctx.data,
                &ox_path::oxpath!("ui", "settings", "new_account", "buffer"),
            );
        if let Some(insert_idx) = find_accounts_header_followup_idx(&rows) {
            let primary = match &buffer {
                Some(buf) => format!("    Name▸ {}\u{258F}", buf),
                None => "    + New connection".to_string(),
            };
            let affordance = ListItem {
                primary,
                primary_spans: None,
                secondary: None,
                badge: None,
                focus: None,
            };
            items.insert(insert_idx, affordance);
            selected = selected.map(|s| if s >= insert_idx { s + 1 } else { s });
        }

        // Pending-delete confirmation banner. Emitted as a ListItem
        // prepended to the items vector when ui/settings/pending_delete
        // is Some(name). Decoration only — focus: None; j/k skips it.
        let pending: Option<String> = crate::settings::renderers::util::read_typed(
            ctx.data,
            &ox_path::oxpath!("ui", "settings", "pending_delete"),
        );
        if let Some(name) = pending {
            let banner = ListItem {
                primary: format!("Delete '{}'? y / n", name),
                primary_spans: None,
                secondary: None,
                badge: None,
                focus: None,
            };
            items.insert(0, banner);
            selected = selected.map(|s| s + 1);
        }

        let selected = selected.filter(|i| !items.is_empty() && *i < items.len());

        // Right-aligned dirty indicator on the title bar. Reads
        // `config/_dirty` (a sentinel ConfigStore exposes that returns
        // true when its runtime layer differs from the last persisted
        // state). Renders nothing when clean — the title bar shows
        // just "Settings" — so users get an at-a-glance signal that
        // edits exist but haven't been saved yet.
        let title_right = read_dirty_indicator(ctx.data);

        View::Frame {
            title: Some("Settings".into()),
            title_right,
            content: Box::new(View::List { items, selected }),
        }
    }

    fn ascend_to(&self) -> AscendRule {
        AscendRule::ExitScreen
    }
}

/// Find the index right AFTER the Accounts entry header in the
/// visible-rows enumeration. Returns `None` when the Accounts entry
/// isn't expanded (or doesn't exist). The returned index is the
/// position in the `items` vector where the compose-mode affordance
/// should be inserted.
fn find_accounts_header_followup_idx(rows: &[visible_rows::VisibleRow]) -> Option<usize> {
    rows.iter()
        .position(|r| {
            matches!(&r.kind, RowKind::Entry { entry_id } if entry_id == "accounts") && r.expanded
        })
        .map(|i| i + 1)
}

/// For a focused selector row, build the flanked carousel as styled
/// spans — indent + glyph + label + dim prev + bright current + dim
/// next. Returns `None` when the row isn't a selector, so the caller
/// falls through to the plain label.
fn selector_carousel_spans(
    row: &visible_rows::VisibleRow,
    indent: &str,
    glyph: &str,
    protocol_options: &[String],
) -> Option<Vec<Span>> {
    // Build an owned option list per arm. Auth's options are a fixed
    // wire-protocol enum (`AUTH_DISPLAY`); Protocol's are resolved
    // per-frame from the broker (`protocol_options`). Owned strings on
    // both branches keep the formatting block below uniform.
    let (label, options, current_idx): (&str, Vec<String>, usize) = match &row.kind {
        RowKind::AccountField {
            account: _,
            field: ox_types::AccountField::Protocol,
        } => {
            // Parse the row label "Protocol: <provider>" to find the
            // current option. `safe_component`-style sanitation doesn't
            // apply here — the label embeds the literal provider string
            // from `AccountConfig.provider`.
            let value = row.label.split(": ").nth(1).unwrap_or("");
            let idx = protocol_options
                .iter()
                .position(|o| o == value)
                .unwrap_or(0);
            ("Protocol", protocol_options.to_vec(), idx)
        }
        RowKind::AccountField {
            account: _,
            field: ox_types::AccountField::Auth,
        } => {
            let value = row.label.split(": ").nth(1).unwrap_or("");
            let idx = AUTH_DISPLAY.iter().position(|o| *o == value).unwrap_or(0);
            (
                "Auth",
                AUTH_DISPLAY.iter().map(|s| s.to_string()).collect(),
                idx,
            )
        }
        _ => return None,
    };
    if options.is_empty() {
        return None;
    }
    let len = options.len();
    let prev = &options[(current_idx + len - 1) % len];
    let current = &options[current_idx];
    let next = &options[(current_idx + 1) % len];
    let dim = Style {
        fg: None,
        bg: None,
        modifiers: ModifierSet {
            dim: true,
            ..ModifierSet::default()
        },
    };
    let bright = Style {
        fg: None,
        bg: None,
        modifiers: ModifierSet {
            bold: true,
            ..ModifierSet::default()
        },
    };
    Some(vec![
        Span::plain(format!("{indent}{glyph}{label}: ")),
        Span {
            text: format!("◂ {prev}  "),
            style: dim,
        },
        Span {
            text: current.clone(),
            style: bright,
        },
        Span {
            text: format!("  {next} ▸"),
            style: dim,
        },
    ])
}

/// When the user is editing a field row, replace the row's
/// "Label: value" with "Label> buffer▏" so the live buffer (not the
/// stored data value) is what the user sees as they type. The `▏`
/// (U+258F LEFT ONE EIGHTH BLOCK) gives a visible insertion cursor.
fn decorate_row_label(
    row: &visible_rows::VisibleRow,
    edit_state: Option<&crate::settings::commands::edit::EditState>,
) -> String {
    let Some(state) = edit_state else {
        return row.label.clone();
    };
    if state.field_path != row.path {
        return row.label.clone();
    }
    let label = match &row.kind {
        RowKind::AccountField {
            field: ox_types::AccountField::Name,
            ..
        } => "Name",
        RowKind::AccountField {
            field: ox_types::AccountField::Protocol,
            ..
        } => "Protocol",
        RowKind::AccountField {
            field: ox_types::AccountField::Endpoint,
            ..
        } => "Endpoint",
        RowKind::AccountField {
            field: ox_types::AccountField::Auth,
            ..
        } => "Auth",
        RowKind::AccountField {
            field: ox_types::AccountField::Key,
            ..
        } => "Key",
        RowKind::ModelField {
            field: ox_types::ModelField::ContextSizeOverride,
            ..
        } => "max_context_size",
        RowKind::ModelField {
            field: ox_types::ModelField::OutputTokensOverride,
            ..
        } => "max_output_tokens",
        // Other row kinds aren't editable; fall through to the
        // original label.
        _ => return row.label.clone(),
    };
    // `▏` (U+258F) renders as a thin vertical bar — a clear cursor
    // mark at end-of-buffer that doesn't get confused with text.
    format!("{label}▸ {}\u{258F}", state.buffer)
}

fn read_cursor(data: &mut dyn structfs_core_store::Reader) -> Option<structfs_core_store::Path> {
    use crate::settings::commands::navigation::path_from_value;
    use ox_path::oxpath;

    // Reads the focused-widget pointer, NOT `ui/settings/cursor`. The
    // page-level cursor (binding scope) stays at `settings/index`
    // while the accordion screen is active; the focused widget inside
    // the tree lives at its own path.
    let r = data
        .read(&oxpath!("ui", "settings", "focused"))
        .ok()
        .flatten()?;
    path_from_value(r.as_value()?)
}

/// Right-aligned title-bar indicator. Returns `Some("● unsaved · Ctrl+S")`
/// when ConfigStore reports its runtime layer differs from the last
/// persisted state, `None` otherwise. Reads the `config/_dirty`
/// sentinel that ConfigStore exposes via its Reader impl.
fn read_dirty_indicator(data: &mut dyn structfs_core_store::Reader) -> Option<String> {
    use ox_path::oxpath;
    use structfs_core_store::Value;
    let dirty = data
        .read(&oxpath!("config", "_dirty"))
        .ok()
        .flatten()
        .and_then(|r| match r.as_value() {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        })
        .unwrap_or(false);
    if dirty {
        Some("● unsaved · Ctrl+S".to_string())
    } else {
        None
    }
}

pub fn register(reg: &mut RendererRegistry) {
    reg.register(
        ox_path::oxpath!("settings", "index"),
        Box::new(IndexRenderer),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    use ox_gate::AccountConfig;
    use ox_path::oxpath;
    use ox_types::{BadgeSource, SettingsIndexEntry};
    use ratatui::layout::Rect;
    use structfs_core_store::{Path, Value};
    use structfs_serde_store::to_value;

    use crate::settings::commands::navigation::path_to_value;
    use crate::settings::snapshot::SettingsSnapshot;
    use crate::settings::visible_rows::expanded_set_to_value;
    use crate::theme::Theme;

    fn entry(id: &str, label: &str, target: &str, badge: BadgeSource) -> SettingsIndexEntry {
        SettingsIndexEntry {
            id: id.to_string(),
            label: label.to_string(),
            description: String::new(),
            target_cursor: Path::parse(target).unwrap(),
            badge,
        }
    }

    fn render(snap: &mut SettingsSnapshot) -> View {
        let theme = Theme::default();
        let registry = RendererRegistry::new();
        let mut ctx = RenderCtx {
            area: Rect::new(0, 0, 80, 24),
            data: snap,
            registry: &registry,
            theme: &theme,
        };
        IndexRenderer.render(&mut ctx)
    }

    fn write_index(snap: &mut SettingsSnapshot) {
        snap.insert(
            &oxpath!("settings", "index", "entries", "accounts"),
            to_value(&entry(
                "accounts",
                "Accounts",
                "settings/accounts",
                BadgeSource::SubtreeCount(Path::parse("config/gate/accounts").unwrap()),
            ))
            .unwrap(),
        );
        snap.insert(
            &oxpath!("settings", "index", "entries", "models"),
            to_value(&entry(
                "models",
                "Models",
                "settings/models",
                BadgeSource::None,
            ))
            .unwrap(),
        );
    }

    fn write_account(snap: &mut SettingsSnapshot, name: &str) {
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp),
            to_value(&AccountConfig {
                provider: name.into(),
            })
            .unwrap(),
        );
    }

    fn assert_list(view: View) -> (Option<String>, Vec<ListItem>, Option<usize>) {
        // The IndexRenderer wraps its View::List in a View::Frame; pull
        // the title from the frame and the items/selected from the
        // inner list.
        match view {
            View::Frame {
                title,
                title_right: _,
                content,
            } => match *content {
                View::List { items, selected } => (title, items, selected),
                other => panic!("expected View::List inside Frame, got {other:?}"),
            },
            other => panic!("expected View::Frame, got {other:?}"),
        }
    }

    #[test]
    fn collapsed_renders_two_top_level_rows_with_glyphs() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        let (title, items, selected) = assert_list(render(&mut snap));
        assert_eq!(title.as_deref(), Some("Settings"));
        assert_eq!(items.len(), 2);
        assert!(
            items[0].primary.starts_with("▸ "),
            "expected collapsed glyph; got {:?}",
            items[0].primary
        );
        assert!(items[0].primary.ends_with("Accounts"));
        assert_eq!(items[0].badge.as_deref(), Some("0"));
        assert_eq!(selected, None);
    }

    #[test]
    fn expanded_renders_inline_children_with_indent() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account(&mut snap, "alpha");
        write_account(&mut snap, "beta");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/accounts".to_string()]),
        );
        let (_title, items, _selected) = assert_list(render(&mut snap));
        // Accounts (▾) + ghost + alpha (▸) + beta (▸) + Models (▸) = 5
        assert_eq!(items.len(), 5);
        assert!(items[0].primary.starts_with("▾ "));
        // Ghost row (depth 1, no expand glyph).
        assert!(
            items[1].primary.contains("+ New connection"),
            "expected ghost row at index 1; got {:?}",
            items[1].primary
        );
        assert!(
            items[1].primary.starts_with("    "),
            "ghost row at depth 1 with no expand glyph should start with four spaces; got {:?}",
            items[1].primary
        );
        // Depth-1 rows are indented two spaces and carry their own
        // expand glyph because they're expandable too.
        assert!(
            items[2].primary.starts_with("  ▸ "),
            "expected depth-1 indented expand glyph; got {:?}",
            items[2].primary
        );
        assert!(items[2].primary.ends_with("alpha"));
        assert!(items[3].primary.ends_with("beta"));
        assert!(items[4].primary.starts_with("▸ "));
    }

    #[test]
    fn expanded_account_inlines_field_rows() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account(&mut snap, "alpha");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&[
                "settings/accounts".to_string(),
                "settings/accounts/alpha".to_string(),
            ]),
        );
        let (_title, items, _selected) = assert_list(render(&mut snap));
        // Accounts (▾) + ghost + alpha (▾) + 5 field rows + Models (▸) = 9.
        assert_eq!(items.len(), 9);
        // First field row is "Name: alpha", indented to depth 2.
        assert!(items[3].primary.contains("Name: alpha"));
        assert!(items[3].primary.starts_with("    "));
        assert!(items[4].primary.contains("Protocol:"));
        assert!(items[7].primary.contains("Key:"));
    }

    #[test]
    fn compose_buffer_renders_inline_name_prompt() {
        // While `ui/settings/new_account/buffer` is `Some(_)`, the
        // affordance line directly under the Accounts header swaps from
        // the static "+ New connection" to a live `Name▸ <buf>▏` prompt.
        // The line has `focus: None` and isn't in visible_rows, so it
        // doesn't claim selection.
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/accounts".to_string()]),
        );
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "buffer"),
            Value::String("per".into()),
        );
        let (_title, items, _selected) = assert_list(render(&mut snap));
        // Accounts header (0), affordance (1), Models header (2).
        assert!(
            items[1].primary.contains("Name▸ per\u{258F}"),
            "expected inline-edit decoration; got {:?}",
            items[1].primary
        );
        assert!(items[1].focus.is_none(), "affordance must be unfocusable");
    }

    #[test]
    fn affordance_renders_static_label_when_buffer_is_absent() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/accounts".to_string()]),
        );
        let (_title, items, _selected) = assert_list(render(&mut snap));
        // Affordance at index 1 (Accounts header at 0).
        assert!(
            items[1].primary.contains("+ New connection"),
            "expected static affordance; got {:?}",
            items[1].primary
        );
        assert!(items[1].focus.is_none(), "affordance must be unfocusable");
    }

    #[test]
    fn focused_drives_selected_index() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            path_to_value(&oxpath!("settings", "models")),
        );
        let (_title, _items, selected) = assert_list(render(&mut snap));
        assert_eq!(selected, Some(1));
    }

    #[test]
    fn stale_focused_yields_no_selection() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            path_to_value(&oxpath!("nonsense")),
        );
        let (_title, _items, selected) = assert_list(render(&mut snap));
        assert_eq!(selected, None);
    }

    #[test]
    fn empty_index_renders_empty_list() {
        let mut snap = SettingsSnapshot::empty();
        let (title, items, selected) = assert_list(render(&mut snap));
        assert_eq!(title.as_deref(), Some("Settings"));
        assert!(items.is_empty());
        assert_eq!(selected, None);
    }

    #[test]
    fn ascend_rule_is_exit_screen() {
        assert_eq!(IndexRenderer.ascend_to(), AscendRule::ExitScreen);
    }

    #[test]
    fn register_inserts_into_registry() {
        let mut reg = RendererRegistry::new();
        register(&mut reg);
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        let theme = Theme::default();
        let mut ctx = RenderCtx {
            area: Rect::new(0, 0, 80, 24),
            data: &mut snap,
            registry: &reg,
            theme: &theme,
        };
        let view = reg.render(&oxpath!("settings", "index"), &mut ctx);
        match view {
            View::Frame { content, .. } => match *content {
                View::List { items, .. } => assert_eq!(items.len(), 2),
                other => panic!("expected List inside Frame, got {other:?}"),
            },
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    #[test]
    fn focused_protocol_row_renders_custom_provider_in_carousel() {
        // Regression: when an account's provider isn't in the preset list
        // (e.g. "LMStudio" from a TOML config that predates the carousel),
        // the focused row's carousel must show that custom name as the
        // current option — not silently render "anthropic" because the
        // value-not-found fallback hit idx 0.
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        let comp = ox_kernel::PathComponent::try_new("local").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp.clone()),
            to_value(&AccountConfig {
                provider: "LMStudio".into(),
            })
            .unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&[
                "settings/accounts".to_string(),
                "settings/accounts/local".to_string(),
            ]),
        );
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            path_to_value(&oxpath!("settings", "accounts", comp, "protocol")),
        );

        let view = render(&mut snap);
        let (_title, items, selected) = assert_list(view);
        let i = selected.expect("a row should be selected");
        let primary_spans = items[i]
            .primary_spans
            .as_ref()
            .expect("focused Protocol row should render carousel spans");
        let joined: String = primary_spans.iter().map(|s| s.text.as_str()).collect();
        assert!(
            joined.contains("LMStudio"),
            "expected carousel to include 'LMStudio'; got {joined:?}"
        );
    }
}

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

use ox_view::{Direction, FocusId, ListItem, ModifierSet, Sizing, Span, Style, View};

use ox_gate::AuthScheme;

use crate::settings::commands::account_model::{compose_form_view, resolve_protocol_options};
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

        // Resolve the Auth carousel's current scheme directly from the
        // typed `ProviderConfig` (via `resolved_auth()` so legacy configs
        // without an `auth` field still resolve to the dialect default).
        // Reading the enum here — instead of round-tripping the row label
        // through a string lookup — keeps the carousel locked to the
        // same source of truth that `selector_cycle_auth_dir` mutates.
        let auth_current: Option<AuthScheme> = selected
            .and_then(|i| rows.get(i))
            .and_then(|r| match &r.kind {
                RowKind::AccountField {
                    account,
                    field: ox_types::AccountField::Auth,
                } => Some(account.clone()),
                _ => None,
            })
            .and_then(|account| resolve_account_auth(ctx.data, &account));

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
                    if let Some(spans) = selector_carousel_spans(
                        row,
                        &indent,
                        glyph,
                        &protocol_options,
                        auth_current.as_ref(),
                    ) {
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

        // Compose mode is the discriminator that drives both the inline
        // "+ New connection" affordance (inactive) AND the
        // `View::Stack { [Form, List] }` projection (active). Reading it
        // once up here keeps the two decisions in lockstep — if active
        // is false the inline affordance shows; if true the form takes
        // over and the inline affordance is suppressed.
        let compose_active: bool = crate::settings::renderers::util::read_typed(
            ctx.data,
            &ox_path::oxpath!("ui", "settings", "new_account", "active"),
        )
        .unwrap_or(false);

        // Emit the inline "+ New connection" affordance directly after the
        // expanded Accounts header — but only when compose mode is NOT
        // active. When active the compose form is projected above the
        // list, so the inline affordance would be redundant. `focus: None`
        // keeps j/k from landing on it.
        if !compose_active {
            if let Some(insert_idx) = find_accounts_header_followup_idx(&rows) {
                let affordance = ListItem {
                    primary: "    + New connection".to_string(),
                    primary_spans: None,
                    secondary: None,
                    badge: None,
                    focus: None,
                };
                items.insert(insert_idx, affordance);
                selected = selected.map(|s| if s >= insert_idx { s + 1 } else { s });
            }
        }

        // Empty-catalog connections contribute zero rows to the
        // visible-rows projection — the renderer reads the data tree
        // directly, identifies them, and inserts two decoration
        // ListItems per account at the alphabetically-correct position
        // in the Models section: an empty-state line ("…(no models —
        // press r to refresh)") and either a static "+ add model
        // manually (m)" affordance or — when `manual_model/account`
        // names this account — the per-stage inline form prompt
        // ("Model id▸ <buf>▏"). Both lines are `focus: None`; j/k
        // skips them. `r` (bound at Prefix(settings/accounts) AND
        // Prefix(settings/models)) keeps refresh reachable; the
        // dispatcher's manual-model scope routes input when the form
        // is active.
        let manual_account: Option<String> = crate::settings::renderers::util::read_typed(
            ctx.data,
            &ox_path::oxpath!("ui", "settings", "manual_model", "account"),
        );

        let models_header_idx: Option<usize> = rows
            .iter()
            .position(|r| matches!(&r.kind, RowKind::Entry { entry_id } if entry_id == "models"));
        if let Some(models_idx) = models_header_idx {
            // Decorations only emit while the Models section is
            // expanded. A collapsed Models entry shows only its
            // header; inserting decorations past the header would
            // leak rows the user explicitly hid.
            let expanded = rows.get(models_idx).map(|r| r.expanded).unwrap_or(false);
            if expanded {
                // Map each non-empty account to its last Model-row
                // index in the items vector — the natural insertion
                // point for an alphabetically-later empty account is
                // "after the last Model row of the previous account".
                let mut last_model_idx_per_account: std::collections::BTreeMap<String, usize> =
                    std::collections::BTreeMap::new();
                for (i, row) in rows.iter().enumerate() {
                    if let RowKind::Model { account, .. } = &row.kind {
                        last_model_idx_per_account.insert(account.clone(), i);
                    }
                }

                // Sorted account names from the data tree drive the
                // alphabetical placement; the same sort that
                // append_model_rows uses on its iteration is what
                // makes "alphabetically-previous account" well-defined.
                let mut sorted_accounts: Vec<String> =
                    crate::settings::renderers::util::child_names_under(
                        ctx.data,
                        "config/gate/accounts",
                    )
                    .into_iter()
                    .filter(|n| ox_kernel::PathComponent::try_new(n.as_str()).is_ok())
                    .collect();
                sorted_accounts.sort();

                // Filter the sorted list to just the empty-catalog
                // accounts. Iterate in REVERSE so each insertion
                // doesn't invalidate the indices we computed for
                // alphabetically-earlier accounts still to process.
                let mut empty_accounts: Vec<String> = sorted_accounts
                    .iter()
                    .filter(|name| {
                        let comp = match ox_kernel::PathComponent::try_new(name.as_str()) {
                            Ok(c) => c,
                            Err(_) => return false,
                        };
                        let models: Vec<ox_gate::ModelInfo> =
                            crate::settings::renderers::util::read_typed(
                                ctx.data,
                                &ox_path::oxpath!(
                                    "config",
                                    "gate",
                                    "accounts",
                                    comp,
                                    "models"
                                ),
                            )
                            .unwrap_or_default();
                        models.is_empty()
                    })
                    .cloned()
                    .collect();
                empty_accounts.reverse();

                for name in empty_accounts {
                    // Insertion point: after the last Model row of the
                    // alphabetically-previous account, or right after
                    // the Models header if no earlier account has
                    // model rows.
                    let prev_model_idx = sorted_accounts
                        .iter()
                        .filter(|n| n.as_str() < name.as_str())
                        .filter_map(|n| last_model_idx_per_account.get(n))
                        .max()
                        .copied();
                    let insert_idx = match prev_model_idx {
                        Some(idx) => idx + 1,
                        None => models_idx + 1,
                    };

                    let empty_state = ListItem {
                        primary: format!("  {} / (no models — press r to refresh)", name),
                        primary_spans: None,
                        secondary: None,
                        badge: None,
                        focus: None,
                    };

                    let in_mode_for_this_account =
                        manual_account.as_deref() == Some(name.as_str());
                    let manual_primary = if in_mode_for_this_account {
                        let stage = crate::settings::renderers::util::read_typed::<
                            ox_types::settings::ManualModelStage,
                        >(
                            ctx.data,
                            &ox_path::oxpath!("ui", "settings", "manual_model", "stage"),
                        );
                        let buffer: String = crate::settings::renderers::util::read_typed(
                            ctx.data,
                            &ox_path::oxpath!("ui", "settings", "manual_model", "buffer"),
                        )
                        .unwrap_or_default();
                        let prompt = match stage {
                            Some(ox_types::settings::ManualModelStage::Id) => "Model id",
                            Some(ox_types::settings::ManualModelStage::Ctx) => "Max context",
                            Some(ox_types::settings::ManualModelStage::Out) => "Max output",
                            None => "Model id",
                        };
                        format!("    {prompt}▸ {buffer}\u{258F}")
                    } else {
                        "    + add model manually (m)".to_string()
                    };
                    let manual = ListItem {
                        primary: manual_primary,
                        primary_spans: None,
                        secondary: None,
                        badge: None,
                        focus: None,
                    };

                    // Insert manual first then empty_state at the same
                    // index so they end up [empty_state, manual].
                    items.insert(insert_idx, manual);
                    items.insert(insert_idx, empty_state);

                    selected = selected.map(|s| if s >= insert_idx { s + 2 } else { s });
                }
            }
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

        let list = View::List { items, selected };
        let inner = if compose_active {
            // Project the compose draft as a typed `View::Form` above
            // the accounts/models list. `Sizing::Fixed(form_height)`
            // pins the form to exactly its line count; the list takes
            // the remaining vertical space. The form is rendered by
            // the existing `render_form` translator — no new translator
            // code needed.
            let form = compose_form_view(ctx.data);
            let h = form_height(&form);
            View::Stack {
                dir: Direction::Vertical,
                children: vec![(form, Sizing::Fixed(h)), (list, Sizing::Fill)],
            }
        } else {
            list
        };

        View::Frame {
            title: Some("Settings".into()),
            title_right,
            content: Box::new(inner),
        }
    }

    fn ascend_to(&self) -> AscendRule {
        AscendRule::ExitScreen
    }
}

/// Vertical space the compose Form needs, in terminal lines.
/// `render_form` draws one line per `FormRow` (errors and hints render
/// inline on the same line as the value), so the height equals the row
/// count. Other variants fall through to 0 because this helper is only
/// ever called with the output of `compose_form_view`, which is always
/// a `View::Form` — the catch-all is defensive in case the projection
/// gets reshaped without the caller noticing.
fn form_height(view: &View) -> u16 {
    match view {
        View::Form { rows, .. } => rows.len() as u16,
        _ => 0,
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
    auth_current: Option<&AuthScheme>,
) -> Option<Vec<Span>> {
    // Build an owned option list per arm. Auth's options are the fixed
    // `AuthScheme::ALL` cycle, formatted via `Display` so the label and
    // the wire format never diverge; Protocol's are resolved per-frame
    // from the broker (`protocol_options`). Owned strings on both
    // branches keep the formatting block below uniform.
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
            // Read the current scheme from the typed `ProviderConfig`
            // resolved upstream — never from the row label. The label
            // is a string projection of the same enum; treating the
            // enum as authoritative is what keeps the carousel and the
            // cycle command in lockstep.
            let current = auth_current.cloned().unwrap_or(AuthScheme::XApiKey);
            let idx = AuthScheme::ALL.iter().position(|a| a == &current).unwrap_or(0);
            (
                "Auth",
                AuthScheme::ALL.iter().map(|a| a.to_string()).collect(),
                idx,
            )
        }
        RowKind::AccountField {
            field:
                ox_types::AccountField::Name
                | ox_types::AccountField::Endpoint
                | ox_types::AccountField::Key,
            ..
        }
        | RowKind::Entry { .. }
        | RowKind::Account { .. }
        | RowKind::Model { .. }
        | RowKind::ModelField { .. } => return None,
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
        RowKind::Entry { .. } | RowKind::Account { .. } | RowKind::Model { .. } => {
            return row.label.clone();
        }
    };
    // `▏` (U+258F) renders as a thin vertical bar — a clear cursor
    // mark at end-of-buffer that doesn't get confused with text.
    format!("{label}▸ {}\u{258F}", state.buffer)
}

/// Resolve the effective `AuthScheme` for an account by walking the
/// account → provider binding and applying `ProviderConfig::resolved_auth()`.
/// Returns `None` only when the account or provider records are
/// missing/malformed; the caller falls back to the default carousel
/// position. Mirrors `selector_cycle_auth_dir`'s read pattern so the
/// renderer and the cycle command see the same effective state.
fn resolve_account_auth(
    data: &mut dyn structfs_core_store::Reader,
    account: &str,
) -> Option<AuthScheme> {
    let acct = visible_rows::read_account_assembling_flat(data, account)?;
    let provider = visible_rows::read_provider_assembling_flat(data, &acct.provider)?;
    Some(provider.resolved_auth())
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
                ..Default::default()
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

    /// Helper: write an account along with a non-empty models catalog
    /// so the Models section shows a real Model row for it. Used by
    /// the empty-state decoration tests below to verify the
    /// alphabetical-position contract against a non-empty neighbor.
    fn write_account_with_models(
        snap: &mut SettingsSnapshot,
        name: &str,
        ids: &[&str],
    ) {
        use ox_gate::ModelInfo;
        use ox_types::ModelInfoSource;
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp.clone()),
            to_value(&AccountConfig {
                provider: name.into(),
                ..Default::default()
            })
            .unwrap(),
        );
        let models: Vec<ModelInfo> = ids
            .iter()
            .map(|id| ModelInfo {
                id: (*id).into(),
                display_name: (*id).into(),
                max_context_size: None,
                max_output_tokens: None,
                source: ModelInfoSource::Server,
            })
            .collect();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp, "models"),
            to_value(&models).unwrap(),
        );
    }

    #[test]
    fn empty_catalog_account_renders_decoration_pair_under_models() {
        // An empty-catalog account contributes no rows to visible_rows;
        // the renderer reads the data tree, identifies the empty
        // account, and inserts two unfocusable decoration ListItems —
        // an empty-state line and a manual-model affordance.
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account(&mut snap, "alpha");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        let (_title, items, _selected) = assert_list(render(&mut snap));
        // Accounts header (▸), Models header (▾), empty-state line,
        // manual affordance = 4 items.
        assert_eq!(items.len(), 4);
        assert!(
            items[2].primary.contains("alpha")
                && items[2].primary.contains("no models")
                && items[2].primary.contains("press r to refresh"),
            "expected empty-state line at idx 2; got {:?}",
            items[2].primary
        );
        assert!(items[2].focus.is_none(), "empty-state line is decoration");
        assert!(
            items[3].primary.contains("+ add model manually (m)"),
            "expected manual affordance at idx 3; got {:?}",
            items[3].primary
        );
        assert!(items[3].focus.is_none(), "manual affordance is decoration");
    }

    #[test]
    fn empty_catalog_decoration_lands_at_alphabetical_position() {
        // Two accounts: alpha has a model (so a Model row exists),
        // beta is empty. The empty-state decoration for beta must
        // land AFTER alpha's Model row, not before.
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account_with_models(&mut snap, "alpha", &["m1"]);
        write_account(&mut snap, "beta");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        let (_title, items, _selected) = assert_list(render(&mut snap));
        // Accounts (▸), Models (▾), alpha/m1, beta empty-state, beta manual = 5
        assert_eq!(items.len(), 5);
        assert!(items[2].primary.contains("alpha"));
        assert!(items[2].primary.contains("m1"));
        assert!(items[3].primary.contains("beta"));
        assert!(items[3].primary.contains("no models"));
        assert!(items[4].primary.contains("+ add model manually"));
    }

    #[test]
    fn empty_catalog_decoration_renders_inline_form_when_in_manual_mode() {
        // When `manual_model/account` matches the empty account, the
        // affordance line swaps to a stage-prompt with the live buffer.
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account(&mut snap, "alpha");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "account"),
            Value::String("alpha".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "stage"),
            to_value(&ox_types::settings::ManualModelStage::Id).unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String("custom".into()),
        );
        let (_title, items, _selected) = assert_list(render(&mut snap));
        // The manual affordance is now an inline form prompt.
        assert!(
            items[3].primary.contains("Model id▸ custom\u{258F}"),
            "expected inline manual form prompt; got {:?}",
            items[3].primary
        );
        assert!(items[3].focus.is_none());
    }

    #[test]
    fn empty_catalog_decoration_skipped_when_models_section_collapsed() {
        // The Models section is collapsed → no decorations leak past
        // the header row.
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account(&mut snap, "alpha");
        let (_title, items, _selected) = assert_list(render(&mut snap));
        // Just the two top-level headers.
        assert_eq!(items.len(), 2);
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
                ..Default::default()
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

    // -- compose-mode View::Form projection --------------------------------------

    /// Open `Accounts` accordion. The compose-form projection only ever
    /// emits while the accordion is expanded — when collapsed there's
    /// no accounts list to stack the form above.
    fn snap_at_accounts_page() -> SettingsSnapshot {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/accounts".to_string()]),
        );
        snap
    }

    /// Compose-mode snapshot: accordion expanded + compose active +
    /// every field at its open-state default. Mirrors the shape
    /// `accounts.compose.open` writes (T5).
    fn snap_with_compose_active() -> SettingsSnapshot {
        let mut snap = snap_at_accounts_page();
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "active"),
            Value::Bool(true),
        );
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "focused_field"),
            Value::String("name".into()),
        );
        for sub in ["name", "endpoint", "key"] {
            let comp = ox_kernel::PathComponent::try_new(sub).unwrap();
            snap.insert(
                &oxpath!("ui", "settings", "new_account", comp),
                Value::String(String::new()),
            );
        }
        for sub in ["protocol", "auth"] {
            let comp = ox_kernel::PathComponent::try_new(sub).unwrap();
            snap.insert(
                &oxpath!("ui", "settings", "new_account", comp),
                Value::Null,
            );
        }
        snap
    }

    /// Walk `Frame -> (Stack | Form)` and return the `View::Form` payload.
    /// Returns `None` when the view isn't a compose-active form layout.
    fn extract_form(view: View) -> Option<(Vec<ox_view::FormRow>, Option<usize>)> {
        use ox_view::View as V;
        match view {
            V::Frame { content, .. } => match *content {
                V::Stack { children, .. } => {
                    for (child, _) in children {
                        if let V::Form { rows, focused } = child {
                            return Some((rows, focused));
                        }
                    }
                    None
                }
                V::Form { rows, focused } => Some((rows, focused)),
                _ => None,
            },
            _ => None,
        }
    }

    #[test]
    fn index_renderer_emits_frame_list_when_compose_inactive() {
        let mut snap = snap_at_accounts_page();
        let view = render(&mut snap);
        match view {
            View::Frame { content, .. } => match *content {
                View::List { .. } => {}
                other => panic!("expected View::List inside Frame when compose inactive, got {other:?}"),
            },
            other => panic!("expected View::Frame, got {other:?}"),
        }
    }

    #[test]
    fn index_renderer_emits_frame_stack_form_list_when_compose_active() {
        use ox_view::{Direction, Sizing};
        let mut snap = snap_with_compose_active();
        let view = render(&mut snap);
        match view {
            View::Frame { content, .. } => match *content {
                View::Stack { dir, children } => {
                    assert_eq!(dir, Direction::Vertical);
                    assert_eq!(children.len(), 2, "expected Form + List children");
                    let (form, _) = &children[0];
                    let (list, list_sizing) = &children[1];
                    assert!(
                        matches!(form, View::Form { .. }),
                        "first child must be the compose Form; got {form:?}"
                    );
                    assert!(
                        matches!(list, View::List { .. }),
                        "second child must be the accounts/models List; got {list:?}"
                    );
                    assert_eq!(*list_sizing, Sizing::Fill);
                }
                other => panic!("expected Stack inside Frame, got {other:?}"),
            },
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    #[test]
    fn compose_form_has_one_row_per_field_in_order() {
        use crate::settings::commands::account_model::{FIELD_ORDER, field_label};
        let mut snap = snap_with_compose_active();
        let view = render(&mut snap);
        let (rows, _) = extract_form(view).expect("Form present");
        assert_eq!(rows.len(), FIELD_ORDER.len());
        for (i, field) in FIELD_ORDER.iter().enumerate() {
            assert_eq!(rows[i].label, field_label(*field));
        }
    }

    #[test]
    fn compose_form_row_kinds_match_field_kinds() {
        // Text fields → FormValue::Text. Selector fields → at compose
        // open-state (no protocol/auth picked yet) → FormValue::ReadOnly
        // placeholder. The kind alignment pins that the projection picks
        // the correct variant per field.
        use crate::settings::commands::account_model::{FIELD_ORDER, FieldKind, field_kind};
        use ox_view::FormValue;
        let mut snap = snap_with_compose_active();
        let view = render(&mut snap);
        let (rows, _) = extract_form(view).expect("Form present");
        for (i, field) in FIELD_ORDER.iter().enumerate() {
            match (field_kind(*field), &rows[i].value) {
                (FieldKind::Text, FormValue::Text { .. }) => {}
                (FieldKind::Selector, FormValue::ReadOnly(_)) => {}
                (kind, value) => {
                    panic!("field {field:?} kind={kind:?} got value={value:?}")
                }
            }
        }
    }

    #[test]
    fn compose_form_focused_index_tracks_focused_field() {
        use crate::settings::commands::account_model::FIELD_ORDER;
        for field in FIELD_ORDER {
            let mut snap = snap_with_compose_active();
            // Override the focused-field discriminator.
            let subpath = match field {
                ox_types::AccountField::Name => "name",
                ox_types::AccountField::Protocol => "protocol",
                ox_types::AccountField::Endpoint => "endpoint",
                ox_types::AccountField::Auth => "auth",
                ox_types::AccountField::Key => "key",
            };
            snap.insert(
                &oxpath!("ui", "settings", "new_account", "focused_field"),
                Value::String(subpath.into()),
            );
            let view = render(&mut snap);
            let (_rows, focused) = extract_form(view).expect("Form present");
            let expected = FIELD_ORDER.iter().position(|f| *f == field);
            assert_eq!(focused, expected, "field {field:?}");
        }
    }

    #[test]
    fn compose_form_threads_errors_into_form_rows() {
        use crate::settings::commands::account_model::FIELD_ORDER;
        use ox_types::settings::ValidationErrors;
        let mut snap = snap_with_compose_active();
        let errors = ValidationErrors {
            name: Some("'with space' is not a valid identifier".into()),
            ..Default::default()
        };
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "errors"),
            to_value(&errors).unwrap(),
        );
        let view = render(&mut snap);
        let (rows, _) = extract_form(view).expect("Form present");
        let name_idx = FIELD_ORDER
            .iter()
            .position(|f| *f == ox_types::AccountField::Name)
            .unwrap();
        assert!(
            rows[name_idx].error.is_some(),
            "name error must thread into the Name FormRow"
        );
        // Other rows have no error in this fixture.
        for (i, row) in rows.iter().enumerate() {
            if i == name_idx {
                continue;
            }
            assert!(
                row.error.is_none(),
                "row {i} ({}) should have no error",
                row.label
            );
        }
    }

    #[test]
    fn key_field_renders_masked_text_value() {
        use crate::settings::commands::account_model::FIELD_ORDER;
        use ox_view::FormValue;
        let mut snap = snap_with_compose_active();
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "key"),
            Value::String("sk-secret".into()),
        );
        let view = render(&mut snap);
        let (rows, _) = extract_form(view).expect("Form present");
        let key_idx = FIELD_ORDER
            .iter()
            .position(|f| *f == ox_types::AccountField::Key)
            .unwrap();
        match &rows[key_idx].value {
            FormValue::Text { value, masked, .. } => {
                assert!(*masked, "Key value must be masked");
                assert_eq!(value, "sk-secret");
            }
            other => panic!("expected FormValue::Text, got {other:?}"),
        }
    }

    #[test]
    fn selector_with_value_renders_selector_form_value() {
        // Once the user has picked a protocol, the row switches from
        // the placeholder `ReadOnly` to a `Selector { options, current }`.
        use crate::settings::commands::account_model::FIELD_ORDER;
        use ox_view::FormValue;
        let mut snap = snap_with_compose_active();
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "protocol"),
            Value::String("anthropic".into()),
        );
        let view = render(&mut snap);
        let (rows, _) = extract_form(view).expect("Form present");
        let proto_idx = FIELD_ORDER
            .iter()
            .position(|f| *f == ox_types::AccountField::Protocol)
            .unwrap();
        match &rows[proto_idx].value {
            FormValue::Selector { options, current } => {
                assert!(!options.is_empty(), "options must be non-empty");
                assert!(*current < options.len(), "current must index into options");
                assert_eq!(options[*current], "anthropic");
            }
            other => panic!("expected FormValue::Selector, got {other:?}"),
        }
    }
}

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

use horns_core::view::{Direction, FocusId, ListItem, ModifierSet, Padding, Sizing, Span, Style, View};

use ox_gate::AuthScheme;

use crate::settings::commands::account_model::{
    compose_form_view, cursor_is_in_compose_form, cursor_is_in_confirm_delete,
    cursor_to_manual_model_stage, resolve_protocol_options,
};
use crate::settings::commands::edit::read_edit_state;
use crate::settings::{AscendRule, RenderCtx, Renderer, RendererRegistry};
use crate::settings::visible_rows::{self, RowKind};

pub struct IndexRenderer;

impl Renderer for IndexRenderer {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View {
        let rows = visible_rows::enumerate(ctx.data);
        let edit_state = read_edit_state(ctx.data);
        let cursor = read_cursor(ctx.data);

        // Compose mode discriminates the Accounts section's middle slot
        // (Form vs affordance). Under cursor-as-focus, the cursor being
        // inside `settings/_compose_form/...` IS the discriminator —
        // no separate `new_account/active` flag exists. The Form's own
        // `focused: Option<usize>` derives from the cursor, and the
        // cursor naturally doesn't match any account row while sitting
        // under the synthetic form namespace, so no `cursor_for_lists`
        // workaround is needed to suppress page-cursor highlighting.
        let compose_active = cursor.as_ref().is_some_and(cursor_is_in_compose_form);

        // Resolve selector option lists once for the focused row, then
        // pass them through to the row→ListItem helpers. The carousel
        // only renders when the focused row IS a selector field, so a
        // single resolved pair per frame is enough.
        let (protocol_options, auth_current) =
            resolve_focused_selector_state(ctx.data, &rows, &cursor);

        // Partition the flat row enumeration into per-section slices at
        // the Models Entry boundary. The Accounts slice owns rows
        // [Accounts header, accounts, account fields…]; the Models
        // slice owns [Models header, models, model fields…].
        let (accounts_rows, models_rows) = partition_rows_by_section(&rows);

        let ctx_state = SectionCtx {
            cursor: cursor.as_ref(),
            edit_state: edit_state.as_ref(),
            protocol_options: &protocol_options,
            auth_current: auth_current.as_ref(),
        };

        let accounts_section =
            render_accounts_section(ctx.data, accounts_rows, &ctx_state, compose_active);
        let models_section = render_models_section(ctx.data, models_rows, &ctx_state);

        // Confirm-delete banner. Emitted as a top-level ListItem
        // prepended above the section stack when the cursor sits at
        // `settings/_confirm_delete`. The target account name lives at
        // the dedicated data path `ui/settings/pending_delete/
        // target_account` (the value half of the retired value-flag).
        // Decoration only — focus: None; j/k skips it.
        let confirm_delete_active = cursor.as_ref().is_some_and(cursor_is_in_confirm_delete);
        let pending: Option<String> = if confirm_delete_active {
            crate::settings::renderers::util::read_typed(
                ctx.data,
                &ox_path::oxpath!("ui", "settings", "pending_delete", "target_account"),
            )
        } else {
            None
        };

        let title_right = read_dirty_indicator(ctx.data);

        let mut stack_children: Vec<(View, Sizing)> = Vec::new();
        if let Some(name) = pending {
            stack_children.push((
                View::List {
                    items: vec![ListItem {
                        primary: format!("Delete '{}'? y / n", name),
                        primary_spans: None,
                        secondary: None,
                        badge: None,
                        focus: None,
                    }],
                    selected: None,
                },
                Sizing::Fixed(1),
            ));
        }
        // Size each section to its measured row count. Two `Min(0)`
        // children would share leftover terminal rows and visibly drift
        // apart; pinning each section to its content height keeps them
        // rendered back-to-back, matching the old flat-list layout. A
        // trailing `Fill` filler absorbs the remaining space at the
        // bottom of the page (the same role the bare list had before).
        let accounts_h = section_height(&accounts_section);
        let models_h = section_height(&models_section);
        stack_children.push((accounts_section, Sizing::Fixed(accounts_h)));
        stack_children.push((models_section, Sizing::Fixed(models_h)));
        stack_children.push((View::Empty, Sizing::Fill));

        View::Frame {
            title: Some("Settings".into()),
            title_right,
            content: Box::new(View::Stack {
                dir: Direction::Vertical,
                children: stack_children,
            }),
        }
    }

    fn ascend_to(&self) -> AscendRule {
        AscendRule::ExitScreen
    }
}

/// Per-frame context bundle used to build sub-Lists from row slices.
/// Bundling these into a struct keeps each section renderer's signature
/// short and makes it obvious that every sub-List sees the same focus /
/// edit / carousel inputs.
struct SectionCtx<'a> {
    cursor: Option<&'a structfs_core_store::Path>,
    edit_state: Option<&'a crate::settings::commands::edit::EditState>,
    protocol_options: &'a [String],
    auth_current: Option<&'a AuthScheme>,
}

/// Split the visible-rows enumeration at the Models Entry boundary.
/// Returns `(accounts_rows, models_rows)`. The Accounts slice always
/// starts at index 0 (which may be empty in degenerate test configs);
/// the Models slice starts at the Models Entry row (or is empty if
/// no Models entry exists).
fn partition_rows_by_section(
    rows: &[visible_rows::VisibleRow],
) -> (&[visible_rows::VisibleRow], &[visible_rows::VisibleRow]) {
    let models_pos = rows
        .iter()
        .position(|r| matches!(&r.kind, RowKind::Entry { entry_id } if entry_id == "models"));
    match models_pos {
        Some(m) => (&rows[..m], &rows[m..]),
        None => (rows, &[]),
    }
}

/// Resolve the (protocol_options, auth_current) pair the row→ListItem
/// helpers need to render the carousel on the currently focused
/// selector row. Returns empties when the focused row isn't a selector;
/// the helpers skip carousel decoration in that case.
fn resolve_focused_selector_state(
    data: &mut dyn structfs_core_store::Reader,
    rows: &[visible_rows::VisibleRow],
    cursor: &Option<structfs_core_store::Path>,
) -> (Vec<String>, Option<AuthScheme>) {
    let selected = cursor
        .as_ref()
        .and_then(|c| visible_rows::position_of(rows, c));

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
            resolve_protocol_options(data, current)
        })
        .unwrap_or_default();

    let auth_current: Option<AuthScheme> = selected
        .and_then(|i| rows.get(i))
        .and_then(|r| match &r.kind {
            RowKind::AccountField {
                account,
                field: ox_types::AccountField::Auth,
            } => Some(account.clone()),
            _ => None,
        })
        .and_then(|account| resolve_account_auth(data, &account));

    (protocol_options, auth_current)
}

/// Build the Accounts section as `Stack[ header_list, optional_middle,
/// optional_content_list ]`. The middle slot is the compose Form when
/// active, the "+ New connection" affordance when not active and the
/// Accounts header is expanded, or absent otherwise. The content list
/// holds the account rows (and their expanded field rows).
fn render_accounts_section(
    data: &mut dyn structfs_core_store::Reader,
    rows: &[visible_rows::VisibleRow],
    ctx: &SectionCtx<'_>,
    compose_active: bool,
) -> View {
    if rows.is_empty() {
        // Degenerate config (no Accounts entry in settings/index/entries)
        // — emit an empty Stack so the page-level Stack still sees a
        // valid child. Matches the old "empty list" behavior.
        return View::Stack {
            dir: Direction::Vertical,
            children: Vec::new(),
        };
    }
    let (header_row, content_rows) = rows
        .split_first()
        .expect("Accounts rows non-empty checked above");
    let header_expanded = header_row.expanded;

    let mut children: Vec<(View, Sizing)> = Vec::new();

    // Header is always present (Accounts entry row).
    let header_list = build_list_from_rows(std::slice::from_ref(header_row), ctx);
    children.push((header_list, Sizing::Fixed(1)));

    // Middle slot: heading+padded Form > affordance > nothing.
    if compose_active {
        // Promote the "+ New connection" affordance into a heading for
        // the compose form, with parenthetical help text. Same 4-space
        // indent as the inactive affordance; non-focusable (j/k skips).
        let heading_item = ListItem {
            primary: "    + New connection (Tab to navigate, Enter to create, Esc to cancel)"
                .to_string(),
            primary_spans: None,
            secondary: None,
            badge: None,
            focus: None,
        };
        let heading_list = View::List {
            items: vec![heading_item],
            selected: None,
        };
        let form = compose_form_view(data);
        let form_h = form_height(&form);
        // Indent the form to align with depth-1 content (four spaces);
        // wrap as View::Pad so the translator inserts the left margin.
        let padded_form = View::Pad {
            padding: Padding {
                left: 4,
                right: 0,
                top: 0,
                bottom: 0,
            },
            child: Box::new(form),
        };
        let compose_block = View::Stack {
            dir: Direction::Vertical,
            children: vec![
                (heading_list, Sizing::Fixed(1)),
                (padded_form, Sizing::Min(form_h)),
            ],
        };
        // The compose block measures as heading (1) + form rows; +1 over
        // the bare-form slot. section_height() walks Stack/List/Form so
        // the outer page Stack still sees the right size.
        children.push((compose_block, Sizing::Min(form_h + 1)));
    } else if header_expanded {
        let affordance = ListItem {
            primary: "    + New connection".to_string(),
            primary_spans: None,
            secondary: None,
            badge: None,
            focus: None,
        };
        children.push((
            View::List {
                items: vec![affordance],
                selected: None,
            },
            Sizing::Fixed(1),
        ));
    }

    // Content sub-List — account rows + expanded field rows. Only
    // appears when the Accounts entry is in the expanded set AND
    // there are actual content rows to show.
    if header_expanded && !content_rows.is_empty() {
        children.push((build_list_from_rows(content_rows, ctx), Sizing::Min(0)));
    }

    View::Stack {
        dir: Direction::Vertical,
        children,
    }
}

/// Build the Models section as `Stack[ header_list, optional_content_list ]`.
/// The content list holds Model rows + empty-catalog decorations
/// interleaved alphabetically. With both pieces living in the same
/// sub-List, the decoration indices are local — no rows-into-items
/// offset math.
fn render_models_section(
    data: &mut dyn structfs_core_store::Reader,
    rows: &[visible_rows::VisibleRow],
    ctx: &SectionCtx<'_>,
) -> View {
    if rows.is_empty() {
        return View::Stack {
            dir: Direction::Vertical,
            children: Vec::new(),
        };
    }
    let (header_row, content_rows) = rows
        .split_first()
        .expect("Models rows non-empty checked above");
    let header_expanded = header_row.expanded;

    let mut children: Vec<(View, Sizing)> = Vec::new();
    let header_list = build_list_from_rows(std::slice::from_ref(header_row), ctx);
    children.push((header_list, Sizing::Fixed(1)));

    if header_expanded {
        // Start with the real Model rows projected from visible_rows.
        let mut content_items = rows_to_list_items(content_rows, ctx);
        // Then interleave empty-catalog decorations for accounts that
        // contribute zero Model rows. Both indices are local to the
        // Models content list — no offset gymnastics.
        interleave_empty_catalog_decorations(data, &mut content_items, content_rows);

        // Each sub-List recomputes its own `selected` from its items'
        // focus IDs. At most one sub-List will return Some.
        let selected = content_items.iter().position(|it| {
            it.focus
                .as_ref()
                .map(|f| Some(&f.0) == ctx.cursor)
                .unwrap_or(false)
        });

        children.push((
            View::List {
                items: content_items,
                selected,
            },
            Sizing::Min(0),
        ));
    }

    View::Stack {
        dir: Direction::Vertical,
        children,
    }
}

/// Convert a slice of `VisibleRow`s into `ListItem`s using the per-frame
/// SectionCtx for focus / edit / selector decoration. The helper is the
/// extracted row → ListItem map block; it's shared across sub-Lists so
/// every section renders rows the same way.
fn rows_to_list_items(rows: &[visible_rows::VisibleRow], ctx: &SectionCtx<'_>) -> Vec<ListItem> {
    rows.iter()
        .map(|row| {
            let indent = "  ".repeat(row.depth);
            let glyph = if row.expandable {
                if row.expanded { "▾ " } else { "▸ " }
            } else {
                "  "
            };
            // The carousel only renders on the focused selector row —
            // that's the visual cue h/l will cycle. The match against
            // `cursor` happens once per row, locally; no global "selected
            // index" needs to be threaded.
            let is_focused = ctx
                .cursor
                .map(|c| c == &row.path)
                .unwrap_or(false);
            if is_focused {
                if let Some(spans) = selector_carousel_spans(
                    row,
                    &indent,
                    glyph,
                    ctx.protocol_options,
                    ctx.auth_current,
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
            let label = decorate_row_label(row, ctx.edit_state);
            ListItem {
                primary: format!("{indent}{glyph}{label}"),
                primary_spans: None,
                secondary: row.secondary.clone(),
                badge: row.badge.clone(),
                focus: Some(FocusId(row.path.clone())),
            }
        })
        .collect()
}

/// Build a `View::List` from a slice of rows, computing its `selected`
/// locally from the cursor. Used by sections that don't need to
/// interleave decorations between rows — just a straight rows→items
/// map plus a single `selected` pass.
fn build_list_from_rows(rows: &[visible_rows::VisibleRow], ctx: &SectionCtx<'_>) -> View {
    let items = rows_to_list_items(rows, ctx);
    let selected = items.iter().position(|it| {
        it.focus
            .as_ref()
            .map(|f| Some(&f.0) == ctx.cursor)
            .unwrap_or(false)
    });
    View::List { items, selected }
}

/// Interleave empty-catalog decorations into the Models content list.
/// For each account that contributes zero Model rows to `content_rows`,
/// insert two decoration items — an empty-state line and either a
/// static "+ add model manually (m)" affordance or — when
/// `manual_model/account` matches — a per-stage inline form prompt.
/// All insertion indices are local to the Models content list, so
/// there's no rows-into-items offset divergence.
fn interleave_empty_catalog_decorations(
    data: &mut dyn structfs_core_store::Reader,
    items: &mut Vec<ListItem>,
    content_rows: &[visible_rows::VisibleRow],
) {
    let manual_account: Option<String> = crate::settings::renderers::util::read_typed(
        data,
        &ox_path::oxpath!("ui", "settings", "manual_model", "account"),
    );

    // Map each non-empty account to its last Model-row index in the
    // content_rows slice. Decorations for empty-catalog accounts land
    // after the alphabetically-previous account's last Model row.
    let mut last_model_idx_per_account: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for (i, row) in content_rows.iter().enumerate() {
        if let RowKind::Model { account, .. } = &row.kind {
            last_model_idx_per_account.insert(account.clone(), i);
        }
    }

    let mut sorted_accounts: Vec<String> =
        crate::settings::renderers::util::child_names_under(data, "config/gate/accounts")
            .into_iter()
            .filter(|n| ox_kernel::PathComponent::try_new(n.as_str()).is_ok())
            .collect();
    sorted_accounts.sort();

    let mut empty_accounts: Vec<String> = sorted_accounts
        .iter()
        .filter(|name| {
            let comp = match ox_kernel::PathComponent::try_new(name.as_str()) {
                Ok(c) => c,
                Err(_) => return false,
            };
            let models: Vec<ox_gate::ModelInfo> = crate::settings::renderers::util::read_typed(
                data,
                &ox_path::oxpath!("config", "gate", "accounts", comp, "models"),
            )
            .unwrap_or_default();
            models.is_empty()
        })
        .cloned()
        .collect();
    // Reverse so earlier insertions don't invalidate later indices.
    empty_accounts.reverse();

    for name in empty_accounts {
        // Insert index is into `items` (the local Models content list).
        // For an empty account, the insertion point is "right after the
        // last Model row of the alphabetically-previous account" — or
        // at the top of the content list when no earlier account has
        // any models.
        let prev_model_idx = sorted_accounts
            .iter()
            .filter(|n| n.as_str() < name.as_str())
            .filter_map(|n| last_model_idx_per_account.get(n))
            .max()
            .copied();
        let insert_idx = match prev_model_idx {
            Some(idx) => idx + 1,
            None => 0,
        };

        let empty_state = ListItem {
            primary: format!("  {} / (no models — press r to refresh)", name),
            primary_spans: None,
            secondary: None,
            badge: None,
            focus: None,
        };

        let in_mode_for_this_account = manual_account.as_deref() == Some(name.as_str());
        let manual_primary = if in_mode_for_this_account {
            // Cursor-as-focus: the cursor's leaf segment under
            // `settings/_manual_model/<stage>` IS the active stage.
            // Falls back to Id-stage prompt when the cursor isn't in
            // the form — defensive; the dispatcher only routes here
            // while cursor is at a stage path, but the renderer also
            // reaches this branch from `manual_model/account` matching
            // alone, so we keep a sensible default.
            let cursor = read_cursor(data);
            let stage = cursor.as_ref().and_then(cursor_to_manual_model_stage);
            let buffer: String = crate::settings::renderers::util::read_typed(
                data,
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

        // Insert manual first, then empty_state at the same index, so
        // they end up [empty_state, manual] in display order.
        items.insert(insert_idx, manual);
        items.insert(insert_idx, empty_state);
    }
}

/// Measured row count of a section (a Stack-of-Lists with an optional
/// Form middle slot). Lists contribute one row per item plus one extra
/// per item with a `secondary` line (the translator stacks them); a
/// Form contributes one row per FormRow. Nested Stacks recurse. Anything
/// else contributes zero (defensive — sections only ever contain the
/// shapes named above).
fn section_height(view: &View) -> u16 {
    match view {
        View::List { items, .. } => items
            .iter()
            .map(|it| 1 + if it.secondary.is_some() { 1 } else { 0 })
            .sum::<usize>() as u16,
        View::Form { rows, .. } => rows.len() as u16,
        View::Stack { children, .. } => children.iter().map(|(c, _)| section_height(c)).sum(),
        // `View::Pad` wraps the compose form for indentation; left/right
        // padding doesn't change row count, and the only padded child
        // we emit has zero top/bottom padding, so the inner child's
        // measured rows pass through unchanged. If the renderer ever
        // grows top/bottom padding we'll need to add it here.
        View::Pad { child, padding } => {
            section_height(child) + padding.top + padding.bottom
        }
        _ => 0,
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

    use horns_core::Rect;
    use ox_gate::AccountConfig;
    use ox_path::oxpath;
    use ox_types::{BadgeSource, SettingsIndexEntry};
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
            theme: &theme as &dyn std::any::Any,
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

    /// Walk the View tree and produce a flat (items, selected) projection
    /// across every sub-List, in render order. The new section-stack
    /// structure has multiple sub-Lists (header + content per section);
    /// pre-existing tests assert against a flat items vector, so this
    /// adapter preserves their semantics. `selected` is the absolute
    /// position of whichever sub-List has a selection set, mapped into
    /// the flattened vector.
    fn assert_list(view: View) -> (Option<String>, Vec<ListItem>, Option<usize>) {
        match view {
            View::Frame {
                title,
                title_right: _,
                content,
            } => {
                let mut out: Vec<ListItem> = Vec::new();
                let mut selected: Option<usize> = None;
                collect_lists(&content, &mut out, &mut selected);
                (title, out, selected)
            }
            other => panic!("expected View::Frame, got {other:?}"),
        }
    }

    /// Recursive walker for `assert_list`. Inlines every sub-List's
    /// items into `out`; if any sub-List carries a `selected: Some`,
    /// records its absolute offset in `selected` (asserts at most one
    /// sub-List has a selection set — the per-section invariant).
    fn collect_lists(view: &View, out: &mut Vec<ListItem>, selected: &mut Option<usize>) {
        match view {
            View::List { items, selected: s } => {
                if let Some(local) = s {
                    assert!(
                        selected.is_none(),
                        "at most one sub-List should carry a selection per frame"
                    );
                    *selected = Some(out.len() + local);
                }
                out.extend(items.iter().cloned());
            }
            View::Stack { children, .. } => {
                for (child, _) in children {
                    collect_lists(child, out, selected);
                }
            }
            View::Frame { content, .. } => collect_lists(content, out, selected),
            _ => {}
        }
    }

    /// Walk Frame → Stack → first child (the Accounts section).
    fn extract_accounts_section(view: View) -> View {
        match view {
            View::Frame { content, .. } => match *content {
                View::Stack { children, .. } => {
                    assert!(!children.is_empty(), "page must have at least one section");
                    children.into_iter().next().unwrap().0
                }
                other => panic!("expected Stack inside Frame, got {other:?}"),
            },
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    /// Walk Frame → Stack → second child (the Models section).
    fn extract_models_section(view: View) -> View {
        match view {
            View::Frame { content, .. } => match *content {
                View::Stack { mut children, .. } => {
                    assert!(children.len() >= 2, "page must have Accounts + Models sections");
                    children.remove(1).0
                }
                other => panic!("expected Stack inside Frame, got {other:?}"),
            },
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    /// Flatten a View into a single sequence of display strings in
    /// render order. Used by positioning-regression tests to assert
    /// "header X appears before decoration Y". Walks Lists (primary
    /// strings) and Forms (row labels).
    fn flatten_to_strings(view: &View) -> Vec<String> {
        let mut out = Vec::new();
        flatten_inner(view, &mut out);
        out
    }

    fn flatten_inner(view: &View, out: &mut Vec<String>) {
        match view {
            View::List { items, .. } => {
                for it in items {
                    out.push(it.primary.clone());
                }
            }
            View::Form { rows, .. } => {
                for r in rows {
                    out.push(r.label.clone());
                }
            }
            View::Stack { children, .. } => {
                for (c, _) in children {
                    flatten_inner(c, out);
                }
            }
            View::Frame { content, .. } => flatten_inner(content, out),
            View::Pad { child, .. } => flatten_inner(child, out),
            _ => {}
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
            theme: &theme as &dyn std::any::Any,
        };
        let view = reg.render(&oxpath!("settings", "index"), &mut ctx);
        // Registry dispatches to IndexRenderer which now emits a
        // section-stack. Flatten to count the rendered rows the way
        // the old assertion did.
        let (_title, items, _selected) = assert_list(view);
        assert_eq!(items.len(), 2);
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
    fn models_decorations_appear_after_models_header_when_new_connection_affordance_also_present()
    {
        // Two accounts, both Accounts and Models expanded, compose-mode
        // inactive. The alphabetically-first account is empty (aaa);
        // the second has models (bbb). The "+ New connection"
        // affordance is inserted in the Accounts section; the empty-
        // catalog decorations for aaa get scheduled in the Models
        // section. With aaa first, the decoration's prev_model_idx is
        // None and falls through to `models_idx + 1` — which after the
        // affordance insert is the items-index of Models itself, so the
        // decorations land BEFORE the Models header instead of after.
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account(&mut snap, "aaa");
        write_account_with_models(&mut snap, "bbb", &["m1"]);
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&[
                "settings/accounts".to_string(),
                "settings/models".to_string(),
            ]),
        );

        let (_title, items, _selected) = assert_list(render(&mut snap));

        let models_header_idx = items
            .iter()
            .position(|it| it.primary.trim_start().starts_with("▾ Models"))
            .expect("Models header present");
        let empty_state_idx = items
            .iter()
            .position(|it| it.primary.contains("no models"))
            .expect("empty-state decoration present for aaa's empty catalog");

        assert!(
            empty_state_idx > models_header_idx,
            "empty-state decoration should be AFTER the Models header, not before. \
             Got empty_state at {empty_state_idx}, Models header at {models_header_idx}",
        );
    }

    #[test]
    fn empty_catalog_decoration_renders_inline_form_when_in_manual_mode() {
        // When `manual_model/account` matches the empty account AND the
        // cursor sits at a manual-model stage path, the affordance line
        // swaps to a stage-prompt with the live buffer.
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
        // Cursor encodes the active stage under cursor-as-focus.
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            path_to_value(&oxpath!("settings", "_manual_model", "id")),
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

    /// Compose-mode snapshot: accordion expanded + cursor at the
    /// compose form's Name field + every draft field at its open-state
    /// default. Mirrors the shape `accounts.compose.open` writes under
    /// cursor-as-focus.
    fn snap_with_compose_active() -> SettingsSnapshot {
        let mut snap = snap_at_accounts_page();
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            path_to_value(&oxpath!("settings", "_compose_form", "name")),
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

    /// Walk the View tree and return the first `View::Form` payload.
    /// After the section-stack refactor the form is nested as
    /// `Frame → Stack → Stack (AccountsSection) → Form`, so a recursive
    /// walk is the simplest way to find it. Returns `None` when no Form
    /// is present (compose inactive).
    fn extract_form(view: View) -> Option<(Vec<horns_core::view::FormRow>, Option<usize>)> {
        use horns_core::view::View as V;
        match view {
            V::Form { rows, focused } => Some((rows, focused)),
            V::Frame { content, .. } => extract_form(*content),
            V::Stack { children, .. } => {
                for (child, _) in children {
                    if let Some(found) = extract_form(child) {
                        return Some(found);
                    }
                }
                None
            }
            V::Pad { child, .. } => extract_form(*child),
            _ => None,
        }
    }

    // -- Section-stack structural tests ------------------------------------------
    //
    // The page is `Frame → Stack[ AccountsSection, ModelsSection ]`. Each
    // section is its own Stack of sub-Lists (and an optional middle slot).
    // Tests here pin that shape; the positioning-regression tests below
    // pin the bug fixes that motivated the structure.

    #[test]
    fn page_emits_frame_stack_of_two_sections() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        let view = render(&mut snap);

        let stack_children = match view {
            View::Frame { content, .. } => match *content {
                View::Stack {
                    dir: Direction::Vertical,
                    children,
                } => children,
                other => panic!("expected Stack inside Frame, got {other:?}"),
            },
            other => panic!("expected Frame, got {other:?}"),
        };
        // Two sections (Accounts, Models) + a trailing Empty filler that
        // absorbs leftover vertical space at the bottom of the page.
        assert_eq!(
            stack_children.len(),
            3,
            "page is two sections + a trailing Fill filler"
        );
        assert!(matches!(stack_children[0].0, View::Stack { .. }));
        assert!(matches!(stack_children[1].0, View::Stack { .. }));
        assert!(matches!(stack_children[2].0, View::Empty));
    }

    #[test]
    fn accounts_section_has_header_only_when_collapsed_and_compose_inactive() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        let view = render(&mut snap);
        let accounts_section = extract_accounts_section(view);

        match accounts_section {
            View::Stack { children, .. } => {
                assert_eq!(
                    children.len(),
                    1,
                    "collapsed compose-inactive Accounts is just the header"
                );
                assert!(matches!(children[0].0, View::List { .. }));
            }
            _ => panic!("AccountsSection should be a Stack"),
        }
    }

    #[test]
    fn accounts_section_adds_affordance_when_expanded_and_compose_inactive() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/accounts".to_string()]),
        );
        write_account(&mut snap, "alpha");

        let view = render(&mut snap);
        let accounts_section = extract_accounts_section(view);

        let children = match accounts_section {
            View::Stack { children, .. } => children,
            _ => panic!("AccountsSection should be a Stack"),
        };
        // Header + affordance + content list.
        assert_eq!(children.len(), 3);
        let middle_items = match &children[1].0 {
            View::List { items, .. } => items,
            _ => panic!("middle slot should be a List of affordance"),
        };
        assert_eq!(middle_items.len(), 1);
        assert!(middle_items[0].primary.contains("+ New connection"));
    }

    #[test]
    fn accounts_section_shows_compose_form_in_middle_when_active() {
        let mut snap = snap_with_compose_active();
        let view = render(&mut snap);
        let accounts_section = extract_accounts_section(view);

        let children = match accounts_section {
            View::Stack { children, .. } => children,
            _ => panic!("AccountsSection should be a Stack"),
        };
        // Header + compose_block (Stack of heading List + Pad → Form).
        // (Content list also present when Accounts is expanded;
        // snap_with_compose_active expands Accounts.)
        assert!(children.len() >= 2);
        // The Form is nested inside the compose_block; do a recursive
        // walk so this test stays anchored to the invariant "form lives
        // inside the Accounts section" without pinning its exact shape.
        fn contains_form(v: &View) -> bool {
            match v {
                View::Form { .. } => true,
                View::Stack { children, .. } => children.iter().any(|(c, _)| contains_form(c)),
                View::Pad { child, .. } => contains_form(child),
                View::Frame { content, .. } => contains_form(content),
                _ => false,
            }
        }
        let form_present = children.iter().any(|(v, _)| contains_form(v));
        assert!(form_present, "compose Form must be inside Accounts section");
        // Header must be the first child (sets the ordering invariant
        // that the positioning regression test covers visually).
        assert!(matches!(children[0].0, View::List { .. }));
    }

    #[test]
    fn models_section_holds_empty_catalog_decorations_inside_its_content() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account(&mut snap, "alpha"); // empty catalog
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );

        let view = render(&mut snap);
        let models_section = extract_models_section(view);
        let children = match models_section {
            View::Stack { children, .. } => children,
            _ => panic!("ModelsSection should be a Stack"),
        };
        assert!(children.len() >= 2);
        let content_items = match &children[1].0 {
            View::List { items, .. } => items,
            _ => panic!("Models content should be a List"),
        };
        assert!(
            content_items.iter().any(|it| it.primary.contains("no models")),
            "empty-state line must live inside the Models content sub-List"
        );
        assert!(
            content_items
                .iter()
                .any(|it| it.primary.contains("add model manually")),
            "manual-add affordance must live inside the Models content sub-List"
        );
    }

    #[test]
    fn focused_path_selects_in_matching_sub_list_only() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account(&mut snap, "alpha");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/accounts".to_string()]),
        );
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            path_to_value(&oxpath!("settings", "accounts", "alpha")),
        );

        let view = render(&mut snap);
        let accounts_section = extract_accounts_section(view);
        let children = match accounts_section {
            View::Stack { children, .. } => children,
            _ => panic!("AccountsSection should be a Stack"),
        };

        // The header sub-List has selected: None (cursor isn't on the header).
        if let View::List { selected, .. } = &children[0].0 {
            assert!(selected.is_none(), "header should not be selected");
        } else {
            panic!("first child should be the header List");
        }
        // The last sub-List is the content (header + affordance + content
        // when expanded compose-inactive). The alpha row sits at index 0
        // of the content list.
        let content_idx = children.len() - 1;
        if let View::List { selected, .. } = &children[content_idx].0 {
            assert_eq!(*selected, Some(0), "content list selects alpha row");
        } else {
            panic!("last child should be the content List");
        }
    }

    // -- Positioning-regression tests --------------------------------------------
    //
    // These pin the two bugs the section-stack refactor structurally fixes:
    // the compose form rendering ABOVE the Connections header, and the
    // Models empty-catalog decoration rendering BEFORE the Models header.

    #[test]
    fn compose_form_renders_below_accounts_header_in_section() {
        let mut snap = snap_with_compose_active();
        let view = render(&mut snap);

        let flat = flatten_to_strings(&view);
        let header_pos = flat
            .iter()
            .position(|s| s.contains("Accounts"))
            .expect("Accounts header present");
        let form_first_field = flat
            .iter()
            .position(|s| s == "Name")
            .expect("compose form Name field present");
        assert!(
            header_pos < form_first_field,
            "Accounts header must render BEFORE compose form fields (got header at {header_pos}, Name at {form_first_field}); \
             flat = {flat:?}"
        );
    }

    #[test]
    fn empty_catalog_decoration_renders_inside_models_section_after_header() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account(&mut snap, "aaa");
        write_account_with_models(&mut snap, "bbb", &["m1"]);
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&[
                "settings/accounts".to_string(),
                "settings/models".to_string(),
            ]),
        );

        let view = render(&mut snap);
        let flat = flatten_to_strings(&view);
        let header_pos = flat
            .iter()
            .position(|s| s.contains("Models"))
            .expect("Models header present");
        let decoration_pos = flat
            .iter()
            .position(|s| s.contains("no models"))
            .expect("decoration present for aaa");
        assert!(
            header_pos < decoration_pos,
            "Models header must render BEFORE empty-catalog decoration (got header at {header_pos}, decoration at {decoration_pos}); \
             flat = {flat:?}"
        );
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
        use horns_core::view::FormValue;
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
    fn compose_form_focused_index_tracks_cursor() {
        use crate::settings::commands::account_model::FIELD_ORDER;
        for field in FIELD_ORDER {
            let mut snap = snap_with_compose_active();
            // Override the cursor to point at the named field. Under
            // cursor-as-focus this IS the focus assignment — no
            // separate `focused_field` discriminator exists.
            let subpath = match field {
                ox_types::AccountField::Name => "name",
                ox_types::AccountField::Protocol => "protocol",
                ox_types::AccountField::Endpoint => "endpoint",
                ox_types::AccountField::Auth => "auth",
                ox_types::AccountField::Key => "key",
            };
            let comp = ox_kernel::PathComponent::try_new(subpath).unwrap();
            snap.insert(
                &oxpath!("ui", "settings", "focused"),
                path_to_value(&oxpath!("settings", "_compose_form", comp)),
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
        use horns_core::view::FormValue;
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

    /// Recursive walker: returns true if any `View::Pad` is found whose
    /// subtree contains a `View::Form`. Used by the compose-form padding
    /// test to assert the indent wrapping without pinning the exact
    /// nesting depth.
    fn view_contains_pad_wrapping_form(view: &View) -> bool {
        fn contains_form(v: &View) -> bool {
            match v {
                View::Form { .. } => true,
                View::Stack { children, .. } => children.iter().any(|(c, _)| contains_form(c)),
                View::Frame { content, .. } => contains_form(content),
                View::Pad { child, .. } => contains_form(child),
                _ => false,
            }
        }
        match view {
            View::Pad { child, .. } if contains_form(child) => true,
            View::Stack { children, .. } => {
                children.iter().any(|(c, _)| view_contains_pad_wrapping_form(c))
            }
            View::Frame { content, .. } => view_contains_pad_wrapping_form(content),
            View::Pad { child, .. } => view_contains_pad_wrapping_form(child),
            _ => false,
        }
    }

    #[test]
    fn compose_active_renders_heading_above_form() {
        let mut snap = snap_with_compose_active();
        let view = render(&mut snap);
        let flat = flatten_to_strings(&view);

        let heading_pos = flat
            .iter()
            .position(|s| s.contains("+ New connection") && s.contains("Tab to navigate"))
            .expect("heading present with help text");
        let name_field_pos = flat
            .iter()
            .position(|s| s == "Name")
            .expect("Name field present");
        assert!(
            heading_pos < name_field_pos,
            "heading must appear before Name field; flat = {flat:?}"
        );
    }

    #[test]
    fn compose_active_form_is_padded() {
        let mut snap = snap_with_compose_active();
        let view = render(&mut snap);
        assert!(
            view_contains_pad_wrapping_form(&view),
            "compose form must be wrapped in View::Pad for indentation"
        );
    }

    #[test]
    fn compose_inactive_affordance_unchanged() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/accounts".to_string()]),
        );
        write_account(&mut snap, "alpha");

        let view = render(&mut snap);
        let flat = flatten_to_strings(&view);
        // Still has the bare "+ New connection" affordance, NO Tab/Enter/Esc
        // help text — that text only appears when compose is active and the
        // affordance has been promoted to a heading.
        let bare_affordance = flat
            .iter()
            .any(|s| s.contains("+ New connection") && !s.contains("Tab to navigate"));
        assert!(
            bare_affordance,
            "inactive affordance is the plain '+ New connection' line; flat = {flat:?}"
        );
    }

    #[test]
    fn selector_with_value_renders_selector_form_value() {
        // Once the user has picked a protocol, the row switches from
        // the placeholder `ReadOnly` to a `Selector { options, current }`.
        use crate::settings::commands::account_model::FIELD_ORDER;
        use horns_core::view::FormValue;
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

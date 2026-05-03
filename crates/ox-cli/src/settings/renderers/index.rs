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

use ox_view::{ListItem, View};

use crate::settings::registry::{AscendRule, RenderCtx, Renderer, RendererRegistry};
use crate::settings::visible_rows;

pub struct IndexRenderer;

impl Renderer for IndexRenderer {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View {
        let rows = visible_rows::enumerate(ctx.data);

        let cursor = read_cursor(ctx.data);
        let selected = cursor
            .as_ref()
            .and_then(|c| visible_rows::position_of(&rows, c));

        let items: Vec<ListItem> = rows
            .iter()
            .map(|row| {
                let indent = "  ".repeat(row.depth);
                let glyph = if row.expandable {
                    if row.expanded { "▾ " } else { "▸ " }
                } else {
                    "  "
                };
                ListItem {
                    primary: format!("{indent}{glyph}{}", row.label),
                    secondary: None,
                    badge: row.badge.clone(),
                }
            })
            .collect();

        let selected = selected.filter(|i| !items.is_empty() && *i < items.len());

        View::List {
            title: Some("Settings".into()),
            items,
            selected,
        }
    }

    fn ascend_to(&self) -> AscendRule {
        AscendRule::ExitScreen
    }
}

fn read_cursor(data: &mut dyn structfs_core_store::Reader) -> Option<structfs_core_store::Path> {
    use crate::settings::commands::navigation::path_from_value;
    use ox_path::oxpath;

    // Reads the focused-row pointer, NOT `ui/settings/cursor`. The
    // page-level cursor (binding scope) stays at `settings/index`
    // while the accordion screen is active; the focused row inside
    // the tree lives at its own path.
    let r = data
        .read(&oxpath!("ui", "settings", "focused_row"))
        .ok()
        .flatten()?;
    path_from_value(r.as_value()?)
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
    use structfs_core_store::Path;
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
        match view {
            View::List {
                title,
                items,
                selected,
            } => (title, items, selected),
            other => panic!("expected View::List, got {other:?}"),
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
        // Accounts (▾) + alpha + beta + Models (▸) = 4 rows
        assert_eq!(items.len(), 4);
        assert!(items[0].primary.starts_with("▾ "));
        assert!(
            items[1].primary.starts_with("    "),
            "expected depth-1 indent; got {:?}",
            items[1].primary
        );
        assert!(items[1].primary.ends_with("alpha"));
        assert!(items[2].primary.ends_with("beta"));
        assert!(items[3].primary.starts_with("▸ "));
    }

    #[test]
    fn focused_row_drives_selected_index() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        snap.insert(
            &oxpath!("ui", "settings", "focused_row"),
            path_to_value(&oxpath!("settings", "models")),
        );
        let (_title, _items, selected) = assert_list(render(&mut snap));
        assert_eq!(selected, Some(1));
    }

    #[test]
    fn stale_focused_row_yields_no_selection() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        snap.insert(
            &oxpath!("ui", "settings", "focused_row"),
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
            View::List { items, .. } => assert_eq!(items.len(), 2),
            other => panic!("unexpected: {other:?}"),
        }
    }
}

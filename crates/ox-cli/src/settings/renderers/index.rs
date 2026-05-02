//! Settings index page (`settings/index`).
//!
//! Displays the list of top-level settings categories with optional badges.
//! Per spec §6.1: a `View::List` of `SettingsIndexEntry` records, each with
//! its badge resolved synchronously from the snapshot.
//!
//! Badge resolution per spec §6.1:
//! - `BadgeSource::None`               → `""`
//! - `BadgeSource::Static(s)`          → `s`
//! - `BadgeSource::SubtreeCount(p)`    → `count.to_string()` (children of `p`)
//! - `BadgeSource::PrimaryReference`   → `"{account} / {model_id}"` from
//!   `config/completions/primary`, or `""` if absent.

use ox_path::oxpath;
use ox_view::{ListItem, View};

use ox_gate::CompletionRole;
use ox_types::{BadgeSource, SettingsIndexEntry};

use crate::settings::registry::{AscendRule, RenderCtx, Renderer, RendererRegistry};

use super::util::{child_names_under, read_typed, subtree_count};

pub struct IndexRenderer;

impl Renderer for IndexRenderer {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View {
        // Discover entry ids by listing direct children under the prefix
        // and read each entry as a `SettingsIndexEntry`.
        let entry_ids = child_names_under(ctx.data, "settings/index/entries");
        let mut entries: Vec<SettingsIndexEntry> = Vec::with_capacity(entry_ids.len());
        for id in &entry_ids {
            let comp = match ox_kernel::PathComponent::try_new(id) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let path = oxpath!("settings", "index", "entries", comp);
            if let Some(entry) = read_typed::<SettingsIndexEntry>(ctx.data, &path) {
                entries.push(entry);
            }
        }

        let selected =
            read_typed::<usize>(ctx.data, &oxpath!("ui", "settings", "index", "selected"));

        let items: Vec<ListItem> = entries
            .iter()
            .map(|entry| {
                let badge = resolve_badge(ctx.data, &entry.badge);
                ListItem {
                    primary: entry.label.clone(),
                    secondary: Some(entry.description.clone()),
                    badge: Some(badge),
                }
            })
            .collect();

        // `selected` only makes sense when there is at least one item, and the
        // index must be in range; otherwise present `None`.
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

fn resolve_badge(data: &mut dyn structfs_core_store::Reader, source: &BadgeSource) -> String {
    match source {
        BadgeSource::None => String::new(),
        BadgeSource::Static(s) => s.clone(),
        BadgeSource::SubtreeCount(p) => subtree_count(data, &p.to_string()).to_string(),
        BadgeSource::PrimaryReference => {
            match read_typed::<CompletionRole>(
                data,
                &oxpath!("config", "gate", "completions", "primary"),
            ) {
                Some(role) => format!("{} / {}", role.account, role.model_id),
                None => String::new(),
            }
        }
    }
}

pub fn register(reg: &mut RendererRegistry) {
    reg.register(oxpath!("settings", "index"), Box::new(IndexRenderer));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::layout::Rect;
    use structfs_core_store::Value;
    use structfs_serde_store::to_value;

    use crate::settings::registry::RendererRegistry;
    use crate::settings::snapshot::SettingsSnapshot;
    use crate::theme::Theme;

    fn entry(id: &str, label: &str, description: &str, badge: BadgeSource) -> SettingsIndexEntry {
        SettingsIndexEntry {
            id: id.to_string(),
            label: label.to_string(),
            description: description.to_string(),
            target_cursor: structfs_core_store::Path::parse(&format!("settings/{id}")).unwrap(),
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

    #[test]
    fn index_renders_two_entries_with_badges() {
        let mut snap = SettingsSnapshot::empty();

        // Two entries: Accounts (SubtreeCount badge) and Models (PrimaryReference badge).
        let accounts_entry = entry(
            "accounts",
            "Accounts",
            "Manage provider accounts",
            BadgeSource::SubtreeCount(
                structfs_core_store::Path::parse("config/gate/accounts").unwrap(),
            ),
        );
        let models_entry = entry(
            "models",
            "Models",
            "Per-model overrides",
            BadgeSource::PrimaryReference,
        );
        snap.insert(
            &oxpath!("settings", "index", "entries", "accounts"),
            to_value(&accounts_entry).unwrap(),
        );
        snap.insert(
            &oxpath!("settings", "index", "entries", "models"),
            to_value(&models_entry).unwrap(),
        );

        // Two accounts populate the SubtreeCount source.
        snap.insert(
            &oxpath!("config", "gate", "accounts", "a", "provider"),
            Value::String("anthropic".into()),
        );
        snap.insert(
            &oxpath!("config", "gate", "accounts", "b", "provider"),
            Value::String("openai".into()),
        );

        // Primary completion role populates the PrimaryReference badge.
        let role = CompletionRole {
            account: "a".into(),
            model_id: "m".into(),
        };
        snap.insert(
            &oxpath!("config", "gate", "completions", "primary"),
            to_value(&role).unwrap(),
        );

        snap.insert(
            &oxpath!("ui", "settings", "index", "selected"),
            Value::Integer(0),
        );

        let view = render(&mut snap);

        let expected = View::List {
            title: Some("Settings".into()),
            items: vec![
                ListItem {
                    primary: "Accounts".into(),
                    secondary: Some("Manage provider accounts".into()),
                    badge: Some("2".into()),
                },
                ListItem {
                    primary: "Models".into(),
                    secondary: Some("Per-model overrides".into()),
                    badge: Some("a / m".into()),
                },
            ],
            selected: Some(0),
        };
        assert_eq!(view, expected);
    }

    #[test]
    fn index_renders_empty_when_no_entries() {
        let mut snap = SettingsSnapshot::empty();
        let view = render(&mut snap);
        let expected = View::List {
            title: Some("Settings".into()),
            items: vec![],
            selected: None,
        };
        assert_eq!(view, expected);
    }

    #[test]
    fn index_subtree_count_zero_when_prefix_empty() {
        let mut snap = SettingsSnapshot::empty();
        let e = entry(
            "accounts",
            "Accounts",
            "Manage provider accounts",
            BadgeSource::SubtreeCount(
                structfs_core_store::Path::parse("config/gate/accounts").unwrap(),
            ),
        );
        snap.insert(
            &oxpath!("settings", "index", "entries", "accounts"),
            to_value(&e).unwrap(),
        );

        let view = render(&mut snap);
        let expected = View::List {
            title: Some("Settings".into()),
            items: vec![ListItem {
                primary: "Accounts".into(),
                secondary: Some("Manage provider accounts".into()),
                badge: Some("0".into()),
            }],
            selected: None,
        };
        assert_eq!(view, expected);
    }

    #[test]
    fn ascend_rule_is_exit_screen() {
        assert_eq!(IndexRenderer.ascend_to(), AscendRule::ExitScreen);
    }
}

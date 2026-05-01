//! Models list page (`settings/models`).
//!
//! Per spec §6.6: a unified per-account model browser. Flattens every
//! `config/gate/accounts/{name}/models: Vec<ModelInfo>` into one row per
//! `(account, model_id)`. Columns:
//!
//! - `primary`:   `"{account} / {display_name}"`
//! - `secondary`: `Some(model.id)`
//! - `badge`:     joined source-tag (`server` / `known` / `override`) +
//!                primary-tag (`★` for the model bound by
//!                `config/completions/primary: CompletionRole`) +
//!                refresh-status chrome (e.g. `refreshing`).
//!
//! Selection (`ui/settings/models/selected: Option<ModelKey>`) translates
//! to the matching list index, or `None` when the pointer is absent or
//! names a vanished `(account, model)` pair.

use ox_path::oxpath;
use ox_view::{ListItem, View};

use ox_gate::{CatalogRefreshStatus, CompletionRole, ModelInfo, ModelInfoSource};
use ox_types::ModelKey;

use crate::settings::registry::{AscendRule, RenderCtx, Renderer, RendererRegistry};

use super::util::{child_names_under, read_typed};

pub struct ModelsListRenderer;

impl Renderer for ModelsListRenderer {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View {
        let account_names = child_names_under(ctx.data, "config/gate/accounts");
        let primary: Option<CompletionRole> =
            read_typed(ctx.data, &oxpath!("config", "completions", "primary"));
        let selected: Option<ModelKey> =
            read_typed::<Option<ModelKey>>(ctx.data, &oxpath!("ui", "settings", "models", "selected"))
                .flatten();

        let mut items: Vec<ListItem> = Vec::new();
        // Cache (name, refresh_status_tag) so we don't read it per-model.
        let mut keys: Vec<(String, String)> = Vec::new();

        for name in &account_names {
            let comp = match ox_kernel::PathComponent::try_new(name) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let refresh: Option<CatalogRefreshStatus> = read_typed(
                ctx.data,
                &oxpath!("config", "gate", "accounts", comp.clone(), "refresh_status"),
            );
            let refresh_tag = refresh_chrome_tag(&refresh);
            let models: Vec<ModelInfo> = read_typed(
                ctx.data,
                &oxpath!("config", "gate", "accounts", comp, "models"),
            )
            .unwrap_or_default();

            for model in models {
                let badge = build_badge(name, &model, primary.as_ref(), &refresh_tag);
                items.push(ListItem {
                    primary: format!("{} / {}", name, model.display_name),
                    secondary: Some(model.id.clone()),
                    badge: Some(badge),
                });
                keys.push((name.clone(), model.id.clone()));
            }
        }

        let selected_idx = selected.and_then(|key| {
            keys.iter()
                .position(|(a, m)| a == &key.account && m == &key.model_id)
        });

        View::List {
            title: Some("Models".into()),
            items,
            selected: selected_idx,
        }
    }

    fn ascend_to(&self) -> AscendRule {
        AscendRule::NearestRegistered
    }
}

fn refresh_chrome_tag(refresh: &Option<CatalogRefreshStatus>) -> Option<&'static str> {
    match refresh {
        Some(CatalogRefreshStatus::Refreshing { .. }) => Some("refreshing"),
        Some(CatalogRefreshStatus::Failed { .. }) => Some("refresh-failed"),
        _ => None,
    }
}

fn build_badge(
    account: &str,
    model: &ModelInfo,
    primary: Option<&CompletionRole>,
    refresh_tag: &Option<&'static str>,
) -> String {
    let source = match model.source {
        ModelInfoSource::Server => "server",
        ModelInfoSource::KnownTable => "known",
        ModelInfoSource::UserOverride => "override",
    };
    let mut parts: Vec<String> = vec![source.to_string()];
    let is_primary = primary
        .map(|r| r.account == account && r.model_id == model.id)
        .unwrap_or(false);
    if is_primary {
        parts.push("★".to_string());
    }
    if let Some(tag) = refresh_tag {
        parts.push((*tag).to_string());
    }
    parts.join(" ")
}

pub fn register(reg: &mut RendererRegistry) {
    reg.register(oxpath!("settings", "models"), Box::new(ModelsListRenderer));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::layout::Rect;
    use structfs_serde_store::to_value;

    use crate::settings::registry::RendererRegistry;
    use crate::settings::snapshot::SettingsSnapshot;
    use crate::theme::Theme;
    use ox_gate::AccountConfig;

    fn render(snap: &mut SettingsSnapshot) -> View {
        let theme = Theme::default();
        let registry = RendererRegistry::new();
        let mut ctx = RenderCtx {
            area: Rect::new(0, 0, 80, 24),
            data: snap,
            registry: &registry,
            theme: &theme,
        };
        ModelsListRenderer.render(&mut ctx)
    }

    fn write_account_with_models(
        snap: &mut SettingsSnapshot,
        name: &str,
        models: Vec<ModelInfo>,
    ) {
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp.clone()),
            to_value(&AccountConfig {
                provider: name.into(),
            })
            .unwrap(),
        );
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp, "models"),
            to_value(&models).unwrap(),
        );
    }

    fn model(id: &str, display_name: &str, source: ModelInfoSource) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            display_name: display_name.into(),
            max_context_size: Some(200_000),
            max_output_tokens: Some(8192),
            source,
        }
    }

    #[test]
    fn models_list_empty() {
        let mut snap = SettingsSnapshot::empty();
        let view = render(&mut snap);
        let expected = View::List {
            title: Some("Models".into()),
            items: vec![],
            selected: None,
        };
        assert_eq!(view, expected);
    }

    #[test]
    fn models_list_three_accounts() {
        let mut snap = SettingsSnapshot::empty();
        write_account_with_models(
            &mut snap,
            "alpha",
            vec![model("m1", "Model One", ModelInfoSource::Server)],
        );
        write_account_with_models(
            &mut snap,
            "beta",
            vec![
                model("m2", "Model Two", ModelInfoSource::Server),
                model("m3", "Model Three", ModelInfoSource::KnownTable),
            ],
        );
        write_account_with_models(
            &mut snap,
            "gamma",
            vec![model("m4", "Model Four", ModelInfoSource::UserOverride)],
        );

        let view = render(&mut snap);
        let items = match view {
            View::List { items, .. } => items,
            other => panic!("expected List, got {other:?}"),
        };
        // Four rows in account-then-model order.
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].primary, "alpha / Model One");
        assert_eq!(items[1].primary, "beta / Model Two");
        assert_eq!(items[2].primary, "beta / Model Three");
        assert_eq!(items[3].primary, "gamma / Model Four");
        // No primary set → no ★ in any badge.
        assert!(items.iter().all(|i| !i.badge.as_ref().unwrap().contains('★')));
    }

    #[test]
    fn models_list_primary_tagged() {
        let mut snap = SettingsSnapshot::empty();
        write_account_with_models(
            &mut snap,
            "alpha",
            vec![
                model("m1", "Model One", ModelInfoSource::Server),
                model("m2", "Model Two", ModelInfoSource::Server),
            ],
        );
        snap.insert(
            &oxpath!("config", "completions", "primary"),
            to_value(&CompletionRole {
                account: "alpha".into(),
                model_id: "m2".into(),
            })
            .unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "models", "selected"),
            to_value(&Some(ModelKey {
                account: "alpha".into(),
                model_id: "m2".into(),
            }))
            .unwrap(),
        );

        let view = render(&mut snap);
        let (items, selected) = match view {
            View::List { items, selected, .. } => (items, selected),
            other => panic!("expected List, got {other:?}"),
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].badge.as_deref(), Some("server"));
        assert_eq!(items[1].badge.as_deref(), Some("server ★"));
        assert_eq!(selected, Some(1));
    }

    #[test]
    fn models_list_unknown_token_fields() {
        // A model where token fields are None (max_context_size / max_output_tokens
        // both unknown) — the renderer surfaces it just like a fully-known model;
        // token introspection happens on the detail page, not the list.
        let mut snap = SettingsSnapshot::empty();
        write_account_with_models(
            &mut snap,
            "alpha",
            vec![ModelInfo {
                id: "m1".into(),
                display_name: "Model One".into(),
                max_context_size: None,
                max_output_tokens: None,
                source: ModelInfoSource::KnownTable,
            }],
        );

        let view = render(&mut snap);
        let items = match view {
            View::List { items, .. } => items,
            other => panic!("expected List, got {other:?}"),
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].primary, "alpha / Model One");
        assert_eq!(items[0].secondary.as_deref(), Some("m1"));
        assert_eq!(items[0].badge.as_deref(), Some("known"));
    }

    #[test]
    fn models_list_refresh_chrome() {
        let mut snap = SettingsSnapshot::empty();
        write_account_with_models(
            &mut snap,
            "alpha",
            vec![model("m1", "Model One", ModelInfoSource::Server)],
        );
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp, "refresh_status"),
            to_value(&CatalogRefreshStatus::Refreshing {
                started_at_ms: 1_000,
            })
            .unwrap(),
        );

        let view = render(&mut snap);
        let items = match view {
            View::List { items, .. } => items,
            other => panic!("expected List, got {other:?}"),
        };
        assert_eq!(items.len(), 1);
        assert!(
            items[0].badge.as_ref().unwrap().contains("refreshing"),
            "expected refreshing chrome, got {:?}",
            items[0].badge
        );
    }

    #[test]
    fn ascend_rule_is_nearest_registered() {
        assert_eq!(ModelsListRenderer.ascend_to(), AscendRule::NearestRegistered);
    }
}

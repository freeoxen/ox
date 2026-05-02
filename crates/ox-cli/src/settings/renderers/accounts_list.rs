//! Accounts list page (`settings/accounts`).
//!
//! Per spec §6.2: a `View::List` of accounts. Each row shows the account
//! name, the resolved provider host (from `config/gate/providers/{ref}`),
//! and a key-presence indicator (`✓key` / `–`) read from
//! `secret/keys/{name}: ApiKey`.
//!
//! Selection is driven by `ui/settings/accounts/selected: Option<String>`;
//! the renderer translates it to the matching list index, or `None` when
//! the pointer is absent or names a vanished account.

use ox_path::oxpath;
use ox_view::{ListItem, View};

use ox_gate::{AccountConfig, ApiKey, ProviderConfig};

use crate::settings::registry::{AscendRule, RenderCtx, Renderer, RendererRegistry};

use super::util::{child_names_under, read_typed};

pub struct AccountsListRenderer;

impl Renderer for AccountsListRenderer {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View {
        let names = child_names_under(ctx.data, "config/gate/accounts");
        let selected_name = read_typed::<Option<String>>(
            ctx.data,
            &oxpath!("ui", "settings", "accounts", "selected"),
        )
        .flatten();

        let mut items: Vec<ListItem> = Vec::with_capacity(names.len());
        for name in &names {
            let comp = match ox_kernel::PathComponent::try_new(name) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let acct: Option<AccountConfig> = read_typed(
                ctx.data,
                &oxpath!("config", "gate", "accounts", comp.clone()),
            );
            let provider_ref = acct.map(|a| a.provider).unwrap_or_default();
            let host = if !provider_ref.is_empty() {
                let pref_comp = ox_kernel::PathComponent::try_new(&provider_ref).ok();
                pref_comp
                    .and_then(|c| {
                        read_typed::<ProviderConfig>(
                            ctx.data,
                            &oxpath!("config", "gate", "providers", c),
                        )
                    })
                    .map(|p| p.endpoint)
                    .unwrap_or_default()
            } else {
                String::new()
            };

            let key_present = read_typed::<ApiKey>(ctx.data, &oxpath!("secret", "keys", comp))
                .map(|k| !k.is_empty())
                .unwrap_or(false);
            let badge = if key_present { "✓key" } else { "–" }.to_string();

            items.push(ListItem {
                primary: name.clone(),
                secondary: Some(host),
                badge: Some(badge),
            });
        }

        let selected = selected_name.and_then(|name| items.iter().position(|i| i.primary == name));

        View::List {
            title: Some("Accounts".into()),
            items,
            selected,
        }
    }

    fn ascend_to(&self) -> AscendRule {
        AscendRule::Fallback(oxpath!("settings", "index"))
    }
}

pub fn register(reg: &mut RendererRegistry) {
    reg.register(
        oxpath!("settings", "accounts"),
        Box::new(AccountsListRenderer),
    );
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

    fn render(snap: &mut SettingsSnapshot) -> View {
        let theme = Theme::default();
        let registry = RendererRegistry::new();
        let mut ctx = RenderCtx {
            area: Rect::new(0, 0, 80, 24),
            data: snap,
            registry: &registry,
            theme: &theme,
        };
        AccountsListRenderer.render(&mut ctx)
    }

    fn write_account(snap: &mut SettingsSnapshot, name: &str, provider: &str) {
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        let acct = AccountConfig {
            provider: provider.to_string(),
        };
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp),
            to_value(&acct).unwrap(),
        );
    }

    fn write_provider(snap: &mut SettingsSnapshot, name: &str, endpoint: &str) {
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        let pc = ProviderConfig {
            dialect: name.to_string(),
            endpoint: endpoint.to_string(),
            version: String::new(),
            auth: None,
        };
        snap.insert(
            &oxpath!("config", "gate", "providers", comp),
            to_value(&pc).unwrap(),
        );
    }

    fn write_key(snap: &mut SettingsSnapshot, name: &str, key: &str) {
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        snap.insert(
            &oxpath!("secret", "keys", comp),
            to_value(&ApiKey::new(key)).unwrap(),
        );
    }

    #[test]
    fn accounts_list_empty() {
        let mut snap = SettingsSnapshot::empty();
        let view = render(&mut snap);
        let expected = View::List {
            title: Some("Accounts".into()),
            items: vec![],
            selected: None,
        };
        assert_eq!(view, expected);
    }

    #[test]
    fn accounts_list_three_with_keys_and_selection() {
        let mut snap = SettingsSnapshot::empty();
        write_provider(&mut snap, "anthropic", "https://api.anthropic.com");
        write_provider(&mut snap, "openai", "https://api.openai.com");

        write_account(&mut snap, "alpha", "anthropic");
        write_account(&mut snap, "beta", "openai");
        write_account(&mut snap, "gamma", "anthropic");

        write_key(&mut snap, "alpha", "sk-1");
        // beta has no key
        write_key(&mut snap, "gamma", "sk-3");

        snap.insert(
            &oxpath!("ui", "settings", "accounts", "selected"),
            to_value(&Some("beta".to_string())).unwrap(),
        );

        let view = render(&mut snap);
        let expected = View::List {
            title: Some("Accounts".into()),
            items: vec![
                ListItem {
                    primary: "alpha".into(),
                    secondary: Some("https://api.anthropic.com".into()),
                    badge: Some("✓key".into()),
                },
                ListItem {
                    primary: "beta".into(),
                    secondary: Some("https://api.openai.com".into()),
                    badge: Some("–".into()),
                },
                ListItem {
                    primary: "gamma".into(),
                    secondary: Some("https://api.anthropic.com".into()),
                    badge: Some("✓key".into()),
                },
            ],
            selected: Some(1),
        };
        assert_eq!(view, expected);
    }

    #[test]
    fn accounts_list_no_selection() {
        let mut snap = SettingsSnapshot::empty();
        write_provider(&mut snap, "anthropic", "https://api.anthropic.com");
        write_account(&mut snap, "alpha", "anthropic");
        write_key(&mut snap, "alpha", "sk-1");

        let view = render(&mut snap);
        let expected = View::List {
            title: Some("Accounts".into()),
            items: vec![ListItem {
                primary: "alpha".into(),
                secondary: Some("https://api.anthropic.com".into()),
                badge: Some("✓key".into()),
            }],
            selected: None,
        };
        assert_eq!(view, expected);
    }

    #[test]
    fn ascend_rule_is_fallback_to_settings_index() {
        assert_eq!(
            AccountsListRenderer.ascend_to(),
            AscendRule::Fallback(oxpath!("settings", "index"))
        );
    }
}

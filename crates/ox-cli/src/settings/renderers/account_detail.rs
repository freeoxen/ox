//! Account detail page (`settings/accounts/_detail`).
//!
//! Per spec §6.3: a `View::Stack { Vertical, [Form, StatusBlock] }`.
//!
//! The Form has one row per `AccountField` variant (Name, Protocol,
//! Endpoint, Auth, Key). Each row's `error` is populated from
//! `validation_status.field_errors` when present. The focused row is
//! determined by `ui/settings/account_detail/field`; the focused text
//! field's cursor by `ui/settings/edit_cursor`.
//!
//! The StatusBlock renders the test/refresh status as styled lines,
//! one per status, in a stable order (test then refresh).
//!
//! Empty-state (selection is `None`, or names a vanished account): a
//! single `View::Text` line — no Form, no StatusBlock.

use ox_path::oxpath;
use ox_view::{
    Color, Direction, FormRow, FormValue, ModifierSet, Sizing, Span, StyledLine, Style, View,
};

use ox_gate::{
    AccountConfig, AccountTestStatus, ApiKey, AuthScheme, CatalogRefreshStatus, ProviderConfig,
};
use ox_types::{AccountField, ValidationDiagnostics};

use crate::settings::registry::{AscendRule, RenderCtx, Renderer, RendererRegistry};

use super::util::read_typed;

pub struct AccountDetailRenderer;

const FIELD_ORDER: &[AccountField] = &[
    AccountField::Name,
    AccountField::Protocol,
    AccountField::Endpoint,
    AccountField::Auth,
    AccountField::Key,
];

const PROTOCOL_OPTIONS: &[&str] = &["anthropic", "openai"];
const AUTH_OPTIONS: &[&str] = &["x-api-key", "bearer-token", "none"];

impl Renderer for AccountDetailRenderer {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View {
        let selected = match read_typed::<Option<String>>(
            ctx.data,
            &oxpath!("ui", "settings", "accounts", "selected"),
        )
        .flatten()
        {
            Some(s) => s,
            None => return View::text("No account selected. Press Esc to return."),
        };

        let comp = match ox_kernel::PathComponent::try_new(&selected) {
            Ok(c) => c,
            Err(_) => return View::text("No account selected. Press Esc to return."),
        };

        let acct: AccountConfig = match read_typed(
            ctx.data,
            &oxpath!("config", "gate", "accounts", comp.clone()),
        ) {
            Some(a) => a,
            None => {
                return View::text(format!(
                    "Account '{}' was removed. Press Esc to return.",
                    selected
                ));
            }
        };

        let provider: Option<ProviderConfig> = ox_kernel::PathComponent::try_new(&acct.provider)
            .ok()
            .and_then(|pc| read_typed(ctx.data, &oxpath!("config", "gate", "providers", pc)));

        let key: Option<ApiKey> = read_typed(ctx.data, &oxpath!("secret", "keys", comp.clone()));

        let test_status: AccountTestStatus = read_typed(
            ctx.data,
            &oxpath!("config", "gate", "accounts", comp.clone(), "test_status"),
        )
        .unwrap_or(AccountTestStatus::Idle);

        let refresh_status: CatalogRefreshStatus = read_typed(
            ctx.data,
            &oxpath!(
                "config",
                "gate",
                "accounts",
                comp.clone(),
                "refresh_status"
            ),
        )
        .unwrap_or(CatalogRefreshStatus::Idle);

        let validation: ValidationDiagnostics = read_typed(
            ctx.data,
            &oxpath!(
                "config",
                "gate",
                "accounts",
                comp.clone(),
                "validation_status"
            ),
        )
        .unwrap_or(ValidationDiagnostics {
            field_errors: Default::default(),
            computed_at_ms: 0,
        });

        let focused_field: AccountField = read_typed(
            ctx.data,
            &oxpath!("ui", "settings", "account_detail", "field"),
        )
        .unwrap_or(AccountField::Name);

        let edit_cursor: u32 =
            read_typed(ctx.data, &oxpath!("ui", "settings", "edit_cursor")).unwrap_or(0);

        let rows: Vec<FormRow> = FIELD_ORDER
            .iter()
            .map(|field| {
                let label = field_label(*field);
                let value = field_value(
                    *field,
                    &selected,
                    &acct,
                    provider.as_ref(),
                    key.as_ref(),
                    edit_cursor,
                    *field == focused_field,
                );
                let error = validation.field_errors.get(field).cloned();
                FormRow {
                    label: label.into(),
                    value,
                    error,
                    hint: None,
                }
            })
            .collect();

        let focused = FIELD_ORDER.iter().position(|f| *f == focused_field);

        let form = View::Form {
            title: Some(format!("Account: {selected}")),
            rows,
            focused,
        };

        let status = status_block(&test_status, &refresh_status);

        View::Stack {
            dir: Direction::Vertical,
            children: vec![(form, Sizing::Min(7)), (status, Sizing::Fill)],
        }
    }

    fn ascend_to(&self) -> AscendRule {
        AscendRule::NearestRegistered
    }
}

fn field_label(field: AccountField) -> &'static str {
    match field {
        AccountField::Name => "Name",
        AccountField::Protocol => "Protocol",
        AccountField::Endpoint => "Endpoint",
        AccountField::Auth => "Auth",
        AccountField::Key => "Key",
    }
}

fn field_value(
    field: AccountField,
    name: &str,
    acct: &AccountConfig,
    provider: Option<&ProviderConfig>,
    key: Option<&ApiKey>,
    cursor: u32,
    focused: bool,
) -> FormValue {
    match field {
        // Name is the path key, immutable post-creation per spec §6.4.
        AccountField::Name => FormValue::ReadOnly(name.to_string()),
        AccountField::Protocol => {
            let current = PROTOCOL_OPTIONS
                .iter()
                .position(|opt| *opt == acct.provider)
                .unwrap_or(0);
            FormValue::Selector {
                options: PROTOCOL_OPTIONS.iter().map(|s| s.to_string()).collect(),
                current,
            }
        }
        AccountField::Endpoint => {
            let value = provider.map(|p| p.endpoint.clone()).unwrap_or_default();
            let cursor = if focused { cursor } else { 0 };
            FormValue::Text {
                value,
                cursor,
                masked: false,
            }
        }
        AccountField::Auth => {
            let scheme = provider
                .map(|p| p.resolved_auth())
                .unwrap_or(AuthScheme::None);
            let scheme_str = match scheme {
                AuthScheme::XApiKey => "x-api-key",
                AuthScheme::BearerToken => "bearer-token",
                AuthScheme::None => "none",
            };
            let current = AUTH_OPTIONS
                .iter()
                .position(|opt| *opt == scheme_str)
                .unwrap_or(0);
            FormValue::Selector {
                options: AUTH_OPTIONS.iter().map(|s| s.to_string()).collect(),
                current,
            }
        }
        AccountField::Key => {
            let value = key.map(|k| k.expose().to_string()).unwrap_or_default();
            let cursor = if focused { cursor } else { 0 };
            FormValue::Text {
                value,
                cursor,
                masked: true,
            }
        }
    }
}

fn status_block(test: &AccountTestStatus, refresh: &CatalogRefreshStatus) -> View {
    let mut lines: Vec<StyledLine> = Vec::new();

    match test {
        AccountTestStatus::Idle => {
            lines.push(neutral_line("Test: idle"));
        }
        AccountTestStatus::Testing { .. } => {
            lines.push(neutral_line("Test: Testing…"));
        }
        AccountTestStatus::Success {
            dialect,
            latency_ms,
            ..
        } => {
            lines.push(success_line(format!(
                "✓ {} responded in {}ms",
                dialect, latency_ms
            )));
        }
        AccountTestStatus::Failed { reason, .. } => {
            lines.push(error_line(format!("✗ {}", reason)));
        }
    }

    match refresh {
        CatalogRefreshStatus::Idle => {
            lines.push(neutral_line("Catalog: idle"));
        }
        CatalogRefreshStatus::Refreshing { .. } => {
            lines.push(neutral_line("Catalog: Refreshing…"));
        }
        CatalogRefreshStatus::Success {
            models_added,
            models_updated,
            ..
        } => {
            lines.push(success_line(format!(
                "✓ Catalog: +{} added, {} updated",
                models_added, models_updated
            )));
        }
        CatalogRefreshStatus::Failed { reason, .. } => {
            lines.push(error_line(format!("✗ Catalog: {}", reason)));
        }
    }

    View::StatusBlock {
        title: "Status".into(),
        lines,
        scroll_offset: 0,
    }
}

fn neutral_line(s: impl Into<String>) -> StyledLine {
    StyledLine(vec![Span::plain(s)])
}

fn success_line(s: impl Into<String>) -> StyledLine {
    StyledLine(vec![Span {
        text: s.into(),
        style: Style {
            fg: Some(Color::Green),
            bg: None,
            modifiers: ModifierSet::default(),
        },
    }])
}

fn error_line(s: impl Into<String>) -> StyledLine {
    StyledLine(vec![Span {
        text: s.into(),
        style: Style {
            fg: Some(Color::Red),
            bg: None,
            modifiers: ModifierSet {
                bold: true,
                ..ModifierSet::default()
            },
        },
    }])
}

pub fn register(reg: &mut RendererRegistry) {
    reg.register(
        oxpath!("settings", "accounts", "_detail"),
        Box::new(AccountDetailRenderer),
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::layout::Rect;
    use std::collections::BTreeMap;
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
        AccountDetailRenderer.render(&mut ctx)
    }

    fn write_account(snap: &mut SettingsSnapshot, name: &str, provider: &str) {
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp),
            to_value(&AccountConfig {
                provider: provider.into(),
            })
            .unwrap(),
        );
    }

    fn write_provider(snap: &mut SettingsSnapshot, name: &str, endpoint: &str, auth: AuthScheme) {
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        snap.insert(
            &oxpath!("config", "gate", "providers", comp),
            to_value(&ProviderConfig {
                dialect: name.to_string(),
                endpoint: endpoint.into(),
                version: String::new(),
                auth: Some(auth),
            })
            .unwrap(),
        );
    }

    fn select(snap: &mut SettingsSnapshot, name: Option<&str>) {
        snap.insert(
            &oxpath!("ui", "settings", "accounts", "selected"),
            to_value(&name.map(|s| s.to_string())).unwrap(),
        );
    }

    #[test]
    fn account_detail_no_selection() {
        let mut snap = SettingsSnapshot::empty();
        let view = render(&mut snap);
        assert_eq!(view, View::text("No account selected. Press Esc to return."));
    }

    #[test]
    fn account_detail_valid() {
        let mut snap = SettingsSnapshot::empty();
        write_provider(
            &mut snap,
            "anthropic",
            "https://api.anthropic.com",
            AuthScheme::XApiKey,
        );
        write_account(&mut snap, "alpha", "anthropic");
        select(&mut snap, Some("alpha"));

        let view = render(&mut snap);

        // Verify shape: a vertical Stack with Form + StatusBlock children.
        let (form, status) = match view {
            View::Stack { dir, children } => {
                assert_eq!(dir, Direction::Vertical);
                assert_eq!(children.len(), 2);
                (children[0].0.clone(), children[1].0.clone())
            }
            other => panic!("expected Stack, got {other:?}"),
        };

        // Form: 5 rows in spec order, focused on Name (default).
        let (rows, focused) = match form {
            View::Form { rows, focused, .. } => (rows, focused),
            other => panic!("expected Form, got {other:?}"),
        };
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].label, "Name");
        assert_eq!(rows[0].value, FormValue::ReadOnly("alpha".into()));
        assert_eq!(rows[1].label, "Protocol");
        assert_eq!(rows[2].label, "Endpoint");
        match &rows[2].value {
            FormValue::Text { value, masked, .. } => {
                assert_eq!(value, "https://api.anthropic.com");
                assert!(!masked);
            }
            other => panic!("expected Text, got {other:?}"),
        }
        assert_eq!(rows[3].label, "Auth");
        assert_eq!(rows[4].label, "Key");
        // No errors anywhere.
        assert!(rows.iter().all(|r| r.error.is_none()));
        assert_eq!(focused, Some(0));

        // StatusBlock: title "Status", two idle lines, no scroll.
        match status {
            View::StatusBlock {
                title,
                lines,
                scroll_offset,
            } => {
                assert_eq!(title, "Status");
                assert_eq!(scroll_offset, 0);
                assert_eq!(lines.len(), 2);
                assert_eq!(lines[0], StyledLine(vec![Span::plain("Test: idle")]));
                assert_eq!(lines[1], StyledLine(vec![Span::plain("Catalog: idle")]));
            }
            other => panic!("expected StatusBlock, got {other:?}"),
        }
    }

    #[test]
    fn account_detail_with_test_failure() {
        let mut snap = SettingsSnapshot::empty();
        write_provider(
            &mut snap,
            "anthropic",
            "https://api.anthropic.com",
            AuthScheme::XApiKey,
        );
        write_account(&mut snap, "alpha", "anthropic");
        select(&mut snap, Some("alpha"));

        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp, "test_status"),
            to_value(&AccountTestStatus::Failed {
                reason: "401 unauthorized".into(),
                completed_at_ms: 1_000,
            })
            .unwrap(),
        );

        let view = render(&mut snap);
        let status = match view {
            View::Stack { children, .. } => children[1].0.clone(),
            other => panic!("expected Stack, got {other:?}"),
        };
        match status {
            View::StatusBlock { lines, .. } => {
                // First line is the test status with the failure reason.
                let first_text = lines[0]
                    .0
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<String>();
                assert!(
                    first_text.contains("401 unauthorized"),
                    "expected reason in line: {first_text}"
                );
                // Should be styled as an error line (red).
                assert_eq!(lines[0].0[0].style.fg, Some(Color::Red));
            }
            other => panic!("expected StatusBlock, got {other:?}"),
        }
    }

    #[test]
    fn account_detail_with_validation_errors() {
        let mut snap = SettingsSnapshot::empty();
        write_provider(
            &mut snap,
            "anthropic",
            "https://api.anthropic.com",
            AuthScheme::XApiKey,
        );
        write_account(&mut snap, "alpha", "anthropic");
        select(&mut snap, Some("alpha"));

        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        let mut errors = BTreeMap::new();
        errors.insert(AccountField::Endpoint, "invalid URL".to_string());
        errors.insert(AccountField::Key, "required".to_string());
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp, "validation_status"),
            to_value(&ValidationDiagnostics {
                field_errors: errors,
                computed_at_ms: 1,
            })
            .unwrap(),
        );

        let view = render(&mut snap);
        let rows = match view {
            View::Stack { children, .. } => match &children[0].0 {
                View::Form { rows, .. } => rows.clone(),
                other => panic!("expected Form, got {other:?}"),
            },
            other => panic!("expected Stack, got {other:?}"),
        };
        // Errors flow through to the matching rows.
        assert_eq!(rows[2].error.as_deref(), Some("invalid URL")); // Endpoint
        assert_eq!(rows[4].error.as_deref(), Some("required")); // Key
        // Other rows untouched.
        assert!(rows[0].error.is_none());
        assert!(rows[1].error.is_none());
        assert!(rows[3].error.is_none());
    }

    #[test]
    fn account_detail_during_test() {
        let mut snap = SettingsSnapshot::empty();
        write_provider(
            &mut snap,
            "anthropic",
            "https://api.anthropic.com",
            AuthScheme::XApiKey,
        );
        write_account(&mut snap, "alpha", "anthropic");
        select(&mut snap, Some("alpha"));

        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp, "test_status"),
            to_value(&AccountTestStatus::Testing {
                started_at_ms: 1_000,
            })
            .unwrap(),
        );

        let view = render(&mut snap);
        let status = match view {
            View::Stack { children, .. } => children[1].0.clone(),
            other => panic!("expected Stack, got {other:?}"),
        };
        match status {
            View::StatusBlock { lines, .. } => {
                let first_text = lines[0]
                    .0
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<String>();
                assert!(
                    first_text.contains("Testing"),
                    "expected Testing in line: {first_text}"
                );
            }
            other => panic!("expected StatusBlock, got {other:?}"),
        }
    }

    #[test]
    fn ascend_rule_is_nearest_registered() {
        assert_eq!(
            AccountDetailRenderer.ascend_to(),
            AscendRule::NearestRegistered
        );
    }
}

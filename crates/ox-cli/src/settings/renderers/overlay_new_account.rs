//! New-account overlay (`settings/accounts/_new`).
//!
//! Per spec §6.4: a `View::Modal { background, foreground, dim: true }`.
//! Background is the accounts-list View — composed by recursing into the
//! registry — so the user sees the page they came from, dimmed, behind
//! the input prompt. Foreground is a single-row Form: the draft name
//! input read from `ui/settings/new_account/name_input: String`.

use ox_path::oxpath;
use ox_view::{FormRow, FormValue, View};

use crate::settings::registry::{AscendRule, RenderCtx, Renderer, RendererRegistry};

use super::util::read_typed;

pub struct OverlayNewAccountRenderer;

impl Renderer for OverlayNewAccountRenderer {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View {
        let bg = ctx.registry.render(&oxpath!("settings", "index"), ctx);

        let name_input: String = read_typed(
            ctx.data,
            &oxpath!("ui", "settings", "new_account", "name_input"),
        )
        .unwrap_or_default();

        let cursor: u32 = name_input.chars().count() as u32;

        let fg = View::Form {
            title: Some("New account".into()),
            rows: vec![FormRow {
                label: "Name".into(),
                value: FormValue::Text {
                    value: name_input,
                    cursor,
                    masked: false,
                },
                error: None,
                hint: Some("Enter to create. Esc to cancel.".into()),
            }],
            focused: Some(0),
        };

        View::Modal {
            background: Box::new(bg),
            foreground: Box::new(fg),
            dim: true,
        }
    }

    fn ascend_to(&self) -> AscendRule {
        AscendRule::NearestRegistered
    }
}

pub fn register(reg: &mut RendererRegistry) {
    reg.register(
        oxpath!("settings", "accounts", "_new"),
        Box::new(OverlayNewAccountRenderer),
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

    /// Build a registry that includes the accounts list, so the overlay's
    /// recursive `ctx.registry.render(settings/accounts, …)` resolves to the
    /// real renderer (not the fallback). Tests still focus on the overlay's
    /// shape; the background's exact content is not asserted byte-for-byte
    /// because that would couple every overlay test to the accounts-list
    /// implementation.
    fn render(snap: &mut SettingsSnapshot) -> View {
        let theme = Theme::default();
        let mut registry = RendererRegistry::new();
        crate::settings::renderers::index::register(&mut registry);
        let mut ctx = RenderCtx {
            area: Rect::new(0, 0, 80, 24),
            data: snap,
            registry: &registry,
            theme: &theme,
        };
        OverlayNewAccountRenderer.render(&mut ctx)
    }

    fn assert_modal_with_name(view: &View, name: &str) {
        match view {
            View::Modal {
                foreground, dim, ..
            } => {
                assert!(*dim, "overlay must dim the background");
                match foreground.as_ref() {
                    View::Form {
                        title,
                        rows,
                        focused,
                    } => {
                        assert_eq!(title.as_deref(), Some("New account"));
                        assert_eq!(*focused, Some(0));
                        assert_eq!(rows.len(), 1);
                        assert_eq!(rows[0].label, "Name");
                        match &rows[0].value {
                            FormValue::Text { value, masked, .. } => {
                                assert_eq!(value, name);
                                assert!(!masked);
                            }
                            other => panic!("expected Text FormValue, got {other:?}"),
                        }
                    }
                    other => panic!("expected Form foreground, got {other:?}"),
                }
            }
            other => panic!("expected Modal, got {other:?}"),
        }
    }

    #[test]
    fn overlay_new_account_empty_input() {
        let mut snap = SettingsSnapshot::empty();
        let view = render(&mut snap);
        assert_modal_with_name(&view, "");
    }

    #[test]
    fn overlay_new_account_partial_name() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "name_input"),
            to_value(&"per".to_string()).unwrap(),
        );
        let view = render(&mut snap);
        assert_modal_with_name(&view, "per");
    }

    #[test]
    fn overlay_new_account_valid_name() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "name_input"),
            to_value(&"personal".to_string()).unwrap(),
        );
        let view = render(&mut snap);
        assert_modal_with_name(&view, "personal");
    }

    #[test]
    fn ascend_rule_is_nearest_registered() {
        assert_eq!(
            OverlayNewAccountRenderer.ascend_to(),
            AscendRule::NearestRegistered
        );
    }
}

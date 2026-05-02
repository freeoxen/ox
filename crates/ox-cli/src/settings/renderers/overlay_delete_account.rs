//! Delete-account overlay (`settings/accounts/_delete`).
//!
//! Per spec §6.5: a `View::Modal { background, foreground, dim: true }`.
//! Background is the accounts-list View — composed by recursing into the
//! registry. Foreground is a confirm box prompting `y`/`n`; when no
//! account is selected the foreground prompts the user to dismiss.

use ox_path::oxpath;
use ox_view::View;

use crate::settings::registry::{AscendRule, RenderCtx, Renderer, RendererRegistry};

use super::util::read_typed;

pub struct OverlayDeleteAccountRenderer;

impl Renderer for OverlayDeleteAccountRenderer {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View {
        let bg = ctx.registry.render(&oxpath!("settings", "accounts"), ctx);

        let selected: Option<String> = read_typed::<Option<String>>(
            ctx.data,
            &oxpath!("ui", "settings", "accounts", "selected"),
        )
        .flatten();

        let fg = match selected {
            Some(name) => View::text(format!("Delete account '{}'? (y/n)", name)),
            None => View::text("Nothing selected. Press Esc."),
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
        oxpath!("settings", "accounts", "_delete"),
        Box::new(OverlayDeleteAccountRenderer),
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
        let mut registry = RendererRegistry::new();
        crate::settings::renderers::accounts_list::register(&mut registry);
        let mut ctx = RenderCtx {
            area: Rect::new(0, 0, 80, 24),
            data: snap,
            registry: &registry,
            theme: &theme,
        };
        OverlayDeleteAccountRenderer.render(&mut ctx)
    }

    #[test]
    fn overlay_delete_with_selection() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "accounts", "selected"),
            to_value(&Some("alpha".to_string())).unwrap(),
        );
        let view = render(&mut snap);
        match view {
            View::Modal {
                foreground, dim, ..
            } => {
                assert!(dim);
                assert_eq!(*foreground, View::text("Delete account 'alpha'? (y/n)"));
            }
            other => panic!("expected Modal, got {other:?}"),
        }
    }

    #[test]
    fn overlay_delete_no_selection() {
        let mut snap = SettingsSnapshot::empty();
        let view = render(&mut snap);
        match view {
            View::Modal {
                foreground, dim, ..
            } => {
                assert!(dim);
                assert_eq!(*foreground, View::text("Nothing selected. Press Esc."));
            }
            other => panic!("expected Modal, got {other:?}"),
        }
    }

    #[test]
    fn ascend_rule_is_nearest_registered() {
        assert_eq!(
            OverlayDeleteAccountRenderer.ascend_to(),
            AscendRule::NearestRegistered
        );
    }
}

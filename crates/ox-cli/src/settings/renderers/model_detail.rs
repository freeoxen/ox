//! Model detail page (`settings/models/_detail`).
//!
//! Per spec §6.7: a `View::Form` showing the selected model's identity
//! and the two overridable token-window fields, each tagged with the
//! source tier that produced it.
//!
//! Rows (in display order):
//! - `id`               — `FormValue::ReadOnly`
//! - `display_name`     — `FormValue::ReadOnly`
//! - `max_context_size` — text + source tag in the row's `hint` (Server /
//!   KnownTable / UserOverride). Editable.
//! - `max_output_tokens` — same shape as above.
//!
//! `Form.focused` is the index of the row matching
//! `ui/settings/model_detail/field`. The fixed-row indices are:
//! `ContextSizeOverride → 2`, `OutputTokensOverride → 3`.
//!
//! Empty-state (selection is `None` or names a vanished model) returns
//! a single `View::Text` line.

use ox_path::oxpath;
use ox_view::{FormRow, FormValue, View};

use ox_gate::{ModelInfo, ModelInfoSource};
use ox_types::{ModelField, ModelKey};

use crate::settings::registry::{AscendRule, RenderCtx, Renderer, RendererRegistry};

use super::util::read_typed;

pub struct ModelDetailRenderer;

impl Renderer for ModelDetailRenderer {
    fn render(&self, ctx: &mut RenderCtx<'_>) -> View {
        let key = match read_typed::<Option<ModelKey>>(
            ctx.data,
            &oxpath!("ui", "settings", "models", "selected"),
        )
        .flatten()
        {
            Some(k) => k,
            None => return View::text("No model selected. Press Esc to return."),
        };

        let acct_comp = match ox_kernel::PathComponent::try_new(&key.account) {
            Ok(c) => c,
            Err(_) => return View::text("No model selected. Press Esc to return."),
        };

        let models: Vec<ModelInfo> = read_typed(
            ctx.data,
            &oxpath!("config", "gate", "accounts", acct_comp, "models"),
        )
        .unwrap_or_default();

        let model = match models.into_iter().find(|m| m.id == key.model_id) {
            Some(m) => m,
            None => {
                return View::text(format!(
                    "Model '{}' was removed. Press Esc to return.",
                    key.model_id
                ));
            }
        };

        let focused_field: Option<ModelField> =
            read_typed(ctx.data, &oxpath!("ui", "settings", "model_detail", "field"));
        let focused = focused_field.map(|f| match f {
            ModelField::ContextSizeOverride => 2,
            ModelField::OutputTokensOverride => 3,
        });

        let source_tag = source_tag(&model.source);

        let rows = vec![
            FormRow {
                label: "id".into(),
                value: FormValue::ReadOnly(model.id.clone()),
                error: None,
                hint: None,
            },
            FormRow {
                label: "display_name".into(),
                value: FormValue::ReadOnly(model.display_name.clone()),
                error: None,
                hint: None,
            },
            FormRow {
                label: "max_context_size".into(),
                value: FormValue::Text {
                    value: render_token_field(model.max_context_size),
                    cursor: 0,
                    masked: false,
                },
                error: None,
                hint: Some(format!("[{source_tag}]")),
            },
            FormRow {
                label: "max_output_tokens".into(),
                value: FormValue::Text {
                    value: render_token_field(model.max_output_tokens),
                    cursor: 0,
                    masked: false,
                },
                error: None,
                hint: Some(format!("[{source_tag}]")),
            },
        ];

        View::Form {
            title: Some(format!("Model: {} / {}", key.account, model.id)),
            rows,
            focused,
        }
    }

    fn ascend_to(&self) -> AscendRule {
        AscendRule::NearestRegistered
    }
}

fn source_tag(source: &ModelInfoSource) -> &'static str {
    match source {
        ModelInfoSource::Server => "server",
        ModelInfoSource::KnownTable => "known",
        ModelInfoSource::UserOverride => "override",
    }
}

fn render_token_field(v: Option<u32>) -> String {
    match v {
        Some(n) => n.to_string(),
        None => "—".to_string(),
    }
}

pub fn register(reg: &mut RendererRegistry) {
    reg.register(
        oxpath!("settings", "models", "_detail"),
        Box::new(ModelDetailRenderer),
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
        ModelDetailRenderer.render(&mut ctx)
    }

    fn write_models(snap: &mut SettingsSnapshot, account: &str, models: Vec<ModelInfo>) {
        let comp = ox_kernel::PathComponent::try_new(account).unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp, "models"),
            to_value(&models).unwrap(),
        );
    }

    fn select(snap: &mut SettingsSnapshot, account: &str, model_id: &str) {
        snap.insert(
            &oxpath!("ui", "settings", "models", "selected"),
            to_value(&Some(ModelKey {
                account: account.into(),
                model_id: model_id.into(),
            }))
            .unwrap(),
        );
    }

    #[test]
    fn model_detail_no_selection() {
        let mut snap = SettingsSnapshot::empty();
        let view = render(&mut snap);
        assert_eq!(view, View::text("No model selected. Press Esc to return."));
    }

    #[test]
    fn model_detail_server_source() {
        let mut snap = SettingsSnapshot::empty();
        write_models(
            &mut snap,
            "alpha",
            vec![ModelInfo {
                id: "m1".into(),
                display_name: "Model One".into(),
                max_context_size: Some(200_000),
                max_output_tokens: Some(8192),
                source: ModelInfoSource::Server,
            }],
        );
        select(&mut snap, "alpha", "m1");

        let view = render(&mut snap);
        let rows = match view {
            View::Form { rows, .. } => rows,
            other => panic!("expected Form, got {other:?}"),
        };
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].value, FormValue::ReadOnly("m1".into()));
        assert_eq!(rows[1].value, FormValue::ReadOnly("Model One".into()));
        match &rows[2].value {
            FormValue::Text { value, .. } => assert_eq!(value, "200000"),
            other => panic!("expected Text, got {other:?}"),
        }
        assert_eq!(rows[2].hint.as_deref(), Some("[server]"));
        match &rows[3].value {
            FormValue::Text { value, .. } => assert_eq!(value, "8192"),
            other => panic!("expected Text, got {other:?}"),
        }
        assert_eq!(rows[3].hint.as_deref(), Some("[server]"));
    }

    #[test]
    fn model_detail_known_table_source() {
        let mut snap = SettingsSnapshot::empty();
        write_models(
            &mut snap,
            "alpha",
            vec![ModelInfo {
                id: "m1".into(),
                display_name: "Model One".into(),
                max_context_size: Some(100_000),
                max_output_tokens: Some(4096),
                source: ModelInfoSource::KnownTable,
            }],
        );
        select(&mut snap, "alpha", "m1");

        let view = render(&mut snap);
        let rows = match view {
            View::Form { rows, .. } => rows,
            other => panic!("expected Form, got {other:?}"),
        };
        assert_eq!(rows[2].hint.as_deref(), Some("[known]"));
        assert_eq!(rows[3].hint.as_deref(), Some("[known]"));
    }

    #[test]
    fn model_detail_user_override() {
        let mut snap = SettingsSnapshot::empty();
        write_models(
            &mut snap,
            "alpha",
            vec![ModelInfo {
                id: "m1".into(),
                display_name: "Model One".into(),
                max_context_size: Some(50_000),
                max_output_tokens: Some(2048),
                source: ModelInfoSource::UserOverride,
            }],
        );
        select(&mut snap, "alpha", "m1");
        snap.insert(
            &oxpath!("ui", "settings", "model_detail", "field"),
            to_value(&ModelField::ContextSizeOverride).unwrap(),
        );

        let view = render(&mut snap);
        let (rows, focused) = match view {
            View::Form { rows, focused, .. } => (rows, focused),
            other => panic!("expected Form, got {other:?}"),
        };
        assert_eq!(rows[2].hint.as_deref(), Some("[override]"));
        assert_eq!(rows[3].hint.as_deref(), Some("[override]"));
        // ContextSizeOverride focuses row 2.
        assert_eq!(focused, Some(2));
    }

    #[test]
    fn model_detail_unknown_fields() {
        // A model where token fields are None — renderer surfaces "—".
        let mut snap = SettingsSnapshot::empty();
        write_models(
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
        select(&mut snap, "alpha", "m1");

        let view = render(&mut snap);
        let rows = match view {
            View::Form { rows, .. } => rows,
            other => panic!("expected Form, got {other:?}"),
        };
        match &rows[2].value {
            FormValue::Text { value, .. } => assert_eq!(value, "—"),
            other => panic!("expected Text, got {other:?}"),
        }
        match &rows[3].value {
            FormValue::Text { value, .. } => assert_eq!(value, "—"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn ascend_rule_is_nearest_registered() {
        assert_eq!(ModelDetailRenderer.ascend_to(), AscendRule::NearestRegistered);
    }
}

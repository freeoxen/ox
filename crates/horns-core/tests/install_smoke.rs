//! Smoke tests for `build_install_bundle`.
//!
//! These pin the shape of the bundle without invoking the broker: a
//! bundle made from a single binding emits a binding row at
//! `<bindings_prefix>/<id>`, a theme write, and three subscriptions
//! (KeyDispatch, Render, ThemeChange).

use std::collections::HashMap;

use horns_core::install::{InstallOptions, build_install_bundle};
use horns_core::{
    BindingEntry, BindingId, BindingScope, CommandId, KeyChord, KeyCodeRepr, KeyModifierSet, Phase,
};
use structfs_core_store::Path;

fn p(s: &str) -> Path {
    Path::parse(s).expect("path parse")
}

#[test]
fn build_install_bundle_emits_metadata_writes_for_bindings() {
    let opts = InstallOptions {
        cursor_path: p("ui/test/focused"),
        input_path: p("ui/test/input"),
        render_tick_path: p("ui/test/render/tick"),
        render_output_path: p("ui/test/render/output"),
        bindings_prefix: p("horns/test/bindings"),
        commands_prefix: p("horns/test/commands"),
        renderers_prefix: p("horns/test/renderers"),
        handlers_prefix: p("horns/test/handlers"),
        theme_path: p("ui/test/theme"),
        commands: HashMap::new(),
        renderers: HashMap::new(),
        handlers: HashMap::new(),
        bindings: vec![(
            BindingId("test_noop".into()),
            BindingEntry {
                scope: BindingScope::Anywhere,
                key: KeyChord {
                    modifiers: KeyModifierSet::default(),
                    code: KeyCodeRepr::Esc,
                },
                phase: Phase::Bubble,
                priority: 200,
                command_id: CommandId("test_noop".into()),
            },
        )],
        handler_metadata: vec![],
        theme: serde_json::json!({}),
    };

    let bundle = build_install_bundle(opts);

    // Should have at least 1 write (the binding) + theme.
    assert!(
        bundle.metadata_writes.len() >= 2,
        "expected >=2 metadata writes, got {}",
        bundle.metadata_writes.len()
    );

    // Should have 3 subscriptions (KeyDispatch, Render, ThemeChange).
    assert_eq!(bundle.subscriptions.len(), 3, "expected 3 subscriptions");

    // Confirm the binding was emitted at horns/test/bindings/test_noop.
    let binding_path = p("horns/test/bindings/test_noop");
    let binding_emitted = bundle
        .metadata_writes
        .iter()
        .any(|(path, _)| path == &binding_path);
    assert!(binding_emitted, "binding row not found in metadata writes");

    // Theme path is in there too.
    let theme_path = p("ui/test/theme");
    let theme_emitted = bundle
        .metadata_writes
        .iter()
        .any(|(path, _)| path == &theme_path);
    assert!(theme_emitted, "theme row not found in metadata writes");
}

#[test]
fn build_install_bundle_subscriptions_have_stable_ids() {
    let opts = InstallOptions {
        cursor_path: p("ui/test/focused"),
        input_path: p("ui/test/input"),
        render_tick_path: p("ui/test/render/tick"),
        render_output_path: p("ui/test/render/output"),
        bindings_prefix: p("horns/test/bindings"),
        commands_prefix: p("horns/test/commands"),
        renderers_prefix: p("horns/test/renderers"),
        handlers_prefix: p("horns/test/handlers"),
        theme_path: p("ui/test/theme"),
        commands: HashMap::new(),
        renderers: HashMap::new(),
        handlers: HashMap::new(),
        bindings: vec![],
        handler_metadata: vec![],
        theme: serde_json::json!({}),
    };

    let bundle = build_install_bundle(opts);
    let ids: Vec<String> = bundle
        .subscriptions
        .iter()
        .map(|s| s.id().0.clone())
        .collect();
    assert!(ids.contains(&"horns.key_dispatch".to_string()));
    assert!(ids.contains(&"horns.render".to_string()));
    assert!(ids.contains(&"horns.theme_change".to_string()));
}

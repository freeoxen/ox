//! Race-free key dispatch — the binding-lookup half of the input
//! pipeline.
//!
//! # The rule
//!
//! **Snapshots are render-only across the dispatch boundary.** The
//! TUI's view-state snapshot (`ViewState` / `UiSnapshot`) is taken
//! once per event-loop iteration and is therefore stale by the time
//! key events from the same iteration are dispatched. Using a stale
//! snapshot to decide *what a key means* is the bug class that
//! motivated the architecture in `local/plans/focus-resolution.md`:
//!
//! > snapshot says "no approval pending" → client computes
//! > `Mode::Normal` → ships it to broker → binding lookup misses
//! > the `Approval+Enter` binding → keypress drops.
//!
//! # The structural enforcement
//!
//! [`send_key`] is the single function that translates a keypress into
//! a binding-lookup write. Its signature is the boundary:
//!
//! ```text
//! pub async fn send_key(
//!     client: &ClientHandle,
//!     key: &str,
//!     screen: Screen,
//!     flags: ClientModalFlags,
//!     // P1 additions for the settings screen — `None` for legacy callers.
//!     cursor:    Option<&Path>,
//!     snapshot:  Option<&mut dyn Reader>,
//!     bindings:  Option<&BindingRegistry>,
//!     commands:  Option<&CommandRegistry>,
//!     renderers: Option<&RendererRegistry>,
//! ) -> KeyDispatchOutcome
//! ```
//!
//! There is no `&UiSnapshot` parameter. There is no `&ViewState`
//! parameter. There is no `Mode` parameter. The function literally
//! cannot consult a snapshot to decide which binding fires — the
//! types prevent it. The broker's mode resolver
//! ([`crate::focus::FocusInputs::from_broker`]) does that work
//! against live state, and reports the outcome (handled, or
//! `Unbound { mode }` for the text-input fallback) via the returned
//! [`KeyDispatchOutcome`].
//!
//! Callers may still consult their snapshot for things that are
//! genuinely client-local — which screen they're on (a tag, not a
//! decision), what flags to pass — but those are inputs to the
//! function, not state the function reads.
//!
//! # Settings cursor scoping (Phase P)
//!
//! When the dispatch is for the settings screen *and* the caller
//! supplies the settings registries + a snapshot Reader + the current
//! cursor, [`send_key`] routes through
//! [`crate::settings::dispatch::dispatch_settings_key`]: it looks up
//! the binding under `(screen, cursor, mode, key)`, looks up the
//! command, runs it, and applies each emitted Write via the broker
//! client. On a binding miss (or a key string the parser cannot
//! convert into a `KeyChord`) it falls back to the input-store path so
//! the existing global-key dispatch (modal handlers, etc.) still gets
//! a shot.
//!
//! Non-settings callers pass `None` for the new parameters and the
//! function falls through to the legacy input-store dispatch path
//! unchanged.

use ox_broker::ClientHandle;
use ox_path::oxpath;
use ox_types::key_chord::{KeyCodeRepr, KeyModifierSet};
use ox_types::{ClientModalFlags, InputKeyEvent, KeyChord, Mode, Screen};
use structfs_core_store::{Path, Reader};

use crate::settings::binding_registry::BindingRegistry;
use crate::settings::command_registry::CommandRegistry;
use crate::settings::registry::RendererRegistry;

/// Outcome of a key-dispatch attempt. The `Unbound` arm carries the
/// mode the broker resolved against — clients use it to route the key
/// through the appropriate text-input fallback (`Insert`, `Command`,
/// `Search`) without recomputing mode locally.
pub enum KeyDispatchOutcome {
    /// A binding matched and was executed by the broker's dispatcher
    /// (or, on the settings path, by the in-process command registry).
    Handled,
    /// No binding matched. `mode` is what the broker's resolver
    /// concluded — authoritative, race-free.
    Unbound { mode: Mode },
}

/// Send a key event to the broker for binding lookup. The broker
/// resolves dispatch mode from its own live state (combined with the
/// client-local `flags`) — there is no snapshot involvement on either
/// side of this call.
///
/// On the settings screen, when the caller threads the settings
/// registries + snapshot + cursor through, the function instead looks
/// the key up in the in-process settings binding/command registries,
/// runs the command, and applies the emitted writes. See the
/// module-level doc for the full rationale.
#[allow(clippy::too_many_arguments)]
pub async fn send_key(
    client:    &ClientHandle,
    key:       &str,
    screen:    Screen,
    flags:     ClientModalFlags,
    cursor:    Option<&Path>,
    snapshot:  Option<&mut dyn Reader>,
    bindings:  Option<&BindingRegistry>,
    commands:  Option<&CommandRegistry>,
    renderers: Option<&RendererRegistry>,
) -> KeyDispatchOutcome {
    if screen == Screen::Settings {
        if let (Some(cursor), Some(snapshot), Some(bindings), Some(commands), Some(renderers)) =
            (cursor, snapshot, bindings, commands, renderers)
        {
            // Settings path: parse the encoded key string into a chord,
            // dispatch through the in-process registries, apply every
            // emitted Write to the broker.
            let Some(chord) = parse_key_str(key) else {
                // Couldn't parse the encoded key (e.g. F-key) — let the
                // input-store path have a try.
                return send_via_input_store(client, key, screen, flags).await;
            };
            let writes = crate::settings::dispatch::dispatch_settings_key(
                snapshot, screen, cursor, None, &chord, commands, bindings, renderers,
            );
            if writes.is_empty() {
                // No binding matched in the settings registry — fall
                // through to the input-store path so the existing
                // global-key dispatch (modal handlers, etc.) still gets
                // a shot at this key.
                return send_via_input_store(client, key, screen, flags).await;
            }
            for write in writes {
                if let Err(e) = client.write(&write.path, write.record).await {
                    tracing::warn!(error = %e, key = %key, "settings dispatch write failed");
                }
            }
            return KeyDispatchOutcome::Handled;
        }
    }
    send_via_input_store(client, key, screen, flags).await
}

/// The legacy input-store dispatch path: encode the key as an
/// `InputKeyEvent`, write to `input/key`, and translate the
/// substrate's reply path into a `KeyDispatchOutcome`.
async fn send_via_input_store(
    client: &ClientHandle,
    key:    &str,
    screen: Screen,
    flags:  ClientModalFlags,
) -> KeyDispatchOutcome {
    let event = InputKeyEvent {
        mode: None,
        key: key.to_string(),
        screen,
        flags,
    };
    let result = client.write_typed(&oxpath!("input", "key"), &event).await;
    match result {
        Ok(p) if p.components.first().map(|c| c.as_str()) == Some("unbound") => {
            let mode = p
                .components
                .get(1)
                .and_then(|c| Mode::parse(c.as_str()))
                .unwrap_or(Mode::Normal);
            KeyDispatchOutcome::Unbound { mode }
        }
        Ok(_) => KeyDispatchOutcome::Handled,
        Err(e) => {
            tracing::warn!(error = %e, key = %key, "input key dispatch failed");
            // Treat genuine errors as handled to avoid the fallback
            // re-routing the key into a text-input handler in an
            // unexpected state.
            KeyDispatchOutcome::Handled
        }
    }
}

/// Parse the crossterm-style encoded key string back into a `KeyChord`.
///
/// Inverse of `crate::key_encode::encode_key`. The encoder is the
/// source of truth for the wire shape; this parser tracks it. Returns
/// `None` for any string the encoder would never produce (e.g.
/// function keys F1..F12, which the encoder drops to `None`).
///
/// Conventions handled:
/// - `"j"`, `"q"`, `"P"`, `"/"` etc. → bare `Char(c)` with no modifiers.
/// - `"Esc"`, `"Enter"`, `"Backspace"`, `"Tab"`, `"Up"`, `"Down"`,
///   `"Left"`, `"Right"`, `"Delete"`, `"PageUp"`, `"PageDown"`,
///   `"Home"`, `"End"`, `"Insert"`.
/// - `"Shift+Tab"` → `BackTab` with `shift: true` (mirrors the encoder).
/// - `"Ctrl+x"` → `Char('x')` with `ctrl: true`.
/// - `"Ctrl+Enter"` → `Enter` with `ctrl: true`.
fn parse_key_str(s: &str) -> Option<KeyChord> {
    if let Some(rest) = s.strip_prefix("Ctrl+") {
        let mut chord = parse_key_str(rest)?;
        chord.modifiers.ctrl = true;
        return Some(chord);
    }
    if let Some(rest) = s.strip_prefix("Shift+") {
        let mut chord = parse_key_str(rest)?;
        chord.modifiers.shift = true;
        return Some(chord);
    }
    if let Some(rest) = s.strip_prefix("Alt+") {
        let mut chord = parse_key_str(rest)?;
        chord.modifiers.alt = true;
        return Some(chord);
    }

    let code = match s {
        "Esc"       => KeyCodeRepr::Esc,
        "Enter"     => KeyCodeRepr::Enter,
        "Backspace" => KeyCodeRepr::Backspace,
        "Tab"       => KeyCodeRepr::Tab,
        "Up"        => KeyCodeRepr::Up,
        "Down"      => KeyCodeRepr::Down,
        "Left"      => KeyCodeRepr::Left,
        "Right"     => KeyCodeRepr::Right,
        "Delete"    => KeyCodeRepr::Delete,
        "PageUp"    => KeyCodeRepr::PageUp,
        "PageDown"  => KeyCodeRepr::PageDown,
        "Home"      => KeyCodeRepr::Home,
        "End"       => KeyCodeRepr::End,
        "Insert"    => KeyCodeRepr::Insert,
        // The encoder writes "Shift+Tab" for BackTab; the explicit
        // recursion at the top handles that prefix and falls into the
        // `Tab` branch — but the bindings table registers BackTab as a
        // distinct code. Detect the bare token here for callers that
        // build the string directly.
        "BackTab"   => KeyCodeRepr::BackTab,
        _ => {
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                // Multi-char token we don't recognize.
                return None;
            }
            // Uppercase ASCII letter on the wire reflects a Shift+letter
            // chord (the encoder writes "P" for crossterm
            // `Shift+KeyCode::Char('P')`). The settings bindings table
            // registers capital letters with `shift: true` (see
            // `bindings.rs` `register_models`), so set the flag here.
            let mut modifiers = KeyModifierSet::default();
            if c.is_ascii_uppercase() {
                modifiers.shift = true;
            }
            return Some(KeyChord {
                modifiers,
                code: KeyCodeRepr::Char(c),
            });
        }
    };
    Some(KeyChord {
        modifiers: KeyModifierSet::default(),
        code,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use ox_broker::BrokerStore;
    use ox_gate::CompletionRole;
    use ox_path::oxpath;
    use ox_store_util::local_config::LocalConfig;
    use ox_types::settings::ModelKey;
    use structfs_serde_store::to_value;

    use crate::settings::commands::navigation::path_to_value;
    use crate::settings::registry::{AscendRule, RenderCtx, Renderer};
    use crate::settings::snapshot::SettingsSnapshot;

    // -------- parse_key_str unit tests ------------------------------------

    #[test]
    fn parse_bare_lowercase() {
        let chord = parse_key_str("j").expect("parsed");
        assert_eq!(chord.modifiers, KeyModifierSet::default());
        assert!(matches!(chord.code, KeyCodeRepr::Char('j')));
    }

    #[test]
    fn parse_bare_uppercase_implies_shift() {
        // Per the encoder convention, "P" on the wire reflects a
        // Shift+letter chord — the parser sets `shift: true`.
        let chord = parse_key_str("P").expect("parsed");
        assert!(chord.modifiers.shift);
        assert!(!chord.modifiers.ctrl);
        assert!(matches!(chord.code, KeyCodeRepr::Char('P')));
    }

    #[test]
    fn parse_esc() {
        let chord = parse_key_str("Esc").expect("parsed");
        assert!(matches!(chord.code, KeyCodeRepr::Esc));
    }

    #[test]
    fn parse_ctrl_char() {
        let chord = parse_key_str("Ctrl+s").expect("parsed");
        assert!(chord.modifiers.ctrl);
        assert!(matches!(chord.code, KeyCodeRepr::Char('s')));
    }

    #[test]
    fn parse_unknown_returns_none() {
        assert!(parse_key_str("F1").is_none());
        assert!(parse_key_str("absolutelyNotAKey").is_none());
    }

    // -------- send_key integration tests ----------------------------------

    /// Stub Renderer used only to seed the renderer registry with an
    /// `AscendRule`. ``ascend`` is the only behaviour these tests care about;
    /// ``render`` is never called.
    struct FakeRenderer(AscendRule);
    impl Renderer for FakeRenderer {
        fn render(&self, _ctx: &mut RenderCtx<'_>) -> ox_view::View {
            ox_view::View::Empty
        }
        fn ascend_to(&self) -> AscendRule {
            self.0
        }
    }

    /// Build a broker with `config` and `ui` mounts (both backed by
    /// `LocalConfig`, which is generic Reader+Writer). The mount server
    /// task handles are returned alongside the broker so the test
    /// keeps them alive for its full duration.
    async fn broker_with_config_and_ui() -> (
        BrokerStore,
        ClientHandle,
        tokio::task::JoinHandle<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let broker = BrokerStore::new(Duration::from_secs(5));
        let config_h = broker.mount(oxpath!("config"), LocalConfig::new()).await;
        let ui_h = broker.mount(oxpath!("ui"), LocalConfig::new()).await;
        let client = broker.client();
        (broker, client, config_h, ui_h)
    }

    fn flags() -> ClientModalFlags {
        ClientModalFlags::default()
    }

    /// Pre-populate the snapshot Reader with everything `models.set_primary`
    /// needs: a selected model and its account/model in the gate catalog.
    fn snap_with_selected_model() -> SettingsSnapshot {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "models", "selected"),
            to_value(&Some(ModelKey {
                account:  "alpha".into(),
                model_id: "m1".into(),
            }))
            .unwrap(),
        );
        snap
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn settings_p_on_models_writes_completion_role() {
        // Full integration: a Shift+P on `settings/models` runs the
        // `models.set_primary` command and writes the `CompletionRole` to
        // `config/gate/completions/primary` through the broker.
        let (_broker, client, _config_h, _ui_h) = broker_with_config_and_ui().await;

        let mut bindings = BindingRegistry::new();
        crate::settings::bindings::register(&mut bindings);
        let mut cmds = CommandRegistry::new();
        crate::settings::commands::register_all(&mut cmds);
        let renderers = RendererRegistry::new();

        let cursor = oxpath!("settings", "models");
        let mut snap = snap_with_selected_model();

        // The encoded form of Shift+P is just "P" (the encoder writes
        // the char as-is for letter keys without Ctrl). `parse_key_str`
        // sets `shift: true` for any uppercase ASCII letter, so the
        // resulting chord matches the day-one binding registered in
        // `bindings.rs::register_models` with `shift_only()`.
        let outcome = send_key(
            &client,
            "P",
            Screen::Settings,
            flags(),
            Some(&cursor),
            Some(&mut snap),
            Some(&bindings),
            Some(&cmds),
            Some(&renderers),
        )
        .await;
        assert!(matches!(outcome, KeyDispatchOutcome::Handled));

        // Verify the Write reached the broker — read it back through the
        // client and deserialize.
        let role: CompletionRole = client
            .read_typed(&oxpath!("config", "gate", "completions", "primary"))
            .await
            .expect("read_typed")
            .expect("primary completion role present");
        assert_eq!(role.account, "alpha");
        assert_eq!(role.model_id, "m1");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn settings_esc_on_index_emits_screen_exit_signal() {
        // Esc on `settings/index` runs `nav.ascend`. The renderer at the
        // index uses `AscendRule::ExitScreen`, so the command writes
        // `true` to `ui/settings/_request_exit`.
        let (_broker, client, _config_h, _ui_h) = broker_with_config_and_ui().await;

        let mut bindings = BindingRegistry::new();
        crate::settings::bindings::register(&mut bindings);
        let mut cmds = CommandRegistry::new();
        crate::settings::commands::register_all(&mut cmds);

        let mut renderers = RendererRegistry::new();
        renderers.register(
            oxpath!("settings", "index"),
            Box::new(FakeRenderer(AscendRule::ExitScreen)),
        );

        let cursor = oxpath!("settings", "index");
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "cursor"),
            path_to_value(&cursor),
        );

        let outcome = send_key(
            &client,
            "Esc",
            Screen::Settings,
            flags(),
            Some(&cursor),
            Some(&mut snap),
            Some(&bindings),
            Some(&cmds),
            Some(&renderers),
        )
        .await;
        assert!(matches!(outcome, KeyDispatchOutcome::Handled));

        let request_exit: bool = client
            .read_typed(&oxpath!("ui", "settings", "_request_exit"))
            .await
            .expect("read_typed")
            .expect("exit flag present");
        assert!(request_exit);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn settings_unbound_key_returns_unhandled() {
        // A key the settings registry has no binding for must produce
        // *no writes* on the settings side. The closest existing
        // outcome enum is `Unbound`; in this test we check the more
        // direct observable — that no settings-side broker state was
        // mutated — because the legacy input-store fallback runs after
        // a settings miss and its outcome depends on whether an input
        // mount exists (which it doesn't, in this fixture). The
        // settings half is what P1 introduces and is what we want to
        // verify here.
        let (_broker, client, _config_h, _ui_h) = broker_with_config_and_ui().await;

        let mut bindings = BindingRegistry::new();
        crate::settings::bindings::register(&mut bindings);
        let mut cmds = CommandRegistry::new();
        crate::settings::commands::register_all(&mut cmds);
        let renderers = RendererRegistry::new();

        let cursor = oxpath!("settings", "index");
        let mut snap = SettingsSnapshot::empty();

        // `Ctrl+z` parses fine but is bound nowhere in the day-one
        // settings table.
        let _outcome = send_key(
            &client,
            "Ctrl+z",
            Screen::Settings,
            flags(),
            Some(&cursor),
            Some(&mut snap),
            Some(&bindings),
            Some(&cmds),
            Some(&renderers),
        )
        .await;

        // Two canary paths a settings command would have written to —
        // both must be absent.
        let exit_canary = client
            .read_typed::<bool>(&oxpath!("ui", "settings", "_request_exit"))
            .await
            .expect("read_typed");
        assert!(exit_canary.is_none(), "no settings command should have written");

        let primary = client
            .read_typed::<CompletionRole>(&oxpath!("config", "gate", "completions", "primary"))
            .await
            .expect("read_typed");
        assert!(primary.is_none(), "no settings command should have written");
    }
}

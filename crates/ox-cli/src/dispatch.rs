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
    client: &ClientHandle,
    key: &str,
    screen: Screen,
    flags: ClientModalFlags,
    cursor: Option<&Path>,
    snapshot: Option<&mut dyn Reader>,
    bindings: Option<&BindingRegistry>,
    commands: Option<&CommandRegistry>,
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
                snapshot, cursor, &chord, commands, bindings, renderers,
            );
            if writes.is_empty() {
                // No binding matched in the settings registry — fall
                // through to the input-store path so the existing
                // global-key dispatch (modal handlers, etc.) still gets
                // a shot at this key.
                return send_via_input_store(client, key, screen, flags).await;
            }
            // A substrate rejection here is always a topology bug — the
            // mount at the write target's prefix doesn't honor
            // state-shaped writes (e.g. a typed-command store sitting
            // where the framework expects a generic key/value store).
            // Log at error level for production visibility and
            // `debug_assert!` so dev builds fail at the seam instead
            // of silently reporting `Handled`.
            let mut first_failure: Option<(Path, String)> = None;
            for write in writes {
                let path = write.path.clone();
                if let Err(e) = client.write(&write.path, write.record).await {
                    if first_failure.is_none() {
                        first_failure = Some((path, e.to_string()));
                    }
                }
            }
            if let Some((path, err)) = first_failure {
                tracing::error!(
                    error = %err, key = %key, path = %path,
                    "settings dispatch: substrate rejected a write — the mount at \
                     this prefix does not accept state-shaped writes; check \
                     broker_setup mount topology"
                );
                debug_assert!(
                    false,
                    "settings dispatch: substrate rejected write to {path}: {err}. \
                     Check that the mount at this prefix accepts arbitrary state \
                     writes (the framework guarantee). UiStore is a typed-command \
                     store and will reject paths it doesn't recognize."
                );
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
    key: &str,
    screen: Screen,
    flags: ClientModalFlags,
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
    // Encoder convention: KeyCode::BackTab → "Shift+Tab" wire string.
    // Bindings register KeyChord { shift: true, code: BackTab }, so we
    // must produce that exact chord rather than Tab+shift.
    if s == "Shift+Tab" {
        return Some(KeyChord {
            modifiers: KeyModifierSet {
                shift: true,
                ..KeyModifierSet::default()
            },
            code: KeyCodeRepr::BackTab,
        });
    }
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
        "Esc" => KeyCodeRepr::Esc,
        "Enter" => KeyCodeRepr::Enter,
        "Backspace" => KeyCodeRepr::Backspace,
        "Tab" => KeyCodeRepr::Tab,
        "Up" => KeyCodeRepr::Up,
        "Down" => KeyCodeRepr::Down,
        "Left" => KeyCodeRepr::Left,
        "Right" => KeyCodeRepr::Right,
        "Delete" => KeyCodeRepr::Delete,
        "PageUp" => KeyCodeRepr::PageUp,
        "PageDown" => KeyCodeRepr::PageDown,
        "Home" => KeyCodeRepr::Home,
        "End" => KeyCodeRepr::End,
        "Insert" => KeyCodeRepr::Insert,
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

    #[test]
    fn parse_shift_tab_yields_back_tab() {
        // Encoder writes "Shift+Tab" for KeyCode::BackTab; bindings
        // register `KeyChord { shift: true, code: BackTab }`. The parser
        // must produce that exact chord — not `Tab` with `shift: true`,
        // which would silently miss the binding.
        let chord = parse_key_str("Shift+Tab").expect("parsed");
        assert!(chord.modifiers.shift);
        assert!(!chord.modifiers.ctrl);
        assert!(!chord.modifiers.alt);
        assert!(matches!(chord.code, KeyCodeRepr::BackTab));
    }

    // -------- encoder/parser round-trip property test --------------------

    /// Property test: every `KeyChord` in the canonical set must round-trip
    /// through `encode_keychord_to_str` → `parse_key_str`. This is the
    /// safety net that would have caught the Shift+Tab bug (`53d3da2`):
    /// the parser produced `Tab + shift` while bindings registered
    /// `BackTab + shift`, so the lookup silently missed.
    ///
    /// Chords the encoder cannot represent (today: `KeyCodeRepr::F(_)`) are
    /// silently skipped — the encoder is the source of truth for what
    /// chords reach the dispatcher; if it returns `None`, no wire form
    /// exists to round-trip. Future encoder extensions automatically pull
    /// those chords into the assertion set.
    #[test]
    fn keychord_encode_parse_roundtrip() {
        use crate::key_chord_canonical::{canonical_chords, encode_keychord_to_str};

        let mut failures: Vec<String> = Vec::new();
        let mut roundtripped = 0usize;
        let mut encoder_skipped = 0usize;
        for chord in canonical_chords() {
            let Some(wire) = encode_keychord_to_str(&chord) else {
                encoder_skipped += 1;
                continue;
            };
            match parse_key_str(&wire) {
                Some(parsed) if parsed == chord => roundtripped += 1,
                Some(parsed) => failures.push(format!(
                    "{chord:?} encoded to {wire:?}, parsed back as {parsed:?} (mismatch)"
                )),
                None => failures.push(format!(
                    "{chord:?} encoded to {wire:?}, parser returned None"
                )),
            }
        }
        assert!(
            failures.is_empty(),
            "round-trip failures ({} encoder gaps tolerated):\n{}",
            encoder_skipped,
            failures.join("\n"),
        );
        assert!(
            roundtripped >= 100,
            "expected ≥100 round-trip-clean chords; got {roundtripped} (encoder skipped {encoder_skipped})"
        );
    }

    /// End-to-end pipeline test: for every day-one binding, dispatch the
    /// original chord *and* the encode→parse round-tripped chord through
    /// `dispatch_settings_key`, then assert the resulting writes match.
    /// If encode/parse drops chord information that bindings depend on,
    /// the two dispatches diverge and this test fails — closing the loop
    /// the unit tests above can't (they only cover one stage at a time).
    #[test]
    fn day_one_bindings_round_trip_through_full_dispatch() {
        use crate::key_chord_canonical::encode_keychord_to_str;
        use crate::settings::binding_registry::BindingRegistry;
        use crate::settings::bindings::register as register_all_bindings;
        use crate::settings::command_registry::CommandRegistry;
        use crate::settings::commands::register_all as register_all_commands;
        use crate::settings::dispatch::dispatch_settings_key;
        use crate::settings::registry::RendererRegistry;

        let mut bindings = BindingRegistry::new();
        register_all_bindings(&mut bindings);
        let mut cmds = CommandRegistry::new();
        register_all_commands(&mut cmds);
        let renderers = RendererRegistry::new();

        let empty_path = oxpath!();
        let entries: Vec<_> = bindings.entries().to_vec();
        let mut tested = 0usize;
        let mut encoder_gaps = 0usize;

        for entry in &entries {
            let Some(wire) = encode_keychord_to_str(&entry.key) else {
                encoder_gaps += 1;
                continue;
            };
            let parsed = parse_key_str(&wire).unwrap_or_else(|| {
                panic!("binding {entry:?} encoded to {wire:?}, parser returned None")
            });

            let cursor = entry.scope.keyed_path().unwrap_or(&empty_path);

            let mut reader_orig = LocalConfig::default();
            let writes_orig = dispatch_settings_key(
                &mut reader_orig,
                cursor,
                &entry.key,
                &cmds,
                &bindings,
                &renderers,
            );

            let mut reader_parsed = LocalConfig::default();
            let writes_parsed = dispatch_settings_key(
                &mut reader_parsed,
                cursor,
                &parsed,
                &cmds,
                &bindings,
                &renderers,
            );

            assert_eq!(
                writes_orig.len(),
                writes_parsed.len(),
                "binding {entry:?}: original chord produced {} writes; parsed chord produced {} (encoded as {wire:?}, parsed back as {parsed:?})",
                writes_orig.len(),
                writes_parsed.len(),
            );
            for (i, (a, b)) in writes_orig.iter().zip(writes_parsed.iter()).enumerate() {
                assert_eq!(
                    a.path, b.path,
                    "binding {entry:?}: write[{i}].path differs between original and round-tripped chord (wire {wire:?})"
                );
            }
            tested += 1;
        }

        assert!(
            tested >= 100,
            "expected ≥100 bindings exercised end-to-end; got {tested} (encoder skipped {encoder_gaps})"
        );
    }

    // -------- send_key integration tests ----------------------------------

    /// Stub Renderer used only to seed the renderer registry with an
    /// `AscendRule`. ``ascend`` is the only behaviour these tests care about;
    /// ``render`` is never called.
    struct FakeRenderer(AscendRule);
    impl Renderer for FakeRenderer {
        fn render(&self, _ctx: &mut RenderCtx<'_>) -> horns_core::view::View {
            horns_core::view::View::Empty
        }
        fn ascend_to(&self) -> AscendRule {
            self.0.clone()
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

    /// Pre-populate the snapshot Reader with everything `models.set_bootstrap`
    /// needs: a selected model and its account/model in the gate catalog.
    fn snap_with_selected_model() -> SettingsSnapshot {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "models", "selected"),
            to_value(&Some(ModelKey {
                account: "alpha".into(),
                model_id: "m1".into(),
            }))
            .unwrap(),
        );
        snap
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn settings_p_on_models_writes_completion_role() {
        // Full integration: a Shift+P on `settings/models` runs the
        // `models.set_bootstrap` command and writes the `CompletionRole` to
        // both `config/gate/completions/bootstrap` (new source of truth) and
        // `config/gate/completions/primary` (legacy, dual-write during the
        // migration window) through the broker.
        let (_broker, client, _config_h, _ui_h) = broker_with_config_and_ui().await;

        let mut bindings = BindingRegistry::new();
        crate::settings::bindings::register(&mut bindings);
        let mut cmds = CommandRegistry::new();
        crate::settings::commands::register_all(&mut cmds);
        let renderers = RendererRegistry::new();

        let cursor = oxpath!("settings", "models");
        let mut snap = snap_with_selected_model();
        // Cursor-as-focus: the dispatcher's `compute_scope_path` reads
        // `ui/settings/focused` to build the scope_path. Seed it at the
        // page cursor so the Bubble walk finds the `Prefix(settings/models)`
        // binding for `P` on `settings/models`'s ancestor chain.
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            crate::settings::commands::navigation::path_to_value(&cursor),
        );

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

        // Verify both writes reached the broker — the new source of
        // truth and the legacy migration path must encode the same role.
        let bootstrap: CompletionRole = client
            .read_typed(&oxpath!("config", "gate", "completions", "bootstrap"))
            .await
            .expect("read_typed")
            .expect("bootstrap completion role present");
        assert_eq!(bootstrap.account, "alpha");
        assert_eq!(bootstrap.model_id, "m1");
        let legacy: CompletionRole = client
            .read_typed(&oxpath!("config", "gate", "completions", "primary"))
            .await
            .expect("read_typed")
            .expect("legacy primary completion role present");
        assert_eq!(legacy.account, "alpha");
        assert_eq!(legacy.model_id, "m1");
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
        snap.insert(&oxpath!("ui", "settings", "cursor"), path_to_value(&cursor));
        // Cursor-as-focus: the dispatcher's `compute_scope_path` reads
        // `ui/settings/focused` to build the scope_path. Seed it at the
        // page cursor so the `Exact(settings/index)` Esc binding is on
        // the ancestor chain.
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
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
        assert!(
            exit_canary.is_none(),
            "no settings command should have written"
        );

        let primary = client
            .read_typed::<CompletionRole>(&oxpath!("config", "gate", "completions", "primary"))
            .await
            .expect("read_typed");
        assert!(primary.is_none(), "no settings command should have written");
    }
}

//! Test-only key-dispatch shim.
//!
//! Pre-Task-10, this module was the in-process binding dispatcher for
//! the settings screen — the event loop called `send_key` synchronously
//! after each keystroke. Task 10 rewires the production path:
//! `event_loop` now writes a `KeyChord` to the install's input path and
//! the horns `KeyDispatchSubscription` (registered by `settings::install`)
//! runs the binding lookup in-broker on the next dispatcher tick.
//!
//! The 1900-line `settings_e2e` integration test suite still drives the
//! pipeline synchronously through `send_key` because rewriting every
//! assertion to wait on subscription writes would be a separate
//! undertaking. This module keeps `send_key` alive as a test-only
//! adapter: same observable behavior as the old wrapper, same
//! signature, but no production callers — `event_loop` does not import
//! this file. The function is `pub` so the integration test (which
//! compiles against the `ox_cli` library crate, not the binary) can
//! reach it.

use horns_core::Dispatcher;
use ox_broker::ClientHandle;
use ox_path::oxpath;
use ox_types::{ClientModalFlags, InputKeyEvent, Mode, Screen};
use structfs_core_store::{Path, Reader};

/// Re-export of `parse_key_str` under the historical module path:
/// callers that reached for `dispatch::parse_key_str` pre-Task-10
/// still resolve. The real implementation lives in
/// `key_chord_canonical`.
pub use crate::key_chord_canonical::parse_key_str;

use crate::settings::BindingRegistry;
use crate::settings::CommandRegistry;
use crate::settings::RendererRegistry;

/// Outcome of a key-dispatch attempt. `Unbound { mode }` carries the
/// mode the legacy input-store resolved against — clients use it to
/// route the key through the appropriate text-input fallback.
pub enum KeyDispatchOutcome {
    Handled,
    Unbound { mode: Mode },
}

/// Send a key event for dispatch. On the settings screen with all
/// registries threaded through, runs the in-process horns dispatcher
/// against the supplied registries and applies the resulting writes
/// through the broker client. Non-settings callers (and settings
/// callers missing any registry) fall back to the legacy input-store
/// path: write the encoded key to `input/key` and let the broker's
/// mode resolver decide handled / unbound.
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
        if let (Some(_cursor), Some(snapshot), Some(bindings), Some(commands), Some(renderers)) =
            (cursor, snapshot, bindings, commands, renderers)
        {
            let Some(chord) = parse_key_str(key) else {
                return send_via_input_store(client, key, screen, flags).await;
            };
            let dispatcher = Dispatcher::new(oxpath!("ui", "settings", "focused"));
            let writes = dispatcher.dispatch(snapshot, &chord, bindings, commands, renderers);
            if writes.is_empty() {
                return send_via_input_store(client, key, screen, flags).await;
            }
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
                    "settings dispatch: substrate rejected a write",
                );
            }
            return KeyDispatchOutcome::Handled;
        }
    }
    send_via_input_store(client, key, screen, flags).await
}

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
        Ok(p) if p.iter().next().map(|c| c.as_str()) == Some("unbound") => {
            let mode = p
                .iter()
                .nth(1)
                .and_then(|c| Mode::parse(c.as_str()))
                .unwrap_or(Mode::Normal);
            KeyDispatchOutcome::Unbound { mode }
        }
        Ok(_) => KeyDispatchOutcome::Handled,
        Err(e) => {
            tracing::warn!(error = %e, key = %key, "input key dispatch failed");
            KeyDispatchOutcome::Handled
        }
    }
}

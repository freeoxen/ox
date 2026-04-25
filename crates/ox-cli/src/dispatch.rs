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

use ox_broker::ClientHandle;
use ox_path::oxpath;
use ox_types::{ClientModalFlags, InputKeyEvent, Mode, Screen};

/// Outcome of a key-dispatch attempt. The `Unbound` arm carries the
/// mode the broker resolved against — clients use it to route the key
/// through the appropriate text-input fallback (`Insert`, `Command`,
/// `Search`) without recomputing mode locally.
pub enum KeyDispatchOutcome {
    /// A binding matched and was executed by the broker's dispatcher.
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
/// See the module-level doc for the full rationale.
pub async fn send_key(
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

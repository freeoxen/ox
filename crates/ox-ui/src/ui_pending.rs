//! Single-slot mailbox for the cross-tick handoff between command
//! handlers (which enqueue) and the event loop (which drains).
//!
//! The slot exists because the event loop processes one tick of UI
//! state, then runs side effects, then re-renders. Side effects that
//! a command needs to trigger (quit, send_input, approval flows,
//! modal toggles, …) cannot happen inside the command itself — that
//! would re-enter the store. They get deposited here and the event
//! loop reads them between ticks.
//!
//! Methods enforce the lifecycle: `set` warns when overwriting an
//! undrained action (which means the event loop missed a tick),
//! `take` is the read-and-clear the event loop wants, `peek` is the
//! non-destructive read for serialization, `clear` discards without
//! observing.

use ox_types::PendingAction;
use structfs_core_store::Value;

/// Single-slot pending-action mailbox. See module doc.
#[derive(Debug, Default)]
pub struct UiPendingMailbox {
    slot: Option<PendingAction>,
}

impl UiPendingMailbox {
    /// Empty mailbox.
    pub fn new() -> Self {
        Self { slot: None }
    }

    /// Enqueue `action`. If the slot already holds a value the prior
    /// action is overwritten — the event loop is expected to drain the
    /// mailbox once per tick, so a same-tick supersession is unusual
    /// enough to log. (`set` deliberately does not return the prior
    /// value: callers that want to observe overwrites should `take`
    /// first; the warn here is the canary.)
    pub fn set(&mut self, action: PendingAction) {
        if let Some(prior) = &self.slot {
            tracing::warn!(
                prior = ?prior, next = ?action,
                "ui pending mailbox: superseding an undrained action — \
                 the event loop did not consume the prior tick's action"
            );
        }
        self.slot = Some(action);
    }

    /// Drain the mailbox: return the current action and clear the slot
    /// in one step. This is the shape the event loop wants — read and
    /// consume are the same operation, so there's no "did I forget to
    /// clear after reading?" failure mode.
    #[allow(dead_code)] // ready for the event_loop migration in a follow-up
    pub fn take(&mut self) -> Option<PendingAction> {
        self.slot.take()
    }

    /// Non-destructive read. Used by the snapshot/serialization path:
    /// the `UiSnapshot` carries `Option<PendingAction>` by value, and
    /// the wire reader at `ui/pending_action` returns the current slot
    /// without draining it (legacy contract — drainage happens via the
    /// `ClearPendingAction` command, not via the read).
    pub fn peek(&self) -> Option<&PendingAction> {
        self.slot.as_ref()
    }

    /// Drop the current action without observing it. Used by the
    /// `ClearPendingAction` command and by `Close` (which clears as
    /// part of its screen-transition contract).
    pub fn clear(&mut self) {
        self.slot = None;
    }

    /// Serialized form for the wire reader at `ui/pending_action`.
    /// `Value::Null` for an empty mailbox; the serialized
    /// `PendingAction` for a held one. Centralizes the
    /// "Option<T> as Value" projection so callers never reconstruct it.
    pub fn as_value(&self) -> Value {
        match &self.slot {
            Some(action) => structfs_serde_store::to_value(action).unwrap_or(Value::Null),
            None => Value::Null,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ox_types::PendingAction;

    #[test]
    fn empty_mailbox_peeks_none_and_takes_none() {
        let mut m = UiPendingMailbox::new();
        assert!(m.peek().is_none());
        assert!(m.take().is_none());
        assert_eq!(m.as_value(), Value::Null);
    }

    #[test]
    fn set_then_peek_returns_action_without_draining() {
        let mut m = UiPendingMailbox::new();
        m.set(PendingAction::Quit);
        assert!(matches!(m.peek(), Some(PendingAction::Quit)));
        // Peek is non-destructive — still there.
        assert!(matches!(m.peek(), Some(PendingAction::Quit)));
    }

    #[test]
    fn take_drains_and_subsequent_take_yields_none() {
        let mut m = UiPendingMailbox::new();
        m.set(PendingAction::Quit);
        assert!(matches!(m.take(), Some(PendingAction::Quit)));
        assert!(m.take().is_none());
    }

    #[test]
    fn clear_drops_action_without_returning_it() {
        let mut m = UiPendingMailbox::new();
        m.set(PendingAction::SendInput);
        m.clear();
        assert!(m.peek().is_none());
    }

    #[test]
    fn as_value_serializes_pending_action() {
        let mut m = UiPendingMailbox::new();
        m.set(PendingAction::Quit);
        // The wire shape mirrors the prior `pending_action_value()` impl —
        // `Value::Null` when empty, the serialized `PendingAction` otherwise.
        // Spot-check non-null; full shape is the responsibility of the
        // PendingAction's own serde impl.
        assert!(!matches!(m.as_value(), Value::Null));
    }

    #[test]
    fn set_over_held_action_overwrites_with_warn() {
        // The supersession warn is logged via tracing; we don't assert
        // on it here (would require a tracing subscriber). The
        // observable behavior is "the next action wins."
        let mut m = UiPendingMailbox::new();
        m.set(PendingAction::Quit);
        m.set(PendingAction::SendInput);
        assert!(matches!(m.peek(), Some(PendingAction::SendInput)));
    }
}

//! Shell types — platform-local state and dispatch for the TUI.

/// What a screen handler returns.
#[allow(dead_code)] // `Ignored` reserved for handlers that punt to global dispatch.
pub(crate) enum Outcome {
    /// Key wasn't handled — fall through to global dispatch.
    Ignored,
    /// State was updated, continue to next frame (skip global dispatch).
    Handled,
}

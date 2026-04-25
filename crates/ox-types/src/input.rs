use serde::{Deserialize, Serialize};

use crate::ui::{Mode, Screen};

/// Modal flags that live in the TUI process (the broker has no view
/// of them). The broker's mode resolver combines these with its own
/// live state (screen, editor open, approval pending, command-line
/// open, inbox-search open) to compute the dispatch mode.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientModalFlags {
    #[serde(default)]
    pub history_search_active: bool,
    #[serde(default)]
    pub show_shortcuts: bool,
    #[serde(default)]
    pub show_usage: bool,
    #[serde(default)]
    pub show_thread_info: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InputKeyEvent {
    /// Optional explicit mode. When supplied (legacy callers, tests,
    /// macros), the broker uses it directly for binding lookup. When
    /// `None`, the broker's `InputStore` mode resolver computes the
    /// mode from `flags` plus its own live state — which closes
    /// snapshot-vs-dispatch races (most notably approval/pending
    /// arriving asynchronously after a thread re-entry).
    #[serde(default)]
    pub mode: Option<Mode>,
    pub key: String,
    pub screen: Screen,
    #[serde(default)]
    pub flags: ClientModalFlags,
}

impl InputKeyEvent {
    /// **Escape hatch.** Build an event with an explicit, client-asserted
    /// mode and default flags. The broker uses the supplied mode
    /// directly for binding lookup — bypassing the server-side resolver
    /// and any race-protection it provides.
    ///
    /// Legitimate uses:
    /// - `input_store` unit tests that exercise binding lookup in
    ///   isolation (no broker, so no resolver is configured).
    /// - Macros / scripted-input replay where the mode is known
    ///   statically.
    /// - Alternate frontends that don't use the
    ///   [`crate::ui::Mode`]-derived focus chain.
    ///
    /// **Production TUI code must not call this.** The TUI client ships
    /// `mode: None` and lets the broker resolve from live state — see
    /// `local/plans/focus-resolution.md`. Reaching for this constructor
    /// in `event_loop.rs` would reintroduce the snapshot-vs-dispatch
    /// race that motivated the resolver in the first place.
    #[doc(hidden)]
    pub fn with_explicit_mode(mode: Mode, key: impl Into<String>, screen: Screen) -> Self {
        Self {
            mode: Some(mode),
            key: key.into(),
            screen,
            flags: ClientModalFlags::default(),
        }
    }
}

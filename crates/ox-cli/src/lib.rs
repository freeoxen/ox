// The bin target (`main.rs`) drives most of these modules; from the lib
// target's perspective many items appear unused. Silence the noise at the
// root rather than sprinkling `#[allow(dead_code)]` across ~50 files.
#![allow(dead_code)]

//! Library surface for `ox-cli`, used by integration tests (crash harness).
//!
//! The `ox` binary is defined in `src/main.rs`; it keeps its own private
//! module tree. This file exists so `tests/` can reach a curated subset of
//! the CLI internals through `ox_cli::…`. Nothing here is load-bearing for
//! the binary.
//!
//! The module list here mirrors `main.rs` because `src/*.rs` freely refer
//! to sibling modules via `crate::…`; both compilation roots must expose
//! the same tree. Items the integration tests actually consume are made
//! `pub`; everything else is `pub(crate)` to keep the public surface tight.

pub mod agents;
pub mod app;
pub mod bindings;
pub mod broker_setup;
pub mod test_support;
pub mod thread_registry;

// Not exposed externally, but referenced from `crate::…` inside lib sources.
pub(crate) mod action_executor;
pub(crate) mod clash_sandbox;
pub(crate) mod commit_drain;
pub(crate) mod config;
pub(crate) mod dialogs;
pub mod dispatch;
pub(crate) mod editor;
pub(crate) mod event_loop;
pub(crate) mod focus;
pub(crate) mod history_state;
pub(crate) mod history_view;
pub(crate) mod inbox_shell;
pub(crate) mod inbox_view;
pub(crate) mod json_backing;
pub(crate) mod key_chord_canonical;
pub(crate) mod key_encode;
pub(crate) mod key_handlers;
pub(crate) mod key_migration;
pub(crate) mod parse;
pub(crate) mod policy;
pub(crate) mod policy_check;
#[allow(dead_code)]
pub(crate) mod session;
pub mod settings;
pub(crate) mod shell;
pub(crate) mod simple_input;
pub(crate) mod tab_bar;
pub(crate) mod text_input_view;
pub(crate) mod theme;

/// Re-export of the post-crash Skip synthetic-`ToolResult` content string
/// used by Task 3d Step 6b's E2E test. The integration test lives outside
/// the crate and cannot reach `pub(crate) mod theme` directly; this
/// re-export keeps the module's own visibility narrow while exposing the
/// one symbol the test contract pins. See
/// `crates/ox-cli/tests/crash_harness_post_crash_reconfirm.rs`.
pub mod test_theme_exports {
    pub use crate::theme::POST_CRASH_SKIP_CONTENT;
}

/// Re-exports of the rendering primitives integration tests need to
/// drive the settings screen through ratatui to a `TestBackend` and
/// capture the visible buffer. Lets a snapshot test verify the *actual
/// rendered output* — what the user sees — rather than just broker
/// writes. Caught the protocol-carousel "doesn't visually toggle"
/// regression that the broker-only e2e tests missed because the
/// regression was in the renderer's read path, not in the cycle's
/// write path.
pub mod test_render_exports {
    pub use crate::theme::Theme;

    /// Test-only wrapper around the in-crate `view_render::render_to_frame`.
    /// The wrapper exists because the underlying function is `pub(crate)`
    /// (its surface is internal-only); this re-exposes it through a
    /// dedicated test entry point without widening the production
    /// visibility.
    pub fn render_to_frame(
        view: &ox_view::View,
        frame: &mut ratatui::Frame,
        area: ratatui::layout::Rect,
        theme: &Theme,
    ) {
        crate::view_render::render_to_frame(view, frame, area, theme);
    }
}
pub(crate) mod thread_shell;
pub(crate) mod thread_view;
pub(crate) mod toml_backing;
pub(crate) mod tui;
pub(crate) mod types;
pub(crate) mod view_render;
pub(crate) mod view_state;

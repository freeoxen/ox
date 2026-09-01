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

pub mod app;
pub mod bindings;
pub mod broker_setup;
pub use ox_executor::test_support;

// Not exposed externally, but referenced from `crate::…` inside lib sources.
pub(crate) mod action_executor;
pub(crate) use ox_config::config;
pub(crate) mod dialogs;
/// Test-only key-dispatch shim. Kept on the library surface so the
/// settings_e2e integration test (which runs against `ox_cli::…`) can
/// drive bindings synchronously without standing up the broker
/// `KeyDispatchSubscription` pipeline. Production callers (in
/// `event_loop.rs`) talk to the broker directly; nothing in `main.rs`
/// reaches into this module.
pub mod dispatch;
pub(crate) mod editor;
pub(crate) mod event_loop;
pub(crate) mod focus;
pub(crate) mod history_state;
pub(crate) mod history_view;
pub(crate) mod horns_loop;
pub(crate) mod inbox_shell;
pub(crate) mod inbox_view;
pub(crate) use ox_config::json_backing;
pub(crate) mod key_chord_canonical;
pub(crate) mod key_encode;
pub(crate) mod key_handlers;
pub(crate) mod key_migration;
pub(crate) mod parse;
#[allow(dead_code)]
pub(crate) mod session;
pub mod settings;
pub(crate) mod shell;
pub(crate) mod shell_copy;
pub(crate) mod simple_input;
pub(crate) mod tab_bar;
pub(crate) mod text_input_view;

/// Re-export of the post-crash Skip synthetic-`ToolResult` content string
/// used by the crash-harness E2E test. The integration test lives outside
/// the crate and cannot reach `pub(crate) mod shell_copy` directly; this
/// re-export keeps the module's own visibility narrow while exposing the
/// one symbol the test contract pins. See
/// `crates/ox-cli/tests/crash_harness_post_crash_reconfirm.rs`.
pub mod test_shell_copy_exports {
    pub use crate::shell_copy::POST_CRASH_SKIP_CONTENT;
}

/// Re-exports of the rendering primitives integration tests need to
/// drive the settings screen through ratatui to a `TestBackend` and
/// capture the visible buffer. Lets a snapshot test assert on the
/// *actual rendered output* — what the user sees — rather than just
/// broker writes. Read-path regressions don't surface in broker-only
/// e2e tests; this surface lets render-path tests catch them.
pub mod test_render_exports {
    pub use horns_ratatui::Theme;
    pub use horns_ratatui::render_to_frame;
}
pub(crate) mod thread_shell;
pub(crate) mod thread_view;
pub(crate) use ox_config::toml_backing;
pub(crate) mod tui;
pub(crate) mod types;
pub(crate) mod view_state;

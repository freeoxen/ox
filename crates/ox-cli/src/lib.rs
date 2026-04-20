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
pub(crate) mod config;
pub(crate) mod dialogs;
pub(crate) mod editor;
pub(crate) mod event_loop;
pub(crate) mod focus;
pub(crate) mod history_state;
pub(crate) mod history_view;
pub(crate) mod inbox_shell;
pub(crate) mod inbox_view;
pub(crate) mod key_encode;
pub(crate) mod key_handlers;
pub(crate) mod parse;
pub(crate) mod policy;
pub(crate) mod policy_check;
#[allow(dead_code)]
pub(crate) mod session;
pub(crate) mod settings_shell;
pub(crate) mod settings_state;
pub(crate) mod settings_view;
pub(crate) mod shell;
pub(crate) mod simple_input;
pub(crate) mod tab_bar;
pub(crate) mod text_input_view;
pub(crate) mod theme;
pub(crate) mod thread_shell;
pub(crate) mod thread_view;
pub(crate) mod toml_backing;
pub(crate) mod transport;
pub(crate) mod tui;
pub(crate) mod types;
pub(crate) mod view_state;

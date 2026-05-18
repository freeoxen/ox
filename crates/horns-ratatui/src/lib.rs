//! horns-ratatui: ratatui backend for the horns framework.
//!
//! Translates a `horns_core::View` tree into ratatui draw calls. Two
//! shapes are available:
//!
//! - **Backend-as-translator**: call [`render_to_frame`] inside the
//!   host's `terminal.draw(|frame| …)` closure. Use this for screens
//!   that compose multiple Views (e.g. tab bar + content + status bar)
//!   into one frame.
//! - **Backend-as-subscription**: call [`install`] to register a
//!   `ViewRenderSubscription` that owns the terminal under a
//!   `parking_lot::Mutex`. Use this for screens that own the full
//!   frame and want render-on-update without going through the host.

pub mod install;
pub mod render;
pub mod theme;

pub use install::{RatatuiHandle, RatatuiOptions, install};
pub use render::render_to_frame;
pub use theme::Theme;

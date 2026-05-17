//! horns-ratatui: ratatui backend for the horns framework.
//!
//! Translates a `horns_core::View` tree into ratatui draw calls.

pub mod render;
pub mod theme;

pub use render::render_to_frame;
pub use theme::Theme;

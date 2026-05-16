//! horns: path-MVU UI toolkit.
//!
//! Re-exports horns-core. With the `ratatui` feature (on by default)
//! also exposes `horns::ratatui` re-exporting horns-ratatui.

pub use horns_core::*;

#[cfg(feature = "ratatui")]
pub mod ratatui {
    pub use horns_ratatui::*;
}

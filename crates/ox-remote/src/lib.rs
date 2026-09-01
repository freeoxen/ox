//! Typed host-side adapters for remote ox execution.

mod error;
mod exe;
mod manager;
mod placement;
mod reconcile;
mod records;
mod state;

pub use error::*;
pub use exe::*;
pub use manager::*;
pub use records::*;
pub use state::*;

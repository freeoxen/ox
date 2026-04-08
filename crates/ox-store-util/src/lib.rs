//! StructFS store utilities — composable wrappers and helpers.
//!
//! Platform-agnostic utilities for working with StructFS stores:
//! - `ReadOnly<S>` — rejects writes, passes reads through
//! - `Masked<S>` — redacts specified paths on read

pub mod local_config;
pub mod masked;
pub mod read_only;

pub use local_config::LocalConfig;
pub use masked::Masked;
pub use read_only::ReadOnly;

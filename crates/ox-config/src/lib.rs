//! Shared `~/.ox` configuration for the ox binaries.
//!
//! ox-cli and ox-gateway read the same `config.toml` + `keys.json`; this
//! crate holds the single copy of the figment resolution, the legacy
//! migrations, and the file backings so the two daemons cannot drift.

pub mod config;
pub mod json_backing;
pub mod toml_backing;

pub use config::{resolve_config, CliOverrides, OxConfig};
pub use json_backing::JsonFileBacking;
pub use toml_backing::TomlFileBacking;

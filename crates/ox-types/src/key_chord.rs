//! Compatibility shim: the data types live in horns-core now.
//! Remove when all callers import from horns_core::key directly.

pub use horns_core::key::{KeyChord, KeyCodeRepr, KeyModifierSet};

//! StructFS store utilities — composable wrappers and helpers.
//!
//! Platform-agnostic utilities for working with StructFS stores:
//! - `ReadOnly<S>` — rejects writes, passes reads through
//! - `Masked<S>` — redacts specified paths on read
//! - `StoreBacking` — platform-agnostic persistence abstraction

pub mod backing;
pub mod cascade;
pub mod jsonl_file_backing;
pub mod local_config;
pub mod masked;
pub mod read_only;

pub use backing::StoreBacking;
pub use cascade::Cascade;
pub use jsonl_file_backing::JsonlFileBacking;
pub use local_config::LocalConfig;
pub use masked::Masked;
pub use read_only::ReadOnly;

use std::collections::BTreeMap;
use structfs_core_store::Value;

/// Recursively expand a value into flat-keyed leaves under `prefix`.
/// `Value::Map` recurses into its fields; non-Map values are leaves
/// inserted at `prefix`. The output preserves the StructFS leaf-only
/// storage shape: a path is either a leaf or a Map of children, never
/// both — so writes that receive a parent Map decompose it here before
/// committing to a flat-keyed backing.
pub fn flatten_value_into(prefix: &str, value: &Value, out: &mut BTreeMap<String, Value>) {
    match value {
        Value::Map(m) => {
            for (k, v) in m {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{}/{}", prefix, k)
                };
                flatten_value_into(&path, v, out);
            }
        }
        _ => {
            out.insert(prefix.to_string(), value.clone());
        }
    }
}

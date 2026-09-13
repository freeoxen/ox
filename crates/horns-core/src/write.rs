//! A pending write into the broker: (path, record).
//!
//! `Write` is an in-process dispatch message. Although StructFS Record
//! supports Serde, this protocol does not define a persisted or wire shape.

use structfs_core_store::{Path, Record};

/// A single write to be dispatched.
#[derive(Clone, Debug)]
pub struct Write {
    pub path: Path,
    pub record: Record,
}

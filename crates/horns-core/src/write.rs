//! A pending write into the broker: (path, record).
//!
//! Carries `Record`, which has no serde impl, so `Write` is
//! **in-process only** — it's never round-tripped through a wire
//! format.

use structfs_core_store::{Path, Record};

/// A single write to be dispatched.
#[derive(Clone, Debug)]
pub struct Write {
    pub path: Path,
    pub record: Record,
}

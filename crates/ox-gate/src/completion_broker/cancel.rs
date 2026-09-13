//! Shared cancellation for completion and gateway block runs.
//!
//! GC cancels the token to wake parked reads. The host remains responsible
//! for joining execution and releasing downstream resources after cancellation.

pub use structfs_handles::CancelToken as CancelHandle;

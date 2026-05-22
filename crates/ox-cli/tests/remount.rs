//! Integration tests for the remount lifecycle. Each `mod` is one
//! scenario; `support` holds shared harness helpers. Explicit `#[path]`
//! attributes are needed because, as the crate root of this
//! integration test, this file's submodule resolution is sibling-based
//! (`tests/<name>.rs`) rather than nested under `remount/`.

#[path = "remount/support.rs"]
mod support;

#[path = "remount/approval_resume.rs"]
mod approval_resume;
#[path = "remount/cross_phase.rs"]
mod cross_phase;
#[path = "remount/durable_stream.rs"]
mod durable_stream;
#[path = "remount/post_crash_reconfirm.rs"]
mod post_crash_reconfirm;
#[path = "remount/smoke.rs"]
mod smoke;

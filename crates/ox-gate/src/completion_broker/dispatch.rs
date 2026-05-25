//! Per-request upstream dispatch — implemented in Task 3.4.
//!
//! This module will own the spawned async task that:
//!   - resolves model → CompletionRole → account → provider → API key
//!   - builds an HttpRequest via the dialect codec
//!   - drives the SseHttpExecutor stream and pushes events into the
//!     shared Inflight buffer
//!   - flips status to Complete or Failed at terminal
//!   - appends a UsageRecord to gateway/usage on Complete

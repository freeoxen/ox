# Phase 1: Extract Codecs

## Goal

Move codec functions from ox-web to a new ox-gate crate. Pure refactor — no
behavior change.

## Checklist

### Create ox-gate crate
- [ ] `crates/ox-gate/Cargo.toml` — deps: ox-kernel, serde, serde_json
- [ ] `crates/ox-gate/src/lib.rs` — `pub mod codec` + re-exports
- [ ] `crates/ox-gate/src/codec/mod.rs` — `UsageInfo` struct, sub-module declarations

### Extract Anthropic codec
- [ ] `crates/ox-gate/src/codec/anthropic.rs` — `parse_sse_events` (from ox-web L75-148)
- [ ] `crates/ox-gate/src/codec/anthropic.rs` — `extract_usage` (from ox-web L1071-1104)

### Extract OpenAI codec
- [ ] `crates/ox-gate/src/codec/openai.rs` — `translate_request` (from ox-web L695-837)
- [ ] `crates/ox-gate/src/codec/openai.rs` — `parse_sse_events` (from ox-web L840-921)

### Wire into workspace
- [ ] `Cargo.toml` — add `"crates/ox-gate"` to workspace members
- [ ] `crates/ox-web/Cargo.toml` — add ox-gate dep
- [ ] `crates/ox-web/src/lib.rs` — replace 4 local functions with ox-gate imports
- [ ] `crates/ox-web/src/lib.rs` — adapt `run_agentic_loop` for `UsageInfo` return

## Test Plan

- Unit tests in `codec/anthropic.rs`:
  - `parse_sse_events` produces correct StreamEvent variants for text, tool_use, error, message_stop
  - `extract_usage` returns (input_tokens, output_tokens) from message_start + message_delta
- Unit tests in `codec/openai.rs`:
  - `translate_request` converts system, user, assistant, tool_result messages
  - `translate_request` converts tool schemas to OpenAI function format
  - `parse_sse_events` handles text, tool calls with index tracking, usage extraction

## Verification Gates

```bash
cargo check --workspace
cargo check --target wasm32-unknown-unknown -p ox-web
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

## Scratch Space

(implementation notes go here)

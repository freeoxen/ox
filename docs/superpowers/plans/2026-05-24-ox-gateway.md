# ox-gateway Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `ox-gateway` — a standalone localhost daemon that exposes Anthropic Messages / OpenAI Chat Completions / `/v1/models` / `count_tokens` HTTP APIs, sharing `~/.ox/config.toml` + `~/.ox/keys.json` with ox-cli, with all state and dispatch routed through StructFS Reader/Writer.

**Architecture:** Three layers, all mounted on `ox-broker::BrokerStore`. (1) The existing `config/` + `secret/` + `gate/` mounts (same backings as ox-cli). (2) Two new mounts: `gateway/completions/` (a `CompletionBrokerStore` modeled on `structfs-http::HttpBrokerStore`, generalized for streaming via an injected `SseHttpExecutor`) and `gateway/usage/` (append-only JSONL ledger). (3) An axum bin that decodes inbound wire → `CompletionRequest`, writes to `gateway/completions`, drains events as wire SSE, GCs handle. Codec symmetry in `ox-gate::codec` completes the four corners (decode_request + SseEncoder); `StreamEvent` moves from `ox-kernel` to `ox-types` and gains `InputUsage` / `OutputUsage` variants so usage roundtrips through `Record::Parsed`.

**Tech Stack:** Rust (edition 2024). axum, tokio (full features including sync::Notify), futures, async-stream, reqwest, ulid, tracing. structfs-core-store, structfs-serde-store, structfs-http. Internal: ox-types, ox-kernel, ox-broker, ox-gate, ox-store-util, ox-path.

**Spec:** `docs/superpowers/specs/2026-05-24-ox-gateway-design.md` (on branch `improvements`; the plan is self-sufficient if you can't access it).

**Scope:** This plan delivers the entire gateway end-to-end. Phases are dependency-ordered; Phase 2 tasks can run in parallel after Phase 1 lands.

---

## File Structure

| File | Responsibility |
|------|---------------|
| **Phase 1 — StreamEvent migration** | |
| `crates/ox-types/src/stream_event.rs` (new) | The widened `StreamEvent` enum + 2 new variants (`InputUsage`, `OutputUsage`) |
| `crates/ox-types/src/lib.rs` (modify) | Re-export `StreamEvent` |
| `crates/ox-kernel/src/lib.rs` (modify) | Remove local `StreamEvent` def; re-export from ox-types for transition; widen `accumulate_response` to ignore new variants |
| `crates/ox-kernel/Cargo.toml` (modify) | Add `ox-types` dep if not already present |
| `crates/ox-gate/src/codec/anthropic.rs` (modify) | `SseParser` emits typed `InputUsage`/`OutputUsage` events instead of side-channel updates |
| `crates/ox-gate/src/codec/openai.rs` (modify) | Same |
| `crates/ox-gate/src/transport.rs` (modify) | Use new variants where it parses upstream SSE |
| `crates/ox-tools/src/completion.rs` (modify) | `stream_event_to_json` gains arms for two new variants |
| **Phase 2 — Codec inverse (Anthropic)** | |
| `crates/ox-gate/src/codec/error.rs` (new) | `CodecError` enum |
| `crates/ox-gate/src/codec/sse_encoder.rs` (new) | `SseEncoder` state machine |
| `crates/ox-gate/src/codec/anthropic.rs` (modify) | Add `decode_request`, `encode_sse_event`, `encode_response` |
| `crates/ox-gate/src/codec/mod.rs` (modify) | Re-export new items |
| **Phase 2 — Codec inverse (OpenAI)** | |
| `crates/ox-gate/src/codec/openai.rs` (modify) | Add `decode_request`, `encode_sse_event`, `encode_response` |
| **Phase 2 — SseHttpExecutor** | |
| `crates/ox-gate/src/transport.rs` (modify) | Add `SseHttpExecutor` trait + `ReqwestSseExecutor` impl alongside existing functions |
| **Phase 2 — JsonlFileBacking** | |
| `crates/ox-store-util/src/jsonl_file_backing.rs` (new) | Append-only JSONL backing |
| `crates/ox-store-util/src/lib.rs` (modify) | Export `JsonlFileBacking` |
| **Phase 3 — UsageStore** | |
| `crates/ox-gate/src/usage_store.rs` (new) | `UsageRecord` + `UsageStore` (Reader/Writer over `JsonlFileBacking`) |
| `crates/ox-gate/src/lib.rs` (modify) | `pub mod usage_store;` + re-exports |
| **Phase 3 — CompletionBrokerStore** | |
| `crates/ox-gate/src/completion_broker/mod.rs` (new) | Public surface + Reader/Writer impls |
| `crates/ox-gate/src/completion_broker/inflight.rs` (new) | `Inflight`, `InflightState`, `CompletionStatus` |
| `crates/ox-gate/src/completion_broker/dispatch.rs` (new) | `per_request_task` + resolution helpers |
| `crates/ox-gate/src/completion_broker/mock.rs` (new, cfg(test)) | `MockSseExecutor` |
| **Phase 4 — ox-gateway bin crate** | |
| `crates/ox-gateway/Cargo.toml` (new) | Manifest |
| `crates/ox-gateway/src/main.rs` (new) | Broker assembly + axum serve |
| `crates/ox-gateway/src/lib.rs` (new) | Re-exports for testability |
| `crates/ox-gateway/src/handle.rs` (new) | The shared async streaming-drain helper |
| `crates/ox-gateway/src/error.rs` (new) | Dialect-shaped error envelope construction |
| `crates/ox-gateway/src/routes/mod.rs` (new) | Router assembly |
| `crates/ox-gateway/src/routes/anthropic.rs` (new) | `POST /v1/messages`, `POST /v1/messages/count_tokens` |
| `crates/ox-gateway/src/routes/openai.rs` (new) | `POST /v1/chat/completions` |
| `crates/ox-gateway/src/routes/models.rs` (new) | `GET /v1/models` (both dialect shapes) |
| `crates/ox-gateway/src/routes/ox_native.rs` (new) | `POST /completions` (ox-native shape) |
| `Cargo.toml` (modify, workspace root) | Add `crates/ox-gateway` to workspace members |
| **Phase 5 — Integration tests** | |
| `crates/ox-gateway/tests/streaming_anthropic.rs` (new) | End-to-end stream test for /v1/messages |
| `crates/ox-gateway/tests/streaming_openai.rs` (new) | End-to-end stream test for /v1/chat/completions |
| `crates/ox-gateway/tests/models.rs` (new) | `/v1/models` aggregation test |
| `crates/ox-gateway/tests/error_paths.rs` (new) | Resolution failures, upstream errors |

---

## Conventions used throughout

- **TDD:** every code change starts with a failing test, then minimal implementation, then verify pass, then commit.
- **Cargo commands** run from the repo root unless noted. The worktree's repo root is `/Users/alex/Devel/AdjectiveNoun/ox/.claude/worktrees/ox-gateway`.
- **Test runs** use `cargo test -p <crate>` to scope. Full workspace builds with `cargo build --workspace`.
- **Commits:** subject-only is fine when the change is single-purpose; use a body when explaining a non-obvious decision (e.g. trait shape).
- **No `// Phase N` comments** in code per the project's "comments explain WHY" rule.

---

## Phase 1: StreamEvent migration

Foundational. Subsequent phases assume `StreamEvent` lives in `ox-types` with the two new variants.

### Task 1.1: Move `StreamEvent` to `ox-types` with widened variants

**Files:**
- Create: `crates/ox-types/src/stream_event.rs`
- Modify: `crates/ox-types/src/lib.rs`
- Modify: `crates/ox-types/Cargo.toml` (add `ox-kernel` dep ONLY if there's nothing circular; this crate already lives below ox-kernel in the dep graph, so check first)

- [ ] **Step 1: Inspect current `StreamEvent` location**

Run: `grep -n "pub enum StreamEvent" crates/ox-kernel/src/lib.rs`
Expected: line ~157 shows current 5-variant enum.

- [ ] **Step 2: Create the new file with the widened enum**

Create `crates/ox-types/src/stream_event.rs`:

```rust
//! A single event from a streaming completion response.
//!
//! Crosses the StructFS substrate boundary as a typed record — promoted
//! from a kernel-internal enum so consumers on either side of a
//! Reader/Writer call can roundtrip the same shape.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    TextDelta { text: String },
    ToolUseStart {
        id: String,
        name: String,
    },
    ToolUseInputDelta { delta: String },
    MessageStop,
    Error { message: String },
    /// Input-side usage (input tokens, cache creation, cache read).
    /// Emitted by upstream SSE parsers at message_start (Anthropic) or
    /// when prompt_tokens lands (OpenAI).
    InputUsage {
        input_tokens: u32,
        cache_creation: u32,
        cache_read: u32,
    },
    /// Output-side usage (completion tokens). Emitted at message_delta
    /// (Anthropic) or final usage block (OpenAI).
    OutputUsage { output_tokens: u32 },
}
```

Note: the existing kernel enum uses bare variants (e.g. `TextDelta(String)`). We're using struct variants (`TextDelta { text }`) for serde-friendliness across the substrate. The kernel's `accumulate_response` pattern-matches on these; Task 1.3 updates the pattern.

- [ ] **Step 3: Re-export from `ox-types`**

Edit `crates/ox-types/src/lib.rs` — add near the other `pub mod` declarations:

```rust
pub mod stream_event;
pub use stream_event::StreamEvent;
```

- [ ] **Step 4: Write a roundtrip test in the new module**

Append to `crates/ox-types/src/stream_event.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_delta_json_roundtrip() {
        let ev = StreamEvent::TextDelta { text: "hello".into() };
        let s = serde_json::to_string(&ev).unwrap();
        let back: StreamEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(ev, back);
    }

    #[test]
    fn input_usage_json_roundtrip() {
        let ev = StreamEvent::InputUsage {
            input_tokens: 100,
            cache_creation: 50,
            cache_read: 25,
        };
        let s = serde_json::to_string(&ev).unwrap();
        let back: StreamEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(ev, back);
    }

    #[test]
    fn output_usage_json_roundtrip() {
        let ev = StreamEvent::OutputUsage { output_tokens: 42 };
        let s = serde_json::to_string(&ev).unwrap();
        let back: StreamEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(ev, back);
    }
}
```

- [ ] **Step 5: Verify the test fails (file not registered yet)**

Run: `cargo test -p ox-types stream_event`
Expected: compile error — `stream_event` module not declared. (The lib.rs edit in Step 3 may have already been done; if so, this passes — that's fine, move on.)

- [ ] **Step 6: Verify the test passes**

Run: `cargo test -p ox-types stream_event`
Expected: 3 tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/ox-types/src/stream_event.rs crates/ox-types/src/lib.rs
git commit -m "feat(ox-types): widen StreamEvent with usage variants; relocate from ox-kernel"
```

### Task 1.2: Remove `StreamEvent` from `ox-kernel`, re-export from `ox-types`

**Files:**
- Modify: `crates/ox-kernel/src/lib.rs` (delete pub enum StreamEvent, add re-export)
- Modify: `crates/ox-kernel/Cargo.toml` (confirm ox-types dep exists — it should, since CompletionRole lives there)

- [ ] **Step 1: Confirm `ox-kernel` already depends on `ox-types`**

Run: `grep "ox-types" crates/ox-kernel/Cargo.toml`
Expected: `ox-types = { workspace = true }` present. If not, add it.

- [ ] **Step 2: Replace the local enum definition with a re-export**

Edit `crates/ox-kernel/src/lib.rs`. Find the existing `pub enum StreamEvent { ... }` block (around line 157) and replace with:

```rust
pub use ox_types::StreamEvent;
```

- [ ] **Step 3: Update `accumulate_response` to ignore the two new variants**

In the same file, find `pub fn accumulate_response(...)` (it walks `StreamEvent` variants). Add explicit no-op arms for the new variants. The function's body match becomes (preserving existing arms):

```rust
for event in events {
    match event {
        StreamEvent::TextDelta { text } => {
            // existing logic — note the variant is now struct-form
            flush_tool(&mut blocks, &mut current_tool);
            current_text.push_str(&text);
            emit(AgentEvent::TextDelta(text));
        }
        StreamEvent::ToolUseStart { id, name } => {
            flush_text(&mut blocks, &mut current_text);
            flush_tool(&mut blocks, &mut current_tool);
            current_tool = Some((id, name, String::new()));
        }
        StreamEvent::ToolUseInputDelta { delta } => {
            if let Some((_, _, ref mut input_json)) = current_tool {
                input_json.push_str(&delta);
            }
        }
        StreamEvent::MessageStop => {
            // existing flush + break logic stays
        }
        StreamEvent::Error { message } => {
            // existing error propagation stays
            // (rename `msg` -> `message` if existing code uses `msg`)
        }
        // New variants — kernel ignores them (usage is captured elsewhere)
        StreamEvent::InputUsage { .. } | StreamEvent::OutputUsage { .. } => {}
    }
}
```

The variant SHAPE changed: bare `TextDelta(String)` → `TextDelta { text }`, `Error(String)` → `Error { message }`. Every `match` on StreamEvent across the codebase needs to update. This task fixes the kernel; Task 1.3 fixes ox-gate and ox-tools; Task 1.4 fixes ox-cli/ox-web.

- [ ] **Step 4: Update `stream_event_to_json` in run.rs to match new variants**

Search for `fn stream_event_to_json` (or similar) in `crates/ox-kernel/src/run.rs`. Update arms:

```rust
fn stream_event_to_json(event: &StreamEvent) -> serde_json::Value {
    match event {
        StreamEvent::TextDelta { text } => serde_json::json!({
            "type": "text_delta",
            "text": text,
        }),
        StreamEvent::ToolUseStart { id, name } => serde_json::json!({
            "type": "tool_use_start",
            "id": id,
            "name": name,
        }),
        StreamEvent::ToolUseInputDelta { delta } => serde_json::json!({
            "type": "tool_use_input_delta",
            "delta": delta,
        }),
        StreamEvent::MessageStop => serde_json::json!({ "type": "message_stop" }),
        StreamEvent::Error { message } => serde_json::json!({
            "type": "error",
            "message": message,
        }),
        StreamEvent::InputUsage { input_tokens, cache_creation, cache_read } => serde_json::json!({
            "type": "input_usage",
            "input_tokens": input_tokens,
            "cache_creation": cache_creation,
            "cache_read": cache_read,
        }),
        StreamEvent::OutputUsage { output_tokens } => serde_json::json!({
            "type": "output_usage",
            "output_tokens": output_tokens,
        }),
    }
}
```

Same shape change applies to `json_to_stream_event` (search for it; update construction sites accordingly).

- [ ] **Step 5: Build ox-kernel**

Run: `cargo build -p ox-kernel`
Expected: compiles. (Failures here mean a callsite missed the variant shape change — fix them.)

- [ ] **Step 6: Run ox-kernel tests**

Run: `cargo test -p ox-kernel`
Expected: all passing tests still pass. Tests that exercise SseEvent shapes may need similar variant-shape updates.

- [ ] **Step 7: Commit**

```bash
git add crates/ox-kernel/
git commit -m "refactor(ox-kernel): re-export StreamEvent from ox-types; handle struct-form variants"
```

### Task 1.3: Update `ox-gate` and `ox-tools` to the new `StreamEvent` shape

**Files:**
- Modify: `crates/ox-gate/src/codec/anthropic.rs` — SseParser construction sites
- Modify: `crates/ox-gate/src/codec/openai.rs` — same
- Modify: `crates/ox-gate/src/transport.rs` — SseParser uses the new variants for usage instead of side-channel
- Modify: `crates/ox-tools/src/completion.rs` — `stream_event_to_json` arms

- [ ] **Step 1: Update Anthropic codec to emit struct-form variants**

In `crates/ox-gate/src/codec/anthropic.rs`, search for `StreamEvent::` constructions. Each variant call site needs the struct form:

| Before | After |
|---|---|
| `StreamEvent::TextDelta(s)` | `StreamEvent::TextDelta { text: s }` |
| `StreamEvent::ToolUseInputDelta(s)` | `StreamEvent::ToolUseInputDelta { delta: s }` |
| `StreamEvent::Error(s)` | `StreamEvent::Error { message: s }` |
| `StreamEvent::ToolUseStart { id, name }` | unchanged |
| `StreamEvent::MessageStop` | unchanged |

Replace mechanically. Run `cargo build -p ox-gate` between edits to catch missed sites.

- [ ] **Step 2: Update Anthropic SseParser to emit usage events**

Find `fn parse_anthropic(&mut self, json: &serde_json::Value) -> Vec<StreamEvent>` in `crates/ox-gate/src/transport.rs`. Currently `message_start`'s usage is written to `self.usage.input_tokens` etc. as a side effect. Replace the side-effect with returning `InputUsage` / `OutputUsage` events:

```rust
"message_start" => {
    let mut out = Vec::new();
    if let Some(usage) = json.get("message").and_then(|m| m.get("usage")) {
        let input_tokens = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let cache_creation = usage.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let cache_read = usage.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        out.push(StreamEvent::InputUsage { input_tokens, cache_creation, cache_read });
    }
    out
}
"message_delta" => {
    let mut out = Vec::new();
    if let Some(usage) = json.get("usage") {
        if let Some(output_tokens) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
            out.push(StreamEvent::OutputUsage { output_tokens: output_tokens as u32 });
        }
    }
    out
}
```

Remove the `self.usage.*` writes — those fields can stay on the struct for now (they're still useful for transport callers that haven't switched over) but the parser stops populating them.

Actually: REMOVE the `usage: UsageInfo` field from `SseParser` entirely. Search for `pub usage: UsageInfo` and `parser.usage`. Callers that need usage should now consume the typed events. Update `streaming_fetch` (the existing sync function) to extract usage from the event stream:

```rust
// inside streaming_fetch, after the loop that collects events:
let usage = UsageInfo::from_events(&all_events);
return Ok((all_events, usage));
```

Add `UsageInfo::from_events`:

```rust
// in ox-gate/src/codec/mod.rs (where UsageInfo is defined)
impl UsageInfo {
    pub fn from_events(events: &[StreamEvent]) -> Self {
        let mut info = Self::default();
        for ev in events {
            match ev {
                StreamEvent::InputUsage { input_tokens, cache_creation, cache_read } => {
                    info.input_tokens = *input_tokens;
                    info.cache_creation_input_tokens = *cache_creation;
                    info.cache_read_input_tokens = *cache_read;
                }
                StreamEvent::OutputUsage { output_tokens } => {
                    info.output_tokens = *output_tokens;
                }
                _ => {}
            }
        }
        info
    }
}
```

- [ ] **Step 3: Update OpenAI SseParser the same way**

In the same `transport.rs`, find `fn parse_openai`. Replace `self.usage.*` writes with emitted events:

```rust
fn parse_openai(&mut self, json: &serde_json::Value) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    if let Some(usage_obj) = json.get("usage") {
        let input_tokens = usage_obj.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let output_tokens = usage_obj.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let cache_read = usage_obj
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        if input_tokens > 0 || cache_read > 0 {
            events.push(StreamEvent::InputUsage { input_tokens, cache_creation: 0, cache_read });
        }
        if output_tokens > 0 {
            events.push(StreamEvent::OutputUsage { output_tokens });
        }
    }

    // (existing choices/delta loop continues here, with variant constructions
    //  updated to struct form per Step 1)

    events
}
```

- [ ] **Step 4: Update `ox-tools::completion::stream_event_to_json`**

In `crates/ox-tools/src/completion.rs`, replicate the same arm updates as Task 1.2 Step 4 (the function is structurally identical between the two crates — kernel's and tools' versions are deliberately parallel).

- [ ] **Step 5: Build the whole workspace**

Run: `cargo build --workspace`
Expected: compiles. Missing variant arms are compile errors and the message tells you the file/line.

- [ ] **Step 6: Run all tests in touched crates**

Run: `cargo test -p ox-types -p ox-kernel -p ox-gate -p ox-tools`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/ox-gate/ crates/ox-tools/
git commit -m "refactor: SseParser emits typed Input/OutputUsage; drop usage side-channel"
```

### Task 1.4: Update remaining touch sites in `ox-web` and `ox-cli`

**Files:**
- Modify: any file in `crates/ox-web/` and `crates/ox-cli/` that matches on `StreamEvent` variants

- [ ] **Step 1: Find call sites**

Run: `grep -rn "StreamEvent::" crates/ox-web/ crates/ox-cli/`
Expected: list of sites. Each line shows a match site.

- [ ] **Step 2: Update each site to the struct-form variants**

Same mechanical replacement as Task 1.3 Step 1. If a site doesn't construct or pattern-match (e.g. only imports the type), it's already fine.

- [ ] **Step 3: Build the workspace**

Run: `cargo build --workspace`
Expected: compiles.

- [ ] **Step 4: Run full test suite**

Run: `cargo test --workspace`
Expected: all pass. (If there are pre-existing failures unrelated to this work, note them and continue — don't fix unrelated breakage in this plan.)

- [ ] **Step 5: Commit**

```bash
git add crates/ox-web/ crates/ox-cli/
git commit -m "refactor: update ox-web and ox-cli call sites for struct-form StreamEvent"
```

---

## Phase 2: Codec inverse — both dialects (Anthropic + OpenAI), `SseHttpExecutor`, `JsonlFileBacking`

These three streams can run in parallel after Phase 1 lands. Each is independent and ends with its own commit.

### Task 2.1: Add `CodecError`

**Files:**
- Create: `crates/ox-gate/src/codec/error.rs`
- Modify: `crates/ox-gate/src/codec/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/ox-gate/src/codec/error.rs`:

```rust
//! Codec error type for inbound (wire → internal) translation.

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum CodecError {
    #[error("missing required field: {0}")]
    MissingField(&'static str),
    #[error("invalid shape: {0}")]
    InvalidShape(String),
    #[error("unsupported feature: {0}")]
    UnsupportedFeature(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_field_display() {
        let e = CodecError::MissingField("model");
        assert_eq!(e.to_string(), "missing required field: model");
    }
}
```

- [ ] **Step 2: Register the module**

In `crates/ox-gate/src/codec/mod.rs` add:

```rust
pub mod error;
pub use error::CodecError;
```

If `thiserror` isn't in `ox-gate`'s `Cargo.toml`, add it (`thiserror = { workspace = true }`).

- [ ] **Step 3: Run the test**

Run: `cargo test -p ox-gate codec::error`
Expected: 1 test passes.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-gate/src/codec/
git commit -m "feat(ox-gate): codec::CodecError for inbound translation failures"
```

### Task 2.2: Anthropic `decode_request`

**Files:**
- Modify: `crates/ox-gate/src/codec/anthropic.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/ox-gate/src/codec/anthropic.rs` (or create a `#[cfg(test)] mod decode_tests` block):

```rust
#[cfg(test)]
mod decode_tests {
    use super::*;

    #[test]
    fn decode_minimal_request() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "hi"}]
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(req.model, "claude-sonnet-4-20250514");
        assert_eq!(req.max_tokens, 1024);
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.system, "");
        assert!(req.tools.is_empty());
    }

    #[test]
    fn decode_with_system_string() {
        let body = serde_json::json!({
            "model": "m",
            "max_tokens": 1,
            "system": "you are helpful",
            "messages": []
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(req.system, "you are helpful");
    }

    #[test]
    fn decode_with_system_block_array() {
        // Anthropic also accepts `system: [{type:"text", text:"..."}]`.
        // Flatten to a single string for the internal CompletionRequest.
        let body = serde_json::json!({
            "model": "m",
            "max_tokens": 1,
            "system": [{"type": "text", "text": "first"}, {"type": "text", "text": "second"}],
            "messages": []
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(req.system, "first\n\nsecond");
    }

    #[test]
    fn missing_model_errors() {
        let body = serde_json::json!({"max_tokens": 1, "messages": []});
        assert_eq!(decode_request(&body).unwrap_err(), CodecError::MissingField("model"));
    }

    #[test]
    fn missing_max_tokens_errors() {
        let body = serde_json::json!({"model": "m", "messages": []});
        assert_eq!(decode_request(&body).unwrap_err(), CodecError::MissingField("max_tokens"));
    }
}
```

- [ ] **Step 2: Run, confirm fails**

Run: `cargo test -p ox-gate codec::anthropic::decode_tests`
Expected: compile error — `decode_request` not defined.

- [ ] **Step 3: Implement `decode_request`**

Add to `crates/ox-gate/src/codec/anthropic.rs`:

```rust
use crate::codec::CodecError;
use ox_kernel::{CompletionRequest, ToolSchema};

pub fn decode_request(body: &serde_json::Value) -> Result<CompletionRequest, CodecError> {
    let obj = body.as_object().ok_or(CodecError::InvalidShape("body must be a JSON object".into()))?;

    let model = obj.get("model").and_then(|v| v.as_str())
        .ok_or(CodecError::MissingField("model"))?
        .to_string();

    let max_tokens = obj.get("max_tokens").and_then(|v| v.as_u64())
        .ok_or(CodecError::MissingField("max_tokens"))? as u32;

    let system = match obj.get("system") {
        None => String::new(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => arr.iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => return Err(CodecError::InvalidShape("system must be string or array of text blocks".into())),
    };

    let messages = obj.get("messages").and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let tools: Vec<ToolSchema> = obj.get("tools").and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let name = t.get("name")?.as_str()?.to_string();
                    let description = t.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let input_schema = t.get("input_schema").cloned().unwrap_or(serde_json::Value::Null);
                    Some(ToolSchema { name, description, input_schema })
                })
                .collect()
        })
        .unwrap_or_default();

    let stream = obj.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    Ok(CompletionRequest { model, max_tokens, system, messages, tools, stream })
}
```

- [ ] **Step 4: Run, confirm passes**

Run: `cargo test -p ox-gate codec::anthropic::decode_tests`
Expected: 5 tests pass.

- [ ] **Step 5: Roundtrip property test**

Append:

```rust
#[test]
fn encode_decode_roundtrip_minimal() {
    let original = CompletionRequest {
        model: "m".into(),
        max_tokens: 100,
        system: "sys".into(),
        messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
        tools: vec![],
        stream: true,
    };
    let wire = translate_request(&original);
    let back = decode_request(&wire).unwrap();
    assert_eq!(back.model, original.model);
    assert_eq!(back.max_tokens, original.max_tokens);
    assert_eq!(back.system, original.system);
    assert_eq!(back.messages, original.messages);
    assert_eq!(back.stream, original.stream);
}
```

Run: `cargo test -p ox-gate codec::anthropic::decode_tests::encode_decode_roundtrip_minimal`
Expected: pass. (If `translate_request` doesn't exist for Anthropic — it's the kernel's `serde_json::to_value(request)` path — substitute `serde_json::to_value(&original).unwrap()`.)

- [ ] **Step 6: Commit**

```bash
git add crates/ox-gate/src/codec/anthropic.rs
git commit -m "feat(ox-gate): codec::anthropic::decode_request for inbound translation"
```

### Task 2.3: `SseEncoder` (in `ox-gate::codec::sse_encoder`)

**Files:**
- Create: `crates/ox-gate/src/codec/sse_encoder.rs`
- Modify: `crates/ox-gate/src/codec/mod.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/ox-gate/src/codec/sse_encoder.rs`:

```rust
//! Encode internal StreamEvents back into wire SSE (dialect-aware).
//!
//! Sans-IO: holds per-stream dialect state (OpenAI's tool-call counter,
//! Anthropic's content_block index, etc.) and emits one or more wire
//! lines per event. Returns None when the dialect has no projection
//! for the event.

use ox_types::StreamEvent;

pub struct SseEncoder {
    dialect: String,
    // Anthropic state
    next_content_block: usize,
    open_text_block: Option<usize>,
    open_tool_block: Option<usize>,
    // OpenAI state
    openai_message_started: bool,
    openai_tool_index: HashMap<String, u32>,
}

use std::collections::HashMap;

impl SseEncoder {
    pub fn new(dialect: &str) -> Self {
        Self {
            dialect: dialect.to_string(),
            next_content_block: 0,
            open_text_block: None,
            open_tool_block: None,
            openai_message_started: false,
            openai_tool_index: HashMap::new(),
        }
    }

    /// Encode one event into zero or more wire SSE *frames* (each frame
    /// is a complete `event: ...\ndata: ...\n\n` block as a single
    /// String). Caller writes each into the SSE response body.
    pub fn encode_sse(&mut self, event: &StreamEvent) -> Vec<String> {
        match self.dialect.as_str() {
            "anthropic" => self.encode_anthropic(event),
            "openai" => self.encode_openai(event),
            _ => vec![],
        }
    }

    /// Final closing frame the dialect requires. `data: [DONE]\n\n` for
    /// OpenAI; nothing for Anthropic (message_stop already closes).
    pub fn finish(&mut self) -> Vec<String> {
        match self.dialect.as_str() {
            "openai" => vec!["data: [DONE]\n\n".into()],
            _ => vec![],
        }
    }

    fn encode_anthropic(&mut self, event: &StreamEvent) -> Vec<String> {
        match event {
            StreamEvent::InputUsage { input_tokens, cache_creation, cache_read } => {
                let frame = serde_json::json!({
                    "type": "message_start",
                    "message": {
                        "type": "message",
                        "role": "assistant",
                        "content": [],
                        "usage": {
                            "input_tokens": input_tokens,
                            "cache_creation_input_tokens": cache_creation,
                            "cache_read_input_tokens": cache_read,
                            "output_tokens": 0,
                        },
                    },
                });
                vec![format!("event: message_start\ndata: {}\n\n", frame)]
            }
            StreamEvent::TextDelta { text } => {
                let mut out = Vec::new();
                if self.open_text_block.is_none() {
                    let idx = self.next_content_block;
                    self.next_content_block += 1;
                    self.open_text_block = Some(idx);
                    out.push(format!(
                        "event: content_block_start\ndata: {}\n\n",
                        serde_json::json!({
                            "type": "content_block_start",
                            "index": idx,
                            "content_block": { "type": "text", "text": "" }
                        })
                    ));
                }
                let idx = self.open_text_block.unwrap();
                out.push(format!(
                    "event: content_block_delta\ndata: {}\n\n",
                    serde_json::json!({
                        "type": "content_block_delta",
                        "index": idx,
                        "delta": { "type": "text_delta", "text": text }
                    })
                ));
                out
            }
            StreamEvent::ToolUseStart { id, name } => {
                let mut out = Vec::new();
                // Close any open text block first
                if let Some(text_idx) = self.open_text_block.take() {
                    out.push(format!(
                        "event: content_block_stop\ndata: {}\n\n",
                        serde_json::json!({ "type": "content_block_stop", "index": text_idx })
                    ));
                }
                let idx = self.next_content_block;
                self.next_content_block += 1;
                self.open_tool_block = Some(idx);
                out.push(format!(
                    "event: content_block_start\ndata: {}\n\n",
                    serde_json::json!({
                        "type": "content_block_start",
                        "index": idx,
                        "content_block": { "type": "tool_use", "id": id, "name": name, "input": {} }
                    })
                ));
                out
            }
            StreamEvent::ToolUseInputDelta { delta } => {
                let idx = match self.open_tool_block {
                    Some(i) => i,
                    None => return vec![],
                };
                vec![format!(
                    "event: content_block_delta\ndata: {}\n\n",
                    serde_json::json!({
                        "type": "content_block_delta",
                        "index": idx,
                        "delta": { "type": "input_json_delta", "partial_json": delta }
                    })
                )]
            }
            StreamEvent::OutputUsage { output_tokens } => {
                vec![format!(
                    "event: message_delta\ndata: {}\n\n",
                    serde_json::json!({
                        "type": "message_delta",
                        "delta": { "stop_reason": "end_turn" },
                        "usage": { "output_tokens": output_tokens }
                    })
                )]
            }
            StreamEvent::MessageStop => {
                let mut out = Vec::new();
                if let Some(idx) = self.open_text_block.take() {
                    out.push(format!(
                        "event: content_block_stop\ndata: {}\n\n",
                        serde_json::json!({ "type": "content_block_stop", "index": idx })
                    ));
                }
                if let Some(idx) = self.open_tool_block.take() {
                    out.push(format!(
                        "event: content_block_stop\ndata: {}\n\n",
                        serde_json::json!({ "type": "content_block_stop", "index": idx })
                    ));
                }
                out.push(format!(
                    "event: message_stop\ndata: {}\n\n",
                    serde_json::json!({ "type": "message_stop" })
                ));
                out
            }
            StreamEvent::Error { message } => {
                vec![format!(
                    "event: error\ndata: {}\n\n",
                    serde_json::json!({
                        "type": "error",
                        "error": { "type": "api_error", "message": message }
                    })
                )]
            }
        }
    }

    fn encode_openai(&mut self, event: &StreamEvent) -> Vec<String> {
        // Reusable preamble: every chunk needs a stable id + model. The
        // gateway populates these via a follow-up wrapper; for v1 use
        // stable placeholders that the wrapper overrides at flush time.
        let chunk = |delta: serde_json::Value, finish_reason: Option<&str>| {
            serde_json::json!({
                "id": "chatcmpl-stub",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": "stub",
                "choices": [{
                    "index": 0,
                    "delta": delta,
                    "finish_reason": finish_reason,
                }],
            })
        };

        match event {
            StreamEvent::TextDelta { text } => {
                let mut delta = serde_json::json!({ "content": text });
                if !self.openai_message_started {
                    delta["role"] = serde_json::Value::String("assistant".into());
                    self.openai_message_started = true;
                }
                vec![format!("data: {}\n\n", chunk(delta, None))]
            }
            StreamEvent::ToolUseStart { id, name } => {
                let next = self.openai_tool_index.len() as u32;
                self.openai_tool_index.insert(id.clone(), next);
                let delta = serde_json::json!({
                    "tool_calls": [{
                        "index": next,
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": "" },
                    }]
                });
                vec![format!("data: {}\n\n", chunk(delta, None))]
            }
            StreamEvent::ToolUseInputDelta { delta } => {
                // Without an id, append to the most recent tool index.
                let idx = self.openai_tool_index.len().saturating_sub(1) as u32;
                let payload = serde_json::json!({
                    "tool_calls": [{ "index": idx, "function": { "arguments": delta } }]
                });
                vec![format!("data: {}\n\n", chunk(payload, None))]
            }
            StreamEvent::MessageStop => {
                vec![format!("data: {}\n\n", chunk(serde_json::json!({}), Some("stop")))]
            }
            StreamEvent::InputUsage { .. } => {
                // OpenAI emits usage only on the final non-streamed-delta chunk.
                // Buffer here, emit on MessageStop in a future refinement; for
                // v1 emit a usage chunk immediately.
                let usage = match event {
                    StreamEvent::InputUsage { input_tokens, cache_read, .. } => {
                        serde_json::json!({
                            "prompt_tokens": input_tokens,
                            "prompt_tokens_details": { "cached_tokens": cache_read },
                        })
                    }
                    _ => unreachable!(),
                };
                vec![format!(
                    "data: {}\n\n",
                    serde_json::json!({
                        "id": "chatcmpl-stub", "object": "chat.completion.chunk",
                        "created": 0, "model": "stub", "choices": [],
                        "usage": usage,
                    })
                )]
            }
            StreamEvent::OutputUsage { output_tokens } => {
                vec![format!(
                    "data: {}\n\n",
                    serde_json::json!({
                        "id": "chatcmpl-stub", "object": "chat.completion.chunk",
                        "created": 0, "model": "stub", "choices": [],
                        "usage": { "completion_tokens": output_tokens },
                    })
                )]
            }
            StreamEvent::Error { message } => {
                vec![format!(
                    "data: {}\n\n",
                    serde_json::json!({ "error": { "message": message } })
                )]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_text_delta_emits_block_start_then_delta() {
        let mut enc = SseEncoder::new("anthropic");
        let frames = enc.encode_sse(&StreamEvent::TextDelta { text: "hi".into() });
        assert_eq!(frames.len(), 2);
        assert!(frames[0].contains("content_block_start"));
        assert!(frames[1].contains("content_block_delta"));
        assert!(frames[1].contains("\"text\":\"hi\""));
    }

    #[test]
    fn anthropic_second_text_delta_reuses_block() {
        let mut enc = SseEncoder::new("anthropic");
        let _ = enc.encode_sse(&StreamEvent::TextDelta { text: "a".into() });
        let frames = enc.encode_sse(&StreamEvent::TextDelta { text: "b".into() });
        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("\"text\":\"b\""));
    }

    #[test]
    fn anthropic_message_stop_closes_open_block() {
        let mut enc = SseEncoder::new("anthropic");
        let _ = enc.encode_sse(&StreamEvent::TextDelta { text: "x".into() });
        let frames = enc.encode_sse(&StreamEvent::MessageStop);
        assert_eq!(frames.len(), 2);
        assert!(frames[0].contains("content_block_stop"));
        assert!(frames[1].contains("message_stop"));
    }

    #[test]
    fn openai_first_text_includes_role() {
        let mut enc = SseEncoder::new("openai");
        let frames = enc.encode_sse(&StreamEvent::TextDelta { text: "hi".into() });
        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("\"role\":\"assistant\""));
        assert!(frames[0].contains("\"content\":\"hi\""));
    }

    #[test]
    fn openai_finish_emits_done() {
        let mut enc = SseEncoder::new("openai");
        let frames = enc.finish();
        assert_eq!(frames, vec!["data: [DONE]\n\n"]);
    }
}
```

- [ ] **Step 2: Register the module**

In `crates/ox-gate/src/codec/mod.rs`:

```rust
pub mod sse_encoder;
pub use sse_encoder::SseEncoder;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-gate codec::sse_encoder`
Expected: 5 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-gate/src/codec/sse_encoder.rs crates/ox-gate/src/codec/mod.rs
git commit -m "feat(ox-gate): codec::SseEncoder — internal StreamEvents → wire SSE"
```

### Task 2.4: Anthropic `encode_response` (non-streaming)

**Files:**
- Modify: `crates/ox-gate/src/codec/anthropic.rs`

- [ ] **Step 1: Write failing test**

Append:

```rust
#[cfg(test)]
mod encode_response_tests {
    use super::*;
    use ox_types::StreamEvent;

    #[test]
    fn encode_text_only_response() {
        let events = vec![
            StreamEvent::InputUsage { input_tokens: 10, cache_creation: 0, cache_read: 0 },
            StreamEvent::TextDelta { text: "Hello".into() },
            StreamEvent::TextDelta { text: " world".into() },
            StreamEvent::OutputUsage { output_tokens: 2 },
            StreamEvent::MessageStop,
        ];
        let resp = encode_response(&events);
        assert_eq!(resp["type"], "message");
        assert_eq!(resp["role"], "assistant");
        assert_eq!(resp["content"][0]["type"], "text");
        assert_eq!(resp["content"][0]["text"], "Hello world");
        assert_eq!(resp["usage"]["input_tokens"], 10);
        assert_eq!(resp["usage"]["output_tokens"], 2);
    }

    #[test]
    fn encode_response_with_tool_use() {
        let events = vec![
            StreamEvent::ToolUseStart { id: "t1".into(), name: "read_file".into() },
            StreamEvent::ToolUseInputDelta { delta: r#"{"path":"#.into() },
            StreamEvent::ToolUseInputDelta { delta: r#""/etc/hosts"}"#.into() },
            StreamEvent::OutputUsage { output_tokens: 5 },
            StreamEvent::MessageStop,
        ];
        let resp = encode_response(&events);
        let blocks = resp["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_use");
        assert_eq!(blocks[0]["id"], "t1");
        assert_eq!(blocks[0]["name"], "read_file");
        assert_eq!(blocks[0]["input"]["path"], "/etc/hosts");
    }
}
```

- [ ] **Step 2: Run, confirm fails**

Run: `cargo test -p ox-gate codec::anthropic::encode_response_tests`
Expected: compile error.

- [ ] **Step 3: Implement**

Add to `anthropic.rs`:

```rust
pub fn encode_response(events: &[StreamEvent]) -> serde_json::Value {
    let mut content_blocks: Vec<serde_json::Value> = Vec::new();
    let mut current_text = String::new();
    let mut current_tool: Option<(String, String, String)> = None;
    let mut input_tokens = 0u32;
    let mut cache_creation = 0u32;
    let mut cache_read = 0u32;
    let mut output_tokens = 0u32;

    let flush_text = |blocks: &mut Vec<serde_json::Value>, text: &mut String| {
        if !text.is_empty() {
            blocks.push(serde_json::json!({ "type": "text", "text": text.clone() }));
            text.clear();
        }
    };
    let flush_tool = |blocks: &mut Vec<serde_json::Value>, tool: &mut Option<(String, String, String)>| {
        if let Some((id, name, input_json)) = tool.take() {
            let input = serde_json::from_str::<serde_json::Value>(&input_json)
                .unwrap_or(serde_json::Value::Object(Default::default()));
            blocks.push(serde_json::json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            }));
        }
    };

    for ev in events {
        match ev {
            StreamEvent::TextDelta { text } => {
                flush_tool(&mut content_blocks, &mut current_tool);
                current_text.push_str(text);
            }
            StreamEvent::ToolUseStart { id, name } => {
                flush_text(&mut content_blocks, &mut current_text);
                flush_tool(&mut content_blocks, &mut current_tool);
                current_tool = Some((id.clone(), name.clone(), String::new()));
            }
            StreamEvent::ToolUseInputDelta { delta } => {
                if let Some((_, _, ref mut input_json)) = current_tool {
                    input_json.push_str(delta);
                }
            }
            StreamEvent::InputUsage { input_tokens: it, cache_creation: cc, cache_read: cr } => {
                input_tokens = *it;
                cache_creation = *cc;
                cache_read = *cr;
            }
            StreamEvent::OutputUsage { output_tokens: ot } => {
                output_tokens = *ot;
            }
            StreamEvent::MessageStop | StreamEvent::Error { .. } => {}
        }
    }
    flush_text(&mut content_blocks, &mut current_text);
    flush_tool(&mut content_blocks, &mut current_tool);

    serde_json::json!({
        "type": "message",
        "role": "assistant",
        "content": content_blocks,
        "model": "",  // populated by route handler from CompletionStatus
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": input_tokens,
            "cache_creation_input_tokens": cache_creation,
            "cache_read_input_tokens": cache_read,
            "output_tokens": output_tokens,
        }
    })
}
```

- [ ] **Step 4: Run, confirm passes**

Run: `cargo test -p ox-gate codec::anthropic::encode_response_tests`
Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-gate/src/codec/anthropic.rs
git commit -m "feat(ox-gate): codec::anthropic::encode_response for non-streaming responses"
```

### Task 2.5: OpenAI `decode_request`

**Files:**
- Modify: `crates/ox-gate/src/codec/openai.rs`

- [ ] **Step 1: Write failing tests**

Append to `crates/ox-gate/src/codec/openai.rs`:

```rust
#[cfg(test)]
mod decode_tests {
    use super::*;
    use crate::codec::CodecError;

    #[test]
    fn decode_minimal_request() {
        let body = serde_json::json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(req.model, "gpt-4o-mini");
        assert_eq!(req.messages.len(), 1);
        // OpenAI default max_tokens is provider-specific; gateway picks
        // a sensible value when caller omits it.
        assert!(req.max_tokens > 0);
    }

    #[test]
    fn decode_extracts_system_from_messages() {
        // OpenAI puts system in the messages array; internal CompletionRequest
        // has a dedicated `system` field. Promote any leading system role.
        let body = serde_json::json!({
            "model": "m",
            "messages": [
                {"role": "system", "content": "be helpful"},
                {"role": "user", "content": "hi"}
            ]
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(req.system, "be helpful");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0]["role"], "user");
    }

    #[test]
    fn decode_max_tokens_when_provided() {
        let body = serde_json::json!({
            "model": "m",
            "max_tokens": 512,
            "messages": []
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(req.max_tokens, 512);
    }

    #[test]
    fn missing_model_errors() {
        let body = serde_json::json!({"messages": []});
        assert_eq!(decode_request(&body).unwrap_err(), CodecError::MissingField("model"));
    }

    #[test]
    fn decode_translates_tools() {
        let body = serde_json::json!({
            "model": "m",
            "messages": [],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "Read a file",
                    "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}
                }
            }]
        });
        let req = decode_request(&body).unwrap();
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "read_file");
        assert_eq!(req.tools[0].description, "Read a file");
    }
}
```

- [ ] **Step 2: Run, confirm fails**

Run: `cargo test -p ox-gate codec::openai::decode_tests`
Expected: compile error.

- [ ] **Step 3: Implement**

Add to `openai.rs`:

```rust
use crate::codec::CodecError;
use ox_kernel::{CompletionRequest, ToolSchema};

pub fn decode_request(body: &serde_json::Value) -> Result<CompletionRequest, CodecError> {
    let obj = body.as_object().ok_or(CodecError::InvalidShape("body must be a JSON object".into()))?;

    let model = obj.get("model").and_then(|v| v.as_str())
        .ok_or(CodecError::MissingField("model"))?
        .to_string();

    // OpenAI default; user can override.
    let max_tokens = obj.get("max_tokens").and_then(|v| v.as_u64()).unwrap_or(4096) as u32;

    let stream = obj.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    let raw_messages = obj.get("messages").and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut system = String::new();
    let mut messages = Vec::new();
    for m in raw_messages {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role == "system" {
            let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if !system.is_empty() {
                system.push_str("\n\n");
            }
            system.push_str(content);
        } else {
            messages.push(m);
        }
    }

    let tools: Vec<ToolSchema> = obj.get("tools").and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let f = t.get("function")?;
                    let name = f.get("name")?.as_str()?.to_string();
                    let description = f.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let input_schema = f.get("parameters").cloned().unwrap_or(serde_json::Value::Null);
                    Some(ToolSchema { name, description, input_schema })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(CompletionRequest { model, max_tokens, system, messages, tools, stream })
}
```

- [ ] **Step 4: Run, confirm passes**

Run: `cargo test -p ox-gate codec::openai::decode_tests`
Expected: 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-gate/src/codec/openai.rs
git commit -m "feat(ox-gate): codec::openai::decode_request for inbound translation"
```

### Task 2.6: OpenAI `encode_response` (non-streaming)

**Files:**
- Modify: `crates/ox-gate/src/codec/openai.rs`

- [ ] **Step 1: Write failing test**

Append:

```rust
#[cfg(test)]
mod encode_response_tests {
    use super::*;
    use ox_types::StreamEvent;

    #[test]
    fn encode_text_only_chat_completion() {
        let events = vec![
            StreamEvent::TextDelta { text: "Hello".into() },
            StreamEvent::TextDelta { text: " world".into() },
            StreamEvent::InputUsage { input_tokens: 10, cache_creation: 0, cache_read: 0 },
            StreamEvent::OutputUsage { output_tokens: 2 },
            StreamEvent::MessageStop,
        ];
        let resp = encode_response(&events);
        assert_eq!(resp["object"], "chat.completion");
        let choices = resp["choices"].as_array().unwrap();
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0]["message"]["role"], "assistant");
        assert_eq!(choices[0]["message"]["content"], "Hello world");
        assert_eq!(choices[0]["finish_reason"], "stop");
        assert_eq!(resp["usage"]["prompt_tokens"], 10);
        assert_eq!(resp["usage"]["completion_tokens"], 2);
        assert_eq!(resp["usage"]["total_tokens"], 12);
    }
}
```

- [ ] **Step 2: Implement**

```rust
pub fn encode_response(events: &[StreamEvent]) -> serde_json::Value {
    let mut content = String::new();
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
    let mut current_tool: Option<(String, String, String)> = None;
    let mut input_tokens = 0u32;
    let mut output_tokens = 0u32;
    let mut cache_read = 0u32;

    for ev in events {
        match ev {
            StreamEvent::TextDelta { text } => content.push_str(text),
            StreamEvent::ToolUseStart { id, name } => {
                if let Some((tid, tname, args)) = current_tool.take() {
                    tool_calls.push(serde_json::json!({
                        "id": tid, "type": "function",
                        "function": { "name": tname, "arguments": args },
                    }));
                }
                current_tool = Some((id.clone(), name.clone(), String::new()));
            }
            StreamEvent::ToolUseInputDelta { delta } => {
                if let Some((_, _, ref mut args)) = current_tool {
                    args.push_str(delta);
                }
            }
            StreamEvent::InputUsage { input_tokens: it, cache_read: cr, .. } => {
                input_tokens = *it;
                cache_read = *cr;
            }
            StreamEvent::OutputUsage { output_tokens: ot } => output_tokens = *ot,
            StreamEvent::MessageStop | StreamEvent::Error { .. } => {}
        }
    }
    if let Some((id, name, args)) = current_tool.take() {
        tool_calls.push(serde_json::json!({
            "id": id, "type": "function",
            "function": { "name": name, "arguments": args },
        }));
    }

    let mut message = serde_json::json!({ "role": "assistant" });
    if !content.is_empty() {
        message["content"] = serde_json::Value::String(content);
    }
    if !tool_calls.is_empty() {
        message["tool_calls"] = serde_json::Value::Array(tool_calls);
    }

    let finish_reason = if !message.get("tool_calls").is_none() { "tool_calls" } else { "stop" };

    let mut usage = serde_json::json!({
        "prompt_tokens": input_tokens,
        "completion_tokens": output_tokens,
        "total_tokens": input_tokens + output_tokens,
    });
    if cache_read > 0 {
        usage["prompt_tokens_details"] = serde_json::json!({ "cached_tokens": cache_read });
    }

    serde_json::json!({
        "id": "chatcmpl-stub",
        "object": "chat.completion",
        "created": 0,
        "model": "",
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": usage,
    })
}
```

- [ ] **Step 3: Run, confirm passes**

Run: `cargo test -p ox-gate codec::openai::encode_response_tests`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-gate/src/codec/openai.rs
git commit -m "feat(ox-gate): codec::openai::encode_response for non-streaming responses"
```

### Task 2.7: `SseHttpExecutor` trait + `ReqwestSseExecutor`

**Files:**
- Modify: `crates/ox-gate/src/transport.rs`
- Modify: `crates/ox-gate/Cargo.toml` (add `futures = { workspace = true }` if not present)

- [ ] **Step 1: Add `futures` dep**

In `crates/ox-gate/Cargo.toml` dependencies:

```toml
futures = { workspace = true }
```

If `futures` isn't in the workspace's `[workspace.dependencies]` table, add it there too with a version (e.g. `futures = "0.3"`).

- [ ] **Step 2: Add trait + impl skeleton**

Append to `crates/ox-gate/src/transport.rs`:

```rust
use futures::stream::BoxStream;
use ox_types::StreamEvent;
use structfs_http::HttpRequest;

/// Streaming HTTP executor — sibling of structfs_http::HttpExecutor for
/// SSE responses. Generic injection point for CompletionBrokerStore.
#[async_trait::async_trait]
pub trait SseHttpExecutor: Send + Sync + 'static {
    /// Send the request and return a stream of typed StreamEvents.
    /// The executor is responsible for parsing wire SSE into the
    /// internal event shape (via the dialect-aware SseParser).
    async fn execute(
        &self,
        request: HttpRequest,
        dialect: String,
    ) -> BoxStream<'static, Result<StreamEvent, String>>;
}

/// Production SseHttpExecutor backed by reqwest.
pub struct ReqwestSseExecutor {
    client: reqwest::Client,
    timeout: std::time::Duration,
}

impl ReqwestSseExecutor {
    pub fn new(timeout: std::time::Duration) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client, timeout })
    }

    pub fn with_default_timeout() -> Result<Self, String> {
        Self::new(std::time::Duration::from_secs(300))
    }
}

#[async_trait::async_trait]
impl SseHttpExecutor for ReqwestSseExecutor {
    async fn execute(
        &self,
        request: HttpRequest,
        dialect: String,
    ) -> BoxStream<'static, Result<StreamEvent, String>> {
        use futures::stream::StreamExt;
        let client = self.client.clone();

        Box::pin(async_stream::stream! {
            let method = match request.method {
                structfs_http::Method::POST => reqwest::Method::POST,
                structfs_http::Method::GET => reqwest::Method::GET,
                m => { yield Err(format!("unsupported method: {:?}", m)); return; }
            };

            let mut req = client.request(method, &request.path);
            for (k, v) in &request.headers {
                req = req.header(k, v);
            }
            if let Some(body) = &request.body {
                req = req.json(body);
            }

            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => { yield Err(format!("network error: {e}")); return; }
            };

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                yield Err(format!("HTTP {} from upstream: {}", status, body));
                return;
            }

            let mut stream = resp.bytes_stream();
            let mut buf = String::new();
            let mut parser = SseParser::new(&dialect);

            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(b) => b,
                    Err(e) => { yield Err(format!("read error: {e}")); return; }
                };
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(nl) = buf.find('\n') {
                    let line = buf[..nl].to_string();
                    buf.drain(..=nl);
                    for ev in parser.feed(&line) {
                        yield Ok(ev);
                    }
                }
            }
            // Flush any trailing line
            if !buf.is_empty() {
                for ev in parser.feed(&buf) {
                    yield Ok(ev);
                }
            }
        })
    }
}
```

Add `async_stream = { workspace = true }` to ox-gate's deps if not present.

- [ ] **Step 3: Quick smoke test (unit, no network)**

```rust
#[cfg(test)]
mod sse_executor_tests {
    use super::*;

    #[test]
    fn executor_constructs_with_default_timeout() {
        let e = ReqwestSseExecutor::with_default_timeout();
        assert!(e.is_ok());
    }
}
```

Run: `cargo test -p ox-gate sse_executor_tests`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-gate/
git commit -m "feat(ox-gate): SseHttpExecutor trait + ReqwestSseExecutor impl"
```

### Task 2.8: `JsonlFileBacking` in `ox-store-util`

**Files:**
- Create: `crates/ox-store-util/src/jsonl_file_backing.rs`
- Modify: `crates/ox-store-util/src/lib.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/ox-store-util/src/jsonl_file_backing.rs`:

```rust
//! Append-only JSON Lines file backing for StoreBacking.
//!
//! Each `save` call appends one JSON line. `load` slurps the whole
//! file into an array Value. Crash-safe by truncation: a partially
//! written line at the end of the file is skipped on load.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use structfs_core_store::{Error as StoreError, Value};

use crate::StoreBacking;

pub struct JsonlFileBacking {
    path: PathBuf,
}

impl JsonlFileBacking {
    pub fn new(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self { path })
    }

    pub fn append(&self, value: &Value) -> Result<(), StoreError> {
        let json = structfs_serde_store::value_to_json(value.clone());
        let mut line = serde_json::to_string(&json)
            .map_err(|e| StoreError::store("jsonl", "append", e.to_string()))?;
        line.push('\n');
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| StoreError::store("jsonl", "open", e.to_string()))?;
        f.write_all(line.as_bytes())
            .map_err(|e| StoreError::store("jsonl", "write", e.to_string()))?;
        Ok(())
    }
}

impl StoreBacking for JsonlFileBacking {
    fn load(&self) -> Result<Option<Value>, StoreError> {
        let f = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Some(Value::Array(vec![])));
            }
            Err(e) => return Err(StoreError::store("jsonl", "open", e.to_string())),
        };
        let reader = BufReader::new(f);
        let mut arr = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(|e| StoreError::store("jsonl", "read", e.to_string()))?;
            if line.trim().is_empty() {
                continue;
            }
            let json: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,  // skip partial/corrupt trailing lines
            };
            arr.push(structfs_serde_store::json_to_value(json));
        }
        Ok(Some(Value::Array(arr)))
    }

    fn save(&self, _value: &Value) -> Result<(), StoreError> {
        // JsonlFileBacking is append-only; whole-value save is not
        // supported. UsageStore writes via the explicit `append`
        // method on the concrete type, never through StoreBacking::save.
        Err(StoreError::store(
            "jsonl",
            "save",
            "JsonlFileBacking is append-only; use .append() directly",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let backing = JsonlFileBacking::new(&path).unwrap();

        backing.append(&Value::String("first".into())).unwrap();
        backing.append(&Value::String("second".into())).unwrap();

        let loaded = backing.load().unwrap().unwrap();
        let arr = match loaded {
            Value::Array(a) => a,
            _ => panic!("expected array"),
        };
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0], Value::String("first".into()));
        assert_eq!(arr[1], Value::String("second".into()));
    }

    #[test]
    fn load_missing_file_returns_empty_array() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.jsonl");
        let backing = JsonlFileBacking::new(&path).unwrap();
        let loaded = backing.load().unwrap().unwrap();
        match loaded {
            Value::Array(a) => assert!(a.is_empty()),
            _ => panic!("expected empty array"),
        }
    }

    #[test]
    fn load_skips_corrupt_trailing_line() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partial.jsonl");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "{}", serde_json::json!({"ok": true})).unwrap();
            write!(f, "{{not-json-and-no-newline").unwrap();
        }
        let backing = JsonlFileBacking::new(&path).unwrap();
        let loaded = backing.load().unwrap().unwrap();
        let arr = match loaded {
            Value::Array(a) => a,
            _ => panic!("expected array"),
        };
        assert_eq!(arr.len(), 1);
    }
}
```

- [ ] **Step 2: Register the module**

In `crates/ox-store-util/src/lib.rs`:

```rust
pub mod jsonl_file_backing;
pub use jsonl_file_backing::JsonlFileBacking;
```

- [ ] **Step 3: Confirm `tempfile` is a dev-dep**

Run: `grep tempfile crates/ox-store-util/Cargo.toml`
If not present, add to `[dev-dependencies]`: `tempfile = { workspace = true }`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-store-util jsonl_file_backing`
Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-store-util/
git commit -m "feat(ox-store-util): JsonlFileBacking — append-only StoreBacking for ledgers"
```

---

## Phase 3: `UsageStore` + `CompletionBrokerStore`

### Task 3.1: `UsageRecord` + `UsageStore`

**Files:**
- Create: `crates/ox-gate/src/usage_store.rs`
- Modify: `crates/ox-gate/src/lib.rs`

- [ ] **Step 1: Write failing test**

Create `crates/ox-gate/src/usage_store.rs`:

```rust
//! Usage ledger as a StructFS mount. Append-only writes, projection
//! reads. Backed by JsonlFileBacking (or any StoreBacking) so tests
//! can swap in an in-memory implementation.

use serde::{Deserialize, Serialize};
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};
use structfs_serde_store::{from_value, to_value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageRecord {
    pub id: String,
    pub account: String,
    pub model_id: String,
    pub dialect: String,
    pub upstream_dialect: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_creation_input_tokens: u32,
    pub cache_read_input_tokens: u32,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub estimated_cost_usd: Option<f64>,
}

pub struct UsageStore {
    backing: Box<dyn ox_store_util::StoreBacking + Send + Sync>,
}

impl UsageStore {
    pub fn new(backing: Box<dyn ox_store_util::StoreBacking + Send + Sync>) -> Self {
        Self { backing }
    }

    fn append(&self, record: &UsageRecord) -> Result<(), StoreError> {
        let value = to_value(record).map_err(|e| StoreError::store("usage", "append", e.to_string()))?;
        // Use the concrete JsonlFileBacking::append if backing is that type;
        // otherwise StoreBacking::save will error (jsonl rejects full-save).
        if let Some(jsonl) = (self.backing.as_ref() as &dyn std::any::Any)
            .downcast_ref::<ox_store_util::JsonlFileBacking>()
        {
            return jsonl.append(&value);
        }
        // Fallback for tests using a different backing: read full ledger,
        // append, save (only works for backings that accept save).
        let mut current: Vec<UsageRecord> = match self.backing.load()? {
            Some(Value::Array(arr)) => arr.into_iter().filter_map(|v| from_value(v).ok()).collect(),
            _ => vec![],
        };
        current.push(record.clone());
        let value = to_value(&current).map_err(|e| StoreError::store("usage", "save", e.to_string()))?;
        self.backing.save(&value)?;
        Ok(())
    }

    fn load_all(&self) -> Result<Vec<UsageRecord>, StoreError> {
        let value = self.backing.load()?.unwrap_or(Value::Array(vec![]));
        let arr = match value {
            Value::Array(a) => a,
            _ => return Ok(vec![]),
        };
        Ok(arr.into_iter().filter_map(|v| from_value(v).ok()).collect())
    }
}

impl Reader for UsageStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        if from.is_empty() {
            let records = self.load_all()?;
            let value = to_value(&records).map_err(|e| StoreError::store("usage", "read", e.to_string()))?;
            return Ok(Some(Record::parsed(value)));
        }
        match from.components.first().map(|c| c.as_str()) {
            Some("today") => {
                let records = self.load_all()?;
                let start_of_today_ms = start_of_today_ms();
                let total: TodayProjection = records.iter()
                    .filter(|r| r.completed_at_ms >= start_of_today_ms)
                    .fold(TodayProjection::default(), |mut acc, r| {
                        acc.count += 1;
                        acc.input_tokens += r.input_tokens as u64;
                        acc.output_tokens += r.output_tokens as u64;
                        if let Some(c) = r.estimated_cost_usd {
                            acc.estimated_cost_usd += c;
                        }
                        acc
                    });
                let value = to_value(&total).map_err(|e| StoreError::store("usage", "read", e.to_string()))?;
                Ok(Some(Record::parsed(value)))
            }
            _ => Ok(None),
        }
    }
}

impl Writer for UsageStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        if to.components.first().map(|c| c.as_str()) != Some("append") {
            return Err(StoreError::store("usage", "write", "only 'append' path supported"));
        }
        let value = data.as_value().ok_or_else(|| StoreError::store("usage", "write", "expected parsed record"))?;
        let record: UsageRecord = from_value(value.clone())
            .map_err(|e| StoreError::store("usage", "write", e.to_string()))?;
        self.append(&record)?;
        Ok(to.clone())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TodayProjection {
    count: u64,
    input_tokens: u64,
    output_tokens: u64,
    estimated_cost_usd: f64,
}

fn start_of_today_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    let day = 86_400_000u64;
    (now / day) * day
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_store_util::JsonlFileBacking;

    fn sample_record(id: &str) -> UsageRecord {
        UsageRecord {
            id: id.into(),
            account: "anthropic".into(),
            model_id: "claude-sonnet-4-20250514".into(),
            dialect: "anthropic".into(),
            upstream_dialect: "anthropic".into(),
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            started_at_ms: 1_000_000,
            completed_at_ms: 1_001_000,
            estimated_cost_usd: Some(0.0015),
        }
    }

    #[test]
    fn append_and_read_full_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.jsonl");
        let backing = Box::new(JsonlFileBacking::new(&path).unwrap());
        let mut store = UsageStore::new(backing);

        let r = sample_record("a");
        let v = to_value(&r).unwrap();
        store.write(
            &structfs_core_store::path!("append"),
            Record::parsed(v),
        ).unwrap();

        let read = store.read(&structfs_core_store::path!("")).unwrap().unwrap();
        let value = read.as_value().unwrap();
        let records: Vec<UsageRecord> = from_value(value.clone()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "a");
    }
}
```

- [ ] **Step 2: Register the module + add deps**

In `crates/ox-gate/src/lib.rs`:

```rust
#[cfg(not(target_arch = "wasm32"))]
pub mod usage_store;
#[cfg(not(target_arch = "wasm32"))]
pub use usage_store::{UsageRecord, UsageStore};
```

In `crates/ox-gate/Cargo.toml` add to `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`:

```toml
ox-store-util = { workspace = true }
```

And `[dev-dependencies]` (already present from earlier):

```toml
tempfile = { workspace = true }
```

- [ ] **Step 3: Run test**

Run: `cargo test -p ox-gate usage_store`
Expected: 1 test passes.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-gate/
git commit -m "feat(ox-gate): UsageStore — append-only ledger over StoreBacking"
```

### Task 3.2: `CompletionBrokerStore` — types + skeleton

**Files:**
- Create: `crates/ox-gate/src/completion_broker/mod.rs`
- Create: `crates/ox-gate/src/completion_broker/inflight.rs`
- Modify: `crates/ox-gate/src/lib.rs`

- [ ] **Step 1: Inflight types**

Create `crates/ox-gate/src/completion_broker/inflight.rs`:

```rust
use ox_kernel::CompletionRequest;
use ox_types::StreamEvent;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

use crate::codec::UsageInfo;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum CompletionStatus {
    Pending,
    Streaming { account: String, model_id: String, started_at_ms: u64 },
    Complete  { account: String, model_id: String, completed_at_ms: u64 },
    Failed    { account: String, model_id: String, reason: String, failed_at_ms: u64 },
}

impl CompletionStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete { .. } | Self::Failed { .. })
    }
}

pub struct Inflight {
    pub state: Mutex<InflightState>,
    pub notify: Notify,
}

impl Inflight {
    pub fn new(request: CompletionRequest) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(InflightState {
                request,
                events: Vec::new(),
                status: CompletionStatus::Pending,
                usage: None,
            }),
            notify: Notify::new(),
        })
    }
}

pub struct InflightState {
    pub request: CompletionRequest,
    pub events: Vec<StreamEvent>,
    pub status: CompletionStatus,
    pub usage: Option<UsageInfo>,
}
```

- [ ] **Step 2: Module skeleton**

Create `crates/ox-gate/src/completion_broker/mod.rs`:

```rust
//! CompletionBrokerStore — substrate-mediated LLM completion dispatch.
//!
//! Modeled on structfs_http::HttpBrokerStore, generalized for streaming:
//!   write /                      CompletionRequest → outstanding/{N}
//!   read  outstanding/{N}        CompletionStatus
//!   read  outstanding/{N}/events/from/{S}   Vec<StreamEvent> (blocking)
//!   read  outstanding/{N}/usage  UsageInfo (None until Complete)
//!   write outstanding/{N} null   GC

mod dispatch;
mod inflight;
#[cfg(test)]
mod mock;

pub use inflight::CompletionStatus;
use inflight::{Inflight, InflightState};

use std::collections::HashMap;
use std::sync::Arc;

use ox_broker::ClientHandle;
use ox_kernel::CompletionRequest;
use structfs_core_store::{Error as StoreError, Path, Record, Value};
use structfs_serde_store::{from_value, to_value};
use tokio::runtime::Handle as TokioHandle;

use crate::transport::SseHttpExecutor;

pub type RequestId = u64;

pub struct CompletionBrokerStore<E: SseHttpExecutor> {
    substrate: ClientHandle,
    executor: Arc<E>,
    handles: HashMap<RequestId, Arc<Inflight>>,
    next_request_id: RequestId,
    usage_writer: ClientHandle,
    runtime: TokioHandle,
}

impl<E: SseHttpExecutor> CompletionBrokerStore<E> {
    pub fn new(
        substrate: ClientHandle,
        executor: Arc<E>,
        usage_writer: ClientHandle,
        runtime: TokioHandle,
    ) -> Self {
        Self {
            substrate,
            executor,
            handles: HashMap::new(),
            next_request_id: 0,
            usage_writer,
            runtime,
        }
    }

    fn parse_handle_path(path: &Path) -> Option<(RequestId, Option<String>)> {
        if path.components.is_empty() || path.components[0].as_str() != "outstanding" {
            return None;
        }
        if path.components.len() == 1 {
            return None;
        }
        let id: RequestId = path.components[1].as_str().parse().ok()?;
        let sub = if path.components.len() > 2 {
            Some(path.components[2..].iter().map(|c| c.as_str().to_string()).collect::<Vec<_>>().join("/"))
        } else {
            None
        };
        Some((id, sub))
    }
}
```

- [ ] **Step 3: Empty dispatch module**

Create `crates/ox-gate/src/completion_broker/dispatch.rs`:

```rust
// Implemented in Task 3.4.
```

- [ ] **Step 4: Register in lib.rs**

In `crates/ox-gate/src/lib.rs`:

```rust
#[cfg(not(target_arch = "wasm32"))]
pub mod completion_broker;
#[cfg(not(target_arch = "wasm32"))]
pub use completion_broker::{CompletionBrokerStore, CompletionStatus, RequestId};
```

- [ ] **Step 5: Build**

Run: `cargo build -p ox-gate`
Expected: compiles.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-gate/
git commit -m "feat(ox-gate): CompletionBrokerStore skeleton + Inflight/CompletionStatus types"
```

### Task 3.3: `MockSseExecutor` for testing

**Files:**
- Create: `crates/ox-gate/src/completion_broker/mock.rs`

- [ ] **Step 1: Implement the mock**

```rust
use crate::transport::SseHttpExecutor;
use futures::stream::BoxStream;
use ox_types::StreamEvent;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use structfs_http::HttpRequest;

/// Test executor that yields a pre-programmed sequence of events with
/// optional inter-event delays.
pub struct MockSseExecutor {
    /// Sequence of (delay_before_emit, event-or-error) tuples.
    pub script: Mutex<Vec<(Duration, Result<StreamEvent, String>)>>,
    pub requests_seen: Arc<Mutex<Vec<HttpRequest>>>,
}

impl MockSseExecutor {
    pub fn new() -> Self {
        Self {
            script: Mutex::new(Vec::new()),
            requests_seen: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn push(&self, delay: Duration, event: Result<StreamEvent, String>) {
        self.script.lock().unwrap().push((delay, event));
    }

    pub fn push_immediate(&self, event: StreamEvent) {
        self.push(Duration::ZERO, Ok(event));
    }
}

#[async_trait::async_trait]
impl SseHttpExecutor for MockSseExecutor {
    async fn execute(
        &self,
        request: HttpRequest,
        _dialect: String,
    ) -> BoxStream<'static, Result<StreamEvent, String>> {
        self.requests_seen.lock().unwrap().push(request);
        let script = std::mem::take(&mut *self.script.lock().unwrap());
        Box::pin(async_stream::stream! {
            for (delay, ev) in script {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                yield ev;
            }
        })
    }
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p ox-gate --tests`
Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add crates/ox-gate/src/completion_broker/mock.rs
git commit -m "test(ox-gate): MockSseExecutor for completion_broker integration tests"
```

### Task 3.4: `CompletionBrokerStore` — `AsyncWriter` (the write trigger)

**Files:**
- Modify: `crates/ox-gate/src/completion_broker/dispatch.rs`
- Modify: `crates/ox-gate/src/completion_broker/mod.rs`

- [ ] **Step 1: Implement `per_request_task` (resolution + dispatch)**

In `crates/ox-gate/src/completion_broker/dispatch.rs`:

```rust
use crate::codec::UsageInfo;
use crate::completion_broker::inflight::{CompletionStatus, Inflight, InflightState};
use crate::transport::SseHttpExecutor;
use crate::{AccountConfig, ApiKey, ProviderConfig};
use ox_broker::ClientHandle;
use ox_kernel::CompletionRequest;
use ox_path::oxpath;
use ox_types::{CompletionRole, StreamEvent};
use std::sync::Arc;
use structfs_core_store::{path, Path, Record};
use structfs_serde_store::to_value;
use ulid::Ulid;

use futures::StreamExt;

pub async fn per_request_task<E: SseHttpExecutor>(
    inflight: Arc<Inflight>,
    substrate: ClientHandle,
    executor: Arc<E>,
    usage_writer: ClientHandle,
    request_id: u64,
) {
    let request = {
        let state = inflight.state.lock().await;
        state.request.clone()
    };

    // (a) Resolve model → CompletionRole
    let role = match resolve_model(&request.model, &substrate).await {
        Ok(r) => r,
        Err(reason) => {
            mark_failed(&inflight, "(unknown)".into(), request.model.clone(), reason).await;
            return;
        }
    };

    // (b) Resolve account/provider/key
    let (account_cfg, provider_cfg, api_key) = match resolve_account(&role, &substrate).await {
        Ok(t) => t,
        Err(reason) => {
            mark_failed(&inflight, role.account.clone(), role.model_id.clone(), reason).await;
            return;
        }
    };

    // (c) Flip to Streaming
    let started_at_ms = now_ms();
    {
        let mut state = inflight.state.lock().await;
        state.status = CompletionStatus::Streaming {
            account: role.account.clone(),
            model_id: role.model_id.clone(),
            started_at_ms,
        };
    }
    inflight.notify.notify_waiters();

    // (d) Build HttpRequest + execute
    let http_request = match build_http_request(&provider_cfg, &api_key, &request, &role.model_id) {
        Ok(r) => r,
        Err(reason) => {
            mark_failed(&inflight, role.account.clone(), role.model_id.clone(), reason).await;
            return;
        }
    };

    let mut stream = executor.execute(http_request, provider_cfg.dialect.clone()).await;
    while let Some(item) = stream.next().await {
        match item {
            Ok(ev) => {
                let mut state = inflight.state.lock().await;
                state.events.push(ev);
                drop(state);
                inflight.notify.notify_waiters();
            }
            Err(reason) => {
                mark_failed(&inflight, role.account.clone(), role.model_id.clone(), reason).await;
                return;
            }
        }
    }

    // (e) Terminal: compute usage, flip Complete, append UsageRecord
    let completed_at_ms = now_ms();
    let usage = {
        let state = inflight.state.lock().await;
        UsageInfo::from_events(&state.events)
    };
    {
        let mut state = inflight.state.lock().await;
        state.status = CompletionStatus::Complete {
            account: role.account.clone(),
            model_id: role.model_id.clone(),
            completed_at_ms,
        };
        state.usage = Some(usage.clone());
    }
    inflight.notify.notify_waiters();

    // Write usage record
    let record = crate::UsageRecord {
        id: Ulid::new().to_string(),
        account: role.account.clone(),
        model_id: role.model_id.clone(),
        dialect: request.model.split_once('/').map(|_| "anthropic".to_string()).unwrap_or_default(),
        upstream_dialect: provider_cfg.dialect.clone(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        started_at_ms,
        completed_at_ms,
        estimated_cost_usd: crate::pricing::model_pricing(&role.model_id).map(|p| {
            let it = usage.input_tokens as f64 / 1_000_000.0;
            let ot = usage.output_tokens as f64 / 1_000_000.0;
            it * p.input_per_mtok + ot * p.output_per_mtok
        }),
    };
    let _ = usage_writer
        .write(&path!("append"), Record::parsed(to_value(&record).unwrap()))
        .await;
}

async fn resolve_model(model: &str, substrate: &ClientHandle) -> Result<CompletionRole, String> {
    if let Some((account, model_id)) = model.split_once('/') {
        return Ok(CompletionRole {
            account: account.to_string(),
            model_id: model_id.to_string(),
        });
    }
    let path = oxpath!("gate", "completions", model);
    let record = substrate.read(&path).await
        .map_err(|e| format!("substrate read failed: {e}"))?
        .ok_or_else(|| format!("no role named '{}'", model))?;
    let value = record.as_value()
        .ok_or_else(|| "role record not parsed".to_string())?
        .clone();
    structfs_serde_store::from_value(value)
        .map_err(|e| format!("invalid CompletionRole: {e}"))
}

async fn resolve_account(
    role: &CompletionRole,
    substrate: &ClientHandle,
) -> Result<(AccountConfig, ProviderConfig, ApiKey), String> {
    let acct_path = oxpath!("gate", "accounts", role.account.as_str());
    let acct: AccountConfig = read_typed(substrate, &acct_path).await?
        .ok_or_else(|| format!("no account named '{}'", role.account))?;

    let prov_path = oxpath!("gate", "providers", acct.provider.as_str());
    let provider: ProviderConfig = read_typed(substrate, &prov_path).await?
        .ok_or_else(|| format!("no provider named '{}'", acct.provider))?;

    let key_path = oxpath!("secret", "keys", role.account.as_str());
    let key: ApiKey = read_typed(substrate, &key_path).await?
        .ok_or_else(|| format!(
            "no API key for account '{}' — set OX_GATE__ACCOUNTS__{}__KEY or edit ~/.ox/keys.json",
            role.account, role.account.to_uppercase()
        ))?;

    Ok((acct, provider, key))
}

async fn read_typed<T: serde::de::DeserializeOwned>(
    substrate: &ClientHandle,
    path: &Path,
) -> Result<Option<T>, String> {
    let record = substrate.read(path).await.map_err(|e| e.to_string())?;
    match record {
        Some(r) => {
            let value = r.as_value().ok_or_else(|| "not parsed".to_string())?.clone();
            structfs_serde_store::from_value(value).map(Some).map_err(|e| e.to_string())
        }
        None => Ok(None),
    }
}

fn build_http_request(
    provider: &ProviderConfig,
    api_key: &ApiKey,
    request: &CompletionRequest,
    upstream_model_id: &str,
) -> Result<structfs_http::HttpRequest, String> {
    let body = match provider.dialect.as_str() {
        "openai" => {
            let mut v = crate::codec::openai::translate_request(&CompletionRequest {
                model: upstream_model_id.to_string(),
                ..request.clone()
            });
            v
        }
        _ => {
            let mut v = serde_json::to_value(&CompletionRequest {
                model: upstream_model_id.to_string(),
                ..request.clone()
            }).map_err(|e| e.to_string())?;
            v
        }
    };

    let mut http = structfs_http::HttpRequest::post(crate::completion_url(provider))
        .with_header("Content-Type", "application/json")
        .with_json_body(body);

    match provider.resolved_auth() {
        crate::AuthScheme::BearerToken => {
            http = http.with_header("Authorization", format!("Bearer {}", api_key.0));
        }
        crate::AuthScheme::XApiKey => {
            http = http.with_header("x-api-key", &api_key.0);
        }
        crate::AuthScheme::None => {}
    }
    if provider.dialect == "anthropic" && !provider.version.is_empty() {
        http = http.with_header("anthropic-version", &provider.version);
    }

    Ok(http)
}

async fn mark_failed(inflight: &Arc<Inflight>, account: String, model_id: String, reason: String) {
    let mut state = inflight.state.lock().await;
    state.status = CompletionStatus::Failed {
        account,
        model_id,
        reason,
        failed_at_ms: now_ms(),
    };
    inflight.notify.notify_waiters();
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}
```

Add `ulid = { workspace = true }` to ox-gate's Cargo.toml if not present (and to `[workspace.dependencies]`).

- [ ] **Step 2: Wire up `AsyncWriter` impl**

In `crates/ox-gate/src/completion_broker/mod.rs`, append:

```rust
use ox_broker::async_store::{AsyncReader, AsyncWriter, BoxFuture};
use std::collections::BTreeMap;
use structfs_serde_store::value_to_json;

impl<E: SseHttpExecutor> AsyncWriter for CompletionBrokerStore<E> {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
        let to = to.clone();

        // Delete: write null to outstanding/{N}
        if let Some((id, None)) = Self::parse_handle_path(&to) {
            if matches!(data.as_value(), Some(Value::Null)) {
                self.handles.remove(&id);
                return Box::pin(async move { Ok(to) });
            }
            return Box::pin(async move {
                Err(StoreError::store("completion_broker", "write", "cannot overwrite an outstanding handle; write null to delete"))
            });
        }

        // Queue: write CompletionRequest to root
        if to.components.is_empty() {
            let value = match data.as_value() {
                Some(v) => v.clone(),
                None => return Box::pin(async move {
                    Err(StoreError::store("completion_broker", "write", "expected parsed record"))
                }),
            };
            let request: CompletionRequest = match from_value(value) {
                Ok(r) => r,
                Err(e) => return Box::pin(async move {
                    Err(StoreError::store("completion_broker", "write", format!("invalid CompletionRequest: {e}")))
                }),
            };

            let id = self.next_request_id;
            self.next_request_id += 1;

            let inflight = Inflight::new(request);
            self.handles.insert(id, inflight.clone());

            let substrate = self.substrate.clone();
            let executor = self.executor.clone();
            let usage_writer = self.usage_writer.clone();
            self.runtime.spawn(async move {
                dispatch::per_request_task(inflight, substrate, executor, usage_writer, id).await;
            });

            let path = Path::try_from_components(vec!["outstanding".to_string(), id.to_string()])
                .map_err(|e| StoreError::store("completion_broker", "write", e.to_string()));
            return Box::pin(async move { path });
        }

        Box::pin(async move {
            Err(StoreError::store("completion_broker", "write", format!("unexpected write path: {to}")))
        })
    }
}
```

- [ ] **Step 3: Build**

Run: `cargo build -p ox-gate`
Expected: compiles. Fix any signature mismatches against your actual `AsyncWriter` trait definition in ox-broker (refer to `crates/ox-broker/src/async_store.rs`).

- [ ] **Step 4: Commit**

```bash
git add crates/ox-gate/
git commit -m "feat(ox-gate): CompletionBrokerStore write — resolve, spawn upstream, append usage"
```

### Task 3.5: `CompletionBrokerStore` — `AsyncReader` (blocking events drain)

**Files:**
- Modify: `crates/ox-gate/src/completion_broker/mod.rs`

- [ ] **Step 1: Implement `AsyncReader`**

Append:

```rust
impl<E: SseHttpExecutor> AsyncReader for CompletionBrokerStore<E> {
    fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let from = from.clone();

        // Root: descriptor
        if from.components.is_empty() {
            let mut map = BTreeMap::new();
            map.insert("outstanding".to_string(), Value::String("outstanding".into()));
            map.insert("docs".to_string(), Value::String("docs".into()));
            return Box::pin(async move { Ok(Some(Record::parsed(Value::Map(map)))) });
        }

        // /outstanding listing
        if from.components.len() == 1 && from.components[0].as_str() == "outstanding" {
            let ids: Vec<Value> = self.handles.keys()
                .map(|id| Value::String(format!("outstanding/{}", id)))
                .collect();
            let mut map = BTreeMap::new();
            map.insert("items".to_string(), Value::Array(ids));
            return Box::pin(async move { Ok(Some(Record::parsed(Value::Map(map)))) });
        }

        // /docs
        if from.components.first().map(|c| c.as_str()) == Some("docs") {
            return Box::pin(async move { Ok(Some(Record::parsed(docs()))) });
        }

        // /outstanding/{N} or sub-paths
        let (id, sub) = match Self::parse_handle_path(&from) {
            Some(t) => t,
            None => return Box::pin(async move { Ok(None) }),
        };

        let inflight = match self.handles.get(&id) {
            Some(arc) => arc.clone(),
            None => return Box::pin(async move { Ok(None) }),
        };

        Box::pin(async move {
            match sub.as_deref() {
                None => {
                    let state = inflight.state.lock().await;
                    let v = to_value(&state.status).map_err(|e| StoreError::store("completion_broker", "read", e.to_string()))?;
                    Ok(Some(Record::parsed(v)))
                }
                Some("request") => {
                    let state = inflight.state.lock().await;
                    let v = to_value(&state.request).map_err(|e| StoreError::store("completion_broker", "read", e.to_string()))?;
                    Ok(Some(Record::parsed(v)))
                }
                Some("usage") => {
                    let state = inflight.state.lock().await;
                    match &state.usage {
                        Some(u) => {
                            let v = to_value(u).map_err(|e| StoreError::store("completion_broker", "read", e.to_string()))?;
                            Ok(Some(Record::parsed(v)))
                        }
                        None => Ok(None),
                    }
                }
                Some(s) if s.starts_with("events/count") => {
                    let state = inflight.state.lock().await;
                    let v = Value::Integer(state.events.len() as i64);
                    Ok(Some(Record::parsed(v)))
                }
                Some(s) if s.starts_with("events/from/") => {
                    let seq: usize = s.trim_start_matches("events/from/").parse()
                        .map_err(|e: std::num::ParseIntError| StoreError::store("completion_broker", "read", e.to_string()))?;
                    loop {
                        let state = inflight.state.lock().await;
                        if state.events.len() > seq {
                            let tail = state.events[seq..].to_vec();
                            let v = to_value(&tail).map_err(|e| StoreError::store("completion_broker", "read", e.to_string()))?;
                            return Ok(Some(Record::parsed(v)));
                        }
                        if state.status.is_terminal() {
                            let tail = state.events[seq..].to_vec();
                            let v = to_value(&tail).map_err(|e| StoreError::store("completion_broker", "read", e.to_string()))?;
                            return Ok(Some(Record::parsed(v)));
                        }
                        drop(state);
                        inflight.notify.notified().await;
                    }
                }
                _ => Ok(None),
            }
        })
    }
}

fn docs() -> Value {
    let json = serde_json::json!({
        "title": "CompletionBrokerStore",
        "paths": {
            "write /": "Queue CompletionRequest → outstanding/{N}",
            "read outstanding/{N}": "CompletionStatus",
            "read outstanding/{N}/request": "Original CompletionRequest",
            "read outstanding/{N}/events/from/{S}": "Vec<StreamEvent> from index S (BLOCKING)",
            "read outstanding/{N}/usage": "UsageInfo (None until Complete)",
            "write outstanding/{N} null": "Delete handle"
        }
    });
    structfs_serde_store::json_to_value(json)
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p ox-gate`
Expected: compiles.

- [ ] **Step 3: Write a lifecycle integration test**

Create `crates/ox-gate/src/completion_broker/tests.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion_broker::mock::MockSseExecutor;
    use ox_kernel::CompletionRequest;
    use ox_types::StreamEvent;
    use std::time::Duration;
    use structfs_core_store::path;

    async fn build_test_substrate() -> (ox_broker::BrokerStore, ClientHandle) {
        use ox_store_util::LocalConfig;
        use ox_path::oxpath;
        use crate::{AccountConfig, ApiKey, ProviderConfig};
        use structfs_serde_store::to_value;
        use ox_types::CompletionRole;

        let broker = ox_broker::BrokerStore::new(Duration::from_secs(2));

        let mut gate_config = LocalConfig::new();
        let role = CompletionRole {
            account: "anthropic".into(),
            model_id: "claude-sonnet-4-20250514".into(),
        };
        gate_config.set("gate/completions/fast", to_value(&role).unwrap());
        gate_config.set(
            "gate/accounts/anthropic",
            to_value(&AccountConfig { provider: "anthropic".into(), ..Default::default() }).unwrap(),
        );
        gate_config.set(
            "gate/providers/anthropic",
            to_value(&ProviderConfig::anthropic()).unwrap(),
        );
        broker.mount(oxpath!(""), gate_config).await;

        let mut secret = LocalConfig::new();
        secret.set("secret/keys/anthropic", to_value(&ApiKey::new("sk-test")).unwrap());
        broker.mount(oxpath!("secret"), secret).await;

        let client = broker.client();
        (broker, client)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn complete_lifecycle_slash_form() {
        let (broker, substrate) = build_test_substrate().await;

        let executor = std::sync::Arc::new(MockSseExecutor::new());
        executor.push_immediate(StreamEvent::InputUsage { input_tokens: 10, cache_creation: 0, cache_read: 0 });
        executor.push_immediate(StreamEvent::TextDelta { text: "Hello".into() });
        executor.push_immediate(StreamEvent::OutputUsage { output_tokens: 1 });
        executor.push_immediate(StreamEvent::MessageStop);

        let usage_backing = Box::new(ox_store_util::LocalConfig::new());
        let usage_store = crate::UsageStore::new(usage_backing);
        broker.mount(oxpath!("gateway", "usage"), usage_store).await;

        let store = CompletionBrokerStore::new(
            substrate.clone(),
            executor.clone(),
            substrate.scoped("gateway/usage"),
            tokio::runtime::Handle::current(),
        );
        broker.mount(oxpath!("gateway", "completions"), store).await;

        let request = CompletionRequest {
            model: "anthropic/claude-sonnet-4-20250514".into(),
            max_tokens: 100,
            system: "".into(),
            messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
            tools: vec![],
            stream: true,
        };

        let client = broker.client();
        let handle = client.write_typed(&path!("gateway/completions"), &request).await.unwrap();

        // Drain events
        let mut next = 0usize;
        let mut all_events = Vec::new();
        loop {
            let events_path = handle.join(&path!(&format!("events/from/{next}")));
            let events: Vec<StreamEvent> = client.read_typed(&events_path).await.unwrap().unwrap_or_default();
            for ev in &events { all_events.push(ev.clone()); }
            next += events.len();
            let status: CompletionStatus = client.read_typed(&handle).await.unwrap().unwrap();
            if status.is_terminal() { break; }
        }

        assert_eq!(all_events.len(), 4);
        assert!(matches!(all_events.last().unwrap(), StreamEvent::MessageStop));
    }
}
```

- [ ] **Step 4: Run the lifecycle test**

Run: `cargo test -p ox-gate completion_broker::tests::complete_lifecycle_slash_form -- --nocapture`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-gate/
git commit -m "feat(ox-gate): CompletionBrokerStore reads — status, events/from blocking, usage"
```

---

## Phase 4: `ox-gateway` bin crate

### Task 4.1: Crate skeleton + `Cargo.toml`

**Files:**
- Create: `crates/ox-gateway/Cargo.toml`
- Create: `crates/ox-gateway/src/lib.rs`
- Create: `crates/ox-gateway/src/main.rs`
- Modify: `Cargo.toml` (workspace root) — add `crates/ox-gateway` to members

- [ ] **Step 1: Workspace member**

Edit root `Cargo.toml`'s `[workspace] members = [...]` and add `"crates/ox-gateway"`.

- [ ] **Step 2: Crate manifest**

Create `crates/ox-gateway/Cargo.toml`:

```toml
[package]
name = "ox-gateway"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
homepage.workspace = true
authors.workspace = true
description = "Local LLM gateway exposing Anthropic / OpenAI APIs over the StructFS substrate"
readme = "README.md"
keywords = ["ai", "llm", "gateway", "api", "structfs"]
categories = ["command-line-utilities"]

[[bin]]
name = "ox-gateway"
path = "src/main.rs"

[dependencies]
ox-types = { workspace = true }
ox-kernel = { workspace = true }
ox-broker = { workspace = true }
ox-gate = { workspace = true }
ox-store-util = { workspace = true }
ox-path = { workspace = true }
structfs-core-store = { workspace = true }
structfs-serde-store = { workspace = true }
structfs-http = { workspace = true }

axum = { workspace = true, features = ["macros"] }
tokio = { workspace = true, features = ["full"] }
async-stream = { workspace = true }
futures = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
ulid = { workspace = true }
anyhow = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
reqwest = { workspace = true, features = ["json", "stream"] }
tempfile = { workspace = true }
```

If any of these aren't in `[workspace.dependencies]` of the root Cargo.toml, add them with sensible versions.

- [ ] **Step 3: `lib.rs` skeleton**

Create `crates/ox-gateway/src/lib.rs`:

```rust
//! Local LLM gateway: thin axum shell over the StructFS substrate.

pub mod error;
pub mod handle;
pub mod routes;
```

- [ ] **Step 4: Minimal `main.rs`**

Create `crates/ox-gateway/src/main.rs`:

```rust
#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("ox-gateway starting");
    // Broker assembly comes in Task 4.6
    Ok(())
}
```

- [ ] **Step 5: Build**

Run: `cargo build -p ox-gateway`
Expected: compiles. (Will produce a bin that does nothing yet.)

- [ ] **Step 6: Commit**

```bash
git add crates/ox-gateway/ Cargo.toml
git commit -m "feat(ox-gateway): new bin crate skeleton"
```

### Task 4.2: `error.rs` — dialect-shaped error envelopes

**Files:**
- Create: `crates/ox-gateway/src/error.rs`

- [ ] **Step 1: Write tests**

Create `crates/ox-gateway/src/error.rs`:

```rust
//! HTTP error envelopes shaped per dialect.

use axum::http::StatusCode;
use serde_json::{json, Value};

pub fn anthropic_error(status: StatusCode, message: impl Into<String>) -> (StatusCode, axum::Json<Value>) {
    let kind = match status.as_u16() {
        400 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        500..=599 => "api_error",
        _ => "api_error",
    };
    let body = json!({
        "type": "error",
        "error": { "type": kind, "message": message.into() }
    });
    (status, axum::Json(body))
}

pub fn openai_error(status: StatusCode, message: impl Into<String>, code: Option<&str>) -> (StatusCode, axum::Json<Value>) {
    let kind = match status.as_u16() {
        400 => "invalid_request_error",
        401 | 403 => "invalid_request_error",
        404 => "invalid_request_error",
        429 => "rate_limit_exceeded",
        _ => "api_error",
    };
    let body = json!({
        "error": {
            "message": message.into(),
            "type": kind,
            "code": code,
        }
    });
    (status, axum::Json(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_400_shape() {
        let (s, body) = anthropic_error(StatusCode::BAD_REQUEST, "bad");
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(body.0["type"], "error");
        assert_eq!(body.0["error"]["type"], "invalid_request_error");
        assert_eq!(body.0["error"]["message"], "bad");
    }

    #[test]
    fn openai_401_shape() {
        let (s, body) = openai_error(StatusCode::UNAUTHORIZED, "no key", None);
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert_eq!(body.0["error"]["type"], "invalid_request_error");
        assert_eq!(body.0["error"]["message"], "no key");
    }
}
```

- [ ] **Step 2: Build + test**

Run: `cargo test -p ox-gateway error`
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/ox-gateway/src/error.rs
git commit -m "feat(ox-gateway): dialect-shaped error envelopes"
```

### Task 4.3: `handle.rs` — the shared streaming-drain helper

**Files:**
- Create: `crates/ox-gateway/src/handle.rs`

- [ ] **Step 1: Implement the helper**

```rust
//! The shared loop that drains gateway/completions/outstanding/{N}/events/from/{S}
//! and yields encoded SSE frames.

use axum::response::sse::{Event, Sse};
use futures::stream::Stream;
use ox_broker::ClientHandle;
use ox_gate::codec::SseEncoder;
use ox_gate::completion_broker::CompletionStatus;
use ox_types::StreamEvent;
use std::convert::Infallible;
use structfs_core_store::{path, Path, Record, Value};

/// Drain events into an SSE stream, encoding via the supplied SseEncoder.
/// `handle` is the path returned by writing the CompletionRequest.
pub fn stream_response(
    client: ClientHandle,
    handle: Path,
    dialect: String,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        let mut encoder = SseEncoder::new(&dialect);
        let mut next = 0usize;
        loop {
            let events_path = handle.join(&Path::try_from_components(vec![
                "events".to_string(), "from".to_string(), next.to_string()
            ]).unwrap());

            let events: Vec<StreamEvent> = match client.read_typed(&events_path).await {
                Ok(Some(v)) => v,
                Ok(None) => Vec::new(),
                Err(e) => {
                    let frame = format!("event: error\ndata: {{\"message\":\"{}\"}}\n\n", e);
                    yield Ok(Event::default().data(frame));
                    break;
                }
            };

            for ev in &events {
                for frame in encoder.encode_sse(ev) {
                    yield Ok(Event::default().data(frame));
                }
            }
            next += events.len();

            let status: Option<CompletionStatus> = client.read_typed(&handle).await.ok().flatten();
            match status {
                Some(CompletionStatus::Complete { .. }) => {
                    for frame in encoder.finish() {
                        yield Ok(Event::default().data(frame));
                    }
                    break;
                }
                Some(CompletionStatus::Failed { reason, .. }) => {
                    let frame = format!("event: error\ndata: {{\"message\":\"{}\"}}\n\n", reason);
                    yield Ok(Event::default().data(frame));
                    break;
                }
                _ => continue,
            }
        }
        // GC the inflight (best-effort)
        let _ = client.write(&handle, Record::parsed(Value::Null)).await;
    };
    Sse::new(stream)
}

/// Non-streaming variant: drain all events into a buffer, return the
/// terminal status + events. Caller encodes via codec::*::encode_response.
pub async fn buffer_response(
    client: ClientHandle,
    handle: Path,
) -> Result<(CompletionStatus, Vec<StreamEvent>), String> {
    let mut next = 0usize;
    let mut all_events = Vec::new();
    loop {
        let events_path = handle.join(&Path::try_from_components(vec![
            "events".to_string(), "from".to_string(), next.to_string()
        ]).map_err(|e| e.to_string())?);

        let events: Vec<StreamEvent> = client.read_typed(&events_path).await.map_err(|e| e.to_string())?.unwrap_or_default();
        next += events.len();
        all_events.extend(events);

        let status: CompletionStatus = client.read_typed(&handle).await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "inflight vanished".to_string())?;
        if status.is_terminal() {
            let _ = client.write(&handle, Record::parsed(Value::Null)).await;
            return Ok((status, all_events));
        }
    }
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p ox-gateway`
Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add crates/ox-gateway/src/handle.rs
git commit -m "feat(ox-gateway): handle::{stream_response, buffer_response} drain helpers"
```

### Task 4.4: `routes/anthropic.rs` — `POST /v1/messages`

**Files:**
- Create: `crates/ox-gateway/src/routes/mod.rs`
- Create: `crates/ox-gateway/src/routes/anthropic.rs`

- [ ] **Step 1: Router skeleton**

Create `crates/ox-gateway/src/routes/mod.rs`:

```rust
pub mod anthropic;
pub mod openai;
pub mod models;
pub mod ox_native;

use axum::Router;
use ox_broker::ClientHandle;

pub fn build_router(client: ClientHandle) -> Router {
    Router::new()
        .merge(anthropic::router(client.clone()))
        .merge(openai::router(client.clone()))
        .merge(models::router(client.clone()))
        .merge(ox_native::router(client))
}
```

- [ ] **Step 2: Anthropic route**

Create `crates/ox-gateway/src/routes/anthropic.rs`:

```rust
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use ox_broker::ClientHandle;
use ox_gate::codec::anthropic as codec;
use ox_gate::codec::CodecError;
use serde_json::Value;
use structfs_core_store::{path, Record};

use crate::error::anthropic_error;
use crate::handle;

pub fn router(client: ClientHandle) -> Router {
    Router::new()
        .route("/v1/messages", post(post_messages))
        .route("/v1/messages/count_tokens", post(post_count_tokens))
        .with_state(client)
}

async fn post_messages(
    State(client): State<ClientHandle>,
    Json(body): Json<Value>,
) -> axum::response::Response {
    let req = match codec::decode_request(&body) {
        Ok(r) => r,
        Err(e) => return anthropic_error(StatusCode::BAD_REQUEST, codec_error_message(&e)).into_response(),
    };
    let streaming = req.stream;

    let handle_path = match client.write_typed(&path!("gateway/completions"), &req).await {
        Ok(p) => p,
        Err(e) => return anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    if streaming {
        handle::stream_response(client, handle_path, "anthropic".into()).into_response()
    } else {
        match handle::buffer_response(client, handle_path).await {
            Ok((status, events)) => {
                use ox_gate::completion_broker::CompletionStatus;
                match status {
                    CompletionStatus::Complete { .. } => Json(codec::encode_response(&events)).into_response(),
                    CompletionStatus::Failed { reason, .. } => {
                        anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, reason).into_response()
                    }
                    _ => anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, "unexpected non-terminal status").into_response(),
                }
            }
            Err(e) => anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        }
    }
}

async fn post_count_tokens(
    State(_client): State<ClientHandle>,
    Json(_body): Json<Value>,
) -> axum::response::Response {
    // v1 stub: passthrough not implemented yet. Return 501 for visibility;
    // implement by reading provider, building HttpRequest via HttpBrokerStore,
    // forwarding response. Tracked as out-of-scope for v1 ledger purposes.
    anthropic_error(StatusCode::NOT_IMPLEMENTED, "count_tokens not yet implemented").into_response()
}

fn codec_error_message(e: &CodecError) -> String {
    e.to_string()
}
```

- [ ] **Step 3: Build**

Run: `cargo build -p ox-gateway`
Expected: compiles. Fix any minor signature issues (ClientHandle::scoped lookup, etc.) against the broker client API.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-gateway/src/routes/
git commit -m "feat(ox-gateway): POST /v1/messages (streaming + non-streaming)"
```

### Task 4.5: `routes/openai.rs` — `POST /v1/chat/completions`

**Files:**
- Create: `crates/ox-gateway/src/routes/openai.rs`

- [ ] **Step 1: Implement**

```rust
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use ox_broker::ClientHandle;
use ox_gate::codec::openai as codec;
use ox_gate::completion_broker::CompletionStatus;
use serde_json::Value;
use structfs_core_store::path;

use crate::error::openai_error;
use crate::handle;

pub fn router(client: ClientHandle) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(post_chat_completions))
        .with_state(client)
}

async fn post_chat_completions(
    State(client): State<ClientHandle>,
    Json(body): Json<Value>,
) -> axum::response::Response {
    let req = match codec::decode_request(&body) {
        Ok(r) => r,
        Err(e) => return openai_error(StatusCode::BAD_REQUEST, e.to_string(), None).into_response(),
    };
    let streaming = req.stream;

    let handle_path = match client.write_typed(&path!("gateway/completions"), &req).await {
        Ok(p) => p,
        Err(e) => return openai_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None).into_response(),
    };

    if streaming {
        handle::stream_response(client, handle_path, "openai".into()).into_response()
    } else {
        match handle::buffer_response(client, handle_path).await {
            Ok((CompletionStatus::Complete { .. }, events)) => Json(codec::encode_response(&events)).into_response(),
            Ok((CompletionStatus::Failed { reason, .. }, _)) => {
                openai_error(StatusCode::INTERNAL_SERVER_ERROR, reason, None).into_response()
            }
            Ok(_) => openai_error(StatusCode::INTERNAL_SERVER_ERROR, "unexpected non-terminal status", None).into_response(),
            Err(e) => openai_error(StatusCode::INTERNAL_SERVER_ERROR, e, None).into_response(),
        }
    }
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p ox-gateway`
Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add crates/ox-gateway/src/routes/openai.rs
git commit -m "feat(ox-gateway): POST /v1/chat/completions"
```

### Task 4.6: `routes/models.rs` — `GET /v1/models`

**Files:**
- Create: `crates/ox-gateway/src/routes/models.rs`

- [ ] **Step 1: Implement**

```rust
use axum::{
    extract::State,
    http::HeaderMap,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use ox_broker::ClientHandle;
use ox_path::oxpath;
use serde_json::{json, Value};
use structfs_core_store::Path;

pub fn router(client: ClientHandle) -> Router {
    Router::new().route("/v1/models", get(list_models)).with_state(client)
}

async fn list_models(
    headers: HeaderMap,
    State(client): State<ClientHandle>,
) -> impl IntoResponse {
    // Iterate accounts. The gate doesn't expose a list endpoint directly;
    // probe the snapshot. (gate/snapshot/state contains accounts + providers.)
    let snapshot = match client.read(&oxpath!("gate", "snapshot", "state")).await {
        Ok(Some(r)) => match r.as_value() {
            Some(v) => v.clone(),
            None => return Json(json!({"data": []})).into_response(),
        },
        _ => return Json(json!({"data": []})).into_response(),
    };

    let json = structfs_serde_store::value_to_json(snapshot);
    let accounts = json.get("accounts").and_then(|v| v.as_object());
    let providers = json.get("providers").and_then(|v| v.as_object());

    let mut items = Vec::new();
    if let (Some(accounts), Some(_providers)) = (accounts, providers) {
        for (account_name, account_val) in accounts {
            let provider_name = account_val.get("provider").and_then(|v| v.as_str()).unwrap_or("");
            // Read models from gate/providers/{provider}/models
            let models_path = oxpath!("gate", "providers", provider_name, "models");
            let models: Vec<ox_types::ModelInfo> = match client.read_typed(&models_path).await {
                Ok(Some(m)) => m,
                _ => continue,
            };
            for m in models {
                items.push((account_name.clone(), m));
            }
        }
    }

    // Determine wire shape from Accept header (Anthropic vs OpenAI list)
    let want_openai = headers.get("accept")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.contains("openai") || s.contains("application/json"))
        .unwrap_or(true);

    if want_openai {
        let data: Vec<Value> = items.iter().map(|(acct, m)| {
            json!({
                "id": format!("{}/{}", acct, m.id),
                "object": "model",
                "created": 0,
                "owned_by": acct,
            })
        }).collect();
        Json(json!({ "object": "list", "data": data })).into_response()
    } else {
        let data: Vec<Value> = items.iter().map(|(acct, m)| {
            json!({
                "id": format!("{}/{}", acct, m.id),
                "display_name": m.display_name,
                "created_at": null,
                "type": "model",
            })
        }).collect();
        Json(json!({ "data": data })).into_response()
    }
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p ox-gateway`
Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add crates/ox-gateway/src/routes/models.rs
git commit -m "feat(ox-gateway): GET /v1/models (aggregated across accounts)"
```

### Task 4.7: `routes/ox_native.rs` — `POST /completions`

**Files:**
- Create: `crates/ox-gateway/src/routes/ox_native.rs`

- [ ] **Step 1: Implement**

```rust
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use ox_broker::ClientHandle;
use ox_gate::codec::SseEncoder;
use ox_kernel::CompletionRequest;
use serde_json::Value;
use structfs_core_store::path;

use crate::handle;

pub fn router(client: ClientHandle) -> Router {
    Router::new()
        .route("/completions", post(post_completions))
        .with_state(client)
}

async fn post_completions(
    State(client): State<ClientHandle>,
    Json(req): Json<CompletionRequest>,
) -> axum::response::Response {
    let streaming = req.stream;
    let handle_path = match client.write_typed(&path!("gateway/completions"), &req).await {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    if streaming {
        // ox-native: stream each StreamEvent as one SSE frame, JSON-encoded
        // verbatim (no dialect translation).
        use axum::response::sse::{Event, Sse};
        use futures::stream::Stream;
        use ox_types::StreamEvent;
        use std::convert::Infallible;
        use ox_gate::completion_broker::CompletionStatus;
        use structfs_core_store::{Path, Record, Value as SfValue};

        let stream = async_stream::stream! {
            let mut next = 0usize;
            loop {
                let events_path = handle_path.join(&Path::try_from_components(vec![
                    "events".to_string(), "from".to_string(), next.to_string()
                ]).unwrap());
                let events: Vec<StreamEvent> = client.read_typed(&events_path).await.ok().flatten().unwrap_or_default();
                for ev in &events {
                    let json = serde_json::to_string(ev).unwrap_or_default();
                    yield Ok::<_, Infallible>(Event::default().data(json));
                }
                next += events.len();
                let status: Option<CompletionStatus> = client.read_typed(&handle_path).await.ok().flatten();
                if matches!(status, Some(s) if s.is_terminal()) { break; }
            }
            let _ = client.write(&handle_path, Record::parsed(SfValue::Null)).await;
        };
        Sse::new(stream).into_response()
    } else {
        match handle::buffer_response(client, handle_path).await {
            Ok((status, events)) => {
                Json(serde_json::json!({
                    "status": status,
                    "events": events,
                })).into_response()
            }
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        }
    }
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p ox-gateway`
Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add crates/ox-gateway/src/routes/ox_native.rs
git commit -m "feat(ox-gateway): POST /completions (ox-native shape)"
```

### Task 4.8: `main.rs` — broker assembly + serve

**Files:**
- Modify: `crates/ox-gateway/src/main.rs`

- [ ] **Step 1: Full main.rs**

```rust
use ox_broker::BrokerStore;
use ox_path::oxpath;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let ox_dir = ox_dir()?;
    let toml_path = ox_dir.join("config.toml");
    let keys_path = ox_dir.join("keys.json");
    let usage_path = ox_dir.join("usage.jsonl");

    let broker = BrokerStore::new(Duration::from_secs(2));

    // config/  — ConfigStore over the same TOML ox-cli reads
    let figment_config = ox_cli::config::resolve_config(&ox_dir, &Default::default());
    let base = figment_config.to_flat_map();
    let config_backing = ox_store_util::TomlFileBacking::new(&toml_path)?;
    let config = ox_ui::config_store::ConfigStore::with_backing(base, Box::new(config_backing));
    broker.mount(oxpath!("config"), config).await;

    // secret/ — LocalConfig over keys.json (mode 0600 on Unix)
    let secret_backing = ox_store_util::JsonFileBacking::new(&keys_path)?;
    let secret = ox_store_util::LocalConfig::with_backing(
        std::collections::BTreeMap::new(),
        Box::new(secret_backing),
    );
    broker.mount(oxpath!("secret"), secret).await;

    // gate/ — GateStore wired to config + secret handles
    let gate = ox_gate::GateStore::new()
        .with_config(Box::new(broker.handle("config")))
        .with_secrets(Box::new(broker.handle("secret")));
    broker.mount(oxpath!("gate"), gate).await;

    // gateway/usage/ — JsonlFileBacking
    let usage_backing = Box::new(ox_store_util::JsonlFileBacking::new(&usage_path)?);
    let usage = ox_gate::UsageStore::new(usage_backing);
    broker.mount(oxpath!("gateway", "usage"), usage).await;

    // gateway/completions/ — CompletionBrokerStore
    let executor = Arc::new(ox_gate::ReqwestSseExecutor::with_default_timeout()
        .map_err(anyhow::Error::msg)?);
    let usage_client = broker.client().scoped("gateway/usage");
    let completions = ox_gate::CompletionBrokerStore::new(
        broker.client(),
        executor,
        usage_client,
        tokio::runtime::Handle::current(),
    );
    broker.mount(oxpath!("gateway", "completions"), completions).await;

    // axum
    let bind_addr = std::env::var("OX_GATEWAY_BIND").unwrap_or_else(|_| "127.0.0.1:11343".into());
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!(addr = %listener.local_addr()?, "ox-gateway listening");

    let app = ox_gateway::routes::build_router(broker.client());
    axum::serve(listener, app).await?;
    Ok(())
}

fn ox_dir() -> anyhow::Result<std::path::PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME not set"))?;
    let dir = std::path::PathBuf::from(home).join(".ox");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
```

If imports don't resolve (e.g. `ox_cli::config::resolve_config` is private), adjust either by making it pub (small change in ox-cli) or by inlining the resolution logic. The figment resolution path lives in `crates/ox-cli/src/config.rs::resolve_config` — confirm it's `pub`.

- [ ] **Step 2: Build**

Run: `cargo build -p ox-gateway`
Expected: compiles.

- [ ] **Step 3: Smoke test — start binary, hit /v1/models**

This needs you to run the binary in one terminal and curl it in another. Skip if unavailable; the integration tests in Phase 5 will verify behavior.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-gateway/src/main.rs
git commit -m "feat(ox-gateway): main.rs — broker assembly + axum serve"
```

---

## Phase 5: Integration tests

### Task 5.1: End-to-end streaming test through `/v1/messages`

**Files:**
- Create: `crates/ox-gateway/tests/streaming_anthropic.rs`

- [ ] **Step 1: Write the test**

```rust
//! End-to-end streaming test: client → axum → CompletionBrokerStore →
//! MockSseExecutor → drain back through SSE.

use ox_broker::BrokerStore;
use ox_path::oxpath;
use ox_types::StreamEvent;
use std::sync::Arc;
use std::time::Duration;
use structfs_serde_store::to_value;

async fn build_test_broker(executor: Arc<ox_gate::completion_broker::mock::MockSseExecutor>) -> BrokerStore {
    use ox_gate::{AccountConfig, ApiKey, ProviderConfig};
    use ox_types::CompletionRole;
    use ox_store_util::LocalConfig;

    let broker = BrokerStore::new(Duration::from_secs(5));

    let mut config = LocalConfig::new();
    config.set(
        "gate/completions/primary",
        to_value(&CompletionRole {
            account: "anthropic".into(),
            model_id: "claude-sonnet-4-20250514".into(),
        }).unwrap(),
    );
    broker.mount(oxpath!(""), config).await;

    let mut secret = LocalConfig::new();
    secret.set("keys/anthropic", to_value(&ApiKey::new("sk-test")).unwrap());
    broker.mount(oxpath!("secret"), secret).await;

    let gate = ox_gate::GateStore::new()
        .with_config(Box::new(broker.handle("")))
        .with_secrets(Box::new(broker.handle("secret")));
    broker.mount(oxpath!("gate"), gate).await;

    let usage_backing = Box::new(LocalConfig::new());
    let usage = ox_gate::UsageStore::new(usage_backing);
    broker.mount(oxpath!("gateway", "usage"), usage).await;

    let completions = ox_gate::CompletionBrokerStore::new(
        broker.client(),
        executor,
        broker.client().scoped("gateway/usage"),
        tokio::runtime::Handle::current(),
    );
    broker.mount(oxpath!("gateway", "completions"), completions).await;

    broker
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_anthropic_messages_endpoint() {
    let executor = Arc::new(ox_gate::completion_broker::mock::MockSseExecutor::new());
    executor.push_immediate(StreamEvent::InputUsage { input_tokens: 10, cache_creation: 0, cache_read: 0 });
    executor.push_immediate(StreamEvent::TextDelta { text: "Hello".into() });
    executor.push_immediate(StreamEvent::OutputUsage { output_tokens: 1 });
    executor.push_immediate(StreamEvent::MessageStop);

    let broker = build_test_broker(executor).await;
    let app = ox_gateway::routes::build_router(broker.client());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/v1/messages", addr))
        .json(&serde_json::json!({
            "model": "anthropic/claude-sonnet-4-20250514",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success());

    let body = resp.text().await.unwrap();
    assert!(body.contains("event: message_start"));
    assert!(body.contains("event: content_block_start"));
    assert!(body.contains("\"text\":\"Hello\""));
    assert!(body.contains("event: message_stop"));
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p ox-gateway --test streaming_anthropic -- --nocapture`
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/ox-gateway/tests/streaming_anthropic.rs
git commit -m "test(ox-gateway): end-to-end streaming /v1/messages"
```

### Task 5.2: Streaming test for `/v1/chat/completions`

**Files:**
- Create: `crates/ox-gateway/tests/streaming_openai.rs`
- Create: `crates/ox-gateway/tests/common/mod.rs` (extract the `build_test_broker` helper here so both tests share it)

- [ ] **Step 1: Extract the shared helper**

Move `build_test_broker` from `tests/streaming_anthropic.rs` into a new `tests/common/mod.rs` and update both test files to `mod common; use common::build_test_broker;` (tests in `tests/` need to declare the common module explicitly since each `.rs` file is its own test binary).

`tests/common/mod.rs`:

```rust
use ox_broker::BrokerStore;
use ox_path::oxpath;
use std::sync::Arc;
use std::time::Duration;
use structfs_serde_store::to_value;

pub async fn build_test_broker(
    executor: Arc<ox_gate::completion_broker::mock::MockSseExecutor>,
    provider_dialect: &str,
) -> BrokerStore {
    use ox_gate::{AccountConfig, ApiKey, ProviderConfig};
    use ox_types::CompletionRole;
    use ox_store_util::LocalConfig;

    let broker = BrokerStore::new(Duration::from_secs(5));

    let mut config = LocalConfig::new();
    config.set(
        "gate/completions/primary",
        to_value(&CompletionRole {
            account: provider_dialect.to_string(),
            model_id: match provider_dialect {
                "openai" => "gpt-4o".into(),
                _ => "claude-sonnet-4-20250514".into(),
            },
        }).unwrap(),
    );
    broker.mount(oxpath!(""), config).await;

    let mut secret = LocalConfig::new();
    secret.set(
        &format!("keys/{}", provider_dialect),
        to_value(&ApiKey::new("sk-test")).unwrap(),
    );
    broker.mount(oxpath!("secret"), secret).await;

    let gate = ox_gate::GateStore::new()
        .with_config(Box::new(broker.handle("")))
        .with_secrets(Box::new(broker.handle("secret")));
    broker.mount(oxpath!("gate"), gate).await;

    let usage_backing = Box::new(LocalConfig::new());
    let usage = ox_gate::UsageStore::new(usage_backing);
    broker.mount(oxpath!("gateway", "usage"), usage).await;

    let completions = ox_gate::CompletionBrokerStore::new(
        broker.client(),
        executor,
        broker.client().scoped("gateway/usage"),
        tokio::runtime::Handle::current(),
    );
    broker.mount(oxpath!("gateway", "completions"), completions).await;

    broker
}
```

- [ ] **Step 2: Write the OpenAI streaming test**

`crates/ox-gateway/tests/streaming_openai.rs`:

```rust
mod common;

use common::build_test_broker;
use ox_types::StreamEvent;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_openai_chat_completions_endpoint() {
    let executor = Arc::new(ox_gate::completion_broker::mock::MockSseExecutor::new());
    executor.push_immediate(StreamEvent::TextDelta { text: "Hi".into() });
    executor.push_immediate(StreamEvent::OutputUsage { output_tokens: 1 });
    executor.push_immediate(StreamEvent::MessageStop);

    let broker = build_test_broker(executor, "openai").await;
    let app = ox_gateway::routes::build_router(broker.client());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/v1/chat/completions", addr))
        .json(&serde_json::json!({
            "model": "openai/gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success());
    let body = resp.text().await.unwrap();
    assert!(body.contains("\"role\":\"assistant\""));
    assert!(body.contains("\"content\":\"Hi\""));
    assert!(body.contains("data: [DONE]"));
}
```

- [ ] **Step 3: Run**

Run: `cargo test -p ox-gateway --test streaming_openai -- --nocapture`
Expected: pass.

- [ ] **Step 4: Update Task 5.1 to use the same helper**

Refactor `tests/streaming_anthropic.rs` to also `mod common; use common::build_test_broker;` and remove its local copy of the helper. Re-run that test to ensure still green.

Run: `cargo test -p ox-gateway --tests`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-gateway/tests/
git commit -m "test(ox-gateway): /v1/chat/completions streaming + shared test fixture"
```

### Task 5.3: Error paths (resolution failures, upstream errors)

**Files:**
- Create: `crates/ox-gateway/tests/error_paths.rs`

- [ ] **Step 1: Write the test file**

```rust
mod common;

use common::build_test_broker;
use ox_types::StreamEvent;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_role_returns_400_or_streams_error() {
    // No mock event sequence — the per_request_task fails before
    // executor.execute is ever called.
    let executor = Arc::new(ox_gate::completion_broker::mock::MockSseExecutor::new());
    let broker = build_test_broker(executor, "anthropic").await;
    let app = ox_gateway::routes::build_router(broker.client());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });

    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/messages", addr))
        .json(&serde_json::json!({
            "model": "nope-does-not-exist",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    // The handler writes the request, gets a handle, then drains. The
    // drain sees Failed status with reason "no role named 'nope-does-not-exist'"
    // and emits an SSE error frame.
    let body = resp.text().await.unwrap();
    assert!(body.contains("error"));
    assert!(body.contains("no role named"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upstream_error_propagates_as_sse_error_frame() {
    let executor = Arc::new(ox_gate::completion_broker::mock::MockSseExecutor::new());
    executor.push(
        std::time::Duration::ZERO,
        Err("HTTP 401 from upstream: invalid key".into()),
    );

    let broker = build_test_broker(executor, "anthropic").await;
    let app = ox_gateway::routes::build_router(broker.client());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });

    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/messages", addr))
        .json(&serde_json::json!({
            "model": "anthropic/claude-sonnet-4-20250514",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(body.contains("event: error") || body.contains("\"error\""));
    assert!(body.contains("401"));
}
```

(A third case — missing API key — works the same way; the resolve_account step inside `per_request_task` returns Err early. Add it if the two above pass and you want more coverage; it's not blocking.)

- [ ] **Step 2: Run**

Run: `cargo test -p ox-gateway --test error_paths -- --nocapture`
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/ox-gateway/tests/error_paths.rs
git commit -m "test(ox-gateway): error paths — unknown role + upstream error frames"
```

### Task 5.4: Models aggregation test

**Files:**
- Create: `crates/ox-gateway/tests/models.rs`

- [ ] **Step 1: Write the test**

```rust
mod common;

use ox_broker::BrokerStore;
use ox_path::oxpath;
use ox_types::ModelInfo;
use std::time::Duration;
use structfs_serde_store::to_value;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn models_aggregates_across_accounts() {
    use ox_gate::{AccountConfig, ProviderConfig};
    use ox_store_util::LocalConfig;

    let broker = BrokerStore::new(Duration::from_secs(5));

    let mut config = LocalConfig::new();
    // Two accounts pointing at distinct providers
    config.set("gate/accounts/anthropic/provider", to_value(&"anthropic".to_string()).unwrap());
    config.set("gate/accounts/openai/provider", to_value(&"openai".to_string()).unwrap());
    config.set("gate/providers/anthropic", to_value(&ProviderConfig::anthropic()).unwrap());
    config.set("gate/providers/openai", to_value(&ProviderConfig::openai()).unwrap());
    broker.mount(oxpath!(""), config).await;

    // Pre-populate per-provider catalogs by writing through GateStore
    let gate = ox_gate::GateStore::new().with_config(Box::new(broker.handle("")));
    broker.mount(oxpath!("gate"), gate).await;

    let client = broker.client();
    let anth_catalog: Vec<ModelInfo> = vec![ModelInfo {
        id: "claude-sonnet-4-20250514".into(),
        display_name: "Claude Sonnet 4".into(),
        max_context_size: None,
        max_output_tokens: None,
        source: ox_types::ModelInfoSource::Server,
    }];
    let oai_catalog: Vec<ModelInfo> = vec![ModelInfo {
        id: "gpt-4o".into(),
        display_name: "GPT-4o".into(),
        max_context_size: None,
        max_output_tokens: None,
        source: ox_types::ModelInfoSource::Server,
    }];
    client.write_typed(&oxpath!("gate", "providers", "anthropic", "models"), &anth_catalog).await.unwrap();
    client.write_typed(&oxpath!("gate", "providers", "openai", "models"), &oai_catalog).await.unwrap();

    let app = ox_gateway::routes::build_router(broker.client());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });

    let resp = reqwest::Client::new()
        .get(format!("http://{}/v1/models", addr))
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    let ids: Vec<String> = body["data"].as_array().unwrap().iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect();

    assert!(ids.contains(&"anthropic/claude-sonnet-4-20250514".to_string()));
    assert!(ids.contains(&"openai/gpt-4o".to_string()));
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p ox-gateway --test models -- --nocapture`
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/ox-gateway/tests/models.rs
git commit -m "test(ox-gateway): /v1/models aggregates across accounts"
```

---

## Final verification

- [ ] **Workspace build**

Run: `cargo build --workspace`
Expected: zero errors.

- [ ] **Workspace test**

Run: `cargo test --workspace`
Expected: all tests pass (or pre-existing failures unrelated to this work; note them in the PR).

- [ ] **Format + lint**

Run: `./scripts/fmt.sh` (if present) and `cargo clippy --workspace --all-targets`
Expected: clean.

- [ ] **Manual smoke test against a real provider**

Set up an Anthropic key in `~/.ox/keys.json`, start the gateway:

```bash
cargo run -p ox-gateway
```

In another terminal:

```bash
curl -N -X POST http://127.0.0.1:11343/v1/messages \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "anthropic/claude-haiku-4-5-20251001",
    "max_tokens": 64,
    "messages": [{"role": "user", "content": "say hi"}],
    "stream": true
  }'
```

Expected: SSE stream with Anthropic-shaped frames. Check `~/.ox/usage.jsonl` after — one new line should have appeared.

- [ ] **Final commit + push for PR**

If everything is green:

```bash
git push -u origin worktree-ox-gateway
gh pr create --title "ox-gateway: local LLM gateway on the StructFS substrate" --body-file docs/superpowers/specs/2026-05-24-ox-gateway-design.md
```

(Or skip `gh` if not wired up — just push and create the PR manually.)

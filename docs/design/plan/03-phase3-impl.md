# Phase 3 Implementation Plan: Three-Phase Kernel

## Context

Phase 2 (`8e90d83`) gave us `GateStore` managing providers/accounts/catalogs via
StructFS, and collapsed ox-web's fetch functions to dispatch by
`ProviderConfig`. But ox-web still duplicates the entire agentic loop because
the kernel's `run_turn` is generic over `Transport` — a synchronous trait that
can't cross the async fetch boundary in wasm.

Phase 3 splits the kernel into three composable phases so the caller controls
the async yield point. This eliminates Transport/EventStream entirely, removes
`stream_once`, and lets both native and wasm drive the same kernel methods.

## Design

### Three kernel phases

```
initiate_completion(context) → CompletionRequest
  ↓ (caller does HTTP fetch — sync or async)
consume_events(events, emit) → Vec<ContentBlock>
  ↓
complete_turn(context, content) → Vec<ToolCall>
```

`run_turn` composes all three in a loop for native callers. Wasm callers drive the loop with `await` between phases.

### Simplification vs design doc

The design doc proposes handle-based indirection (gate/handles/{id}, pagination, WasmHttpStore). That machinery adds complexity for no immediate benefit — responses are fully buffered today. We take the three-phase decomposition (the real value) without the handle protocol. Handle protocol can land as Phase 3.5 when streaming is needed.

### complete_turn does NOT execute tools

`complete_turn` writes the assistant message to history and returns tool calls. The caller executes tools (important: ox-web needs to dispatch to both Rust and JS tools) and writes results. This matches the current ox-web pattern.

## Execution

### Step 1: ox-kernel
- Remove `Transport` trait, `EventStream` trait, `stream_once`
- Change `accumulate_response`: take `Vec<StreamEvent>` instead of `&mut dyn EventStream`
- Add `initiate_completion(&mut self, context: &mut dyn Store) -> Result<CompletionRequest, String>`
- Add `consume_events(&mut self, events: Vec<StreamEvent>, emit: &mut dyn FnMut(AgentEvent)) -> Result<Vec<ContentBlock>, String>`
- Add `complete_turn(&mut self, context: &mut dyn Store, content: &[ContentBlock]) -> Result<Vec<ToolCall>, String>` — writes assistant msg, returns tool calls
- Rewrite `run_turn`: `send: &dyn Fn(&CompletionRequest) -> Result<Vec<StreamEvent>, String>` replaces `T: Transport`

### Step 2: ox-core
- `Agent<T: Transport>` → `Agent`
- Store transport as `Box<dyn Fn(&CompletionRequest) -> Result<Vec<StreamEvent>, String>>`
- Update constructor, prompt(), re-exports

### Step 3: ox-web
- Delete `PreloadedTransport`, `BufferedStream`
- Rewrite `run_agentic_loop` using three phases with async fetch between initiate and consume
- Remove unused imports

### Step 4: ox-wasi, ox-emscripten
- Remove Transport/EventStream from re-exports

### Verification
```bash
cargo check --workspace
cargo check --target wasm32-unknown-unknown -p ox-web
cargo test --workspace
cargo clippy --workspace
```

## Files
- `crates/ox-kernel/src/lib.rs` — major
- `crates/ox-core/src/lib.rs` — moderate
- `crates/ox-web/src/lib.rs` — moderate
- `crates/ox-wasi/src/lib.rs` — trivial
- `crates/ox-emscripten/src/lib.rs` — trivial

## Deferred
- Handle-based completion protocol — Phase 3.5
- WasmHttpStore — Phase 3.5
- Codec trait in ox-gate — not needed yet
- Tool::execute context parameter — Phase 4
- Moving fetch into ox-gate — Phase 3.5

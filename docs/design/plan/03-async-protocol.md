# Phase 3: Async Protocol + HTTP Store + Kernel Changes

## Goal

Gate implements write-handle-read completion protocol. Kernel drops Transport,
gains three-phase methods. Tool::execute gains context parameter.

## Checklist

### Codec trait
- [ ] `crates/ox-gate/src/codec/traits.rs` — `Codec` trait (`encode_request`, `decode_chunk`), `HttpRequest` struct
- [ ] `crates/ox-gate/src/codec/anthropic.rs` — `AnthropicCodec` implementing `Codec` (wraps Phase 1 functions)
- [ ] `crates/ox-gate/src/codec/openai.rs` — `OpenAiCodec` implementing `Codec`

### Completion handle
- [ ] `crates/ox-gate/src/handle.rs` — `CompletionHandle` struct (gate_id, http_handle_path, codec, events, cursor, done, usage)

### HTTP Store
- [ ] `crates/ox-web/src/http_store.rs` — `WasmHttpStore` (two-phase: write queues, take_pending/deliver_response/deliver_chunk)

### Gate handle protocol
- [ ] `crates/ox-gate/src/lib.rs` — `accounts/{name}/complete` write path
- [ ] `crates/ox-gate/src/lib.rs` — `handles/{id}` read paths with pagination
- [ ] `crates/ox-gate/src/lib.rs` — `handles/{id}/usage` read path
- [ ] `crates/ox-gate/src/lib.rs` — http_store wiring

### Kernel changes
- [ ] `crates/ox-kernel/src/lib.rs` — **Tool trait**: add `context: &mut dyn Store` to `execute`
- [ ] `crates/ox-kernel/src/lib.rs` — **Remove** Transport, EventStream, stream_once
- [ ] `crates/ox-kernel/src/lib.rs` — **Add** `initiate_completion`, `consume_completion`, `complete_turn`
- [ ] `crates/ox-kernel/src/lib.rs` — **Rewrite** `run_turn` to compose from phases
- [ ] `crates/ox-kernel/src/lib.rs` — **Modify** `accumulate_response` to take `Vec<StreamEvent>`

### ox-core changes
- [ ] `crates/ox-core/src/lib.rs` — remove `T: Transport` generic from `Agent<T>`
- [ ] `crates/ox-core/src/lib.rs` — remove transport param from constructor
- [ ] `crates/ox-core/src/lib.rs` — update `prompt` to call `run_turn` without transport
- [ ] `crates/ox-core/src/lib.rs` — remove Transport/EventStream from re-exports

### ox-web changes
- [ ] Remove PreloadedTransport/BufferedStream
- [ ] Remove fetch_anthropic_completion, fetch_openai_completion
- [ ] Add generic `fetch_http(HttpRequest)`
- [ ] Rewrite `run_agentic_loop` to use phase-based kernel API
- [ ] Update `execute_tool` to pass context

### Stub crate updates
- [ ] `crates/ox-wasi/src/lib.rs` — update re-exports for Agent without generic
- [ ] `crates/ox-emscripten/src/lib.rs` — update re-exports for Agent without generic

## Test Plan

- Codec trait implementations (encode/decode roundtrips)
- Gate handle protocol (write complete → read events → pagination)
- WasmHttpStore two-phase protocol
- Kernel phase methods with mock stores
- Existing kernel tests updated for new Tool::execute signature

## Verification Gates

```bash
cargo check --workspace
cargo check --target wasm32-unknown-unknown -p ox-web
cargo test --workspace
cargo clippy --workspace -- -D warnings
wasm-pack build crates/ox-web --target web --out-dir ../../target/wasm-pkg
```

## Scratch Space

(implementation notes go here)

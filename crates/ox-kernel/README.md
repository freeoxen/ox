# ox-kernel

Core types, state machine, and agentic loop for the [ox](https://github.com/freeoxen/ox) agent framework.

## What's in the box

- **Message types** — `Message`, `ContentBlock`, `ToolCall`, `ToolResult`
- **Completion protocol** — `CompletionRequest`, `StreamEvent`, `EventStream`
- **Tool abstraction** — `Tool` trait and `ToolRegistry`
- **Transport abstraction** — `Transport` trait for pluggable LLM backends
- **Kernel** — the agentic loop state machine, reads prompts from a `Store` and writes results back
- **StructFS re-exports** — `Reader`, `Writer`, `Store`, `Path`, `Value`, `Record`, `path!`

## Design

The kernel is synchronous and transport-agnostic. It reads a fully-assembled `CompletionRequest` from a `Store` (via `path!("prompt")`), streams a response through the `Transport`, executes tool calls, writes results back, and loops until the model produces no more tool calls.

This keeps the kernel portable across native, Wasm, and WASI targets. The async boundary lives in the shell (e.g. `ox-web`), not here.

## Usage

Most consumers should depend on [`ox-core`](https://crates.io/crates/ox-core), which re-exports everything from this crate and provides the high-level `Agent` type. Depend on `ox-kernel` directly only if you're building a custom agent composition.

## License

Apache-2.0 — see [LICENSE](../../LICENSE).

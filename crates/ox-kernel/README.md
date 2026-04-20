# ox-kernel

Core types, state machine, and agentic loop for the [ox](https://github.com/freeoxen/ox) agent framework.

## What's in the box

- **Message types** — `Message`, `ContentBlock`, `ToolCall`, `ToolResult`
- **Completion protocol** — `CompletionRequest`, `StreamEvent`, `EventStream`
- **Kernel** — the agentic loop state machine, reads prompts from a `Store` and writes results back
- **StructFS re-exports** — `Reader`, `Writer`, `Store`, `Path`, `Value`, `Record`, `path!`

## Design

The kernel is synchronous and transport-agnostic. It reads a fully-assembled `CompletionRequest` from a `Store` (via `path!("prompt")`), streams a response through the `Transport`, executes tool calls, writes results back, and loops until the model produces no more tool calls.

This keeps the kernel portable across native, Wasm, and WASI targets. The async boundary lives in the shell (e.g. `ox-web`), not here.

## Execution shape (read this before writing a plan)

A **turn** is one call to `run_turn(context, emit)` at [`src/run.rs`](src/run.rs). It is a synchronous function that runs to completion and returns. It is not a "worker" or "coroutine"; there is no persistent entity that outlives a turn.

When a tool requires approval, the kernel writes `approval/request` to the namespace and the host-function bridge (`ox-runtime`) blocks the Wasm-module thread on a `oneshot::Receiver<Decision>` via `rt_handle.block_on`. When the user responds, the oneshot resolves, the thread unparks, and `run_turn` continues. If the process dies while the thread is parked, nothing survives except what the log already holds — there is no future to "rehydrate."

Log entries produced during a turn are written to `log/append`, which routes to `LogStore::write` and on to `SharedLog::append` ([`src/log.rs`](src/log.rs)). `SharedLog::append` returns `Result<(), StoreError>` and is fallible: when a `Durability` sink is installed (typically `ox_inbox::ledger_writer::LedgerWriterHandle`), the commit to `ledger.jsonl` happens inside `append`'s critical section before the entry is observable to readers. When no sink is installed (e.g. during replay), `append` is in-memory only. See [`docs/architecture/life-of-a-log-entry.md`](../../docs/architecture/life-of-a-log-entry.md) and [`docs/architecture/save-and-restore.md`](../../docs/architecture/save-and-restore.md) for the full durability story.

**If you are planning changes to durability, resumption, or approval flow:** read [`docs/architecture/life-of-a-log-entry.md`](../../docs/architecture/life-of-a-log-entry.md) and [`docs/architecture/data-model.md`](../../docs/architecture/data-model.md) first. Plans that rely on a mental model of "async LogStore" or "parked agent worker" do not match current code.

## Usage

Most consumers should depend on [`ox-core`](https://crates.io/crates/ox-core), which re-exports everything from this crate and provides the high-level `Agent` type. Depend on `ox-kernel` directly only if you're building a custom agent composition.

## License

Apache-2.0 — see [LICENSE](../../LICENSE).

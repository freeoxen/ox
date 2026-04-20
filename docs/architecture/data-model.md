# Data Model Reference

A map of the types that cross durability, approval, and log boundaries. Each entry names the type, its crate, its purpose, and its lifetime — so type names that sound similar don't get conflated.

> **For plan authors:** read this before writing about `ApprovalRequest`, `ApprovalRequested`, `LogEntry`, `LedgerEntry`, or similar. The conflation of runtime types with their log-variant cousins has historically produced incorrect plans.

Cross-references use `file:line` where verification is cheap.

---

## Approval types — three distinct shapes

These three names refer to **three different types**. They are related but not interchangeable.

### `ox_types::ApprovalRequest` — runtime, in-memory

- **Location**: `crates/ox-types/src/approval.rs:3–8`
- **Fields**:
  ```rust
  pub struct ApprovalRequest {
      pub tool_name: String,
      pub tool_input: serde_json::Value,  // FULL input, not a preview
  }
  ```
- **Lifetime**: Lives in `ApprovalStore.pending` (`crates/ox-ui/src/approval_store.rs:12`). Cleared when the user decides.
- **Persistence**: Not persisted. A crash loses it. Recovery must reconstruct it (see `life-of-a-log-entry.md` → "Approval resumption").

### `LogEntry::ApprovalRequested` — log variant, durable

- **Location**: `crates/ox-kernel/src/log.rs:98–102`
- **Fields**:
  ```rust
  ApprovalRequested {
      tool_name: String,
      input_preview: String,  // truncated for display — NOT the full input
  },
  ```
- **Lifetime**: Written once, persists in `ledger.jsonl` forever.
- **Write site**: `crates/ox-cli/src/thread_registry.rs:281–292` — `ThreadNamespace::write` routes `approval/request` through here, computing `input_preview` by peeking at `tool_input.path` or `tool_input.command`.
- **Gap**: This variant loses the full `tool_input`. Recovering a pending approval from the log alone cannot reconstruct a complete `ApprovalRequest`. Plans that propose "rehydrate the approval from the log" must address this gap (extend the variant with `tool_input: serde_json::Value` or read the input from the matching `ToolCall` entry).

### `ApprovalResponse` / `Decision` — the user's answer

- **Location**: `crates/ox-types/src/approval.rs:10–55`
- `Decision` is a `Copy + PartialEq + Eq` enum: `AllowOnce`, `AllowSession`, `AllowAlways`, `DenyOnce`, `DenySession`, `DenyAlways`.
- Delivered through `ApprovalStore::deferred_tx: Option<tokio::sync::oneshot::Sender<Decision>>` (`approval_store.rs:13`).
- **Does not have a "cancel turn" variant today.** Any plan that needs "user cancels" must decide whether to extend `Decision` or add a sibling signal.

---

## Log types — in-memory vs on-disk

### `LogEntry` — enum of structured log events

- **Location**: `crates/ox-kernel/src/log.rs:23–116`
- **Variants (at time of writing — verify before claiming exhaustive)**: `User`, `Assistant`, `ToolCall`, `ToolResult`, `Meta`, `TurnStart`, `TurnEnd`, `CompletionEnd`, `ApprovalRequested`, `ApprovalResolved`, `Error`. 11 variants.
- `Meta { data: serde_json::Value }` is an open-payload escape hatch. Exhaustive `match` works at the enum level, but `Meta` content is untyped.
- Not `#[non_exhaustive]`.

### `SharedLog` — in-memory projection (with optional durability sink)

- **Location**: `crates/ox-kernel/src/log.rs:138` (struct), `:128` (`Durability` trait), `:148–211` (impl).
- `SharedLog { inner: Arc<Mutex<SharedLogInner>> }` where `SharedLogInner { entries: Vec<LogEntry>, durability: Option<Arc<dyn Durability>> }`. The single mutex covers both so append-order equals observation-order under concurrent writers.
- Methods: `append(&self, entry) -> Result<(), StoreError>` (fallible, sync, line 186), `entries()`, `len()`, `last_n(n)`, `with_durability(sink)` (line 163), `clear_durability()` (line 171).
- **Durability trait** (line 128): `pub trait Durability: Send + Sync { fn commit(&self, entry: &LogEntry) -> Result<(), StoreError>; }`. The concrete impl lives in `ox-inbox` (`ledger_writer::LedgerWriterHandle`) — `ox-kernel` holds only the trait to keep the crate dependency pointed one way.
- **When a sink is installed**, `append` calls `sink.commit(&entry)` inside the critical section; the entry is pushed to `entries` only on success. Commit failure propagates the `StoreError` and the entry is **not** pushed.
- **Lifetime**: In-process. Cleared on CLI exit. Reconstructed on relaunch by replaying `ledger.jsonl` through `log/append` — replay must run **before** `with_durability` is called, otherwise replayed entries are re-persisted.

### `LogStore` — StructFS `Writer` facade over `SharedLog`

- **Location**: `crates/ox-kernel/src/log.rs` (struct) — `impl Writer for LogStore` sync `write(&mut self, to, data)` propagates `SharedLog::append`'s `Result` via `?`.
- **Does not touch the ledger file directly.** When `SharedLog` has a durability sink installed, the sink is what writes the ledger — `LogStore::write` just funnels into `SharedLog::append`, which in turn calls the sink.

### `LedgerEntry` — on-disk envelope

- **Location**: `crates/ox-inbox/src/ledger.rs:10–15`
- **Fields**:
  ```rust
  pub struct LedgerEntry {
      pub seq: u64,
      pub hash: String,       // SHA-256 truncated to 16 hex chars
      pub parent: Option<String>,
      pub msg: serde_json::Value,  // the original LogEntry, serialized
  }
  ```
- `msg` is a serialized `LogEntry`, wrapped in this envelope.
- Each entry's `hash` is computed by `ledger::entry_hash(msg)` (line 18) — **content-addressed on the `msg` alone**. `parent` links to the previous entry's `hash`, forming a chain.
- **Ledger file format**: JSONL. Each line: `{"seq": N, "hash": "...", "parent": "...", "msg": {...}}`.
- **Not the same as writing `LogEntry` as JSONL.** Any plan that says "write the log entry to disk" must account for the envelope and the hash chain.

### `SaveResult` — return type of `save_thread_state`

- **Location**: `crates/ox-inbox/src/snapshot.rs:19–27`
- **Fields**: `last_seq`, `last_hash`, `message_count` (count of user+assistant entries).
- Used by the inbox indexer to keep SQLite listings fresh mid-session.

---

## File artifacts per thread

Each thread directory (`~/.ox/threads/{thread_id}/`) holds:

| File | Owner | Contents |
|---|---|---|
| `context.json` | `ox-inbox/src/snapshot.rs:65–75` | `ContextFile { version, thread_id, title, labels, created_at, updated_at, stores }` where `stores` is a map of snapshot states from `PARTICIPATING_MOUNTS` (`["system", "gate"]`). Written by `save_thread_state`. Overwritten each save. |
| `view.json` | `ox-inbox/src/thread_dir.rs` | UI metadata. Written by `save_thread_state` only if missing. |
| `ledger.jsonl` | `ox-inbox/src/ledger.rs:28` (`append_entry`) | JSONL of `LedgerEntry`. Append-only. Parent-hash chained. |

---

## Kernel execution types

### `run_turn(context: &mut dyn Store, emit: &mut dyn FnMut(AgentEvent)) -> Result<(), String>`

- **Location**: `crates/ox-kernel/src/run.rs:597`
- **Synchronous.** Called once per turn.
- Invoked from `crates/ox-cli/src/agents.rs:437` via `module.run(host_store)` — the Wasm-module boundary. Wasmtime uses `Engine::default()` (no async feature).
- **There is no "agent worker" or "parked coroutine."** A turn is one call to `run_turn`. When it needs approval, the Wasm-module thread blocks inside a host import function via `rt_handle.block_on(...)`. When the turn ends (naturally or via error), the thread returns. Until the next user input, there is no running kernel.

### `AgentEvent` — what `run_turn` emits

- **Location**: `crates/ox-kernel/src/lib.rs` (serialized via `agent_event_to_json`)
- Variants include `TurnStart`, `TextDelta`, `ToolCallStart`, `ToolCallResult`, `TurnEnd`, `Error`.
- The emit callback is how the kernel talks to the host. Log entries are written separately through the `Store` interface.

---

## Store traits — sync vs async

Two trait families, two worlds. Plans that conflate them produce incorrect durability stories.

### Sync — `structfs_core_store`

- `Reader::read(&mut self, from: &Path) -> Result<Option<Record>, Error>`
- `Writer::write(&mut self, to: &Path, data: Record) -> Result<Path, Error>`
- Used by kernel, LogStore, in-process stores.
- Blocking. Called from Wasm host functions, test harnesses, anywhere without a runtime constraint.

### Async — `ox_broker::async_store`

- **Location**: `crates/ox-broker/src/async_store.rs:11–20`
- `AsyncReader::read` returns `BoxFuture<Result<Option<Record>, Error>>`.
- `AsyncWriter::write` returns `BoxFuture<Result<Path, Error>>`.
- Used by broker dispatch, UI-side store access, anything needing tokio-level concurrency.
- `ApprovalStore` implements `AsyncReader` + `AsyncWriter` (writes from the TUI land here).

### Bridge

The broker has a sync/async adapter (`crates/ox-broker/src/sync_adapter.rs`). The broker's client (`ClientHandle`) is async; it dispatches into mounted stores which may be sync or async.

**When a plan proposes "make `X::append` async" or "durable-sync the write path," it must specify which trait layer the change lives at.** Otherwise the architecture is ambiguous.

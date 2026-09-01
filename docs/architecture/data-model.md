# Data Model Reference

A map of types crossing durability, approval, log, and Store boundaries. Similar
names here are distinct shapes; plans must not substitute one for another.

Verified against the repository on 2026-08-31 with:

```sh
rg -n "struct ApprovalRequest|enum Decision|enum LogEntry|struct SharedLog|trait Durability" crates/ox-types crates/ox-kernel
rg -n "struct LedgerEntry|struct SaveResult|struct ContextFile|enum LedgerHealth" crates/ox-inbox
rg -n "trait AsyncReader|trait AsyncWriter|pub fn run_turn|enum AgentEvent" crates/ox-broker crates/ox-kernel
```

## Approval types

### `ox_types::ApprovalRequest` — runtime state

- Location: `crates/ox-types/src/approval.rs:3-8`.
- Fields: `tool_name` and full `tool_input: serde_json::Value`.
- Lifetime: held in `ApprovalStore.pending` and paired with a process-local
  oneshot sender (`crates/ox-ui/src/approval_store.rs:11-20`).
- Persistence: none. Crash recovery reconstructs it from durable log entries.

### `LogEntry::ApprovalRequested` — durable event

- Location: `crates/ox-kernel/src/log.rs:125-143`.
- Fields: `tool_name`, display-only `input_preview`, and
  `post_crash_reconfirm` (default false and omitted when false).
- It does not contain the full tool input. Recovery joins it to the nearest
  matching `ToolCall { id, name, input }` (`log.rs:70-77`).
- Write routing is owned by `ThreadNamespace::write`; the log event and runtime
  `ApprovalRequest` are related but not interchangeable.

### `ApprovalResponse` and `Decision` — user answer

- Location: `crates/ox-types/src/approval.rs:10-73`.
- `Decision` variants are `AllowOnce`, `AllowSession`, `AllowAlways`,
  `DenyOnce`, `DenySession`, `DenyAlways`, and `CancelTurn`.
- `CancelTurn` is neither allow nor deny. Exhaustive callers must handle it
  directly; `is_allow()` and `is_deny()` both return false.
- Normal runtime delivery uses `ApprovalStore.deferred_tx`; durable recovery
  records the surrounding request/resolution/abort log shapes.

## Log and ledger types

### `LogEntry` — structured conversation event

- Location: `crates/ox-kernel/src/log.rs:47-207`.
- Current variants: `User`, `Assistant`, `ToolCall`, `ToolResult`, `Meta`,
  `TurnStart`, `TurnEnd`, `CompletionEnd`, `ApprovalRequested`,
  `ApprovalResolved`, `Error`, `TurnAborted`, `ToolAborted`, and
  `AssistantProgress`.
- `Meta` has an open `serde_json::Value` payload. The enum itself is not marked
  `non_exhaustive`, so state-machine matches should remain explicit.
- `ToolCall` is the round-trippable durable source of tool name/input.

### `SharedLog` and `Durability` — memory plus commit seam

- Location: `crates/ox-kernel/src/log.rs:209-300`.
- `SharedLogInner` contains `entries: Vec<LogEntry>` and an optional
  `Arc<dyn Durability>` under one mutex (`log.rs:223-237`).
- `Durability::commit` is synchronous and fallible (`log.rs:209-220`).
- `SharedLog::append` commits while holding the ordering mutex, then publishes
  the entry only on success (`log.rs:266-283`).
- Replay runs without a sink; `with_durability` is installed afterward.

### `LogStore` — StructFS facade

`LogStore` implements synchronous StructFS `Reader`/`Writer`. Its append path
deserializes a `LogEntry` and funnels it through `SharedLog::append`. It does not
open a ledger file itself; the installed durability sink owns that I/O.

### `LedgerEntry` — JSONL disk envelope

- Location: `crates/ox-inbox/src/ledger.rs:8-15`.
- Fields: `seq: u64`, truncated SHA-256 `hash`, optional parent hash, and
  `msg: serde_json::Value` containing the serialized `LogEntry`.
- File format: one JSON object per line in `ledger.jsonl`.
- `ledger::append_entry` constructs the next sequence/parent envelope at
  `ledger.rs:102-140`; live ownership belongs to `LedgerWriter`, not snapshot
  code.

### `LedgerHealth` — mount/write health

- Location: `crates/ox-inbox/src/ledger.rs:17-54`.
- Variants: `Ok`, `Missing`, `RepairFailed`, `Degraded`.
- Missing/repair-failed/degraded conversations surface explicit health and do
  not silently claim writable durability.

### `SaveResult` — cumulative commit projection

- Location: `crates/ox-inbox/src/snapshot.rs:27-41`.
- Fields: `last_seq`, optional `last_hash`, and user/assistant
  `message_count`.
- It is published through `LedgerWriterHandle::latest_save_result`
  (`crates/ox-inbox/src/ledger_writer.rs:158-171`), then a per-thread
  `CommitDrain` forwards it to inbox metadata. It is not returned by a
  `save_thread_state` function.

## Files per thread

Each `~/.ox/threads/{thread_id}/` directory contains:

| File | Authoritative writer | Contents/cadence |
|---|---|---|
| `ledger.jsonl` | one `LedgerWriter` | hash-chained `LedgerEntry`; every live log append |
| `context.json` | `snapshot::save_config_snapshot` | `ContextFile` metadata plus `system`/`gate` snapshots; turn boundary |
| `view.json` | `snapshot::write_default_view_if_missing` | projection metadata; once during mount if absent |

`ContextFile` lives at `crates/ox-inbox/src/thread_dir.rs:7-20`.
`save_config_snapshot` writes it at `crates/ox-inbox/src/snapshot.rs:43-94`.
`view.json` bootstrap is at `snapshot.rs:96-104`.

Restore reads config and ledger with durability disabled, then installs the
writer. See [`save-and-restore.md`](save-and-restore.md).

## Kernel execution types

### `run_turn`

- Location: `crates/ox-kernel/src/run.rs:1142`.
- Signature: synchronous `run_turn(context: &mut dyn Store, emit: &mut dyn
  FnMut(AgentEvent)) -> Result<(), String>`.
- The CLI invokes the Wasm boundary through `AgentModule::run` at
  `crates/ox-cli/src/agents.rs:735`.
- Approval may block the conversation's execution thread through the async
  Store bridge. This does not preserve a coroutine across process death;
  durable restart decisions come from the log classifier and run-turn resume
  prologue.

### `AgentEvent`

- Location: `crates/ox-kernel/src/lib.rs:175`.
- Transient host/UI emission is separate from durable `LogEntry` writes. A
  remote event projection must name which source it is projecting; it must not
  assume every `AgentEvent` is independently durable.

## Store trait families

### Synchronous StructFS

- `Reader::read(&mut self, &Path) -> Result<Option<Record>, Error>`.
- `Writer::write(&mut self, &Path, Record) -> Result<Path, Error>`.
- Used by kernel and in-process Stores. Calls may block.

### Repository-local asynchronous StructFS

- Location: `crates/ox-broker/src/async_store.rs:1-20`.
- `AsyncReader` and `AsyncWriter` preserve StructFS `Path`, `Record`, and
  `Error` while returning `Send + 'static` futures.
- Broker `mount_async` independently spawns request futures; public cursor and
  remote Stores must use this seam so a parked request does not stall a mount.

### StructFS transport values

The pinned StructFS `Value` and `Record` enums are non-exhaustive. Current
`Value` shapes include null, bool, signed i64, f64, string, bytes, array, and
string-keyed map. `Record` is raw bytes plus format or parsed `Value`. A wire
codec must preserve all current shapes and reject unsupported future shapes
explicitly; JSON-only conversion is lossy.

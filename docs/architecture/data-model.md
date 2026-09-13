# Data Model Reference

A map of types crossing durability, approval, log, and Store boundaries. Similar
names here are distinct shapes; plans must not substitute one for another.

Verified against the repository on 2026-09-01 with:

```sh
rg -n "struct ApprovalRequest|enum Decision|enum LogEntry|struct SharedLog|trait Durability" crates/ox-types crates/ox-kernel
rg -n "struct LedgerEntry|struct SaveResult|struct ContextFile|enum LedgerHealth" crates/ox-inbox
rg -n "trait AsyncReader|trait AsyncWriter|pub fn run_turn|enum AgentEvent" crates/ox-broker crates/ox-kernel
rg -n "CreateEnvelope|PromptEnvelope|DecisionEnvelope|CancelEnvelope|accepted_seq" crates/ox-inbox/src/worker_ingress.rs crates/ox-inbox/src/schema.rs
```

## Approval types

### `ox_types::ApprovalRequest` — runtime state

- Location: `crates/ox-types/src/approval.rs:4-9`.
- Fields: `tool_name` and full `tool_input: serde_json::Value`.
- Lifetime: held in `ApprovalStore.pending` and paired with a process-local
  oneshot sender (`crates/ox-ui/src/approval_store.rs:11-31`).
- Persistence: none. Crash recovery reconstructs it from durable log entries.

### `LogEntry::ApprovalRequested` — durable event

- Location: `crates/ox-kernel/src/log.rs:130-148`.
- Fields: `tool_name`, display-only `input_preview`, and
  `post_crash_reconfirm` (default false and omitted when false).
- It does not contain the full tool input. Recovery joins it to the nearest
  matching `ToolCall { id, name, input }` (`log.rs:70-77`).
- Write routing is owned by `ThreadNamespace::write`; the log event and runtime
  `ApprovalRequest` are related but not interchangeable.

### `ApprovalResponse` and `Decision` — user answer

- Location: `crates/ox-types/src/approval.rs:12-75`.
- `Decision` variants are `AllowOnce`, `AllowSession`, `AllowAlways`,
  `DenyOnce`, `DenySession`, `DenyAlways`, and `CancelTurn`.
- `CancelTurn` is neither allow nor deny. Exhaustive callers must handle it
  directly; `is_allow()` and `is_deny()` both return false.
- Normal runtime delivery uses `ApprovalStore.deferred_tx`; durable recovery
  records the surrounding request/resolution/abort log shapes.

## Log and ledger types

### `LogEntry` — structured conversation event

- Location: `crates/ox-kernel/src/log.rs:52-211`.
- Current variants: `User`, `Assistant`, `ToolCall`, `ToolResult`, `Meta`,
  `TurnStart`, `TurnEnd`, `CompletionEnd`, `ApprovalRequested`,
  `ApprovalResolved`, `Error`, `TurnAborted`, `ToolAborted`, and
  `AssistantProgress`.
- `Meta` has an open `serde_json::Value` payload. The enum itself is not marked
  `non_exhaustive`, so state-machine matches should remain explicit.
- `ToolCall` is the round-trippable durable source of tool name/input.

### `SharedLog` and `Durability` — memory plus commit seam

- Location: `crates/ox-kernel/src/log.rs:221-313`.
- `SharedLogInner` contains `entries: Vec<LogEntry>` and an optional
  `Arc<dyn Durability>` under one mutex (`log.rs:232-246`).
- `Durability::commit` is synchronous and fallible (`log.rs:221-231`).
- `SharedLog::append` commits while holding the ordering mutex, then publishes
  the entry only on success (`log.rs:279-296`).
- Replay runs without a sink; `with_durability` is installed afterward.

### `LogStore` — StructFS facade

`LogStore` implements synchronous StructFS `Reader`/`Writer`. Its append path
deserializes a `LogEntry` and funnels it through `SharedLog::append`. It does not
open a ledger file itself; the installed durability sink owns that I/O.

### `LedgerEntry` — JSONL disk envelope

- Location: `crates/ox-inbox/src/ledger.rs:10-17`.
- Fields: `seq: u64`, truncated SHA-256 `hash`, optional parent hash, and
  `msg: serde_json::Value` containing the serialized `LogEntry`.
- File format: one JSON object per line in `ledger.jsonl`.
- `ledger::append_entry` constructs the next sequence/parent envelope at
  `ledger.rs:102-140`; live ownership belongs to `LedgerWriter`, not snapshot
  code.

### `LedgerHealth` — mount/write health

- Location: `crates/ox-inbox/src/ledger.rs:25-62`.
- Variants: `Ok`, `Missing`, `RepairFailed`, `Degraded`.
- Missing/repair-failed/degraded conversations surface explicit health and do
  not silently claim writable durability.

### `SaveResult` — cumulative commit projection

- Location: `crates/ox-inbox/src/snapshot.rs:33-47`.
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

`ContextFile` lives at `crates/ox-inbox/src/thread_dir.rs:9-22`.
`save_config_snapshot` writes it at `crates/ox-inbox/src/snapshot.rs:50-97`.
`view.json` bootstrap is at `snapshot.rs:99-107`.

Restore reads config and ledger with durability disabled, then installs the
writer. See [`save-and-restore.md`](save-and-restore.md).

## Kernel execution types

### `run_turn`

- Location: `crates/ox-kernel/src/run.rs:1142`.
- Signature: synchronous `run_turn(context: &mut dyn Store, emit: &mut dyn
  FnMut(AgentEvent)) -> Result<(), String>`.
- The executor invokes the cancellable Wasm boundary through
  `AgentModule::run_with_cancellation` at
  `crates/ox-executor/src/agents.rs:2065`.
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

### Detached asynchronous StructFS

- Upstream `DetachedReader::read_detached` and
  `DetachedWriter::write_detached` preserve StructFS `Path`, `Record`, and
  `Error` while returning `Send + 'static` futures that do not borrow the store.
- The broker's `async_store` module now only reexports a generic boxed-future
  utility. Its former local store traits have been removed.
- Broker `mount_async` independently spawns request futures; public cursor and
  remote Stores must use this seam so a parked request does not stall a mount.
- Spawned stores retain an explicit `'static` bound. ClientHandle also implements
  the upstream detached traits. Verified with
  `rg -n 'DetachedReader|DetachedWriter|mount_async' crates/ox-broker/src/{client,server,lib}.rs`
  at client.rs:411/421, server.rs:94 and lib.rs:341 during cleanup.

Upstream borrowing `AsyncReader::read_async` and `AsyncWriter::write_async` are
different interfaces: their futures retain the mutable store borrow. Remote
transport clients support both families, but broker server scheduling uses the
detached family.

### Path serialization

StructFS 0.2 implements Path Serde as a slash-separated string. Existing
Ox/Horns record fields retain component arrays through the shared
`horns_core::path_serde` adapter; ox-types reexports that implementation.
Removing field adapters would change persisted and transmitted shapes.

### StructFS transport values

The registry-pinned StructFS 0.2 `Value` and `Record` enums are non-exhaustive.
`Value` shapes include null, bool, signed i64, unsigned u64, f64, string, bytes,
array, and string-keyed map. Normalized integers use the unsigned variant only
above `i64::MAX`. `Record` is raw bytes plus format or parsed `Value`.

Ox wire v1 retains its original signed-i64 range: small unsigned values use the
existing integer encoding; larger unsigned values fail explicitly. Bytes and
non-finite floats remain supported by the wire codec. Plain JSON conversion is
fallible and rejects those values; persistence callers propagate that failure
before writing. Snapshot hashes retain the existing canonical JSON bytes for
supported states. See `crates/ox-structfs-transport/WIRE.md` and the migration
feedback document for the compatibility boundaries.

Verified during the September 13 migration with:

```sh
rg -n 'Unsigned' crates/ox-structfs-transport/src/frame.rs
cargo test -p ox-structfs-transport --test conformance --offline
cargo test -p ox-kernel snapshot --offline
cargo test -p ox-store-util --offline
```

## Worker ingress records

Worker ingress is durable idempotency metadata in the existing `InboxStore`,
not a second conversation model:

- `CreateEnvelope`, `PromptEnvelope`, `DecisionEnvelope`, and `CancelEnvelope`
  are defined at `crates/ox-inbox/src/worker_ingress.rs:18-43`.
- Four `ox.db` tables store typed record bytes, canonical request hashes,
  accepted/applied state, stable results, and one cross-kind `accepted_seq`
  (`crates/ox-inbox/src/schema.rs:36-77`).
- Canonical domain-separated hashes are built at
  `worker_ingress.rs:140-161`; acceptance and global replay ordering are at
  `worker_ingress.rs:236-288` and `:421-463`.
- The existing thread row, per-thread directory, `ledger.jsonl`, approval
  store, and executor worker remain authoritative. Ingress source markers use
  the existing open `LogEntry::Meta` payload only to bind semantic IDs and
  request hashes to following durable log evidence.

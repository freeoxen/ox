# Life of a Log Entry

Traces a `LogEntry` from creation to durable storage and back into memory. This
document describes the current flow; [`data-model.md`](data-model.md) describes
the participating types.

Verified against the repository on 2026-08-31 with:

```sh
rg -n "enum LogEntry|trait Durability|with_durability|pub fn append" crates/ox-kernel/src/log.rs
rg -n "log/append|ApprovalRequested|ApprovalResolved" crates/ox-kernel/src/run.rs crates/ox-cli/src/thread_registry.rs
rg -n "LedgerWriter::spawn|snapshot::restore|save_config_snapshot|resume_needed" crates/ox-cli/src/thread_registry.rs crates/ox-cli/src/agents.rs
```

## 1. Creation and append routing

`LogEntry` is the structured event enum at
`crates/ox-kernel/src/log.rs:47-207`. The kernel writes turn, assistant, tool,
usage, streaming-progress, and error events through the thread namespace's
`log/append` path. `LogStore::write` deserializes that record and calls
`SharedLog::append`.

Approval events have an additional routing step. `ThreadNamespace::write`
intercepts `approval/request` and `approval/response`, appends the corresponding
`LogEntry::ApprovalRequested` or `LogEntry::ApprovalResolved`, and forwards the
runtime request/decision to the approval store. The log event is durable state;
the pending oneshot is process-local coordination.

`ApprovalRequested` contains a display preview, not the round-trippable tool
input (`log.rs:125-143`). Crash recovery reconstructs the full input from the
preceding `ToolCall { id, name, input }` (`log.rs:70-77`).

## 2. Commit before visibility

`SharedLog` owns the in-memory entries and an optional durability sink under one
mutex (`crates/ox-kernel/src/log.rs:223-237`). Its invariant is:

> append order on disk equals observation order in memory.

`SharedLog::append` (`log.rs:266-283`) holds that mutex, calls
`Durability::commit`, and only pushes the entry after the commit succeeds. If
the commit fails, the entry is not visible and the `StoreError` propagates.
Without a sink, as during replay, append only updates memory.

`SharedLog::with_durability` (`log.rs:249-257`) must therefore be called after
replay. Installing it before replay would append every restored entry to the
ledger a second time.

## 3. Durable ledger writer

The CLI's concrete durability implementation is
`ox_inbox::ledger_writer::LedgerWriterHandle`. The kernel knows it only through
the `Durability` trait (`log.rs:209-220`), avoiding a reverse dependency from
`ox-kernel` to `ox-inbox`.

The writer contract is documented and implemented at
`crates/ox-inbox/src/ledger_writer.rs:1-38`:

- exactly one writer owns a conversation ledger;
- a dedicated OS thread serializes its hash chain;
- a commit is acknowledged only after `write_all` and `File::sync_data`;
- each request receives a synchronous acknowledgement;
- a latest-wins `SaveResult` slot reports cumulative sequence, hash, and
  message count to the inbox index;
- owner drop sends an ordered shutdown message and joins the writer thread.

`LedgerWriter::spawn` (`ledger_writer.rs:193-224`) reads the existing ledger head
before accepting commits. This continues the `seq`/`parent` chain rather than
starting a second chain after remount.

Each `ledger.jsonl` line is a `LedgerEntry` envelope around the serialized
`LogEntry`:

```jsonl
{"seq":0,"hash":"4a2b1c3d5e6f7890","parent":null,"msg":{"type":"user","content":"hi"}}
{"seq":1,"hash":"8b1c2d3e4f506172","parent":"4a2b1c3d5e6f7890","msg":{"type":"turn_start"}}
```

The ledger is not written by the config-snapshot path. `LedgerWriter` is the
single live-session writer.

## 4. Configuration snapshot is separate

`ox_inbox::snapshot::save_config_snapshot`
(`crates/ox-inbox/src/snapshot.rs:43-94`) writes only `context.json`. It reads
`system/snapshot/state` and `gate/snapshot/state`, preserves `created_at`, and
writes thread metadata plus those store snapshots.

`run_one_turn` invokes the wrapper after a run at
`crates/ox-cli/src/agents.rs:819-829`. This is a turn-boundary snapshot of
configuration. It is not the durability boundary for log entries.

`view.json` bootstrap is also separate:
`snapshot::write_default_view_if_missing` (`snapshot.rs:96-104`) runs during
thread mount, not after every turn.

## 5. Restore and mount ordering

`snapshot::restore` (`snapshot.rs:134-220`) performs two reads:

1. Rehydrate each participating store from `context.json` by writing its
   `snapshot/state` path.
2. Read `ledger.jsonl` with torn-tail repair and replay each envelope's `msg`
   through `log/append`.

`ThreadNamespace::from_thread_dir` pins the lifecycle:

1. construct stores with no durability sink;
2. replay `context.json` and `ledger.jsonl`, or take the ledger-only recovery
   path when a crash preceded the first config snapshot;
3. reconstruct derived history/session state;
4. spawn `LedgerWriter` and install its handle only when ledger health is
   `Ok` (`crates/ox-cli/src/thread_registry.rs:393-429`);
5. classify the restored log tail and durably append any required abort marker
   (`thread_registry.rs:443-554`).

Missing, unrecoverable, or degraded ledgers mount without a durability writer
and expose their health through `shell/ledger_health`; they do not silently
accept non-durable appends.

## 6. Crash and approval recovery

`ox_kernel::resume::classify` is a pure classifier over the log tail
(`crates/ox-kernel/src/resume.rs:65-230`). At mount it distinguishes idle,
interrupted stream, unresolved approval, interrupted tool dispatch, and a turn
that produced no progress.

The mount lifecycle records `TurnAborted` or `ToolAborted` where needed and sets
the one-shot `shell/resume_needed` signal for resumable approval shapes
(`thread_registry.rs:443-554`). The CLI worker consumes and clears that signal
before its prompt loop (`crates/ox-cli/src/agents.rs:590-635`).

`run_turn` now has a resume prologue. `inspect_log_for_resume`
(`crates/ox-kernel/src/run.rs:872-990`) recognizes an unresolved
`ApprovalRequested` or a `ToolAborted`, joins it to the full `ToolCall` input,
and requests explicit post-crash confirmation without another model round trip.
Otherwise it follows the normal new-turn path.

One known seam remains documented in current code: appending `ToolAborted` and
setting the in-memory `resume_needed` flag are not atomic
(`thread_registry.rs:534-547`). A crash in that window delays the reconfirm
surface until the next user prompt; the durable log still prevents data loss.

## 7. Current write-path diagram

```text
producer
  -> ThreadNamespace / LogStore
  -> SharedLog::append (holds ordering mutex)
  -> LedgerWriterHandle::commit
  -> writer thread: append hash-chain line + sync_data
  -> commit acknowledgement
  -> SharedLog publishes entry to readers
  -> HistoryView and UI project the durable entry

turn boundary
  -> save_config_snapshot
  -> context.json only

relaunch
  -> replay with durability disabled
  -> install LedgerWriter
  -> classify tail and expose/record recovery state
```

There is no normal in-memory-versus-ledger gap: when a durability sink is
installed, an entry cannot become observable before its ledger commit.

# Save and Restore

Thread persistence is deliberately split between a synchronous event ledger
and a turn-boundary configuration snapshot. There is no longer one
`save_thread_state` function with three responsibilities.

> See [`life-of-a-log-entry.md`](life-of-a-log-entry.md) for the event flow and
> [`data-model.md`](data-model.md) for the participating types.

Verified against the repository on 2026-08-31 with:

```sh
rg -n "save_thread_state|save_config_snapshot|write_default_view_if_missing" crates/
rg -n "LedgerWriter|with_durability|latest_save_result" crates/ox-inbox crates/ox-executor crates/ox-kernel
rg -n "pub fn restore|read_ledger_with_repair|resume_needed" crates/ox-inbox/src/snapshot.rs crates/ox-executor/src/thread_registry.rs crates/ox-executor/src/agents.rs
```

`rg 'save_thread_state' crates/` returns no matches.

## Persistence owners

### Event ledger: `LedgerWriter`

`SharedLog::append` commits every live `LogEntry` through a per-thread
`LedgerWriterHandle` before publishing it in memory
(`crates/ox-kernel/src/log.rs:266-283`). The dedicated writer thread owns
`ledger.jsonl`, serializes the hash chain, and acknowledges only after
`write_all` plus `sync_data` (`crates/ox-inbox/src/ledger_writer.rs:1-38`).

The writer also publishes cumulative `SaveResult` values. The per-thread
commit drain forwards advancing sequence/hash/message-count state to the inbox
index through `write_save_result_to_inbox`
(`crates/ox-executor/src/agents.rs:1315-1353`). This keeps listings fresh without
rescanning the ledger on every event.

### Config snapshot: `save_config_snapshot`

`ox_inbox::snapshot::save_config_snapshot`
(`crates/ox-inbox/src/snapshot.rs:43-94`) writes `context.json` from:

- `system/snapshot/state`;
- `gate/snapshot/state`;
- thread id, title, labels, and timestamps.

The executor invokes it after `module.run` and per-run bookkeeping
(`crates/ox-executor/src/agents.rs:1089-1259`). A failed config snapshot is recorded as
a warning and an error log entry, but it does not retroactively invalidate log
entries already committed by `LedgerWriter`.

### View bootstrap: mount-time only

`snapshot::write_default_view_if_missing`
(`crates/ox-inbox/src/snapshot.rs:96-104`) creates `view.json` once. It is called
from `ThreadNamespace::from_thread_dir` before restore
(`crates/ox-executor/src/thread_registry.rs:218-228`). It is not part of the
per-turn path.

## Restore lifecycle

`ThreadNamespace::from_thread_dir` owns the restore ordering
(`crates/ox-executor/src/thread_registry.rs:199-556`):

1. Build the namespace without a durability sink.
2. If `context.json` exists, `snapshot::restore` rehydrates participating
   stores and replays the repair-checked ledger through `log/append`
   (`crates/ox-inbox/src/snapshot.rs:147-220`).
3. If only `ledger.jsonl` exists, use the ledger-only repair/replay path. This
   covers a crash before the first config snapshot
   (`crates/ox-executor/src/thread_registry.rs:269-312`).
4. Reconstruct derived session usage and partial-stream projection
   (`crates/ox-executor/src/thread_registry.rs:366-379`).
5. Spawn `LedgerWriter`, seed it from the existing ledger head, and install its
   durability handle after replay (`crates/ox-executor/src/thread_registry.rs:392-428`).
6. Classify the tail and durably record or signal recovery behavior
   (`crates/ox-executor/src/thread_registry.rs:442-553`).

Replay must happen before installing durability. Reversing those steps would
double-write every restored event.

## Artifacts and formats

| Artifact | Owner | Update cadence | Purpose |
|---|---|---|---|
| `ledger.jsonl` | one `LedgerWriter` per thread | every log append | ordered, hash-chained conversation truth |
| `context.json` | `save_config_snapshot` | successful/failed run boundary | non-ledger store state and thread metadata |
| `view.json` | mount bootstrap | once if absent | UI metadata |
| `ox.db` thread rollup | commit drain and inbox store | latest committed sequence | listing, counts, search/index coordination |

The ledger envelope contains `seq`, `hash`, `parent`, and the serialized
`LogEntry` in `msg`. `context.json` does not duplicate the ledger.

## Failure decisions

- A ledger commit failure prevents the entry from becoming visible in
  `SharedLog`; the caller receives a `StoreError`.
- A torn final ledger line is repairable during `read_ledger_with_repair`.
- Missing, repair-failed, or writer-degraded ledgers mount without live
  durability and surface `shell/ledger_health`; they are not treated as
  healthy writable conversations.
- A config snapshot failure leaves the already-durable conversation ledger
  intact but may lose the most recent system/gate configuration changes.
- Writer owner drop sends an ordered shutdown and joins the OS thread, so
  already queued commits precede shutdown.

## Planning checklist

Any work that relocates thread execution or persistence must preserve these
answers:

1. Is there exactly one active writer for each `ledger.jsonl`?
2. Does commit-before-visibility still hold for every append path?
3. Is replay performed with durability disabled, then the writer installed?
4. Who writes `context.json`, and at what configuration boundary?
5. Who propagates `SaveResult` to the inbox/index projection?
6. How are missing, torn, and unrecoverable ledgers surfaced?
7. Does each conversation have its own ledger writer, workspace, approval
   state, and lifecycle lock so another conversation cannot block or corrupt it?

If any answer is missing, a shared-node remote-execution plan is not ready.

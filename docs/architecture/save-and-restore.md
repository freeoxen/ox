# Save and Restore

How thread state reaches disk today, and how it comes back on restart. The center of gravity is `save_thread_state` — **one function, three responsibilities.** Plans that propose restructuring durability must address all three.

> **Complementary to** [`life-of-a-log-entry.md`](life-of-a-log-entry.md) (the write path for individual entries) and [`data-model.md`](data-model.md) (the types).

---

## The function in question

`crates/ox-cli/src/agents.rs:563` — `save_thread_state(store, inbox_root, thread_id, title) -> Option<SaveResult>`.

Called twice per turn today:

- `agents.rs:409` — **pre-run** save (after the user's input is in memory, before `module.run`).
- `agents.rs:504` — **post-run** save (after the turn completes or errors).

Delegates to `ox_inbox::snapshot::save` at `crates/ox-inbox/src/snapshot.rs:34–123`.

---

## The three responsibilities

### (1) Config-snapshot persistence → `context.json`

Reads `{mount}/snapshot/state` for each mount in `PARTICIPATING_MOUNTS` (`snapshot.rs:15`):

```rust
pub const PARTICIPATING_MOUNTS: [&str; 2] = ["system", "gate"];
```

The `system` mount holds the system prompt; `gate` holds model-transport config. Each has its own `snapshot/state` read path that returns serializable state.

Packages them into `ContextFile.stores` and writes `context.json` atomically (via `std::fs::write`).

**Implication:** if the user changes their system prompt or switches models mid-session, those changes only reach disk on the next `save_thread_state` call. Remove the post-turn save, and a model change made during a turn is lost on crash.

### (2) `view.json` bootstrap

If `view.json` doesn't exist, `save_thread_state` writes a default. Idempotent after the first call. UI metadata that never changes at runtime.

### (3) Ledger append → `ledger.jsonl`

Reads `log/entries` (the full in-memory `SharedLog`), diffs against `count_messages_in_ledger(&ledger_path)`, and appends new entries via `ledger::append_entry` — which maintains the content-addressed hash chain (see `data-model.md` → `LedgerEntry`).

**This is the only code path that writes the ledger today.** `LogStore::write` does not touch disk.

---

## What restoration does

`ox-inbox/src/snapshot.rs:156–194` — `restore(namespace, thread_dir, mounts)`:

1. **Reads `context.json`.** For each `stores` entry, writes back to the matching mount's `snapshot/state` path. This re-hydrates the system prompt, gate config, etc.
2. **Reads `ledger.jsonl`** via `ledger::read_ledger`. For each `LedgerEntry`, writes its `msg` to `log/append` — reconstructing `SharedLog`.
3. **Reads existing `context.json` timestamps** to preserve `created_at`.

Note the asymmetry: **save writes two artifacts (context.json + ledger.jsonl); restore reads two artifacts.** Remove the save and you break restore.

---

## `SaveResult` — what the save reports back

```rust
pub struct SaveResult {
    pub last_seq: i64,
    pub last_hash: Option<String>,
    pub message_count: i64,  // user + assistant only
}
```

Consumed by `write_save_result_to_inbox` (`agents.rs:610`) which pushes it to the broker's inbox index so listings show live `message_count` and `last_seq` without re-scanning the ledger.

**Implication:** the inbox indexer depends on `save_thread_state`'s return value. A plan that removes the save must replace this integration — probably by counting appends directly at the durability seam.

---

## Failure paths

- **`std::fs::create_dir_all` fails** (permissions, disk full): `save` returns `Err`. `save_thread_state` (`agents.rs:585–604`) writes a `LogEntry::Error` to the log and returns `None`. The turn proceeds, but durability is broken until the problem is fixed.
- **`context.json` write fails**: same path as above.
- **`ledger::append_entry` fails mid-save**: partial save — context.json may have been written but ledger.jsonl appends did not complete. No explicit repair. Next save tries again with whatever the ledger's current `last_entry` is.

These are all observed via `tracing::error!` calls at the failure sites. No structured state is persisted saying "this thread is degraded"; the UI sees an `Error` log entry inline.

---

## If you're planning durability work

Before restructuring, answer these:

1. **Where does `context.json` get written in your new design?** If not on every turn, how do mount-state changes reach disk?
2. **Who computes `SaveResult` (`last_seq`, `last_hash`, `message_count`) in your new design?** The inbox indexer depends on it.
3. **How does the hash chain stay intact?** Per-append durability means the writer must read the current head (seq + hash) to compute the next entry's envelope.
4. **What happens to `view.json`?** Write-once is easy to preserve but shouldn't be silently dropped.
5. **What's the failure path when the new durability layer can't write?** The current design logs an error and continues; if your design refuses to continue, that's a user-visible UX change.

If a plan doesn't answer these, it isn't ready to land.

---

## Current call sites (verify before editing)

```
$ rg 'save_thread_state' crates/
crates/ox-cli/src/agents.rs:409   # pre-run
crates/ox-cli/src/agents.rs:504   # post-run
crates/ox-cli/src/agents.rs:563   # definition
```

```
$ rg 'snapshot::save' crates/
crates/ox-cli/src/agents.rs:575   # call from save_thread_state
(tests in snapshot.rs)
```

```
$ rg 'ledger::append_entry' crates/
crates/ox-inbox/src/snapshot.rs:112   # called per new message during save
crates/ox-inbox/src/ledger.rs:28      # definition
```

If these grep results differ materially when you run them, this doc is stale. Update it.

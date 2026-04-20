# Life of a Log Entry

Traces a single `LogEntry` from the moment it's born to the moment it's durable on disk — and, on relaunch, from disk back into memory. Written to make the write path un-guessable: every claim below is tied to `file:line`.

> **Complementary to** [`data-model.md`](data-model.md), which describes the types. This document describes the *flow*.

---

## 1. Birth: who creates a `LogEntry`?

Three producers, each writing through a different path in the namespace.

### (a) Kernel — via `emit` and `log/append`

`crates/ox-kernel/src/run.rs:597` — `run_turn(context, emit)` is synchronous. During a turn, it produces:

- `User` — from user input written to `history/append` before the turn runs (`agents.rs:386–390`).
- `Assistant` — the final assistant message after a streaming completion.
- `ToolCall` — each tool invocation.
- `ToolResult` — each tool's output.
- `TurnStart` / `TurnEnd` — boundaries.
- `CompletionEnd` — per-completion usage accounting.
- `Error` — runtime errors during the turn.

These are written through `log/append` on the thread's namespace, which routes to `LogStore::write` (`crates/ox-kernel/src/log.rs:322`).

### (b) Approval routing — via `ThreadNamespace`

`crates/ox-cli/src/thread_registry.rs:281–292` — when the kernel writes `approval/request`, `ThreadNamespace::write` intercepts, extracts `tool_name` and computes a display `input_preview`, then writes `LogEntry::ApprovalRequested` to `log/append`. Similarly for `approval/response` → `LogEntry::ApprovalResolved`.

**This is the only site that writes `ApprovalRequested` / `ApprovalResolved`.** The kernel does not write these variants; the namespace routing layer does.

### (c) Error fallback — via `save_thread_state`

`crates/ox-cli/src/agents.rs:598–601` — if `save_thread_state` fails, it writes a `LogEntry::Error` to the log before returning. This is a side-channel, rare path.

---

## 2. In memory (and, conditionally, to disk): `SharedLog`

`LogStore::write` deserializes the JSON to a `LogEntry` and forwards to `SharedLog::append`, propagating its `Result` via `?` (the method became fallible when the durability seam landed).

`SharedLog::append` (`crates/ox-kernel/src/log.rs:186`) takes the mutex. Two shapes depending on whether a `Durability` sink is installed:

- **No sink** (fresh `SharedLog`, or during replay): pushes the entry to the in-memory `Vec<LogEntry>` and returns. No disk I/O.
- **Sink installed** (via `SharedLog::with_durability(sink)`): calls `sink.commit(&entry)` inside the critical section. The entry is pushed to the Vec only after `commit` succeeds. On commit failure the entry is **not** pushed and the `StoreError` propagates to the caller.

The single-mutex design preserves **append-order equals observation-order**: readers of `entries()` block while a commit is in progress, but they never see an entry that hasn't been durably committed. This is the invariant Task 1a of the durable-conversation-state plan introduced.

In-workspace the concrete sink is `ox_inbox::ledger_writer::LedgerWriterHandle`, which hands off to a dedicated OS thread that drives `write_all` + `sync_data()` on `ledger.jsonl`. `ox-kernel` keeps only the `Durability` trait to avoid a reverse crate dependency.

Readers of `SharedLog`:

- `LogStore::read("entries")` — all entries as a JSON array.
- `LogStore::read("count")` — scalar count.
- `LogStore::read("last/N")` — last N entries.
- `HistoryView` (`crates/ox-history/src/lib.rs`) — projects entries into message-shaped views for the UI.

---

## 3. To disk: `save_thread_state`

`crates/ox-cli/src/agents.rs:409` (pre-run) and `:504` (post-run) — these are the only places the ledger is written today. Both call `save_thread_state(store, inbox_root, thread_id, title)` (line 563), which delegates to `ox_inbox::snapshot::save`.

`ox-inbox/src/snapshot.rs:34–123` — the `save` function does **three things**, not one:

1. **Reads `{system,gate}/snapshot/state`** from the participating mounts (`PARTICIPATING_MOUNTS = ["system", "gate"]`, line 15). Packages them into `ContextFile.stores`.
2. **Writes `context.json`** with the snapshot states plus thread metadata (title, labels, timestamps). Overwrites the file each call.
3. **Appends to `ledger.jsonl`**. Reads `log/entries` (in-memory), diffs against `count_messages_in_ledger`, appends new entries via `ledger::append_entry`.

**"Delete the `save_thread_state` call" means losing all three responsibilities.** Any plan removing these calls must provide separate paths for (1) and (2), not just (3).

### Ledger append details

`ox-inbox/src/ledger.rs:28` — `append_entry(path, msg, prev)`:

1. Compute `seq = prev.seq + 1` (or 0 for first).
2. Compute `hash = SHA-256(msg)[..8]` as 16 hex chars.
3. Compute `parent = prev.hash` (or `None`).
4. Wrap as `{"seq", "hash", "parent", "msg"}` and append one JSON line.

**The ledger is content-addressed with a parent-hash chain.** Each entry's integrity depends on the previous entry's hash. Per-append durability must maintain this chain — meaning the writer must know the current head before writing each entry.

### Ledger file format (example)

```jsonl
{"seq":0,"hash":"4a2b1c3d5e6f7890","parent":null,"msg":{"type":"user","content":"hi"}}
{"seq":1,"hash":"8b1c2d3e4f506172","parent":"4a2b1c3d5e6f7890","msg":{"type":"turn_start"}}
{"seq":2,"hash":"c3d4e5f607182930","parent":"8b1c2d3e4f506172","msg":{"type":"assistant","content":[...]}}
```

Note: the `msg` field holds the *serialized `LogEntry`*, not just the raw content. The `LedgerEntry` envelope wraps it with `seq`, `hash`, `parent`.

---

## 4. On relaunch: replay

`ox-inbox/src/snapshot.rs:156–194` — `restore(namespace, thread_dir, mounts)`:

1. Read `context.json`. Write each `stores` entry back to its mount's `snapshot/state` path.
2. Open `ledger.jsonl` via `ledger::read_ledger`. For each entry, write its `msg` to `log/append` — which routes through `LogStore::write` → `SharedLog::append`.

After restore, `SharedLog` holds the in-memory `Vec<LogEntry>` matching what was durable. The UI's `HistoryView` projects from it.

**Gap between save and memory**: any `LogEntry` appended to `SharedLog` after the last `save_thread_state` call is in memory but not on disk. A crash at this point loses those entries.

---

## 5. Approval flow (the most confusing path)

Worked example: user types something; kernel decides to call a tool; policy requires approval.

1. Kernel (during `run_turn`) reaches the tool-dispatch point. Calls policy. Policy says "needs approval."
2. Kernel writes `approval/request` with `{tool_name, tool_input}` to the thread namespace.
3. **`ThreadNamespace::write`** (`thread_registry.rs:265–294`) handles this path. It:
   - Computes `input_preview` by peeking at `tool_input.path` or `tool_input.command`.
   - Writes `LogEntry::ApprovalRequested { tool_name, input_preview }` to `log/append` (which goes to `LogStore::write` → `SharedLog::append`).
   - Forwards the write to `ApprovalStore::write("request", ...)`, which stores the runtime `ApprovalRequest` (with full `tool_input`) in `pending` and creates a `oneshot::Sender<Decision>` in `deferred_tx`.
4. The write returns a `BoxFuture<...>` that resolves when the user decides. The kernel's host-function bridge `block_on`s this future inside the Wasm module's thread.
5. **TUI reads `approval/pending`** via the broker. Gets back `ApprovalRequest { tool_name, tool_input }`. Renders the modal using the full `tool_input`.
6. User picks a `Decision`. TUI writes `approval/response` with it.
7. `ThreadNamespace::write` again: writes `LogEntry::ApprovalResolved { tool_name, decision }` to the log, then forwards to `ApprovalStore::write("response", ...)` which sends the `Decision` through the `oneshot`.
8. The kernel's future resolves. The Wasm thread unparks. Tool dispatches (or doesn't, on deny).

### Why this matters for recovery

**On crash between steps 3 and 7**, the durable state is:

- `LogEntry::ApprovalRequested { tool_name, input_preview }` — preview only. The full `tool_input` in the runtime `ApprovalRequest` is lost.
- The matching `ToolCall { id, name, input }` log entry, if any, **does** carry the full input (it's written earlier by the kernel).

So recovery can reconstruct the full input by looking up the matching `ToolCall` entry by `tool_use_id`. Plans that need the full input during recovery should **read the `ToolCall` entry**, not assume `ApprovalRequested` carries it.

---

## 6. What `run_turn` does on entry (current code)

This is the single most-load-bearing behavior for any resumption plan. It determines whether the kernel can "pick up where it left off" after a crash.

**Current (verified 2026-04-20).** `run_turn` at `crates/ox-kernel/src/run.rs:597` does **not** inspect the log tail. It unconditionally enters a loop, emits a fresh `TurnStart`, writes the corresponding log entry, and issues a completion. Calling `run_turn` again after a crash therefore starts a new turn on top of whatever dangling state the log contains — it does not resume mid-turn.

Adding a log-inspection prologue is Phase 3 (Task 3b) of the durable-conversation-state plan (`docs/superpowers/plans/2026-04-19-durable-conversation-state.md`). When that lands, this section and the plan's P7 both need updating.

**Re-verify before planning on top of it.** Behavior changes; the exact line numbers drift; this doc tells you *where* to look, and the plan tells you what the current answer is — but the code is the source of truth.

---

## 7. Summary write-path diagram

```
[user types input]
       ↓
agents.rs:386 — write "history/append"
       ↓ (routes through ThreadNamespace)
LogStore::write  →  SharedLog::append  (in memory only)
       ↓
agents.rs:409 — save_thread_state (pre-run)
       ↓
snapshot::save  →  context.json + ledger.jsonl
       ↓
agents.rs:437 — module.run(host_store)  →  run_turn()  (synchronous)
       ↓
kernel writes User/Assistant/ToolCall/ToolResult/TurnStart/TurnEnd/CompletionEnd/Error
       → LogStore::write → SharedLog::append  (in memory only)
       ↓
[if approval needed]
kernel writes approval/request
       ↓
ThreadNamespace::write (thread_registry.rs:281)
       ├→ writes LogEntry::ApprovalRequested to SharedLog
       └→ writes ApprovalRequest to ApprovalStore.pending + oneshot
       ↓
host-function bridge blocks via rt_handle.block_on(oneshot_receiver)
       ↓
[TUI renders modal, user decides]
       ↓
TUI writes approval/response → ThreadNamespace::write
       ├→ writes LogEntry::ApprovalResolved
       └→ ApprovalStore sends Decision through oneshot
       ↓
kernel unparks, continues
       ↓
agents.rs:504 — save_thread_state (post-run)
       ↓
snapshot::save  →  context.json + ledger.jsonl  (final durable state)
```

**The in-memory-vs-disk gap is between every `SharedLog::append` and the next `save_thread_state` call.** Every plan about durability should start here.

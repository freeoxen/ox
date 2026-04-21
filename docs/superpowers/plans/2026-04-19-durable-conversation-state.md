# Durable Conversation State — S-tier

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `ox-cli` conversation state lossless across exit and crash. Every event the user saw in the UI before the process ended is present and in the correct order after relaunch. A turn blocked on a permission approval resumes into bit-identical UI state with no extra LLM round-trip.

**Testing posture (scope note).** Every crash-correctness assertion in this plan is reached via **in-process soft crash only** — drop the `App`, remount from the same temp dir. Subprocess spawn and `SIGKILL` are out of scope for this plan's tests. Scenarios that would otherwise need an OS-level kill are expressed through deterministic `LedgerWriter` test hooks (`OX_TEST_FREEZE_AT`, failure injection) so the break happens inside the process. Real-syscall kill coverage is deferred to a later plan that first adds a headless mode to the `ox` binary.

**User-facing impact when this lands:**

- `Ctrl+C` or a crash mid-turn never "guts" a conversation. The transcript on relaunch shows exactly the events that had been rendered before the process died, terminated honestly with a `TurnAborted` marker when a turn couldn't complete.
- A conversation waiting on a permission prompt survives exit and relaunch. The modal reappears, the same approve/deny answer dispatches the tool, and no second LLM call is made.
- No new user-visible command is added. "Restart a conversation" is a guarantee the CLI makes, not an action the user takes — close the app, reopen, pick the thread, continue.

**Tech Stack:** Rust (edition 2024), tokio, ox-broker, ox-inbox, ox-kernel, ox-history, ox-cli.

**Scope:** Self-contained. One infrastructure task (crash harness) + four correctness phases + one integration task. Each is independently shippable and strictly improving correctness. Tasks land as separate commits for bisect friendliness.

---

## Design properties

Every task below advances one of these. If a task doesn't advance a property, it's not in the plan. If a property isn't advanced by any task, the plan is incomplete.

1. **Ledger-first visibility.** No `LogEntry` reaches `SharedLog` (and therefore no UI observer) before it is durable on disk.
2. **Atomic appends, torn-tail tolerant.** Each append is all-or-nothing. The last line may be partial after an abrupt interruption between `write_all` and `sync_data`; replay detects and truncates it without losing any prior entry. (Exercised in-process via the `LedgerWriter` freeze hook, not via an OS signal.)
3. **Order preserved.** Append order on disk equals observation order in the UI. Replay reproduces the same order.
4. **Replay is total.** Any ledger produced by a correctly-functioning ox-cli replays into exactly one position of a well-defined state machine. There is no "in-between" state the restart logic has to guess at.
5. **Permission-blocked ≠ interrupted.** A thread whose log tail is `ApprovalRequested` without `ApprovalResolved` restarts into bit-identical UI state with a live worker parked on the same approval. No new LLM call.
6. **Aborts are explicit.** A turn that cannot resume becomes a `TurnAborted` log entry on next mount, rendered in the transcript as an interrupted turn. No dangling `TurnStart`, no phantom assistant bubbles.
7. **Kernel control flow is a deterministic function of the log.** `run_turn()`'s *decisions* on entry — which branch of the kernel to take, which tool to dispatch, whether to park on approval, whether to call the model — depend only on log contents, not on in-memory Rust futures carried across the crash. Tool *outcomes* are not pure (they perform I/O), but tool *dispatch decisions* are. No coroutine is "preserved"; the same log tail always produces the same next decision when `run_turn` is called fresh.
8. **Every ledger write emits `tracing`.** Commit latency, group-commit batch size, torn-tail repairs, and state classification results are all traced for operator diagnosis.
9. **Correctness is proven at two scales.** Classification is property-tested on randomly-truncated ledgers (10k cases via `proptest` in PR CI). A nightly large-sample soak (`proptest`-driven, 30-minute budget) proves no event is lost across a wide space of turn shapes and in-process drop points (soft-crash + `LedgerWriter` freeze-hook combinations). Failed iterations auto-shrink to minimal reproductions. Real-signal (`SIGKILL`) coverage is deferred.
10. **No silent deferrals.** Tool re-dispatch safety and streaming durability are answered in-plan. Ledger format versioning is explicitly deferred with rationale (see Non-goals). Anything not answered in-plan is named in Non-goals with a reason.
11. **Every error path has a named UX decision.** Torn-tail repair failures, approval-payload incompleteness, and Wasm-bridge blocking semantics each resolve to a specific state and a specific user-visible surface. No silent degradation.

---

## Prerequisites (verify before implementation)

Each prerequisite has a **verification recipe** — the exact command or grep that proves it. If a prerequisite fails, the plan is blocked and must be revised before any task runs. Failures that require code changes before the plan can proceed are called out as **blockers with scope estimates**.

Each prerequisite is **verified against current code**. Results recorded below.

- [x] **P1. Ledger writes funnel through one entry point.** Verified at `ox-inbox/src/ledger.rs:28` (`pub fn append_entry`). Single point of entry.

- [x] **P2. `SharedLog::append` is the single in-memory append method.** Verified at `ox-kernel/src/log.rs:128` — `pub fn append(&self, entry: LogEntry)` on `SharedLog(Arc<Mutex<Vec<LogEntry>>>)`. `LogStore::write` (the `structfs_core_store::Writer` impl) dispatches to `SharedLog::append`. No bypass paths found.

- [x] **P3. `ApprovalStore` parks on a `oneshot::Sender<Decision>`.** Verified at `ox-ui/src/approval_store.rs:13`: `deferred_tx: Option<tokio::sync::oneshot::Sender<ox_types::Decision>>`. `pending: Option<ApprovalRequest>` at line 12.

- [x] **P4. `LogEntry` is a closed enum, with caveats.** Verified at `ox-kernel/src/log.rs:23`. No `#[non_exhaustive]`. However, there **is** a `Meta { data: serde_json::Value }` variant (line 62) which is an open-payload escape hatch. Full variant list at time of writing: `User`, `Assistant`, `ToolCall`, `ToolResult`, `Meta`, `TurnStart`, `TurnEnd`, `CompletionEnd`, `ApprovalRequested`, `ApprovalResolved`, `Error`. The classifier (Task 2) must exhaust all 11 variants explicitly, including the ones this plan adds (`TurnAborted`, `AssistantProgress`, `ToolAborted`).

- [x] **P5. All `LogEntry` variants round-trip through JSONL.** Per tests in `2026-04-13-audit-log.md`.

- [x] **P6. Full tool input is recoverable from the log even though `ApprovalRequested` only carries a preview.** Verified at `ox-kernel/src/log.rs:43–50`: `LogEntry::ToolCall { id, name, input: serde_json::Value, ... }`. The `ToolCall` entry is written by the kernel *before* the approval flow begins and carries the full input. For recovery, the kernel prologue joins `ApprovalRequested` with the matching `ToolCall` entry by `tool_use_id`. **No schema change to `ApprovalRequested` is needed.** Task 3 Step 1 adds only a `post_crash_reconfirm: bool` flag for the distinct concern of signaling re-confirmation.

- [ ] **P7. `run_turn` inspects the log tail on entry.** **VERIFIED FALSE.** At `ox-kernel/src/run.rs:597–672`, `run_turn` unconditionally enters a loop, emits `TurnStart` (line 634), writes a `turn_start` log entry (lines 637–647), and issues a fresh completion (line 671). It does not look at the log tail. Consequence: Phase 3 **requires a kernel change** to teach `run_turn` to inspect the log before emitting `TurnStart`. Scope: ~2–4 days of kernel work + substantial test churn. This is not a verification step — it is an implementation task folded into Task 3 Step 2.

- [x] **P8. The broker's async write path bridges to sync stores via `SyncClientAdapter`.** Verified: `ox-broker/src/sync_adapter.rs:56–68`. Writes use `tokio::task::block_in_place(|| handle.block_on(self.client.write(...)))`. Sync writes through `LogStore::write` are reachable from any context — kernel Wasm host, broker async dispatch, direct sync callers. The durability seam at `LogStore::write` catches all of them.

- [x] **P9. Mount is lazy per-thread.** Verified: `ox-cli/src/thread_registry.rs:3–4` doc comment ("lazy-mounts thread stores from disk on first access") and `from_thread_dir` at line 48 which constructs per-thread state on demand. Startup cost is O(1); per-thread replay runs on first access.

- [x] **P10. `LogEntry::ApprovalRequested` is written by `ThreadNamespace::write`, not by the kernel.** Verified: `ox-cli/src/thread_registry.rs:281–292`. When `approval/request` is written, `ThreadNamespace` extracts `tool_name`, computes a display `input_preview` from `tool_input.path`/`tool_input.command`, and writes `LogEntry::ApprovalRequested` to `log/append`. **Consequence:** the write site for the log variant is in `ox-cli`, not `ox-kernel`. P6's schema change touches this file, not `run.rs`.

- [x] **P11. Replay routes through `LogStore::write`.** Verified: `ox-inbox/src/snapshot.rs:156–194` restore path writes each replayed entry via `log/append`. If the durability seam lives at `LogStore::write`, replay will double-write the ledger. **Durability must be disabled during replay.**

**Kernel mental model correction.** There is no "agent worker" or "parked coroutine." `run_turn()` is a synchronous function called once per turn from `agents.rs:437` via `module.run(host_store)`. When approval is needed, the Wasm-module thread blocks inside a host import function via `rt_handle.block_on`. On crash, that thread dies.

**However**: `run_turn` does NOT inspect the log tail on entry — it starts a new turn from scratch with a fresh `TurnStart`. So simply calling `run_turn` again after a crash does **not** resume mid-turn — it starts a new turn, stacking a second `TurnStart` on top of the prior unfinished one. Phase 3 therefore requires either (a) modifying `run_turn` to peek at the log tail and branch, or (b) not calling `run_turn` at all on `AwaitingApproval` and instead using a separate recovery mechanism. See Task 3 for the chosen path.

---

## File map

| File | Changes |
|------|---------|
| `crates/ox-cli/tests/crash_harness/` | **New.** Headless crash-test infrastructure (Task 0). In-process soft-crash only. No TUI capture, no subprocess spawn. |
| `crates/ox-cli/tests/crash_harness/README.md` | **New.** How to author a new crash scenario in-process; when to reach for `LedgerWriter` freeze hooks. |
| `crates/ox-cli/tests/fixtures/canonical_turn_messages.json` | **New.** Semantic-golden baseline — message-shaped ledger entries only. |
| `crates/ox-cli/tests/fixtures/REVIEWERS.md` | **New.** Golden-file governance. |
| `crates/ox-cli/tests/fixtures/crash_repros/` | **New.** Auto-populated by Task 5 S7 on failure. |
| `crates/ox-inbox/src/ledger.rs` | `LedgerWriter` dedicated OS thread per thread-dir; head-state + `message_count` tracking; group-commit `File::sync_data()`; torn-tail repair; `LedgerDegraded` / `LedgerRepairFailed` / `LedgerMissing`. |
| `crates/ox-inbox/benches/ledger_commit.rs` | **New.** `File::sync_data()` benchmark to fix the coalesce window. |
| `crates/ox-inbox/src/snapshot.rs` | Rename `save` → `save_config_snapshot`, strip ledger-append block (now `snapshot.rs:82–116`), strip `SaveResult` return; `restore` gains torn-tail repair. |
| `crates/ox-inbox/src/resume.rs` | **New.** `classify(ledger_tail) → ThreadResumeState`. Pure. |
| `crates/ox-kernel/src/log.rs` | Add `TurnAborted`, `AssistantProgress`, `ToolAborted` variants. `SharedLog::append` becomes fallible and gains `with_durability(handle)` callback. `ApprovalRequested` gains `post_crash_reconfirm: bool`. |
| `crates/ox-kernel/src/run.rs` | **Largest kernel change**: log-inspection prologue detecting resume shapes and branching. Kernel rules for Skip (synthetic `ToolResult`) and Cancel (`TurnAborted(UserCanceledAfterCrash)`). `AssistantProgress` emission (Task 4). |
| `crates/ox-types/src/approval.rs` | Extend `Decision` with `CancelTurn`; update `is_allow` / `is_deny` / `as_str` / `Display`. Audit `match Decision` sites for exhaustiveness. |
| `crates/ox-history/src/lib.rs` | Project `AssistantProgress` into `turn/streaming` on replay. |
| `crates/ox-ui/src/approval_store.rs` | Handle `Decision::CancelTurn` arrival on the oneshot. Thread `post_crash_reconfirm` through the runtime `ApprovalRequest` so the UI modal can distinguish. |
| `crates/ox-cli/src/thread_registry.rs` | `ThreadNamespace::write` (`approval/request` handler at `:281–292`): pass `post_crash_reconfirm` into `LogEntry::ApprovalRequested`. Mount lifecycle per Task 2. Install `LedgerWriterHandle` via `SharedLog::with_durability` *after* replay. |
| `crates/ox-cli/src/agents.rs` | Remove pre-run save at `:409`; switch post-run save at `:504` to `save_config_snapshot`. |
| `crates/ox-cli/src/history_view.rs` | Render `TurnAborted` / `ToolAborted` as muted markers; ledger-error banners. |
| `crates/ox-cli/src/theme.rs` | Styles + constant strings for new log variants, post-crash-reconfirm modal copy, synthetic-ToolResult content, ledger-error banners. |
| `crates/ox-cli/src/<new>` | `CommitDrain` task per mounted thread: receives `CommitResult::Ok { last_seq, last_hash, message_count }` from the `LedgerWriter` and calls `write_save_result_to_inbox` debounced at ~100ms. No config-snapshot observer — `save_config_snapshot` runs at turn boundary. |

---

## Task 0 — Crash harness (infrastructure)

**Advances: Property 9.**

This task is infrastructure, not a correctness phase. Every later task's "write a failing test first" step presupposes this harness exists.

### Testing layer: headless, not terminal-level

**We do not capture TUI output.** The plan's invariants are about ledger contents and the `SharedLog` projection derived from them. The UI is a deterministic render of `SharedLog`; if `SharedLog` round-trips across a crash, the UI does too by construction. Testing the render path would just be testing `ratatui` — a separate concern.

Every assertion in this plan operates at one of two layers:

- **Ledger bytes** — read the JSONL file directly and parse.
- **`SharedLog` snapshot** — reconstruct via `ThreadRegistry::from_thread_dir`, capture an ordered `Vec<LogEntry>`, compare.

No `vt100`, no pty, no screen-cell diffing.

### One crash mode: in-process soft crash

Every scenario in this plan is expressed as drop `App` → remount from the
same thread dir. The ledger file and `SharedLog` are the entire surface under
test; drop + remount catches the bugs this plan targets. Subprocess spawn and
`SIGKILL` are out of scope.

| Mode | When to use | Mechanism |
|------|-------------|-----------|
| **In-process "soft crash"** | All scenarios in Tasks 1–5. | Drop `App`; await writer-thread shutdown; reconstruct via `ThreadRegistry::from_thread_dir`. |
| **`LedgerWriter` freeze hook** | Torn-tail, write/sync failure, crash-during-repair, any scenario that would otherwise want an OS-level signal between `write_all` and `sync_data`. | The writer parks at `OX_TEST_FREEZE_AT=<point>` inside the test process; the test advances or aborts it deterministically. |

### The change

Build the harness in `crates/ox-cli/tests/crash_harness/`:

- **`HarnessBuilder`**: constructs a temp `$HOME/.ox`, wires a fake transport, builds an `App` in-process. No subprocess plumbing.
- **Scripted fake transport** (`ox-gate` mock or in-process `Transport` trait impl): emits a scripted sequence of SSE events. Supports `pause_before(event_index)`, `emit(event)`, `fail_if_called_more_than(n)`. Records call count.
- **`SharedLog` snapshotter**: a handle into the running CLI that captures the current log entries on demand via the `App`'s registry.
- **Crash injection (in-process only)**:
  - Advance the fake transport to the scripted pause, drop the `App`, wait for writer-thread shutdown.
  - For failure modes that would otherwise need an OS signal between `write_all` and `sync_data`, the `LedgerWriter` honors `OX_TEST_FREEZE_AT=<point>` and parks on a channel at the named point; the test advances or aborts the commit deterministically from inside the process.
- **Headless assertion helpers**:
  - `assert_ledger_entries_eq(expected: &[LogEntry])` — parse ledger file, compare ordered.
  - `assert_shared_log_matches_pre_kill(snapshot: &[LogEntry])` — remount, compare to captured snapshot.
  - `assert_no_dangling_turn_start(ledger: &[LogEntry])` — structural check.
  - `assert_transport_called_exactly(n: usize)`.
  - `assert_tool_counter_eq(n: usize)` — for Task 3 side-effect tests.
- **Relaunch helper**: `remount(temp_dir)` constructs a fresh `App` against the same thread dir and returns it.

### Tasks

- [x] **Step 1:** Scaffolding: `tests/crash_harness/mod.rs` with `HarnessBuilder`, temp-dir setup, fake-transport trait impl. Prove a scripted turn runs end-to-end in in-process mode.
- [x] **Step 2:** `SharedLog` snapshot mechanism for in-process mode. Unit test: snapshot before/after an append, verify ordering.
- [x] **Step 2.5:** **Verify `App::drop` releases all OS resources.** `App` has no explicit `Drop` impl today (verified: grep `impl Drop for App` returned no matches). Cleanup happens via field-by-field drop. Audit: grep the `App` struct's field types for anything that could leak (spawned tasks without cancellation, open file descriptors, tempfiles, lock files).

**Decision rule, applied in order:**
  - *Any detached `tokio::spawn` handle or background OS thread without a cancellation hook* → **build an explicit `Drop for App`** that joins/aborts them. Non-negotiable for deterministic tests.
  - *Any open file handle that outlives the drop* → **rely on Rust's `File` drop for flush**, but verify via test that the file's content is observable to a fresh reader post-drop.
  - *Any tempfile or lockfile not cleaned up* → **add cleanup to the explicit `Drop`**. A stale lockfile would block the remount.
  - *Anything else deemed safe-to-leak* (e.g. `tracing` subscribers, `Arc` cycles that will drop when the runtime shuts down) → document in `tests/crash_harness/README.md` with rationale; allow in tests.

Success: after `App::drop` returns and any explicit `Drop` work completes, the temp `$HOME/.ox` contains only the expected on-disk state. No stray lockfiles, no background threads, no open FDs to the ledger file. Soft-crash tests must be deterministic — same drop sequence → same disk state, assertable via filesystem snapshot.
- [x] **Step 3:** In-process soft-crash: `App::drop` + writer-thread join + remount. Test: cleanly-run turn, snapshot `SharedLog`, drop, remount, assert snapshot matches. Regression-watch Step 2.5's audit.
- [ ] **Step 5:** `LedgerWriter` freeze-point hook (gated by `OX_TEST_FREEZE_AT`). Test: freeze between `write_all` and `sync_data`, abort the commit in-process, remount, verify torn-tail behavior is reachable. In-process only — no subprocess.
- [x] **Step 6:** Assertion helpers listed above. Each with a dedicated unit test.
- [x] **Step 7:** Document the harness in `crates/ox-cli/tests/crash_harness/README.md` — how to write an in-process scenario; when to use a `LedgerWriter` freeze hook.

> **Step 4 (subprocess spawn + `SIGKILL` + exit code) — removed.** Subprocess-based tests are out of scope for this plan. When a scenario needs a realistic fsync-boundary or mid-syscall break, it goes through the `LedgerWriter` freeze hook from Step 5 instead.

### Success criteria

- In-process mode can deterministically simulate crash + remount for every Task 1–5 scenario without touching a process signal.
- The `LedgerWriter` freeze hook reproduces: drop mid-stream, drop while approval pending, drop between `write_all` and `sync_data` — all in-process.
- Harness tests run under 2 seconds per scenario.
- Zero terminal-emulation dependencies. Zero subprocess spawn.

### Commit

`test(cli): crash harness infrastructure`

---

## Task 1 — Phase 1: Durable append

**Advances: Properties 1, 2, 3, 8.**

### The change

Replace "save at turn boundaries" with "every log entry is synchronously durable before it becomes visible to readers." This requires three coordinated changes, not one.

**Change 1 — `SharedLog` gains a durability callback.** Currently `SharedLog::append` (`ox-kernel/src/log.rs:128`) pushes to an in-memory Vec and returns. We extend it with an optional callback installed via `SharedLog::with_durability(writer: LedgerWriterHandle)`. When installed, `append` submits to the writer, blocks on commit, and only then pushes to the Vec.

```rust
// ox-kernel/src/log.rs (sketch)
pub struct SharedLog {
    // Single mutex covers both the durability handle and the Vec —
    // held across commit + push to preserve Property 3 (append-order
    // equals observation-order) under concurrent callers.
    inner: Arc<Mutex<SharedLogInner>>,
}
struct SharedLogInner {
    entries: Vec<LogEntry>,
    durability: Option<LedgerWriterHandle>,
}
pub fn append(&self, entry: LogEntry) -> Result<(), StoreError> {
    let mut inner = self.inner.lock().unwrap();
    if let Some(writer) = inner.durability.as_ref() {
        writer.commit_blocking(&entry)?;  // sync channel to LedgerWriter thread
    }
    inner.entries.push(entry);
    Ok(())
}
```

**Concurrency semantics (Property 3).** The single-mutex design serializes all appends. That means holding the lock across the commit wait (~1–5ms per append during streaming) — readers of `entries()` block for that window. This is acceptable because:

- Appends are already the critical path for durability; ordering *is* the point.
- Read paths (`entries()`, `len()`, `last_n()`) are fast and tolerate brief blocking.
- Concurrent writers are rare in this codebase — the kernel Wasm thread and the broker async-dispatch thread may occasionally contend, but most writes are single-producer per turn.

**Alternative considered and rejected:** two-lock design with separate durability and entries locks. Violates Property 3 under concurrent writers because ack arrival order is not push order. Not worth the micro-perf gain on read paths.

`append` becomes fallible — the error path bubbles up through `LogStore::write` (which already returns `Result`). Callers that used to ignore the return value get unwrapped in review.

**Why at `SharedLog::append` not `LogStore::write`:** `LogStore::write` is one of several paths into `SharedLog::append`. Kernel code calls `log_entry(context, ...)` which writes via `context.write("log/append", ...)` which routes via `ThreadNamespace::write` (`thread_registry.rs`) to `LogStore::write` to `SharedLog::append`. But `HistoryView` constructors may also touch `SharedLog` directly. Placing durability at the `SharedLog::append` seam catches the single chokepoint; `LogStore::write` is upstream and routable-around.

**Change 2 — replay disables the callback.** `ox-inbox/src/snapshot.rs:156–194` replays `ledger.jsonl` through `log/append`. If the durability callback were live during replay, each replayed entry would be re-written to disk. The `ThreadNamespace::from_thread_dir` path constructs `SharedLog` *without* a callback, runs `snapshot::restore` (replay finishes, fills the Vec), then installs the `LedgerWriterHandle`. Subsequent writes are durable; replay writes are not re-persisted.

**Change 3 — `save_thread_state` narrows, doesn't vanish.** Per [`docs/architecture/save-and-restore.md`](../../architecture/save-and-restore.md), `save_thread_state` has three responsibilities. Only (3) moves to the new seam; (1) and (2) stay in place:

- **(1) `context.json` — config-snapshot persistence.** Narrow `save_thread_state` into `save_config_snapshot` — only writes `context.json`, does not touch the ledger, returns `Result<(), String>` (no `SaveResult`). Call site stays post-turn at `agents.rs:504`. The pre-turn call at `:409` is removed because no config state changes between user input and `module.run`.
- **(2) `view.json`** — already write-once-if-missing. Move the bootstrap into `ThreadNamespace::from_thread_dir` on first construction so it's not coupled to per-turn saves.
- **(3) `ledger.jsonl` — per-append durability.** This is the new seam via `LedgerWriter`. Deletes the incremental append loop in `snapshot::save` (currently `ox-inbox/src/snapshot.rs:82–116`).

**Rationale for keeping a post-turn config-snapshot call (rather than inventing an observer):** the broker has no write-hook mechanism (verified: no `observer` / `subscribe` / `on_write` in `ox-broker/src/`). Building one is ~2–3 days of scope that doesn't belong in this plan. A post-turn call is simple, cheap (one `fs::write` on a small JSON file), and honors all existing config-persistence assumptions. Config state can still change mid-turn (model switch via a tool), but the next turn boundary captures it — acceptable granularity for a local CLI.

**`SaveResult` propagation.** `save_thread_state` returns `SaveResult { last_seq, last_hash, message_count }` consumed by `write_save_result_to_inbox` to update the broker's inbox index (`agents.rs:610`). Under per-append durability:

- `last_seq` and `last_hash` come from `LedgerWriter`'s head state after each commit — trivial.
- `message_count` is maintained as a running counter in `LedgerWriter`, incremented per commit when the entry's `msg` satisfies `ox_inbox::snapshot::is_message_entry` (the existing helper at `snapshot.rs:135`). Seeded on writer construction by calling `count_messages_in_ledger` once at startup. Avoids per-commit file scans.

After each commit, `LedgerWriter` sends a `SaveResult` across a channel. An `ox-cli` drain task (debounced ~100ms) calls `write_save_result_to_inbox` with the latest value. Replaces the direct return-value propagation from `save_thread_state` without losing the inbox-index-freshness guarantee.

**`LedgerWriter` — a dedicated OS thread per thread-dir**, not a Tokio task. A plain `std::thread::spawn` with three channels wired around it:

- **Input channel** (`std::sync::mpsc::Sender<CommitRequest>`) — callers submit entries here. A `CommitRequest` bundles the `LogEntry` and a per-request `sync_channel(0)::Sender<CommitResult>` for the ack.
- **Per-request ack channel** (`sync_channel(0)`, created per call) — the writer resolves it after `sync_data()` completes. This is what `SharedLog::append` blocks on. Scope is one commit; drops when the caller returns.
- **Drain channel** (`std::sync::mpsc::Sender<SaveResult>`, created once at writer construction) — the writer broadcasts a `SaveResult { last_seq, last_hash, message_count }` after each commit to a separate receiver owned by the `CommitDrain` task (see Step 10). This channel has no back-pressure — the drain task debounces on receive; if it falls behind, the writer overwrites via a single-slot queue pattern (latest-wins).

Two channel families, one commit. The per-request ack is for correctness (caller needs to know when their entry is durable). The drain channel is for inbox-index freshness (no caller waits on it; it fires async). Keeping them separate avoids conflating "my write is durable" with "the inbox index is updated" — the former must block, the latter must not.

**Commit protocol** on the `LedgerWriter` thread:

1. `recv()` from the input channel (blocks).
2. Compute next envelope: `seq = head.seq + 1`, `parent = Some(head.hash)`, `hash = entry_hash(msg)` — reusing `ox_inbox::ledger::entry_hash` (`ox-inbox/src/ledger.rs:18`). Head state and a `message_count` running counter are maintained in the writer's local state. Seeded on construction by reading `ledger::read_last_entry` (for head) and `count_messages_in_ledger` (for the counter).
3. If `ox_inbox::snapshot::is_message_entry(&msg)` is true (`snapshot.rs:135`), increment the counter.
4. Serialize envelope to `{"seq","hash","parent","msg"}` JSON line ending in `\n`.
5. `write_all` to the file.
6. Coalesce additional pending entries for up to the Step 1-benchmarked window; each advances head state and the counter.
7. **Durability sync** — `File::sync_data()`. See platform note below.
8. Resolve all coalesced acks with `CommitResult::Ok { last_seq, last_hash, message_count }` (or `Err(LedgerIoError)` on failure — see failure-mode spec below).

**Platform note on durability sync.** `File::sync_data()` maps to `fdatasync(2)` on Linux and `fsync(2)` on macOS. On macOS, `fsync` flushes to the disk *controller* but not necessarily to physical media — `fcntl(F_FULLFSYNC)` is required for that guarantee. For a local developer CLI, `File::sync_data()` is the right default: it defeats OS page-cache loss on crash (the main failure mode), costs ~1–5ms, and matches the durability guarantees that other local-interactive tools (git, sqlite with default settings) provide. If a future requirement demands cross-power-loss durability, swap in `F_FULLFSYNC` at cost of ~10–50ms per commit; gate behind `OX_STRICT_DURABILITY=1`.

Why a real OS thread instead of a tokio task: `SharedLog::append` is called from contexts with and without a tokio runtime (Wasm host imports, broker async dispatch via `SyncClientAdapter::block_in_place`, direct sync tests). A dedicated OS thread is contextless — the caller blocks on a sync channel without care for runtime awareness. `sync_data` is a blocking syscall; a dedicated thread is the conventional fit.

**Replay repair.** On mount, validate each ledger line: if the last line fails to parse or doesn't end in `\n`, truncate the file to the last good byte offset and emit `LedgerTailRepaired { thread_id, bytes_dropped }`.

**Delete** `agents.rs:409` (pre-run save) outright — no `context.json` changes happen between user input and `module.run`. Keep `agents.rs:504` (post-run save) **only** for the config-snapshot responsibility, reduced to writing `context.json` when `system`/`gate` state changed during the turn. The ledger responsibility is gone from both.

### Failure-mode spec (Property 11)

Three failure paths, each with an explicit UX decision:

| Failure | Detection | User-visible outcome |
|---------|-----------|----------------------|
| `write_all` or `File::sync_data()` returns an error during a commit | `LedgerWriter` task | All pending oneshots resolve `Err(LedgerIoError)`. `LogStore::append` propagates the error. The thread enters `LedgerDegraded` state: UI banner "this thread's log cannot be written — conversation frozen." No further appends attempted. Thread mountable read-only on next launch. |
| Torn-tail repair cannot truncate the file (read-only disk, permissions) | `snapshot::restore` truncation attempt | Thread mounts in `LedgerRepairFailed` state. UI banner "this thread's log is damaged and cannot be repaired — mounted read-only." No worker spawned. |
| Ledger file missing or unreadable on mount | `snapshot::restore` open | Thread mounts in `LedgerMissing` state. UI banner "this thread's log is missing." Inbox entry shows error badge. User can delete the thread via normal UI path. |

All three states are durable (recorded in `context.json` on mount) so that later Task 2 classification sees them first and short-circuits.

### Tasks

- [ ] **Step 1:** Benchmark `File::sync_data()` on representative local SSD (dev machine macOS + Linux CI). Choose the coalesce window. Record the value in a constant with a comment citing the benchmark and the platform note about `fsync` vs `F_FULLFSYNC` on macOS. Commit benchmark harness under `crates/ox-inbox/benches/`.
- [ ] **Step 2:** Record a **semantic golden** under the existing (pre-change) code. Run one canonical turn through the Task 0 harness, capture the ledger as `Vec<LedgerEntry>`, extract just the message-shaped entries (user, assistant, tool_result — the subset today's `save_thread_state` writes), commit as `tests/fixtures/canonical_turn_messages.json`. **Not byte-equality** — the new code's ledger will have TurnStart/CompletionEnd/ApprovalRequested entries the old save never wrote. The golden is the *subset* that must match across architectures.
- [ ] **Step 3:** Write failing crash-harness tests (require Task 0):
  1. Soft-crash mid-turn (in-process `App` drop) → assert every `LogEntry` in `SharedLog` pre-drop is in the ledger post-remount, none that weren't.
  2. Freeze-hook crash between `write_all` and `sync_data` (in-process via `OX_TEST_FREEZE_AT`) → drop `App`, remount, assert no torn entry is visible and the prior entries are intact.
  3. Inject `write_all` error via `LedgerWriter` test hook → assert `LedgerDegraded` state reached and persisted.
  4. Torn-tail: hand-written truncated line, simulate mount, assert repair + recovery.
  5. Torn-tail with truncation failure (test-hook simulating permissions error) → assert `LedgerRepairFailed` state.
  6. **Replay-amplification regression:** remount a thread with a populated ledger and assert the ledger file's size does not grow during replay.
- [ ] **Step 4:** Introduce `LedgerWriter` as a dedicated OS thread per thread-dir. Seed head state by calling `ox_inbox::ledger::read_last_entry` at construction. Each commit maintains the hash chain (`seq`, `parent`, `hash` computed per envelope). Unit-test: batch coalescing, `CommitResult::Err` propagation, head-state continuity across appends.
- [ ] **Step 5:** Extend `SharedLog` with the optional durability callback (`with_durability(handle)`). `SharedLog::append` becomes fallible. Update all callers of `append` to handle the `Result` — grep `SharedLog` for direct call sites.
- [ ] **Step 6:** Wire the `LedgerWriter` handle into `ThreadNamespace::from_thread_dir` **after** `snapshot::restore` completes. Replay must happen with durability OFF; durability is installed only after the existing ledger is fully loaded. Unit test: replay a ledger, assert no new writes hit the file.
- [ ] **Step 7:** Implement `LedgerDegraded`, `LedgerRepairFailed`, `LedgerMissing` states. Wire UI banners in `ox-cli` (`history_view.rs` / `theme.rs`). Per Property 11: explicit user-visible surfaces.
- [ ] **Step 8:** Add torn-tail repair in `snapshot::restore`. On truncation failure, escalate to `LedgerRepairFailed`.
- [x] **Step 9:** Narrow `save_thread_state` (per [`docs/architecture/save-and-restore.md`](../../architecture/save-and-restore.md)):
   - Rename `ox_inbox::snapshot::save` → `ox_inbox::snapshot::save_config_snapshot` and reduce its body: writes `context.json` from `PARTICIPATING_MOUNTS` state. Does **not** touch the ledger. No `SaveResult` (return `Result<(), String>`).
   - Rename `ox_cli::agents::save_thread_state` → `save_config_snapshot` at the CLI layer too; it becomes a thin wrapper over the inbox function.
   - Extract `write_default_view_if_missing(thread_dir)` — called from `ThreadNamespace::from_thread_dir` on construction (one-shot, not per-turn).
   - Delete the ledger-append block from `snapshot::save` (today `snapshot.rs:82–116`); that responsibility is now `LedgerWriter`.
   - **Rename blast radius** — update all call sites: `agents.rs:575` (inside old `save_thread_state`), and the test suite in `snapshot.rs` (look for `snapshot::save(` in `#[cfg(test)]` blocks — expect ~4–6 test sites). Also update the audit-log plan's tests if they call it directly.
   - The `SaveResult` struct itself goes away from this function; tests that asserted on its fields now assert on the `CommitResult::Ok { last_seq, last_hash, message_count }` from `LedgerWriter` instead.
- [x] **Step 10:** Wire the drain channel. `LedgerWriter` publishes a `SaveResult { last_seq, last_hash, message_count }` to a latest-wins slot after each commit (separate from the per-request ack). A `CommitDrain` task in `ox-cli` (one `tokio::spawn` per mounted thread) polls the slot at ~100ms cadence and calls `write_save_result_to_inbox` when it changes. Replaces the direct return-value propagation from `save_thread_state`. Unit test: run N commits, assert inbox `message_count` updates within 200ms of the last commit. Assert that a burst of 1000 commits does not queue 1000 drain writes — latest-wins holds.
- [x] **Step 11:** Remove pre-turn save at `agents.rs:409` entirely. Keep post-turn call at `:504` but switch to `save_config_snapshot`. The ledger part is gone; only config-state persistence remains here.
- [ ] **Step 13:** Audit existing tests. Grep for `save_thread_state`, `snapshot::save`, `count_messages_in_ledger`, `context.json` in test files. Update assertions to match the new model: some tests that counted ledger entries after a save now need to count after the expected append-path commits.
- [ ] **Step 14:** Happy-path regression: run canonical turn through new code, filter ledger entries through `ox_inbox::snapshot::is_message_entry` (the existing helper at `snapshot.rs:135`), assert semantic equality against the Step 2 golden. **Governance**: `tests/fixtures/REVIEWERS.md` gates changes.
- [ ] **Step 15:** Crash-harness tests from Step 3 pass.
- [ ] **Step 16:** Tracing: emit `LedgerCommit { entries, bytes, sync_data_us, message_count }` per commit, `LedgerTailRepaired` / `LedgerDegraded` / `LedgerRepairFailed` / `LedgerMissing` / `ConfigSnapshotWritten` as appropriate.

### Success criteria

- In-process crash at any instant during a turn — either `App` drop or a `LedgerWriter` freeze hook held between `write_all` and `sync_data`. On remount, every `LogEntry` that had been appended to `SharedLog` pre-crash is present in the ledger, and no entry that hadn't been appended is present.
- Replay on mount does not grow the ledger file (Step 3 scenario 6).
- p99 `SharedLog::append` latency under 20ms on dev hardware (SSD), bounded by the Step 1-chosen window + `File::sync_data()` p99.
- Semantic-golden regression (Step 15) passes: message-shaped entries in the new ledger equal the Step 2 golden, even though the new ledger contains additional variants.
- All three failure-mode states (`LedgerDegraded`, `LedgerRepairFailed`, `LedgerMissing`) reachable and visually distinct in the UI.
- `context.json` reflects `system`/`gate` state at turn boundaries (test: mutate one during a turn, finish the turn, read `context.json`, assert updated — one-turn granularity is the contract).
- `write_save_result_to_inbox` still fires per user-observable turn (test: run a turn, assert inbox `message_count` updates within 200ms of the final commit via the drain task).

---

## Task 2 — Phase 2: State classification and honest abort

**Advances: Properties 4, 6, 7, 8.**

### The change

Introduce an explicit state-machine classifier for the log tail. Every possible tail maps to exactly one position.

```rust
// ox-inbox/src/resume.rs
pub enum ThreadResumeState {
    Idle,
    InStreamNoFinal,                         // TurnStart + AssistantProgress, no Assistant final
    AwaitingApproval { tool_use_id: String }, // pending ApprovalRequested
    AwaitingToolResult { tool_use_id: String, was_approved: bool }, // Assistant(tool_use) resolved or auto-approved, no ToolResult
    InTurnNoProgress,                        // TurnStart with nothing after it
}

pub fn classify(entries: &[LogEntry]) -> ThreadResumeState;
```

**Classifier must exhaust the real `LogEntry` enum** (`ox-kernel/src/log.rs:23`). At time of writing, the variants are: `User`, `Assistant`, `ToolCall`, `ToolResult`, `Meta`, `TurnStart`, `TurnEnd`, `CompletionEnd`, `ApprovalRequested`, `ApprovalResolved`, `Error`. This plan adds `TurnAborted`, `AssistantProgress`, and `ToolAborted`. The classifier uses an exhaustive `match` over all 14 variants and the compiler will reject silent drops when new variants are added.

Variant handling rules:

- **State-changing (set `ThreadResumeState`):** `TurnStart`, `Assistant`, `TurnEnd`, `TurnAborted`, `ApprovalRequested`, `ApprovalResolved`, `ToolCall`, `ToolResult`, `ToolAborted`, `AssistantProgress`.
- **Informational (does not change state; skip when walking back from tail):** `Meta`, `CompletionEnd`, `Error`, `User`.

The classifier walks backward from the tail, skipping informational variants, until it either:

- finds a state-changing variant that determines the `ThreadResumeState`, or
- reaches a `TurnStart` — the upstream kernel invariant is that turns are bounded by `MAX_TOTAL_ITERATIONS` (`ox-kernel/src/run.rs:623`), so a turn's entries are bounded by that constant times the per-iteration append count. The classifier uses `TurnStart` as the natural upper bound: classification needs to inspect at most one turn's worth of entries, and the most recent `TurnStart` marks where that starts.

If the walk reaches the beginning of the log without finding either, return `Idle`. If it walks more than `2 × MAX_TOTAL_ITERATIONS × 20` entries (a generous safety bound for a pathologically-long turn — 20 is a rough max of append-events-per-iteration) without hitting a `TurnStart`, return `Idle` and emit `tracing::warn!("ClassifierWalkCapped", thread_id, entries_scanned)`. The cap is expressed as a function of a kernel constant, not a magic number.

Single pass, bounded by one turn's worth of entries.

Classification is pure and lives in `ox-inbox` (it's a property of the durable ledger, not CLI logic).

### Thread mount lifecycle

Strict ordering. Each step completes before the next begins; no concurrent writers during any step.

```
1. Open thread dir.          (read-only I/O)
2. Read context.json.        (read-only I/O)
3. Open ledger read-only.    (file descriptor for replay)
4. Replay entries → in-memory SharedLog.  (LedgerWriter not started)
5. Close read-only fd.
6. Torn-tail repair if needed:
     6a. Re-open ledger read-write.
     6b. Truncate to last good offset.
     6c. Close.
     (on failure → LedgerRepairFailed terminal state, skip to step 10)
7. Start LedgerWriter on the (now-clean) ledger. No appends yet.
8. classify(SharedLog tail) → ThreadResumeState.
9. Dispatch by ThreadResumeState. **Phase 2 sets the stage but does not call `run_turn`** — that's wired in Phase 3 after the kernel prologue lands.
     - Idle:                  mark thread ready for user input.
     - InStreamNoFinal:       append TurnAborted(CrashDuringStream) via LedgerWriter; await commit; thread ready.
     - InTurnNoProgress:      append TurnAborted(CrashBeforeFirstToken) via LedgerWriter; await commit; thread ready.
     - AwaitingApproval:      (Phase 2) thread ready — UI shows the stale modal from the replayed `ApprovalRequested`; user can send new input (which starts a new turn and drops the old pending approval).
                              (Phase 3, after kernel prologue lands) call run_turn() — the kernel's prologue sees the unresolved tool_use and the prior ApprovalRequested, reconstructs the ApprovalRequest by joining with the matching ToolCall, writes a new ApprovalRequested, and blocks on approval. UI modal state == pre-crash state.
     - AwaitingToolResult:    (Phase 2) append ToolAborted(CrashDuringDispatch) via LedgerWriter; await commit; thread ready with the abort visible in history.
                              (Phase 3) after ToolAborted is appended, call run_turn() — kernel prologue sees the resume-tool-dispatch shape and writes a new ApprovalRequested with post_crash_reconfirm=true.
     - LedgerDegraded/
       LedgerRepairFailed/
       LedgerMissing:         display banner; do not start LedgerWriter; do not call run_turn.
10. Mount complete. Return handle to caller.
```

Invariants:
- **No `run_turn()` call before classification completes.** `run_turn` can append events that would change classification.
- **`LedgerWriter` is running before any `TurnAborted` / `ToolAborted` append.** Durable-append requires the writer.
- **Any `LedgerXxx` error state aborts mount.** No partial state exposed to the UI.
- **Approval state is not carried across the crash in a channel — it is reconstructed by letting `run_turn()` re-request approval from the log** (Phase 3 once the kernel prologue lands). Between Phases 2 and 3, `AwaitingApproval` threads are in a visually-honest but not-interactive state: the user sees the old approval request in history but cannot act on it; sending new input starts a new turn.

### Multi-thread mount (what's verified, what's not)

**Verified.** Mount is lazy per-thread (`ox-cli/src/thread_registry.rs:3–4`, `from_thread_dir` at line 48). A thread is mounted when first accessed, not on startup. Startup cost is O(1) regardless of inbox size.

**Verified.** Per-thread isolation: each thread has its own `LedgerWriter`, `SharedLog`, `ApprovalStore`. No cross-thread shared mutable state is introduced by this plan.

**Not verified — and I'm not claiming it.** Whether mount runs on its own Tokio task or inline on the broker dispatch task is an implementation detail of `ThreadRegistry`'s `AsyncReader` / `AsyncWriter` impls (`thread_registry.rs:236`, `:252`). I did not trace it. If mount is inline, a slow replay of thread A will block dispatch to thread B until A's mount completes — a possible UX hiccup on cold-open of a large ledger.

**Decision for this plan:** do not depend on concurrent mount working. If the inline-mount case causes a noticeable hiccup in testing, spawn mount onto its own task as a follow-up. Treat concurrent-mount performance as out of scope here.

Inbox-level operations that touch multiple threads (listing, search) only read `context.json` per thread, never the full ledger. They cannot trigger mount and cannot interact with in-flight mount.

`TurnAborted` is a new `LogEntry` variant rendered in the transcript as a muted "interrupted" marker.

### Tasks

- [x] **Step 1:** Add `TurnAborted` and `ToolAborted` variants to `LogEntry`:
  - `TurnAborted { reason: TurnAbortReason }` where `TurnAbortReason ∈ { CrashDuringStream, CrashBeforeFirstToken, UserCanceledAfterCrash }`.
  - `ToolAborted { tool_use_id: String, reason: ToolAbortReason }` where `ToolAbortReason ∈ { CrashDuringDispatch }`.
  Round-trip test per variant. All read-site `match` over `LogEntry` must be exhaustive (compiler-enforced per Property 4's caveat).
- [x] **Step 2:** Write `classify(entries: &[LogEntry]) -> ThreadResumeState` in `ox-kernel/src/resume.rs` (deviation: moved from `ox-inbox` to `ox-kernel` so non-CLI shells can share it). Exhaustive `match` over the 13 variants that exist in Task 2 (the 14th, `AssistantProgress`, is Task 4). Walks tail backward, skipping informational variants (`Meta`, `CompletionEnd`, `Error`, `User`). One unit test per `ThreadResumeState` variant. One unit test per informational variant asserting it is correctly skipped. Golden-file hand-crafted ledgers.
- [x] **Step 3:** Property test: generate random valid ledgers from a shape grammar, classify, replay, re-classify — assert idempotence.
- [x] **Step 4:** Property test: generate valid ledgers, truncate at random byte offsets, feed through torn-tail repair + classify — assert the result is always a valid `ThreadResumeState`, never a panic.
- [x] **Step 5:** Wire `classify` into `ThreadRegistry::from_thread_dir` per the lifecycle sequence. Dispatch `InStreamNoFinal` / `InTurnNoProgress` to append `TurnAborted`. Dispatch `AwaitingToolResult` to append `ToolAborted`.
- [x] **Step 6:** Render `TurnAborted` and `ToolAborted` in `history_view.rs` as muted "interrupted" markers (theme entries in `theme.rs`). `ToolAborted` renders inline with its tool call, `TurnAborted` renders at turn boundary.
- [x] **Step 7:** Tracing: emit `ThreadResumeClassified { thread_id, state }` on every mount; `ToolAbortedAppended { thread_id, tool_use_id, reason }` on append.

### Success criteria

- Every possible ledger produces a valid UI. No dangling `TurnStart`. No assistant bubble without a response.
- Property tests pass under `proptest` with 10k cases.

---

## Task 3 — Phase 3: Approval resumption

**Advances: Properties 5, 7.**

### The change

P7 verification found that `run_turn` does not inspect the log tail — it starts fresh each call with a `TurnStart`. **Resumption therefore requires a real kernel change**, not just wiring.

**The kernel change (Task 3 Step 2).** Add a log-inspection prologue to `run_turn` at `ox-kernel/src/run.rs:597`. Before the existing code path (emit `TurnStart` + issue completion), check whether the log tail matches a "resume" shape:

- **Resume-approval shape:** log tail is `Assistant(tool_use) → ApprovalRequested(…)` with no subsequent `ApprovalResolved` for this `tool_use_id`. Skip `TurnStart`, skip the completion call; jump to the approval-wait state with the same `tool_use_id` the old turn had.
- **Resume-tool-dispatch shape:** log tail is `Assistant(tool_use) → ApprovalResolved(allow) → ToolAborted(…)`. Jump to the post-crash-reconfirm state (see `AwaitingToolResult` section).
- **Normal shape:** everything else. Fall through to the existing `TurnStart` + completion path.

This is the smallest change that makes `run_turn` composable with the classifier's output without forking a separate `resume_turn` function. Kernel tests cover each shape.

**The durable-state preservation.** `LogEntry::ApprovalRequested` stores only `input_preview` (P10, verified). The full `tool_input` lives in the matching `LogEntry::ToolCall` entry written earlier in the same turn. The kernel's resume-approval prologue reconstructs the full `ApprovalRequest` by joining the `ApprovalRequested` entry with its matching `ToolCall` entry by `tool_use_id`. **No P6 schema change is needed** — the `ToolCall` entry already carries the full input (`ox-kernel/src/log.rs:43–50`). P6 is downgraded from "schema change" to "adjust the kernel prologue to read from `ToolCall` for recovery."

**Flow on `AwaitingApproval`:**

1. Classifier returns `AwaitingApproval { tool_use_id }`.
2. Mount lifecycle step 9 calls `run_turn()`.
3. Kernel prologue detects the resume-approval shape by scanning the tail.
4. Kernel reconstructs `ApprovalRequest { tool_name, tool_input }` from the `ToolCall` + `ApprovalRequested` entries.
5. Kernel writes `approval/request` with that request. `ThreadNamespace::write` (`thread_registry.rs:281–292`) appends a **new** `LogEntry::ApprovalRequested` to the log and populates `ApprovalStore.pending` + fresh oneshot.
6. Wasm thread blocks on the oneshot. TUI reads `approval/pending` and renders the modal. Same visual state the user had pre-crash.
7. User decides. `ApprovalResolved` entry written. Oneshot resolves. Tool dispatches (or denies).

**No second LLM completion call.** The kernel prologue routes around the completion step when the resume shape matches.

**Duplicated `ApprovalRequested` entries in the log.** The old entry (pre-crash) stays in the log as history; a new one (post-mount) is written to drive the current approval wait. Both are durable. The log is honest about what happened.

### `AwaitingToolResult` — avoid double side effects

Most dangerous recovery path. `AwaitingToolResult` means: we have an `Assistant(tool_use)` (and possibly an `ApprovalResolved(allow)`) but **no `ToolResult`**. The log does not tell us whether the tool:

- **(a)** never started (crash happened before dispatch), or
- **(b)** was running when the process died (side effect may be partially complete), or
- **(c)** completed on the OS level but the `ToolResult` write was lost.

**From the log alone, we cannot distinguish (a) from (b) from (c).** Assume the side effect may have happened. Naive re-dispatch would double-execute.

**Decision — mechanism:**

On classifier returning `AwaitingToolResult`, the mount lifecycle (Task 2 step 9) **appends a `ToolAborted { tool_use_id, reason: CrashDuringDispatch }` log entry before calling `run_turn()`.**

When `run_turn` inspects the log tail and sees `Assistant(tool_use)` → (optional `ApprovalResolved(allow)`) → `ToolAborted` (new), it treats this as "the tool was interrupted and needs re-confirmation" — **not** "the tool was already approved, dispatch it." This requires a small kernel rule: `ToolAborted` invalidates any prior `ApprovalResolved` for the same `tool_use_id`. The kernel writes a fresh `ApprovalRequested` with a `post_crash_reconfirm: true` flag on it.

The UI picks up the fresh approval request and renders the re-confirm modal. Copy (draft):

> ⚠ **This tool may have already run.**
>
> `{tool_name}` was dispatched before ox-cli exited, but its result was never recorded. The operation may have completed, partially completed, or not started.
>
> **Input that was sent:** `{abbreviated tool_input}`
>
> Retrying may repeat the side effect (e.g. re-run a shell command, re-send a request, re-write a file).
>
> **[Retry]** — run the tool again.
> **[Skip]** — record a synthetic result and let the model continue.
> **[Cancel turn]** — abort this turn entirely.

**User's choice drives what gets written:**

- **Retry** → `ApprovalResolved(allow, post_crash_reconfirm)` → tool dispatches normally via existing code.
- **Skip** → `ApprovalResolved(deny, post_crash_reconfirm)` → kernel writes a synthetic `ToolResult` per the shape spec below and continues the turn.
- **Cancel turn** → kernel exits with `TurnAborted(UserCanceledAfterCrash)`; no further work in this turn.

**Covers auto-approved tools.** If the original tool was auto-approved (original log: `Assistant(tool_use)` → `ApprovalResolved(allow, auto)` → no result), the classifier still produces `AwaitingToolResult`, and the `ToolAborted` entry still triggers re-confirmation. Auto-approval policy does not skip the re-confirm modal. **Encoded in the kernel rule, not a UI bolt-on.**

**No runtime flag disables this.** The risk of double side-effect is higher than the cost of a click.

### Skip-path `ToolResult` shape (pinned)

The synthetic `ToolResult` on Skip must be constructed so the model treats it as a terminal signal — not an error that invites retry, not an empty response that invites rephrasing. Shape:

```json
{
  "type": "tool_result",
  "tool_use_id": "<original>",
  "is_error": false,
  "content": "[ox-cli: skipped by user after crash recovery. The tool was not re-executed. Do not retry this tool in this turn.]"
}
```

Key properties:
- `is_error: false` — the model treats this as a successful non-answer, not a retryable failure.
- **Marker prefix** `[ox-cli:` — makes the synthetic origin recognizable in transcripts and logs.
- **"Do not retry" directive** — a strong instruction not to re-call the same tool. Models honor this in practice; if we see regressions, we can also inject a system-prompt note on Skip (deferred until needed).
- The content string lives in `ox-cli/src/theme.rs` (or a strings module if one is introduced) as a `const`, not inline.

This shape is part of the contract for Task 3 Step 6b; changing it is a plan amendment, not a drive-by edit.

### Tasks

- [x] **Step 1:** Add a `post_crash_reconfirm: bool` field to `LogEntry::ApprovalRequested` (`ox-kernel/src/log.rs:98–102`). Default `false` on serialize-elided, round-trip tests per variant. Also extend the `ThreadNamespace::write` writer at `thread_registry.rs:281–292` to pass the flag through (default `false` on normal-path writes).
- [ ] **Step 2 (the big one — kernel change).** Add a log-inspection prologue to `run_turn` at `ox-kernel/src/run.rs:597`. Before the existing `TurnStart` emission (line 634), scan the log tail for resume shapes:
  - `Assistant(tool_use) → ApprovalRequested` with no matching `ApprovalResolved` → resume-approval.
  - `Assistant(tool_use) → ApprovalResolved(allow) → ToolAborted` → resume-tool-dispatch.
  - Otherwise → normal path.
  On resume-approval: look up the matching `ToolCall` entry by `tool_use_id` to reconstruct the full `ApprovalRequest`, write `approval/request`, block on the fresh oneshot, then continue with tool dispatch on decision. On resume-tool-dispatch: write a new `ApprovalRequested { post_crash_reconfirm: true }`, same wait path. Unit tests: one per shape, using hand-crafted logs and an in-process `Store`. Scope: this is the single largest kernel change in the plan.
- [ ] **Step 3:** Kernel rule for Skip (deny + post_crash_reconfirm): when `ApprovalStore::write("response", deny)` resolves the oneshot and `post_crash_reconfirm` was set on the request, the kernel writes a synthetic `ToolResult` per the shape spec (content prefixed `[ox-cli:` etc.), then continues the turn (loops back to completion). Unit test with fake tool + counter assertion.
- [ ] **Step 4:** Kernel rule for Cancel. Extend `ox_types::Decision` with a `CancelTurn` variant (`ox-types/src/approval.rs:13–20`). When the decision resolves as `CancelTurn`, the kernel writes `TurnAborted { reason: UserCanceledAfterCrash }` and returns `Ok(())` from `run_turn`. Update existing `Decision::is_allow` / `is_deny` to handle the new variant (`CancelTurn` is neither allow nor deny; audit call sites). The UI modal surfaces this as the `[Cancel turn]` button, writing `approval/response` with `{"decision": "cancel_turn"}`.
  - [x] **Schema half (Task 3a):** `Decision::CancelTurn` added with `is_allow`/`is_deny`/`as_str`/`Display` updated; every `match Decision` and `LogEntry::ApprovalRequested { .. }` destructuring across the workspace audited and fixed.
  - [ ] **Kernel-behavior half (Task 3c):** write `TurnAborted { reason: UserCanceledAfterCrash }` on `CancelTurn` and return from `run_turn`.
- [ ] **Step 5:** End-to-end test: `AwaitingApproval` resumption with a fake transport that **fails if called more than once per turn**. Scenario: start turn → approval requested → soft-crash → remount → modal reappears → approve → tool runs → turn completes. Assert transport call count == 1. Task 0 harness, in-process mode.
- [ ] **Step 6:** End-to-end tests for `AwaitingToolResult` — fake tool side effect increments a shared counter. Scenario: crash *after* tool dispatch began (counter = 1) *before* `ToolResult` written.
  - **6a. Retry:** relaunch → `ToolAborted` appended → modal appears (`post_crash_reconfirm` set) → Retry → counter reaches 2, turn completes.
  - **6b. Skip:** modal → Skip → counter stays at 1, synthetic `ToolResult` written, model receives it, turn continues. **Default safe path.**
  - **6c. Cancel:** modal → Cancel → `Decision::CancelTurn` resolves oneshot → kernel writes `TurnAborted(UserCanceledAfterCrash)` and exits cleanly. Counter stays at 1.
  - **6d. Auto-approved tool:** configure policy to auto-approve; crash during dispatch; relaunch → modal appears with `post_crash_reconfirm`. Policy cannot skip it.
- [ ] **Step 7:** Tracing: `ApprovalReRequested { thread_id, tool_name, post_crash_reconfirm }` on kernel write; `PostCrashReconfirmDecision { thread_id, tool_name, decision }` on user pick; `TurnAbortedUserCanceled { thread_id }` on Cancel.

### Success criteria

- Approval-resume (pre-dispatch) scenario: UI modal state after remount matches pre-crash state. Fake-transport call count == 1.
- Tool-dispatch-resume scenario: `ToolAborted` appended on mount; `post_crash_reconfirm` modal appears; Retry/Skip/Cancel each produce the asserted behavior (counter, synthetic result, `TurnAborted`).
- **No tool side effect is re-executed without explicit user confirmation.** Retry is the only re-execution path; Skip and Cancel never re-execute; auto-approval policy cannot bypass the modal.
- Kernel prologue in `run_turn` handles all three tail shapes without regressing the normal (non-resume) path — regression test: canonical turn produces byte-identical emit stream before and after Task 3.
- `Decision::CancelTurn` is handled exhaustively across all `match` sites on `Decision` (compiler-enforced).

---

## Task 4 — Phase 4: Durable streaming

**Advances: Properties 1, 3.**

### The change

Without this, a mid-stream crash loses the assistant text that was on screen when the user `Ctrl+C`'d. With this, the partial text is preserved, followed by a `TurnAborted` marker.

- Add `AssistantProgress { accumulated: String, epoch: u64 }` variant to `LogEntry`.
- Emit one `AssistantProgress` per UI repaint cadence (~100–200ms), not per token. Cadence is already rate-limited by the UI; piggyback on it.
- `HistoryView` projects the latest `AssistantProgress` for an in-flight turn into `turn/streaming` on replay. This happens for free once the variant is serialized into the log.
- On stream finalization, append `Assistant { final: ... }`. Replay prefers `Assistant` over trailing `AssistantProgress` for the same turn.

Budget: ~5–10 extra appends/sec during streaming. With group-commit `File::sync_data()` this is imperceptible on local SSD.

### Tasks

- [ ] **Step 1:** Add `AssistantProgress` variant with round-trip tests.
- [ ] **Step 2:** In `ox-kernel::run.rs`, emit `AssistantProgress` at repaint cadence during streaming. Guard behind `OX_DURABLE_STREAM=1` initially to allow A/B comparison.
- [ ] **Step 3:** Update `HistoryView` to project the latest `AssistantProgress` into `turn/streaming` on replay. Verify existing `turn/streaming` consumers keep working.
- [ ] **Step 4:** Update classification: `InStreamNoFinal` tail is identified by `TurnStart + AssistantProgress*, no Assistant final`. Emit `TurnAborted { reason: CrashDuringStream }` on mount and render the partial text followed by the abort marker.
- [ ] **Step 5:** Crash-harness test: start a turn with a slow-streaming fake transport, drop the `App` mid-stream (soft crash, optionally paired with the `LedgerWriter` freeze hook to park between `write_all` and `sync_data`), remount, assert the partial text visible pre-crash is visible post-remount.
- [ ] **Step 6:** Flip `OX_DURABLE_STREAM` default to on when all of the following are true, measured against the Phase 3 baseline from the Task 0 harness:
  - p99 `LogStore::append` commit latency regression ≤ 10% (absolute: under 20ms on dev SSD).
  - No new failing tests in `cargo test` over two consecutive weeks of daily runs.
  - No increase in `LedgerCommit` sync_data_us p99.
  If any metric regresses, keep the flag off and file an issue before flipping. Record the flip decision (with measured numbers) in the PR description.

### Success criteria

- Crash mid-stream → relaunch shows the partial text the user was watching, followed by a `TurnAborted` marker.
- Per-turn p99 commit latency remains under 20ms.

---

## Task 5 — Integrated cross-phase scenarios

**Advances: Properties 1, 4, 5, 6, 9.**

Individual-phase tests cover one mechanism at a time. This task covers the interactions.

### Scenarios

Each scenario is a single crash-harness test asserting the exact relaunch state.

- [ ] **S1. Crash mid-stream with no approval pending.** Tests Phase 1 + Phase 4 interaction. Drop the `App` (in-process soft crash) while `AssistantProgress` is being appended, optionally with the `LedgerWriter` parked at `after_write_before_sync`. Remount: partial text visible, `TurnAborted(CrashDuringStream)` marker follows.
- [ ] **S2. Crash with approval pending mid-stream.** Cannot actually happen (approval gates tool dispatch, which happens after stream finalization) — but assert the classifier handles this shape defensively and never panics. Property test covers this.
- [ ] **S3. Crash between tool result write and next LLM call.** `ToolResult` durably committed, kernel about to call model again, soft-crash. Remount: classification is `Idle` (or similar — the turn is mid-agentic-loop but has no outstanding tool). Kernel on spawn continues the loop: re-prompts model with full history. No user-visible oddity.
- [ ] **S4. Crash during torn-tail repair.** Use a `LedgerWriter` freeze hook placed inside the repair path so the test can drop the `App` mid-truncate in-process. Remount: second repair attempt succeeds, no data loss beyond the original torn tail.
- [ ] **S5. Crash while `LedgerDegraded` state is being written.** Simulate a disk-full error via a `LedgerWriter` test hook (`ErrorKind::StorageFull` on the Nth call, same mechanism as `OX_TEST_FREEZE_AT`); drop the `App` before the degraded state is persisted. Remount: torn-tail repair triggers, then the first append attempt re-enters `LedgerDegraded`. User sees the banner either way. We do not use real disk-full simulation (tmpfs quota, `libfiu`) because test-hook injection is deterministic and portable across developer OSes.
- [ ] **S6. Back-to-back crashes.** Three successive in-process drop+remount cycles during the same turn at different script points. Final remount still produces a valid, classifiable ledger.
- [ ] **S7. Long-running random-drop soak.** Nightly CI job (not in-PR). Runs for a wall-clock budget of **30 minutes** against a `proptest`-generated space of turn shapes and in-process crash points (combinations of `App` drop, `OX_TEST_FREEZE_AT`, and injected write/sync failures). Each iteration: pick a turn shape, run it under the harness, crash at a uniformly-random script point, remount, classify, assert no panic / ledger valid / every committed entry present. Results posted to a shared dashboard; a failed iteration saves a minimized reproduction via `proptest`'s shrinking into `tests/fixtures/crash_repros/` and opens an issue automatically. This is not "fuzz" in the `cargo-fuzz` sense (no sanitizer-driven coverage guidance); it's a large-sample deterministic-under-seed soak. Real-signal (`SIGKILL`) soak coverage is a separate plan.

### Success criteria

- S1–S6 pass deterministically in PR CI.
- S7 (nightly) produces zero panics and zero unrecoverable states over its wall-clock budget for seven consecutive nights before Phase 4 is considered stable.

### Commit

`test(cli): cross-phase crash scenarios`

---

## Observability

Tracing events added by this plan, by phase:

| Event | Phase | Fields |
|-------|-------|--------|
| `LedgerCommit` | 1 | `thread_id`, `entries`, `bytes`, `sync_data_us` |
| `LedgerTailRepaired` | 1 | `thread_id`, `bytes_dropped` |
| `LedgerDegraded` | 1 | `thread_id`, `error` |
| `LedgerRepairFailed` | 1 | `thread_id`, `error` |
| `LedgerMissing` | 1 | `thread_id` |
| `ThreadResumeClassified` | 2 | `thread_id`, `state` |
| `TurnAbortedAppended` | 2 | `thread_id`, `reason` |
| `ToolAbortedAppended` | 2 | `thread_id`, `tool_use_id`, `reason` |
| `ApprovalReRequested` | 3 | `thread_id`, `tool_name`, `post_crash_reconfirm` (bool) |
| `PostCrashReconfirmDecision` | 3 | `thread_id`, `tool_name`, `decision` (`Retry` \| `Skip` \| `Cancel`) |
| `AssistantProgressAppended` | 4 | `thread_id`, `epoch`, `accumulated_len` |

A debug panel (`OX_DEBUG_RESUME=1`) displays the classified state on thread mount.

---

## Testing strategy

1. **Crash harness (Task 0).** Headless, in-process-only test rig. Drop `App` → remount for the default crash path; `LedgerWriter` freeze hooks (`OX_TEST_FREEZE_AT`, failure injection) cover syscall-boundary scenarios. Scripted fake transport. Asserts on ledger bytes and `SharedLog` snapshots; no terminal emulation, no subprocess spawn. Foundation for everything else.
2. **Happy-path golden regression.** Byte-equals comparison (modulo whitelisted timestamp fields) between pre-change and post-change ledgers for a canonical turn. Runs on every PR. Golden updates gated by `tests/fixtures/REVIEWERS.md` governance.
3. **Classifier unit tests.** One per `ThreadResumeState` variant. Golden-file inputs and outputs.
4. **Classifier property tests.** Random valid ledgers, random truncations — idempotence and non-panic over 10k cases via `proptest`.
5. **Torn-tail repair unit tests.** Hand-crafted partial writes, assert recovery. Separate tests for repair-failure paths via the `LedgerWriter` test-injection mechanism (not real disk simulation).
6. **Approval-resume E2E.** Task 3 Step 5 — fake transport that fails if called more than once.
7. **Post-crash-reconfirm matrix.** Task 3 Step 6 — all three user decisions (Retry / Skip / Cancel) plus auto-approved-tool variant. Counter-based side-effect detection.
8. **Cross-phase scenarios (Task 5).** Six deterministic PR-CI scenarios (S1–S6) covering inter-phase interactions.
9. **Nightly random-drop soak (Task 5 S7).** 30-minute `proptest`-driven run across randomized turn shapes and in-process crash points (drops, freeze-hooks, injected failures). Failed iterations auto-shrink to reproductions under `tests/fixtures/crash_repros/`.
10. **Fsync accounting benchmark.** Assert p99 `append` latency under budget; catches per-entry fsync storm regressions.

---

## Non-goals

Documented so that readers of this plan do not expect them, each with a reason:

- **Retry / rewind / branching of turns** — rejected in planning conversation (user chose option 1: pure resume). A future plan may reconsider.
- **Cross-machine durability.** Scope is local disk only.
- **Preserving the Rust future of the parked worker.** We don't need to — kernel control flow is deterministic from the log (Property 7).
- **Surviving disk corruption beyond torn-tail repair.** Out of scope; `LedgerDegraded` / `LedgerRepairFailed` states cover the user-visible surface.
- **New user-visible "restart" command.** "Restart" is a runtime guarantee, not an action.
- **Ledger format versioning.** Deferred to its own plan. *Reason:* this plan does not change the shape of any existing `LogEntry` variant (it only adds new variants — `TurnAborted`, `AssistantProgress` — which are trivially additive via serde's untagged-variant handling). A format version number only pays for itself when we need to evolve an *existing* variant's shape. When that day comes, write the versioning plan then; adding `"v"` now is speculative.
- **Tool idempotency inference.** We do not attempt to classify tools as "safe to auto-retry" vs "dangerous." All post-crash tool-result-missing states go through the `PostCrashReconfirm` modal, full stop.

- **Ledger compaction.** Phase 4 adds ~5–10 `AssistantProgress` appends per second during streaming. Over a multi-month conversation, the ledger grows to MBs–GBs, and mount-time replay becomes user-visible latency. *This plan does not address compaction.* **Revisit trigger** (any of): mount replay p99 > 500ms; single-thread ledger > 100MB; user-reported slow startup. When any trigger fires, a follow-up plan adds either (a) a compaction pass that collapses trailing `AssistantProgress` into the final `Assistant` entry at turn end, or (b) a snapshot-and-truncate mechanism. Until then, unbounded growth is a known cost.

---

## Commit boundaries

One *commit per Task* is aspirational; Task 1 and Task 3 each warrant splitting for bisect friendliness given their scope. Landing order:

0. `test(cli): crash harness infrastructure`
1a. `feat(inbox): LedgerWriter thread + SharedLog durability callback`
1b. `feat(inbox): narrow save → save_config_snapshot (strip ledger-append)`
1c. `feat(cli): CommitDrain task for inbox-index propagation`
1d. `feat(cli): remove pre-turn save; switch post-turn to save_config_snapshot`
2. `feat(inbox): resume-state classifier + TurnAborted + ToolAborted rendering`
3a. `feat(types): Decision::CancelTurn + exhaustive audit`
3b. `feat(kernel): log-inspection prologue for run_turn (resume shapes)`
3c. `feat(kernel): Skip + Cancel rules for post_crash_reconfirm`
3d. `feat(cli): wire approval resumption into mount lifecycle`
4. `feat(kernel): durable streaming via AssistantProgress entries`
5. `test(cli): cross-phase crash scenarios`

Dependency graph:

```
Task 0 ──> 1a ──> 1b ──> 1c ──> 1d ──> 2 ──┬──> 3a ──> 3b ──> 3c ──> 3d
                                           └──> 4
                                                3d + 4 ──> 5
```

Each commit is independently revertible. Tasks 1a–1d stack linearly because the config-snapshot responsibility must land before the pre/post-turn saves are removed. Task 3a can be done in parallel with Task 2 but must land before 3b. Task 4 is parallel to Task 3 once Task 2 is merged.

# Thread-info modal — S-tier

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring the `i` thread-info modal to S-tier. Deliver the design properties that make this feature correct, live-per-entry, discoverable, observable, performance-bounded, and error-explicit — with no deferrals or silent failure modes.

**User-facing impact when this lands:**

- The info modal updates per log entry, not per save boundary. Mid-turn stats tick up in real time.
- `i` works on both the Inbox and Thread screens — same modal, same semantics.
- Every key binding is visible in the `?` shortcuts modal.
- When a fetch fails or a thread is deleted while the modal is open, the user sees a distinct state instead of a silently-blank modal.

**Tech Stack:** Rust (edition 2024), tokio, ox-broker, ox-inbox, ox-kernel, ratatui.

**Scope:** Self-contained. Everything in this document ships together as one logical unit, split into two atomic commits for bisect friendliness.

---

## Design properties

Every task below advances one of these. If a task doesn't advance a property, it's not in the plan. If a property isn't advanced by any task, the plan is incomplete.

1. **Cache keys reflect data identity, not positional aliases.** Key is `(thread_id, log_count)` — the thread and the count of log entries at cache time. Not row index, not SQLite `last_seq` (which lags log writes).
2. **Live updates per log entry.** Freshness is derived from `threads/{id}/log/count` — a scalar read on the live log, atomic under the log's internal `Mutex`. No save-boundary staleness.
3. **Async boundaries are honest.** I/O-doing functions are `async fn`. No sync wrappers around `rt_handle.block_on` that force `spawn_blocking` in tests.
4. **Tests prove mechanism by mutating state between calls.** Every cache assertion follows: capture → mutate underlying store → re-observe. No outcome-matchers.
5. **Exhaustive enum matching at every variant seam, compiler-enforced.** Where exhaustiveness matters, a `match` with all named variants makes the compiler reject silent drops when variants are added.
6. **Every keybinding is discoverable.** `i` appears in the `?` shortcuts modal on every screen where it's bound. Cross-screen consistency is explicit — deliberate binding or deliberately no binding with rationale.
7. **Every I/O seam emits `tracing`.** Cache hit/miss/duration, modal open/close, fetch errors.
8. **Performance is pinned by a linearity-ratio test.** Catches quadratic regressions without depending on CI-runner absolute speed.
9. **Error paths have UX decisions, not silent defaults.** Distinct states for "loading," "fetched OK," and "fetch failed" — visible to the user and traced for operators.
10. **No deferrals.** Every prior-round item is either executed in this plan or deleted with a named, non-soft reason.

---

## Prerequisites (verify before implementation)

- [x] `threads/{id}/log/count` returns `Value::Integer(n)` equal to the current length of the thread's log entries. Grep: `ox_kernel::log::LogStore::read` — confirm the `"count"` path is implemented and returns the live count under the log's mutex.
- [x] `log/append` writes through the broker accept typed `LogEntry` values end-to-end — already proven by `broker_setup::tests::fetch_thread_info_reads_log_through_broker_end_to_end`.
- [x] `inbox/threads/{id}` returns a single thread row (not only a list) for Thread-screen cache lookup. Grep: `ox_inbox::reader` — confirm scalar thread read by id.

If any prerequisite fails, the plan is blocked and must be revised before any task runs.

---

## Task 1 — Cache redesign with live-per-entry semantics

**Advances: Properties 1, 2.**

### The change

```rust
// event_loop.rs
pub(crate) struct DialogState {
    // ...
    pub thread_info: Option<ThreadInfoEntry>,
    // drops thread_info_selected_row entirely
}

pub(crate) struct ThreadInfoEntry {
    pub info: crate::types::ThreadInfo,
    pub log_count_at_cache: i64,
}
```

`refresh_thread_info_cache` flow:

1. Read `UiSnapshot` once.
2. Resolve the selected thread id:
   - Inbox screen: read the inbox listing (or search results), index by `selected_row`.
   - Thread screen: `ScreenSnapshot::Thread(s)` → `s.thread_id`.
3. Read `threads/{id}/log/count` — scalar, cheap, serialized by the log's internal mutex.
4. Compare `(id, log_count)` to `dialog.thread_info.as_ref().map(|e| (&e.info.meta.id, e.log_count_at_cache))`. Match → hit, return.
5. Miss → `fetch_thread_info`, store `ThreadInfoEntry { info, log_count_at_cache }`.

### Concurrency contract (documented on `refresh_thread_info_cache`)

> The freshness signal is `log/count`, which reads `SharedLog`'s `Vec<LogEntry>` length under its `Mutex`. Appends to the log and reads of its count are serialized — we can never read a partial count relative to an in-flight append. The log entries themselves, read later by `fetch_thread_info`, see at least the entries counted; they may see strictly more if an append lands between the two reads. That's "cache may serve fresher than its key claims," which is benign — the next frame sees an even higher `log_count` and refetches.

### Invariants after landing

- Cache hit iff `(thread_id, log_count)` identical to cached pair.
- Modal reflects a new log entry within one frame of the entry appending — independent of save boundaries. Verified by Test A.
- Selection change between threads invalidates the cache — verified by Test C.
- Search-state transition to a different thread at the same row index invalidates — verified by Test D.

### Tasks

- [x] **1.1** Define `ThreadInfoEntry` on `DialogState`, remove `thread_info_selected_row`, update `DialogState` initializer.
- [x] **1.2** Rewrite `refresh_thread_info_cache`: resolve id by screen, read `log/count` for that id, compare, fetch on miss. Single function, no branch duplication between Inbox/Thread.
- [x] **1.3** Update the `ToggleThreadInfo` / `DismissThreadInfo` branches in `action_executor` to clear `thread_info` (no separate row-tracker).
- [x] **1.4** Update every reader of `dialog.thread_info` (`tui.rs`, `view_state.rs`) to unwrap `ThreadInfoEntry.info`.
- [x] **1.5** Rewrite existing cache tests (`action_executor::tests::toggle_thread_info_flips_flag_and_drops_cache`, etc.) to the new field shape. The Loading-state test from Task 8 will cover the user-visible "not yet fetched" case.

---

## Task 2 — Async conversion of `write_save_result_to_inbox`, with codebase audit

**Advances: Property 3.**

### The change

```rust
// Before:
pub(crate) fn write_save_result_to_inbox(
    broker_client: &ox_broker::ClientHandle,
    rt_handle: &tokio::runtime::Handle,
    thread_id: &str,
    result: &ox_inbox::snapshot::SaveResult,
);

// After:
pub(crate) async fn write_save_result_to_inbox(
    broker_client: &ox_broker::ClientHandle,
    thread_id: &str,
    result: &ox_inbox::snapshot::SaveResult,
);
```

Body: `rt_handle.block_on(client.write(...))` → `client.write(...).await`.

### Tasks

- [x] **2.1** Change signature to `async fn`, drop `rt_handle`, replace `block_on` with `.await`.
- [x] **2.2** Update both `agents.rs` call sites (pre-turn save at line ~411, post-turn save at line ~502) to `.await`. Remove any `rt_handle` captures that existed only for this call.
- [x] **2.3** Update `broker_setup::tests::write_save_result_to_inbox_updates_live_counts` — delete the `spawn_blocking` wrap, call the helper with `.await`.
- [x] **2.4** **`block_on` audit.** Run `grep -rn "block_on" crates/ox-cli/ crates/ox-ui/ crates/ox-inbox/ --include='*.rs'`. For every hit, categorize in the appendix below:
  - **(a) Convertible.** Inside an `async fn`, could be `.await`. → Convert in this PR.
  - **(b) Sync-trait boundary.** Inside a trait impl that must be sync (e.g., `Writer` dispatcher closures). → Leave + one-line justification naming the trait.
  - **(c) Sync entry point.** `main()` or pre-runtime code. → Leave + one-line justification.
  - **PR merge gate:** this audit appendix must contain one categorized line per `grep` hit, or the explicit text `no hits found`. Unfilled audit = no merge.

### Audit appendix (must be filled before merge)

```
block_on audit — filled during implementation
Source: rg -n "block_on" crates/ox-cli/ crates/ox-ui/ crates/ox-inbox/ --include='*.rs'

ox-ui:    no hits found.
ox-inbox: no hits found.

ox-cli:
  main.rs                                      main() is `#[tokio::main(flavor = "multi_thread")]`; no block_on calls remain.
  app.rs                                       App::{read_history_at, update_thread_state, history_up, history_down, send_input_with_text, do_compose, do_reply} are all `async fn`; no block_on calls remain.
  crates/ox-cli/src/broker_setup.rs:53       (b)  CommandStore Dispatcher closure (Box<dyn FnMut(&Path, Record) -> Result + Send + Sync>) invoked from sync Writer::write.
  crates/ox-cli/src/broker_setup.rs:64       (b)  InputStore Dispatcher closure (same Dispatcher type) invoked from sync Writer::write.
  crates/ox-cli/src/agents.rs:47             (b)  ox_tools::completion::CompletionTransport::send is a sync trait method.
  crates/ox-cli/src/agents.rs:54             (b)  Same trait impl as :47; per-turn token usage write.
  crates/ox-cli/src/agents.rs:400            (c)  agent_worker — sync OS thread spawned via thread::spawn; bridges to broker.
  crates/ox-cli/src/agents.rs:410            (c)  agent_worker — pre-turn write_save_result_to_inbox call site.
  crates/ox-cli/src/agents.rs:505            (c)  agent_worker — post-turn write_save_result_to_inbox call site.
  crates/ox-cli/src/agents.rs:515            (c)  agent_worker — search-index trigger via inbox/index/{tid}.
  crates/ox-cli/src/agents.rs:547            (c)  agent_worker — final thread-state update.
  crates/ox-cli/src/agents.rs:665            (b)  CliEffects::broker_write helper, called only from HostEffects::emit_event (sync trait).
  crates/ox-cli/src/agents.rs:685            (b)  HostEffects::emit_event sync trait — ToolCallStart broker write.
  crates/ox-cli/src/policy_check.rs:75       (b)  set_inbox_state — called transitively from PolicyCheck::check (sync trait) via handle_ask.
  crates/ox-cli/src/policy_check.rs:97       (b)  PolicyCheck::check sync trait — approval request write.
```

### Invariants after landing

- `write_save_result_to_inbox` is `async fn`; tests invoke with `.await`, no `spawn_blocking`.
- Every `block_on` remaining in the three crates is categorized in the appendix with a concrete justification (not "sync context").

---

## Task 3 — Kill the prefilter in `is_message_entry`, compiler-enforced exhaustive test

**Advances: Properties 5, 10.**

### The change

Replace:

```rust
pub(crate) fn is_message_entry(msg: &serde_json::Value) -> bool {
    // cheap prefilter on msg["type"] then typed confirm
}
```

with:

```rust
pub(crate) fn is_message_entry(msg: &serde_json::Value) -> bool {
    use ox_kernel::log::LogEntry;
    matches!(
        serde_json::from_value::<LogEntry>(msg.clone()),
        Ok(LogEntry::User { .. }) | Ok(LogEntry::Assistant { .. })
    )
}
```

### Compiler-enforced exhaustivity test

The agreement test uses an `expected_is_message` helper that is an **explicit exhaustive match** — adding a new `LogEntry` variant breaks the compilation of the test until the author updates both the match and the production code.

```rust
// In snapshot::tests:
fn expected_is_message(entry: &ox_kernel::log::LogEntry) -> bool {
    use ox_kernel::log::LogEntry;
    // Exhaustive. A new variant added to LogEntry will not compile
    // here until the author decides whether it's a "message".
    match entry {
        LogEntry::User { .. } | LogEntry::Assistant { .. } => true,
        LogEntry::ToolCall { .. }
        | LogEntry::ToolResult { .. }
        | LogEntry::Meta { .. }
        | LogEntry::TurnStart { .. }
        | LogEntry::TurnEnd { .. }
        | LogEntry::CompletionEnd { .. }
        | LogEntry::ApprovalRequested { .. }
        | LogEntry::ApprovalResolved { .. }
        | LogEntry::Error { .. } => false,
    }
}

#[test]
fn is_message_entry_matches_exhaustive_expectation() {
    use ox_kernel::log::LogEntry;
    let samples: Vec<LogEntry> = vec![
        LogEntry::User { content: "x".into(), scope: None },
        LogEntry::Assistant {
            content: vec![], source: None, scope: None, completion_id: 0,
        },
        LogEntry::ToolCall {
            id: "1".into(), name: "t".into(), input: serde_json::json!({}), scope: None,
        },
        LogEntry::ToolResult {
            id: "1".into(), output: serde_json::json!({}), is_error: false, scope: None,
        },
        LogEntry::Meta { data: serde_json::json!({}) },
        LogEntry::TurnStart { scope: None },
        LogEntry::TurnEnd {
            scope: None, model: None,
            input_tokens: 0, output_tokens: 0,
            cache_creation_input_tokens: 0, cache_read_input_tokens: 0,
        },
        LogEntry::CompletionEnd {
            scope: "s".into(), model: "m".into(), completion_id: 0,
            input_tokens: 0, output_tokens: 0,
            cache_creation_input_tokens: 0, cache_read_input_tokens: 0,
        },
        LogEntry::ApprovalRequested { tool_name: "t".into(), input_preview: "".into() },
        LogEntry::ApprovalResolved {
            tool_name: "t".into(), decision: ox_types::Decision::AllowOnce,
        },
        LogEntry::Error { message: "x".into(), scope: None },
    ];
    for entry in &samples {
        let json = serde_json::to_value(entry).unwrap();
        assert_eq!(
            is_message_entry(&json),
            expected_is_message(entry),
            "disagreement on variant: {entry:?}",
        );
    }
}
```

A new `LogEntry` variant breaks the `samples` list and the `expected_is_message` match — author must decide in both places.

### Tasks

- [x] **3.1** Rewrite `is_message_entry` to the typed-only form. Delete the prefilter, its cost comment, and any tests that exercised only the prefilter.
- [x] **3.2** Add the `expected_is_message` helper and `is_message_entry_matches_exhaustive_expectation` test in `ox-inbox::snapshot::tests`.

### Invariants after landing

- `is_message_entry` has one code path.
- Adding a new `LogEntry` variant fails compilation in `expected_is_message` and the `samples` list — both must be updated, forcing a decision.

---

## Task 4 — Mechanism tests (mutate-between-calls)

**Advances: Properties 2, 4.**

### Helpers (define in `broker_setup::tests` module)

```rust
// LogEntry constructors (pure, for readability in test sketches).
fn user(text: &str) -> ox_kernel::log::LogEntry {
    ox_kernel::log::LogEntry::User { content: text.into(), scope: None }
}
fn assistant(text: &str) -> ox_kernel::log::LogEntry {
    ox_kernel::log::LogEntry::Assistant {
        content: vec![ox_kernel::ContentBlock::Text { text: text.into() }],
        source: None, scope: None, completion_id: 0,
    }
}

// Broker-driving helpers (async).
async fn create_thread(client: &ox_broker::ClientHandle, title: &str) -> String;
async fn append_log_message(
    client: &ox_broker::ClientHandle, tid: &str, entry: ox_kernel::log::LogEntry,
);
async fn fetch_log_count(client: &ox_broker::ClientHandle, tid: &str) -> i64;
async fn save_and_write_through(client: &ox_broker::ClientHandle, tid: &str);
async fn fetch_inbox_row(client: &ox_broker::ClientHandle, tid: &str) -> crate::parse::InboxThread;
fn dialog_with_info_open() -> DialogState;

/// Seed two threads such that: with chip `"aaa"` active, row 0 is
/// `tid_a`; with chip cleared, row 0 is `tid_b`. Achieved by naming
/// the threads so the substring filter matches `tid_a` only, and by
/// sleeping 10ms between creations so `updated_at` ordering puts
/// `tid_b` above `tid_a` in the default listing.
async fn arrange_filtered_first_row_differs_from_default(
    client: &ox_broker::ClientHandle,
) -> (String /* tid_a */, String /* tid_b */);
async fn clear_all_chips(client: &ox_broker::ClientHandle);
```

### Tests

**Test A — live per-entry update.** Append log entry between refreshes; second refresh sees new count *without* waiting for a save. Specifically does NOT call `save_and_write_through` between the refreshes — proves the live signal is `log/count`, not SQLite `last_seq`.

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cache_updates_per_log_entry_not_per_save() {
    let handle = test_setup().await;
    let client = handle.client();

    let tid = create_thread(&client, "t_live").await;
    append_log_message(&client, &tid, user("first")).await;
    append_log_message(&client, &tid, assistant("hi")).await;
    // Intentionally NOT calling save — SQLite last_seq stays at initial.

    let mut dialog = dialog_with_info_open();
    refresh_thread_info_cache(&client, &mut dialog).await;
    let entry = dialog.thread_info.as_ref().unwrap();
    assert_eq!(entry.info.stats.message_count, 2);
    let count_1 = entry.log_count_at_cache;

    append_log_message(&client, &tid, user("third")).await;
    // Again: no save. A cache keyed on SQLite last_seq would miss this.

    refresh_thread_info_cache(&client, &mut dialog).await;
    let entry = dialog.thread_info.as_ref().unwrap();
    assert!(entry.log_count_at_cache > count_1);
    assert_eq!(
        entry.info.stats.message_count, 3,
        "cache must reflect the new log entry without a save boundary",
    );
}
```

**Test B — hit short-circuit on unchanged state.** Two refreshes, no mutation. Same thread id, same `log_count` cached. Prove by asserting a fresh `log/count` read returns the same value as cached.

**Test C — selection change invalidates.** Create two threads with different message counts, refresh, move selection via `SelectNext`, refresh; assert thread_id changed.

**Test D — search-state aliasing regression.** Use `arrange_filtered_first_row_differs_from_default`. Open with chip active → thread A cached. Clear chips → selected_row is still 0 but row 0 is now thread B. Refresh; assert cached id ≠ thread A.

### Tasks

- [x] **4.1** Implement the six helpers above. `arrange_filtered_first_row_differs_from_default` uses concrete title-based filtering — the docstring and impl make the strategy deterministic (not "depends on sort order").
- [x] **4.2** Write Tests A / B / C / D per the sketches.
- [x] **4.3** **Delete** the existing outcome-matching cache tests in `broker_setup::tests`:
  - `refresh_thread_info_cache_noop_when_modal_closed` (padding — proves only the early return).
  - `refresh_thread_info_cache_populates_on_open_and_short_circuits_on_hit` (outcome-matching — passes whether or not we short-circuit).
  - The mechanism tests above subsume both.

### Invariants after landing

- Every cache behavior test mutates underlying state between observations.
- No cache test can pass on an implementation that silently re-fetches on every call.

---

## Task 5 — Discoverability and cross-screen consistency

**Advances: Property 6.**

### Discoverability test

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn i_appears_in_inbox_shortcut_bindings() {
    let handle = test_setup().await;
    let client = handle.client();

    let bindings = client.read(&path!("input/bindings/normal/inbox"))
        .await.unwrap().unwrap();
    let arr = match bindings.as_value().unwrap() {
        Value::Array(a) => a,
        _ => panic!("expected array"),
    };
    let has_i = arr.iter().any(|v| match v {
        Value::Map(m) => matches!(m.get("key"), Some(Value::String(s)) if s == "i"),
        _ => false,
    });
    assert!(has_i, "i must be bound on Normal+Inbox and visible to the shortcuts modal");
}
```

Same assertion for `Thread` screen after Task 5.2.

### Thread-screen binding

Add to `bindings.rs::normal_mode`:

```rust
out.push(bind_screen(
    Normal, &key(Char('i')), Thread,
    invoke(Cmd::ToggleThreadInfo),
    "Thread info",
));
```

### Tasks

- [x] **5.1** Add `i_appears_in_inbox_shortcut_bindings` and `i_appears_in_thread_shortcut_bindings`.
- [x] **5.2** Add the Thread-screen binding.
- [x] **5.3** Task 1.2 already handles Thread screen in `refresh_thread_info_cache`; verify the flow end-to-end with a new test `i_on_thread_screen_shows_current_thread_info` that opens a thread and asserts the cache populates for that thread's id.

### Invariants after landing

- `i` is discoverable on Inbox and Thread screens.
- Opening the info modal on a thread screen shows info for the currently-viewed thread.

---

## Task 6 — Observability at every seam

**Advances: Property 7.**

### The change

Add `tracing::debug!` (cache hit/miss with duration, modal open/close) and `tracing::warn!` (fetch errors). Target: `"thread_info"` — matches no existing target in the codebase; declared here as the convention for this feature.

```rust
// refresh_thread_info_cache hit:
tracing::debug!(
    target: "thread_info",
    thread_id = %id, log_count,
    "cache hit",
);

// refresh_thread_info_cache miss:
let start = std::time::Instant::now();
let info = fetch_thread_info(client, &row).await;
tracing::debug!(
    target: "thread_info",
    thread_id = %id, log_count,
    duration_us = start.elapsed().as_micros() as u64,
    "cache miss — fetched",
);

// fetch_thread_info read failures (log, turn, row):
tracing::warn!(
    target: "thread_info",
    thread_id = %id, error = %e,
    "read failed; modal will show partial or loading state",
);

// action_executor modal lifecycle:
tracing::debug!(
    target: "thread_info",
    open = dialog.show_thread_info,
    "modal toggled",
);
```

### Tasks

- [x] **6.1** Add the `debug!`/`warn!` calls listed above.
- [x] **6.2** **Verify with a documented manual run.** Running `RUST_LOG=thread_info=debug cargo test -p ox-cli cache_updates_per_log_entry_not_per_save -- --nocapture` must produce a readable narrative of miss → hit. Paste the expected trace output as a comment above Test A so the convention is pinned in source.

### Invariants after landing

- Every broker-failure path in `fetch_thread_info` emits a `tracing::warn!` with `thread_id` + error.
- `cache hit`, `cache miss — fetched`, and `modal toggled` events appear during a typical session.
- No test asserts on log output (tests verify behavior; tracing is for operators).

---

## Task 7 — Performance floor via linearity ratio

**Advances: Property 8.**

### The change

Instead of asserting an absolute wall-clock ceiling (flaky on loaded CI), assert that `fetch_thread_info` scales roughly linearly: the 10k-entry case must take no more than 15× the 1k-entry case. This catches quadratic regressions across any runner speed.

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_thread_info_scales_linearly_with_log_size() {
    use ox_kernel::log::LogEntry;
    use ox_kernel::ContentBlock;

    let handle = test_setup().await;
    let client = handle.client();

    async fn time_fetch_for_size(
        client: &ox_broker::ClientHandle, n: usize, tag: &str,
    ) -> std::time::Duration {
        let tid = create_thread(client, tag).await;
        for i in 0..(n / 2) {
            append_log_message(client, &tid,
                LogEntry::User { content: format!("u{i}"), scope: None }).await;
            append_log_message(client, &tid, LogEntry::Assistant {
                content: vec![ContentBlock::Text { text: format!("a{i}").into() }],
                source: None, scope: None, completion_id: i as u64,
            }).await;
        }
        let row = fetch_inbox_row(client, &tid).await;
        // Take the minimum of 3 runs for noise resistance.
        let mut best = std::time::Duration::from_secs(999);
        for _ in 0..3 {
            let t0 = std::time::Instant::now();
            let info = crate::view_state::fetch_thread_info(client, &row).await;
            best = best.min(t0.elapsed());
            assert_eq!(info.stats.message_count, n);
        }
        best
    }

    let t_1k = time_fetch_for_size(&client, 1_000, "t_1k").await;
    let t_10k = time_fetch_for_size(&client, 10_000, "t_10k").await;

    let ratio = t_10k.as_secs_f64() / t_1k.as_secs_f64().max(1e-6);
    assert!(
        ratio < 15.0,
        "10× input → {ratio:.1}× time (t_1k={t_1k:?}, t_10k={t_10k:?}); \
         S-tier allows up to ~10× linear + headroom for constants. \
         A quadratic regression would produce ratio near 100.",
    );

    // Backstop: if 10k takes longer than 2s even with ratio OK,
    // something fundamental broke.
    assert!(
        t_10k.as_secs() < 2,
        "t_10k={t_10k:?} exceeds sanity backstop",
    );
}
```

### Tasks

- [x] **7.1** Add `fetch_inbox_row` helper (reads `inbox/threads`, finds by id).
- [x] **7.2** Add the test above.
- [x] **7.3** Confirm the test passes locally. If it fails: profile, fix, or raise the ratio with a commit-message justification naming the new measured ratio.

### Invariants after landing

- Any change that makes `fetch_thread_info` quadratic fails CI with a specific ratio number.
- The test's absolute backstop (`< 2s`) catches catastrophic regressions too.

---

## Task 8 — Error-path UX and loading state

**Advances: Property 9.**

### The change

The modal must distinguish three states: *loading* (modal open, cache not yet populated this session), *ready* (cache populated OK), *failed* (fetch errored — tracing::warn was emitted, cache stays empty).

Simplest representation: keep `dialog.thread_info: Option<ThreadInfoEntry>`. When `show_thread_info == true && thread_info.is_none()`, the renderer shows "Loading info…". Post-fetch, cache is either populated (Ready) or stays None (Failed — tracing::warn recorded the reason).

We do not add a discriminated `ThreadInfoState::{Loading, Ready, Failed}` enum because:

- The "Failed" state is indistinguishable from "Loading" for the user — both render as "not yet shown." The actionable signal is in tracing for operators.
- A failed fetch on one frame is always followed by another refresh attempt on the next frame, because `refresh_thread_info_cache` runs each tick. Transient failures self-heal.
- Persistent failures (thread deleted) manifest as "Loading…" that never resolves, which matches the actual state: nothing to show. The user dismisses with Esc.

If evidence later emerges that users confuse "loading" with "failed," we add the enum. Until then, the tracing::warn is the operator-visible signal and the modal is visibly unresolved — a more honest UX than falsely-successful zero counts.

### Tasks

- [x] **8.1** In `tui.rs::draw_thread_info_modal`, handle `info: Option<&ThreadInfo>` instead of `&ThreadInfo`. When `None`, render a centered "Loading info…" placeholder with the same dismiss hint.
- [x] **8.2** Update `tui.rs` callsite to pass `dialog.thread_info.as_ref().map(|e| &e.info)`.
- [x] **8.3** Add test `modal_shows_loading_placeholder_before_first_fetch`: set `show_thread_info = true`, `thread_info = None`; render to a test buffer (ratatui supports this via `TestBackend`); assert the buffer contains "Loading info".
- [x] **8.4** Add test `fetch_failure_keeps_modal_in_loading_state_and_warns`: construct a thread id that won't resolve (`t_nonexistent`), drive `refresh_thread_info_cache`, assert `dialog.thread_info.is_none()` and `tracing::warn!` was emitted (via a `tracing` test subscriber captured into a `Vec<String>`).

### Invariants after landing

- Opening the modal on an unloaded cache renders a "Loading" placeholder — the user never sees a blank modal with silently-zero stats.
- Every fetch failure in `fetch_thread_info` emits `tracing::warn!` with thread id + cause.
- A persistent failure (thread deleted mid-open) manifests as perpetual "Loading" — dismissable with Esc, no crash.

---

## Rollout

Two atomic commits on a single PR:

- **Unit I — Core redesign.** Tasks 1, 2, 3, 8.1, 8.2. Cache key change, async conversion, prefilter removal, renderer accepts `Option<&ThreadInfo>`.
- **Unit II — Coverage, observability, perf, error tests.** Tasks 4, 5, 6, 7, 8.3, 8.4. All new tests + tracing + Thread-screen binding.

**Merge gate:** both units land, Task 2.4 audit appendix is filled, all tests pass, clippy is clean.

### Rollback

If Unit I lands and a regression surfaces in production (cache thrashing, stats disagreement, log/count returning unexpected values): revert the Unit I commit. The pre-plan code remains functional with known defects — reverting does not lose user data (cache is in-memory; no on-disk schema change). If Unit II alone regresses (test flake, spurious tracing, perf-gate false positive): revert Unit II; Unit I continues to work. Write-through to `inbox.db` is unaffected by either revert.

---

## Success definition

The plan is complete when *all* hold:

1. **All ten design properties are demonstrably upheld by the listed tests and audits:**
   - Property 1 (identity keys) — Test D passes.
   - Property 2 (live per-entry) — Test A passes *without* a save boundary between refreshes.
   - Property 3 (async honest) — Task 2.4 appendix filled; `spawn_blocking` removed from `write_save_result_to_inbox` test.
   - Property 4 (mechanism tests) — Tasks 4.3 deletion done; new tests all mutate.
   - Property 5 (exhaustive enum) — Task 3.2 test passes; removing a variant from `expected_is_message` fails compilation (locally verified).
   - Property 6 (discoverable) — Task 5.1 tests on both screens.
   - Property 7 (tracing) — Task 6.2 manual run output documented in source.
   - Property 8 (perf pinned) — Task 7 ratio test passes.
   - Property 9 (error UX) — Task 8.3, 8.4 tests pass; `tracing::warn!` in every failure branch.
   - Property 10 (no deferrals) — no "not in scope," "maybe later," or soft-triggered deferral remains in this document.
2. `cargo test --workspace` passes.
3. `cargo clippy --workspace --tests --all-targets` is clean.
4. The `block_on` audit appendix contains one categorized line per grep hit, or `no hits found`.
5. The PR description contains: the user-impact statement from the top of this plan, a link to this file, and the filled audit appendix.

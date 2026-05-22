# Crash Harness

Infrastructure for headless crash-and-remount tests of `ox-cli`. Built for the
durable-conversation-state plan (`docs/superpowers/plans/2026-04-19-…`) but
intentionally general-purpose: any test that wants to prove "the user sees the
same state after restart" can drive it.

**No terminal emulation.** The UI is a deterministic projection of the
`SharedLog` (`ox-kernel/src/log.rs`). If the `SharedLog` round-trips across a
crash, the UI does too by construction. Every assertion in this harness reads
one of two layers:

- **Ledger bytes** — `ox-inbox/src/ledger.rs` format, read directly off disk.
- **`SharedLog` snapshot** — reconstructed via the broker after a remount.

Nothing else.

## One mode: in-process soft crash

`Harness::soft_crash()` drops the `App`; worker channels close;
`Harness::remount_app()` constructs a fresh `App` against the same temp dir.
The ledger file and `SharedLog` are the entire surface under test, so drop +
remount is enough to cover the plan's correctness invariants.

Subprocess-and-SIGKILL crash modes are intentionally out of scope. When a
scenario would otherwise need a signal, route the failure through a
`LedgerWriter` test hook (`OX_TEST_FREEZE_AT`, below) so the break happens
deterministically inside the process.

## Writing a new scenario

```rust
mod crash_harness;

use crash_harness::{HarnessBuilder, read_shared_log};
use ox_cli::test_support::FakeTransport;
use ox_kernel::log::LogEntry;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn my_crash_scenario() {
    let transport = FakeTransport::new();
    transport.push_turn(/* scripted StreamEvents … */);
    let mut harness = HarnessBuilder::new().with_transport(transport).build().await;

    // 1. Drive some work.
    let tid = crash_harness::create_thread(&harness.client(), "t").await;
    crash_harness::append_log_entry(
        &harness.client(),
        &tid,
        LogEntry::User { content: "hi".into(), scope: None },
    )
    .await;

    // 2. Snapshot pre-crash state.
    let pre_kill = harness.snapshot_shared_log(&tid).await;

    // 3. Crash.
    harness.soft_crash();

    // 4. Remount and compare.
    harness.remount_app().await;
    let post = read_shared_log(&harness.client(), &tid).await;
    crash_harness::assert_shared_log_matches_pre_kill(&post, &pre_kill);
}
```

## Freeze-point protocol (Step 5, wired by Task 1a)

One environment variable lets tests park the `LedgerWriter` commit loop at
precise points:

- `OX_TEST_FREEZE_AT=<point>` — `before_write`, `after_write_before_sync`,
  `after_sync`. The writer blocks on a test-only channel at the named point
  until the test allows it to continue. Used to simulate torn-tail and
  post-fsync-loss conditions without killing the process.

The variable is defined as a `pub const` in `mod.rs` so every scenario
references the same name.

## What's *not* tested here

- Rendering. `ratatui` is correct-by-construction from the log; if we break
  rendering, the snapshot tests under `crates/ox-cli/src/editor_snapshots.rs`
  catch it.
- Kernel logic. That's tested in `ox-kernel/tests/` against hand-crafted
  inputs.
- Broker dispatch. Tested in `ox-broker/`.

The harness is exclusively about **durability**.

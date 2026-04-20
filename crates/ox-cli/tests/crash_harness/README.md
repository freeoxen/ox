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

## Two modes

| Mode | When to use | Mechanism |
|------|-------------|-----------|
| **In-process "soft crash"** | Default. Anything that doesn't actively need OS-level signal semantics. | `Harness::soft_crash()` drops the `App`; worker channels close; `Harness::remount_app()` constructs a fresh `App` against the same temp dir. |
| **Subprocess `SIGKILL`** | Tests that need fsync-after-kill, kill mid-sync-syscall, or otherwise exercise the OS boundary. | `SubprocessHarness::spawn()` runs the real `ox` binary with `HOME` pointed at a temp dir; the test signals it and remounts in-process for assertions. |

Prefer in-process. It's 10× faster, fully deterministic, and catches ~95% of
the bugs because the ledger file and `SharedLog` are the entire surface.
Reach for subprocess only when the scenario explicitly involves a syscall.

## Writing a new scenario (in-process)

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

## Writing a new scenario (subprocess)

```rust
use crash_harness::SubprocessHarness;

#[test]
fn kill_subprocess_exits_with_signal_9() {
    let h = SubprocessHarness::new();
    let mut child = h.spawn();
    // let the child settle — replace with a state-signaling mechanism when
    // there's something to wait for.
    std::thread::sleep(std::time::Duration::from_millis(200));
    #[cfg(unix)]
    unsafe {
        libc::kill(child.id() as i32, libc::SIGKILL);
    }
    let status = child.wait().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(9));
    }
}
```

## Freeze-point protocol (Step 5, wired by Task 1a)

Two environment variables let tests park the subprocess at precise points in
the `LedgerWriter` commit loop:

- `OX_TEST_FREEZE_AT=<point>` — `before_write`, `after_write_before_sync`,
  `after_sync`. The writer blocks on a test-only channel at the named point
  until the process is killed.
- `OX_TEST_FAKE_TRANSPORT_SCRIPT=<path>` — reserved for subprocess scripted
  turns. Today's harness does not honor it (the `LedgerWriter` doesn't exist
  yet); the name is fixed here so Task 1a doesn't have to re-negotiate.

Both variables are defined as `pub const` strings in `mod.rs` so every scenario
references the same name.

## What's *not* tested here

- Rendering. `ratatui` is correct-by-construction from the log; if we break
  rendering, the snapshot tests under `crates/ox-cli/src/editor_snapshots.rs`
  catch it.
- Kernel logic. That's tested in `ox-kernel/tests/` against hand-crafted
  inputs.
- Broker dispatch. Tested in `ox-broker/`.

The harness is exclusively about **durability**.

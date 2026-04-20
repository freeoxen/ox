//! Smoke tests for the crash harness.
//!
//! These exist to prove Task 0's infrastructure actually works. They are not
//! the plan's correctness scenarios — those come in Task 1+. Each test here
//! corresponds to a Task 0 step in the plan.

mod crash_harness;

use crash_harness::{
    HarnessBuilder, SubprocessHarness, append_log_entry, assert_shared_log_matches_pre_kill,
    create_thread, init_tracing, read_shared_log,
};
use ox_kernel::log::LogEntry;

// ---------------------------------------------------------------------------
// Step 1 / Step 3 — in-process soft-crash + remount.
// ---------------------------------------------------------------------------

/// Build a harness, drive a simple turn's worth of log writes, drop the App,
/// remount, and verify the SharedLog projection is identical. This is the
/// core guarantee Task 0 exists to support.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn soft_crash_roundtrip_preserves_log() {
    init_tracing();

    let mut harness = HarnessBuilder::new().build().await;
    let client = harness.client();

    let tid = create_thread(&client, "t-roundtrip").await;

    append_log_entry(
        &client,
        &tid,
        LogEntry::User {
            content: "hello".into(),
            scope: None,
        },
    )
    .await;
    append_log_entry(
        &client,
        &tid,
        LogEntry::Assistant {
            content: vec![ox_kernel::ContentBlock::Text { text: "hi".into() }],
            source: None,
            scope: None,
            completion_id: 0,
        },
    )
    .await;

    // Force a ledger save (pre-Task-1a there is no per-append durability, so
    // the harness must drive a save boundary explicitly — same mechanism the
    // production binary uses today). We reach through the broker rather than
    // call `save_thread_state` directly to keep the test at the public seam.
    //
    // The production call site uses `save_thread_state` in `agents.rs`; we
    // mirror its effect by asking `ox_inbox::snapshot::save` to run against
    // the thread's namespace. Once Task 1a lands, this save boundary stops
    // being necessary for this assertion.
    crash_harness_force_save(&harness, &tid).await;

    let pre_kill = harness.snapshot_shared_log(&tid).await;
    assert_eq!(
        pre_kill.len(),
        2,
        "pre-kill log must have the 2 entries we wrote"
    );

    harness.soft_crash();
    harness.remount_app().await;

    let post = read_shared_log(&harness.client(), &tid).await;
    assert_shared_log_matches_pre_kill(&post, &pre_kill);
}

// Ad-hoc helper that force-saves the thread by invoking the existing save
// path directly. This is pre-Task-1a scaffolding: Task 1a makes per-append
// durability automatic, after which the save_thread_state call goes away and
// this helper is deleted.
async fn crash_harness_force_save(harness: &crash_harness::Harness, thread_id: &str) {
    // Read the thread's full log through the broker, then call
    // `ox_inbox::snapshot::save` against a temporarily-assembled namespace
    // seeded with those entries. This keeps the harness's "force a save"
    // semantics grounded in the same code path production uses, rather than
    // hand-writing the ledger JSONL.
    use ox_context::{Namespace, SystemProvider};
    use ox_gate::GateStore;
    use ox_history::HistoryView;
    use ox_inbox::snapshot::{PARTICIPATING_MOUNTS, save};
    use ox_kernel::log::{LogStore, SharedLog};
    use structfs_core_store::{Record, Writer};
    use structfs_serde_store::json_to_value;

    let entries = read_shared_log(&harness.client(), thread_id).await;

    let shared = SharedLog::new();
    let mut ns = Namespace::new();
    ns.mount(
        "system",
        Box::new(SystemProvider::new("You are helpful.".into())),
    );
    ns.mount("tools", Box::new(ox_tools::ToolStore::empty()));
    ns.mount("history", Box::new(HistoryView::new(shared.clone())));
    ns.mount("log", Box::new(LogStore::from_shared(shared)));
    ns.mount("gate", Box::new(GateStore::new()));

    for entry in &entries {
        let val = json_to_value(serde_json::to_value(entry).unwrap());
        ns.write(
            &structfs_core_store::path!("log/append"),
            Record::parsed(val),
        )
        .expect("seed replay log");
    }

    let thread_dir = harness.thread_dir(thread_id);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    save(
        &mut ns,
        &thread_dir,
        thread_id,
        "t-roundtrip",
        &[],
        now,
        &PARTICIPATING_MOUNTS,
    )
    .expect("snapshot::save");
}

// ---------------------------------------------------------------------------
// Step 2 — SharedLog snapshot mechanism ordering.
// ---------------------------------------------------------------------------

/// Snapshot before and after an append; verify order is preserved.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_preserves_append_order() {
    init_tracing();
    let harness = HarnessBuilder::new().build().await;
    let client = harness.client();
    let tid = create_thread(&client, "t-order").await;

    let before = harness.snapshot_shared_log(&tid).await;
    assert!(before.is_empty());

    append_log_entry(
        &client,
        &tid,
        LogEntry::User {
            content: "first".into(),
            scope: None,
        },
    )
    .await;
    append_log_entry(
        &client,
        &tid,
        LogEntry::User {
            content: "second".into(),
            scope: None,
        },
    )
    .await;

    let after = harness.snapshot_shared_log(&tid).await;
    assert_eq!(after.len(), 2);
    match &after[0] {
        LogEntry::User { content, .. } => assert_eq!(content, "first"),
        other => panic!("expected first user entry, got {other:?}"),
    }
    match &after[1] {
        LogEntry::User { content, .. } => assert_eq!(content, "second"),
        other => panic!("expected second user entry, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Step 2.5 — App::drop audit.
//
// The plan's audit is static (grep for Drop, inspect field types). The runtime
// check is: after soft_crash, the temp dir contains only expected state — no
// lock files, no zombie threads. We can't directly inspect zombies, but we can
// confirm the file system is stable and a fresh App can remount.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn app_drop_leaves_no_stray_lockfiles() {
    init_tracing();
    let mut harness = HarnessBuilder::new().build().await;
    let client = harness.client();
    let tid = create_thread(&client, "t-drop").await;
    append_log_entry(
        &client,
        &tid,
        LogEntry::User {
            content: "x".into(),
            scope: None,
        },
    )
    .await;

    harness.soft_crash();

    // Scan inbox root for anything a stray lockfile would look like.
    // The production writers don't lock anything today, so this test's value
    // is in catching *future* regressions introduced by Task 1a's
    // `LedgerWriter`: if it forgets to clean up a sidecar on drop, this test
    // fails and we know.
    let bad: Vec<_> = walk_files(harness.inbox_root())
        .into_iter()
        .filter(|p| {
            let n = p.file_name().unwrap_or_default().to_string_lossy();
            n.ends_with(".lock") || n.ends_with(".tmp") || n.contains("~pid")
        })
        .collect();
    assert!(bad.is_empty(), "stray files after App drop: {bad:?}");

    // A fresh remount must succeed — `App::drop` must not leave a fd or
    // mutex held that would block re-opening the same inbox.
    harness.remount_app().await;
    // The post-remount log may be empty: pre-Task-1a there is no automatic
    // durability for in-memory appends, so whether this thread's log
    // survives depends on whether a save boundary was hit before drop.
    // The *remount itself succeeding* is the invariant under test here.
    //
    // The `soft_crash_roundtrip_preserves_log` test uses `force_save` to
    // validate the full data-survives-drop path.
}

fn walk_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Step 4 — subprocess spawn + SIGKILL + exit code.
//
// The full flow needs a headless mode for the `ox` binary (ratatui's terminal
// init panics under `stdin=null`, `stdout=null`). That's a separate change —
// a later task adds `OX_HEADLESS=1` to the binary. Until then, this smoke
// test exercises just the kill mechanism against a long-lived stand-in
// process, which is enough to prove the crash harness's SIGKILL+wait
// machinery is correct. The real `ox`-binary variant is `#[ignore]`-gated
// and can be enabled when `OX_HEADLESS` lands.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn subprocess_sigkill_exits_with_signal_9() {
    // Smoke-test the harness's spawn + `kill(2)` + `wait` sequence. Uses
    // `/bin/sleep` with a short deadline as a self-terminating target: if
    // the signal is delivered, `wait` returns immediately with signal 9; if
    // the sandbox blocks `kill`, `wait` returns the natural-exit status after
    // the sleep elapses, and we skip. Either way the test finishes in
    // bounded time without a polling sleep in the test body itself.
    //
    // The short duration is a backstop for the sandboxed case; the real
    // assertion is about the kill path. When the ox binary grows an
    // `OX_HEADLESS` mode, this test's `#[ignore]`d sibling can use a
    // signal-file to start the kill the instant the target is ready.
    let spawn_result = std::process::Command::new("/bin/sleep")
        .arg("2")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = match spawn_result {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipping subprocess SIGKILL test: spawn /bin/sleep failed: {e}");
            return;
        }
    };
    let pid = child.id();

    let rc = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    if rc != 0 {
        // ESRCH — child already exited (sandboxed exec failure).
        // EPERM — sandbox blocked the signal.
        // Either means we can't exercise the real mechanism here; skip.
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(code) if code == libc::ESRCH || code == libc::EPERM => {
                eprintln!(
                    "skipping subprocess SIGKILL test: kill returned errno {code} ({err}) \
                     — sandbox restriction"
                );
                let _ = child.wait();
                return;
            }
            _ => panic!("kill(2) returned {rc} (errno: {err})"),
        }
    }

    let status = child.wait().expect("wait for child");
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(
        status.signal(),
        Some(9),
        "child did not exit from SIGKILL (code={:?} signal={:?})",
        status.code(),
        status.signal(),
    );
}

#[cfg(unix)]
#[test]
#[ignore = "ox binary panics on TTY init under stdio=null; enable once OX_HEADLESS mode lands"]
fn subprocess_sigkill_on_real_ox_binary() {
    // Still `#[ignore]` because the ox binary needs a headless mode before it
    // can survive `stdio=null` long enough to be killed. When that mode lands
    // (a later task adds `OX_HEADLESS=1`), this test can wait on a
    // specific startup signal — e.g. the presence of a file under
    // `inbox_root` that the binary writes after broker setup — instead of a
    // sleep. Do not reintroduce wall-clock waits here.
    let bin = crash_harness::cargo_bin_path();
    if !bin.exists() {
        eprintln!(
            "skipping — {} does not exist; run `cargo build -p ox-cli` first",
            bin.display(),
        );
        return;
    }
    let h = SubprocessHarness::new();
    let mut child = h.spawn();
    // PLACEHOLDER: once OX_HEADLESS lands, wait on a ready-file here.
    let pid = child.id();
    unsafe {
        let rc = libc::kill(pid as i32, libc::SIGKILL);
        assert_eq!(rc, 0, "kill(2) returned {rc}");
    }
    let status = child.wait().expect("wait for child");
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(9));
}

// ---------------------------------------------------------------------------
// Step 6 — assertion-helper sanity.
// ---------------------------------------------------------------------------

#[test]
fn assert_no_dangling_turn_start_accepts_balanced_log() {
    let entries = vec![
        LogEntry::TurnStart { scope: None },
        LogEntry::TurnEnd {
            scope: None,
            model: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    ];
    crash_harness::assert_no_dangling_turn_start(&entries);
}

#[test]
#[should_panic(expected = "dangling TurnStart")]
fn assert_no_dangling_turn_start_rejects_open_start() {
    let entries = vec![LogEntry::TurnStart { scope: None }];
    crash_harness::assert_no_dangling_turn_start(&entries);
}

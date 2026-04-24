//! Task 4 Step 5 — durable-streaming crash-recovery E2E.
//!
//! Scenario: a turn starts a streaming completion. Partial text is
//! observed by the UI and persisted via `AssistantProgress` entries.
//! The process crashes before the stream finalizes (no `Assistant`
//! final, no `TurnEnd`). On remount:
//!
//! 1. The mount classifier walks the log tail (`TurnStart +
//!    AssistantProgress*`) and returns `InStreamNoFinal`.
//! 2. `ThreadNamespace::from_thread_dir` appends `TurnAborted {
//!    CrashDuringStream }` to the durable log via `LedgerWriter`.
//! 3. `HistoryView::reconstruct_turn_streaming` projects the latest
//!    unsuperseded `AssistantProgress.accumulated` into
//!    `turn/streaming`, so reading `history/messages` on the remounted
//!    thread shows the partial text the user was watching pre-crash.
//!
//! Crash mechanism: direct log injection. The plan asks for a "slow
//! streaming fake transport" but a real stall between transport events
//! would need either `tokio::time::sleep` (banned by the testing
//! posture) or a cross-thread barrier that adds scaffolding without
//! strengthening the assertion — the assertion is about log shape
//! post-remount, which is independent of the crash mechanism. The
//! injection pattern matches `crash_harness_approval_resume.rs` and
//! `crash_harness_post_crash_reconfirm.rs`. See the "Task 4 test
//! deviation" note in the plan document.

mod crash_harness;

use crash_harness::{
    HarnessBuilder, append_log_entry, create_thread, init_tracing, read_shared_log,
};
use ox_kernel::log::{LogEntry, TurnAbortReason};

/// Write a `TurnStart` and a monotonically-growing sequence of
/// `AssistantProgress` entries for `thread_id`, simulating a stream
/// that was interrupted mid-flight. The pre-crash log tail has no
/// `Assistant` final — the classifier is expected to return
/// `InStreamNoFinal`.
async fn inject_interrupted_stream(
    client: &ox_broker::ClientHandle,
    thread_id: &str,
    snapshots: &[&str],
    epoch: u64,
) {
    append_log_entry(client, thread_id, LogEntry::TurnStart { scope: None }).await;
    append_log_entry(
        client,
        thread_id,
        LogEntry::User {
            content: "tell me about rust".into(),
            scope: None,
        },
    )
    .await;
    for snap in snapshots {
        append_log_entry(
            client,
            thread_id,
            LogEntry::AssistantProgress {
                accumulated: (*snap).into(),
                epoch,
            },
        )
        .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_progress_tail_classifies_as_in_stream_and_appends_turn_aborted() {
    init_tracing();

    let mut harness = HarnessBuilder::new().build().await;
    let client = harness.client();

    let tid = create_thread(&client, "t-durable-stream").await;
    inject_interrupted_stream(
        &client,
        &tid,
        &[
            "Rust is a",
            "Rust is a systems",
            "Rust is a systems language",
        ],
        0,
    )
    .await;

    let pre_kill = harness.snapshot_shared_log(&tid).await;
    assert!(
        matches!(pre_kill.last(), Some(LogEntry::AssistantProgress { .. })),
        "pre-crash tail must be AssistantProgress; got {pre_kill:?}",
    );
    let pre_progress_count = pre_kill
        .iter()
        .filter(|e| matches!(e, LogEntry::AssistantProgress { .. }))
        .count();
    assert_eq!(
        pre_progress_count, 3,
        "expected three progress snapshots pre-crash; log={pre_kill:?}",
    );

    // Crash + remount. The mount lifecycle runs the classifier on the
    // restored log; an `InStreamNoFinal` classification triggers a
    // durable `TurnAborted { CrashDuringStream }` append BEFORE the
    // remount returns.
    harness.soft_crash();
    harness.remount_app().await;

    let client = harness.client();
    let post_log = read_shared_log(&client, &tid).await;

    // The partial-text progress entries are preserved verbatim across
    // the crash — replay loaded them from the durable ledger.
    let post_progress_count = post_log
        .iter()
        .filter(|e| matches!(e, LogEntry::AssistantProgress { .. }))
        .count();
    assert_eq!(
        post_progress_count, 3,
        "expected all three progress snapshots to survive the crash; log={post_log:?}",
    );

    // A single `TurnAborted { CrashDuringStream }` marker is appended
    // on mount AFTER the last progress entry.
    let aborted_positions: Vec<usize> = post_log
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            LogEntry::TurnAborted {
                reason: TurnAbortReason::CrashDuringStream,
            } => Some(i),
            _ => None,
        })
        .collect();
    assert_eq!(
        aborted_positions.len(),
        1,
        "expected exactly one TurnAborted(CrashDuringStream); log={post_log:?}",
    );
    let last_progress_pos = post_log
        .iter()
        .rposition(|e| matches!(e, LogEntry::AssistantProgress { .. }))
        .expect("progress preserved");
    assert!(
        aborted_positions[0] > last_progress_pos,
        "TurnAborted must follow the last AssistantProgress (post_log={post_log:?})",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_view_projects_latest_progress_into_turn_streaming_on_remount() {
    init_tracing();

    let mut harness = HarnessBuilder::new().build().await;
    let client = harness.client();

    let tid = create_thread(&client, "t-durable-stream-projection").await;
    // Craft a sequence of progress entries with the latest being the
    // longest visible accumulator. `reconstruct_turn_streaming` walks
    // tail-ward and picks the first unsuperseded progress.
    inject_interrupted_stream(
        &client,
        &tid,
        &[
            "The first sentence",
            "The first sentence. Starting the second",
        ],
        0,
    )
    .await;

    harness.soft_crash();
    harness.remount_app().await;

    let client = harness.client();

    // `history/messages` appends the projected partial assistant turn
    // when `turn.is_active()` — the streaming projection sets
    // `turn.thinking = true` so this path fires on a replayed thread.
    let tid_comp = ox_kernel::PathComponent::try_new(&tid).expect("valid thread id");
    let msgs_path = ox_path::oxpath!("threads", tid_comp, "history", "messages");
    let record = client
        .read(&msgs_path)
        .await
        .expect("read history/messages")
        .expect("messages record present");
    let value = record
        .as_value()
        .expect("history/messages returns parsed value")
        .clone();
    let json = structfs_serde_store::value_to_json(value);
    let arr = json.as_array().expect("messages is an array").clone();

    // Last entry must be an assistant partial carrying the latest
    // progress accumulator. Everything before it is the user prompt
    // we injected.
    let last = arr.last().expect("at least one projected message");
    assert_eq!(
        last["role"], "assistant",
        "expected assistant partial at tail; got {arr:?}",
    );
    let text = last["content"][0]["text"]
        .as_str()
        .expect("assistant partial carries text");
    assert_eq!(
        text, "The first sentence. Starting the second",
        "turn/streaming must reflect the latest unsuperseded progress snapshot",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_final_supersedes_trailing_progress_projection() {
    // If a completion finalized cleanly — `AssistantProgress` entries
    // followed by an `Assistant { completion_id, .. }` with matching
    // epoch — `reconstruct_turn_streaming` must NOT project the
    // progress; the turn is no longer in-flight.
    init_tracing();

    let mut harness = HarnessBuilder::new().build().await;
    let client = harness.client();

    let tid = create_thread(&client, "t-durable-stream-superseded").await;
    append_log_entry(&client, &tid, LogEntry::TurnStart { scope: None }).await;
    append_log_entry(
        &client,
        &tid,
        LogEntry::User {
            content: "hi".into(),
            scope: None,
        },
    )
    .await;
    append_log_entry(
        &client,
        &tid,
        LogEntry::AssistantProgress {
            accumulated: "Hell".into(),
            epoch: 0,
        },
    )
    .await;
    append_log_entry(
        &client,
        &tid,
        LogEntry::AssistantProgress {
            accumulated: "Hello".into(),
            epoch: 0,
        },
    )
    .await;
    append_log_entry(
        &client,
        &tid,
        LogEntry::Assistant {
            content: vec![ox_kernel::ContentBlock::Text {
                text: "Hello".into(),
            }],
            source: None,
            scope: None,
            completion_id: 0,
        },
    )
    .await;
    append_log_entry(
        &client,
        &tid,
        LogEntry::TurnEnd {
            scope: Some("root".into()),
            model: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    )
    .await;

    harness.soft_crash();
    harness.remount_app().await;

    let client = harness.client();
    // No TurnAborted should have been appended — classifier walks
    // back, sees `TurnEnd` first, returns `Idle`.
    let post_log = read_shared_log(&client, &tid).await;
    let aborted = post_log
        .iter()
        .filter(|e| matches!(e, LogEntry::TurnAborted { .. }))
        .count();
    assert_eq!(
        aborted, 0,
        "no TurnAborted expected on clean-finalized replay; log={post_log:?}",
    );

    // messages projection shows the completed Assistant turn, not the
    // progress text — `is_active()` stays false after clean replay.
    let tid_comp = ox_kernel::PathComponent::try_new(&tid).expect("valid thread id");
    let msgs_path = ox_path::oxpath!("threads", tid_comp, "history", "messages");
    let record = client
        .read(&msgs_path)
        .await
        .expect("read history/messages")
        .expect("messages record present");
    let value = record.as_value().expect("parsed").clone();
    let json = structfs_serde_store::value_to_json(value);
    let arr = json.as_array().expect("array").clone();
    // Expect user + assistant — no trailing partial.
    assert_eq!(arr.len(), 2, "expected user + assistant; got {arr:?}");
    assert_eq!(arr[0]["role"], "user");
    assert_eq!(arr[1]["role"], "assistant");
    assert_eq!(arr[1]["content"][0]["text"], "Hello");
}

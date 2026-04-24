//! Task 3d Step 5 — `AwaitingApproval` resumption E2E.
//!
//! Scenario: a turn issues a tool call that requires approval. The process
//! crashes while waiting for the user. On remount:
//!
//! 1. The mount classifier returns `AwaitingApproval`.
//! 2. `ThreadNamespace::from_thread_dir` writes `shell/resume_needed = true`.
//! 3. The agent worker observes the flag and drives a single `run_turn`
//!    so the kernel's resume prologue re-requests approval with
//!    `post_crash_reconfirm: true`.
//! 4. The test (standing in for the user) writes `approval/response`
//!    with `AllowOnce`.
//! 5. The tool dispatches; the model's next completion returns text;
//!    the turn ends cleanly.
//!
//! The key invariant: **the fake transport is called exactly once**.
//! The pre-crash completion already happened; resumption must not
//! trigger a second LLM round-trip.

mod crash_harness;

use std::time::Duration;

use crash_harness::{HarnessBuilder, append_log_entry, create_thread, init_tracing};
use ox_broker::ClientHandle;
use ox_cli::test_support::FakeTransport;
use ox_kernel::ContentBlock;
use ox_kernel::log::LogEntry;
use ox_types::ApprovalRequest;

/// Poll the broker for a pending approval request on `thread_id`, yielding
/// control to tokio between attempts. Bounded by `timeout`. Returns the
/// pending request as soon as it appears.
///
/// No wall-clock `sleep`: we `yield_now()` so the worker thread's
/// blocking `block_on` gets progress ticks from the runtime.
async fn wait_for_pending_approval(
    client: &ClientHandle,
    thread_id: &str,
    timeout: Duration,
) -> ApprovalRequest {
    let tid = ox_kernel::PathComponent::try_new(thread_id).expect("valid thread id");
    let pending_path = ox_path::oxpath!("threads", tid, "approval", "pending");
    tokio::time::timeout(timeout, async {
        loop {
            if let Ok(Some(record)) = client.read(&pending_path).await {
                if let Some(value) = record.as_value() {
                    // Null means "no pending" — keep polling.
                    if !matches!(value, structfs_core_store::Value::Null) {
                        let json = structfs_serde_store::value_to_json(value.clone());
                        if let Ok(req) = serde_json::from_value::<ApprovalRequest>(json) {
                            return req;
                        }
                    }
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("approval did not appear within timeout")
}

/// Poll the broker for a tail log-entry matching `predicate`. Returns
/// when `predicate` returns `true` for *any* entry in the snapshot. Used
/// to detect turn-end (the kernel writes `TurnEnd` after the last
/// completion).
async fn wait_for_log_entry<F>(
    client: &ClientHandle,
    thread_id: &str,
    timeout: Duration,
    predicate: F,
) where
    F: Fn(&LogEntry) -> bool,
{
    tokio::time::timeout(timeout, async {
        loop {
            let entries = crash_harness::read_shared_log(client, thread_id).await;
            if entries.iter().any(&predicate) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("expected log entry did not appear within timeout");
}

/// Write an `approval/response` through the broker, simulating a user
/// click. Mirrors the path/payload shape `ApprovalStore` expects.
async fn respond_to_approval(client: &ClientHandle, thread_id: &str, decision: ox_types::Decision) {
    let tid = ox_kernel::PathComponent::try_new(thread_id).expect("valid thread id");
    let resp_path = ox_path::oxpath!("threads", tid, "approval", "response");
    let response = ox_types::ApprovalResponse { decision };
    client
        .write_typed(&resp_path, &response)
        .await
        .expect("approval/response write");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn awaiting_approval_resume_completes_turn_without_second_llm_call() {
    init_tracing();

    // Transport: one scripted turn after resume emits text-only so the
    // turn ends cleanly. `fail_if_called_more_than(1)` is the key
    // assertion: resumption must NOT issue a second LLM round-trip.
    let transport = FakeTransport::new();
    transport.push_turn(vec![
        ox_kernel::StreamEvent::TextDelta("Done.".into()),
        ox_kernel::StreamEvent::MessageStop,
    ]);
    transport.fail_if_called_more_than(1);

    let mut harness = HarnessBuilder::new()
        .with_transport(transport.clone())
        .build()
        .await;
    let client = harness.client();

    // Hand-craft the pre-crash log tail: a completed User, then
    // Assistant(tool_use) + ToolCall + ApprovalRequested — the shape
    // the classifier reports as `AwaitingApproval`. Kernel tests use
    // the same approach (see `run_turn_resume_approval_allow_dispatches_tool`).
    let tid = create_thread(&client, "t-approval-resume").await;
    append_log_entry(&client, &tid, LogEntry::TurnStart { scope: None }).await;
    append_log_entry(
        &client,
        &tid,
        LogEntry::User {
            content: "list current dir".into(),
            scope: None,
        },
    )
    .await;
    append_log_entry(
        &client,
        &tid,
        LogEntry::Assistant {
            content: vec![ContentBlock::ToolUse(ox_kernel::ToolCall {
                id: "tu-resume-1".into(),
                name: "shell".into(),
                input: serde_json::json!({"command": "ls"}),
            })],
            source: None,
            scope: None,
            completion_id: 0,
        },
    )
    .await;
    append_log_entry(
        &client,
        &tid,
        LogEntry::ToolCall {
            id: "tu-resume-1".into(),
            name: "shell".into(),
            input: serde_json::json!({"command": "ls"}),
            scope: None,
        },
    )
    .await;
    append_log_entry(
        &client,
        &tid,
        LogEntry::ApprovalRequested {
            tool_name: "shell".into(),
            input_preview: "ls".into(),
            post_crash_reconfirm: false,
        },
    )
    .await;

    let pre_kill = harness.snapshot_shared_log(&tid).await;
    assert!(
        matches!(pre_kill.last(), Some(LogEntry::ApprovalRequested { .. })),
        "pre-crash tail must be ApprovalRequested; got {pre_kill:?}",
    );

    // Crash. The temp dir + ledger survive; the in-memory state is lost.
    harness.soft_crash();
    harness.remount_app().await;

    // Spawn a worker. The worker's first adapter write observes the
    // mount-written `shell/resume_needed = true` flag and drives one
    // `run_turn`; the kernel prologue detects the resume-approval
    // shape and writes a new `ApprovalRequested { post_crash_reconfirm: true }`.
    harness.app().pool.ensure_worker(&tid);

    let client = harness.client();
    let pending = wait_for_pending_approval(&client, &tid, Duration::from_secs(10)).await;
    assert_eq!(pending.tool_name, "shell");
    assert_eq!(
        pending.tool_input.get("command").and_then(|v| v.as_str()),
        Some("ls"),
        "prologue must reconstruct tool_input from the prior ToolCall, not from the \
         ApprovalRequested.input_preview display string (P10)",
    );

    // User approves. Kernel executes the tool, then issues a fresh
    // completion which returns text and ends the turn.
    respond_to_approval(&client, &tid, ox_types::Decision::AllowOnce).await;

    wait_for_log_entry(&client, &tid, Duration::from_secs(10), |e| {
        matches!(e, LogEntry::TurnEnd { .. })
    })
    .await;

    // The transport cap was 1. `call_count() == 1` proves the resume
    // prologue skipped the pre-crash completion and only the post-decision
    // completion happened.
    assert_eq!(
        transport.call_count(),
        1,
        "resumption must not trigger a second LLM round-trip (pre-crash completion \
         was already done)",
    );

    // A new ApprovalRequested with post_crash_reconfirm=true must be in
    // the log — the kernel's resume prologue wrote it via ThreadNamespace.
    let post_log = harness.snapshot_shared_log(&tid).await;
    let post_crash_count = post_log
        .iter()
        .filter(|e| {
            matches!(
                e,
                LogEntry::ApprovalRequested {
                    post_crash_reconfirm: true,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        post_crash_count, 1,
        "expected exactly one ApprovalRequested with post_crash_reconfirm=true; \
         log={post_log:?}",
    );

    // A matching ApprovalResolved(allow_once) and a TurnEnd must also be there.
    assert!(
        post_log.iter().any(|e| matches!(
            e,
            LogEntry::ApprovalResolved {
                decision: ox_types::Decision::AllowOnce,
                ..
            }
        )),
        "expected ApprovalResolved(AllowOnce); log={post_log:?}",
    );
    assert!(
        post_log
            .iter()
            .any(|e| matches!(e, LogEntry::TurnEnd { .. })),
        "expected TurnEnd; log={post_log:?}",
    );
}

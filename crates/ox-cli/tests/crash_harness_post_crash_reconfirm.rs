//! Task 3d Step 6 — post-crash re-confirm matrix.
//!
//! Scenario: a tool was mid-dispatch when the process exited. Its side
//! effect may have landed (counter = 1). On remount, the kernel's
//! resume prologue forces a re-confirmation modal — regardless of the
//! policy's auto-approve stance (Step 6d). The four sub-tests exercise
//! each user decision:
//!
//! - **6a Retry** (`AllowOnce`) — tool runs again, counter = 2.
//! - **6b Skip** (`DenyOnce`) — synthetic `ToolResult` with the plan's
//!   pinned Skip string; counter stays at 1.
//! - **6c Cancel** (`CancelTurn`) — `TurnAborted(UserCanceledAfterCrash)`
//!   is written; counter stays at 1; turn exits.
//! - **6d Auto-approved** — same as 6a, but the original dispatch would
//!   have been auto-approved by policy. The resume modal still appears
//!   because the re-confirm is a kernel-enforced invariant.
//!
//! The counter-backed tool is supplied via
//! [`ox_cli::test_support::ToolInjector`]; crash/remount spawns a fresh
//! worker that sees the same `Arc<AtomicU64>`.

mod crash_harness;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crash_harness::{
    append_log_entry, create_thread, init_tracing, respond_to_approval, wait_for_log_entry,
    wait_for_pending_approval,
};
use ox_broker::ClientHandle;
use ox_cli::app::App;
use ox_cli::broker_setup::{BrokerHandle, setup as broker_setup};
use ox_cli::test_support::{FakeTransport, ToolInjector, factory_for};
use ox_inbox::InboxStore;
use ox_kernel::ContentBlock;
use ox_kernel::log::LogEntry;
use ox_tools::native::FnTool;

// ---------------------------------------------------------------------------
// Local harness — a lightweight variant with tool-injector support.
//
// The shared `crash_harness::Harness` doesn't expose the `ToolInjector`
// hook; threading it through would bloat the Step 0 crash-harness surface.
// We re-implement just the remount lifecycle locally to keep that
// narrow.
// ---------------------------------------------------------------------------

struct LocalHarness {
    app: Option<App>,
    broker_handle: Option<BrokerHandle>,
    inbox_root: std::path::PathBuf,
    workspace: std::path::PathBuf,
    _inbox_root_dir: tempfile::TempDir,
    _workspace_dir: tempfile::TempDir,
    fake_transport: FakeTransport,
    tool_injector: ToolInjector,
}

impl LocalHarness {
    async fn new(fake_transport: FakeTransport, tool_injector: ToolInjector) -> Self {
        let inbox_root_dir = tempfile::Builder::new()
            .prefix("ox-reconfirm-inbox-")
            .tempdir()
            .expect("inbox temp dir");
        let workspace_dir = tempfile::Builder::new()
            .prefix("ox-reconfirm-ws-")
            .tempdir()
            .expect("workspace temp dir");
        let inbox_root = inbox_root_dir.path().to_path_buf();
        let workspace = workspace_dir.path().to_path_buf();

        let broker_handle = build_broker(&inbox_root).await;
        let broker = broker_handle.broker.clone();
        let factory = factory_for(fake_transport.clone());

        let app = App::new_with_test_hooks(
            workspace.clone(),
            inbox_root.clone(),
            /* no_policy */ true,
            broker,
            tokio::runtime::Handle::current(),
            Some(factory),
            Some(tool_injector.clone()),
        )
        .expect("construct App");

        Self {
            app: Some(app),
            broker_handle: Some(broker_handle),
            inbox_root,
            workspace,
            _inbox_root_dir: inbox_root_dir,
            _workspace_dir: workspace_dir,
            fake_transport,
            tool_injector,
        }
    }

    fn app(&mut self) -> &mut App {
        self.app.as_mut().expect("App dropped")
    }

    fn client(&self) -> ClientHandle {
        self.broker_handle.as_ref().expect("broker").client()
    }

    fn soft_crash(&mut self) {
        self.app.take();
        self.broker_handle.take();
    }

    async fn remount(&mut self) {
        let broker_handle = build_broker(&self.inbox_root).await;
        let broker = broker_handle.broker.clone();
        let factory = factory_for(self.fake_transport.clone());
        let app = App::new_with_test_hooks(
            self.workspace.clone(),
            self.inbox_root.clone(),
            true,
            broker,
            tokio::runtime::Handle::current(),
            Some(factory),
            Some(self.tool_injector.clone()),
        )
        .expect("remount App");
        self.app = Some(app);
        self.broker_handle = Some(broker_handle);
    }
}

async fn build_broker(inbox_root: &std::path::Path) -> BrokerHandle {
    use std::collections::BTreeMap;
    use structfs_core_store::Value;
    let inbox = InboxStore::open(inbox_root).expect("open inbox store");
    let bindings = ox_cli::bindings::default_bindings();
    let mut cfg: BTreeMap<String, Value> = BTreeMap::new();
    cfg.insert(
        "gate/defaults/model".into(),
        Value::String("claude-sonnet-4-20250514".into()),
    );
    cfg.insert(
        "gate/defaults/account".into(),
        Value::String("anthropic".into()),
    );
    cfg.insert("gate/defaults/max_tokens".into(), Value::Integer(4096));
    cfg.insert(
        "gate/accounts/anthropic/provider".into(),
        Value::String("anthropic".into()),
    );
    cfg.insert(
        "gate/accounts/anthropic/key".into(),
        Value::String("fake-reconfirm-key".into()),
    );
    broker_setup(inbox, bindings, inbox_root.to_path_buf(), cfg).await
}

// ---------------------------------------------------------------------------
// Counter tool — the observable side effect the tests crash around.
// ---------------------------------------------------------------------------

const COUNTER_TOOL_NAME: &str = "ox_test_counter";

fn counter_tool_injector(counter: Arc<AtomicU64>) -> ToolInjector {
    Arc::new(move || {
        let c = counter.clone();
        let tool = FnTool::new(
            COUNTER_TOOL_NAME,
            "native/ox_test_counter",
            "Test-only tool: increments a counter and returns its new value.",
            serde_json::json!({"type": "object", "properties": {}, "required": []}),
            move |_input| {
                let prev = c.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::json!({"ok": true, "new_value": prev + 1}))
            },
        );
        vec![Box::new(tool) as Box<dyn ox_tools::native::NativeTool>]
    })
}

// ---------------------------------------------------------------------------
// Helpers — `wait_for_pending_approval`, `wait_for_log_entry`, and
// `respond_to_approval` were promoted into `crash_harness::mod` in Task 5.
// ---------------------------------------------------------------------------

/// Hand-craft the pre-crash log tail for an `AwaitingToolResult` scenario:
/// `TurnStart → User → Assistant(tool_use) → ToolCall → ApprovalResolved(allow_once)`.
/// The missing `ToolResult` is what the mount classifier picks up.
async fn seed_awaiting_tool_result(client: &ClientHandle, thread_id: &str, tool_use_id: &str) {
    append_log_entry(client, thread_id, LogEntry::TurnStart { scope: None }).await;
    append_log_entry(
        client,
        thread_id,
        LogEntry::User {
            content: "increment please".into(),
            scope: None,
        },
    )
    .await;
    append_log_entry(
        client,
        thread_id,
        LogEntry::Assistant {
            content: vec![ContentBlock::ToolUse(ox_kernel::ToolCall {
                id: tool_use_id.into(),
                name: COUNTER_TOOL_NAME.into(),
                input: serde_json::json!({}),
            })],
            source: None,
            scope: None,
            completion_id: 0,
        },
    )
    .await;
    append_log_entry(
        client,
        thread_id,
        LogEntry::ToolCall {
            id: tool_use_id.into(),
            name: COUNTER_TOOL_NAME.into(),
            input: serde_json::json!({}),
            scope: None,
        },
    )
    .await;
    append_log_entry(
        client,
        thread_id,
        LogEntry::ApprovalResolved {
            tool_name: COUNTER_TOOL_NAME.into(),
            decision: ox_types::Decision::AllowOnce,
        },
    )
    .await;
}

// ---------------------------------------------------------------------------
// 6a — Retry (AllowOnce): counter reaches 2, turn completes.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconfirm_retry_reruns_tool_counter_reaches_two() {
    init_tracing();

    // The counter is pre-set to 1 to simulate "the tool's side effect
    // landed before the crash." Retry must increment it to 2.
    let counter = Arc::new(AtomicU64::new(1));
    let injector = counter_tool_injector(counter.clone());

    // Transport script: one turn after the tool-result is recorded,
    // emits text-only so the turn ends cleanly.
    let transport = FakeTransport::new();
    transport.push_turn(vec![
        ox_kernel::StreamEvent::TextDelta("Counter bumped.".into()),
        ox_kernel::StreamEvent::MessageStop,
    ]);

    let mut harness = LocalHarness::new(transport.clone(), injector).await;
    let tid = create_thread(&harness.client(), "t-reconfirm-retry").await;
    seed_awaiting_tool_result(&harness.client(), &tid, "tu-retry-1").await;

    harness.soft_crash();
    harness.remount().await;

    harness.app().pool.ensure_worker(&tid);

    let client = harness.client();
    let pending = wait_for_pending_approval(&client, &tid, Duration::from_secs(10)).await;
    assert_eq!(pending.tool_name, COUNTER_TOOL_NAME);

    // Retry
    respond_to_approval(&client, &tid, ox_types::Decision::AllowOnce).await;

    wait_for_log_entry(&client, &tid, Duration::from_secs(10), |e| {
        matches!(e, LogEntry::TurnEnd { .. })
    })
    .await;

    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "Retry must re-execute the counter tool, bumping it from 1 to 2",
    );
    assert_eq!(
        transport.call_count(),
        1,
        "post-dispatch turn needs exactly one completion call after Retry",
    );

    // Ledger invariants: ToolAborted, post-crash ApprovalRequested, and
    // a ToolResult (from the retry) must all be present.
    let log = crash_harness::read_shared_log(&client, &tid).await;
    assert!(
        log.iter()
            .any(|e| matches!(e, LogEntry::ToolAborted { .. })),
        "mount lifecycle must have appended a ToolAborted on remount",
    );
    assert!(
        log.iter().any(|e| matches!(
            e,
            LogEntry::ApprovalRequested {
                post_crash_reconfirm: true,
                ..
            }
        )),
        "kernel resume prologue must have written a post-crash ApprovalRequested",
    );
    assert!(
        log.iter().any(|e| matches!(e, LogEntry::ToolResult { .. })),
        "Retry must produce a real ToolResult",
    );
}

// ---------------------------------------------------------------------------
// 6b — Skip (DenyOnce): counter stays at 1, synthetic ToolResult written.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconfirm_skip_writes_synthetic_tool_result_counter_stays_one() {
    init_tracing();

    let counter = Arc::new(AtomicU64::new(1));
    let injector = counter_tool_injector(counter.clone());

    let transport = FakeTransport::new();
    transport.push_turn(vec![
        ox_kernel::StreamEvent::TextDelta("Acknowledged skip.".into()),
        ox_kernel::StreamEvent::MessageStop,
    ]);

    let mut harness = LocalHarness::new(transport.clone(), injector).await;
    let tid = create_thread(&harness.client(), "t-reconfirm-skip").await;
    seed_awaiting_tool_result(&harness.client(), &tid, "tu-skip-1").await;

    harness.soft_crash();
    harness.remount().await;

    harness.app().pool.ensure_worker(&tid);

    let client = harness.client();
    let pending = wait_for_pending_approval(&client, &tid, Duration::from_secs(10)).await;
    assert_eq!(pending.tool_name, COUNTER_TOOL_NAME);

    // Skip
    respond_to_approval(&client, &tid, ox_types::Decision::DenyOnce).await;

    wait_for_log_entry(&client, &tid, Duration::from_secs(10), |e| {
        matches!(e, LogEntry::TurnEnd { .. })
    })
    .await;

    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "Skip must NOT re-execute the tool",
    );

    // The synthetic ToolResult must carry the plan-pinned
    // `POST_CRASH_SKIP_CONTENT` string (sourced from the shell at
    // `shell/post_crash_skip_content`, per Task 3c's deviation).
    let log = crash_harness::read_shared_log(&client, &tid).await;
    let tr = log
        .iter()
        .find_map(|e| match e {
            LogEntry::ToolResult { id, output, .. } if id == "tu-skip-1" => Some(output.clone()),
            _ => None,
        })
        .expect("Skip must record a synthetic ToolResult");
    let tr_str = match &tr {
        serde_json::Value::String(s) => s.clone(),
        other => panic!("synthetic ToolResult output must be a string; got {other:?}"),
    };
    assert_eq!(
        tr_str,
        ox_cli::test_theme_exports::POST_CRASH_SKIP_CONTENT,
        "Skip-path ToolResult content must match `POST_CRASH_SKIP_CONTENT` \
         verbatim — changing this is a plan amendment",
    );
}

// ---------------------------------------------------------------------------
// 6c — Cancel (CancelTurn): TurnAborted written, counter stays at 1.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconfirm_cancel_writes_turn_aborted_counter_stays_one() {
    init_tracing();

    let counter = Arc::new(AtomicU64::new(1));
    let injector = counter_tool_injector(counter.clone());

    // Cancel exits before issuing any completion — the transport
    // should never be called. `fail_if_called_more_than(0)` catches
    // any regression that routes through the normal turn path.
    let transport = FakeTransport::new();
    transport.fail_if_called_more_than(0);

    let mut harness = LocalHarness::new(transport.clone(), injector).await;
    let tid = create_thread(&harness.client(), "t-reconfirm-cancel").await;
    seed_awaiting_tool_result(&harness.client(), &tid, "tu-cancel-1").await;

    harness.soft_crash();
    harness.remount().await;

    harness.app().pool.ensure_worker(&tid);

    let client = harness.client();
    let pending = wait_for_pending_approval(&client, &tid, Duration::from_secs(10)).await;
    assert_eq!(pending.tool_name, COUNTER_TOOL_NAME);

    // Cancel
    respond_to_approval(&client, &tid, ox_types::Decision::CancelTurn).await;

    // The kernel's cancel branch writes `TurnAborted(UserCanceledAfterCrash)`
    // and returns from `run_turn`.
    wait_for_log_entry(&client, &tid, Duration::from_secs(10), |e| {
        matches!(
            e,
            LogEntry::TurnAborted {
                reason: ox_kernel::log::TurnAbortReason::UserCanceledAfterCrash,
            }
        )
    })
    .await;

    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "Cancel must NOT re-execute the tool",
    );
    assert_eq!(
        transport.call_count(),
        0,
        "Cancel must not issue any LLM completion",
    );
}

// ---------------------------------------------------------------------------
// 6d — Auto-approved tool: modal still appears (kernel-enforced invariant).
// ---------------------------------------------------------------------------
//
// The Task 3d narrative: "if a tool would have been auto-approved, the
// re-confirm modal STILL appears." The harness already runs with
// `no_policy: true` (PermissivePolicy), so every tool is effectively
// auto-approved on the normal path. That configuration is the test:
// the fact that an `ApprovalRequested` appears at all after
// `AwaitingToolResult` proves the re-confirm is driven by the kernel's
// resume prologue, not by the policy check. Retrying here also verifies
// the end-to-end flow.

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconfirm_auto_approved_tool_still_surfaces_modal() {
    init_tracing();

    let counter = Arc::new(AtomicU64::new(1));
    let injector = counter_tool_injector(counter.clone());

    let transport = FakeTransport::new();
    transport.push_turn(vec![
        ox_kernel::StreamEvent::TextDelta("Retried.".into()),
        ox_kernel::StreamEvent::MessageStop,
    ]);

    // `no_policy: true` inside `LocalHarness::new`'s App builder means
    // the normal-path policy would auto-approve. The resume modal must
    // still appear.
    let mut harness = LocalHarness::new(transport.clone(), injector).await;
    let tid = create_thread(&harness.client(), "t-reconfirm-auto").await;
    seed_awaiting_tool_result(&harness.client(), &tid, "tu-auto-1").await;

    harness.soft_crash();
    harness.remount().await;

    harness.app().pool.ensure_worker(&tid);

    let client = harness.client();
    // The observable assertion: a pending approval DOES appear even
    // though policy would normally auto-approve this tool.
    let pending = wait_for_pending_approval(&client, &tid, Duration::from_secs(10)).await;
    assert_eq!(pending.tool_name, COUNTER_TOOL_NAME);

    // Retry; the flow completes normally.
    respond_to_approval(&client, &tid, ox_types::Decision::AllowOnce).await;
    wait_for_log_entry(&client, &tid, Duration::from_secs(10), |e| {
        matches!(e, LogEntry::TurnEnd { .. })
    })
    .await;

    assert_eq!(counter.load(Ordering::SeqCst), 2);

    let log = crash_harness::read_shared_log(&client, &tid).await;
    let post_crash = log
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
        post_crash, 1,
        "auto-approved tools must still produce a post-crash ApprovalRequested \
         — the re-confirm is a kernel invariant, not a UI bolt-on",
    );
}

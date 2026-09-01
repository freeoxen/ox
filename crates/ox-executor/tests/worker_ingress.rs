use ox_executor::test_support::{FakeTransport, ToolInjector, factory_for};
use ox_executor::{ExecutionCore, ExecutorConfig, IngressBoundary, mount_execution_stores};
use ox_inbox::worker_ingress::{
    CancelEnvelope, CreateEnvelope, DecisionEnvelope, IntentKind, PromptEnvelope,
};
use ox_kernel::log::LogEntry;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use structfs_core_store::{Reader, Record, Value, Writer, path};

async fn broker_for(root: &Path) -> (ox_broker::BrokerStore, Vec<tokio::task::JoinHandle<()>>) {
    let broker = ox_broker::BrokerStore::default();
    let servers = mount_execution_stores(
        &broker,
        ox_inbox::InboxStore::open(root).unwrap(),
        root.to_path_buf(),
        ox_store_util::LocalConfig::new(),
        ox_store_util::LocalConfig::new(),
    )
    .await;
    (broker, servers)
}

fn core_for(
    root: &Path,
    workspace: &Path,
    broker: ox_broker::BrokerStore,
    transport: &FakeTransport,
    config: ExecutorConfig,
) -> ExecutionCore {
    core_for_with_injector(root, workspace, broker, transport, config, None)
}

fn core_for_with_injector(
    root: &Path,
    workspace: &Path,
    broker: ox_broker::BrokerStore,
    transport: &FakeTransport,
    config: ExecutorConfig,
    tool_injector: Option<ToolInjector>,
) -> ExecutionCore {
    ExecutionCore::new_with_config_and_test_hooks(
        workspace.to_path_buf(),
        true,
        ox_inbox::InboxStore::open(root).unwrap(),
        root.to_path_buf(),
        broker,
        tokio::runtime::Handle::current(),
        config,
        Some(factory_for(transport.clone())),
        tool_injector,
    )
    .unwrap()
}

fn create_plain_thread(root: &Path, title: &str) -> String {
    let mut inbox = ox_inbox::InboxStore::open(root).unwrap();
    let mut create = BTreeMap::new();
    create.insert("title".into(), Value::String(title.into()));
    inbox
        .write(&path!("threads"), Record::parsed(Value::Map(create)))
        .unwrap()
        .iter()
        .last()
        .unwrap()
        .clone()
}

async fn wait_for_state(root: &Path, kind: IntentKind, id: &str, state: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let inbox = ox_inbox::InboxStore::open(root).unwrap();
            if inbox
                .worker_intent(kind, id)
                .unwrap()
                .is_some_and(|intent| intent.state == state)
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn wait_for_boundary(root: &Path, thread_id: &str, predicate: impl Fn(&[LogEntry]) -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let ledger = root.join("threads").join(thread_id).join("ledger.jsonl");
            if ledger.exists() {
                let entries = ox_inbox::ledger::read_ledger(&ledger)
                    .unwrap()
                    .into_iter()
                    .map(|entry| serde_json::from_value(entry.msg).unwrap())
                    .collect::<Vec<LogEntry>>();
                if predicate(&entries) {
                    return;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn wait_for_thread(root: &Path, thread_id: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !root.join("threads").join(thread_id).exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

fn user_count(root: &Path, thread_id: &str) -> usize {
    ox_inbox::ledger::read_ledger(&root.join("threads").join(thread_id).join("ledger.jsonl"))
        .unwrap()
        .into_iter()
        .filter(|entry| entry.msg["type"] == "user")
        .count()
}

fn entries(root: &Path, thread_id: &str) -> Vec<LogEntry> {
    ox_inbox::ledger::read_ledger(&root.join("threads").join(thread_id).join("ledger.jsonl"))
        .unwrap()
        .into_iter()
        .map(|entry| serde_json::from_value(entry.msg).unwrap())
        .collect()
}

async fn wait_for_pending_approval(broker: &ox_broker::BrokerStore, thread_id: &str) {
    let client = broker.client().scoped(&format!("threads/{thread_id}"));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let pending = client.read(&path!("approval/pending")).await.unwrap();
            if pending.as_ref().and_then(Record::as_value) != Some(&Value::Null)
                && pending.is_some()
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

fn accept_tool_prompt(root: &Path, thread_id: &str, id: &str) {
    ox_inbox::InboxStore::open(root)
        .unwrap()
        .accept_worker_message(
            thread_id,
            &PromptEnvelope {
                message_id: id.into(),
                content: "run the requested tool".into(),
            },
        )
        .unwrap();
}

fn shell_turn(command: &str) -> Vec<ox_kernel::StreamEvent> {
    vec![
        ox_kernel::StreamEvent::ToolUseStart {
            id: "tool-1".into(),
            name: "shell".into(),
        },
        ox_kernel::StreamEvent::ToolUseInputDelta {
            delta: serde_json::json!({"command": command}).to_string(),
        },
        ox_kernel::StreamEvent::MessageStop,
    ]
}

fn blocking_tool_turn() -> Vec<ox_kernel::StreamEvent> {
    vec![
        ox_kernel::StreamEvent::ToolUseStart {
            id: "tool-1".into(),
            name: "blocking_test".into(),
        },
        ox_kernel::StreamEvent::ToolUseInputDelta { delta: "{}".into() },
        ox_kernel::StreamEvent::MessageStop,
    ]
}

fn assert_terminal_cancel(root: &Path, thread_id: &str) {
    let entries = entries(root, thread_id);
    assert!(matches!(
        entries.last(),
        Some(LogEntry::TurnAborted {
            reason: ox_kernel::log::TurnAbortReason::UserCanceled
        })
    ));
    let mut inbox = ox_inbox::InboxStore::open(root).unwrap();
    let record = inbox
        .read(&structfs_core_store::Path::parse(&format!("threads/{thread_id}")).unwrap())
        .unwrap()
        .unwrap();
    let state = match record.as_value() {
        Some(Value::Map(map)) => map.get("thread_state"),
        other => panic!("expected thread metadata map, got {other:?}"),
    };
    assert_eq!(state, Some(&Value::String("interrupted".into())));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_initial_prompt_recovers_after_action_before_enqueue() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let inbox = ox_inbox::InboxStore::open(root.path()).unwrap();
    let accepted = inbox
        .accept_worker_create(&CreateEnvelope {
            create_id: "create-restart".into(),
            title: "restart".into(),
            prompt: "initial prompt".into(),
            parent_id: None,
        })
        .unwrap();
    let thread_id = accepted.thread_id.unwrap();
    let transport = FakeTransport::new();
    transport.push_turn(vec![ox_kernel::StreamEvent::MessageStop]);
    let config = ExecutorConfig::default();
    config
        .ingress_failpoints
        .arm(IngressBoundary::AfterCreateActionBeforeMark);
    let (broker, servers) = broker_for(root.path()).await;
    let handle = core_for(root.path(), workspace.path(), broker, &transport, config).into_handle(8);
    wait_for_thread(root.path(), &thread_id).await;
    drop(handle);
    drop(servers);

    let (broker, servers) = broker_for(root.path()).await;
    let handle = core_for(
        root.path(),
        workspace.path(),
        broker,
        &transport,
        ExecutorConfig::default(),
    )
    .into_handle(8);
    wait_for_state(root.path(), IntentKind::Create, "create-restart", "applied").await;
    assert_eq!(user_count(root.path(), &thread_id), 1);
    assert_eq!(transport.call_count(), 1);
    drop(handle);
    drop(servers);
}

async fn message_restart_case(boundary: IngressBoundary, expected_calls_before: usize) {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let thread_id = create_plain_thread(root.path(), "message restart");
    let inbox = ox_inbox::InboxStore::open(root.path()).unwrap();
    inbox
        .accept_worker_message(
            &thread_id,
            &PromptEnvelope {
                message_id: "message-restart".into(),
                content: "once".into(),
            },
        )
        .unwrap();
    let transport = FakeTransport::new();
    transport.push_turn(vec![ox_kernel::StreamEvent::MessageStop]);
    transport.fail_if_called_more_than(1);
    let config = ExecutorConfig::default();
    config.ingress_failpoints.arm(boundary);
    let (broker, servers) = broker_for(root.path()).await;
    let handle = core_for(root.path(), workspace.path(), broker, &transport, config).into_handle(8);
    wait_for_boundary(root.path(), &thread_id, |entries| match boundary {
        IngressBoundary::AfterMessageMarkerBeforeUser => !entries.is_empty(),
        IngressBoundary::AfterMessageUserBeforeTurn => entries
            .iter()
            .any(|entry| matches!(entry, LogEntry::User { .. })),
        IngressBoundary::AfterMessageTurnBeforeMark => entries
            .iter()
            .any(|entry| matches!(entry, LogEntry::TurnEnd { .. })),
        _ => false,
    })
    .await;
    assert_eq!(transport.call_count(), expected_calls_before);
    drop(handle);
    drop(servers);

    let (broker, servers) = broker_for(root.path()).await;
    let handle = core_for(
        root.path(),
        workspace.path(),
        broker,
        &transport,
        ExecutorConfig::default(),
    )
    .into_handle(8);
    wait_for_state(
        root.path(),
        IntentKind::Message,
        "message-restart",
        "applied",
    )
    .await;
    assert_eq!(user_count(root.path(), &thread_id), 1);
    assert_eq!(transport.call_count(), 1);
    drop(handle);
    drop(servers);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn message_recovers_marker_only() {
    message_restart_case(IngressBoundary::AfterMessageMarkerBeforeUser, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn message_recovers_user_before_turn_without_duplicate_append() {
    message_restart_case(IngressBoundary::AfterMessageUserBeforeTurn, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn message_recovers_turn_before_mark_without_second_turn() {
    message_restart_case(IngressBoundary::AfterMessageTurnBeforeMark, 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn decision_recovers_after_response_before_applied_mark() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let thread_id = create_plain_thread(root.path(), "decision restart");
    accept_tool_prompt(root.path(), &thread_id, "decision-prompt");
    let transport = FakeTransport::new();
    transport.push_turn(shell_turn("echo denied"));
    transport.push_turn(vec![ox_kernel::StreamEvent::MessageStop]);
    let config = ExecutorConfig::default();
    config
        .ingress_failpoints
        .arm(IngressBoundary::AfterDecisionResponseBeforeMark);
    let (broker, servers) = broker_for(root.path()).await;
    let handle = core_for(
        root.path(),
        workspace.path(),
        broker.clone(),
        &transport,
        config,
    )
    .into_handle(8);
    wait_for_pending_approval(&broker, &thread_id).await;
    assert_eq!(
        handle.dispatch_worker_ingress().unwrap(),
        0,
        "an accepted in-flight prompt is dispatched once per process"
    );
    assert_eq!(user_count(root.path(), &thread_id), 1);
    let approval_id =
        ox_executor::derive_unresolved_approval_id(&thread_id, &entries(root.path(), &thread_id))
            .expect("pending approval has durable tool-call evidence");
    ox_inbox::InboxStore::open(root.path())
        .unwrap()
        .accept_worker_decision(
            &thread_id,
            &DecisionEnvelope {
                approval_id: approval_id.clone(),
                decision: ox_types::Decision::DenyOnce,
            },
        )
        .unwrap();
    handle.dispatch_worker_ingress().unwrap();
    wait_for_boundary(root.path(), &thread_id, |entries| {
        entries.iter().any(|entry| {
            matches!(entry, LogEntry::ApprovalResolved { decision, .. }
                if *decision == ox_types::Decision::DenyOnce)
        })
    })
    .await;
    wait_for_boundary(root.path(), &thread_id, |entries| {
        entries
            .iter()
            .any(|entry| matches!(entry, LogEntry::TurnEnd { .. }))
    })
    .await;
    assert_eq!(
        ox_inbox::InboxStore::open(root.path())
            .unwrap()
            .worker_intent(IntentKind::Decision, &approval_id)
            .unwrap()
            .unwrap()
            .state,
        "accepted"
    );
    drop(handle);
    drop(servers);

    let (broker, servers) = broker_for(root.path()).await;
    let handle = core_for(
        root.path(),
        workspace.path(),
        broker,
        &transport,
        ExecutorConfig::default(),
    )
    .into_handle(8);
    wait_for_state(root.path(), IntentKind::Decision, &approval_id, "applied").await;
    assert_eq!(
        entries(root.path(), &thread_id)
            .iter()
            .filter(|entry| matches!(entry, LogEntry::ApprovalResolved { .. }))
            .count(),
        1
    );
    drop(handle);
    drop(servers);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_wakes_blocked_approval_and_applies_every_cancel_id() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let thread_id = create_plain_thread(root.path(), "cancel approval");
    accept_tool_prompt(root.path(), &thread_id, "cancel-approval-prompt");
    let transport = FakeTransport::new();
    transport.push_turn(shell_turn("echo should-not-run"));
    let (broker, servers) = broker_for(root.path()).await;
    let handle = core_for(
        root.path(),
        workspace.path(),
        broker.clone(),
        &transport,
        ExecutorConfig::default(),
    )
    .into_handle(8);
    wait_for_pending_approval(&broker, &thread_id).await;
    let inbox = ox_inbox::InboxStore::open(root.path()).unwrap();
    for id in ["cancel-one", "cancel-two"] {
        inbox
            .accept_worker_cancel(
                &thread_id,
                &CancelEnvelope {
                    cancel_id: id.into(),
                    reason: Some("test".into()),
                },
            )
            .unwrap();
    }
    handle.dispatch_worker_ingress().unwrap();
    wait_for_state(root.path(), IntentKind::Cancel, "cancel-one", "applied").await;
    wait_for_state(root.path(), IntentKind::Cancel, "cancel-two", "applied").await;
    assert_terminal_cancel(root.path(), &thread_id);
    drop(handle);
    drop(servers);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_recovers_after_terminal_evidence_before_applied_mark() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let thread_id = create_plain_thread(root.path(), "cancel restart");
    let inbox = ox_inbox::InboxStore::open(root.path()).unwrap();
    inbox
        .accept_worker_cancel(
            &thread_id,
            &CancelEnvelope {
                cancel_id: "cancel-restart".into(),
                reason: Some("restart boundary".into()),
            },
        )
        .unwrap();

    let config = ExecutorConfig::default();
    config
        .ingress_failpoints
        .arm(IngressBoundary::AfterCancelAbortBeforeMark);
    let transport = FakeTransport::new();
    let (broker, servers) = broker_for(root.path()).await;
    let handle = core_for(root.path(), workspace.path(), broker, &transport, config).into_handle(8);
    wait_for_boundary(root.path(), &thread_id, |entries| {
        entries.iter().any(|entry| {
            matches!(
                entry,
                LogEntry::TurnAborted {
                    reason: ox_kernel::log::TurnAbortReason::UserCanceled
                }
            )
        })
    })
    .await;
    assert_eq!(
        ox_inbox::InboxStore::open(root.path())
            .unwrap()
            .worker_intent(IntentKind::Cancel, "cancel-restart")
            .unwrap()
            .unwrap()
            .state,
        "accepted"
    );
    drop(handle);
    drop(servers);

    let (broker, servers) = broker_for(root.path()).await;
    let handle = core_for(
        root.path(),
        workspace.path(),
        broker,
        &transport,
        ExecutorConfig::default(),
    )
    .into_handle(8);
    wait_for_state(root.path(), IntentKind::Cancel, "cancel-restart", "applied").await;
    assert_eq!(
        entries(root.path(), &thread_id)
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    LogEntry::TurnAborted {
                        reason: ox_kernel::log::TurnAbortReason::UserCanceled
                    }
                )
            })
            .count(),
        1
    );
    assert_terminal_cancel(root.path(), &thread_id);
    drop(handle);
    drop(servers);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_finalizes_after_active_tool_returns() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let thread_id = create_plain_thread(root.path(), "cancel shell");
    accept_tool_prompt(root.path(), &thread_id, "cancel-shell-prompt");
    let transport = FakeTransport::new();
    transport.push_turn(blocking_tool_turn());
    let active = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let injector: ToolInjector = {
        let active = active.clone();
        let release = release.clone();
        Arc::new(move || {
            let active = active.clone();
            let release = release.clone();
            vec![Box::new(ox_tools::native::FnTool::new(
                "blocking_test",
                "custom/blocking_test",
                "test-only blocking tool",
                serde_json::json!({"type": "object"}),
                move |_| {
                    active.store(true, Ordering::Release);
                    while !release.load(Ordering::Acquire) {
                        std::thread::yield_now();
                    }
                    Ok(serde_json::json!({"released": true}))
                },
            ))]
        })
    };
    let mut config = ExecutorConfig::default();
    config
        .remote_native_tool_allowlist
        .insert("blocking_test".into());
    let (broker, servers) = broker_for(root.path()).await;
    let handle = core_for_with_injector(
        root.path(),
        workspace.path(),
        broker.clone(),
        &transport,
        config,
        Some(injector),
    )
    .into_handle(8);
    wait_for_pending_approval(&broker, &thread_id).await;
    broker
        .client()
        .scoped(&format!("threads/{thread_id}"))
        .write_typed(
            &path!("approval/response"),
            &ox_types::ApprovalResponse {
                decision: ox_types::Decision::AllowOnce,
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !active.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    ox_inbox::InboxStore::open(root.path())
        .unwrap()
        .accept_worker_cancel(
            &thread_id,
            &CancelEnvelope {
                cancel_id: "cancel-shell".into(),
                reason: None,
            },
        )
        .unwrap();
    handle.dispatch_worker_ingress().unwrap();
    release.store(true, Ordering::Release);
    wait_for_state(root.path(), IntentKind::Cancel, "cancel-shell", "applied").await;
    assert_terminal_cancel(root.path(), &thread_id);
    drop(handle);
    drop(servers);
}

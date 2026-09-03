#![cfg(unix)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{os::unix::fs::PermissionsExt, time::Duration};

use ox_broker::async_store::{AsyncReader, AsyncWriter};
use ox_executor::test_support::{FakeTransport, TransportFactory, factory_for};
use ox_inbox::worker_ingress::{CancelEnvelope, CreateEnvelope, PromptEnvelope};
use ox_worker::public_store::test_support as public_test_support;
use ox_worker::{WorkerConfig, WorkerLimits, WorkerService};
use structfs_core_store::{Format, Path, Record, Value, path};

#[derive(Default)]
struct FourRoleState {
    b_streaming: AtomicBool,
    c_active: AtomicBool,
    d_active: AtomicBool,
    release_b: AtomicBool,
    release_c: AtomicBool,
    release_d: AtomicBool,
}

#[derive(Clone)]
struct FourRoleTransport(Arc<FourRoleState>);

impl ox_tools::completion::CompletionTransport for FourRoleTransport {
    fn send(
        &self,
        request: &ox_kernel::CompletionRequest,
        on_event: &dyn Fn(&ox_kernel::StreamEvent),
    ) -> Result<ox_tools::completion::CompletionOutput, String> {
        let prompt = serde_json::to_string(&request.messages).unwrap();
        if prompt.contains("ROLE_A") {
            let events = vec![
                ox_kernel::StreamEvent::ToolUseStart {
                    id: "role-a-tool".into(),
                    name: "shell".into(),
                },
                ox_kernel::StreamEvent::ToolUseInputDelta {
                    delta: serde_json::json!({"command": "true"}).to_string(),
                },
                ox_kernel::StreamEvent::MessageStop,
            ];
            for event in &events {
                on_event(event);
            }
            return Ok(ox_tools::completion::CompletionOutput {
                events,
                ..Default::default()
            });
        }

        let (active, release, prefix) = if prompt.contains("ROLE_B") {
            (&self.0.b_streaming, &self.0.release_b, Some("stream"))
        } else if prompt.contains("ROLE_C") {
            (&self.0.c_active, &self.0.release_c, None)
        } else if prompt.contains("ROLE_D") {
            (&self.0.d_active, &self.0.release_d, None)
        } else {
            return Err("unknown four-role prompt".into());
        };
        let mut events = Vec::new();
        if let Some(prefix) = prefix {
            for index in 0..24 {
                let event = ox_kernel::StreamEvent::TextDelta {
                    text: format!("{prefix}-{index} "),
                };
                on_event(&event);
                events.push(event);
            }
        }
        active.store(true, Ordering::Release);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !release.load(Ordering::Acquire) {
            if std::time::Instant::now() >= deadline {
                return Err("four-role release timed out".into());
            }
            std::thread::yield_now();
        }
        let stop = ox_kernel::StreamEvent::MessageStop;
        on_event(&stop);
        events.push(stop);
        Ok(ox_tools::completion::CompletionOutput {
            events,
            ..Default::default()
        })
    }
}

fn four_role_factory(state: Arc<FourRoleState>) -> TransportFactory {
    Arc::new(move || Box::new(FourRoleTransport(state.clone())))
}

fn record<T: serde::Serialize>(value: &T) -> Record {
    Record::parsed(structfs_serde_store::to_value(value).unwrap())
}

fn private_tempdir() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    temp
}

#[test]
fn defensive_public_value_decoders_reject_corrupt_internal_state() {
    assert_eq!(
        public_test_support::required_count(Some(Value::Integer(3))).unwrap(),
        3
    );
    assert!(public_test_support::required_count(Some(Value::Integer(-1))).is_err());
    assert!(public_test_support::required_count(None).is_err());
    assert!(public_test_support::required_count(Some(Value::Null)).is_err());

    assert_eq!(public_test_support::thread_count(None).unwrap(), 0);
    assert_eq!(
        public_test_support::thread_count(Some(Value::Array(vec![Value::Null, Value::Null])))
            .unwrap(),
        2
    );
    assert!(public_test_support::thread_count(Some(Value::Integer(1))).is_err());

    assert_eq!(public_test_support::pending_value(None), None);
    assert_eq!(public_test_support::pending_value(Some(Value::Null)), None);
    assert_eq!(
        public_test_support::pending_value(Some(Value::String("request".into()))),
        Some(Value::String("request".into()))
    );

    let receipt = structfs_serde_store::json_to_value(serde_json::json!({"thread_id": "t_1"}));
    assert_eq!(
        public_test_support::receipt_thread_id(receipt).unwrap(),
        "t_1"
    );
    let missing = structfs_serde_store::json_to_value(serde_json::json!({"status": "created"}));
    assert!(public_test_support::receipt_thread_id(missing).is_err());
    assert!(public_test_support::receipt_thread_id(Value::Null).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_core_hosts_two_threads_with_bounded_nonblocking_public_control() {
    let temp = private_tempdir();
    let root = temp.path().join("inbox");
    let socket = temp.path().join("runtime").join("worker.sock");
    let limits = WorkerLimits {
        max_active_turns: 1,
        max_queued_inputs_per_thread: 1,
        max_total_threads: 2,
        max_parked_cursors: 2,
        ..WorkerLimits::default()
    };
    let service = WorkerService::start_in_process(WorkerConfig {
        inbox_root: root.clone(),
        socket_path: socket.clone(),
        node_id: "node-a".into(),
        attempt_id: "attempt-1".into(),
        command_capacity: 16,
        limits,
        transport: ox_structfs_transport::ServerConfig::default(),
    })
    .await
    .unwrap();

    let mut store = service.public_store.clone();
    assert!(public_test_support::message_admission_is_reused(
        &store,
        "same-thread"
    ));
    assert!(public_test_support::message_admission_recovers_from_poison(
        &store
    ));
    let first = CreateEnvelope {
        create_id: "create-1".into(),
        title: "one".into(),
        prompt: "hello".into(),
        parent_id: None,
    };
    let second = CreateEnvelope {
        create_id: "create-2".into(),
        title: "two".into(),
        prompt: "hello".into(),
        parent_id: None,
    };
    let first_path = store
        .write(&path!("conversations"), record(&first))
        .await
        .unwrap();
    let second_path = store
        .write(&path!("conversations"), record(&second))
        .await
        .unwrap();
    let first_id = first_path.iter().nth(1).unwrap().clone();
    let second_id = second_path.iter().nth(1).unwrap().clone();
    assert_ne!(first_id, second_id);
    assert!(root.join("threads").join(&first_id).is_dir());
    assert!(root.join("threads").join(&second_id).is_dir());

    let duplicate = store
        .write(&path!("conversations"), record(&first))
        .await
        .unwrap();
    assert_eq!(duplicate, first_path);
    let third = CreateEnvelope {
        create_id: "create-3".into(),
        title: "three".into(),
        prompt: "no".into(),
        parent_id: None,
    };
    assert!(
        store
            .write(&path!("conversations"), record(&third))
            .await
            .unwrap_err()
            .to_string()
            .contains("total thread limit")
    );

    let message_path = Path::parse(&format!("conversations/{first_id}/messages")).unwrap();
    store
        .write(
            &message_path,
            record(&PromptEnvelope {
                message_id: "message-1".into(),
                content: "queued".into(),
            }),
        )
        .await
        .unwrap();
    let saturated = store
        .write(
            &message_path,
            record(&PromptEnvelope {
                message_id: "message-2".into(),
                content: "bounded".into(),
            }),
        )
        .await;
    assert!(
        saturated
            .unwrap_err()
            .to_string()
            .contains("queued input limit")
    );

    let mut health_store = service.public_store.clone();
    let health = tokio::time::timeout(
        Duration::from_millis(250),
        health_store.read(&path!("health")),
    )
    .await
    .expect("health must not wait for turn admission")
    .unwrap()
    .unwrap();
    assert!(matches!(health.as_value(), Some(Value::Map(_))));
    let mut capacity_store = service.public_store.clone();
    let capacity = tokio::time::timeout(
        Duration::from_millis(250),
        capacity_store.read(&path!("capacity")),
    )
    .await
    .expect("capacity must not enter executor control queue")
    .unwrap()
    .unwrap();
    let capacity_json = structfs_serde_store::value_to_json(capacity.as_value().unwrap().clone());
    assert_eq!(capacity_json["resident_threads"], 2);
    assert_eq!(capacity_json["total_threads"], 2);
    assert_eq!(
        capacity_json["active_turns_include_approval_parked_wasm"],
        true
    );

    let cancel_path = Path::parse(&format!("conversations/{second_id}/control/cancel")).unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        store.write(
            &cancel_path,
            record(&CancelEnvelope {
                cancel_id: "cancel-2".into(),
                reason: Some("test".into()),
            }),
        ),
    )
    .await
    .expect("cancel remains independent of turn permit")
    .unwrap();

    assert!(store.read(&path!("secret")).await.unwrap().is_none());
    assert!(
        store
            .write(&path!("threads"), Record::parsed(Value::Null))
            .await
            .is_err()
    );
    service.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_surface_is_typed_and_fail_closed_for_bad_or_unknown_mutations() {
    let temp = private_tempdir();
    let root = temp.path().join("inbox");
    let transport = FakeTransport::new();
    transport.push_turn(vec![ox_kernel::StreamEvent::MessageStop]);
    let service = WorkerService::start_in_process_with_test_hooks(
        WorkerConfig {
            inbox_root: root,
            socket_path: temp.path().join("unused.sock"),
            node_id: "surface-node".into(),
            attempt_id: "surface-attempt".into(),
            command_capacity: 8,
            limits: WorkerLimits::default(),
            transport: ox_structfs_transport::ServerConfig::default(),
        },
        Some(factory_for(transport)),
        None,
    )
    .await
    .unwrap();
    let mut store = service.public_store.clone();

    let health = store.read(&path!("health")).await.unwrap().unwrap();
    let health = structfs_serde_store::value_to_json(health.as_value().unwrap().clone());
    assert_eq!(health["status"], "ready");
    assert_eq!(health["node_id"], "surface-node");
    assert_eq!(health["attempt_id"], "surface-attempt");
    assert_eq!(health["sandbox_enforcement"]["preflight"], "passed");
    assert!(health["agent_wasm_sha256"].as_str().unwrap().len() >= 64);

    let capabilities = store.read(&path!("capabilities")).await.unwrap().unwrap();
    let capabilities =
        structfs_serde_store::value_to_json(capabilities.as_value().unwrap().clone());
    assert_eq!(capabilities["multiple_conversations"], true);
    assert_eq!(capabilities["protocol"], "ox-worker-v1");
    assert_eq!(capabilities["operations"].as_array().unwrap().len(), 5);

    assert!(
        store
            .read(&path!("conversations/missing"))
            .await
            .unwrap()
            .is_none()
    );
    let pending = store
        .read(&path!("conversations/missing/approvals/pending"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending.as_value(), Some(&Value::Null));

    for target in [
        path!("conversations"),
        path!("conversations/missing/messages"),
        path!("conversations/missing/approvals/approval_missing"),
        path!("conversations/missing/control/cancel"),
    ] {
        assert!(
            store
                .write(&target, Record::parsed(Value::Null))
                .await
                .is_err()
        );
    }
    assert!(
        store
            .write(
                &path!("conversations"),
                Record::raw(vec![1, 2, 3], Format::OCTET_STREAM),
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("expected parsed record")
    );

    let message = store
        .write(
            &path!("conversations/missing/messages"),
            record(&PromptEnvelope {
                message_id: "unknown-message".into(),
                content: "no destination".into(),
            }),
        )
        .await
        .unwrap_err();
    assert!(message.to_string().contains("unknown conversation"));

    let approval = store
        .write(
            &path!("conversations/missing/approvals/approval_missing"),
            record(&ox_types::ApprovalResponse {
                decision: ox_types::Decision::DenyOnce,
            }),
        )
        .await
        .unwrap_err();
    assert!(approval.to_string().contains("stale or missing"));

    let cancel = store
        .write(
            &path!("conversations/missing/control/cancel"),
            record(&CancelEnvelope {
                cancel_id: "unknown-cancel".into(),
                reason: None,
            }),
        )
        .await
        .unwrap_err();
    assert!(cancel.to_string().contains("unknown conversation"));
    assert!(
        store
            .write(&path!("health"), Record::parsed(Value::Null))
            .await
            .is_err()
    );

    service.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approval_saturates_turn_capacity_but_not_public_control() {
    let temp = private_tempdir();
    let root = temp.path().join("inbox");
    let transport = FakeTransport::new();
    transport.push_turn(vec![
        ox_kernel::StreamEvent::ToolUseStart {
            id: "tool-approval".into(),
            name: "shell".into(),
        },
        ox_kernel::StreamEvent::ToolUseInputDelta {
            delta: serde_json::json!({"command": "true"}).to_string(),
        },
        ox_kernel::StreamEvent::MessageStop,
    ]);
    transport.push_turn(vec![ox_kernel::StreamEvent::MessageStop]);
    let limits = WorkerLimits {
        max_active_turns: 1,
        max_total_threads: 4,
        ..WorkerLimits::default()
    };
    let service = WorkerService::start_in_process_with_test_hooks(
        WorkerConfig {
            inbox_root: root,
            socket_path: temp.path().join("unused.sock"),
            node_id: "node-a".into(),
            attempt_id: "attempt-approval".into(),
            command_capacity: 16,
            limits,
            transport: ox_structfs_transport::ServerConfig::default(),
        },
        Some(factory_for(transport)),
        None,
    )
    .await
    .unwrap();
    let mut store = service.public_store.clone();
    let first = store
        .write(
            &path!("conversations"),
            record(&CreateEnvelope {
                create_id: "approval-create".into(),
                title: "approval".into(),
                prompt: "use shell".into(),
                parent_id: None,
            }),
        )
        .await
        .unwrap();
    let first_id = first.iter().nth(1).unwrap().clone();
    let pending_path = Path::parse(&format!("conversations/{first_id}/approvals/pending")).unwrap();
    let pending = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pending = store.read(&pending_path).await.unwrap().unwrap();
            if pending.as_value() != Some(&Value::Null) {
                break pending;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first turn parks on approval");
    let pending_json = structfs_serde_store::value_to_json(pending.as_value().unwrap().clone());
    let approval_id = pending_json["approval_id"].as_str().unwrap();

    let capacity = store.read(&path!("capacity")).await.unwrap().unwrap();
    let json = structfs_serde_store::value_to_json(capacity.as_value().unwrap().clone());
    assert_eq!(json["active_turns"], 1);
    assert_eq!(json["active_turns_include_approval_parked_wasm"], true);

    let second = store
        .write(
            &path!("conversations"),
            record(&CreateEnvelope {
                create_id: "waiting-create".into(),
                title: "waiting".into(),
                prompt: "wait for permit".into(),
                parent_id: None,
            }),
        )
        .await
        .unwrap();
    let second_id = second.iter().nth(1).unwrap().clone();
    let status_path = Path::parse(&format!("conversations/{second_id}")).unwrap();
    tokio::time::timeout(Duration::from_millis(250), store.read(&path!("health")))
        .await
        .expect("health bypasses saturated turn admission")
        .unwrap();
    tokio::time::timeout(Duration::from_millis(250), store.read(&status_path))
        .await
        .expect("status bypasses saturated turn admission")
        .unwrap();
    let cancel_path = Path::parse(&format!("conversations/{second_id}/control/cancel")).unwrap();
    tokio::time::timeout(
        Duration::from_millis(500),
        store.write(
            &cancel_path,
            record(&CancelEnvelope {
                cancel_id: "cancel-waiting".into(),
                reason: None,
            }),
        ),
    )
    .await
    .expect("cancel bypasses saturated turn admission")
    .unwrap();

    let approval_path =
        Path::parse(&format!("conversations/{first_id}/approvals/{approval_id}")).unwrap();
    let deny = ox_types::ApprovalResponse {
        decision: ox_types::Decision::DenyOnce,
    };
    let first_result = store.write(&approval_path, record(&deny)).await.unwrap();
    let retry_result = store.write(&approval_path, record(&deny)).await.unwrap();
    assert_eq!(
        retry_result, first_result,
        "post-resolution retry is stable"
    );
    let conflict = store
        .write(
            &approval_path,
            record(&ox_types::ApprovalResponse {
                decision: ox_types::Decision::DenyAlways,
            }),
        )
        .await
        .unwrap_err();
    assert!(conflict.to_string().contains("conflict"));
    service.shutdown().await.unwrap();
}

async fn create_role(store: &mut ox_worker::PublicStore, create_id: &str, prompt: &str) -> String {
    store
        .write(
            &path!("conversations"),
            record(&CreateEnvelope {
                create_id: create_id.into(),
                title: create_id.into(),
                prompt: prompt.into(),
                parent_id: None,
            }),
        )
        .await
        .unwrap()
        .iter()
        .nth(1)
        .unwrap()
        .clone()
}

async fn wait_flag(flag: &AtomicBool, label: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !flag.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{label} did not become active"));
}

async fn wait_terminal_log(root: &std::path::Path, thread_id: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let path = root.join("threads").join(thread_id).join("ledger.jsonl");
            if path.exists() {
                let entries = ox_inbox::ledger::read_ledger(&path).unwrap();
                let matched = entries.iter().any(|entry| {
                    matches!(
                        entry.msg["type"].as_str(),
                        Some("turn_end" | "turn_aborted")
                    )
                });
                if matched {
                    return;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("expected log evidence for {thread_id}"));
}

async fn cancel_role(store: &mut ox_worker::PublicStore, thread_id: &str, cancel_id: &str) {
    let target = Path::parse(&format!("conversations/{thread_id}/control/cancel")).unwrap();
    tokio::time::timeout(
        Duration::from_millis(500),
        store.write(
            &target,
            record(&CancelEnvelope {
                cancel_id: cancel_id.into(),
                reason: Some("four-role stress".into()),
            }),
        ),
    )
    .await
    .expect("cancel must bypass active turns")
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn four_roles_progress_without_a_shared_logical_lock() {
    let temp = private_tempdir();
    let root = temp.path().join("inbox");
    let state = Arc::new(FourRoleState::default());
    let service = WorkerService::start_in_process_with_test_hooks(
        WorkerConfig {
            inbox_root: root.clone(),
            socket_path: temp.path().join("unused.sock"),
            node_id: "node-four-role".into(),
            attempt_id: "attempt-four-role".into(),
            command_capacity: 32,
            limits: WorkerLimits {
                max_active_turns: 4,
                max_total_threads: 4,
                ..WorkerLimits::default()
            },
            transport: ox_structfs_transport::ServerConfig::default(),
        },
        Some(four_role_factory(state.clone())),
        None,
    )
    .await
    .unwrap();
    let mut store = service.public_store.clone();

    // A parks on approval and keeps its own turn permit.
    let a = create_role(&mut store, "role-a", "ROLE_A request approval").await;
    let a_pending = Path::parse(&format!("conversations/{a}/approvals/pending")).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pending = store.read(&a_pending).await.unwrap().unwrap();
            if pending.as_value() != Some(&Value::Null) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("A must park on approval");

    // B has emitted streaming events, while C and D remain in independent
    // long turns. These blocking transports model model/tool waits without a
    // second executor or a process-per-conversation test double.
    let b = create_role(&mut store, "role-b", "ROLE_B stream").await;
    wait_flag(&state.b_streaming, "B").await;
    let c = create_role(&mut store, "role-c", "ROLE_C long tool-equivalent turn").await;
    wait_flag(&state.c_active, "C").await;
    let d = create_role(&mut store, "role-d", "ROLE_D cancel while active").await;
    wait_flag(&state.d_active, "D").await;

    let capacity = tokio::time::timeout(Duration::from_millis(250), store.read(&path!("capacity")))
        .await
        .expect("capacity must bypass four active turns")
        .unwrap()
        .unwrap();
    let capacity = structfs_serde_store::value_to_json(capacity.as_value().unwrap().clone());
    assert_eq!(capacity["active_turns"], 4);
    assert_eq!(capacity["resident_threads"], 4);
    for id in [&a, &b, &c, &d] {
        let status = Path::parse(&format!("conversations/{id}")).unwrap();
        tokio::time::timeout(Duration::from_millis(250), store.read(&status))
            .await
            .expect("status must bypass unrelated turns")
            .unwrap();
    }

    cancel_role(&mut store, &d, "cancel-role-d").await;
    state.release_d.store(true, Ordering::Release);
    wait_terminal_log(&root, &d).await;

    // B can finish while A remains parked and C remains blocked.
    state.release_b.store(true, Ordering::Release);
    wait_terminal_log(&root, &b).await;
    assert!(state.c_active.load(Ordering::Acquire));

    state.release_c.store(true, Ordering::Release);
    wait_terminal_log(&root, &c).await;
    cancel_role(&mut store, &a, "cancel-role-a").await;
    wait_terminal_log(&root, &a).await;
    service.shutdown().await.unwrap();
}

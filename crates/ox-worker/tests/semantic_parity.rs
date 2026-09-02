#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use ox_broker::async_store::{AsyncReader, AsyncWriter};
use ox_executor::test_support::{FakeTransport, factory_for};
use ox_executor::{ExecutionCore, ExecutorConfig, PolicyProfile, ThreadExecutionConfig};
use ox_inbox::thread_dir::ContextFile;
use ox_inbox::worker_ingress::CreateEnvelope;
use ox_kernel::log::LogEntry;
use ox_worker::{WorkerConfig, WorkerLimits, WorkerService};
use structfs_core_store::{Path as StorePath, Record, path};

const PROMPT: &str = "parity prompt";
const RESPONSE: &str = "the same semantic response";

fn private_tempdir() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    temp
}

fn script() -> FakeTransport {
    let transport = FakeTransport::new();
    transport
        .push_turn(vec![
            ox_kernel::StreamEvent::TextDelta {
                text: RESPONSE.into(),
            },
            ox_kernel::StreamEvent::MessageStop,
        ])
        .with_token_usage(7, 5);
    transport
}

fn approval_script() -> FakeTransport {
    let transport = FakeTransport::new();
    transport.push_turn(vec![
        ox_kernel::StreamEvent::ToolUseStart {
            id: "parity-tool".into(),
            name: "shell".into(),
        },
        ox_kernel::StreamEvent::ToolUseInputDelta {
            delta: serde_json::json!({"command": "printf should-not-run"}).to_string(),
        },
        ox_kernel::StreamEvent::MessageStop,
    ]);
    transport.push_turn(vec![
        ox_kernel::StreamEvent::TextDelta {
            text: "tool denial observed".into(),
        },
        ox_kernel::StreamEvent::MessageStop,
    ]);
    transport
}

async fn direct_core(root: &Path) -> (String, Vec<tokio::task::JoinHandle<()>>) {
    let broker = ox_broker::BrokerStore::default();
    let mounts = ox_executor::mount_execution_stores(
        &broker,
        ox_inbox::InboxStore::open(root).unwrap(),
        root.to_path_buf(),
        ox_store_util::LocalConfig::new(),
        ox_store_util::LocalConfig::new(),
    )
    .await;
    let transport = script();
    let mut core = ExecutionCore::new_with_config_and_test_hooks(
        root.join("workspaces"),
        false,
        ox_inbox::InboxStore::open(root).unwrap(),
        root.to_path_buf(),
        broker,
        tokio::runtime::Handle::current(),
        ExecutorConfig::remote(4).unwrap(),
        Some(factory_for(transport)),
        None,
    )
    .unwrap();
    let execution = ThreadExecutionConfig::new(
        root.join("workspaces/direct"),
        PolicyProfile::RemoteEnforced,
    );
    let thread_id = core
        .create_thread_with_config("semantic parity", execution.clone())
        .unwrap();
    core.send_prompt_with_config(&thread_id, PROMPT.into(), execution)
        .unwrap();
    wait_for_turn(root, &thread_id).await;
    drop(core);
    (thread_id, mounts)
}

async fn worker_adapter(root: &Path) -> (String, WorkerService) {
    let transport = script();
    let service = WorkerService::start_in_process_with_test_hooks(
        WorkerConfig {
            inbox_root: root.to_path_buf(),
            socket_path: root.join("unused.sock"),
            node_id: "parity-node".into(),
            attempt_id: "parity-attempt".into(),
            command_capacity: 16,
            limits: WorkerLimits::default(),
            transport: ox_structfs_transport::ServerConfig::default(),
        },
        Some(factory_for(transport)),
        None,
    )
    .await
    .unwrap();
    let mut store = service.public_store.clone();
    let result = store
        .write(
            &path!("conversations"),
            Record::parsed(
                structfs_serde_store::to_value(&CreateEnvelope {
                    create_id: "parity-create".into(),
                    title: "semantic parity".into(),
                    prompt: PROMPT.into(),
                    parent_id: None,
                })
                .unwrap(),
            ),
        )
        .await
        .unwrap();
    let thread_id = result.iter().nth(1).unwrap().clone();
    wait_for_turn(root, &thread_id).await;
    (thread_id, service)
}

async fn wait_for_turn(root: &Path, thread_id: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let entries = ledger_entries(root, thread_id);
            if entries
                .iter()
                .any(|entry| matches!(entry, LogEntry::TurnEnd { .. }))
                && root
                    .join("threads")
                    .join(thread_id)
                    .join("context.json")
                    .exists()
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("turn and context snapshot must complete");
}

fn ledger_entries(root: &Path, thread_id: &str) -> Vec<LogEntry> {
    let ledger = root.join("threads").join(thread_id).join("ledger.jsonl");
    if !ledger.exists() {
        return Vec::new();
    }
    ox_inbox::ledger::read_ledger(&ledger)
        .unwrap()
        .into_iter()
        .map(|entry| serde_json::from_value(entry.msg).unwrap())
        .collect()
}

fn semantic_ledger(root: &Path, thread_id: &str) -> Vec<serde_json::Value> {
    ledger_entries(root, thread_id)
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry,
                LogEntry::Meta { data }
                    if data.get("kind").and_then(serde_json::Value::as_str)
                        == Some("worker_ingress")
            )
        })
        .map(|entry| serde_json::to_value(entry).unwrap())
        .collect()
}

fn context(root: &Path, thread_id: &str) -> ContextFile {
    ox_inbox::thread_dir::read_context(&root.join("threads").join(thread_id))
        .unwrap()
        .unwrap()
}

async fn wait_direct_approval(broker: &ox_broker::BrokerStore, thread_id: &str) {
    let client = broker.client().scoped(&format!("threads/{thread_id}"));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pending = client.read(&path!("approval/pending")).await.unwrap();
            if pending.as_ref().and_then(Record::as_value)
                != Some(&structfs_core_store::Value::Null)
                && pending.is_some()
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("direct core must request approval");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_execution_core_and_public_worker_have_semantic_parity() {
    let direct = private_tempdir();
    let remote = private_tempdir();
    let (direct_id, direct_mounts) = direct_core(direct.path()).await;
    let (remote_id, service) = worker_adapter(remote.path()).await;

    assert_eq!(
        semantic_ledger(direct.path(), &direct_id),
        semantic_ledger(remote.path(), &remote_id),
        "the public adapter may add an idempotency marker, but must not change executor semantics"
    );
    let direct_context = context(direct.path(), &direct_id);
    let remote_context = context(remote.path(), &remote_id);
    assert_eq!(direct_context.version, remote_context.version);
    assert_eq!(direct_context.title, remote_context.title);
    assert_eq!(direct_context.labels, remote_context.labels);
    assert_eq!(direct_context.stores, remote_context.stores);

    service.shutdown().await.unwrap();
    drop(direct_mounts);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approval_and_denied_tool_result_have_adapter_parity() {
    let direct = private_tempdir();
    let direct_broker = ox_broker::BrokerStore::default();
    let direct_mounts = ox_executor::mount_execution_stores(
        &direct_broker,
        ox_inbox::InboxStore::open(direct.path()).unwrap(),
        direct.path().to_path_buf(),
        ox_store_util::LocalConfig::new(),
        ox_store_util::LocalConfig::new(),
    )
    .await;
    let mut direct_core = ExecutionCore::new_with_config_and_test_hooks(
        direct.path().join("workspaces"),
        false,
        ox_inbox::InboxStore::open(direct.path()).unwrap(),
        direct.path().to_path_buf(),
        direct_broker.clone(),
        tokio::runtime::Handle::current(),
        ExecutorConfig::remote(2).unwrap(),
        Some(factory_for(approval_script())),
        None,
    )
    .unwrap();
    let direct_execution = ThreadExecutionConfig::new(
        direct.path().join("workspaces/direct-approval"),
        PolicyProfile::RemoteEnforced,
    );
    let direct_id = direct_core
        .create_thread_with_config("approval parity", direct_execution.clone())
        .unwrap();
    direct_core
        .send_prompt_with_config(&direct_id, "request a tool".into(), direct_execution)
        .unwrap();
    wait_direct_approval(&direct_broker, &direct_id).await;
    direct_broker
        .client()
        .scoped(&format!("threads/{direct_id}"))
        .write_typed(
            &path!("approval/response"),
            &ox_types::ApprovalResponse {
                decision: ox_types::Decision::DenyOnce,
            },
        )
        .await
        .unwrap();
    wait_for_turn(direct.path(), &direct_id).await;

    let remote = private_tempdir();
    let remote_root = remote.path().join("inbox");
    let service = WorkerService::start_in_process_with_test_hooks(
        WorkerConfig {
            inbox_root: remote_root.clone(),
            socket_path: remote.path().join("unused.sock"),
            node_id: "approval-node".into(),
            attempt_id: "approval-attempt".into(),
            command_capacity: 16,
            limits: WorkerLimits::default(),
            transport: ox_structfs_transport::ServerConfig::default(),
        },
        Some(factory_for(approval_script())),
        None,
    )
    .await
    .unwrap();
    let mut store = service.public_store.clone();
    let created = store
        .write(
            &path!("conversations"),
            Record::parsed(
                structfs_serde_store::to_value(&CreateEnvelope {
                    create_id: "approval-parity-create".into(),
                    title: "approval parity".into(),
                    prompt: "request a tool".into(),
                    parent_id: None,
                })
                .unwrap(),
            ),
        )
        .await
        .unwrap();
    let remote_id = created.iter().nth(1).unwrap().clone();
    let pending_path =
        StorePath::parse(&format!("conversations/{remote_id}/approvals/pending")).unwrap();
    let approval_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pending = store.read(&pending_path).await.unwrap().unwrap();
            let pending = structfs_serde_store::value_to_json(pending.as_value().unwrap().clone());
            if let Some(id) = pending["approval_id"].as_str() {
                return id.to_string();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("public adapter must project approval");
    let response_path = StorePath::parse(&format!(
        "conversations/{remote_id}/approvals/{approval_id}"
    ))
    .unwrap();
    store
        .write(
            &response_path,
            Record::parsed(
                structfs_serde_store::to_value(&ox_types::ApprovalResponse {
                    decision: ox_types::Decision::DenyOnce,
                })
                .unwrap(),
            ),
        )
        .await
        .unwrap();
    wait_for_turn(&remote_root, &remote_id).await;

    let direct_entries = semantic_ledger(direct.path(), &direct_id);
    let remote_entries = semantic_ledger(&remote_root, &remote_id);
    assert_eq!(direct_entries, remote_entries);
    assert!(
        direct_entries
            .iter()
            .any(|entry| entry["type"] == "tool_result")
    );
    assert!(
        direct_entries
            .iter()
            .any(|entry| entry["type"] == "approval_resolved")
    );

    service.shutdown().await.unwrap();
    drop(direct_core);
    drop(direct_mounts);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_records_and_thread_ledgers_do_not_cross_contaminate_canaries() {
    let temp = private_tempdir();
    let root = temp.path().join("inbox");
    let transport = FakeTransport::new();
    transport.push_turn(vec![ox_kernel::StreamEvent::MessageStop]);
    transport.push_turn(vec![ox_kernel::StreamEvent::MessageStop]);
    let service = WorkerService::start_in_process_with_test_hooks(
        WorkerConfig {
            inbox_root: root.clone(),
            socket_path: temp.path().join("unused.sock"),
            node_id: "leak-node".into(),
            attempt_id: "leak-attempt".into(),
            command_capacity: 16,
            limits: WorkerLimits::default(),
            transport: ox_structfs_transport::ServerConfig::default(),
        },
        Some(factory_for(transport)),
        None,
    )
    .await
    .unwrap();
    let mut store = service.public_store.clone();
    let mut ids = Vec::new();
    for (create_id, title, canary) in [
        ("leak-create-a", "thread-a", "CANARY_A_31b8c3"),
        ("leak-create-b", "thread-b", "CANARY_B_a789d2"),
    ] {
        let result = store
            .write(
                &path!("conversations"),
                Record::parsed(
                    structfs_serde_store::to_value(&CreateEnvelope {
                        create_id: create_id.into(),
                        title: title.into(),
                        prompt: canary.into(),
                        parent_id: None,
                    })
                    .unwrap(),
                ),
            )
            .await
            .unwrap();
        ids.push(result.iter().nth(1).unwrap().clone());
    }
    for id in &ids {
        wait_for_turn(&root, id).await;
    }

    let ledger_a =
        std::fs::read_to_string(root.join("threads").join(&ids[0]).join("ledger.jsonl")).unwrap();
    let ledger_b =
        std::fs::read_to_string(root.join("threads").join(&ids[1]).join("ledger.jsonl")).unwrap();
    assert!(ledger_a.contains("CANARY_A_31b8c3"));
    assert!(!ledger_a.contains("CANARY_B_a789d2"));
    assert!(ledger_b.contains("CANARY_B_a789d2"));
    assert!(!ledger_b.contains("CANARY_A_31b8c3"));

    for public_path in ["health", "capabilities", "capacity", "conversations"] {
        let record = store
            .read(&StorePath::parse(public_path).unwrap())
            .await
            .unwrap();
        let encoded = format!("{record:?}");
        assert!(!encoded.contains("CANARY_A_31b8c3"), "{public_path}");
        assert!(!encoded.contains("CANARY_B_a789d2"), "{public_path}");
    }

    service.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ledger_cursor_and_result_project_the_bounded_durable_chain() {
    let temp = private_tempdir();
    let root = temp.path().join("inbox");
    let service = WorkerService::start_in_process_with_test_hooks(
        WorkerConfig {
            inbox_root: root.clone(),
            socket_path: temp.path().join("unused.sock"),
            node_id: "cursor-node".into(),
            attempt_id: "cursor-attempt".into(),
            command_capacity: 8,
            limits: WorkerLimits {
                max_ledger_batch_entries: 2,
                ..WorkerLimits::default()
            },
            transport: ox_structfs_transport::ServerConfig::default(),
        },
        Some(factory_for(script())),
        None,
    )
    .await
    .unwrap();
    let mut store = service.public_store.clone();
    let created = store
        .write(
            &path!("conversations"),
            Record::parsed(
                structfs_serde_store::to_value(&CreateEnvelope {
                    create_id: "cursor-create".into(),
                    title: "cursor".into(),
                    prompt: PROMPT.into(),
                    parent_id: None,
                })
                .unwrap(),
            ),
        )
        .await
        .unwrap();
    let thread_id = created.iter().nth(1).unwrap().clone();
    wait_for_turn(&root, &thread_id).await;

    let first_path = StorePath::parse(&format!("conversations/{thread_id}/ledger/from/0")).unwrap();
    let first = store.read(&first_path).await.unwrap().unwrap();
    let first = structfs_serde_store::value_to_json(first.as_value().unwrap().clone());
    assert_eq!(first["entries"].as_array().unwrap().len(), 2);
    assert_eq!(first["next_seq"], 2);
    assert_eq!(first["has_more"], true);

    let second_path =
        StorePath::parse(&format!("conversations/{thread_id}/ledger/from/2")).unwrap();
    let second = store.read(&second_path).await.unwrap().unwrap();
    let second = structfs_serde_store::value_to_json(second.as_value().unwrap().clone());
    assert_eq!(second["entries"][0]["seq"], 2);

    let result_path = StorePath::parse(&format!("conversations/{thread_id}/result")).unwrap();
    let result = store.read(&result_path).await.unwrap().unwrap();
    let result = structfs_serde_store::value_to_json(result.as_value().unwrap().clone());
    assert_eq!(result["projection"], "durable_ledger_tail");
    assert!(result["ledger_tail"].as_array().unwrap().len() <= 2);
    assert!(result["next_seq"].as_u64().unwrap() > 2);

    let invalid =
        StorePath::parse(&format!("conversations/{thread_id}/ledger/from/notseq")).unwrap();
    assert!(
        store
            .read(&invalid)
            .await
            .unwrap_err()
            .to_string()
            .contains("invalid sequence")
    );
    let missing = StorePath::parse("conversations/t_missing/ledger/from/0").unwrap();
    assert!(store.read(&missing).await.unwrap().is_none());
    let missing_result = StorePath::parse("conversations/t_missing/result").unwrap();
    assert!(store.read(&missing_result).await.unwrap().is_none());

    service.shutdown().await.unwrap();
}

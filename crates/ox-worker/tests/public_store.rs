#![cfg(unix)]

use std::{os::unix::fs::PermissionsExt, time::Duration};

use ox_broker::async_store::{AsyncReader, AsyncWriter};
use ox_executor::test_support::{FakeTransport, factory_for};
use ox_inbox::worker_ingress::{CancelEnvelope, CreateEnvelope, PromptEnvelope};
use ox_worker::{WorkerConfig, WorkerLimits, WorkerService};
use structfs_core_store::{Path, Record, Value, path};

fn record<T: serde::Serialize>(value: &T) -> Record {
    Record::parsed(structfs_serde_store::to_value(value).unwrap())
}

fn private_tempdir() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    temp
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

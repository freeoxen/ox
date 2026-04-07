//! Thread mount/unmount — manages per-thread stores in the broker.
//!
//! Each agent thread needs five stores mounted at `threads/{thread_id}/{store}`:
//! system, history, tools, model, gate. This module provides functions to
//! mount them from a [`ThreadConfig`], unmount them, and restore prior state
//! from the inbox snapshot files.

use ox_broker::BrokerStore;
use ox_context::{ModelProvider, SystemProvider, ToolsProvider};
use ox_gate::GateStore;
use ox_history::HistoryProvider;
use ox_inbox::snapshot;
use structfs_core_store::{Record, Value, Writer, path};
use tokio::task::JoinHandle;

/// Configuration for mounting a thread's stores.
pub struct ThreadConfig {
    pub system_prompt: String,
    pub model: String,
    pub max_tokens: u32,
    pub tool_schemas: Vec<ox_kernel::ToolSchema>,
    pub provider: String,
    pub api_key: String,
}

/// Handles returned from mounting a thread — keep alive to keep servers running.
#[allow(dead_code)]
pub struct ThreadMountHandles {
    pub server_handles: Vec<JoinHandle<()>>,
    pub thread_id: String,
}

/// Mount all five stores for a thread at `threads/{thread_id}/{store}`.
///
/// The stores are: system, history, tools, model, gate. After mounting,
/// the GateStore's bootstrap account is configured with the provided
/// API key and model.
pub async fn mount_thread(
    broker: &BrokerStore,
    thread_id: &str,
    config: ThreadConfig,
) -> Result<ThreadMountHandles, String> {
    let prefix = format!("threads/{thread_id}");
    let mut handles = Vec::new();

    // System
    let path =
        structfs_core_store::Path::parse(&format!("{prefix}/system")).map_err(|e| e.to_string())?;
    handles.push(
        broker
            .mount(path, SystemProvider::new(config.system_prompt))
            .await,
    );

    // History
    let path = structfs_core_store::Path::parse(&format!("{prefix}/history"))
        .map_err(|e| e.to_string())?;
    handles.push(broker.mount(path, HistoryProvider::new()).await);

    // Tools
    let path =
        structfs_core_store::Path::parse(&format!("{prefix}/tools")).map_err(|e| e.to_string())?;
    handles.push(
        broker
            .mount(path, ToolsProvider::new(config.tool_schemas))
            .await,
    );

    // Model
    let path =
        structfs_core_store::Path::parse(&format!("{prefix}/model")).map_err(|e| e.to_string())?;
    handles.push(
        broker
            .mount(
                path,
                ModelProvider::new(config.model.clone(), config.max_tokens),
            )
            .await,
    );

    // Gate — configured with provider + API key
    let gate_path =
        structfs_core_store::Path::parse(&format!("{prefix}/gate")).map_err(|e| e.to_string())?;
    let mut gate = GateStore::new();
    // Set the bootstrap account to the configured provider
    gate.write(
        &path!("bootstrap"),
        Record::parsed(Value::String(config.provider.clone())),
    )
    .map_err(|e: structfs_core_store::Error| e.to_string())?;
    // Set the API key on the provider's account
    let key_path = structfs_core_store::Path::parse(&format!("accounts/{}/key", config.provider))
        .map_err(|e| e.to_string())?;
    gate.write(&key_path, Record::parsed(Value::String(config.api_key)))
        .map_err(|e: structfs_core_store::Error| e.to_string())?;
    // Set the model on the provider's account
    let model_path =
        structfs_core_store::Path::parse(&format!("accounts/{}/model", config.provider))
            .map_err(|e| e.to_string())?;
    gate.write(&model_path, Record::parsed(Value::String(config.model)))
        .map_err(|e: structfs_core_store::Error| e.to_string())?;
    handles.push(broker.mount(gate_path, gate).await);

    Ok(ThreadMountHandles {
        server_handles: handles,
        thread_id: thread_id.to_string(),
    })
}

/// Unmount all five stores for a thread.
pub async fn unmount_thread(broker: &BrokerStore, thread_id: &str) {
    let prefix = format!("threads/{thread_id}");
    for store_name in &["system", "history", "tools", "model", "gate"] {
        if let Ok(path) = structfs_core_store::Path::parse(&format!("{prefix}/{store_name}")) {
            broker.unmount(&path).await;
        }
    }
}

/// Restore thread state from inbox snapshot files (context.json + ledger.jsonl).
///
/// The adapter should be scoped to the thread's prefix (`threads/{thread_id}`)
/// so that snapshot::restore can write to `system/snapshot/state`, `history/append`,
/// etc. directly.
///
/// Returns Ok(()) if no context.json exists (fresh thread with no prior state).
pub fn restore_thread_state(
    adapter: &mut ox_broker::SyncClientAdapter,
    inbox_root: &std::path::Path,
    thread_id: &str,
) -> Result<(), String> {
    let thread_dir = inbox_root.join("threads").join(thread_id);
    if !thread_dir.join("context.json").exists() {
        return Ok(());
    }
    snapshot::restore(adapter, &thread_dir, &snapshot::PARTICIPATING_MOUNTS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_broker::SyncClientAdapter;
    use structfs_core_store::{Reader, Value, Writer, path};
    use structfs_serde_store::json_to_value;

    fn test_config() -> ThreadConfig {
        ThreadConfig {
            system_prompt: "You are a test assistant.".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            max_tokens: 4096,
            tool_schemas: vec![],
            provider: "anthropic".to_string(),
            api_key: "sk-test-key".to_string(),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mount_thread_creates_all_stores() {
        let broker = BrokerStore::default();
        let _handles = mount_thread(&broker, "t_test1", test_config())
            .await
            .unwrap();

        let client = broker.client();

        // System prompt is readable
        let record = client
            .read(&path!("threads/t_test1/system"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            record.as_value().unwrap(),
            &Value::String("You are a test assistant.".to_string()),
        );

        // Model ID is readable
        let record = client
            .read(&path!("threads/t_test1/model/id"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            record.as_value().unwrap(),
            &Value::String("claude-sonnet-4-20250514".to_string()),
        );

        // Max tokens is readable
        let record = client
            .read(&path!("threads/t_test1/model/max_tokens"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.as_value().unwrap(), &Value::Integer(4096));

        // Gate bootstrap is set to the configured provider
        let record = client
            .read(&path!("threads/t_test1/gate/bootstrap"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            record.as_value().unwrap(),
            &Value::String("anthropic".to_string()),
        );

        // Gate API key is set
        let record = client
            .read(&path!("threads/t_test1/gate/accounts/anthropic/key"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            record.as_value().unwrap(),
            &Value::String("sk-test-key".to_string()),
        );

        // History starts empty
        let record = client
            .read(&path!("threads/t_test1/history/count"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.as_value().unwrap(), &Value::Integer(0));

        // Tools schemas readable (empty)
        let record = client
            .read(&path!("threads/t_test1/tools/schemas"))
            .await
            .unwrap()
            .unwrap();
        match record.as_value().unwrap() {
            Value::Array(a) => assert!(a.is_empty()),
            _ => panic!("expected array"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scoped_client_reads_thread_stores() {
        let broker = BrokerStore::default();
        let _handles = mount_thread(&broker, "t_scoped", test_config())
            .await
            .unwrap();

        let scoped = broker.client().scoped("threads/t_scoped");

        // Read system prompt through scoped client
        let record = scoped.read(&path!("system")).await.unwrap().unwrap();
        assert_eq!(
            record.as_value().unwrap(),
            &Value::String("You are a test assistant.".to_string()),
        );

        // Write to history through scoped client
        let msg = serde_json::json!({"role": "user", "content": "hello"});
        scoped
            .write(
                &path!("history/append"),
                structfs_core_store::Record::parsed(json_to_value(msg)),
            )
            .await
            .unwrap();

        // Verify message count
        let record = scoped.read(&path!("history/count")).await.unwrap().unwrap();
        assert_eq!(record.as_value().unwrap(), &Value::Integer(1));

        // Verify through unscoped client
        let full = broker.client();
        let record = full
            .read(&path!("threads/t_scoped/history/count"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.as_value().unwrap(), &Value::Integer(1));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unmount_thread_removes_all_stores() {
        let broker = BrokerStore::default();
        let _handles = mount_thread(&broker, "t_unmount", test_config())
            .await
            .unwrap();

        // Verify mounted
        let client = broker.client();
        assert!(
            client
                .read(&path!("threads/t_unmount/system"))
                .await
                .is_ok()
        );

        // Unmount
        unmount_thread(&broker, "t_unmount").await;

        // All five stores should be gone (NoRoute)
        for store_name in &["system", "history", "tools", "model", "gate"] {
            let path = structfs_core_store::Path::parse(&format!("threads/t_unmount/{store_name}"))
                .unwrap();
            let result = client.read(&path).await;
            assert!(result.is_err(), "expected NoRoute for {store_name}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_adapter_with_mounted_thread() {
        let broker = BrokerStore::default();
        let _handles = mount_thread(&broker, "t_sync", test_config())
            .await
            .unwrap();

        let scoped = broker.client().scoped("threads/t_sync");
        let rt_handle = tokio::runtime::Handle::current();

        // Run SyncClientAdapter in a blocking thread (not on the tokio runtime)
        let result = tokio::task::spawn_blocking(move || {
            let mut adapter = SyncClientAdapter::new(scoped, rt_handle);

            // Read system prompt
            let record = adapter.read(&path!("system")).unwrap().unwrap();
            assert_eq!(
                record.as_value().unwrap(),
                &Value::String("You are a test assistant.".to_string()),
            );

            // Read model
            let record = adapter.read(&path!("model/id")).unwrap().unwrap();
            assert_eq!(
                record.as_value().unwrap(),
                &Value::String("claude-sonnet-4-20250514".to_string()),
            );

            // Write to history
            let msg = serde_json::json!({"role": "user", "content": "sync hello"});
            adapter
                .write(
                    &path!("history/append"),
                    structfs_core_store::Record::parsed(json_to_value(msg)),
                )
                .unwrap();

            // Read history count
            let record = adapter.read(&path!("history/count")).unwrap().unwrap();
            assert_eq!(record.as_value().unwrap(), &Value::Integer(1));

            // Synthesize prompt through the adapter (exercises full namespace read)
            let prompt_result = ox_context::synthesize_prompt(&mut adapter);
            assert!(prompt_result.is_ok(), "prompt synthesis should succeed");
            let prompt_record = prompt_result.unwrap().unwrap();
            let value = prompt_record.as_value().unwrap().clone();
            let json = structfs_serde_store::value_to_json(value);
            let request: ox_kernel::CompletionRequest = serde_json::from_value(json).unwrap();
            assert_eq!(request.model, "claude-sonnet-4-20250514");
            assert_eq!(request.system, "You are a test assistant.");
            assert_eq!(request.messages.len(), 1);
        })
        .await;

        result.expect("spawn_blocking task should succeed");
    }
}

//! Policy-refusal contract: when an agent worker starts in a workspace
//! with a malformed `.clash/policy.json`, it MUST:
//!
//! 1. Refuse to start (not silently fall back to permissive defaults —
//!    that would be a security regression for users who explicitly
//!    wrote a restrictive policy).
//! 2. Surface the failure where the user is already looking — by
//!    writing a synthesized assistant turn to the thread's history
//!    explaining what failed and how to fix.
//!
//! The unit test `policy::tests::malformed_policy_file_surfaces_parse_error`
//! pins (1) — that `PolicyGuard::load` returns Err on a malformed
//! policy file. This integration test pins (2): the agents.rs caller
//! actually surfaces the refusal in a place the user can see.

use std::time::Duration;

use ox_broker::ClientHandle;
use ox_kernel::PathComponent;
use ox_path::oxpath;
use structfs_core_store::Value;

use ox_cli::app::App;
use ox_cli::bindings::default_bindings;
use ox_cli::broker_setup;

/// Poll the thread's history-log path for an assistant message whose
/// text contains `Agent failed to start`. Bounded at 3s — the worker
/// writes synchronously before exiting, so this should land quickly.
async fn wait_for_refusal_in_history(client: &ClientHandle, thread_id: &str) -> Option<String> {
    let tid = PathComponent::try_new(thread_id).expect("valid thread id");
    let history_path = oxpath!("threads", tid, "history", "messages");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if let Ok(Some(rec)) = client.read(&history_path).await {
            if let Some(Value::Array(entries)) = rec.as_value() {
                for entry in entries {
                    let Value::Map(map) = entry else { continue };
                    let Some(Value::Array(content_arr)) = map.get("content") else {
                        continue;
                    };
                    let Some(Value::Map(first)) = content_arr.first() else {
                        continue;
                    };
                    let Some(Value::String(text)) = first.get("text") else {
                        continue;
                    };
                    if text.contains("Agent failed to start") {
                        return Some(text.clone());
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_policy_writes_failure_to_thread_history() {
    let workspace_dir = tempfile::tempdir().expect("workspace tempdir");
    let inbox_root_dir = tempfile::tempdir().expect("inbox tempdir");
    let workspace = workspace_dir.path().to_path_buf();
    let inbox_root = inbox_root_dir.path().to_path_buf();

    // Plant a malformed .clash/policy.json that the worker will refuse.
    let clash_dir = workspace.join(".clash");
    std::fs::create_dir_all(&clash_dir).expect("create .clash");
    std::fs::write(clash_dir.join("policy.json"), "{ not valid json")
        .expect("write malformed policy");

    // Set up the full broker the same way main.rs does.
    let inbox = ox_inbox::InboxStore::open(&inbox_root).expect("open inbox");
    let broker_handle = broker_setup::setup(
        inbox,
        default_bindings(),
        inbox_root.clone(),
        std::collections::BTreeMap::new(),
    )
    .await;
    let broker = broker_handle.broker.clone();
    let client = broker.client();

    // Construct the App with no_policy=false so the policy file is honored.
    let mut app = App::new_with_test_hooks(
        workspace,
        inbox_root,
        /* no_policy: */ false,
        broker,
        tokio::runtime::Handle::current(),
        /* transport_factory: */ None,
        /* tool_injector: */ None,
    )
    .expect("construct App");

    // do_compose creates the thread, spawns the worker (which fails
    // policy load and writes the failure to history), and tries to
    // send the prompt to a now-dead channel (which silently fails).
    let thread_id = app
        .send_input_with_text("hello".to_string(), ox_types::Mode::Normal, None, None)
        .await
        .expect("send_input_with_text must produce a thread id from compose");

    let refusal_text = wait_for_refusal_in_history(&client, &thread_id)
        .await
        .expect(
            "agent worker must write a synthesized 'Agent failed to start' \
             assistant turn to thread history when policy load fails; \
             user has no in-TUI indication otherwise",
        );

    // The refusal text must name what failed and suggest a fix —
    // not just "agent died." Both bits are user-actionable.
    assert!(
        refusal_text.contains("policy"),
        "refusal must mention 'policy' so the user can locate the fix; got: {refusal_text}",
    );
    assert!(
        refusal_text.contains("Fix") || refusal_text.contains("remove"),
        "refusal must suggest remediation; got: {refusal_text}",
    );
}

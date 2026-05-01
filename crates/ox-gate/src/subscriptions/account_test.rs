//! `AccountTestSubscription` — fires on writes to
//! `config/gate/accounts/{name}/test_now`.
//!
//! ## Lifecycle
//!
//! 1. Read the `AccountConfig`, `ProviderConfig`, and `ApiKey` from the
//!    snapshot. If anything's missing, return an empty write set —
//!    handlers are total over the input.
//! 2. Run `validate_account`. On error, write `validation` and
//!    `test_status: Failed { reason: "validation failed" }`. Don't spawn.
//! 3. Synchronously write `test_status: Testing { started_at_ms }` so
//!    the UI can pick up the in-flight state on the very next snapshot.
//! 4. Abort any prior in-flight task for the same account (supersession).
//! 5. Spawn a future that calls `Transport::test_connection`. On
//!    completion, the future writes `Success` or `Failed` back through
//!    the broker's `AsyncWriter`.
//!
//! Per spec §3.3 (subscription protocol), §4.6 (action paths), §6.4
//! (overlay action wires).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ox_broker::subscription::{Subscription, SubCtx};
use ox_path::oxpath;
use ox_types::subscription::{PathPattern, SubscriptionId, Write};
use structfs_core_store::Record;
use tokio::task::AbortHandle;

use crate::subscriptions::util::{
    account_path, instance_segment, now_ms, provider_path, read_typed_via_reader,
    secret_key_path, test_status_path, validation_path, write_typed,
};
use crate::transport::Transport;
use crate::validation::validate_account;
use crate::{AccountConfig, AccountTestStatus, ApiKey, ProviderConfig};

/// Subscription id constant — also the key passed to logging / telemetry.
pub const ID: &str = "gate.account_test";

/// Subscription that watches every account's `test_now` trigger and
/// orchestrates the test-connection lifecycle.
///
/// `in_flight` maps account name → the AbortHandle of the test task
/// currently running for that account. A second trigger before the
/// first completes aborts the prior task — the user's most recent
/// click wins. `Mutex<HashMap<...>>` (sync) is used because the
/// subscription's `handle` is a sync function; only the spawned future
/// itself is async.
pub struct AccountTestSubscription {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
    transport: Arc<dyn Transport>,
    in_flight: Mutex<HashMap<String, AbortHandle>>,
}

impl AccountTestSubscription {
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self {
            id: SubscriptionId(ID.to_string()),
            watches: vec![PathPattern::PrefixSuffix {
                prefix: oxpath!("config", "gate", "accounts"),
                suffix: oxpath!("test_now"),
            }],
            transport,
            in_flight: Mutex::new(HashMap::new()),
        }
    }
}

impl Subscription for AccountTestSubscription {
    fn id(&self) -> &SubscriptionId {
        &self.id
    }

    fn watches(&self) -> &[PathPattern] {
        &self.watches
    }

    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write> {
        let prefix = oxpath!("config", "gate", "accounts");
        let suffix = oxpath!("test_now");
        let Some(name) = instance_segment(&ctx.change.path, &prefix, &suffix) else {
            tracing::debug!(path = %ctx.change.path, "account_test: change path doesn't match prefix/suffix shape");
            return vec![];
        };

        let Ok(acct_path) = account_path(&name) else {
            return vec![];
        };
        let cfg: Option<AccountConfig> = read_typed_via_reader(ctx.snapshot, &acct_path);
        let Some(cfg) = cfg else {
            tracing::debug!(account = %name, "account_test: no AccountConfig at path");
            return vec![];
        };

        let provider: Option<ProviderConfig> = provider_path(&name)
            .ok()
            .and_then(|p| read_typed_via_reader(ctx.snapshot, &p));
        let key: Option<ApiKey> = secret_key_path(&name)
            .ok()
            .and_then(|p| read_typed_via_reader(ctx.snapshot, &p));

        // -- Validation gate -------------------------------------------------
        if let Some(diag) =
            validate_account(&cfg, provider.as_ref(), key.as_ref().map(|k| k.expose()))
        {
            let mut writes = Vec::new();
            if let Ok(vp) = validation_path(&name) {
                writes.push(write_typed(&vp, &diag));
            }
            if let Ok(tsp) = test_status_path(&name) {
                writes.push(write_typed(
                    &tsp,
                    &AccountTestStatus::Failed {
                        reason: "validation failed".into(),
                        completed_at_ms: now_ms(),
                    },
                ));
            }
            return writes;
        }

        // Past validation — provider and key (when required) are present.
        // unwrap is safe because validate_account would have flagged a
        // missing provider as Endpoint error.
        let provider = match provider {
            Some(p) => p,
            None => return vec![],
        };
        let api_key = key
            .map(|k| k.expose().to_string())
            .unwrap_or_default();

        // -- Synchronous status flip ----------------------------------------
        let start = Instant::now();
        let started_at_ms = now_ms();
        let mut writes: Vec<Write> = Vec::new();
        if let Ok(tsp) = test_status_path(&name) {
            writes.push(write_typed(
                &tsp,
                &AccountTestStatus::Testing { started_at_ms },
            ));
        }

        // -- Supersession ---------------------------------------------------
        if let Some(prior) = self.in_flight.lock().unwrap().remove(&name) {
            tracing::debug!(account = %name, "account_test: aborting prior task");
            prior.abort();
        }

        // -- Spawn the network call -----------------------------------------
        let transport = self.transport.clone();
        let writer = ctx.writer.clone();
        let dialect = provider.dialect.clone();
        let name_for_task = name.clone();
        let handle = ctx.spawn.spawn(Box::pin(async move {
            let outcome = match transport
                .test_connection(&name_for_task, &provider, &api_key)
                .await
            {
                Ok((observed_dialect, latency)) => AccountTestStatus::Success {
                    // Prefer the dialect the server actually responded
                    // with — a misrouted endpoint surfaces here as a
                    // mismatch rather than silently agreeing.
                    dialect: if observed_dialect.is_empty() {
                        dialect
                    } else {
                        observed_dialect
                    },
                    latency_ms: u64::try_from(latency.min(u128::from(u64::MAX)))
                        .unwrap_or(start.elapsed().as_millis() as u64),
                    completed_at_ms: now_ms(),
                },
                Err(reason) => AccountTestStatus::Failed {
                    reason,
                    completed_at_ms: now_ms(),
                },
            };
            let path = match test_status_path(&name_for_task) {
                Ok(p) => p,
                Err(_) => return,
            };
            let value = match structfs_serde_store::to_value(&outcome) {
                Ok(v) => v,
                Err(_) => return,
            };
            let _ = writer.write(path, Record::parsed(value)).await;
        }));
        self.in_flight.lock().unwrap().insert(name, handle);

        writes
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use ox_broker::subscription::{AsyncWriter, Subscription, SubCtx};
    use ox_path::oxpath;
    use ox_types::subscription::PathChange;
    use structfs_core_store::{Path, Record};

    use super::*;
    use crate::subscriptions::util::testing::{
        populate_anthropic_account, CapturingWriter, InMemoryReader, MockTransport, TestSpawn,
    };

    fn trigger_path(name: &str) -> Path {
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        oxpath!("config", "gate", "accounts", comp, "test_now")
    }

    /// Drive a single subscription invocation against the given fixtures
    /// and return the synchronously-emitted writes plus the spawned
    /// task handles for inspection.
    async fn drive(
        sub: &AccountTestSubscription,
        reader: &mut InMemoryReader,
        spawn: &TestSpawn,
        writer: Arc<dyn AsyncWriter>,
        name: &str,
    ) -> Vec<Write> {
        let path = trigger_path(name);
        let change = PathChange {
            path: path.clone(),
            before: None,
            after: Some(Record::parsed(structfs_core_store::Value::Null)),
        };
        let ctx = SubCtx {
            snapshot: reader,
            change: &change,
            spawn,
            writer,
        };
        sub.handle(ctx)
    }

    /// Wait for every recorded spawn-task to terminate (succeeded or
    /// aborted). Polls each handle's `is_finished` rather than relying
    /// on JoinHandles, which we don't keep around.
    async fn wait_for_all(spawn: &TestSpawn) {
        for handle in spawn.handles() {
            for _ in 0..100 {
                if handle.is_finished() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        // One extra tick lets the writer's async write (which runs after
        // the future body finishes) finish recording.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn writes_validation_status_then_short_circuits_on_invalid_endpoint() {
        // Set up a provider whose endpoint will fail validate_endpoint.
        let mut reader = InMemoryReader::new();
        populate_anthropic_account(&mut reader, "alpha", "sk-key");
        // Overwrite the provider with an invalid endpoint.
        let bad = ProviderConfig {
            dialect: "anthropic".into(),
            endpoint: "no-scheme".into(),
            version: "2023-06-01".into(),
            auth: Some(crate::AuthScheme::XApiKey),
        };
        reader.set("config/gate/providers/alpha", &bad);

        let transport = Arc::new(MockTransport::new());
        let sub = AccountTestSubscription::new(transport.clone());
        let spawn = TestSpawn::new();
        let writer = Arc::new(CapturingWriter::new()) as Arc<dyn AsyncWriter>;

        let writes = drive(&sub, &mut reader, &spawn, writer.clone(), "alpha").await;

        // Two synchronous writes: validation diag + test_status: Failed.
        assert_eq!(writes.len(), 2, "writes: {writes:?}");
        assert!(writes
            .iter()
            .any(|w| w.path.to_string() == "config/gate/accounts/alpha/validation"));
        assert!(writes
            .iter()
            .any(|w| w.path.to_string() == "config/gate/accounts/alpha/test_status"));

        // No spawn — validation short-circuits.
        assert!(spawn.handles().is_empty(), "validation must not spawn");
        // Mock transport must not be called.
        assert!(transport.test_calls.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn writes_testing_then_success_on_valid_response() {
        let mut reader = InMemoryReader::new();
        populate_anthropic_account(&mut reader, "alpha", "sk-key");

        let transport = Arc::new(
            MockTransport::new().with_test_result(Ok(("anthropic".into(), 87))),
        );
        let sub = AccountTestSubscription::new(transport.clone());
        let spawn = TestSpawn::new();
        let cap = CapturingWriter::new();
        let writer: Arc<dyn AsyncWriter> = Arc::new(cap.clone());

        let writes = drive(&sub, &mut reader, &spawn, writer, "alpha").await;
        assert_eq!(writes.len(), 1, "expected one synchronous write (Testing)");
        let testing_status: AccountTestStatus =
            structfs_serde_store::from_value(writes[0].record.as_value().unwrap().clone()).unwrap();
        assert!(matches!(testing_status, AccountTestStatus::Testing { .. }));

        wait_for_all(&spawn).await;

        // Spawned task wrote Success.
        let final_status: AccountTestStatus = cap
            .typed("config/gate/accounts/alpha/test_status")
            .expect("writer should have recorded the success status");
        match final_status {
            AccountTestStatus::Success {
                dialect,
                latency_ms,
                ..
            } => {
                assert_eq!(dialect, "anthropic");
                assert_eq!(latency_ms, 87);
            }
            other => panic!("expected Success, got {other:?}"),
        }
        assert_eq!(transport.test_calls.lock().unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn writes_testing_then_failed_on_transport_error() {
        let mut reader = InMemoryReader::new();
        populate_anthropic_account(&mut reader, "alpha", "sk-key");

        let transport = Arc::new(
            MockTransport::new().with_test_result(Err("HTTP 401".into())),
        );
        let sub = AccountTestSubscription::new(transport.clone());
        let spawn = TestSpawn::new();
        let cap = CapturingWriter::new();
        let writer: Arc<dyn AsyncWriter> = Arc::new(cap.clone());

        let _writes = drive(&sub, &mut reader, &spawn, writer, "alpha").await;
        wait_for_all(&spawn).await;

        let final_status: AccountTestStatus = cap
            .typed("config/gate/accounts/alpha/test_status")
            .expect("status not recorded");
        match final_status {
            AccountTestStatus::Failed { reason, .. } => assert_eq!(reason, "HTTP 401"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supersession_aborts_prior_task() {
        let mut reader = InMemoryReader::new();
        populate_anthropic_account(&mut reader, "alpha", "sk-key");

        // Slow transport: holds the response for ~150ms so the second
        // trigger lands while the first is still spawning.
        struct SlowTransport;
        #[async_trait::async_trait]
        impl Transport for SlowTransport {
            async fn test_connection(
                &self,
                _account: &str,
                _provider: &ProviderConfig,
                _api_key: &str,
            ) -> Result<(String, u128), String> {
                tokio::time::sleep(Duration::from_millis(150)).await;
                Ok(("anthropic".into(), 1))
            }
            async fn fetch_catalog(
                &self,
                _account: &str,
                _provider: &ProviderConfig,
                _api_key: &str,
            ) -> Result<Vec<crate::ModelInfo>, String> {
                Ok(vec![])
            }
        }

        let sub = AccountTestSubscription::new(Arc::new(SlowTransport));
        let spawn = TestSpawn::new();
        let writer = Arc::new(CapturingWriter::new()) as Arc<dyn AsyncWriter>;

        // First trigger.
        let _ = drive(&sub, &mut reader, &spawn, writer.clone(), "alpha").await;
        // Yield once so tokio actually picks up the spawned future.
        tokio::time::sleep(Duration::from_millis(10)).await;
        // Second trigger — must abort the first.
        let _ = drive(&sub, &mut reader, &spawn, writer, "alpha").await;

        let handles = spawn.handles();
        assert_eq!(handles.len(), 2);
        // Give the abort a moment to be observed.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            handles[0].is_finished() || handles[0].is_finished(),
            "prior task should have been aborted"
        );
        // The standard signal we want to assert: the prior handle was
        // explicitly aborted. AbortHandle exposes `is_finished` rather
        // than a dedicated `is_aborted`; once a task is aborted, it
        // resolves and `is_finished` flips.
        assert!(
            handles[0].is_finished(),
            "prior task should be finished after supersession",
        );
    }
}


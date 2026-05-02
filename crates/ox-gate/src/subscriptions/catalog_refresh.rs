//! `CatalogRefreshSubscription` — fires on writes to
//! `config/gate/accounts/{name}/refresh_now`.
//!
//! Same lifecycle shape as `AccountTestSubscription` (validate → in-flight
//! status → spawn → success/failure), but the spawned task fetches the
//! provider's model catalog and merges it with the known-family fallback
//! table so the saved `Vec<ModelInfo>` has token limits even when the
//! upstream API only ships ids and display names (Anthropic's
//! `/v1/models`).
//!
//! On success the subscription writes:
//! - `config/gate/accounts/{name}/models: Vec<ModelInfo>` — the new catalog
//! - `config/gate/accounts/{name}/refresh_status: Success { added, updated }`
//!
//! On failure it writes only the status; the previous models are left in
//! place so a transient network blip doesn't blank the account's catalog.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use ox_broker::subscription::{SubCtx, Subscription};
use ox_path::oxpath;
use ox_types::subscription::{PathPattern, SubscriptionId, Write};
use structfs_core_store::Record;
use tokio::task::AbortHandle;

use crate::known_family::known_family_metadata;
use crate::subscriptions::util::{
    account_path, instance_segment, models_path, now_ms, provider_path, read_typed_via_reader,
    refresh_status_path, secret_key_path, validation_path, write_typed,
};
use crate::transport::Transport;
use crate::validation::validate_account;
use crate::{
    AccountConfig, ApiKey, CatalogRefreshStatus, ModelInfo, ModelInfoSource, ProviderConfig,
};

pub const ID: &str = "gate.catalog_refresh";

pub struct CatalogRefreshSubscription {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
    transport: Arc<dyn Transport>,
    in_flight: Mutex<HashMap<String, AbortHandle>>,
}

impl CatalogRefreshSubscription {
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self {
            id: SubscriptionId(ID.to_string()),
            watches: vec![PathPattern::PrefixSuffix {
                prefix: oxpath!("config", "gate", "accounts"),
                suffix: oxpath!("refresh_now"),
            }],
            transport,
            in_flight: Mutex::new(HashMap::new()),
        }
    }
}

/// Merge a server-supplied catalog with the known-family fallback table.
///
/// For every model whose `max_context_size` or `max_output_tokens` is
/// missing, look up `(model.id, dialect)` in the family table. If a
/// match exists, fill in the missing fields and flip `source` to
/// `KnownTable`; otherwise leave the entry as-is with `source: Server`.
fn fill_known_family(mut models: Vec<ModelInfo>, dialect: &str) -> Vec<ModelInfo> {
    for m in &mut models {
        if m.max_context_size.is_some() && m.max_output_tokens.is_some() {
            continue;
        }
        if let Some(entry) = known_family_metadata(&m.id, dialect) {
            if m.max_context_size.is_none() {
                m.max_context_size = entry.max_context_size;
            }
            if m.max_output_tokens.is_none() {
                m.max_output_tokens = entry.max_output_tokens;
            }
            // Source flips to KnownTable when the table actually
            // contributed data — meaning *something* was filled in. If
            // the table also has both fields blank, leave the source
            // alone so we don't lie about provenance.
            if entry.max_context_size.is_some() || entry.max_output_tokens.is_some() {
                m.source = ModelInfoSource::KnownTable;
            }
        }
    }
    models
}

/// Diff a new catalog against the previous one: returns `(added, updated)`.
///
/// `added` counts model ids that didn't exist before. `updated` counts
/// ids that existed but whose `max_context_size`, `max_output_tokens`,
/// or `display_name` changed. The diff ignores `source` because that
/// field can flip without a meaningful change to the user-visible
/// metadata.
fn diff_catalog(new_models: &[ModelInfo], old_models: &[ModelInfo]) -> (u32, u32) {
    let old_by_id: HashMap<&str, &ModelInfo> =
        old_models.iter().map(|m| (m.id.as_str(), m)).collect();
    let mut added = 0u32;
    let mut updated = 0u32;
    let mut seen_old: HashSet<&str> = HashSet::new();
    for nm in new_models {
        match old_by_id.get(nm.id.as_str()) {
            None => added += 1,
            Some(om) => {
                seen_old.insert(om.id.as_str());
                if om.max_context_size != nm.max_context_size
                    || om.max_output_tokens != nm.max_output_tokens
                    || om.display_name != nm.display_name
                {
                    updated += 1;
                }
            }
        }
    }
    (added, updated)
}

impl Subscription for CatalogRefreshSubscription {
    fn id(&self) -> &SubscriptionId {
        &self.id
    }

    fn watches(&self) -> &[PathPattern] {
        &self.watches
    }

    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write> {
        let prefix = oxpath!("config", "gate", "accounts");
        let suffix = oxpath!("refresh_now");
        let Some(name) = instance_segment(&ctx.change.path, &prefix, &suffix) else {
            tracing::debug!(path = %ctx.change.path, "catalog_refresh: change path doesn't match");
            return vec![];
        };

        let Ok(acct_path) = account_path(&name) else {
            return vec![];
        };
        let cfg: Option<AccountConfig> = read_typed_via_reader(ctx.snapshot, &acct_path);
        let Some(cfg) = cfg else {
            return vec![];
        };

        let provider: Option<ProviderConfig> = provider_path(&name)
            .ok()
            .and_then(|p| read_typed_via_reader(ctx.snapshot, &p));
        let key: Option<ApiKey> = secret_key_path(&name)
            .ok()
            .and_then(|p| read_typed_via_reader(ctx.snapshot, &p));

        if let Some(diag) =
            validate_account(&cfg, provider.as_ref(), key.as_ref().map(|k| k.expose()))
        {
            let mut writes = Vec::new();
            if let Ok(vp) = validation_path(&name) {
                writes.push(write_typed(&vp, &diag));
            }
            if let Ok(rsp) = refresh_status_path(&name) {
                writes.push(write_typed(
                    &rsp,
                    &CatalogRefreshStatus::Failed {
                        reason: "validation failed".into(),
                        completed_at_ms: now_ms(),
                    },
                ));
            }
            return writes;
        }

        let provider = match provider {
            Some(p) => p,
            None => return vec![],
        };
        let api_key = key.map(|k| k.expose().to_string()).unwrap_or_default();

        // Read the current catalog so the diff in the spawned task can
        // count added vs updated. Do this synchronously while we still
        // have the snapshot reader.
        let old_models: Vec<ModelInfo> = models_path(&name)
            .ok()
            .and_then(|p| read_typed_via_reader::<Vec<ModelInfo>>(ctx.snapshot, &p))
            .unwrap_or_default();

        let started_at_ms = now_ms();
        let mut writes: Vec<Write> = Vec::new();
        if let Ok(rsp) = refresh_status_path(&name) {
            writes.push(write_typed(
                &rsp,
                &CatalogRefreshStatus::Refreshing { started_at_ms },
            ));
        }

        if let Some(prior) = self.in_flight.lock().unwrap().remove(&name) {
            tracing::debug!(account = %name, "catalog_refresh: aborting prior task");
            prior.abort();
        }

        let transport = self.transport.clone();
        let writer = ctx.writer.clone();
        let dialect = provider.dialect.clone();
        let name_for_task = name.clone();
        let handle = ctx.spawn.spawn(Box::pin(async move {
            match transport
                .fetch_catalog(&name_for_task, &provider, &api_key)
                .await
            {
                Ok(server_models) => {
                    let new_models = fill_known_family(server_models, &dialect);
                    let (added, updated) = diff_catalog(&new_models, &old_models);

                    // Write the catalog itself.
                    if let Ok(mp) = models_path(&name_for_task) {
                        if let Ok(v) = structfs_serde_store::to_value(&new_models) {
                            let _ = writer.write(mp, Record::parsed(v)).await;
                        }
                    }
                    // Then the success status.
                    if let Ok(rsp) = refresh_status_path(&name_for_task) {
                        let outcome = CatalogRefreshStatus::Success {
                            models_added: added,
                            models_updated: updated,
                            completed_at_ms: now_ms(),
                        };
                        if let Ok(v) = structfs_serde_store::to_value(&outcome) {
                            let _ = writer.write(rsp, Record::parsed(v)).await;
                        }
                    }
                }
                Err(reason) => {
                    // Failure: write status only. Don't clobber existing
                    // models — a transient blip should not blank the
                    // account's catalog.
                    if let Ok(rsp) = refresh_status_path(&name_for_task) {
                        let outcome = CatalogRefreshStatus::Failed {
                            reason,
                            completed_at_ms: now_ms(),
                        };
                        if let Ok(v) = structfs_serde_store::to_value(&outcome) {
                            let _ = writer.write(rsp, Record::parsed(v)).await;
                        }
                    }
                }
            }
        }));
        self.in_flight.lock().unwrap().insert(name, handle);

        writes
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use ox_broker::subscription::{AsyncWriter, SubCtx, Subscription};
    use ox_path::oxpath;
    use ox_types::subscription::PathChange;
    use structfs_core_store::{Path, Record, Value};

    use super::*;
    use crate::subscriptions::util::testing::{
        CapturingWriter, InMemoryReader, MockTransport, TestSpawn, populate_anthropic_account,
    };

    fn trigger_path(name: &str) -> Path {
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        oxpath!("config", "gate", "accounts", comp, "refresh_now")
    }

    async fn drive(
        sub: &CatalogRefreshSubscription,
        reader: &mut InMemoryReader,
        spawn: &TestSpawn,
        writer: Arc<dyn AsyncWriter>,
        name: &str,
    ) -> Vec<Write> {
        let path = trigger_path(name);
        let change = PathChange {
            path,
            before: None,
            after: Some(Record::parsed(Value::Null)),
        };
        let ctx = SubCtx {
            snapshot: reader,
            change: &change,
            spawn,
            writer,
        };
        sub.handle(ctx)
    }

    async fn wait_for_all(spawn: &TestSpawn) {
        for handle in spawn.handles() {
            for _ in 0..100 {
                if handle.is_finished() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    fn server_model(id: &str) -> ModelInfo {
        ModelInfo {
            id: id.to_string(),
            display_name: id.to_string(),
            max_context_size: None,
            max_output_tokens: None,
            source: ModelInfoSource::Server,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refresh_writes_models_to_account_path() {
        let mut reader = InMemoryReader::new();
        populate_anthropic_account(&mut reader, "alpha", "sk-key");

        let transport = Arc::new(
            MockTransport::new().with_catalog(Ok(vec![server_model("claude-haiku-4-5-20251001")])),
        );
        let sub = CatalogRefreshSubscription::new(transport);
        let spawn = TestSpawn::new();
        let cap = CapturingWriter::new();
        let writer: Arc<dyn AsyncWriter> = Arc::new(cap.clone());

        let _ = drive(&sub, &mut reader, &spawn, writer, "alpha").await;
        wait_for_all(&spawn).await;

        let saved: Vec<ModelInfo> = cap
            .typed("config/gate/accounts/alpha/models")
            .expect("models record not written");
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].id, "claude-haiku-4-5-20251001");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refresh_status_progresses_idle_refreshing_success() {
        let mut reader = InMemoryReader::new();
        populate_anthropic_account(&mut reader, "alpha", "sk-key");

        let transport = Arc::new(MockTransport::new().with_catalog(Ok(vec![])));
        let sub = CatalogRefreshSubscription::new(transport);
        let spawn = TestSpawn::new();
        let cap = CapturingWriter::new();
        let writer: Arc<dyn AsyncWriter> = Arc::new(cap.clone());

        let writes = drive(&sub, &mut reader, &spawn, writer, "alpha").await;
        // Synchronous side: Refreshing.
        assert_eq!(writes.len(), 1);
        let s: CatalogRefreshStatus =
            structfs_serde_store::from_value(writes[0].record.as_value().unwrap().clone()).unwrap();
        assert!(matches!(s, CatalogRefreshStatus::Refreshing { .. }));

        wait_for_all(&spawn).await;
        let s: CatalogRefreshStatus = cap
            .typed("config/gate/accounts/alpha/refresh_status")
            .expect("final status not recorded");
        assert!(matches!(s, CatalogRefreshStatus::Success { .. }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refresh_failure_does_not_clobber_existing_catalog() {
        // Pre-populate an existing catalog so we can assert the write
        // never reached `models`.
        let existing = vec![server_model("legacy-model")];
        let mut reader = InMemoryReader::new();
        populate_anthropic_account(&mut reader, "alpha", "sk-key");
        reader.set("config/gate/accounts/alpha/models", &existing);

        let transport = Arc::new(MockTransport::new().with_catalog(Err("network down".into())));
        let sub = CatalogRefreshSubscription::new(transport);
        let spawn = TestSpawn::new();
        let cap = CapturingWriter::new();
        let writer: Arc<dyn AsyncWriter> = Arc::new(cap.clone());

        let _ = drive(&sub, &mut reader, &spawn, writer, "alpha").await;
        wait_for_all(&spawn).await;

        // Status is Failed.
        let s: CatalogRefreshStatus = cap
            .typed("config/gate/accounts/alpha/refresh_status")
            .expect("status not recorded");
        match s {
            CatalogRefreshStatus::Failed { reason, .. } => assert_eq!(reason, "network down"),
            other => panic!("expected Failed, got {other:?}"),
        }
        // Models path not touched on failure.
        assert!(
            cap.get("config/gate/accounts/alpha/models").is_none(),
            "models must not be overwritten on failure"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refresh_supersession_aborts_prior_task() {
        struct SlowTransport;
        #[async_trait::async_trait]
        impl Transport for SlowTransport {
            async fn test_connection(
                &self,
                _account: &str,
                _provider: &ProviderConfig,
                _api_key: &str,
            ) -> Result<(String, u128), String> {
                Ok(("anthropic".into(), 1))
            }
            async fn fetch_catalog(
                &self,
                _account: &str,
                _provider: &ProviderConfig,
                _api_key: &str,
            ) -> Result<Vec<ModelInfo>, String> {
                tokio::time::sleep(Duration::from_millis(150)).await;
                Ok(vec![])
            }
        }

        let mut reader = InMemoryReader::new();
        populate_anthropic_account(&mut reader, "alpha", "sk-key");

        let sub = CatalogRefreshSubscription::new(Arc::new(SlowTransport));
        let spawn = TestSpawn::new();
        let writer = Arc::new(CapturingWriter::new()) as Arc<dyn AsyncWriter>;

        let _ = drive(&sub, &mut reader, &spawn, writer.clone(), "alpha").await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        let _ = drive(&sub, &mut reader, &spawn, writer, "alpha").await;

        let handles = spawn.handles();
        assert_eq!(handles.len(), 2);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(handles[0].is_finished(), "prior task must be aborted");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refresh_fills_known_table_tokens_for_anthropic() {
        let mut reader = InMemoryReader::new();
        populate_anthropic_account(&mut reader, "alpha", "sk-key");

        // Server returns a haiku-4-5 entry with no max_* — the
        // known_family_metadata("claude-haiku-4-5", "anthropic") row
        // should fill it in and set source=KnownTable.
        let transport = Arc::new(
            MockTransport::new().with_catalog(Ok(vec![server_model("claude-haiku-4-5-20251001")])),
        );
        let sub = CatalogRefreshSubscription::new(transport);
        let spawn = TestSpawn::new();
        let cap = CapturingWriter::new();
        let writer: Arc<dyn AsyncWriter> = Arc::new(cap.clone());

        let _ = drive(&sub, &mut reader, &spawn, writer, "alpha").await;
        wait_for_all(&spawn).await;

        let saved: Vec<ModelInfo> = cap
            .typed("config/gate/accounts/alpha/models")
            .expect("models not recorded");
        assert_eq!(saved.len(), 1);
        let m = &saved[0];
        assert_eq!(m.source, ModelInfoSource::KnownTable);
        assert_eq!(m.max_context_size, Some(200_000));
        assert_eq!(m.max_output_tokens, Some(8_192));
    }
}

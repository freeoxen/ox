//! Headless end-to-end tests for the settings screen (Phase R, Task R1).
//!
//! Each test wires up a real `BrokerStore` with `LocalConfig` mounts under
//! `settings/`, `config/`, `ui/`, and `secret/`, registers the day-one
//! gate subscriptions against a `MockTransport`, and registers the
//! settings renderers / commands / bindings. Keystrokes are sent through
//! the real `crate::dispatch::send_key` path; subscription-driven async
//! work is awaited by polling the broker until the expected post-state
//! lands (or the timeout fires).
//!
//! These tests intentionally exercise the full pipeline — binding lookup
//! → command run → broker write → subscription dispatch → spawned task
//! writeback — so a regression anywhere along that path surfaces here
//! instead of as a UX bug at runtime.
//!
//! ## MockTransport
//!
//! The same shape as `ox-gate`'s `subscriptions::util::testing::MockTransport`
//! lives there only as `pub(crate)` so it is not reachable from this
//! integration crate; we reproduce the minimal surface here.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ox_broker::{BrokerStore, ClientHandle};
use ox_gate::transport::{TestResult, Transport};
use ox_gate::{
    AccountConfig, AccountTestStatus, ApiKey, CatalogRefreshStatus, ModelInfo, ModelInfoSource,
    ProviderConfig,
};
use ox_path::oxpath;
use ox_store_util::local_config::LocalConfig;
use ox_types::settings::ModelKey;
use ox_types::{ClientModalFlags, CompletionRole, Screen};
use ox_ui::UiStore;
use structfs_core_store::{Path, Record, Value};

use ox_cli::dispatch::{KeyDispatchOutcome, send_key};
use ox_cli::settings::binding_registry::BindingRegistry;
use ox_cli::settings::command_registry::CommandRegistry;
use ox_cli::settings::commands::navigation::path_to_value;
use ox_cli::settings::registry::RendererRegistry;
use ox_cli::settings::snapshot::{SettingsSnapshot, fetch_settings_view_state};

// ---------------------------------------------------------------------------
// MockTransport — records every call, returns scripted responses.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct MockTransport {
    test_response: Arc<Mutex<Result<TestResult, String>>>,
    catalog_response: Arc<Mutex<Result<Vec<ModelInfo>, String>>>,
    test_calls: Arc<Mutex<Vec<String>>>,
    catalog_calls: Arc<Mutex<Vec<String>>>,
}

impl MockTransport {
    fn new() -> Self {
        Self {
            test_response: Arc::new(Mutex::new(Ok(("anthropic".to_string(), 42)))),
            catalog_response: Arc::new(Mutex::new(Ok(vec![]))),
            test_calls: Arc::new(Mutex::new(Vec::new())),
            catalog_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn with_test_result(self, result: Result<TestResult, String>) -> Self {
        *self.test_response.lock().unwrap() = result;
        self
    }

    fn with_catalog(self, result: Result<Vec<ModelInfo>, String>) -> Self {
        *self.catalog_response.lock().unwrap() = result;
        self
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn test_connection(
        &self,
        account: &str,
        _provider: &ProviderConfig,
        _api_key: &str,
    ) -> Result<TestResult, String> {
        self.test_calls.lock().unwrap().push(account.to_string());
        self.test_response.lock().unwrap().clone()
    }

    async fn fetch_catalog(
        &self,
        account: &str,
        _provider: &ProviderConfig,
        _api_key: &str,
    ) -> Result<Vec<ModelInfo>, String> {
        self.catalog_calls.lock().unwrap().push(account.to_string());
        self.catalog_response.lock().unwrap().clone()
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// All the state a single scenario needs in one place. The mount join
/// handles are kept alive for the test's lifetime — dropping the
/// `BrokerStore` doesn't tear them down, so we own them on the harness.
struct E2eHarness {
    _broker: BrokerStore,
    client: ClientHandle,
    renderers: RendererRegistry,
    commands: CommandRegistry,
    bindings: BindingRegistry,
    #[allow(dead_code)]
    transport: Arc<MockTransport>,
    _mounts: Vec<tokio::task::JoinHandle<()>>,
}

impl E2eHarness {
    async fn new() -> Self {
        Self::new_with_transport(Arc::new(MockTransport::new())).await
    }

    async fn new_with_transport(transport: Arc<MockTransport>) -> Self {
        let broker = BrokerStore::new(Duration::from_secs(5));
        let client = broker.client();

        // Mount the four namespaces the settings screen reads / writes.
        let mut mounts = Vec::new();
        mounts.push(broker.mount(oxpath!("settings"), LocalConfig::new()).await);
        mounts.push(broker.mount(oxpath!("config"), LocalConfig::new()).await);
        mounts.push(broker.mount(oxpath!("ui"), LocalConfig::new()).await);
        mounts.push(broker.mount(oxpath!("secret"), LocalConfig::new()).await);

        // Wire subscriptions against the mock transport.
        let transport_dyn: Arc<dyn Transport> = transport.clone();
        ox_gate::subscriptions::register_all(&broker, transport_dyn);

        // Register settings renderers / commands / bindings.
        let mut renderers = RendererRegistry::new();
        ox_cli::settings::renderers::register_all(&mut renderers);
        let mut commands = CommandRegistry::new();
        ox_cli::settings::commands::register_all(&mut commands);
        let mut bindings = BindingRegistry::new();
        ox_cli::settings::bindings::register(&mut bindings);

        Self {
            _broker: broker,
            client,
            renderers,
            commands,
            bindings,
            transport,
            _mounts: mounts,
        }
    }

    async fn snapshot(&self) -> SettingsSnapshot {
        fetch_settings_view_state(&self.client).await
    }

    /// Read the focused-widget path written by the tree commands.
    async fn focused(&self) -> Option<Path> {
        let rec = self
            .client
            .read(&oxpath!("ui", "settings", "focused"))
            .await
            .expect("read focused")?;
        let value = rec.as_value()?.clone();
        match value {
            Value::Array(items) => {
                let mut comps: Vec<String> = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        Value::String(s) => comps.push(s),
                        _ => return None,
                    }
                }
                Path::try_from_components(comps).ok()
            }
            _ => None,
        }
    }

    /// Read the current settings cursor as an oxpath. Returns `None`
    /// when no cursor has been written yet.
    async fn current_cursor(&self) -> Option<Path> {
        let rec = self
            .client
            .read(&oxpath!("ui", "settings", "cursor"))
            .await
            .expect("read cursor")?;
        let value = rec.as_value()?.clone();
        match value {
            Value::Array(items) => {
                let mut comps: Vec<String> = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        Value::String(s) => comps.push(s),
                        _ => return None,
                    }
                }
                Path::try_from_components(comps).ok()
            }
            _ => None,
        }
    }

    /// Drive a single key through `send_key`, scoped to the settings
    /// screen and the broker's current cursor. The snapshot used for
    /// binding dispatch is freshly fetched so post-write reads inside
    /// the same key see consistent state.
    async fn dispatch(&self, key: &str) -> KeyDispatchOutcome {
        let mut snap = self.snapshot().await;
        let cursor = self.current_cursor().await.unwrap_or_else(|| oxpath!());
        send_key(
            &self.client,
            key,
            Screen::Settings,
            ClientModalFlags::default(),
            Some(&cursor),
            Some(&mut snap),
            Some(&self.bindings),
            Some(&self.commands),
            Some(&self.renderers),
        )
        .await
    }

    /// Convenience: write a value at `path` via the broker.
    async fn write_typed<T: serde::Serialize>(&self, path: &Path, value: &T) {
        self.client
            .write_typed(path, value)
            .await
            .expect("write_typed");
    }

    /// Convenience: write a `Path` at `path` (encoded via `path_to_value`).
    async fn write_path(&self, path: &Path, target: &Path) {
        self.client
            .write(path, Record::parsed(path_to_value(target)))
            .await
            .expect("write path");
    }
}

// ---------------------------------------------------------------------------
// Polling helpers
// ---------------------------------------------------------------------------

/// Poll `condition` until it returns `Some(_)` or the timeout expires.
/// Sleeps 50ms between attempts; default timeout is 2s. Returns the
/// produced value on success, `None` on timeout.
async fn poll_until<F, Fut, T>(mut condition: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    for _ in 0..40 {
        if let Some(v) = condition().await {
            return Some(v);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

// ---------------------------------------------------------------------------
// Common fixtures used by scenarios
// ---------------------------------------------------------------------------

async fn populate_index(h: &E2eHarness) {
    ox_cli::settings::bootstrap::populate_index_entries(&h.client)
        .await
        .expect("populate index");
}

/// Pre-populate `config/gate/accounts/{name}`, `config/gate/providers/{name}`,
/// and `secret/keys/{name}` so the subscriptions see a complete account
/// when handling test/refresh/delete triggers.
async fn populate_account(h: &E2eHarness, name: &str, key: &str) {
    let comp = ox_kernel::PathComponent::try_new(name).unwrap();
    h.write_typed(
        &oxpath!("config", "gate", "accounts", comp.clone()),
        &AccountConfig {
            provider: name.to_string(),
        },
    )
    .await;
    h.write_typed(
        &oxpath!("config", "gate", "providers", comp.clone()),
        &ProviderConfig::anthropic(),
    )
    .await;
    h.write_typed(&oxpath!("secret", "keys", comp), &ApiKey::new(key))
        .await;
}

async fn write_models_for_account(h: &E2eHarness, name: &str, ids: &[&str]) {
    let comp = ox_kernel::PathComponent::try_new(name).unwrap();
    let models: Vec<ModelInfo> = ids
        .iter()
        .map(|id| ModelInfo {
            id: (*id).to_string(),
            display_name: (*id).to_string(),
            max_context_size: None,
            max_output_tokens: None,
            source: ModelInfoSource::Server,
        })
        .collect();
    h.write_typed(
        &oxpath!("config", "gate", "accounts", comp, "models"),
        &models,
    )
    .await;
}

// ---------------------------------------------------------------------------
// Scenario: navigate index → models, then set bootstrap
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn navigate_index_to_models_set_bootstrap() {
    let h = E2eHarness::new().await;
    populate_index(&h).await;

    populate_account(&h, "anthropic", "sk-test").await;
    write_models_for_account(&h, "anthropic", &["claude-haiku-4-5-20251001"]).await;

    // The page-level cursor (binding scope) sits at the index. The
    // focused-widget state lives at `ui/settings/focused`.
    h.write_path(
        &oxpath!("ui", "settings", "cursor"),
        &oxpath!("settings", "index"),
    )
    .await;
    h.write_path(
        &oxpath!("ui", "settings", "focused"),
        &oxpath!("settings", "accounts"),
    )
    .await;

    // `j` advances the focused row to Models (the only other
    // top-level row visible while nothing is expanded).
    assert!(matches!(h.dispatch("j").await, KeyDispatchOutcome::Handled));
    let focused = h.focused().await.expect("focused written");
    assert_eq!(focused, oxpath!("settings", "models"));

    // `Enter` on a category row toggles expansion in place.
    assert!(matches!(
        h.dispatch("Enter").await,
        KeyDispatchOutcome::Handled
    ));
    let expanded: Vec<String> = h
        .client
        .read_typed(&oxpath!("ui", "settings", "expanded"))
        .await
        .expect("read expanded")
        .expect("expanded present");
    assert_eq!(expanded, vec!["settings/models".to_string()]);

    // `j` again moves focus into the now-visible model leaf row.
    // The row's path uses sanitized components (UAX#31 forbids `-` in
    // path identifiers) so `claude-haiku-4-5-20251001` becomes
    // `claude_haiku_4_5_20251001` in the cursor — the real model id
    // stays intact on the row's `RowKind`, which `tree.activate` reads
    // when descending to the legacy detail page.
    assert!(matches!(h.dispatch("j").await, KeyDispatchOutcome::Handled));
    let focused = h.focused().await.expect("focused written");
    assert_eq!(
        focused.to_string(),
        "settings/models/anthropic/claude_haiku_4_5_20251001",
    );

    // `P` on the focused model row fires `models.set_bootstrap`
    // through the per-row `Prefix(settings/models)` binding — no
    // page-flip required. The command resolves the unsanitized
    // (account, model_id) from the row's `RowKind` and writes
    // both `config/gate/completions/bootstrap` (new source of truth)
    // and `config/gate/completions/primary` (legacy migration path).
    assert!(matches!(h.dispatch("P").await, KeyDispatchOutcome::Handled));
    let bootstrap: CompletionRole = h
        .client
        .read_typed(&oxpath!("config", "gate", "completions", "bootstrap"))
        .await
        .expect("read bootstrap")
        .expect("bootstrap present");
    assert_eq!(bootstrap.account, "anthropic");
    assert_eq!(bootstrap.model_id, "claude-haiku-4-5-20251001");
    let legacy: CompletionRole = h
        .client
        .read_typed(&oxpath!("config", "gate", "completions", "primary"))
        .await
        .expect("read legacy primary")
        .expect("legacy primary present");
    assert_eq!(legacy.account, "anthropic");
    assert_eq!(legacy.model_id, "claude-haiku-4-5-20251001");
}

// ---------------------------------------------------------------------------
// Scenario: add account creation flow
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn add_account_create_flow() {
    let h = E2eHarness::new().await;
    populate_index(&h).await;

    // Cursor at the accordion, focused on the Accounts header so
    // `a` resolves to accounts.add via Prefix(settings/accounts).
    h.write_path(
        &oxpath!("ui", "settings", "cursor"),
        &oxpath!("settings", "index"),
    )
    .await;
    h.write_path(
        &oxpath!("ui", "settings", "focused"),
        &oxpath!("settings", "accounts"),
    )
    .await;

    // `a` expands the section, focuses the ghost row, and enters edit mode.
    assert!(matches!(h.dispatch("a").await, KeyDispatchOutcome::Handled));

    // Type the name through the dispatcher so the edit-mode pass routes
    // each printable through edit.insert_char.
    for ch in "anthropic_personal".chars() {
        let key = ch.to_string();
        assert!(
            matches!(h.dispatch(&key).await, KeyDispatchOutcome::Handled),
            "dispatch returned Unhandled for {key:?}"
        );
    }

    // Enter routes through edit.commit, which writes the AccountConfig
    // directly. The CLI's writes are synchronous; subsequent reads see
    // the materialized state.
    assert!(matches!(
        h.dispatch("Enter").await,
        KeyDispatchOutcome::Handled
    ));

    let comp = ox_kernel::PathComponent::try_new("anthropic_personal").unwrap();
    let account: AccountConfig = h
        .client
        .read_typed(&oxpath!("config", "gate", "accounts", comp.clone()))
        .await
        .expect("read account record")
        .expect("account record present after synchronous create");
    assert_eq!(account.provider, "anthropic");

    // Cursor settled at settings/index and focused points at the new account row.
    let cursor = h.current_cursor().await.expect("cursor present");
    assert_eq!(cursor, oxpath!("settings", "index"));
    let focused = h.focused().await.expect("focused present");
    assert_eq!(focused, oxpath!("settings", "accounts", comp.clone()));

    // Expanded set must include both settings/accounts and the new
    // account's row so the user sees the field rows immediately.
    let expanded: Vec<String> = h
        .client
        .read_typed(&oxpath!("ui", "settings", "expanded"))
        .await
        .expect("read expanded")
        .flatten()
        .unwrap_or_default();
    assert!(
        expanded.iter().any(|s| s == "settings/accounts"),
        "expanded set must contain settings/accounts; got {expanded:?}"
    );
    assert!(
        expanded
            .iter()
            .any(|s| s == "settings/accounts/anthropic_personal"),
        "expanded set must contain settings/accounts/anthropic_personal; got {expanded:?}"
    );

    let selected: Option<String> = h
        .client
        .read_typed(&oxpath!("ui", "settings", "accounts", "selected"))
        .await
        .expect("read selected")
        .flatten();
    assert_eq!(selected.as_deref(), Some("anthropic_personal"));
}

// ---------------------------------------------------------------------------
// Scenario: delete account flow
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_account_flow() {
    let h = E2eHarness::new().await;
    populate_index(&h).await;
    populate_account(&h, "anthropic", "sk-test").await;

    // Cursor at accounts list, selection points at the account.
    h.write_path(
        &oxpath!("ui", "settings", "cursor"),
        &oxpath!("settings", "accounts"),
    )
    .await;
    h.write_typed(
        &oxpath!("ui", "settings", "accounts", "selected"),
        &Some("anthropic".to_string()),
    )
    .await;

    // `d` — arm the inline delete-confirmation banner. Cursor stays
    // at settings/accounts; ui/settings/pending_delete becomes
    // Some(<name>) and the dispatcher routes y/n/Esc through the
    // synthetic `_pending_delete` scope.
    assert!(matches!(h.dispatch("d").await, KeyDispatchOutcome::Handled));
    let pending: Option<String> = h
        .client
        .read_typed(&oxpath!("ui", "settings", "pending_delete"))
        .await
        .ok()
        .flatten();
    assert_eq!(pending.as_deref(), Some("anthropic"));
    assert_eq!(
        h.current_cursor().await.expect("cursor"),
        oxpath!("settings", "accounts"),
    );

    // `y` — confirm delete. The CLI's null-write removes the account
    // record directly; AccountDeleteCleanupSubscription fires async to
    // clean up side data. Poll for the account record's removal.
    assert!(matches!(h.dispatch("y").await, KeyDispatchOutcome::Handled));

    let comp = ox_kernel::PathComponent::try_new("anthropic").unwrap();
    let acct_path = oxpath!("config", "gate", "accounts", comp);
    let gone = poll_until(|| async {
        match h.client.read(&acct_path).await {
            Ok(None) => Some(()),
            Ok(Some(rec)) => match rec.as_value() {
                // `null` write is the canonical delete.
                Some(Value::Null) => Some(()),
                _ => None,
            },
            Err(_) => None,
        }
    })
    .await;
    assert!(
        gone.is_some(),
        "account record should be removed after delete"
    );

    // Selection cleared.
    let cleared = poll_until(|| async {
        let opt: Option<Option<String>> = h
            .client
            .read_typed(&oxpath!("ui", "settings", "accounts", "selected"))
            .await
            .ok();
        match opt {
            None => Some(()),
            Some(None) => Some(()),
            Some(Some(_)) => None,
        }
    })
    .await;
    assert!(
        cleared.is_some(),
        "selection should be cleared after delete"
    );

    // Cursor stays at settings/accounts throughout — the inline
    // banner never moved it. pending_delete is cleared by
    // accounts.confirm.delete.
    let cursor = h.current_cursor().await.expect("cursor");
    assert_eq!(cursor, oxpath!("settings", "accounts"));
    let pending_after: Option<String> = h
        .client
        .read_typed(&oxpath!("ui", "settings", "pending_delete"))
        .await
        .ok()
        .flatten();
    assert!(
        pending_after.is_none(),
        "pending_delete should be cleared after confirm; got {pending_after:?}"
    );
}

// ---------------------------------------------------------------------------
// Scenario: test connection progresses Idle → Testing → Success
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_account_progresses_status() {
    let transport =
        Arc::new(MockTransport::new().with_test_result(Ok(("anthropic".to_string(), 87))));
    let h = E2eHarness::new_with_transport(transport.clone()).await;
    populate_index(&h).await;
    populate_account(&h, "anthropic", "sk-test").await;

    // Cursor at the detail page; selection on the account.
    h.write_path(
        &oxpath!("ui", "settings", "cursor"),
        &oxpath!("settings", "accounts", "_detail"),
    )
    .await;
    h.write_typed(
        &oxpath!("ui", "settings", "accounts", "selected"),
        &Some("anthropic".to_string()),
    )
    .await;

    // `t` runs `account.test`. The synchronous side of the subscription
    // writes `Testing { … }` before the spawned task runs. Polling
    // observes either Testing or Success — both confirm the lifecycle
    // engaged (Testing is racy under multi-thread schedulers).
    assert!(matches!(h.dispatch("t").await, KeyDispatchOutcome::Handled));

    let comp = ox_kernel::PathComponent::try_new("anthropic").unwrap();
    let status_path = oxpath!("config", "gate", "accounts", comp, "test_status");

    // The Success transition is the load-bearing one — it proves the
    // spawned task ran end-to-end. Wait for it.
    let final_status = poll_until(|| async {
        let s: Option<AccountTestStatus> = h.client.read_typed(&status_path).await.ok().flatten();
        match s {
            Some(AccountTestStatus::Success { .. }) => s,
            _ => None,
        }
    })
    .await;
    let final_status = final_status.expect("test_status should reach Success");
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

    // Mock transport observed exactly one call with the right account.
    assert_eq!(
        transport.test_calls.lock().unwrap().as_slice(),
        &["anthropic".to_string()],
    );
}

// ---------------------------------------------------------------------------
// Scenario: refresh writes catalog
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refresh_writes_catalog() {
    let scripted: Vec<ModelInfo> = vec![
        ModelInfo {
            id: "claude-haiku-4-5-20251001".into(),
            display_name: "Claude Haiku 4.5".into(),
            max_context_size: Some(200_000),
            max_output_tokens: Some(8_192),
            source: ModelInfoSource::Server,
        },
        ModelInfo {
            id: "claude-sonnet-4-20250514".into(),
            display_name: "Claude Sonnet 4".into(),
            max_context_size: Some(200_000),
            max_output_tokens: Some(8_192),
            source: ModelInfoSource::Server,
        },
        ModelInfo {
            id: "claude-opus-4-20250514".into(),
            display_name: "Claude Opus 4".into(),
            max_context_size: Some(200_000),
            max_output_tokens: Some(8_192),
            source: ModelInfoSource::Server,
        },
    ];
    let transport = Arc::new(MockTransport::new().with_catalog(Ok(scripted.clone())));
    let h = E2eHarness::new_with_transport(transport.clone()).await;
    populate_index(&h).await;
    populate_account(&h, "anthropic", "sk-test").await;

    // Cursor at the models list; selection points at a model belonging
    // to the account whose catalog we'll refresh. The refresh command
    // reads `selected: Option<ModelKey>` and pulls `account` off it.
    h.write_path(
        &oxpath!("ui", "settings", "cursor"),
        &oxpath!("settings", "models"),
    )
    .await;
    h.write_typed(
        &oxpath!("ui", "settings", "models", "selected"),
        &Some(ModelKey {
            account: "anthropic".to_string(),
            model_id: "placeholder".to_string(),
        }),
    )
    .await;

    // `r` triggers the catalog refresh subscription.
    assert!(matches!(h.dispatch("r").await, KeyDispatchOutcome::Handled));

    let comp = ox_kernel::PathComponent::try_new("anthropic").unwrap();
    let models_path = oxpath!("config", "gate", "accounts", comp.clone(), "models");
    let refresh_status_path = oxpath!("config", "gate", "accounts", comp, "refresh_status");

    let saved = poll_until(|| async {
        h.client
            .read_typed::<Vec<ModelInfo>>(&models_path)
            .await
            .ok()
            .flatten()
            .filter(|m| m.len() == scripted.len())
    })
    .await;
    let saved = saved.expect("catalog should be written by refresh subscription");
    assert_eq!(saved.len(), 3);
    let ids: Vec<&str> = saved.iter().map(|m| m.id.as_str()).collect();
    assert!(ids.contains(&"claude-haiku-4-5-20251001"));
    assert!(ids.contains(&"claude-sonnet-4-20250514"));
    assert!(ids.contains(&"claude-opus-4-20250514"));

    let refresh_status = poll_until(|| async {
        let s: Option<CatalogRefreshStatus> = h
            .client
            .read_typed(&refresh_status_path)
            .await
            .ok()
            .flatten();
        match s {
            Some(CatalogRefreshStatus::Success { .. }) => s,
            _ => None,
        }
    })
    .await;
    match refresh_status.expect("refresh_status should reach Success") {
        CatalogRefreshStatus::Success {
            models_added,
            models_updated,
            ..
        } => {
            assert_eq!(models_added, 3);
            assert_eq!(models_updated, 0);
        }
        other => panic!("expected Success, got {other:?}"),
    }

    assert_eq!(
        transport.catalog_calls.lock().unwrap().as_slice(),
        &["anthropic".to_string()],
    );
}

// Pin the production substrate. Other tests in this file mount a
// generic `LocalConfig` at `ui/`, which accepts arbitrary state-shaped
// writes — production mounts `UiStore`, which only honors them through
// the embedded `settings/*` sub-store. Without this test, that sub-store
// could be removed and the suite would still go green while production
// silently rejected every settings keystroke.

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn production_ui_store_routes_settings_writes() {
    let broker = BrokerStore::new(Duration::from_secs(5));
    let client = broker.client();

    let mut mounts = Vec::new();
    mounts.push(broker.mount(oxpath!("settings"), LocalConfig::new()).await);
    mounts.push(broker.mount(oxpath!("config"), LocalConfig::new()).await);
    mounts.push(broker.mount(oxpath!("ui"), UiStore::new()).await);
    mounts.push(broker.mount(oxpath!("secret"), LocalConfig::new()).await);

    let mut renderers = RendererRegistry::new();
    ox_cli::settings::renderers::register_all(&mut renderers);
    let mut commands = CommandRegistry::new();
    ox_cli::settings::commands::register_all(&mut commands);
    let mut bindings = BindingRegistry::new();
    ox_cli::settings::bindings::register(&mut bindings);

    ox_cli::settings::bootstrap::populate_index_entries(&client)
        .await
        .expect("populate index");

    client
        .write(
            &oxpath!("ui", "settings", "cursor"),
            Record::parsed(path_to_value(&oxpath!("settings", "index"))),
        )
        .await
        .expect(
            "cursor write must reach UiStore's settings sub-store — if this \
             errors, the `settings/*` arm has been removed from UiStore::write",
        );

    // Seed page cursor (binding scope) and the focused row.
    client
        .write(
            &oxpath!("ui", "settings", "cursor"),
            Record::parsed(path_to_value(&oxpath!("settings", "index"))),
        )
        .await
        .expect("seed page cursor");
    client
        .write(
            &oxpath!("ui", "settings", "focused"),
            Record::parsed(path_to_value(&oxpath!("settings", "accounts"))),
        )
        .await
        .expect("seed focused");

    let mut snap = fetch_settings_view_state(&client).await;
    let cursor = oxpath!("settings", "index");
    let outcome = send_key(
        &client,
        "j",
        Screen::Settings,
        ClientModalFlags::default(),
        Some(&cursor),
        Some(&mut snap),
        Some(&bindings),
        Some(&commands),
        Some(&renderers),
    )
    .await;
    assert!(matches!(outcome, KeyDispatchOutcome::Handled));

    // The focused-row write must round-trip through UiStore's
    // settings sub-store. `None` here would mean the sub-store has
    // been removed or replaced with a typed-command surface.
    let focused_record = client
        .read(&oxpath!("ui", "settings", "focused"))
        .await
        .expect("read focused")
        .expect(
            "focused write must persist — UiStore's settings sub-store is \
             gone if this is None (see crates/ox-ui/src/ui_store.rs)",
        );
    let new_focus =
        ox_cli::settings::commands::navigation::path_from_value(focused_record.as_value().unwrap())
            .expect("focused decodes as path");
    assert_eq!(
        new_focus.to_string(),
        "settings/models",
        "j on the Accounts row must advance focus to Models",
    );
}

// ---------------------------------------------------------------------------
// Scenario: cycling Protocol on a custom-provider account
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cycling_protocol_with_toml_loaded_flat_keys_advances_through_broker() {
    // The TomlFileBacking-loaded shape: provider record arrives as
    // flat sub-keys (no parent Map). Cycle twice through the real
    // broker dispatch path — both cycles must advance the dialect.
    // The second cycle exercises the runtime-Map override-of-base-
    // flat-keys read path, which is structurally distinct from the
    // first (synthesizes a default ProviderConfig when no parent Map
    // exists yet).
    let h = E2eHarness::new().await;
    populate_index(&h).await;

    // Account `LMStudio` as flat sub-key (mimics TOML loading): no parent
    // AccountConfig leaf, just the child `provider` string.
    let comp = ox_kernel::PathComponent::try_new("LMStudio").unwrap();
    h.client
        .write(
            &oxpath!("config", "gate", "accounts", comp.clone(), "provider"),
            Record::parsed(Value::String("LMStudio".into())),
        )
        .await
        .expect("write provider child");
    // Provider `LMStudio` record as flat sub-keys, dialect=openai.
    h.client
        .write(
            &oxpath!("config", "gate", "providers", comp.clone(), "dialect"),
            Record::parsed(Value::String("openai".into())),
        )
        .await
        .expect("write dialect");
    h.client
        .write(
            &oxpath!("config", "gate", "providers", comp.clone(), "endpoint"),
            Record::parsed(Value::String("http://127.0.0.1:1234".into())),
        )
        .await
        .expect("write endpoint");
    h.client
        .write(
            &oxpath!("config", "gate", "providers", comp.clone(), "auth"),
            Record::parsed(Value::String("none".into())),
        )
        .await
        .expect("write auth");
    h.client
        .write(
            &oxpath!("config", "gate", "providers", comp.clone(), "version"),
            Record::parsed(Value::String(String::new())),
        )
        .await
        .expect("write version");

    h.client
        .write(
            &oxpath!("ui", "settings", "expanded"),
            Record::parsed(ox_cli::settings::visible_rows::expanded_set_to_value(&[
                "settings/accounts".to_string(),
                "settings/accounts/LMStudio".to_string(),
            ])),
        )
        .await
        .expect("write expanded set");

    h.write_path(
        &oxpath!("ui", "settings", "cursor"),
        &oxpath!("settings", "accounts", comp.clone()),
    )
    .await;
    h.write_path(
        &oxpath!("ui", "settings", "focused"),
        &oxpath!("settings", "accounts", comp.clone(), "protocol"),
    )
    .await;

    // First cycle.
    assert!(matches!(h.dispatch("l").await, KeyDispatchOutcome::Handled));
    let pc1: ProviderConfig = h
        .client
        .read_typed(&oxpath!("config", "gate", "providers", comp.clone()))
        .await
        .expect("read provider")
        .expect("provider present after first cycle");
    // From acct.provider="LMStudio" (since no parent ProviderConfig Map
    // existed yet to read dialect from), synthesized default has
    // dialect="LMStudio". options=[anthropic, openai, LMStudio]; idx=2;
    // forward → idx 0 = "anthropic".
    assert_eq!(
        pc1.dialect, "anthropic",
        "first cycle must advance from synthesized 'LMStudio' to 'anthropic'"
    );

    // Second cycle — now the runtime override exists; the read should
    // see dialect="anthropic", and forward should land on "openai".
    assert!(matches!(h.dispatch("l").await, KeyDispatchOutcome::Handled));
    let pc2: ProviderConfig = h
        .client
        .read_typed(&oxpath!("config", "gate", "providers", comp))
        .await
        .expect("read provider")
        .expect("provider present after second cycle");
    assert_eq!(
        pc2.dialect, "openai",
        "second cycle must advance from 'anthropic' to 'openai'"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cycling_protocol_mutates_bound_provider_dialect_not_account() {
    // Drives the full dispatch path (key → binding → command → broker
    // write) for the Protocol carousel. The Protocol field characterizes
    // the wire-format dialect the bound endpoint speaks — anthropic,
    // openai — not the provider record's name. Cycling mutates the
    // bound provider record's `dialect` field; the account's provider
    // reference (the record name) stays stable so endpoint, auth, and
    // sharing relationships survive the cycle.
    let h = E2eHarness::new().await;
    populate_index(&h).await;

    // Account `local` bound to provider record `LMStudio`; that record
    // currently speaks the openai dialect.
    let acct_comp = ox_kernel::PathComponent::try_new("local").unwrap();
    h.write_typed(
        &oxpath!("config", "gate", "accounts", acct_comp.clone()),
        &AccountConfig {
            provider: "LMStudio".to_string(),
        },
    )
    .await;
    let prov_comp = ox_kernel::PathComponent::try_new("LMStudio").unwrap();
    h.write_typed(
        &oxpath!("config", "gate", "providers", prov_comp.clone()),
        &ProviderConfig {
            dialect: "openai".to_string(),
            endpoint: "http://127.0.0.1:1234".to_string(),
            version: String::new(),
            auth: Some(ox_gate::AuthScheme::None),
        },
    )
    .await;

    // Expand the connection so the Protocol field row exists in the
    // visible enumeration cycle_field walks.
    h.client
        .write(
            &oxpath!("ui", "settings", "expanded"),
            Record::parsed(ox_cli::settings::visible_rows::expanded_set_to_value(&[
                "settings/accounts".to_string(),
                "settings/accounts/local".to_string(),
            ])),
        )
        .await
        .expect("write expanded set");

    h.write_path(
        &oxpath!("ui", "settings", "cursor"),
        &oxpath!("settings", "accounts", acct_comp.clone()),
    )
    .await;
    h.write_path(
        &oxpath!("ui", "settings", "focused"),
        &oxpath!("settings", "accounts", acct_comp.clone(), "protocol"),
    )
    .await;

    assert!(matches!(h.dispatch("l").await, KeyDispatchOutcome::Handled));

    // Provider record's dialect cycled openai → anthropic (wrap from
    // idx 1 in [anthropic, openai]). Endpoint and auth survive.
    let pc: ProviderConfig = h
        .client
        .read_typed(&oxpath!("config", "gate", "providers", prov_comp))
        .await
        .expect("read provider")
        .expect("provider present");
    assert_eq!(
        pc.dialect, "anthropic",
        "forward cycle from openai must mutate dialect to anthropic"
    );
    assert_eq!(pc.endpoint, "http://127.0.0.1:1234");

    // Account's provider reference is unchanged — the record name still
    // points at LMStudio, just with a different dialect now.
    let acct: AccountConfig = h
        .client
        .read_typed(&oxpath!("config", "gate", "accounts", acct_comp))
        .await
        .expect("read account")
        .expect("account present");
    assert_eq!(
        acct.provider, "LMStudio",
        "cycling Protocol must not mutate the account's provider reference"
    );
}

// ---------------------------------------------------------------------------
// Render snapshot tests — drive the settings View through ratatui to a
// TestBackend and capture the visible buffer. Catches regressions in the
// renderer's read path that broker-only e2e tests miss (e.g. the cycle
// writes the right thing to the broker but the renderer doesn't see it
// because it reads from a different path or with different fallback
// semantics).
// ---------------------------------------------------------------------------

use ox_cli::test_render_exports::{Theme, render_to_frame};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

/// Render the settings screen at `width` × `height` and return the
/// visible buffer as a `\n`-separated string. Drops styling — we
/// snapshot the visible characters; styles can be added later if a test
/// needs them.
async fn render_settings_to_string(h: &E2eHarness, width: u16, height: u16) -> String {
    use ratatui::layout::Rect;

    // Same fetch as the production event loop.
    let mut snap = fetch_settings_view_state(&h.client).await;
    let cursor: Path = h.current_cursor().await.unwrap_or_else(|| oxpath!());

    let theme = Theme::default();
    let view = {
        use ox_cli::settings::registry::RenderCtx;
        let mut ctx = RenderCtx {
            area: Rect::new(0, 0, width, height),
            data: &mut snap as &mut dyn structfs_core_store::Reader,
            registry: &h.renderers,
            theme: &theme,
        };
        h.renderers.render(&cursor, &mut ctx)
    };

    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test backend init");
    terminal
        .draw(|frame| {
            let area = Rect::new(0, 0, width, height);
            render_to_frame(&view, frame, area, &theme);
        })
        .expect("draw");
    buffer_to_string(terminal.backend().buffer())
}

/// Walk the buffer cell-by-cell; emit visible chars row by row. Trims
/// trailing spaces per row to reduce snapshot diff noise. Strips ANSI;
/// the snapshot is plain text.
fn buffer_to_string(buf: &Buffer) -> String {
    let area = buf.area();
    let mut out = String::with_capacity((area.width as usize + 1) * area.height as usize);
    for y in 0..area.height {
        let mut row = String::with_capacity(area.width as usize);
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            row.push_str(cell.symbol());
        }
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn protocol_cycle_visibly_toggles_in_rendered_carousel() {
    // Captures the rendered buffer before and after each cycle to
    // verify the renderer's read path picks up the cycle's broker
    // writes. Broker-only e2e tests can't catch the case where a
    // write lands but the read path doesn't surface it (e.g. an
    // AccountConfig::default() fallback that prevents the bound
    // provider from being resolved); identical frames across cycles
    // is the failure signal.
    let h = E2eHarness::new().await;
    populate_index(&h).await;

    // Mimic the user's TOML shape: account + provider as flat sub-keys
    // (TomlFileBacking output), no parent ProviderConfig Map. The
    // rendered initial carousel must show some sensible Protocol value;
    // each `l` press must produce a *visibly different* frame.
    let acct_comp = ox_kernel::PathComponent::try_new("LMStudio").unwrap();
    h.client
        .write(
            &oxpath!("config", "gate", "accounts", acct_comp.clone(), "provider"),
            Record::parsed(Value::String("LMStudio".into())),
        )
        .await
        .expect("write account/provider");
    let prov_comp = ox_kernel::PathComponent::try_new("LMStudio").unwrap();
    for (sub, val) in [
        ("dialect", "openai"),
        ("endpoint", "http://127.0.0.1:1234"),
        ("auth", "none"),
        ("version", ""),
    ] {
        let sub_comp = ox_kernel::PathComponent::try_new(sub).unwrap();
        h.client
            .write(
                &oxpath!("config", "gate", "providers", prov_comp.clone(), sub_comp),
                Record::parsed(Value::String(val.into())),
            )
            .await
            .expect("write provider sub-key");
    }

    h.client
        .write(
            &oxpath!("ui", "settings", "expanded"),
            Record::parsed(ox_cli::settings::visible_rows::expanded_set_to_value(&[
                "settings/accounts".to_string(),
                "settings/accounts/LMStudio".to_string(),
            ])),
        )
        .await
        .expect("write expanded set");
    // Page cursor stays at settings/index for the accordion design — the
    // index renderer is what renders the whole tree. focused is what
    // identifies the Protocol field row inside that tree (used for both
    // the renderer's `selected` highlight and binding-scope dispatch).
    h.write_path(
        &oxpath!("ui", "settings", "cursor"),
        &oxpath!("settings", "index"),
    )
    .await;
    h.write_path(
        &oxpath!("ui", "settings", "focused"),
        &oxpath!("settings", "accounts", acct_comp.clone(), "protocol"),
    )
    .await;

    let frame_before = render_settings_to_string(&h, 80, 24).await;
    insta::assert_snapshot!("protocol_carousel_before_cycle", &frame_before);

    assert!(matches!(h.dispatch("l").await, KeyDispatchOutcome::Handled));
    let frame_after_one = render_settings_to_string(&h, 80, 24).await;
    insta::assert_snapshot!("protocol_carousel_after_one_cycle", &frame_after_one);

    assert!(matches!(h.dispatch("l").await, KeyDispatchOutcome::Handled));
    let frame_after_two = render_settings_to_string(&h, 80, 24).await;
    insta::assert_snapshot!("protocol_carousel_after_two_cycles", &frame_after_two);

    // The bug-detection assertions, independent of the snapshot
    // contents. Each cycle must produce a visibly different frame —
    // the renderer must be picking up the broker writes the cycle
    // produced.
    assert_ne!(
        frame_before, frame_after_one,
        "first cycle must produce a visible change in the rendered carousel — \
         if these frames are identical, the renderer isn't reading the cycle's \
         provider-record write (likely AccountConfig::default() fallback)"
    );
    assert_ne!(
        frame_after_one, frame_after_two,
        "second cycle must also produce a visible change"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn add_connection_inline_ghost_row_accepts_typing() {
    // End-to-end render assertion for the inline new-connection ghost
    // row's typing surface. Opens it via `a` from the Connections
    // section, types a mix of upper- and lowercase chars, captures the
    // rendered frame after each keystroke. Each press must produce a
    // visibly-different frame and the cumulative input must appear in
    // the focused ghost row's inline buffer — covers case-sensitive
    // dispatch routing AND inline-edit write-back with one shape.
    let h = E2eHarness::new().await;
    populate_index(&h).await;

    // Cursor sits at the accordion (settings/index, where the renderer
    // lives); focused sits on settings/accounts so the `a` binding
    // (Prefix(settings/accounts)) resolves to accounts.add.
    h.write_path(
        &oxpath!("ui", "settings", "cursor"),
        &oxpath!("settings", "index"),
    )
    .await;
    h.write_path(
        &oxpath!("ui", "settings", "focused"),
        &oxpath!("settings", "accounts"),
    )
    .await;

    assert!(matches!(h.dispatch("a").await, KeyDispatchOutcome::Handled));

    let frame_after_a = render_settings_to_string(&h, 80, 24).await;
    insta::assert_snapshot!("new_connection_inline_just_opened", &frame_after_a);

    let mut prior_frame = frame_after_a.clone();
    for (snap_idx, ch) in (1..).zip("Test".chars()) {
        let key = ch.to_string();
        assert!(
            matches!(h.dispatch(&key).await, KeyDispatchOutcome::Handled),
            "dispatch returned Unhandled for {key:?}"
        );
        let frame = render_settings_to_string(&h, 80, 24).await;
        let snap_name = format!("new_connection_inline_after_{}_{}", snap_idx, ch);
        insta::assert_snapshot!(snap_name, &frame);
        assert_ne!(
            prior_frame, frame,
            "typing {ch:?} into the inline ghost row must produce a \
             visible change in the rendered frame"
        );
        prior_frame = frame;
    }

    // The final frame's inline buffer must contain the full word —
    // catches the case where each char produces *some* visual change
    // (e.g. cursor blink) but the actual write-back doesn't fill in.
    let final_frame = prior_frame;
    assert!(
        final_frame.contains("Test"),
        "rendered ghost row must show 'Test' in the inline buffer after typing it; \
         got:\n{final_frame}"
    );
}

// ---------------------------------------------------------------------------
// Repro: deletion-doesn't-clear-connection bug.
//
// User report: "Deletion doesn't seem to work? I get most of the UI signals
// but the actual connection doesn't go away?"
//
// Hypothesis: `accounts.confirm.delete` writes `Value::Null` at
// `config/gate/accounts/<name>`, but the renderer enumerates accounts via
// `child_names_under("config/gate/accounts")`. If the broker's flat-keyed
// LocalConfig map still surfaces the account's path components after a
// Null write — even though the record value is Null — the row will still
// appear in the rendered tree.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_account_removes_connection_from_rendered_frame() {
    let h = E2eHarness::new().await;
    populate_index(&h).await;
    populate_account(&h, "anthropic", "sk-test").await;

    // Expand the Connections section so the account row is visible in
    // the rendered tree. Without this the bug is masked — a collapsed
    // section never asks the renderer to enumerate accounts.
    h.client
        .write(
            &oxpath!("ui", "settings", "expanded"),
            Record::parsed(ox_cli::settings::visible_rows::expanded_set_to_value(&[
                "settings/accounts".to_string(),
            ])),
        )
        .await
        .expect("write expanded set");

    // Page cursor at the index renderer, focused on the account row so
    // `d` resolves via the `Prefix(settings/accounts)` binding to
    // `accounts.delete_confirm`. `accounts_delete_confirm` reads the
    // focused row first, falling back to `accounts/selected`; we set
    // both so the resolution path is unambiguous.
    let acct_comp = ox_kernel::PathComponent::try_new("anthropic").unwrap();
    h.write_path(
        &oxpath!("ui", "settings", "cursor"),
        &oxpath!("settings", "index"),
    )
    .await;
    h.write_path(
        &oxpath!("ui", "settings", "focused"),
        &oxpath!("settings", "accounts", acct_comp.clone()),
    )
    .await;
    h.write_typed(
        &oxpath!("ui", "settings", "accounts", "selected"),
        &Some("anthropic".to_string()),
    )
    .await;

    // Frame before any keystroke — the account row should be visible.
    let frame_before = render_settings_to_string(&h, 80, 24).await;
    insta::assert_snapshot!("delete_repro_before_delete", &frame_before);
    assert!(
        frame_before.contains("anthropic"),
        "pre-delete frame must show the account row; got:\n{frame_before}",
    );

    // `d` — arms the inline delete-confirmation banner.
    assert!(matches!(h.dispatch("d").await, KeyDispatchOutcome::Handled));
    let pending: Option<String> = h
        .client
        .read_typed(&oxpath!("ui", "settings", "pending_delete"))
        .await
        .ok()
        .flatten();
    assert_eq!(
        pending.as_deref(),
        Some("anthropic"),
        "pressing `d` must arm pending_delete with the focused account name",
    );

    let frame_after_d = render_settings_to_string(&h, 80, 24).await;
    insta::assert_snapshot!("delete_repro_after_d_pressed", &frame_after_d);
    assert!(
        frame_after_d.contains("Delete 'anthropic'?"),
        "after `d`, the inline confirmation banner must be visible; got:\n{frame_after_d}",
    );

    // `y` — confirms the delete. The CLI writes Value::Null at the
    // account path; AccountDeleteCleanupSubscription fires async to
    // clear the secret and provider records. Wait for the side-data
    // cleanup to settle before snapshotting so the result is stable.
    assert!(matches!(h.dispatch("y").await, KeyDispatchOutcome::Handled));

    // Wait until the account record is at least Null at its canonical
    // path — proves the synchronous delete write landed.
    let acct_path = oxpath!("config", "gate", "accounts", acct_comp.clone());
    let _ = poll_until(|| async {
        match h.client.read(&acct_path).await {
            Ok(None) => Some(()),
            Ok(Some(rec)) => match rec.as_value() {
                Some(Value::Null) => Some(()),
                _ => None,
            },
            Err(_) => None,
        }
    })
    .await;
    // Wait for the side-data cleanup subscription to remove the secret
    // key. This is the canonical "delete fully completed" signal — if
    // the renderer still shows the account at this point, the bug is
    // surfaced.
    let _ = poll_until(|| async {
        match h
            .client
            .read(&oxpath!("secret", "keys", acct_comp.clone()))
            .await
        {
            Ok(None) => Some(()),
            Ok(Some(rec)) => match rec.as_value() {
                Some(Value::Null) => Some(()),
                _ => None,
            },
            Err(_) => None,
        }
    })
    .await;

    let frame_after_y = render_settings_to_string(&h, 80, 24).await;
    insta::assert_snapshot!("delete_repro_after_y_pressed", &frame_after_y);

    // The bug-detection assertion: the account name must no longer
    // appear in the rendered tree.
    assert!(
        !frame_after_y.contains("anthropic"),
        "after confirming delete with `y`, the account row must be gone from the \
         rendered tree — the bug is that the renderer still enumerates the deleted \
         account because the broker's flat-key map retains the path components \
         after a Null write.\n\nFrame:\n{frame_after_y}",
    );

    // Direct broker reads — diagnostic output for the test report.
    // Walk the broker's enumeration of `config/gate/accounts` via
    // `read_subtree`, which is the same shape the snapshot pipeline
    // uses. Counting which entries still announce a child segment is
    // the most direct proxy for what the renderer sees.
    let entries = h
        .client
        .read_subtree(&oxpath!("config", "gate", "accounts"))
        .await
        .expect("read_subtree accounts");
    let mut child_names: Vec<String> = Vec::new();
    let prefix_len = oxpath!("config", "gate", "accounts").len();
    for path in entries.keys() {
        if path.len() <= prefix_len {
            continue;
        }
        let segment = path.components[prefix_len].clone();
        if !child_names.contains(&segment) {
            child_names.push(segment);
        }
    }
    assert!(
        !child_names.iter().any(|n| n == "anthropic"),
        "child enumeration of `config/gate/accounts` must not return \"anthropic\" \
         after delete; got: {child_names:?}",
    );
    let key_after = h
        .client
        .read(&oxpath!("secret", "keys", acct_comp.clone()))
        .await
        .expect("read secret key");
    let key_gone = match key_after {
        None => true,
        Some(rec) => matches!(rec.as_value(), Some(Value::Null)),
    };
    assert!(key_gone, "secret/keys/anthropic should be gone after delete");
    let prov_after = h
        .client
        .read(&oxpath!("config", "gate", "providers", acct_comp))
        .await
        .expect("read provider");
    let prov_gone = match prov_after {
        None => true,
        Some(rec) => matches!(rec.as_value(), Some(Value::Null)),
    };
    assert!(
        prov_gone,
        "config/gate/providers/anthropic should be gone after delete",
    );
}

// ---------------------------------------------------------------------------
// Repro: cycling Auth doesn't visibly advance past `x-api-key` in the
// rendered carousel.
//
// User report: "Auth seems to not actually show 'bearer-token' when
// rotating, I think there's a bug somewhere?"
//
// The cycle command mutates `ProviderConfig.auth` correctly — a direct
// read after one forward cycle returns `Some(BearerToken)`. But the
// rendered carousel still shows `x-api-key`. The mismatch lives in
// `visible_rows::append_account_field_rows`, which formats the auth row
// label as `format!("{scheme:?}").to_lowercase()` ("xapikey",
// "bearertoken", "none"). The renderer's index.rs then tries to match
// that against `AUTH_DISPLAY = ["x-api-key", "bearer-token", "none"]` to
// recover the carousel index, fails, and falls back to idx 0 — so the
// carousel always renders as "x-api-key".
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auth_cycle_renders_bearer_token_after_one_cycle() {
    let h = E2eHarness::new().await;
    populate_index(&h).await;
    // anthropic provider, default auth = Some(XApiKey).
    populate_account(&h, "anthropic", "sk-test").await;

    let acct_comp = ox_kernel::PathComponent::try_new("anthropic").unwrap();

    // Expand both the Connections section AND the account itself so the
    // Auth field row is visible in the rendered tree.
    h.client
        .write(
            &oxpath!("ui", "settings", "expanded"),
            Record::parsed(ox_cli::settings::visible_rows::expanded_set_to_value(&[
                "settings/accounts".to_string(),
                "settings/accounts/anthropic".to_string(),
            ])),
        )
        .await
        .expect("write expanded set");

    // Cursor at the index renderer; focused on the Auth field row so
    // `cycle.field.next` (bound to `l`) routes to `selector_cycle_auth_dir`.
    h.write_path(
        &oxpath!("ui", "settings", "cursor"),
        &oxpath!("settings", "index"),
    )
    .await;
    h.write_path(
        &oxpath!("ui", "settings", "focused"),
        &oxpath!("settings", "accounts", acct_comp.clone(), "auth"),
    )
    .await;

    // Frame before any cycle — initial state should render `x-api-key`
    // as the current carousel option (this is correct for the initial
    // anthropic default of `Some(XApiKey)`).
    let frame_before = render_settings_to_string(&h, 80, 24).await;
    insta::assert_snapshot!("auth_cycle_repro_before", &frame_before);

    // One forward cycle: XApiKey -> BearerToken.
    assert!(matches!(h.dispatch("l").await, KeyDispatchOutcome::Handled));

    // Direct broker read: the cycle's state mutation must have landed.
    // This is the floor — if even this fails, the bug is in the cycle
    // command, not the renderer. (Spoiler: it is the renderer.)
    let pc: ProviderConfig = h
        .client
        .read_typed(&oxpath!("config", "gate", "providers", acct_comp.clone()))
        .await
        .expect("read provider")
        .expect("provider present after cycle");
    assert_eq!(
        pc.auth,
        Some(ox_gate::AuthScheme::BearerToken),
        "one forward auth cycle from XApiKey must land on BearerToken \
         in the broker (state-mutation invariant)",
    );

    // Frame after one cycle. This is what the user actually sees.
    let frame_after_one = render_settings_to_string(&h, 80, 24).await;
    insta::assert_snapshot!("auth_cycle_repro_after_one_cycle", &frame_after_one);

    // The bug-detector. State says BearerToken; the rendered carousel
    // must reflect that. With the current `format!("{scheme:?}")`
    // formatting in `append_account_field_rows`, the label is
    // "bearertoken" (no hyphen), the renderer's `AUTH_DISPLAY.position`
    // returns None, the `unwrap_or(0)` fallback fires, and the carousel
    // re-renders as `x-api-key`.
    //
    // We check for `bearer-token` as the *current* (middle) carousel
    // option, not just its presence in the row — `bearer-token` shows
    // up as the `next` option in the buggy idx-0 fallback rendering
    // too. The carousel is `◂ {prev}  {current}  {next} ▸`, so the
    // discriminating substring when BearerToken is current is the
    // contiguous run `x-api-key  bearer-token  none` (prev=x-api-key,
    // current=bearer-token, next=none).
    assert!(
        frame_after_one.contains("x-api-key  bearer-token  none"),
        "rendered frame after one auth cycle must show `bearer-token` as \
         the *current* (middle) carousel option, with `x-api-key` as the \
         prev option and `none` as the next option (matching the broker's \
         BearerToken state at idx 1 of [x-api-key, bearer-token, none]); \
         got:\n{frame_after_one}"
    );
}

// ---------------------------------------------------------------------------
// Repro: each new connection shares the same provider record.
//
// User report: "When I added a connection named 'Test' it added a
// gate.providers.anthropic entry. When I added another with a different
// endpoint, it overwrote the endpoint and functionally edited the other
// connection."
//
// Diagnosis: `accounts_compose_commit` hardcodes `AccountConfig { provider:
// "anthropic".to_string() }` for every newly-created account. The `provider`
// field is the broker key for the provider record at
// `config/gate/providers/<provider>`. Every default-created account points
// at the same provider record, so editing one connection's endpoint mutates
// a record shared by every other "anthropic"-defaulted account.
//
// This test reproduces the shared-record shape at create-time and asserts
// the desired invariant: each account should have its own provider record.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn add_connections_have_independent_providers() {
    let h = E2eHarness::new().await;
    populate_index(&h).await;

    // Cursor at the accordion, focused on the Accounts header so the `a`
    // binding (Prefix(settings/accounts)) resolves to accounts.add.
    h.write_path(
        &oxpath!("ui", "settings", "cursor"),
        &oxpath!("settings", "index"),
    )
    .await;
    h.write_path(
        &oxpath!("ui", "settings", "focused"),
        &oxpath!("settings", "accounts"),
    )
    .await;

    // --- Create first connection "alpha" via the dispatcher.
    assert!(matches!(h.dispatch("a").await, KeyDispatchOutcome::Handled));
    for ch in "alpha".chars() {
        let key = ch.to_string();
        assert!(
            matches!(h.dispatch(&key).await, KeyDispatchOutcome::Handled),
            "dispatch returned Unhandled for {key:?}"
        );
    }
    assert!(matches!(
        h.dispatch("Enter").await,
        KeyDispatchOutcome::Handled
    ));

    let alpha_comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
    let account_alpha: AccountConfig = h
        .client
        .read_typed(&oxpath!("config", "gate", "accounts", alpha_comp.clone()))
        .await
        .expect("read alpha account record")
        .expect("alpha account record present after create");

    // Mirror what a user editing alpha's endpoint would write: stamp a
    // distinctive endpoint into the ProviderConfig at the path implied by
    // alpha's `provider` field. This isolates the test from any inline
    // field-edit machinery — we exercise only the create-time path.
    let alpha_provider_comp =
        ox_kernel::PathComponent::try_new(&account_alpha.provider).unwrap();
    h.write_typed(
        &oxpath!("config", "gate", "providers", alpha_provider_comp.clone()),
        &ProviderConfig {
            dialect: "anthropic".to_string(),
            endpoint: "https://alpha.example.com".to_string(),
            version: "2023-06-01".to_string(),
            auth: Some(ox_gate::AuthScheme::XApiKey),
        },
    )
    .await;

    // --- Create second connection "beta" via the dispatcher.
    //
    // Re-focus the Accounts header so `a` resolves to accounts.add again.
    // After the first commit the focused row sits on the new account row;
    // we want the section header so the binding picks up cleanly.
    h.write_path(
        &oxpath!("ui", "settings", "focused"),
        &oxpath!("settings", "accounts"),
    )
    .await;
    assert!(matches!(h.dispatch("a").await, KeyDispatchOutcome::Handled));
    for ch in "beta".chars() {
        let key = ch.to_string();
        assert!(
            matches!(h.dispatch(&key).await, KeyDispatchOutcome::Handled),
            "dispatch returned Unhandled for {key:?}"
        );
    }
    assert!(matches!(
        h.dispatch("Enter").await,
        KeyDispatchOutcome::Handled
    ));

    let beta_comp = ox_kernel::PathComponent::try_new("beta").unwrap();
    let account_beta: AccountConfig = h
        .client
        .read_typed(&oxpath!("config", "gate", "accounts", beta_comp.clone()))
        .await
        .expect("read beta account record")
        .expect("beta account record present after create");

    // --- Diagnostic: enumerate `config/gate/providers/` child names so the
    // failure output makes the bug shape obvious. read_subtree returns a
    // flat map keyed by absolute path; the first segment past the prefix
    // is the provider record's name.
    let providers_prefix = oxpath!("config", "gate", "providers");
    let provider_entries = h
        .client
        .read_subtree(&providers_prefix)
        .await
        .expect("read_subtree providers");
    let mut provider_child_names: Vec<String> = Vec::new();
    let prefix_len = providers_prefix.len();
    for path in provider_entries.keys() {
        if path.len() <= prefix_len {
            continue;
        }
        let segment = path.components[prefix_len].clone();
        if !provider_child_names.contains(&segment) {
            provider_child_names.push(segment);
        }
    }
    provider_child_names.sort();

    // --- The bug-detector. If alpha.provider == beta.provider, both
    // accounts reference the SAME provider record by name. Any endpoint
    // edit to one will mutate the other's view — which is exactly what
    // the user reported. Each connection should have its own provider
    // record so endpoint edits don't bleed across accounts.
    assert_ne!(
        account_alpha.provider, account_beta.provider,
        "alpha and beta must reference distinct provider records so editing \
         one connection's endpoint cannot leak into the other; \
         got account_alpha.provider={:?}, account_beta.provider={:?}, \
         providers/ child names={:?}",
        account_alpha.provider, account_beta.provider, provider_child_names,
    );

    // Belt-and-suspenders: alpha's endpoint must still match what we wrote
    // through alpha's provider path. If beta's create flow stomped on the
    // shared record (e.g., wrote a default ProviderConfig at the same
    // path) the endpoint we stamped above will be gone.
    let alpha_provider_after: ProviderConfig = h
        .client
        .read_typed(&oxpath!(
            "config",
            "gate",
            "providers",
            alpha_provider_comp.clone()
        ))
        .await
        .expect("read alpha provider after beta create")
        .expect("alpha provider record present after beta create");
    assert_eq!(
        alpha_provider_after.endpoint, "https://alpha.example.com",
        "alpha's distinctive endpoint must survive beta's creation; if it \
         has been reset to the anthropic default then beta's create wrote \
         over the shared provider record. providers/ child names={:?}",
        provider_child_names,
    );
}

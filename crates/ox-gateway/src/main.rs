//! ox-gateway entry point. Assembles the shared broker (config + secret +
//! gate + gateway/usage + gateway/completions), then serves axum on
//! 127.0.0.1:11343 (configurable via OX_GATEWAY_BIND).
//!
//! The config resolution and file-backing helpers are inlined here rather
//! than pulled from ox-cli because ox-cli's transitive dep tree includes
//! starlark_map, which has a pre-existing build break on this platform.
//! The relevant code (~120 lines) is a faithful copy of ox-cli's config.rs,
//! toml_backing.rs, and json_backing.rs.

use anyhow::Context;
use ox_broker::{BrokerStore, SyncClientAdapter};
use ox_path::oxpath;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let ox_dir = ox_dir()?;
    let toml_path = ox_dir.join("config.toml");
    let keys_path = ox_dir.join("keys.json");
    let usage_path = ox_dir.join("usage.jsonl");

    let broker = BrokerStore::new(Duration::from_secs(2));

    // config/ — ConfigStore over the same TOML ox-cli reads.
    // The figment-resolved base flat map is loaded from the file; runtime
    // overrides accumulate in memory (and save back via a separate path).
    let ox_config = resolve_config(&ox_dir);
    let base = ox_config.to_flat_map();
    let config_backing = TomlFileBacking::new(toml_path.clone());
    let config = ox_ui::ConfigStore::with_backing(base, Box::new(config_backing));
    broker.mount(oxpath!("config"), config).await;

    // secret/ — ConfigStore over keys.json (same store type, different file).
    let secret_backing = JsonFileBacking::new(keys_path.clone());
    let secret = ox_ui::ConfigStore::with_backing(
        std::collections::BTreeMap::new(),
        Box::new(secret_backing),
    );
    broker.mount(oxpath!("secret"), secret).await;

    // gate/ — GateStore wired to config + secret handles. Same wiring
    // ox-cli uses; this is the cross-process shared substrate.
    let rt = tokio::runtime::Handle::current();
    let config_adapter = SyncClientAdapter::new(broker.client().scoped("config"), rt.clone());
    let secret_adapter = SyncClientAdapter::new(broker.client().scoped("secret"), rt.clone());
    let gate = ox_gate::GateStore::new()
        .with_config(Box::new(config_adapter))
        .with_secrets(Box::new(secret_adapter));
    broker.mount(oxpath!("gate"), gate).await;

    // gateway/usage/ — JsonlFileBacking over ~/.ox/usage.jsonl.
    let usage_backing = Box::new(
        ox_store_util::JsonlFileBacking::new(&usage_path)
            .context("opening usage.jsonl backing")?,
    );
    let usage = ox_gate::UsageStore::new(usage_backing);
    broker.mount(oxpath!("gateway", "usage"), usage).await;

    // gateway/completions/ — CompletionBrokerStore with the production
    // SSE executor. Uses mount_async (the store is AsyncReader/AsyncWriter).
    let executor = Arc::new(
        ox_gate::transport::ReqwestSseExecutor::with_default_timeout()
            .map_err(anyhow::Error::msg)
            .context("constructing ReqwestSseExecutor")?,
    );
    let usage_client = broker.client().scoped("gateway/usage");
    let completions = ox_gate::CompletionBrokerStore::new(
        broker.client(),
        executor,
        usage_client,
        tokio::runtime::Handle::current(),
    );
    broker.mount_async(oxpath!("gateway", "completions"), completions).await;

    // axum
    let bind_addr =
        std::env::var("OX_GATEWAY_BIND").unwrap_or_else(|_| "127.0.0.1:11343".into());
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("binding {bind_addr}"))?;
    tracing::info!(addr = %listener.local_addr()?, "ox-gateway listening");

    let app = ox_gateway::routes::build_router(broker.client());
    axum::serve(listener, app).await.context("axum::serve")?;
    Ok(())
}

fn ox_dir() -> anyhow::Result<std::path::PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME is not set"))?;
    let dir = std::path::PathBuf::from(home).join(".ox");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

// ---------------------------------------------------------------------------
// Config resolution — inlined from ox-cli to avoid the starlark_map dep.
// Mirrors ox-cli/src/config.rs: OxConfig, GateConfig, ProviderEntry,
// AccountEntry, DefaultsConfig, and resolve_config.
// ---------------------------------------------------------------------------

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use structfs_core_store::Value;

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
struct OxConfig {
    #[serde(default)]
    gate: GateConfig,
}

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
struct GateConfig {
    #[serde(default)]
    providers: HashMap<String, ProviderEntry>,
    #[serde(default)]
    accounts: HashMap<String, AccountEntry>,
    #[serde(default)]
    defaults: DefaultsConfig,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct ProviderEntry {
    dialect: String,
    endpoint: String,
    #[serde(default)]
    version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth: Option<ox_gate::AuthScheme>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct AccountEntry {
    provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    endpoint: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct DefaultsConfig {
    #[serde(default = "default_account")]
    account: String,
    #[serde(default = "default_model")]
    model: String,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            account: default_account(),
            model: default_model(),
        }
    }
}

fn default_account() -> String {
    "anthropic".to_string()
}

fn default_model() -> String {
    "claude-sonnet-4-20250514".to_string()
}

impl OxConfig {
    /// Migrate legacy `gate.accounts.{name}.endpoint` into a provider entry.
    fn migrate_legacy_account_endpoints(&mut self) {
        let legacy: Vec<(String, String, String)> = self
            .gate
            .accounts
            .iter()
            .filter_map(|(name, entry)| {
                entry
                    .endpoint
                    .as_ref()
                    .filter(|s| !s.is_empty())
                    .map(|ep| (name.clone(), entry.provider.clone(), ep.clone()))
            })
            .collect();

        for (acct_name, prev_provider, endpoint) in legacy {
            let dialect = match prev_provider.as_str() {
                "openai" => "openai".to_string(),
                "anthropic" => "anthropic".to_string(),
                _ => prev_provider.clone(),
            };
            let provider_name = acct_name.clone();
            self.gate.providers.entry(provider_name.clone()).or_insert(ProviderEntry {
                dialect,
                endpoint,
                version: String::new(),
                auth: None,
            });
            if let Some(entry) = self.gate.accounts.get_mut(&acct_name) {
                entry.provider = provider_name;
                entry.endpoint = None;
            }
        }
    }

    /// Produce the flat BTreeMap<String, Value> that ConfigStore uses as its
    /// base layer.
    fn to_flat_map(&self) -> BTreeMap<String, Value> {
        let mut map = BTreeMap::new();

        for (name, prov) in &self.gate.providers {
            map.insert(
                format!("gate/providers/{name}/dialect"),
                Value::String(prov.dialect.clone()),
            );
            map.insert(
                format!("gate/providers/{name}/endpoint"),
                Value::String(prov.endpoint.clone()),
            );
            map.insert(
                format!("gate/providers/{name}/version"),
                Value::String(prov.version.clone()),
            );
            if let Some(ref auth) = prov.auth {
                let auth_str = match auth {
                    ox_gate::AuthScheme::XApiKey => "x-api-key",
                    ox_gate::AuthScheme::BearerToken => "bearer-token",
                    ox_gate::AuthScheme::None => "none",
                };
                map.insert(
                    format!("gate/providers/{name}/auth"),
                    Value::String(auth_str.into()),
                );
            }
        }

        for (name, entry) in &self.gate.accounts {
            map.insert(
                format!("gate/accounts/{name}/provider"),
                Value::String(entry.provider.clone()),
            );
        }

        let role = ox_types::CompletionRole {
            account: self.gate.defaults.account.clone(),
            model_id: self.gate.defaults.model.clone(),
        };
        if let Ok(role_value) = structfs_serde_store::to_value(&role) {
            map.insert("gate/completions/primary".into(), role_value);
        }
        map
    }
}

/// Resolve configuration via figment: defaults → TOML file → env vars.
fn resolve_config(config_dir: &std::path::Path) -> OxConfig {
    use figment::Figment;
    use figment::providers::{Env, Format, Toml};

    let toml_path = config_dir.join("config.toml");
    let figment = Figment::new()
        .merge(figment::providers::Serialized::defaults(OxConfig::default()))
        .merge(Toml::file(toml_path))
        .merge(Env::prefixed("OX_").split("__"));

    let mut config: OxConfig = figment.extract().expect("config extraction failed");
    config.migrate_legacy_account_endpoints();
    config
}

// ---------------------------------------------------------------------------
// File-backing helpers — inlined from ox-cli to avoid the starlark_map dep.
// Mirrors ox-cli/src/toml_backing.rs and ox-cli/src/json_backing.rs.
// ---------------------------------------------------------------------------

/// Persists a flat path-keyed BTreeMap as nested TOML.
struct TomlFileBacking {
    path: std::path::PathBuf,
}

impl TomlFileBacking {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }
}

impl ox_store_util::StoreBacking for TomlFileBacking {
    fn load(&self) -> Result<Option<Value>, structfs_core_store::Error> {
        use structfs_core_store::Error as StoreError;
        let content = match std::fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StoreError::store("toml_backing", "load", e.to_string())),
        };
        let table: toml::Table = content
            .parse()
            .map_err(|e: toml::de::Error| StoreError::store("toml_backing", "load", e.to_string()))?;
        let mut flat = BTreeMap::new();
        flatten_toml("", &toml::Value::Table(table), &mut flat);
        Ok(Some(Value::Map(flat)))
    }

    fn save(&self, value: &Value) -> Result<(), structfs_core_store::Error> {
        use structfs_core_store::Error as StoreError;
        let Value::Map(flat) = value else {
            return Err(StoreError::store("toml_backing", "save", "expected Value::Map"));
        };
        let mut root = toml::Table::new();
        for (path_key, val) in flat {
            let parts: Vec<&str> = path_key.split('/').collect();
            insert_nested_toml(&mut root, &parts, val);
        }
        let content = toml::to_string_pretty(&root)
            .map_err(|e| StoreError::store("toml_backing", "save", e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StoreError::store("toml_backing", "save", e.to_string()))?;
        }
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, content)
            .map_err(|e| StoreError::store("toml_backing", "save", e.to_string()))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| StoreError::store("toml_backing", "save", e.to_string()))?;
        Ok(())
    }
}

fn flatten_toml(prefix: &str, value: &toml::Value, out: &mut BTreeMap<String, Value>) {
    match value {
        toml::Value::Table(table) => {
            for (key, val) in table {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}/{key}")
                };
                flatten_toml(&path, val, out);
            }
        }
        toml::Value::String(s) => {
            out.insert(prefix.to_string(), Value::String(s.clone()));
        }
        toml::Value::Integer(n) => {
            out.insert(prefix.to_string(), Value::Integer(*n));
        }
        toml::Value::Boolean(b) => {
            out.insert(prefix.to_string(), Value::Bool(*b));
        }
        _ => {}
    }
}

fn insert_nested_toml(table: &mut toml::Table, parts: &[&str], value: &Value) {
    if parts.is_empty() {
        return;
    }
    if parts.len() == 1 {
        match value {
            Value::String(s) => {
                table.insert(parts[0].to_string(), toml::Value::String(s.clone()));
            }
            Value::Integer(n) => {
                table.insert(parts[0].to_string(), toml::Value::Integer(*n));
            }
            Value::Bool(b) => {
                table.insert(parts[0].to_string(), toml::Value::Boolean(*b));
            }
            _ => {}
        }
        return;
    }
    let sub = table
        .entry(parts[0].to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let toml::Value::Table(sub_table) = sub {
        insert_nested_toml(sub_table, &parts[1..], value);
    }
}

/// Persists a flat path-keyed BTreeMap as a JSON object with 0600 perms.
struct JsonFileBacking {
    path: std::path::PathBuf,
}

impl JsonFileBacking {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }
}

impl ox_store_util::StoreBacking for JsonFileBacking {
    fn load(&self) -> Result<Option<Value>, structfs_core_store::Error> {
        use structfs_core_store::Error as StoreError;
        let content = match std::fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StoreError::store("json_backing", "load", e.to_string())),
        };
        let json: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| StoreError::store("json_backing", "load", e.to_string()))?;
        let value = structfs_serde_store::json_to_value(json);
        Ok(Some(value))
    }

    fn save(&self, value: &Value) -> Result<(), structfs_core_store::Error> {
        use structfs_core_store::Error as StoreError;
        let Value::Map(_) = value else {
            return Err(StoreError::store("json_backing", "save", "expected Value::Map"));
        };
        let json = structfs_serde_store::value_to_json(value.clone());
        let content = serde_json::to_string_pretty(&json)
            .map_err(|e| StoreError::store("json_backing", "save", e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StoreError::store("json_backing", "save", e.to_string()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &content)
            .map_err(|e| StoreError::store("json_backing", "save", e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| StoreError::store("json_backing", "save", e.to_string()))?;
        }
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| StoreError::store("json_backing", "save", e.to_string()))?;
        Ok(())
    }
}

//! StructFS-native LLM transport layer for the ox agent framework.
//!
//! `ox-gate` provides codec functions for translating between the internal
//! Anthropic-format messages and various LLM provider wire formats, plus a
//! [`GateStore`] that manages provider configs, accounts, and model catalogs
//! via the StructFS Reader/Writer interface.

pub mod account;
pub mod account_test_status;
pub mod api_key;
pub mod catalog_refresh_status;
pub mod codec;
pub mod known_family;
pub mod pricing;
pub mod provider;
// Subscriptions sit on top of ox-broker, which uses
// `tokio::task::block_in_place` and so requires the multi-thread runtime —
// neither is available on wasm. ox-web has no need for the broker-side
// subscription runtime; gate it out so the wasm build remains clean.
#[cfg(not(target_arch = "wasm32"))]
pub mod subscriptions;
// `transport` uses `reqwest::blocking` and `Send`-bound async futures,
// neither of which compiles on wasm. Browser callers (ox-web) talk to
// providers directly via `fetch`/wasm-bindgen and don't need this module.
#[cfg(not(target_arch = "wasm32"))]
pub mod transport;
pub mod validation;

pub use account::AccountConfig;
pub use account_test_status::AccountTestStatus;
pub use api_key::ApiKey;
pub use catalog_refresh_status::CatalogRefreshStatus;
pub use codec::UsageInfo;
pub use known_family::{KnownFamilyEntry, known_family_metadata};
pub use ox_types::{CompletionRole, ModelInfo, ModelInfoSource};
pub use provider::{
    AuthScheme, Preset, ProviderConfig, completion_url, dialect_paths, models_url, presets,
    validate_endpoint,
};
#[cfg(not(target_arch = "wasm32"))]
pub use transport::{HttpTransport, Transport};

use ox_kernel::ToolSchema;
use std::collections::{BTreeMap, HashMap};
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Store, Value, Writer};
use structfs_serde_store::{from_value, to_value};

/// Account name handed back by `completions/primary` when no config handle
/// has been attached. Kept as a const so the gate has a working default for
/// ox-core / unit-test contexts that never seed a role.
const FALLBACK_ACCOUNT: &str = "anthropic";

/// Model id paired with [`FALLBACK_ACCOUNT`] in the same fallback role.
const FALLBACK_MODEL: &str = "claude-sonnet-4-20250514";

/// Gate store — manages providers, accounts, and model catalogs.
///
/// Mount this at `"gate"` in the namespace. Read/write paths:
///
/// - `providers/{name}` — ProviderConfig (dialect, endpoint, version)
/// - `providers/{name}/models` — model catalog for provider
/// - `accounts/{name}` — AccountConfig (provider)
/// - `accounts/{name}/key` — API key (read-only; resolved from the secrets
///   handle at `keys/{name}`, i.e. `secret/keys/{name}` at broker root)
/// - `accounts/{name}/provider` — provider name
/// - `completions/primary` — [`CompletionRole`] naming the (account, model)
///   pair the kernel should drive next; resolved from the attached config
///   handle, falling back to a built-in role when none is wired.
pub struct GateStore {
    providers: HashMap<String, ProviderConfig>,
    accounts: HashMap<String, AccountConfig>,
    catalogs: HashMap<String, Vec<ModelInfo>>,
    config: Option<Box<dyn Store + Send + Sync>>,
    /// Secrets handle. Reads `keys/{name}: ApiKey` for the
    /// `accounts/{name}/key` synthetic read path. Wired separately from
    /// `config` so secrets can persist to a different file (`keys.json`,
    /// `chmod 0600`) without touching the config TOML.
    secrets: Option<Box<dyn Store + Send + Sync>>,
}

impl GateStore {
    /// Create a new gate with default Anthropic and OpenAI providers and a
    /// default account pointing to Anthropic.
    pub fn new() -> Self {
        let mut providers = HashMap::new();
        providers.insert("anthropic".to_string(), ProviderConfig::anthropic());
        providers.insert("openai".to_string(), ProviderConfig::openai());

        let mut accounts = HashMap::new();
        accounts.insert(
            "anthropic".to_string(),
            AccountConfig {
                provider: "anthropic".to_string(),
                ..Default::default()
            },
        );
        accounts.insert(
            "openai".to_string(),
            AccountConfig {
                provider: "openai".to_string(),
                ..Default::default()
            },
        );

        Self {
            providers,
            accounts,
            catalogs: HashMap::new(),
            config: None,
            secrets: None,
        }
    }

    /// Attach a config handle for config-aware reads.
    ///
    /// When reading account-, provider-, or completion-shape fields, the
    /// GateStore checks the config handle first, falling back to local
    /// fields. API keys come from the *secrets* handle — see
    /// [`with_secrets`].
    pub fn with_config(mut self, config: Box<dyn Store + Send + Sync>) -> Self {
        self.config = Some(config);
        self
    }

    /// Attach a secrets handle for API-key reads.
    ///
    /// Expected to expose `keys/{name}: ApiKey` (i.e. `secret/keys/{name}` at
    /// broker root, scoped to the `secret` mount). When unset, all account
    /// key reads return an empty string.
    pub fn with_secrets(mut self, secrets: Box<dyn Store + Send + Sync>) -> Self {
        self.secrets = Some(secrets);
        self
    }

    /// Read a string value from the config handle at the given path.
    fn config_string(&mut self, path_str: &str) -> Option<String> {
        let config = self.config.as_mut()?;
        let path = Path::parse(path_str).ok()?;
        let record = config.read(&path).ok()??;
        match record.as_value() {
            Some(Value::String(s)) if !s.is_empty() => {
                tracing::debug!(path = path_str, "config string read from handle");
                Some(s.clone())
            }
            _ => None,
        }
    }

    /// Resolve an account by name: config handle first (user-defined), then
    /// the local hardcoded built-ins.
    fn resolve_account(&mut self, name: &str) -> Option<AccountConfig> {
        if let Some(config) = self.config.as_mut() {
            let map_path = format!("gate/accounts/{name}");
            if let Ok(path) = Path::parse(&map_path) {
                if let Ok(Some(record)) = config.read(&path) {
                    if let Some(value) = record.as_value() {
                        if let Ok(parsed) = from_value::<AccountConfig>(value.clone()) {
                            tracing::debug!(name, "account resolved from config handle");
                            return Some(parsed);
                        }
                    }
                }
            }
            if let Some(provider) = self.config_string(&format!("gate/accounts/{name}/provider")) {
                tracing::debug!(name, "account resolved from config-handle leaf");
                return Some(AccountConfig {
                    provider,
                    ..Default::default()
                });
            }
        }
        self.accounts.get(name).cloned()
    }

    /// Resolve a provider by name: config handle first (user-defined), then
    /// the local hardcoded built-ins.
    ///
    /// The config handle is expected to expose either a parsed
    /// `ProviderConfig` map at `gate/providers/{name}` or the three string
    /// leaves `dialect`/`endpoint`/`version` directly under it.
    fn resolve_provider(&mut self, name: &str) -> Option<ProviderConfig> {
        if let Some(config) = self.config.as_mut() {
            let map_path = format!("gate/providers/{name}");
            if let Ok(path) = Path::parse(&map_path) {
                if let Ok(Some(record)) = config.read(&path) {
                    if let Some(value) = record.as_value() {
                        if let Ok(parsed) = from_value::<ProviderConfig>(value.clone()) {
                            tracing::debug!(name, "provider resolved from config handle");
                            return Some(parsed);
                        }
                    }
                }
            }
            let dialect = self.config_string(&format!("gate/providers/{name}/dialect"));
            let endpoint = self.config_string(&format!("gate/providers/{name}/endpoint"));
            if let (Some(dialect), Some(endpoint)) = (dialect, endpoint) {
                let version = self
                    .config_string(&format!("gate/providers/{name}/version"))
                    .unwrap_or_default();
                let auth_str = self.config_string(&format!("gate/providers/{name}/auth"));
                let auth = auth_str.and_then(|s| match s.as_str() {
                    "x-api-key" => Some(crate::AuthScheme::XApiKey),
                    "bearer-token" => Some(crate::AuthScheme::BearerToken),
                    "none" => Some(crate::AuthScheme::None),
                    _ => None,
                });
                tracing::debug!(name, "provider resolved from config-handle leaves");
                return Some(ProviderConfig {
                    dialect,
                    endpoint,
                    version,
                    auth,
                });
            }
        }
        self.providers.get(name).cloned()
    }

    /// Read the API key for an account via the secrets handle.
    ///
    /// Returns the wrapped string when present and non-empty, `None`
    /// otherwise. The path read is `keys/{name}` on the secrets handle,
    /// which resolves to `secret/keys/{name}` at broker root.
    fn account_key(&mut self, name: &str) -> Option<String> {
        let secrets = self.secrets.as_mut()?;
        let path_str = format!("keys/{name}");
        let path = Path::parse(&path_str).ok()?;
        let record = secrets.read(&path).ok()??;
        let value = record.as_value()?;
        let key: ApiKey = from_value(value.clone()).ok()?;
        if key.is_empty() {
            tracing::debug!(account = %name, "account key read (empty)");
            None
        } else {
            tracing::debug!(account = %name, "account key read (present)");
            Some(key.0)
        }
    }

    /// Generate [`ToolSchema`]s for all accounts with API keys set.
    pub fn completion_tool_schemas(&mut self) -> Vec<ToolSchema> {
        let names: Vec<String> = self.accounts.keys().cloned().collect();
        names
            .iter()
            .filter_map(|name| {
                let has_key = self.account_key(name).is_some();
                if !has_key {
                    return None;
                }
                let account = self.accounts.get(name)?;
                let provider = self.providers.get(&account.provider)?;
                Some(ToolSchema {
                    name: format!("complete_{name}"),
                    description: format!(
                        "Send a completion to the {} account ({} dialect)",
                        name, provider.dialect,
                    ),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "prompt": {
                                "type": "string",
                                "description": "The user prompt to send"
                            },
                            "system": {
                                "type": "string",
                                "description": "Optional system prompt"
                            },
                            "model": {
                                "type": "string",
                                "description": "Model ID to use (overrides default)"
                            },
                            "max_tokens": {
                                "type": "integer",
                                "description": "Max tokens for completion (overrides default)"
                            }
                        },
                        "required": ["prompt"]
                    }),
                })
            })
            .collect()
    }

    /// Build the snapshot state: providers + accounts (keys excluded).
    fn snapshot_state(&self) -> Value {
        let mut state = BTreeMap::new();

        let mut providers_map = BTreeMap::new();
        for (name, config) in &self.providers {
            let v = to_value(config).expect("ProviderConfig always serializes");
            providers_map.insert(name.clone(), v);
        }
        state.insert("providers".to_string(), Value::Map(providers_map));

        let mut accounts_map = BTreeMap::new();
        for (name, config) in &self.accounts {
            let mut acct = BTreeMap::new();
            acct.insert(
                "provider".to_string(),
                Value::String(config.provider.clone()),
            );
            accounts_map.insert(name.clone(), Value::Map(acct));
        }
        state.insert("accounts".to_string(), Value::Map(accounts_map));

        Value::Map(state)
    }

    /// Restore the store from a snapshot state value.
    fn restore_from_snapshot(&mut self, state: Value) -> Result<(), StoreError> {
        let state_map = match state {
            Value::Map(m) => m,
            _ => {
                return Err(StoreError::store(
                    "gate",
                    "write",
                    "snapshot state must be a map",
                ));
            }
        };

        // Older snapshots may carry a top-level `defaults` map or the legacy
        // `bootstrap` field; both encoded a session-defaults shape that O2
        // retired. They are intentionally ignored here — the live state a
        // snapshot now restores is providers + accounts. CompletionRole (the
        // post-O1 replacement for the `defaults` shape) lives in the broker
        // config store, which has its own ledger entries and snapshot path.

        if let Some(providers_val) = state_map.get("providers") {
            let providers_json = structfs_serde_store::value_to_json(providers_val.clone());
            let providers: HashMap<String, ProviderConfig> = serde_json::from_value(providers_json)
                .map_err(|e| StoreError::store("gate", "write", e.to_string()))?;
            self.providers = providers;
        }

        if let Some(accounts_val) = state_map.get("accounts") {
            let mut new_accounts = HashMap::new();
            match accounts_val {
                Value::Map(accts) => {
                    for (name, acct_val) in accts {
                        let acct_json = structfs_serde_store::value_to_json(acct_val.clone());
                        let provider = acct_json
                            .get("provider")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        new_accounts.insert(
                            name.clone(),
                            AccountConfig {
                                provider,
                                ..Default::default()
                            },
                        );
                    }
                }
                _ => return Err(StoreError::store("gate", "write", "accounts must be a map")),
            }
            self.accounts = new_accounts;
        }

        Ok(())
    }
}

impl Default for GateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Reader for GateStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        if from.is_empty() {
            return Ok(None);
        }

        let first = from[0].as_str();
        match first {
            "providers" => {
                if from.len() < 2 {
                    return Ok(None);
                }
                let name = from[1].as_str().to_string();

                // Resolve provider config: prefer config handle (user-defined
                // providers seeded from OxConfig/TOML), fall back to local
                // hardcoded built-ins. Mirrors the same pattern keys use.
                let resolved = self.resolve_provider(&name);
                let Some(config) = resolved else {
                    return Ok(None);
                };

                if from.len() == 2 {
                    let value = to_value(&config)
                        .map_err(|e| StoreError::store("gate", "read", e.to_string()))?;
                    return Ok(Some(Record::parsed(value)));
                }

                let field = from[2].as_str();
                match field {
                    "dialect" => Ok(Some(Record::parsed(Value::String(config.dialect)))),
                    "endpoint" => Ok(Some(Record::parsed(Value::String(config.endpoint)))),
                    "version" => Ok(Some(Record::parsed(Value::String(config.version)))),
                    "models" => {
                        let catalog = self.catalogs.get(&name).cloned().unwrap_or_default();
                        let value = to_value(&catalog)
                            .map_err(|e| StoreError::store("gate", "read", e.to_string()))?;
                        Ok(Some(Record::parsed(value)))
                    }
                    _ => Ok(None),
                }
            }

            "accounts" => {
                if from.len() < 2 {
                    return Ok(None);
                }
                let name = from[1].as_str().to_string();

                // Keys come from the secrets handle (`secret/keys/{name}: ApiKey`).
                // Synthetic read shape — the underlying storage is a typed
                // `ApiKey` record at a path that has nothing to do with the
                // gate's namespace; the gate just exposes it under the
                // `accounts/{name}/key` shape callers already know.
                if from.len() > 2 && from[2].as_str() == "key" {
                    let key = self.account_key(&name).unwrap_or_default();
                    return Ok(Some(Record::parsed(Value::String(key))));
                }

                // Resolve account: prefer config handle (user-defined accounts
                // seeded from OxConfig/TOML via the ConfigStore), fall back to
                // local hardcoded built-ins. Same indirection shape providers
                // and keys already use — without this, accounts the user
                // creates are invisible to running threads.
                let resolved = self.resolve_account(&name);
                let Some(config) = resolved else {
                    return Ok(None);
                };

                if from.len() == 2 {
                    let value = to_value(&config)
                        .map_err(|e| StoreError::store("gate", "read", e.to_string()))?;
                    return Ok(Some(Record::parsed(value)));
                }

                let field = from[2].as_str();
                match field {
                    "provider" => Ok(Some(Record::parsed(Value::String(config.provider)))),
                    _ => Ok(None),
                }
            }

            "tools" => {
                if from.len() >= 2 && from[1].as_str() == "schemas" {
                    let schemas = self.completion_tool_schemas();
                    let value = to_value(&schemas)
                        .map_err(|e| StoreError::store("gate", "read", e.to_string()))?;
                    Ok(Some(Record::parsed(value)))
                } else {
                    Ok(None)
                }
            }

            "snapshot" => {
                let state = self.snapshot_state();
                if from.len() >= 2 {
                    match from[1].as_str() {
                        "hash" => {
                            let hash = ox_kernel::snapshot::snapshot_hash(&state);
                            Ok(Some(Record::parsed(Value::String(hash))))
                        }
                        "state" => Ok(Some(Record::parsed(state))),
                        _ => Ok(None),
                    }
                } else {
                    Ok(Some(Record::parsed(ox_kernel::snapshot::snapshot_record(
                        state,
                    ))))
                }
            }

            // Pass-through to the config handle for paths like
            // `completions/primary` that aren't held in GateStore's local
            // state. The namespace fact lives in the broker's config mount
            // at `config/gate/completions/primary`; re-prepend `gate/` (the
            // mount that routed us here) and read through the config
            // handle, which is itself scoped with the `config/` prefix.
            //
            // When no config handle is attached (ox-core unit tests, etc.)
            // and the path is `completions/primary`, fall back to a
            // built-in CompletionRole pointing at the FALLBACK_ACCOUNT /
            // FALLBACK_MODEL constants — same pair the retired session-
            // defaults shape handed out, so kernel tests that never seed a
            // role still get a usable default.
            "completions" => {
                if let Some(handle) = self.config.as_mut() {
                    let mut full = vec!["gate".to_string()];
                    full.extend(from.iter().cloned());
                    if let Ok(prefixed) = Path::try_from_components(full) {
                        if let Some(record) = handle.read(&prefixed)? {
                            return Ok(Some(record));
                        }
                    }
                }
                if from.len() == 2 && from[1].as_str() == "primary" {
                    let role = ox_types::CompletionRole {
                        account: FALLBACK_ACCOUNT.to_string(),
                        model_id: FALLBACK_MODEL.to_string(),
                    };
                    let value = to_value(&role)
                        .map_err(|e| StoreError::store("gate", "read", e.to_string()))?;
                    return Ok(Some(Record::parsed(value)));
                }
                Ok(None)
            }

            _ => Ok(None),
        }
    }
}

impl Writer for GateStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        if to.is_empty() {
            return Err(StoreError::store("gate", "write", "empty path"));
        }

        let first = to[0].as_str();
        match first {
            "providers" => {
                if to.len() < 2 {
                    return Err(StoreError::store(
                        "gate",
                        "write",
                        "providers requires a name",
                    ));
                }
                let name = to[1].as_str().to_string();

                if to.len() == 2 {
                    // Write full ProviderConfig
                    let value = match data {
                        Record::Parsed(v) => v,
                        _ => {
                            return Err(StoreError::store(
                                "gate",
                                "write",
                                "expected parsed record",
                            ));
                        }
                    };
                    let config: ProviderConfig = from_value(value)
                        .map_err(|e| StoreError::store("gate", "write", e.to_string()))?;
                    self.providers.insert(name, config);
                    return Ok(to.clone());
                }

                let field = to[2].as_str();
                match field {
                    "models" => {
                        let value = match data {
                            Record::Parsed(v) => v,
                            _ => {
                                return Err(StoreError::store(
                                    "gate",
                                    "write",
                                    "expected parsed record for models",
                                ));
                            }
                        };
                        let catalog: Vec<ModelInfo> = from_value(value)
                            .map_err(|e| StoreError::store("gate", "write", e.to_string()))?;
                        self.catalogs.insert(name, catalog);
                        Ok(to.clone())
                    }
                    _ => Err(StoreError::store(
                        "gate",
                        "write",
                        format!("unknown provider field: {field}"),
                    )),
                }
            }

            "accounts" => {
                if to.len() < 2 {
                    return Err(StoreError::store(
                        "gate",
                        "write",
                        "accounts requires a name",
                    ));
                }
                let name = to[1].as_str().to_string();

                if to.len() == 2 {
                    // Write full AccountConfig
                    let value = match data {
                        Record::Parsed(v) => v,
                        _ => {
                            return Err(StoreError::store(
                                "gate",
                                "write",
                                "expected parsed record",
                            ));
                        }
                    };
                    let config: AccountConfig = from_value(value)
                        .map_err(|e| StoreError::store("gate", "write", e.to_string()))?;
                    self.accounts.insert(name, config);
                    return Ok(to.clone());
                }

                let field = to[2].as_str();
                match field {
                    "provider" => match data {
                        Record::Parsed(Value::String(s)) => {
                            if let Some(account) = self.accounts.get_mut(&name) {
                                account.provider = s;
                            } else {
                                return Err(StoreError::store(
                                    "gate",
                                    "write",
                                    format!("no account named '{name}'"),
                                ));
                            }
                            Ok(to.clone())
                        }
                        _ => Err(StoreError::store(
                            "gate",
                            "write",
                            "expected string for provider",
                        )),
                    },
                    _ => Err(StoreError::store(
                        "gate",
                        "write",
                        format!("unknown account field: {field}"),
                    )),
                }
            }

            "snapshot" => {
                let value = match data {
                    Record::Parsed(v) => v,
                    _ => return Err(StoreError::store("gate", "write", "expected parsed record")),
                };
                let state = if to.len() >= 2 && to[1].as_str() == "state" {
                    value
                } else {
                    ox_kernel::snapshot::extract_snapshot_state(value)
                        .map_err(|e| StoreError::store("gate", "write", e))?
                };
                self.restore_from_snapshot(state)?;
                Ok(to.clone())
            }

            _ => Err(StoreError::store(
                "gate",
                "write",
                format!("unknown path: {to}"),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;
    use structfs_serde_store::{json_to_value, value_to_json};

    #[test]
    fn test_default_providers() {
        let mut gate = GateStore::new();

        // Anthropic provider exists
        let record = gate.read(&path!("providers/anthropic")).unwrap().unwrap();
        let json = match record {
            Record::Parsed(v) => value_to_json(v),
            _ => panic!("expected parsed"),
        };
        assert_eq!(json["dialect"], "anthropic");
        assert_eq!(json["endpoint"], "https://api.anthropic.com");

        // OpenAI provider exists
        let record = gate.read(&path!("providers/openai")).unwrap().unwrap();
        let json = match record {
            Record::Parsed(v) => value_to_json(v),
            _ => panic!("expected parsed"),
        };
        assert_eq!(json["dialect"], "openai");
    }

    /// Helper: build a `LocalConfig`-backed secrets handle pre-populated
    /// with the given account/key pairs at `keys/{name}: ApiKey`.
    fn secrets_with(pairs: &[(&str, &str)]) -> ox_store_util::LocalConfig {
        let mut secrets = ox_store_util::LocalConfig::new();
        for (name, key) in pairs {
            let v = to_value(&ApiKey::new(*key)).expect("ApiKey serializes");
            secrets.set(&format!("keys/{name}"), v);
        }
        secrets
    }

    #[test]
    fn test_account_key_from_secrets_handle() {
        let secrets = secrets_with(&[("anthropic", "sk-test-123")]);
        let mut gate = GateStore::new().with_secrets(Box::new(secrets));

        // Read key back — comes from secrets handle, surfaced as String
        // for backward-compatible read shape under accounts/{name}/key.
        let record = gate
            .read(&path!("accounts/anthropic/key"))
            .unwrap()
            .unwrap();
        match record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "sk-test-123"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn test_account_create() {
        let mut gate = GateStore::new();

        let config = AccountConfig {
            provider: "anthropic".to_string(),
            ..Default::default()
        };
        let value = to_value(&config).unwrap();
        gate.write(&path!("accounts/custom"), Record::parsed(value))
            .unwrap();

        // Read fields back
        let record = gate
            .read(&path!("accounts/custom/provider"))
            .unwrap()
            .unwrap();
        match record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "anthropic"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn completions_primary_falls_back_to_built_in_role_when_no_config() {
        // No config handle: completions/primary returns the built-in
        // CompletionRole baked from FALLBACK_ACCOUNT/FALLBACK_MODEL. This is
        // the contract that ox-core unit tests and other handle-less callers
        // depend on now that GateStore no longer carries session-defaults
        // state on the struct.
        let mut gate = GateStore::new();
        let record = gate.read(&path!("completions/primary")).unwrap().unwrap();
        let value = match record {
            Record::Parsed(v) => v,
            _ => panic!("expected parsed record"),
        };
        let role: ox_types::CompletionRole = from_value(value).unwrap();
        assert_eq!(role.account, "anthropic");
        assert_eq!(role.model_id, "claude-sonnet-4-20250514");
    }

    #[test]
    fn test_catalog_roundtrip() {
        let mut gate = GateStore::new();

        let models = vec![
            ModelInfo {
                id: "claude-sonnet-4-20250514".to_string(),
                display_name: "Claude Sonnet 4".to_string(),
                max_context_size: None,
                max_output_tokens: None,
                source: ModelInfoSource::Server,
            },
            ModelInfo {
                id: "claude-haiku-4-5-20251001".to_string(),
                display_name: "Claude Haiku 4.5".to_string(),
                max_context_size: None,
                max_output_tokens: None,
                source: ModelInfoSource::Server,
            },
        ];
        let value = to_value(&models).unwrap();
        gate.write(&path!("providers/anthropic/models"), Record::parsed(value))
            .unwrap();

        let record = gate
            .read(&path!("providers/anthropic/models"))
            .unwrap()
            .unwrap();
        let json = match record {
            Record::Parsed(v) => value_to_json(v),
            _ => panic!("expected parsed"),
        };
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], "claude-sonnet-4-20250514");
        // Source field round-trips as a bare PascalCase string per
        // `model_info::tests::model_info_source_serializes_as_bare_pascal_case_string`.
        // Asserting it here pins the catalog wire shape for both entries.
        assert_eq!(arr[0]["source"], "Server");
        assert_eq!(arr[1]["id"], "claude-haiku-4-5-20251001");
        assert_eq!(arr[1]["source"], "Server");
    }

    #[test]
    fn test_unknown_account_returns_none() {
        let mut gate = GateStore::new();
        assert!(gate.read(&path!("accounts/nonexistent")).unwrap().is_none());
        // Key reads for unknown accounts return empty string (no config handle)
        let record = gate
            .read(&path!("accounts/nonexistent/key"))
            .unwrap()
            .unwrap();
        match record {
            Record::Parsed(Value::String(s)) => assert!(s.is_empty()),
            _ => panic!("expected empty string"),
        }
    }

    #[test]
    fn test_tools_schemas_empty_without_keys() {
        let mut gate = GateStore::new();
        let record = gate.read(&path!("tools/schemas")).unwrap().unwrap();
        let json = match record {
            Record::Parsed(v) => value_to_json(v),
            _ => panic!("expected parsed"),
        };
        assert_eq!(json, serde_json::json!([]));
    }

    #[test]
    fn test_tools_schemas_with_keys() {
        let secrets = secrets_with(&[("anthropic", "sk-test")]);
        let mut gate = GateStore::new().with_secrets(Box::new(secrets));

        let record = gate.read(&path!("tools/schemas")).unwrap().unwrap();
        let json = match record {
            Record::Parsed(v) => value_to_json(v),
            _ => panic!("expected parsed"),
        };
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "complete_anthropic");
    }

    // -- Snapshot tests --

    fn unwrap_value(record: Record) -> Value {
        match record {
            Record::Parsed(v) => v,
            _ => panic!("expected parsed record"),
        }
    }

    #[test]
    fn snapshot_read_returns_hash_and_state() {
        let mut gate = GateStore::new();
        let val = unwrap_value(gate.read(&path!("snapshot")).unwrap().unwrap());
        match &val {
            Value::Map(m) => {
                let hash = match m.get("hash").unwrap() {
                    Value::String(s) => s.clone(),
                    _ => panic!("expected string hash"),
                };
                assert_eq!(hash.len(), 16);
                let state = m.get("state").unwrap();
                match state {
                    Value::Map(sm) => {
                        // O2: `defaults` is gone; the live snapshot keys are
                        // providers + accounts.
                        assert!(!sm.contains_key("defaults"));
                        assert!(sm.contains_key("providers"));
                        assert!(sm.contains_key("accounts"));
                        let accounts = match sm.get("accounts").unwrap() {
                            Value::Map(a) => a,
                            _ => panic!("expected map"),
                        };
                        for acct in accounts.values() {
                            let acct_json = value_to_json(acct.clone());
                            assert!(
                                acct_json.get("key").is_none(),
                                "API keys must be excluded from snapshot"
                            );
                        }
                    }
                    _ => panic!("expected map state"),
                }
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn snapshot_read_hash_only() {
        let mut gate = GateStore::new();
        let val = unwrap_value(gate.read(&path!("snapshot/hash")).unwrap().unwrap());
        match val {
            Value::String(h) => assert_eq!(h.len(), 16),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn snapshot_read_state_only() {
        let mut gate = GateStore::new();
        let val = unwrap_value(gate.read(&path!("snapshot/state")).unwrap().unwrap());
        match val {
            Value::Map(m) => {
                // O2: `defaults` is gone; the live snapshot keys are
                // providers + accounts.
                assert!(!m.contains_key("defaults"));
                assert!(m.contains_key("providers"));
                assert!(m.contains_key("accounts"));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn snapshot_excludes_api_keys() {
        let secrets = secrets_with(&[("anthropic", "sk-secret")]);
        let mut gate = GateStore::new().with_secrets(Box::new(secrets));

        let val = unwrap_value(gate.read(&path!("snapshot/state")).unwrap().unwrap());
        let json = value_to_json(val);
        let accounts = &json["accounts"];
        for (_name, acct) in accounts.as_object().unwrap() {
            assert!(
                acct.get("key").is_none(),
                "API keys must not appear in snapshot"
            );
        }
    }

    #[test]
    fn snapshot_write_restores_state() {
        let mut gate = GateStore::new();

        // Older snapshots may carry a `defaults` map at top level — the gate
        // ignores it now (the kernel reads CompletionRole from the broker
        // config store instead). The post-O2 contract is that providers and
        // accounts roundtrip; the rest of the prior `defaults` payload is
        // dropped on the floor.
        let state_json = serde_json::json!({
            "defaults": {
                "account": "openai",
                "model": "gpt-4o",
                "max_tokens": 8192
            },
            "providers": {
                "openai": {
                    "dialect": "openai",
                    "endpoint": "https://api.openai.com/v1/chat/completions",
                    "version": ""
                }
            },
            "accounts": {
                "openai": {
                    "provider": "openai"
                }
            }
        });
        let state = json_to_value(state_json);
        let mut snap_map = std::collections::BTreeMap::new();
        snap_map.insert("state".to_string(), state);

        gate.write(&path!("snapshot"), Record::parsed(Value::Map(snap_map)))
            .unwrap();

        assert!(gate.read(&path!("providers/anthropic")).unwrap().is_none());
        assert!(gate.read(&path!("providers/openai")).unwrap().is_some());
        assert!(gate.read(&path!("accounts/anthropic")).unwrap().is_none());

        let val = unwrap_value(gate.read(&path!("accounts/openai/key")).unwrap().unwrap());
        match val {
            Value::String(s) => assert!(s.is_empty(), "keys should be empty after restore"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn snapshot_restores_legacy_bootstrap_field() {
        // Pre-O2 snapshots wrote a top-level `bootstrap` string the gate
        // restored into the session-defaults account slot. With that slot
        // gone, the field is silently ignored — but the surrounding
        // providers/accounts payload must still roundtrip. This test pins
        // that backwards-compatible no-op so we don't regress to an error
        // path.
        let mut gate = GateStore::new();
        let state_json = serde_json::json!({
            "bootstrap": "openai",
            "providers": {
                "openai": {
                    "dialect": "openai",
                    "endpoint": "https://api.openai.com/v1/chat/completions",
                    "version": ""
                }
            },
            "accounts": {
                "openai": {
                    "provider": "openai"
                }
            }
        });
        let state = json_to_value(state_json);
        gate.write(&path!("snapshot/state"), Record::parsed(state))
            .unwrap();

        assert!(gate.read(&path!("providers/openai")).unwrap().is_some());
        assert!(gate.read(&path!("accounts/openai")).unwrap().is_some());
    }

    #[test]
    fn snapshot_write_via_state_path() {
        let mut gate = GateStore::new();
        let state_json = serde_json::json!({
            "providers": {
                "openai": {
                    "dialect": "openai",
                    "endpoint": "https://api.openai.com/v1/chat/completions",
                    "version": ""
                }
            },
            "accounts": {
                "openai": {
                    "provider": "openai"
                }
            }
        });
        let state = json_to_value(state_json);
        gate.write(&path!("snapshot/state"), Record::parsed(state))
            .unwrap();

        assert!(gate.read(&path!("providers/anthropic")).unwrap().is_none());
        assert!(gate.read(&path!("providers/openai")).unwrap().is_some());
        assert!(gate.read(&path!("accounts/openai")).unwrap().is_some());
    }

    // -- Config handle tests --

    #[test]
    fn config_handle_overrides_completions_primary() {
        // Replaces the pre-O2 `config_handle_overrides_defaults_model` test.
        // CompletionRole written under the broker-relative
        // `gate/completions/primary` is what `gate.read("completions/primary")`
        // resolves through the config handle, beating the built-in fallback
        // baked from FALLBACK_ACCOUNT/FALLBACK_MODEL.
        use ox_store_util::LocalConfig;
        let mut config = LocalConfig::new();
        let role = ox_types::CompletionRole {
            account: "openai".to_string(),
            model_id: "gpt-4o-mini".to_string(),
        };
        let value = to_value(&role).unwrap();
        config.set("gate/completions/primary", value);

        let mut gate = GateStore::new().with_config(Box::new(config));
        let record = gate.read(&path!("completions/primary")).unwrap().unwrap();
        let resolved: ox_types::CompletionRole = match record {
            Record::Parsed(v) => from_value(v).unwrap(),
            _ => panic!("expected parsed record"),
        };
        assert_eq!(resolved.account, "openai");
        assert_eq!(resolved.model_id, "gpt-4o-mini");
    }

    #[test]
    fn secrets_handle_provides_any_account_key() {
        let secrets = secrets_with(&[("anthropic", "config-key-123")]);
        let mut gate = GateStore::new().with_secrets(Box::new(secrets));
        let record = gate
            .read(&path!("accounts/anthropic/key"))
            .unwrap()
            .unwrap();
        match record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "config-key-123"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn secrets_handle_overrides_non_bootstrap_account_key() {
        let secrets = secrets_with(&[("openai", "sk-openai-secret")]);
        let mut gate = GateStore::new().with_secrets(Box::new(secrets));
        // The built-in account is "anthropic", but secrets provides an
        // openai key. The synthetic accounts/{name}/key read still surfaces
        // it without needing a primary completion role.
        let record = gate.read(&path!("accounts/openai/key")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "sk-openai-secret"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn secrets_handle_populates_completion_schemas_any_account() {
        let secrets = secrets_with(&[("openai", "sk-from-secrets")]);
        let mut gate = GateStore::new().with_secrets(Box::new(secrets));
        let schemas = gate.completion_tool_schemas();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name, "complete_openai");
    }

    #[test]
    fn empty_api_key_in_secrets_treated_as_absent() {
        // An empty `ApiKey` at `secret/keys/{name}` must NOT show up as a
        // schema-eligible account. Without this guard, the migration could
        // round-trip an empty key file into an "account ready to call" lie.
        let secrets = secrets_with(&[("openai", "")]);
        let mut gate = GateStore::new().with_secrets(Box::new(secrets));
        let schemas = gate.completion_tool_schemas();
        assert!(
            schemas.is_empty(),
            "empty ApiKey should not enable a completion schema, got: {:?}",
            schemas.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn defaults_path_is_no_longer_routed() {
        // O2 deleted the `defaults/*` arm. Reads return None (the trailing
        // catch-all in match), and writes return an "unknown path" error.
        // This test guards the deletion: any reintroduction of a session-
        // defaults shaped surface should be an explicit, deliberate change.
        let mut gate = GateStore::new();
        assert!(gate.read(&path!("defaults/model")).unwrap().is_none());
        assert!(gate.read(&path!("defaults/account")).unwrap().is_none());
        assert!(gate.read(&path!("defaults/max_tokens")).unwrap().is_none());

        let err = gate
            .write(
                &path!("defaults/model"),
                Record::parsed(Value::String("gpt-4o".into())),
            )
            .unwrap_err();
        assert!(err.to_string().contains("unknown path"));
    }
}

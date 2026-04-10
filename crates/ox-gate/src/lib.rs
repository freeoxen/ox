//! StructFS-native LLM transport layer for the ox agent framework.
//!
//! `ox-gate` provides codec functions for translating between the internal
//! Anthropic-format messages and various LLM provider wire formats, plus a
//! [`GateStore`] that manages provider configs, accounts, and model catalogs
//! via the StructFS Reader/Writer interface.

pub mod account;
pub mod codec;
pub mod provider;
pub mod tools;

pub use account::AccountConfig;
pub use codec::UsageInfo;
pub use provider::ProviderConfig;
pub use tools::completion_tool;

#[allow(deprecated)] // Tool pending migration to ToolStore
use ox_kernel::{ModelInfo, Tool, ToolSchema};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Store, Value, Writer};
use structfs_serde_store::{from_value, to_value};

/// Session defaults — which account, model, and token limit to use.
#[derive(Debug, Clone)]
struct Defaults {
    account: String,
    model: String,
    max_tokens: u32,
}

impl Defaults {
    fn new() -> Self {
        Self {
            account: "anthropic".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            max_tokens: 4096,
        }
    }
}

/// Gate store — manages providers, accounts, model catalogs, and session defaults.
///
/// Mount this at `"gate"` in the namespace. Read/write paths:
///
/// - `providers/{name}` — ProviderConfig (dialect, endpoint, version)
/// - `providers/{name}/models` — model catalog for provider
/// - `accounts/{name}` — AccountConfig (provider)
/// - `accounts/{name}/key` — API key (read-only, from config handle)
/// - `accounts/{name}/provider` — provider name
/// - `defaults/account` — name of the default account
/// - `defaults/model` — default model ID (falls back to config handle)
/// - `defaults/max_tokens` — default token limit (falls back to config handle)
pub struct GateStore {
    providers: HashMap<String, ProviderConfig>,
    accounts: HashMap<String, AccountConfig>,
    defaults: Defaults,
    catalogs: HashMap<String, Vec<ModelInfo>>,
    config: Option<Box<dyn Store + Send + Sync>>,
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
            },
        );
        accounts.insert(
            "openai".to_string(),
            AccountConfig {
                provider: "openai".to_string(),
            },
        );

        Self {
            providers,
            accounts,
            defaults: Defaults::new(),
            catalogs: HashMap::new(),
            config: None,
        }
    }

    /// Attach a config handle for config-aware reads.
    ///
    /// When reading defaults and account keys, GateStore checks the config
    /// handle first, falling back to local fields.
    pub fn with_config(mut self, config: Box<dyn Store + Send + Sync>) -> Self {
        self.config = Some(config);
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

    /// Read an integer value from the config handle at the given path.
    fn config_integer(&mut self, path_str: &str) -> Option<i64> {
        let config = self.config.as_mut()?;
        let path = Path::parse(path_str).ok()?;
        let record = config.read(&path).ok()??;
        match record.as_value() {
            Some(Value::Integer(n)) => {
                tracing::debug!(
                    path = path_str,
                    value = n,
                    "config integer read from handle"
                );
                Some(*n)
            }
            _ => None,
        }
    }

    /// Generate [`ToolSchema`]s for all accounts with API keys set.
    pub fn completion_tool_schemas(&mut self) -> Vec<ToolSchema> {
        let names: Vec<String> = self.accounts.keys().cloned().collect();
        names
            .iter()
            .filter_map(|name| {
                let has_key = self
                    .config_string(&format!("gate/accounts/{name}/key"))
                    .is_some();
                if !has_key {
                    return None;
                }
                let account = self.accounts.get(name)?;
                let provider = self.providers.get(&account.provider)?;
                Some(tools::completion_tool_schema(name, provider))
            })
            .collect()
    }

    /// Create completion tool instances for all accounts with API keys set.
    ///
    /// `send` is a synchronous function that sends a [`ox_kernel::CompletionRequest`]
    /// and returns parsed [`ox_kernel::StreamEvent`]s.
    #[allow(deprecated)] // Tool/FnTool pending migration to ToolStore
    pub fn create_completion_tools(&mut self, send: Arc<tools::SendFn>) -> Vec<Box<dyn Tool>> {
        let default_model = self
            .config_string("gate/defaults/model")
            .unwrap_or_else(|| self.defaults.model.clone());
        let default_max_tokens = self
            .config_integer("gate/defaults/max_tokens")
            .map(|n| n as u32)
            .unwrap_or(self.defaults.max_tokens);

        let names: Vec<String> = self.accounts.keys().cloned().collect();
        names
            .iter()
            .filter_map(|name| {
                let has_key = self
                    .config_string(&format!("gate/accounts/{name}/key"))
                    .is_some();
                if !has_key {
                    return None;
                }
                let account = self.accounts.get(name)?;
                let provider = self.providers.get(&account.provider)?;
                Some(Box::new(tools::completion_tool(
                    name.clone(),
                    provider,
                    default_model.clone(),
                    default_max_tokens,
                    send.clone(),
                )) as Box<dyn Tool>)
            })
            .collect()
    }

    /// Build the snapshot state: defaults + providers + accounts (keys excluded).
    fn snapshot_state(&self) -> Value {
        let mut state = BTreeMap::new();

        let mut defaults_map = BTreeMap::new();
        defaults_map.insert(
            "account".to_string(),
            Value::String(self.defaults.account.clone()),
        );
        defaults_map.insert(
            "model".to_string(),
            Value::String(self.defaults.model.clone()),
        );
        defaults_map.insert(
            "max_tokens".to_string(),
            Value::Integer(self.defaults.max_tokens as i64),
        );
        state.insert("defaults".to_string(), Value::Map(defaults_map));

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

        if let Some(Value::Map(defaults)) = state_map.get("defaults") {
            if let Some(Value::String(a)) = defaults.get("account") {
                self.defaults.account = a.clone();
            }
            if let Some(Value::String(m)) = defaults.get("model") {
                self.defaults.model = m.clone();
            }
            if let Some(Value::Integer(n)) = defaults.get("max_tokens") {
                self.defaults.max_tokens = *n as u32;
            }
        }
        // Backwards compat: old snapshots have "bootstrap" at top level
        if let Some(Value::String(b)) = state_map.get("bootstrap") {
            self.defaults.account = b.clone();
        }

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
                        new_accounts.insert(name.clone(), AccountConfig { provider });
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

        let first = from.components[0].as_str();
        match first {
            "defaults" => {
                if from.components.len() < 2 {
                    return Ok(None);
                }
                let field = from.components[1].as_str();
                match field {
                    "account" => {
                        if let Some(s) = self.config_string("gate/defaults/account") {
                            tracing::debug!(account = %s, "defaults/account from config");
                            return Ok(Some(Record::parsed(Value::String(s))));
                        }
                        tracing::debug!(account = %self.defaults.account, "defaults/account from local");
                        Ok(Some(Record::parsed(Value::String(
                            self.defaults.account.clone(),
                        ))))
                    }
                    "model" => {
                        if let Some(s) = self.config_string("gate/defaults/model") {
                            tracing::debug!(model = %s, "defaults/model from config");
                            return Ok(Some(Record::parsed(Value::String(s))));
                        }
                        tracing::debug!(model = %self.defaults.model, "defaults/model from local");
                        Ok(Some(Record::parsed(Value::String(
                            self.defaults.model.clone(),
                        ))))
                    }
                    "max_tokens" => {
                        if let Some(n) = self.config_integer("gate/defaults/max_tokens") {
                            tracing::debug!(max_tokens = n, "defaults/max_tokens from config");
                            return Ok(Some(Record::parsed(Value::Integer(n))));
                        }
                        tracing::debug!(
                            max_tokens = self.defaults.max_tokens,
                            "defaults/max_tokens from local"
                        );
                        Ok(Some(Record::parsed(Value::Integer(
                            self.defaults.max_tokens as i64,
                        ))))
                    }
                    _ => Ok(None),
                }
            }

            "providers" => {
                if from.components.len() < 2 {
                    return Ok(None);
                }
                let name = from.components[1].as_str();
                let Some(config) = self.providers.get(name) else {
                    return Ok(None);
                };

                if from.components.len() == 2 {
                    let value = to_value(config)
                        .map_err(|e| StoreError::store("gate", "read", e.to_string()))?;
                    return Ok(Some(Record::parsed(value)));
                }

                let field = from.components[2].as_str();
                match field {
                    "dialect" => Ok(Some(Record::parsed(Value::String(config.dialect.clone())))),
                    "endpoint" => Ok(Some(Record::parsed(Value::String(config.endpoint.clone())))),
                    "version" => Ok(Some(Record::parsed(Value::String(config.version.clone())))),
                    "models" => {
                        let catalog = self.catalogs.get(name).cloned().unwrap_or_default();
                        let value = to_value(&catalog)
                            .map_err(|e| StoreError::store("gate", "read", e.to_string()))?;
                        Ok(Some(Record::parsed(value)))
                    }
                    _ => Ok(None),
                }
            }

            "accounts" => {
                if from.components.len() < 2 {
                    return Ok(None);
                }
                let name = from.components[1].as_str();

                // Keys come from config handle only (key files/env vars injected there)
                if from.components.len() > 2 && from.components[2].as_str() == "key" {
                    if let Some(k) = self.config_string(&format!("gate/accounts/{name}/key")) {
                        tracing::debug!(account = name, "account key read (present)");
                        return Ok(Some(Record::parsed(Value::String(k))));
                    }
                    tracing::debug!(account = name, "account key read (empty)");
                    return Ok(Some(Record::parsed(Value::String(String::new()))));
                }

                let Some(config) = self.accounts.get(name) else {
                    return Ok(None);
                };

                if from.components.len() == 2 {
                    let value = to_value(config)
                        .map_err(|e| StoreError::store("gate", "read", e.to_string()))?;
                    return Ok(Some(Record::parsed(value)));
                }

                let field = from.components[2].as_str();
                match field {
                    "provider" => Ok(Some(Record::parsed(Value::String(config.provider.clone())))),
                    _ => Ok(None),
                }
            }

            "tools" => {
                if from.components.len() >= 2 && from.components[1].as_str() == "schemas" {
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
                if from.components.len() >= 2 {
                    match from.components[1].as_str() {
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

            _ => Ok(None),
        }
    }
}

impl Writer for GateStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        if to.is_empty() {
            return Err(StoreError::store("gate", "write", "empty path"));
        }

        let first = to.components[0].as_str();
        match first {
            "defaults" => {
                if to.components.len() < 2 {
                    return Err(StoreError::store(
                        "gate",
                        "write",
                        "defaults requires a field name",
                    ));
                }
                let field = to.components[1].as_str();
                match field {
                    "account" => match data {
                        Record::Parsed(Value::String(s)) => {
                            self.defaults.account = s;
                            Ok(to.clone())
                        }
                        _ => Err(StoreError::store(
                            "gate",
                            "write",
                            "expected string for defaults/account",
                        )),
                    },
                    "model" => match data {
                        Record::Parsed(Value::String(s)) => {
                            self.defaults.model = s;
                            Ok(to.clone())
                        }
                        _ => Err(StoreError::store(
                            "gate",
                            "write",
                            "expected string for defaults/model",
                        )),
                    },
                    "max_tokens" => match data {
                        Record::Parsed(Value::Integer(n)) => {
                            self.defaults.max_tokens = n as u32;
                            Ok(to.clone())
                        }
                        _ => Err(StoreError::store(
                            "gate",
                            "write",
                            "expected integer for defaults/max_tokens",
                        )),
                    },
                    _ => Err(StoreError::store(
                        "gate",
                        "write",
                        format!("unknown defaults field: {field}"),
                    )),
                }
            }

            "providers" => {
                if to.components.len() < 2 {
                    return Err(StoreError::store(
                        "gate",
                        "write",
                        "providers requires a name",
                    ));
                }
                let name = to.components[1].as_str().to_string();

                if to.components.len() == 2 {
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

                let field = to.components[2].as_str();
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
                if to.components.len() < 2 {
                    return Err(StoreError::store(
                        "gate",
                        "write",
                        "accounts requires a name",
                    ));
                }
                let name = to.components[1].as_str().to_string();

                if to.components.len() == 2 {
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

                let field = to.components[2].as_str();
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
                let state = if to.components.len() >= 2 && to.components[1].as_str() == "state" {
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
        assert_eq!(json["endpoint"], "https://api.anthropic.com/v1/messages");

        // OpenAI provider exists
        let record = gate.read(&path!("providers/openai")).unwrap().unwrap();
        let json = match record {
            Record::Parsed(v) => value_to_json(v),
            _ => panic!("expected parsed"),
        };
        assert_eq!(json["dialect"], "openai");
    }

    #[test]
    fn test_account_key_from_config_handle() {
        use ox_store_util::LocalConfig;
        let mut config = LocalConfig::new();
        config.set(
            "gate/accounts/anthropic/key",
            Value::String("sk-test-123".into()),
        );
        let mut gate = GateStore::new().with_config(Box::new(config));

        // Read key back — comes from config handle
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
    fn test_defaults_roundtrip() {
        let mut gate = GateStore::new();

        // Default account is "anthropic"
        let record = gate.read(&path!("defaults/account")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "anthropic"),
            _ => panic!("expected string"),
        }

        // Default model
        let record = gate.read(&path!("defaults/model")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "claude-sonnet-4-20250514"),
            _ => panic!("expected string"),
        }

        // Default max_tokens
        let record = gate.read(&path!("defaults/max_tokens")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::Integer(n)) => assert_eq!(n, 4096),
            _ => panic!("expected integer"),
        }

        // Set account to "openai"
        gate.write(
            &path!("defaults/account"),
            Record::parsed(Value::String("openai".to_string())),
        )
        .unwrap();

        let record = gate.read(&path!("defaults/account")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "openai"),
            _ => panic!("expected string"),
        }

        // Set model
        gate.write(
            &path!("defaults/model"),
            Record::parsed(Value::String("gpt-4o".to_string())),
        )
        .unwrap();

        let record = gate.read(&path!("defaults/model")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "gpt-4o"),
            _ => panic!("expected string"),
        }

        // Set max_tokens
        gate.write(
            &path!("defaults/max_tokens"),
            Record::parsed(Value::Integer(8192)),
        )
        .unwrap();

        let record = gate.read(&path!("defaults/max_tokens")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::Integer(n)) => assert_eq!(n, 8192),
            _ => panic!("expected integer"),
        }
    }

    #[test]
    fn test_catalog_roundtrip() {
        let mut gate = GateStore::new();

        let models = vec![
            ModelInfo {
                id: "claude-sonnet-4-20250514".to_string(),
                display_name: "Claude Sonnet 4".to_string(),
            },
            ModelInfo {
                id: "claude-haiku-4-5-20251001".to_string(),
                display_name: "Claude Haiku 4.5".to_string(),
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
        use ox_store_util::LocalConfig;
        let mut config = LocalConfig::new();
        config.set(
            "gate/accounts/anthropic/key",
            Value::String("sk-test".into()),
        );
        let mut gate = GateStore::new().with_config(Box::new(config));

        let record = gate.read(&path!("tools/schemas")).unwrap().unwrap();
        let json = match record {
            Record::Parsed(v) => value_to_json(v),
            _ => panic!("expected parsed"),
        };
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "complete_anthropic");
    }

    #[test]
    fn test_create_completion_tools() {
        use ox_store_util::LocalConfig;
        let mut config = LocalConfig::new();
        config.set(
            "gate/accounts/openai/key",
            Value::String("sk-openai".into()),
        );
        let mut gate = GateStore::new().with_config(Box::new(config));

        let send: Arc<tools::SendFn> = Arc::new(|_| Ok(vec![]));
        let tools = gate.create_completion_tools(send);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "complete_openai");
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
                        assert!(sm.contains_key("defaults"));
                        assert!(sm.contains_key("providers"));
                        assert!(sm.contains_key("accounts"));
                        let accounts = match sm.get("accounts").unwrap() {
                            Value::Map(a) => a,
                            _ => panic!("expected map"),
                        };
                        for (_name, acct) in accounts {
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
                assert!(m.contains_key("defaults"));
                assert!(m.contains_key("providers"));
                assert!(m.contains_key("accounts"));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn snapshot_excludes_api_keys() {
        use ox_store_util::LocalConfig;
        let mut config = LocalConfig::new();
        config.set(
            "gate/accounts/anthropic/key",
            Value::String("sk-secret".into()),
        );
        let mut gate = GateStore::new().with_config(Box::new(config));

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

        let val = unwrap_value(gate.read(&path!("defaults/account")).unwrap().unwrap());
        match val {
            Value::String(s) => assert_eq!(s, "openai"),
            _ => panic!("expected string"),
        }

        let val = unwrap_value(gate.read(&path!("defaults/model")).unwrap().unwrap());
        match val {
            Value::String(s) => assert_eq!(s, "gpt-4o"),
            _ => panic!("expected string"),
        }

        let val = unwrap_value(gate.read(&path!("defaults/max_tokens")).unwrap().unwrap());
        match val {
            Value::Integer(n) => assert_eq!(n, 8192),
            _ => panic!("expected integer"),
        }

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

        // Legacy "bootstrap" should populate defaults.account
        let val = unwrap_value(gate.read(&path!("defaults/account")).unwrap().unwrap());
        match val {
            Value::String(s) => assert_eq!(s, "openai"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn snapshot_write_via_state_path() {
        let mut gate = GateStore::new();
        let state_json = serde_json::json!({
            "defaults": {
                "account": "openai",
                "model": "gpt-4o",
                "max_tokens": 4096
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
        gate.write(&path!("snapshot/state"), Record::parsed(state))
            .unwrap();

        let val = unwrap_value(gate.read(&path!("defaults/account")).unwrap().unwrap());
        match val {
            Value::String(s) => assert_eq!(s, "openai"),
            _ => panic!("expected string"),
        }
    }

    // -- Config handle tests --

    #[test]
    fn config_handle_overrides_defaults_model() {
        use ox_store_util::LocalConfig;
        let mut config = LocalConfig::new();
        config.set("gate/defaults/model", Value::String("config-model".into()));
        let mut gate = GateStore::new().with_config(Box::new(config));
        let record = gate.read(&path!("defaults/model")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "config-model"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn config_handle_overrides_defaults_max_tokens() {
        use ox_store_util::LocalConfig;
        let mut config = LocalConfig::new();
        config.set("gate/defaults/max_tokens", Value::Integer(16384));
        let mut gate = GateStore::new().with_config(Box::new(config));
        let record = gate.read(&path!("defaults/max_tokens")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::Integer(n)) => assert_eq!(n, 16384),
            _ => panic!("expected integer"),
        }
    }

    #[test]
    fn config_handle_overrides_any_account_key() {
        use ox_store_util::LocalConfig;
        let mut config = LocalConfig::new();
        config.set(
            "gate/accounts/anthropic/key",
            Value::String("config-key-123".into()),
        );
        let mut gate = GateStore::new().with_config(Box::new(config));
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
    fn config_handle_overrides_non_bootstrap_account_key() {
        use ox_store_util::LocalConfig;
        let mut config = LocalConfig::new();
        config.set(
            "gate/accounts/openai/key",
            Value::String("sk-openai-config".into()),
        );
        let mut gate = GateStore::new().with_config(Box::new(config));
        // defaults.account is "anthropic", but config provides openai key
        let record = gate.read(&path!("accounts/openai/key")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "sk-openai-config"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn config_key_populates_completion_schemas_any_account() {
        use ox_store_util::LocalConfig;
        let mut config = LocalConfig::new();
        config.set(
            "gate/accounts/openai/key",
            Value::String("sk-from-config".into()),
        );
        let mut gate = GateStore::new().with_config(Box::new(config));
        let schemas = gate.completion_tool_schemas();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name, "complete_openai");
    }

    #[test]
    fn config_handle_falls_back_to_local_defaults() {
        let mut gate = GateStore::new();
        // No config handle — should return local defaults
        let record = gate.read(&path!("defaults/model")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::String(s)) => {
                assert_eq!(s, "claude-sonnet-4-20250514");
            }
            _ => panic!("expected string"),
        }

        let record = gate.read(&path!("defaults/max_tokens")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::Integer(n)) => assert_eq!(n, 4096),
            _ => panic!("expected integer"),
        }
    }
}

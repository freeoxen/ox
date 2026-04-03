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
pub use tools::CompletionTool;

use ox_kernel::{ModelInfo, Tool, ToolSchema};
use std::collections::HashMap;
use std::sync::Arc;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};
use structfs_serde_store::{from_value, to_value};

/// Gate store — manages providers, accounts, and model catalogs.
///
/// Mount this at `"gate"` in the namespace. Read/write paths:
///
/// - `providers/{name}` — ProviderConfig (dialect, endpoint, version)
/// - `providers/{name}/models` — model catalog for provider
/// - `accounts/{name}` — AccountConfig (provider, key, model)
/// - `accounts/{name}/key` — API key
/// - `accounts/{name}/provider` — provider name
/// - `accounts/{name}/model` — default model
/// - `bootstrap` — name of the active account
pub struct GateStore {
    providers: HashMap<String, ProviderConfig>,
    accounts: HashMap<String, AccountConfig>,
    bootstrap: String,
    catalogs: HashMap<String, Vec<ModelInfo>>,
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
                key: String::new(),
                model: "claude-sonnet-4-20250514".to_string(),
            },
        );
        accounts.insert(
            "openai".to_string(),
            AccountConfig {
                provider: "openai".to_string(),
                key: String::new(),
                model: "gpt-4o".to_string(),
            },
        );

        Self {
            providers,
            accounts,
            bootstrap: "anthropic".to_string(),
            catalogs: HashMap::new(),
        }
    }

    /// Generate [`ToolSchema`]s for all accounts with API keys set.
    pub fn completion_tool_schemas(&self) -> Vec<ToolSchema> {
        self.accounts
            .iter()
            .filter(|(_, account)| !account.key.is_empty())
            .filter_map(|(name, account)| {
                let provider = self.providers.get(&account.provider)?;
                Some(CompletionTool::schema_for(name, provider))
            })
            .collect()
    }

    /// Create [`CompletionTool`] instances for all accounts with API keys set.
    ///
    /// `send` is a synchronous function that sends a [`ox_kernel::CompletionRequest`]
    /// and returns parsed [`ox_kernel::StreamEvent`]s.
    pub fn create_completion_tools(&self, send: Arc<tools::SendFn>) -> Vec<Box<dyn Tool>> {
        self.accounts
            .iter()
            .filter(|(_, account)| !account.key.is_empty())
            .filter_map(|(name, account)| {
                let provider = self.providers.get(&account.provider)?;
                Some(Box::new(CompletionTool::new(
                    name.clone(),
                    account,
                    provider,
                    send.clone(),
                )) as Box<dyn Tool>)
            })
            .collect()
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
            "bootstrap" => Ok(Some(Record::parsed(Value::String(self.bootstrap.clone())))),

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
                    "key" => Ok(Some(Record::parsed(Value::String(config.key.clone())))),
                    "provider" => Ok(Some(Record::parsed(Value::String(config.provider.clone())))),
                    "model" => Ok(Some(Record::parsed(Value::String(config.model.clone())))),
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
            "bootstrap" => match data {
                Record::Parsed(Value::String(s)) => {
                    self.bootstrap = s;
                    Ok(to.clone())
                }
                _ => Err(StoreError::store(
                    "gate",
                    "write",
                    "expected string for bootstrap",
                )),
            },

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
                    "key" => match data {
                        Record::Parsed(Value::String(s)) => {
                            if let Some(account) = self.accounts.get_mut(&name) {
                                account.key = s;
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
                            "expected string for key",
                        )),
                    },
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
                    "model" => match data {
                        Record::Parsed(Value::String(s)) => {
                            if let Some(account) = self.accounts.get_mut(&name) {
                                account.model = s;
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
                            "expected string for model",
                        )),
                    },
                    _ => Err(StoreError::store(
                        "gate",
                        "write",
                        format!("unknown account field: {field}"),
                    )),
                }
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
    use structfs_serde_store::value_to_json;

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
    fn test_account_key_roundtrip() {
        let mut gate = GateStore::new();

        // Write key
        gate.write(
            &path!("accounts/anthropic/key"),
            Record::parsed(Value::String("sk-test-123".to_string())),
        )
        .unwrap();

        // Read key back
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
            key: "sk-new".to_string(),
            model: "claude-haiku".to_string(),
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

        let record = gate.read(&path!("accounts/custom/model")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "claude-haiku"),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn test_bootstrap_roundtrip() {
        let mut gate = GateStore::new();

        // Default is "anthropic"
        let record = gate.read(&path!("bootstrap")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "anthropic"),
            _ => panic!("expected string"),
        }

        // Set to "openai"
        gate.write(
            &path!("bootstrap"),
            Record::parsed(Value::String("openai".to_string())),
        )
        .unwrap();

        let record = gate.read(&path!("bootstrap")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "openai"),
            _ => panic!("expected string"),
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
        assert!(
            gate.read(&path!("accounts/nonexistent/key"))
                .unwrap()
                .is_none()
        );
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
        let mut gate = GateStore::new();
        gate.write(
            &path!("accounts/anthropic/key"),
            Record::parsed(Value::String("sk-test".to_string())),
        )
        .unwrap();

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
        let mut gate = GateStore::new();
        gate.write(
            &path!("accounts/openai/key"),
            Record::parsed(Value::String("sk-openai".to_string())),
        )
        .unwrap();

        let send: Arc<tools::SendFn> = Arc::new(|_| Ok(vec![]));
        let tools = gate.create_completion_tools(send);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "complete_openai");
    }
}

//! Config resolution via figment — defaults → TOML file → env vars → CLI flags.
//! Config shape: gate.accounts.{name}.{provider,endpoint} + gate.defaults.{account,model,max_tokens}

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use structfs_core_store::Value;

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
pub struct OxConfig {
    #[serde(default)]
    pub gate: GateConfig,
}

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
pub struct GateConfig {
    #[serde(default)]
    pub accounts: HashMap<String, AccountEntry>,
    #[serde(default)]
    pub defaults: DefaultsConfig,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AccountEntry {
    pub provider: String,
    #[serde(default)]
    pub endpoint: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DefaultsConfig {
    #[serde(default = "default_account")]
    pub account: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: i64,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            account: default_account(),
            model: default_model(),
            max_tokens: default_max_tokens(),
        }
    }
}

fn default_account() -> String {
    "anthropic".to_string()
}
fn default_model() -> String {
    "claude-sonnet-4-20250514".to_string()
}
fn default_max_tokens() -> i64 {
    4096
}

#[derive(Debug, Default)]
pub struct CliOverrides {
    pub account: Option<String>,
    pub model: Option<String>,
    pub max_tokens: Option<i64>,
}

impl OxConfig {
    pub fn apply_overrides(&mut self, overrides: &CliOverrides) {
        if let Some(ref a) = overrides.account {
            self.gate.defaults.account = a.clone();
        }
        if let Some(ref m) = overrides.model {
            self.gate.defaults.model = m.clone();
        }
        if let Some(t) = overrides.max_tokens {
            self.gate.defaults.max_tokens = t;
        }
    }

    pub fn to_flat_map(&self) -> BTreeMap<String, Value> {
        let mut map = BTreeMap::new();
        for (name, entry) in &self.gate.accounts {
            map.insert(
                format!("gate/accounts/{name}/provider"),
                Value::String(entry.provider.clone()),
            );
            if let Some(ref ep) = entry.endpoint {
                map.insert(
                    format!("gate/accounts/{name}/endpoint"),
                    Value::String(ep.clone()),
                );
            }
        }
        map.insert(
            "gate/defaults/account".into(),
            Value::String(self.gate.defaults.account.clone()),
        );
        map.insert(
            "gate/defaults/model".into(),
            Value::String(self.gate.defaults.model.clone()),
        );
        map.insert(
            "gate/defaults/max_tokens".into(),
            Value::Integer(self.gate.defaults.max_tokens),
        );
        map
    }
}

pub fn resolve_config(config_dir: &std::path::Path, overrides: &CliOverrides) -> OxConfig {
    use figment::Figment;
    use figment::providers::{Env, Format, Toml};

    let toml_path = config_dir.join("config.toml");
    let figment = Figment::new()
        .merge(figment::providers::Serialized::defaults(OxConfig::default()))
        .merge(Toml::file(toml_path))
        .merge(Env::prefixed("OX_").split("__"));

    let mut config: OxConfig = figment.extract().unwrap_or_default();
    config.apply_overrides(overrides);
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_produce_expected_base() {
        let config = OxConfig::default();
        let flat = config.to_flat_map();
        assert_eq!(
            flat.get("gate/defaults/model").unwrap(),
            &Value::String("claude-sonnet-4-20250514".into())
        );
        assert_eq!(
            flat.get("gate/defaults/max_tokens").unwrap(),
            &Value::Integer(4096)
        );
        assert_eq!(
            flat.get("gate/defaults/account").unwrap(),
            &Value::String("anthropic".into())
        );
        assert!(!flat.keys().any(|k| k.starts_with("gate/accounts/")));
    }

    #[test]
    fn cli_overrides_merge_into_config() {
        let overrides = CliOverrides {
            account: Some("work".into()),
            model: Some("gpt-4o".into()),
            max_tokens: None,
        };
        let mut config = OxConfig::default();
        config.apply_overrides(&overrides);
        let flat = config.to_flat_map();
        assert_eq!(
            flat.get("gate/defaults/account").unwrap(),
            &Value::String("work".into())
        );
        assert_eq!(
            flat.get("gate/defaults/model").unwrap(),
            &Value::String("gpt-4o".into())
        );
        assert_eq!(
            flat.get("gate/defaults/max_tokens").unwrap(),
            &Value::Integer(4096)
        );
    }

    #[test]
    fn resolve_from_toml_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"
[gate.accounts.personal]
provider = "anthropic"

[gate.accounts.openai]
provider = "openai"
endpoint = "https://custom.openai.example/v1/chat/completions"

[gate.defaults]
account = "personal"
model = "claude-opus-4-20250514"
max_tokens = 8192
"#,
        )
        .unwrap();
        let config = resolve_config(dir.path(), &CliOverrides::default());
        assert_eq!(config.gate.defaults.account, "personal");
        assert_eq!(config.gate.defaults.model, "claude-opus-4-20250514");
        assert_eq!(config.gate.defaults.max_tokens, 8192);
        assert_eq!(config.gate.accounts.len(), 2);
        assert_eq!(config.gate.accounts["personal"].provider, "anthropic");
        assert!(config.gate.accounts["personal"].endpoint.is_none());
        assert_eq!(
            config.gate.accounts["openai"].endpoint.as_deref(),
            Some("https://custom.openai.example/v1/chat/completions")
        );

        let flat = config.to_flat_map();
        assert!(flat.contains_key("gate/accounts/personal/provider"));
        assert!(!flat.contains_key("gate/accounts/personal/endpoint"));
        assert!(flat.contains_key("gate/accounts/openai/endpoint"));
    }

    #[test]
    fn env_vars_resolve_through_figment() {
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("OX_GATE__DEFAULTS__MODEL", "env-model");
            std::env::set_var("OX_GATE__DEFAULTS__ACCOUNT", "env-acct");
            std::env::set_var("OX_GATE__ACCOUNTS__MYACCT__PROVIDER", "anthropic");
        }
        let config = resolve_config(dir.path(), &CliOverrides::default());
        assert_eq!(config.gate.defaults.model, "env-model");
        assert_eq!(config.gate.defaults.account, "env-acct");
        assert_eq!(config.gate.accounts["myacct"].provider, "anthropic");

        unsafe {
            std::env::remove_var("OX_GATE__DEFAULTS__MODEL");
            std::env::remove_var("OX_GATE__DEFAULTS__ACCOUNT");
            std::env::remove_var("OX_GATE__ACCOUNTS__MYACCT__PROVIDER");
        }
    }

    #[test]
    fn cli_overrides_beat_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[gate.defaults]\nmodel = \"from-file\"\n",
        )
        .unwrap();
        let overrides = CliOverrides {
            model: Some("from-cli".into()),
            ..Default::default()
        };
        let config = resolve_config(dir.path(), &overrides);
        assert_eq!(config.gate.defaults.model, "from-cli");
    }
}

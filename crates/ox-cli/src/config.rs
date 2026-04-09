//! Config resolution via figment — defaults → TOML file → env vars → CLI flags.
//! Config shape: gate.accounts.{name}.{provider,endpoint} + gate.defaults.{account,model,max_tokens}

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
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

    /// Produce the flat config map with resolved keys injected.
    pub fn to_flat_map_with_keys(&self, keys: &BTreeMap<String, String>) -> BTreeMap<String, Value> {
        let mut map = self.to_flat_map();
        for (name, key) in keys {
            map.insert(
                format!("gate/accounts/{name}/key"),
                Value::String(key.clone()),
            );
        }
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

/// Resolve API keys from key files and env vars.
///
/// For each account in config, checks:
/// 1. Env var `OX_GATE__ACCOUNTS__{NAME}__KEY` (highest priority)
/// 2. Key file `{keys_dir}/{name}.key`
pub fn resolve_keys(keys_dir: &Path, config: &OxConfig) -> BTreeMap<String, String> {
    let mut keys = BTreeMap::new();
    for name in config.gate.accounts.keys() {
        let env_var = format!("OX_GATE__ACCOUNTS__{}__KEY", name.to_uppercase());
        if let Ok(k) = std::env::var(&env_var) {
            if !k.is_empty() {
                keys.insert(name.clone(), k);
                continue;
            }
        }
        if let Ok(contents) = std::fs::read_to_string(keys_dir.join(format!("{name}.key"))) {
            let trimmed = contents.trim().to_string();
            if !trimmed.is_empty() {
                keys.insert(name.clone(), trimmed);
            }
        }
    }
    keys
}

/// Write an API key to a key file, creating the keys directory if needed.
pub fn write_key_file(keys_dir: &Path, name: &str, key: &str) -> std::io::Result<()> {
    if !keys_dir.exists() {
        std::fs::create_dir_all(keys_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(keys_dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    std::fs::write(keys_dir.join(format!("{name}.key")), key)
}

/// Read an API key from a key file.
pub fn read_key_file(keys_dir: &Path, name: &str) -> Option<String> {
    let contents = std::fs::read_to_string(keys_dir.join(format!("{name}.key"))).ok()?;
    let trimmed = contents.trim().to_string();
    if trimmed.is_empty() { None } else { Some(trimmed) }
}

/// Delete a key file.
pub fn delete_key_file(keys_dir: &Path, name: &str) -> std::io::Result<()> {
    let path = keys_dir.join(format!("{name}.key"));
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// Check if any account has a usable key (from key files or env vars).
pub fn has_any_key(keys_dir: &Path, config: &OxConfig) -> bool {
    !resolve_keys(keys_dir, config).is_empty()
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

    #[test]
    fn resolve_keys_from_files() {
        let dir = tempfile::tempdir().unwrap();
        let keys_dir = dir.path().join("keys");
        std::fs::create_dir_all(&keys_dir).unwrap();
        std::fs::write(keys_dir.join("anthropic.key"), "sk-test-key\n").unwrap();

        let mut config = OxConfig::default();
        config.gate.accounts.insert(
            "anthropic".into(),
            AccountEntry { provider: "anthropic".into(), endpoint: None },
        );

        let keys = resolve_keys(&keys_dir, &config);
        assert_eq!(keys.get("anthropic").unwrap(), "sk-test-key");
    }

    #[test]
    fn resolve_keys_env_beats_file() {
        let dir = tempfile::tempdir().unwrap();
        let keys_dir = dir.path().join("keys");
        std::fs::create_dir_all(&keys_dir).unwrap();
        std::fs::write(keys_dir.join("testacct2.key"), "from-file").unwrap();

        let mut config = OxConfig::default();
        config.gate.accounts.insert(
            "testacct2".into(),
            AccountEntry { provider: "anthropic".into(), endpoint: None },
        );

        unsafe { std::env::set_var("OX_GATE__ACCOUNTS__TESTACCT2__KEY", "from-env"); }
        let keys = resolve_keys(&keys_dir, &config);
        assert_eq!(keys.get("testacct2").unwrap(), "from-env");
        unsafe { std::env::remove_var("OX_GATE__ACCOUNTS__TESTACCT2__KEY"); }
    }

    #[test]
    fn write_and_read_key_file() {
        let dir = tempfile::tempdir().unwrap();
        let keys_dir = dir.path().join("keys");
        write_key_file(&keys_dir, "test", "sk-12345").unwrap();
        assert_eq!(read_key_file(&keys_dir, "test").unwrap(), "sk-12345");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::metadata(&keys_dir).unwrap().permissions();
            assert_eq!(perms.mode() & 0o777, 0o700);
        }
    }

    #[test]
    fn has_any_key_false_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let config = OxConfig::default();
        assert!(!has_any_key(&dir.path().join("keys"), &config));
    }

    #[test]
    fn to_flat_map_with_keys_injects_keys() {
        let mut config = OxConfig::default();
        config.gate.accounts.insert(
            "anthropic".into(),
            AccountEntry { provider: "anthropic".into(), endpoint: None },
        );
        let mut keys = BTreeMap::new();
        keys.insert("anthropic".into(), "sk-injected".into());
        let flat = config.to_flat_map_with_keys(&keys);
        assert_eq!(
            flat.get("gate/accounts/anthropic/key").unwrap(),
            &Value::String("sk-injected".into())
        );
    }
}

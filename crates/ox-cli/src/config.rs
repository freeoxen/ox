//! Config resolution via figment — defaults → TOML file → env vars → CLI flags.
//! All config lives under the `[gate]` section.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use structfs_core_store::Value;

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
pub struct OxConfig {
    #[serde(default)]
    pub gate: GateConfig,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct GateConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: i64,
    pub api_key: Option<String>,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            model: default_model(),
            max_tokens: default_max_tokens(),
            api_key: None,
        }
    }
}

fn default_provider() -> String {
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
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub max_tokens: Option<i64>,
}

impl OxConfig {
    pub fn apply_overrides(&mut self, overrides: &CliOverrides) {
        if let Some(ref p) = overrides.provider {
            self.gate.provider = p.clone();
        }
        if let Some(ref m) = overrides.model {
            self.gate.model = m.clone();
        }
        if let Some(ref k) = overrides.api_key {
            self.gate.api_key = Some(k.clone());
        }
        if let Some(t) = overrides.max_tokens {
            self.gate.max_tokens = t;
        }
    }

    pub fn to_flat_map(&self) -> BTreeMap<String, Value> {
        let mut map = BTreeMap::new();
        map.insert(
            "gate/model".to_string(),
            Value::String(self.gate.model.clone()),
        );
        map.insert(
            "gate/max_tokens".to_string(),
            Value::Integer(self.gate.max_tokens),
        );
        map.insert(
            "gate/provider".to_string(),
            Value::String(self.gate.provider.clone()),
        );
        if let Some(ref key) = self.gate.api_key {
            map.insert("gate/api_key".to_string(), Value::String(key.clone()));
        }
        map
    }
}

pub fn resolve_config(config_dir: &Path, overrides: &CliOverrides) -> OxConfig {
    use figment::Figment;
    use figment::providers::{Env, Format, Toml};

    let toml_path = config_dir.join("config.toml");
    let figment = Figment::new()
        .merge(figment::providers::Serialized::defaults(OxConfig::default()))
        .merge(Toml::file(toml_path))
        .merge(Env::prefixed("OX_").split("__"));

    let mut config: OxConfig = figment.extract().unwrap_or_default();
    config.apply_overrides(overrides);

    // Legacy env var fallback: ANTHROPIC_API_KEY / OPENAI_API_KEY
    if config.gate.api_key.is_none() {
        let env_key = match config.gate.provider.as_str() {
            "openai" => std::env::var("OPENAI_API_KEY").ok(),
            _ => std::env::var("ANTHROPIC_API_KEY").ok(),
        };
        if let Some(key) = env_key {
            if !key.is_empty() {
                config.gate.api_key = Some(key);
            }
        }
    }

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
            flat.get("gate/model").unwrap(),
            &Value::String("claude-sonnet-4-20250514".into())
        );
        assert_eq!(flat.get("gate/max_tokens").unwrap(), &Value::Integer(4096));
        assert_eq!(
            flat.get("gate/provider").unwrap(),
            &Value::String("anthropic".into())
        );
        assert!(!flat.contains_key("gate/api_key"));
    }

    #[test]
    fn cli_overrides_merge_into_config() {
        let overrides = CliOverrides {
            provider: Some("openai".into()),
            model: Some("gpt-4o".into()),
            api_key: Some("sk-test".into()),
            max_tokens: None,
        };
        let mut config = OxConfig::default();
        config.apply_overrides(&overrides);
        let flat = config.to_flat_map();
        assert_eq!(
            flat.get("gate/provider").unwrap(),
            &Value::String("openai".into())
        );
        assert_eq!(
            flat.get("gate/model").unwrap(),
            &Value::String("gpt-4o".into())
        );
        assert_eq!(
            flat.get("gate/api_key").unwrap(),
            &Value::String("sk-test".into())
        );
        assert_eq!(flat.get("gate/max_tokens").unwrap(), &Value::Integer(4096));
    }

    #[test]
    fn resolve_from_toml_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[gate]\nmodel = \"from-file\"\nmax_tokens = 8192\nprovider = \"openai\"\n",
        )
        .unwrap();
        let config = resolve_config(dir.path(), &CliOverrides::default());
        let flat = config.to_flat_map();
        assert_eq!(
            flat.get("gate/model").unwrap(),
            &Value::String("from-file".into())
        );
        assert_eq!(flat.get("gate/max_tokens").unwrap(), &Value::Integer(8192));
        assert_eq!(
            flat.get("gate/provider").unwrap(),
            &Value::String("openai".into())
        );
    }

    #[test]
    fn env_var_with_double_underscore_separator() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: test runs single-threaded for this env var manipulation
        unsafe {
            std::env::set_var("OX_GATE__MODEL", "env-model");
            std::env::set_var("OX_GATE__API_KEY", "sk-from-env");
        }
        let config = resolve_config(dir.path(), &CliOverrides::default());
        unsafe {
            std::env::remove_var("OX_GATE__MODEL");
            std::env::remove_var("OX_GATE__API_KEY");
        }

        assert_eq!(config.gate.model, "env-model");
        assert_eq!(config.gate.api_key.as_deref(), Some("sk-from-env"));
    }

    #[test]
    fn cli_overrides_beat_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[gate]\nmodel = \"from-file\"\n",
        )
        .unwrap();
        let overrides = CliOverrides {
            model: Some("from-cli".into()),
            ..Default::default()
        };
        let config = resolve_config(dir.path(), &overrides);
        assert_eq!(config.gate.model, "from-cli");
    }
}

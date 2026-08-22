//! Config resolution via figment — defaults → TOML file → env vars → CLI flags.
//!
//! Config shape mirrors ox-gate's namespace:
//! - `gate.providers.{name}.{dialect, endpoint, version}` — provider definitions
//! - `gate.accounts.{name}.provider` — account points at a provider
//! - `gate.defaults.{account, model, max_tokens}` — selection
//!
//! Legacy `gate.accounts.{name}.endpoint` is migrated on load: a provider entry
//! named after the account is synthesized and the account is rewritten to point
//! at it. This preserves user data through the schema split.

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
    pub providers: HashMap<String, ProviderEntry>,
    #[serde(default)]
    pub accounts: HashMap<String, AccountEntry>,
    #[serde(default)]
    pub defaults: DefaultsConfig,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ProviderEntry {
    pub dialect: String,
    pub endpoint: String,
    #[serde(default)]
    pub version: String,
    /// Auth scheme. `None` (i.e. field absent in TOML) means
    /// "default for dialect" — see `AuthScheme::default_for_dialect`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<ox_gate::AuthScheme>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AccountEntry {
    pub provider: String,
    /// Deprecated. Present only to migrate older config files; never emitted
    /// by `to_flat_map`. Use `gate.providers.{name}` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

    /// One-shot migration of legacy `gate.accounts.{name}.endpoint` fields:
    /// for each account that carries an inline endpoint, synthesize a
    /// `gate.providers.{name}` entry (dialect inherited from the account's
    /// `provider` string when it matches a known dialect, defaulted to
    /// "anthropic" otherwise) and rewrite the account to point at it.
    ///
    /// Idempotent: a second run on already-migrated config is a no-op.
    pub fn migrate_legacy_account_endpoints(&mut self) {
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
            tracing::warn!(
                account = %acct_name,
                provider = %provider_name,
                "migrating legacy accounts.{{name}}.endpoint into gate.providers"
            );
            self.gate
                .providers
                .entry(provider_name.clone())
                .or_insert(ProviderEntry {
                    dialect,
                    endpoint,
                    version: String::new(),
                    // Migration can't tell whether the original endpoint
                    // was authenticated; leave `None` so resolved_auth()
                    // falls back to the dialect default. Users who set up
                    // an unauthenticated provider via the new dialog get
                    // `AuthScheme::None` written explicitly.
                    auth: None,
                });
            if let Some(entry) = self.gate.accounts.get_mut(&acct_name) {
                entry.provider = provider_name;
                entry.endpoint = None;
            }
        }
    }

    pub fn to_flat_map(&self) -> BTreeMap<String, Value> {
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
        // Post-O2 the broker namespace exposes (account, model_id) as a
        // single `gate/completions/primary: CompletionRole` typed record;
        // the older split `gate/defaults/{account, model}` paths were
        // retired. `max_tokens` no longer enters the namespace at all —
        // the kernel reads it from `gate/accounts/{name}/models` (the
        // per-account catalog). The TOML schema keeps `[gate.defaults]`
        // as the user-facing surface; the figment loader still resolves
        // those into the in-memory `DefaultsConfig`.
        // Emit the role in the flattened child shape ("primary/account",
        // "primary/model_id") — the same shape the TOML file backing loads.
        // A typed leaf at "gate/completions/primary" collides with the
        // backing's children for the same path and the store rejects reads
        // of a path that is both leaf and parent. Children-walk reads
        // reassemble the map, which deserializes to CompletionRole as before.
        map.insert(
            "gate/completions/primary/account".into(),
            Value::String(self.gate.defaults.account.clone()),
        );
        map.insert(
            "gate/completions/primary/model_id".into(),
            Value::String(self.gate.defaults.model.clone()),
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

    // `expect` rather than `unwrap_or_default`: a silent fallback to
    // the empty default hides real deser failures (an unknown env-var
    // shape, a TOML schema drift) behind "no entry found for key"
    // panics in callers that index the resulting empty maps. Loud
    // failure is the right default; specific recovery belongs at the
    // call site, not buried here.
    let mut config: OxConfig = figment.extract().expect("config extraction failed");
    config.migrate_legacy_account_endpoints();
    config.apply_overrides(overrides);
    tracing::debug!(
        providers = config.gate.providers.len(),
        accounts = config.gate.accounts.len(),
        account_names = ?config.gate.accounts.keys().collect::<Vec<_>>(),
        default_account = %config.gate.defaults.account,
        model = %config.gate.defaults.model,
        max_tokens = config.gate.defaults.max_tokens,
        "config resolved from figment"
    );
    config
}

/// Read legacy on-disk API keys from `{keys_dir}/*.key` and the matching
/// env var `OX_GATE__ACCOUNTS__{NAME}__KEY`, returning a map of
/// `account name → raw key`.
///
/// The env var takes precedence over the file (matches the pre-A0
/// behaviour of `resolve_keys`). Empty values are filtered. The function
/// reads everything in the keys directory, not just accounts in `config`,
/// so an orphan `*.key` file still migrates — better to land it once than
/// silently lose it.
///
/// Used by the one-shot startup migration (`migrate_legacy_keys`) and
/// nowhere else; kept here so the filesystem-touching code that hands
/// out raw key bytes lives at exactly one path.
pub fn read_legacy_key_sources(keys_dir: &Path) -> BTreeMap<String, String> {
    let mut keys = BTreeMap::new();

    // Env vars first — collect all `OX_GATE__ACCOUNTS__{NAME}__KEY` set
    // in the environment. Pre-A0 only checked vars for accounts in
    // config; we relax that since the migration's job is to suck up
    // every key the user has supplied via either channel before the
    // namespace becomes the only source of truth.
    for (var, val) in std::env::vars() {
        let Some(rest) = var.strip_prefix("OX_GATE__ACCOUNTS__") else {
            continue;
        };
        let Some(name_upper) = rest.strip_suffix("__KEY") else {
            continue;
        };
        let trimmed = val.trim().to_string();
        if !trimmed.is_empty() {
            keys.insert(name_upper.to_lowercase(), trimmed);
        }
    }

    // Then the on-disk key files. Existing entries (env wins) are not
    // overwritten.
    if let Ok(read_dir) = std::fs::read_dir(keys_dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            let Some(name) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            if path.extension().and_then(|s| s.to_str()) != Some("key") {
                continue;
            }
            if keys.contains_key(&name) {
                continue;
            }
            if let Ok(contents) = std::fs::read_to_string(&path) {
                let trimmed = contents.trim().to_string();
                if !trimmed.is_empty() {
                    keys.insert(name, trimmed);
                }
            }
        }
    }

    keys
}

/// Returns `true` when the user has completed initial setup.
///
/// "Setup is complete" means: at least one account exists in the config.
/// The wizard's job is first-run guidance, not recovery — an account
/// that exists but has a broken auth shape is a Settings problem, and
/// runtime errors (key rejected, server unreachable, …) now surface with
/// precise URL/account/provider context. Sending the user back through
/// the wizard every launch because a key file is missing is wrong:
///   - LM Studio / Ollama accounts never need a key file.
///   - A pre-existing account written before the AuthScheme refactor
///     may not yet have its auth field populated; deriving auth from
///     dialect would re-trigger setup for unauthenticated local
///     providers, which is the bug this function fixes.
pub fn has_any_usable_account(config: &OxConfig) -> bool {
    !config.gate.accounts.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Process-wide lock for tests that read OR mutate `OX_*` env vars.
    /// Figment reads the env on every `resolve_config` / consumers of
    /// `read_legacy_key_sources` read env directly — concurrent
    /// `set_var` from one test corrupts the view another test sees,
    /// which then deserializes a malformed `AccountEntry` (provider
    /// missing because the other test only set a `key` field) and the
    /// surrounding extract returns `Err`. Hold this for the duration
    /// of every env-touching test body.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        // Poisoning means a prior test panicked while holding the lock;
        // the env state may be inconsistent but blocking forever helps
        // nobody — recover the guard so the rest of the suite can run
        // and surface the original failure.
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    fn role_from_flat(flat: &BTreeMap<String, Value>) -> ox_types::CompletionRole {
        let leaf = |key: &str| -> String {
            match flat.get(key) {
                Some(Value::String(s)) => s.clone(),
                other => panic!("expected string at {key}, got {other:?}"),
            }
        };
        ox_types::CompletionRole {
            account: leaf("gate/completions/primary/account"),
            model_id: leaf("gate/completions/primary/model_id"),
        }
    }

    #[test]
    fn defaults_produce_expected_base() {
        let config = OxConfig::default();
        let flat = config.to_flat_map();
        let role = role_from_flat(&flat);
        assert_eq!(role.account, "anthropic");
        assert_eq!(role.model_id, "claude-sonnet-4-20250514");
        assert!(!flat.keys().any(|k| k.starts_with("gate/accounts/")));
        // max_tokens is no longer surfaced into the broker namespace post-O2.
        assert!(!flat.keys().any(|k| k.contains("max_tokens")));
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
        let role = role_from_flat(&flat);
        assert_eq!(role.account, "work");
        assert_eq!(role.model_id, "gpt-4o");
    }

    #[test]
    fn resolve_from_toml_file() {
        let _env = env_lock();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"
[gate.providers.lm-studio]
dialect = "openai"
endpoint = "http://127.0.0.1:1234"

[gate.accounts.personal]
provider = "anthropic"

[gate.accounts.local]
provider = "lm-studio"

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
        assert_eq!(config.gate.accounts["local"].provider, "lm-studio");
        assert_eq!(config.gate.providers["lm-studio"].dialect, "openai");
        assert_eq!(
            config.gate.providers["lm-studio"].endpoint,
            "http://127.0.0.1:1234"
        );

        let flat = config.to_flat_map();
        assert!(flat.contains_key("gate/accounts/personal/provider"));
        assert!(flat.contains_key("gate/accounts/local/provider"));
        assert!(flat.contains_key("gate/providers/lm-studio/dialect"));
        assert!(flat.contains_key("gate/providers/lm-studio/endpoint"));
        assert!(!flat.keys().any(|k| k.ends_with("/accounts/local/endpoint")));
    }

    #[test]
    fn legacy_account_endpoint_is_migrated_to_provider() {
        let _env = env_lock();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"
[gate.accounts.local]
provider = "openai"
endpoint = "http://127.0.0.1:1234/v1/chat/completions"
"#,
        )
        .unwrap();
        let config = resolve_config(dir.path(), &CliOverrides::default());

        // Account no longer carries an inline endpoint…
        assert!(config.gate.accounts["local"].endpoint.is_none());
        // …it now points at a synthesized provider named after the account.
        let provider_name = &config.gate.accounts["local"].provider;
        let prov = &config.gate.providers[provider_name];
        assert_eq!(prov.dialect, "openai");
        assert_eq!(prov.endpoint, "http://127.0.0.1:1234/v1/chat/completions");

        // Flat map carries the provider entry, not the legacy account endpoint.
        let flat = config.to_flat_map();
        assert!(flat.contains_key(&format!("gate/providers/{provider_name}/endpoint")));
        assert!(!flat.keys().any(|k| k.ends_with("accounts/local/endpoint")));
    }

    #[test]
    fn env_vars_resolve_through_figment() {
        let _env = env_lock();
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
        let _env = env_lock();
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
    fn read_legacy_key_sources_picks_up_key_files() {
        let dir = tempfile::tempdir().unwrap();
        let keys_dir = dir.path().join("keys");
        std::fs::create_dir_all(&keys_dir).unwrap();
        std::fs::write(keys_dir.join("anthropic.key"), "sk-test-key\n").unwrap();
        std::fs::write(keys_dir.join("openai.key"), "  sk-other  \n").unwrap();
        // Non-`.key` extension is ignored.
        std::fs::write(keys_dir.join("notes.txt"), "something else").unwrap();

        let keys = read_legacy_key_sources(&keys_dir);
        assert_eq!(keys.get("anthropic").unwrap(), "sk-test-key");
        assert_eq!(keys.get("openai").unwrap(), "sk-other");
        assert!(!keys.contains_key("notes"));
    }

    #[test]
    fn read_legacy_key_sources_env_beats_file() {
        let _env = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let keys_dir = dir.path().join("keys");
        std::fs::create_dir_all(&keys_dir).unwrap();
        std::fs::write(keys_dir.join("testacct2.key"), "from-file").unwrap();

        unsafe {
            std::env::set_var("OX_GATE__ACCOUNTS__TESTACCT2__KEY", "from-env");
        }
        let keys = read_legacy_key_sources(&keys_dir);
        unsafe {
            std::env::remove_var("OX_GATE__ACCOUNTS__TESTACCT2__KEY");
        }
        assert_eq!(keys.get("testacct2").unwrap(), "from-env");
    }

    #[test]
    fn read_legacy_key_sources_missing_dir_is_ok() {
        let _env = env_lock();
        // The keys directory may not exist on a fresh install — return the
        // env-only set rather than erroring.
        let dir = tempfile::tempdir().unwrap();
        let keys = read_legacy_key_sources(&dir.path().join("nonexistent"));
        assert!(keys.is_empty());
    }

    #[test]
    fn has_any_usable_account_true_when_lm_studio_account_present() {
        // An LM Studio account has no key file and never will. The wizard
        // must not re-fire on every launch in that case — even when the
        // provider entry predates the AuthScheme field.
        let mut config = OxConfig::default();
        config.gate.providers.insert(
            "lm-studio".into(),
            ProviderEntry {
                dialect: "openai".into(),
                endpoint: "http://127.0.0.1:1234".into(),
                version: String::new(),
                auth: None, // legacy: written before AuthScheme existed
            },
        );
        config.gate.accounts.insert(
            "local".into(),
            AccountEntry {
                provider: "lm-studio".into(),
                endpoint: None,
            },
        );
        assert!(has_any_usable_account(&config));
    }

    #[test]
    fn has_any_usable_account_true_when_authenticated_account_present_without_key() {
        // An Anthropic account exists but has no key resolved yet. The
        // wizard does NOT fire — setup is complete; the missing key is a
        // Settings-screen / runtime concern, surfaced at request time.
        let mut config = OxConfig::default();
        config.gate.accounts.insert(
            "personal".into(),
            AccountEntry {
                provider: "anthropic".into(),
                endpoint: None,
            },
        );
        assert!(has_any_usable_account(&config));
    }

    #[test]
    fn has_any_usable_account_false_for_empty_config() {
        let config = OxConfig::default();
        assert!(!has_any_usable_account(&config));
    }

    #[test]
    fn to_flat_map_does_not_emit_account_keys() {
        // After A0, API keys never enter the flat config map. They live at
        // `secret/keys/{name}: ApiKey`, not `gate/accounts/{name}/key`.
        let mut config = OxConfig::default();
        config.gate.accounts.insert(
            "anthropic".into(),
            AccountEntry {
                provider: "anthropic".into(),
                endpoint: None,
            },
        );
        let flat = config.to_flat_map();
        assert!(
            !flat.keys().any(|k| k.ends_with("/key")),
            "flat config must not carry account keys, got: {:?}",
            flat.keys().collect::<Vec<_>>()
        );
    }
}

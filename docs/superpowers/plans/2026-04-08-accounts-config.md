# Accounts-First Config Redesign — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Separate auth identity (accounts: provider + key) from session preferences (defaults: account + model + max_tokens) across the entire config stack.

**Architecture:** AccountConfig shrinks to `{provider, key}`. GateStore gains a `defaults` struct (`{account, model, max_tokens}`) replacing the old `bootstrap` field and the model/max_tokens fields that lived on AccountConfig. Config paths move from `gate/model` → `gate/defaults/model`, `gate/api_key` → per-account `gate/accounts/{name}/key`. CLI gains `--account`, loses `--provider` and `--api-key`.

**Tech Stack:** Rust (ox-gate, ox-cli, ox-context, ox-web, ox-ui), figment (config resolution), TOML (config file), StructFS (Reader/Writer)

**Spec:** `docs/superpowers/specs/2026-04-08-accounts-config-design.md`

---

### Task 1: Shrink AccountConfig

**Files:**
- Modify: `crates/ox-gate/src/account.rs` (all 22 lines)

- [ ] **Step 1: Update AccountConfig struct**

Remove `model` and `max_tokens` fields. AccountConfig is now pure auth.

```rust
//! Account configuration for LLM API access.

use serde::{Deserialize, Serialize};

/// An account binds an API key to a named provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    /// Name of the provider this account uses (e.g. `"anthropic"`).
    pub provider: String,
    /// API key for authentication.
    #[serde(default)]
    pub key: String,
}
```

- [ ] **Step 2: Fix all compilation errors from removed fields**

Every site that constructs `AccountConfig` with `model` / `max_tokens` must be updated. These are:

1. `crates/ox-gate/src/lib.rs` — `GateStore::new()` (lines 52–69): Remove `model` and `max_tokens` from account construction.
2. `crates/ox-gate/src/lib.rs` — `restore_from_snapshot()` (lines 250–267): Remove `model` and `max_tokens` parsing from snapshot restore.
3. `crates/ox-gate/src/tools.rs` — test helpers (lines 148–153, 166–171): Remove `model` and `max_tokens` from test `AccountConfig` construction.
4. `crates/ox-web/src/lib.rs` — `set_api_key()` (lines 143–152): Remove `model` and `max_tokens` from account creation.

In `GateStore::new()`, accounts become:
```rust
accounts.insert(
    "anthropic".to_string(),
    AccountConfig {
        provider: "anthropic".to_string(),
        key: String::new(),
    },
);
accounts.insert(
    "openai".to_string(),
    AccountConfig {
        provider: "openai".to_string(),
        key: String::new(),
    },
);
```

In `restore_from_snapshot()`, account restore becomes:
```rust
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
            key: String::new(),
        },
    );
}
```

In `tools.rs` tests, account construction becomes:
```rust
let account = AccountConfig {
    provider: "test".to_string(),
    key: "sk-test".to_string(),
};
```

In `ox-web/src/lib.rs` `set_api_key()`, account creation becomes:
```rust
let config = AccountConfig {
    provider: provider.to_string(),
    key: key.to_string(),
};
```

- [ ] **Step 3: Run `cargo check -p ox-gate -p ox-web`**

Expected: compiles (some warnings about unused fields in GateStore reader/writer are fine — we'll fix those in Task 2).

- [ ] **Step 4: Commit**

```bash
git add crates/ox-gate/src/account.rs crates/ox-gate/src/lib.rs crates/ox-gate/src/tools.rs crates/ox-web/src/lib.rs
git commit -m "refactor(ox-gate): shrink AccountConfig to {provider, key}

model and max_tokens move to GateStore defaults (next commit)."
```

---

### Task 2: Add Defaults struct and update GateStore

**Files:**
- Modify: `crates/ox-gate/src/lib.rs` (struct + Reader + Writer + snapshot + tests)

This is the largest task. GateStore gains a `Defaults` struct that replaces `bootstrap` and holds the model/max_tokens that were on AccountConfig.

- [ ] **Step 1: Add Defaults struct and update GateStore struct**

Add after the imports, before GateStore:

```rust
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
```

Update GateStore struct — replace `bootstrap: String` with `defaults: Defaults`:

```rust
pub struct GateStore {
    providers: HashMap<String, ProviderConfig>,
    accounts: HashMap<String, AccountConfig>,
    defaults: Defaults,
    catalogs: HashMap<String, Vec<ModelInfo>>,
    config: Option<Box<dyn Store + Send + Sync>>,
}
```

Update `GateStore::new()`:

```rust
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
        },
    );
    accounts.insert(
        "openai".to_string(),
        AccountConfig {
            provider: "openai".to_string(),
            key: String::new(),
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
```

- [ ] **Step 2: Update config_string path for per-account keys**

Update `completion_tool_schemas()` — check config for per-account keys (any account, not just bootstrap):

```rust
pub fn completion_tool_schemas(&mut self) -> Vec<ToolSchema> {
    let names: Vec<String> = self.accounts.keys().cloned().collect();
    names
        .iter()
        .filter_map(|name| {
            let has_key = {
                let local_key = self
                    .accounts
                    .get(name)
                    .map(|a| a.key.clone())
                    .unwrap_or_default();
                if !local_key.is_empty() {
                    true
                } else {
                    self.config_string(&format!("gate/accounts/{name}/key"))
                        .is_some()
                }
            };
            if !has_key {
                return None;
            }
            let account = self.accounts.get(name)?;
            let provider = self.providers.get(&account.provider)?;
            Some(tools::completion_tool_schema(name, provider))
        })
        .collect()
}
```

Same pattern for `create_completion_tools()`:

```rust
pub fn create_completion_tools(&mut self, send: Arc<tools::SendFn>) -> Vec<Box<dyn Tool>> {
    let names: Vec<String> = self.accounts.keys().cloned().collect();
    names
        .iter()
        .filter_map(|name| {
            let has_key = {
                let local_key = self
                    .accounts
                    .get(name)
                    .map(|a| a.key.clone())
                    .unwrap_or_default();
                if !local_key.is_empty() {
                    true
                } else {
                    self.config_string(&format!("gate/accounts/{name}/key"))
                        .is_some()
                }
            };
            if !has_key {
                return None;
            }
            let account = self.accounts.get(name)?;
            let provider = self.providers.get(&account.provider)?;
            Some(Box::new(tools::completion_tool(
                name.clone(),
                account,
                provider,
                send.clone(),
            )) as Box<dyn Tool>)
        })
        .collect()
}
```

- [ ] **Step 3: Update Reader impl — replace bootstrap/model/max_tokens with defaults/**

Replace the Reader impl. Key changes:

1. `"bootstrap"` path → removed (replaced by `"defaults"`)
2. `"model"` path → removed
3. `"max_tokens"` path → removed
4. Add `"defaults"` path handling:

```rust
"defaults" => {
    if from.components.len() < 2 {
        return Ok(None);
    }
    let field = from.components[1].as_str();
    match field {
        "account" => {
            if let Some(s) = self.config_string("gate/defaults/account") {
                return Ok(Some(Record::parsed(Value::String(s))));
            }
            Ok(Some(Record::parsed(Value::String(
                self.defaults.account.clone(),
            ))))
        }
        "model" => {
            if let Some(s) = self.config_string("gate/defaults/model") {
                return Ok(Some(Record::parsed(Value::String(s))));
            }
            Ok(Some(Record::parsed(Value::String(
                self.defaults.model.clone(),
            ))))
        }
        "max_tokens" => {
            if let Some(n) = self.config_integer("gate/defaults/max_tokens") {
                return Ok(Some(Record::parsed(Value::Integer(n))));
            }
            Ok(Some(Record::parsed(Value::Integer(
                self.defaults.max_tokens as i64,
            ))))
        }
        _ => Ok(None),
    }
}
```

5. In the `"accounts"` arm, update key reading for any account (not just bootstrap) to check config:

Replace the special bootstrap key check (lines 356–370) with a generic per-account check:

```rust
"accounts" => {
    if from.components.len() < 2 {
        return Ok(None);
    }
    let name = from.components[1].as_str();

    // Check config for per-account key before account lookup
    if from.components.len() > 2 {
        let field = from.components[2].as_str();
        if field == "key" {
            let local_empty = self
                .accounts
                .get(name)
                .map(|a| a.key.is_empty())
                .unwrap_or(true);
            if local_empty {
                if let Some(k) =
                    self.config_string(&format!("gate/accounts/{name}/key"))
                {
                    return Ok(Some(Record::parsed(Value::String(k))));
                }
            }
        }
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
        "key" => Ok(Some(Record::parsed(Value::String(config.key.clone())))),
        "provider" => Ok(Some(Record::parsed(Value::String(
            config.provider.clone(),
        )))),
        _ => Ok(None),
    }
}
```

Note: removed `"model"` and `"max_tokens"` from per-account field reads — those are no longer on AccountConfig.

- [ ] **Step 4: Update Writer impl — replace bootstrap/model/max_tokens with defaults/**

1. Remove top-level `"bootstrap"`, `"model"`, `"max_tokens"` write arms.
2. Add `"defaults"` write arm:

```rust
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
```

3. In the `"accounts"` write arm, remove `"model"` and `"max_tokens"` field writes:

```rust
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
    _ => Err(StoreError::store(
        "gate",
        "write",
        format!("unknown account field: {field}"),
    )),
}
```

- [ ] **Step 5: Update snapshot_state and restore_from_snapshot**

`snapshot_state()` — snapshot saves defaults (account name only, no key) and accounts (provider only, no key, no model, no max_tokens):

```rust
fn snapshot_state(&self) -> Value {
    let mut state = BTreeMap::new();

    // Save defaults (account name, model, max_tokens)
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
```

`restore_from_snapshot()` — restore defaults from snapshot:

```rust
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

    // Restore defaults
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
        let providers: HashMap<String, ProviderConfig> =
            serde_json::from_value(providers_json)
                .map_err(|e| StoreError::store("gate", "write", e.to_string()))?;
        self.providers = providers;
    }

    if let Some(accounts_val) = state_map.get("accounts") {
        let mut new_accounts = HashMap::new();
        match accounts_val {
            Value::Map(accts) => {
                for (name, acct_val) in accts {
                    let acct_json =
                        structfs_serde_store::value_to_json(acct_val.clone());
                    let provider = acct_json
                        .get("provider")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    new_accounts.insert(
                        name.clone(),
                        AccountConfig {
                            provider,
                            key: String::new(),
                        },
                    );
                }
            }
            _ => {
                return Err(StoreError::store(
                    "gate",
                    "write",
                    "accounts must be a map",
                ))
            }
        }
        self.accounts = new_accounts;
    }

    Ok(())
}
```

- [ ] **Step 6: Update GateStore doc comment**

Update the module doc on GateStore to reflect new paths:

```rust
/// Gate store — manages providers, accounts, model catalogs, and session defaults.
///
/// Mount this at `"gate"` in the namespace. Read/write paths:
///
/// - `providers/{name}` — ProviderConfig (dialect, endpoint, version)
/// - `providers/{name}/models` — model catalog for provider
/// - `accounts/{name}` — AccountConfig (provider, key)
/// - `accounts/{name}/key` — API key (falls back to config handle)
/// - `accounts/{name}/provider` — provider name
/// - `defaults/account` — name of the default account
/// - `defaults/model` — default model ID (falls back to config handle)
/// - `defaults/max_tokens` — default token limit (falls back to config handle)
```

- [ ] **Step 7: Rewrite all tests**

Replace all existing tests with tests for the new API. The tests must cover:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;
    use structfs_serde_store::{json_to_value, value_to_json};

    #[test]
    fn test_default_providers() {
        let mut gate = GateStore::new();
        let record = gate.read(&path!("providers/anthropic")).unwrap().unwrap();
        let json = match record {
            Record::Parsed(v) => value_to_json(v),
            _ => panic!("expected parsed"),
        };
        assert_eq!(json["dialect"], "anthropic");
        assert_eq!(json["endpoint"], "https://api.anthropic.com/v1/messages");

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
        gate.write(
            &path!("accounts/anthropic/key"),
            Record::parsed(Value::String("sk-test-123".to_string())),
        )
        .unwrap();
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
        };
        let value = to_value(&config).unwrap();
        gate.write(&path!("accounts/custom"), Record::parsed(value))
            .unwrap();
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

        // Write new defaults
        gate.write(
            &path!("defaults/account"),
            Record::parsed(Value::String("openai".to_string())),
        )
        .unwrap();
        gate.write(
            &path!("defaults/model"),
            Record::parsed(Value::String("gpt-4o".to_string())),
        )
        .unwrap();
        gate.write(
            &path!("defaults/max_tokens"),
            Record::parsed(Value::Integer(8192)),
        )
        .unwrap();

        let record = gate.read(&path!("defaults/account")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "openai"),
            _ => panic!("expected string"),
        }
        let record = gate.read(&path!("defaults/model")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "gpt-4o"),
            _ => panic!("expected string"),
        }
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
                        // Verify defaults structure
                        let defaults = match sm.get("defaults").unwrap() {
                            Value::Map(d) => d,
                            _ => panic!("expected map"),
                        };
                        assert!(defaults.contains_key("account"));
                        assert!(defaults.contains_key("model"));
                        assert!(defaults.contains_key("max_tokens"));
                        // Keys excluded from accounts
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
    fn snapshot_excludes_api_keys() {
        let mut gate = GateStore::new();
        gate.write(
            &path!("accounts/anthropic/key"),
            Record::parsed(Value::String("sk-secret".to_string())),
        )
        .unwrap();
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
        assert!(gate.read(&path!("providers/anthropic")).unwrap().is_none());
        assert!(gate.read(&path!("providers/openai")).unwrap().is_some());
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
        let record = gate
            .read(&path!("accounts/openai/key"))
            .unwrap()
            .unwrap();
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
        let record = gate.read(&path!("defaults/model")).unwrap().unwrap();
        match record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "claude-sonnet-4-20250514"),
            _ => panic!("expected string"),
        }
    }
}
```

- [ ] **Step 8: Run tests**

Run: `cargo test -p ox-gate`
Expected: all tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/ox-gate/src/lib.rs
git commit -m "refactor(ox-gate): replace bootstrap with defaults struct

GateStore now has defaults/{account,model,max_tokens} paths.
Per-account key resolution from config handle (any account, not just bootstrap).
Backwards-compat for legacy 'bootstrap' field in snapshots."
```

---

### Task 3: Update figment types and config resolution

**Files:**
- Modify: `crates/ox-cli/src/config.rs` (all 236 lines)

- [ ] **Step 1: Rewrite figment types**

Replace the entire config.rs with:

```rust
//! Config resolution via figment — defaults → TOML file → env vars → CLI flags.
//! Config shape: gate.accounts.{name}.{provider,key} + gate.defaults.{account,model,max_tokens}

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
    pub key: String,
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
            if !entry.key.is_empty() {
                map.insert(
                    format!("gate/accounts/{name}/key"),
                    Value::String(entry.key.clone()),
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
        // No account entries by default (accounts come from TOML/env)
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
key = "sk-ant-personal"

[gate.accounts.openai]
provider = "openai"
key = "sk-oai-test"

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
        assert_eq!(config.gate.accounts["personal"].key, "sk-ant-personal");
        assert_eq!(config.gate.accounts["openai"].provider, "openai");

        let flat = config.to_flat_map();
        assert!(flat.contains_key("gate/accounts/personal/provider"));
        assert!(flat.contains_key("gate/accounts/personal/key"));
        assert!(flat.contains_key("gate/accounts/openai/key"));
    }

    #[test]
    fn env_vars_resolve_through_figment() {
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("OX_GATE__DEFAULTS__MODEL", "env-model");
            std::env::set_var("OX_GATE__DEFAULTS__ACCOUNT", "env-acct");
            std::env::set_var("OX_GATE__ACCOUNTS__MYACCT__PROVIDER", "anthropic");
            std::env::set_var("OX_GATE__ACCOUNTS__MYACCT__KEY", "sk-from-env");
        }
        let config = resolve_config(dir.path(), &CliOverrides::default());
        assert_eq!(config.gate.defaults.model, "env-model");
        assert_eq!(config.gate.defaults.account, "env-acct");
        assert_eq!(config.gate.accounts["myacct"].provider, "anthropic");
        assert_eq!(config.gate.accounts["myacct"].key, "sk-from-env");

        unsafe {
            std::env::remove_var("OX_GATE__DEFAULTS__MODEL");
            std::env::remove_var("OX_GATE__DEFAULTS__ACCOUNT");
            std::env::remove_var("OX_GATE__ACCOUNTS__MYACCT__PROVIDER");
            std::env::remove_var("OX_GATE__ACCOUNTS__MYACCT__KEY");
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
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p ox-cli -- config`
Expected: all config tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/ox-cli/src/config.rs
git commit -m "refactor(ox-cli): accounts-first figment types

GateConfig now has accounts HashMap + DefaultsConfig.
Removed legacy ANTHROPIC_API_KEY/OPENAI_API_KEY env var mapping.
Config paths: gate/defaults/{account,model,max_tokens}, gate/accounts/{name}/{provider,key}."
```

---

### Task 4: Update CLI flags and main.rs

**Files:**
- Modify: `crates/ox-cli/src/main.rs:31-94`

- [ ] **Step 1: Update Cli struct and overrides wiring**

Replace the Cli struct and override logic:

```rust
#[derive(Parser)]
#[command(name = "ox", about = "Agentic coding CLI")]
struct Cli {
    /// Named account from config (overrides gate.defaults.account)
    #[arg(long)]
    account: Option<String>,

    /// Model identifier
    #[arg(long, short)]
    model: Option<String>,

    /// Workspace root directory
    #[arg(long, default_value = ".")]
    workspace: String,

    /// Max tokens per completion
    #[arg(long)]
    max_tokens: Option<u32>,

    /// Disable policy enforcement (allow all tool calls)
    #[arg(long)]
    no_policy: bool,
}
```

Update the overrides construction and API key check. Replace lines 71–94:

```rust
    let overrides = config::CliOverrides {
        account: cli.account.clone(),
        model: cli.model.clone(),
        max_tokens: cli.max_tokens.map(|t| t as i64),
    };
    let resolved = config::resolve_config(&inbox_root, &overrides);

    // Verify the default account has a key (from config file or env vars)
    let default_account = &resolved.gate.defaults.account;
    let has_key = resolved
        .gate
        .accounts
        .get(default_account)
        .map(|a| !a.key.is_empty())
        .unwrap_or(false);
    if !has_key {
        eprintln!("error: no API key for account '{default_account}'");
        eprintln!("  configure in ~/.ox/config.toml under [gate.accounts.{default_account}]");
        eprintln!("  or set OX_GATE__ACCOUNTS__{}_KEY", default_account.to_uppercase());
        std::process::exit(1);
    }
```

- [ ] **Step 2: Run `cargo check -p ox-cli`**

Expected: compiles. (Other crates may have errors — we'll fix those in subsequent tasks.)

- [ ] **Step 3: Commit**

```bash
git add crates/ox-cli/src/main.rs
git commit -m "refactor(ox-cli): --account replaces --provider/--api-key

CLI no longer accepts --provider or --api-key flags.
Account keys come from config file or env vars only."
```

---

### Task 5: Update synthesize_prompt

**Files:**
- Modify: `crates/ox-context/src/lib.rs:121-161`

- [ ] **Step 1: Update path reads**

Change `gate/model` to `gate/defaults/model` and `gate/max_tokens` to `gate/defaults/max_tokens`:

```rust
    // Read model ID
    let model_id = {
        let record = reader.read(&path!("gate/defaults/model"))?.ok_or_else(|| {
            StoreError::store(
                "synthesize_prompt",
                "read",
                "gate store returned None for defaults/model",
            )
        })?;
        match record {
            Record::Parsed(Value::String(s)) => s,
            _ => {
                return Err(StoreError::store(
                    "synthesize_prompt",
                    "read",
                    "expected string from gate store for defaults/model",
                ));
            }
        }
    };

    // Read max_tokens
    let max_tokens = {
        let record = reader
            .read(&path!("gate/defaults/max_tokens"))?
            .ok_or_else(|| {
                StoreError::store(
                    "synthesize_prompt",
                    "read",
                    "gate store returned None for defaults/max_tokens",
                )
            })?;
        match record {
            Record::Parsed(Value::Integer(n)) => n as u32,
            _ => {
                return Err(StoreError::store(
                    "synthesize_prompt",
                    "read",
                    "expected integer from gate store for defaults/max_tokens",
                ));
            }
        }
    };
```

- [ ] **Step 2: Run `cargo check -p ox-context`**

Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add crates/ox-context/src/lib.rs
git commit -m "refactor(ox-context): synthesize_prompt reads gate/defaults/ paths"
```

---

### Task 6: Update agent worker

**Files:**
- Modify: `crates/ox-cli/src/agents.rs:200-256`

- [ ] **Step 1: Update bootstrap/provider/key reads**

Replace the agent worker's config reads to use `defaults/account` instead of `bootstrap`:

```rust
    // Read default account and provider from thread's GateStore (resolves through config handle)
    let default_account = match adapter.read(&path!("gate/defaults/account")) {
        Ok(Some(r)) => match r.as_value() {
            Some(Value::String(s)) => s.clone(),
            _ => "anthropic".to_string(),
        },
        _ => "anthropic".to_string(),
    };
    let provider = match adapter.read(&structfs_core_store::Path::from_components(vec![
        "gate".into(),
        "accounts".into(),
        default_account.clone(),
        "provider".into(),
    ])) {
        Ok(Some(r)) => match r.as_value() {
            Some(Value::String(s)) => s.clone(),
            _ => "anthropic".to_string(),
        },
        _ => "anthropic".to_string(),
    };
    let api_key_for_transport =
        match adapter.read(&structfs_core_store::Path::from_components(vec![
            "gate".into(),
            "accounts".into(),
            default_account.clone(),
            "key".into(),
        ])) {
            Ok(Some(r)) => match r.as_value() {
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            },
            _ => String::new(),
        };
    let provider_config = match provider.as_str() {
        "openai" => ProviderConfig::openai(),
        _ => ProviderConfig::anthropic(),
    };

    // Register completion tools using a temporary GateStore with the resolved key
    let mut gate_for_tools = GateStore::new();
    gate_for_tools
        .write(
            &ox_kernel::Path::from_components(vec![
                "accounts".to_string(),
                default_account.clone(),
                "key".to_string(),
            ]),
            Record::parsed(Value::String(api_key_for_transport.clone())),
        )
        .ok();
    let send = Arc::new(crate::transport::make_send_fn(
        provider_config.clone(),
        api_key_for_transport.clone(),
    ));
    for tool in gate_for_tools.create_completion_tools(send) {
        tools.register(tool);
    }
```

- [ ] **Step 2: Run `cargo check -p ox-cli`**

Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add crates/ox-cli/src/agents.rs
git commit -m "refactor(ox-cli): agent worker reads defaults/account instead of bootstrap"
```

---

### Task 7: Update ox-web

**Files:**
- Modify: `crates/ox-web/src/lib.rs`

- [ ] **Step 1: Update OxAgent::new()**

Change the model/max_tokens writes from `gate/model` / `gate/max_tokens` to `gate/defaults/model` / `gate/defaults/max_tokens`:

```rust
        context
            .write(
                &path!("gate/defaults/model"),
                Record::parsed(Value::String(model)),
            )
            .ok();
        context
            .write(
                &path!("gate/defaults/max_tokens"),
                Record::parsed(Value::Integer(max_tokens as i64)),
            )
            .ok();
```

- [ ] **Step 2: Update set_provider()**

Change `gate/bootstrap` writes to `gate/defaults/account`:

Find all occurrences of `path!("gate/bootstrap")` in ox-web/src/lib.rs and replace with `path!("gate/defaults/account")`.

- [ ] **Step 3: Update get_provider() and other bootstrap reads**

Replace all `gate/bootstrap` reads with `gate/defaults/account` reads. These are in:
- `get_provider()` 
- `list_models()`
- the agentic loop bootstrap read (around line 787)

- [ ] **Step 4: Update set_model()**

Change `gate/model` write to `gate/defaults/model`:

Replace `path!("gate/model")` write in `set_model()` with `path!("gate/defaults/model")`.

- [ ] **Step 5: Update model reads in the agentic loop**

Replace `gate/model` reads with `gate/defaults/model` reads. Replace `gate/max_tokens` reads with `gate/defaults/max_tokens` reads.

- [ ] **Step 6: Run `cargo check --target wasm32-unknown-unknown -p ox-web`**

Expected: compiles for wasm target.

- [ ] **Step 7: Commit**

```bash
git add crates/ox-web/src/lib.rs
git commit -m "refactor(ox-web): use gate/defaults/ paths for model and max_tokens"
```

---

### Task 8: Update ConfigStore save filter

**Files:**
- Modify: `crates/ox-ui/src/config_store.rs:61-64`

- [ ] **Step 1: Update the save_runtime filter**

The old filter excluded paths containing `"api_key"`. The new paths use `gate/accounts/{name}/key`, so update:

```rust
    pub fn save_runtime(&self) -> Result<(), StoreError> {
        let Some(ref backing) = self.backing else {
            return Ok(());
        };
        let filtered: BTreeMap<String, Value> = self
            .runtime
            .iter()
            .filter(|(k, _)| !k.contains("/key"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        backing.save(&Value::Map(filtered))
    }
```

- [ ] **Step 2: Update the test for key exclusion**

Find the existing test that checks `api_key` filtering and update it to use the new path format. If the test uses `"gate/api_key"`, change it to `"gate/accounts/anthropic/key"`.

- [ ] **Step 3: Run `cargo test -p ox-ui -- config`**

Expected: all ConfigStore tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-ui/src/config_store.rs
git commit -m "refactor(ox-ui): update ConfigStore save filter for per-account key paths"
```

---

### Task 9: Update completion tools to accept model/max_tokens params

**Files:**
- Modify: `crates/ox-gate/src/tools.rs`

- [ ] **Step 1: Update completion_params_schema()**

Add `model` and `max_tokens` as optional parameters:

```rust
fn completion_params_schema() -> serde_json::Value {
    serde_json::json!({
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
    })
}
```

- [ ] **Step 2: Update completion_tool() to accept default model and max_tokens**

The tool no longer reads model from AccountConfig (it doesn't have one). Instead it receives defaults:

```rust
pub fn completion_tool(
    account_name: String,
    provider: &ProviderConfig,
    default_model: String,
    default_max_tokens: u32,
    send: Arc<SendFn>,
) -> FnTool {
    let description = format!(
        "Send a completion to the {} account ({} dialect)",
        account_name, provider.dialect,
    );
    FnTool::new(
        tool_name_for(&account_name),
        description,
        completion_params_schema(),
        move |input| {
            let prompt = input
                .get("prompt")
                .and_then(|v| v.as_str())
                .ok_or("missing required 'prompt' field")?;
            let system = input.get("system").and_then(|v| v.as_str()).unwrap_or("");
            let model = input
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or(&default_model);
            let max_tokens = input
                .get("max_tokens")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32)
                .unwrap_or(default_max_tokens);
            complete_via_gate(&*send, model, max_tokens, prompt, system)
        },
    )
}
```

- [ ] **Step 3: Update complete_via_gate() to accept max_tokens**

```rust
pub fn complete_via_gate(
    send: &dyn Fn(&CompletionRequest) -> Result<Vec<StreamEvent>, String>,
    model: &str,
    max_tokens: u32,
    prompt: &str,
    system: &str,
) -> Result<String, String> {
    let request = CompletionRequest {
        model: model.to_string(),
        max_tokens,
        system: system.to_string(),
        messages: vec![serde_json::json!({"role": "user", "content": prompt})],
        tools: vec![],
        stream: true,
    };

    let events = send(&request)?;

    let text: String = events
        .iter()
        .filter_map(|e| {
            if let StreamEvent::TextDelta(t) = e {
                Some(t.as_str())
            } else {
                None
            }
        })
        .collect();

    Ok(text)
}
```

- [ ] **Step 4: Update GateStore::create_completion_tools()**

Pass default model and max_tokens from GateStore defaults:

```rust
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
            let has_key = {
                let local_key = self
                    .accounts
                    .get(name)
                    .map(|a| a.key.clone())
                    .unwrap_or_default();
                if !local_key.is_empty() {
                    true
                } else {
                    self.config_string(&format!("gate/accounts/{name}/key"))
                        .is_some()
                }
            };
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
```

- [ ] **Step 5: Update tests in tools.rs**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ox_kernel::Tool;

    fn mock_send(request: &CompletionRequest) -> Result<Vec<StreamEvent>, String> {
        let content = request.messages[0]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();
        Ok(vec![
            StreamEvent::TextDelta(format!("Response to: {content}")),
            StreamEvent::MessageStop,
        ])
    }

    #[test]
    fn complete_via_gate_basic() {
        let result = complete_via_gate(&mock_send, "test-model", 4096, "Hello", "").unwrap();
        assert_eq!(result, "Response to: Hello");
    }

    #[test]
    fn complete_via_gate_with_system() {
        let send = |req: &CompletionRequest| -> Result<Vec<StreamEvent>, String> {
            assert_eq!(req.system, "Be brief");
            Ok(vec![
                StreamEvent::TextDelta("OK".to_string()),
                StreamEvent::MessageStop,
            ])
        };
        let result = complete_via_gate(&send, "test-model", 4096, "Hi", "Be brief").unwrap();
        assert_eq!(result, "OK");
    }

    #[test]
    fn complete_via_gate_uses_max_tokens() {
        let send = |req: &CompletionRequest| -> Result<Vec<StreamEvent>, String> {
            assert_eq!(req.max_tokens, 8192);
            Ok(vec![
                StreamEvent::TextDelta("OK".to_string()),
                StreamEvent::MessageStop,
            ])
        };
        let result = complete_via_gate(&send, "test-model", 8192, "Hi", "").unwrap();
        assert_eq!(result, "OK");
    }

    #[test]
    fn completion_tool_execute() {
        let provider = ProviderConfig::anthropic();
        let send: Arc<SendFn> = Arc::new(mock_send);
        let tool = completion_tool(
            "test".to_string(),
            &provider,
            "test-model".to_string(),
            4096,
            send,
        );
        let result = tool
            .execute(serde_json::json!({"prompt": "Hello"}))
            .unwrap();
        assert_eq!(result, "Response to: Hello");
    }

    #[test]
    fn completion_tool_model_override() {
        let send: Arc<SendFn> = Arc::new(|req| {
            assert_eq!(req.model, "custom-model");
            Ok(vec![
                StreamEvent::TextDelta("OK".to_string()),
                StreamEvent::MessageStop,
            ])
        });
        let provider = ProviderConfig::anthropic();
        let tool = completion_tool(
            "test".to_string(),
            &provider,
            "default-model".to_string(),
            4096,
            send,
        );
        let result = tool
            .execute(serde_json::json!({"prompt": "Hello", "model": "custom-model"}))
            .unwrap();
        assert_eq!(result, "OK");
    }

    #[test]
    fn completion_tool_missing_prompt() {
        let provider = ProviderConfig::anthropic();
        let send: Arc<SendFn> = Arc::new(mock_send);
        let tool = completion_tool(
            "test".to_string(),
            &provider,
            "test-model".to_string(),
            4096,
            send,
        );
        let result = tool.execute(serde_json::json!({}));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("prompt"));
    }

    #[test]
    fn schema_for_generates_correct_name() {
        let schema = completion_tool_schema("openai", &ProviderConfig::openai());
        assert_eq!(schema.name, "complete_openai");
        assert!(schema.description.contains("openai"));
        assert!(
            schema.input_schema["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("prompt"))
        );
    }

    #[test]
    fn tool_name_format() {
        assert_eq!(tool_name_for("anthropic"), "complete_anthropic");
        assert_eq!(tool_name_for("openai"), "complete_openai");
    }
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p ox-gate`
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/ox-gate/src/tools.rs crates/ox-gate/src/lib.rs
git commit -m "feat(ox-gate): completion tools accept model/max_tokens parameters

Tools read defaults from GateStore. Callers can override model and
max_tokens per invocation. Removes hardcoded 4096 from complete_via_gate."
```

---

### Task 10: Quality gates and status doc

**Files:**
- Run: `./scripts/quality_gates.sh`
- Modify: `docs/design/rfc/structfs-tui-status.md`

- [ ] **Step 1: Run formatter**

Run: `./scripts/fmt.sh`

- [ ] **Step 2: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: all 14 gates pass.

- [ ] **Step 3: Fix any failures**

If clippy or tests fail, fix the issues. Common things to watch for:
- Unused imports from removed code paths
- Dead code warnings for fields that were renamed
- Test assertions using old path names

- [ ] **Step 4: Update status doc**

Add a Phase 5 section to `docs/design/rfc/structfs-tui-status.md` after the Phase 4c entry:

```markdown
#### Phase 5: Accounts-First Config Redesign (complete, 14/14 quality gates)
- AccountConfig shrunk to {provider, key} — model/max_tokens moved to GateStore Defaults
- GateStore: defaults/{account,model,max_tokens} paths replace bootstrap/model/max_tokens
- Per-account key resolution from config handle (any account, not just default)
- figment types: GateConfig with accounts HashMap + DefaultsConfig
- CLI: --account replaces --provider/--api-key
- Config paths: gate/defaults/* namespace, gate/accounts/{name}/* namespace
- Legacy env var mapping (ANTHROPIC_API_KEY/OPENAI_API_KEY) removed
- Completion tools accept model/max_tokens as parameters
- Backwards-compatible snapshot restore (legacy "bootstrap" field)
- **Spec:** `docs/superpowers/specs/2026-04-08-accounts-config-design.md`
- **Plan:** `docs/superpowers/plans/2026-04-08-accounts-config.md`
```

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "docs: update status for Phase 5 accounts-first config redesign"
```

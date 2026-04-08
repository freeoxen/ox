# Phase 4b: Config System Last Mile

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collapse ModelProvider into GateStore (one source of truth for all LLM config), then connect the config plumbing from Phase 4a into working features: figment startup config, TOML persistence, config handles, and agent worker cleanup.

**Architecture:** ModelProvider is eliminated — GateStore owns model ID and max_tokens on the bootstrap account, exposes convenience paths (`model`, `max_tokens`), and reads from an optional config handle. `synthesize_prompt()` reads `gate/model` and `gate/max_tokens` instead of `model/id` and `model/max_tokens`. figment composes startup config into a flat map for ConfigStore. ConfigStore persists runtime changes via StoreBacking.

**Tech Stack:** figment (with `toml` + `env` features), toml (for TomlFileBacking), existing ox-store-util StoreBacking trait

---

## File Map

| Action | File | Responsibility |
|--------|------|----------------|
| Modify | `crates/ox-gate/src/account.rs` | Add max_tokens to AccountConfig |
| Modify | `crates/ox-gate/src/lib.rs` | Convenience paths, with_config(), snapshot max_tokens |
| Modify | `crates/ox-gate/Cargo.toml` | Add ox-store-util dev-dep |
| Modify | `crates/ox-context/src/lib.rs` | Remove ModelProvider, update synthesize_prompt |
| Modify | `crates/ox-cli/src/thread_registry.rs` | Drop model mount, wire GateStore config handle |
| Modify | `crates/ox-cli/src/agents.rs` | Remove direct broker config reads |
| Modify | `crates/ox-web/src/lib.rs` | Use GateStore for model/max_tokens instead of ModelProvider |
| Modify | `crates/ox-ui/src/config_store.rs` | Add StoreBacking support |
| Modify | `crates/ox-ui/Cargo.toml` | Add ox-store-util dep |
| Create | `crates/ox-cli/src/toml_backing.rs` | StoreBacking for TOML files |
| Create | `crates/ox-cli/src/config.rs` | figment types + resolution |
| Modify | `crates/ox-cli/Cargo.toml` | Add figment, toml deps |
| Modify | `crates/ox-cli/src/main.rs` | Replace hand-rolled config with figment |
| Modify | `crates/ox-cli/src/broker_setup.rs` | Accept flat config map, wire backing |

---

### Task 1: Add max_tokens to AccountConfig + GateStore convenience paths

GateStore becomes the single source of truth for model config. AccountConfig gains `max_tokens`. GateStore exposes `model` and `max_tokens` as top-level read paths that resolve from the bootstrap account.

**Files:**
- Modify: `crates/ox-gate/src/account.rs`
- Modify: `crates/ox-gate/src/lib.rs`

- [ ] **Step 1: Write failing tests**

Add to `crates/ox-gate/src/lib.rs` tests:

```rust
#[test]
fn read_convenience_model_returns_bootstrap_account_model() {
    let mut gate = GateStore::new();
    // Default bootstrap is "anthropic", default model is "claude-sonnet-4-20250514"
    let record = gate.read(&path!("model")).unwrap().unwrap();
    match record {
        Record::Parsed(Value::String(s)) => assert_eq!(s, "claude-sonnet-4-20250514"),
        _ => panic!("expected string"),
    }
}

#[test]
fn read_convenience_max_tokens_returns_bootstrap_account_max_tokens() {
    let mut gate = GateStore::new();
    let record = gate.read(&path!("max_tokens")).unwrap().unwrap();
    match record {
        Record::Parsed(Value::Integer(n)) => assert_eq!(n, 4096),
        _ => panic!("expected integer"),
    }
}

#[test]
fn write_convenience_model_updates_bootstrap_account() {
    let mut gate = GateStore::new();
    gate.write(&path!("model"), Record::parsed(Value::String("gpt-4o".into())))
        .unwrap();
    // Read back via account path
    let record = gate.read(&path!("accounts/anthropic/model")).unwrap().unwrap();
    match record {
        Record::Parsed(Value::String(s)) => assert_eq!(s, "gpt-4o"),
        _ => panic!("expected string"),
    }
}

#[test]
fn write_convenience_max_tokens_updates_bootstrap_account() {
    let mut gate = GateStore::new();
    gate.write(
        &path!("max_tokens"),
        Record::parsed(Value::Integer(8192)),
    )
    .unwrap();
    let record = gate.read(&path!("accounts/anthropic/max_tokens")).unwrap().unwrap();
    match record {
        Record::Parsed(Value::Integer(n)) => assert_eq!(n, 8192),
        _ => panic!("expected integer"),
    }
}

#[test]
fn max_tokens_in_account_config() {
    let config = AccountConfig {
        provider: "anthropic".into(),
        key: String::new(),
        model: "claude-sonnet-4-20250514".into(),
        max_tokens: 8192,
    };
    assert_eq!(config.max_tokens, 8192);
}

#[test]
fn snapshot_includes_max_tokens() {
    let mut gate = GateStore::new();
    gate.write(
        &path!("max_tokens"),
        Record::parsed(Value::Integer(8192)),
    )
    .unwrap();

    let val = unwrap_value(gate.read(&path!("snapshot/state")).unwrap().unwrap());
    let json = value_to_json(val);
    let anthropic_acct = &json["accounts"]["anthropic"];
    assert_eq!(anthropic_acct["max_tokens"], 8192);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ox-gate -- read_convenience_model 2>&1 | head -20`
Expected: compilation error or test failure.

- [ ] **Step 3: Add max_tokens to AccountConfig**

In `crates/ox-gate/src/account.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    pub provider: String,
    pub key: String,
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_max_tokens() -> u32 {
    4096
}
```

- [ ] **Step 4: Update GateStore defaults to set max_tokens**

In `GateStore::new()`, update account construction:

```rust
accounts.insert(
    "anthropic".to_string(),
    AccountConfig {
        provider: "anthropic".to_string(),
        key: String::new(),
        model: "claude-sonnet-4-20250514".to_string(),
        max_tokens: 4096,
    },
);
accounts.insert(
    "openai".to_string(),
    AccountConfig {
        provider: "openai".to_string(),
        key: String::new(),
        model: "gpt-4o".to_string(),
        max_tokens: 4096,
    },
);
```

- [ ] **Step 5: Add convenience paths to Reader impl**

In the `Reader` impl's match on `first`, add before the `_ => Ok(None)` arm:

```rust
"model" => {
    let account = self.accounts.get(&self.bootstrap);
    Ok(account.map(|a| Record::parsed(Value::String(a.model.clone()))))
}

"max_tokens" => {
    let account = self.accounts.get(&self.bootstrap);
    Ok(account.map(|a| Record::parsed(Value::Integer(a.max_tokens as i64))))
}
```

- [ ] **Step 6: Add convenience paths to Writer impl**

In the `Writer` impl's match on `first`, add before the `_ => Err(...)` arm:

```rust
"model" => match data {
    Record::Parsed(Value::String(s)) => {
        if let Some(account) = self.accounts.get_mut(&self.bootstrap) {
            account.model = s;
            Ok(to.clone())
        } else {
            Err(StoreError::store("gate", "write", "no bootstrap account"))
        }
    }
    _ => Err(StoreError::store("gate", "write", "expected string for model")),
},

"max_tokens" => match data {
    Record::Parsed(Value::Integer(n)) => {
        if let Some(account) = self.accounts.get_mut(&self.bootstrap) {
            account.max_tokens = n as u32;
            Ok(to.clone())
        } else {
            Err(StoreError::store("gate", "write", "no bootstrap account"))
        }
    }
    _ => Err(StoreError::store("gate", "write", "expected integer for max_tokens")),
},
```

- [ ] **Step 7: Add max_tokens to account Reader**

In the `"accounts"` arm of Reader, add a match for `"max_tokens"`:

```rust
"max_tokens" => Ok(Some(Record::parsed(Value::Integer(config.max_tokens as i64)))),
```

And in the Writer `"accounts"` arm, add:

```rust
"max_tokens" => match data {
    Record::Parsed(Value::Integer(n)) => {
        if let Some(account) = self.accounts.get_mut(&name) {
            account.max_tokens = n as u32;
        } else {
            return Err(StoreError::store(
                "gate", "write", format!("no account named '{name}'"),
            ));
        }
        Ok(to.clone())
    }
    _ => Err(StoreError::store("gate", "write", "expected integer for max_tokens")),
},
```

- [ ] **Step 8: Update snapshot_state to include max_tokens**

In `snapshot_state()`, update the account serialization:

```rust
for (name, config) in &self.accounts {
    let mut acct = BTreeMap::new();
    acct.insert("model".to_string(), Value::String(config.model.clone()));
    acct.insert(
        "provider".to_string(),
        Value::String(config.provider.clone()),
    );
    acct.insert(
        "max_tokens".to_string(),
        Value::Integer(config.max_tokens as i64),
    );
    accounts_map.insert(name.clone(), Value::Map(acct));
}
```

Update `restore_from_snapshot()` to read max_tokens:

```rust
let max_tokens = acct_json
    .get("max_tokens")
    .and_then(|v| v.as_i64())
    .unwrap_or(4096) as u32;
new_accounts.insert(
    name.clone(),
    AccountConfig {
        provider,
        key: String::new(),
        model,
        max_tokens,
    },
);
```

- [ ] **Step 9: Run tests**

Run: `cargo test -p ox-gate`
Expected: All tests pass, including the 6 new ones.

- [ ] **Step 10: Commit**

```bash
git add crates/ox-gate/src/account.rs crates/ox-gate/src/lib.rs
git commit -m "feat(ox-gate): max_tokens on AccountConfig, convenience model/max_tokens paths"
```

---

### Task 2: GateStore with_config() for config-aware reads

GateStore gains an optional config handle. Convenience paths (`model`, `max_tokens`) and bootstrap account key reads check the config handle first.

**Files:**
- Modify: `crates/ox-gate/src/lib.rs`
- Modify: `crates/ox-gate/Cargo.toml`

- [ ] **Step 1: Write failing tests**

Add to `crates/ox-gate/src/lib.rs` tests:

```rust
#[test]
fn gate_config_handle_overrides_model() {
    use ox_store_util::LocalConfig;

    let mut config = LocalConfig::new();
    config.set("gate/model", Value::String("config-model".into()));

    let mut gate = GateStore::new().with_config(Box::new(config));

    let record = gate.read(&path!("model")).unwrap().unwrap();
    match record {
        Record::Parsed(Value::String(s)) => assert_eq!(s, "config-model"),
        _ => panic!("expected string"),
    }
}

#[test]
fn gate_config_handle_overrides_max_tokens() {
    use ox_store_util::LocalConfig;

    let mut config = LocalConfig::new();
    config.set("gate/max_tokens", Value::Integer(16384));

    let mut gate = GateStore::new().with_config(Box::new(config));

    let record = gate.read(&path!("max_tokens")).unwrap().unwrap();
    match record {
        Record::Parsed(Value::Integer(n)) => assert_eq!(n, 16384),
        _ => panic!("expected integer"),
    }
}

#[test]
fn gate_config_handle_overrides_bootstrap_key() {
    use ox_store_util::LocalConfig;

    let mut config = LocalConfig::new();
    config.set("gate/api_key", Value::String("config-key-123".into()));

    let mut gate = GateStore::new().with_config(Box::new(config));

    let record = gate.read(&path!("accounts/anthropic/key")).unwrap().unwrap();
    match record {
        Record::Parsed(Value::String(s)) => assert_eq!(s, "config-key-123"),
        _ => panic!("expected string"),
    }
}

#[test]
fn gate_config_handle_falls_back_to_local() {
    // No config handle — reads use local fields
    let mut gate = GateStore::new();
    let record = gate.read(&path!("model")).unwrap().unwrap();
    match record {
        Record::Parsed(Value::String(s)) => assert_eq!(s, "claude-sonnet-4-20250514"),
        _ => panic!("expected string"),
    }
}

#[test]
fn gate_config_key_populates_completion_schemas() {
    use ox_store_util::LocalConfig;

    let mut config = LocalConfig::new();
    config.set("gate/api_key", Value::String("sk-from-config".into()));

    let mut gate = GateStore::new().with_config(Box::new(config));

    let schemas = gate.completion_tool_schemas();
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].name, "complete_anthropic");
}
```

- [ ] **Step 2: Add ox-store-util dev-dependency**

In `crates/ox-gate/Cargo.toml`:

```toml
[dev-dependencies]
ox-store-util = { path = "../ox-store-util" }
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ox-gate -- gate_config_handle 2>&1 | head -20`
Expected: compilation error — `with_config` doesn't exist.

- [ ] **Step 4: Implement with_config() and config-aware reads**

Add `config` field to `GateStore`:

```rust
pub struct GateStore {
    providers: HashMap<String, ProviderConfig>,
    accounts: HashMap<String, AccountConfig>,
    bootstrap: String,
    catalogs: HashMap<String, Vec<ModelInfo>>,
    config: Option<Box<dyn Store + Send + Sync>>,
}
```

Initialize `config: None` in `new()`.

Add builder:

```rust
/// Attach a config store whose values take priority over local fields.
///
/// Config handle paths consulted:
/// - `gate/model` → overrides bootstrap account model
/// - `gate/max_tokens` → overrides bootstrap account max_tokens
/// - `gate/api_key` or `gate/api_key_raw` → provides key for bootstrap account
/// - `gate/provider` → overrides bootstrap selection
pub fn with_config(mut self, config: Box<dyn Store + Send + Sync>) -> Self {
    self.config = Some(config);
    self
}
```

Add helper for resolving config values:

```rust
/// Try reading a string from the config handle.
fn config_string(&mut self, path_str: &str) -> Option<String> {
    let config = self.config.as_mut()?;
    let path = Path::parse(path_str).ok()?;
    let record = config.read(&path).ok()??;
    match record.as_value() {
        Some(Value::String(s)) if !s.is_empty() && s != "***" => Some(s.clone()),
        _ => None,
    }
}

/// Try reading an integer from the config handle.
fn config_integer(&mut self, path_str: &str) -> Option<i64> {
    let config = self.config.as_mut()?;
    let path = Path::parse(path_str).ok()?;
    let record = config.read(&path).ok()??;
    match record.as_value() {
        Some(Value::Integer(n)) => Some(*n),
        _ => None,
    }
}
```

Update convenience path reads to check config first:

```rust
"model" => {
    if let Some(s) = self.config_string("gate/model") {
        return Ok(Some(Record::parsed(Value::String(s))));
    }
    let account = self.accounts.get(&self.bootstrap);
    Ok(account.map(|a| Record::parsed(Value::String(a.model.clone()))))
}

"max_tokens" => {
    if let Some(n) = self.config_integer("gate/max_tokens") {
        return Ok(Some(Record::parsed(Value::Integer(n))));
    }
    let account = self.accounts.get(&self.bootstrap);
    Ok(account.map(|a| Record::parsed(Value::Integer(a.max_tokens as i64))))
}
```

Update account key reads to check config for bootstrap:

```rust
"key" => {
    if name == self.bootstrap && config.key.is_empty() {
        // Try config handle for bootstrap account key
        if let Some(k) = self.config_string("gate/api_key_raw") {
            return Ok(Some(Record::parsed(Value::String(k))));
        }
        if let Some(k) = self.config_string("gate/api_key") {
            return Ok(Some(Record::parsed(Value::String(k))));
        }
    }
    Ok(Some(Record::parsed(Value::String(config.key.clone()))))
}
```

**Note:** The borrow checker issue — `self.config_string()` borrows `&mut self` but we've already matched on `self.accounts.get(name)`. Fix by reading config *before* the account lookup:

```rust
"accounts" => {
    if from.components.len() < 2 {
        return Ok(None);
    }
    let name = from.components[1].as_str();

    if from.components.len() > 2 {
        let field = from.components[2].as_str();

        // Check config handle before account lookup (avoids borrow conflict)
        if name == self.bootstrap {
            match field {
                "key" => {
                    // Try config for bootstrap key
                    if let Some(k) = self.config_string("gate/api_key_raw")
                        .or_else(|| self.config_string("gate/api_key"))
                    {
                        // Only use config key if local key is empty
                        if self.accounts.get(name).map(|a| a.key.is_empty()).unwrap_or(true) {
                            return Ok(Some(Record::parsed(Value::String(k))));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let Some(config) = self.accounts.get(name) else {
        return Ok(None);
    };

    // ... rest of existing match
}
```

Update `completion_tool_schemas` to `&mut self` and use config-resolved keys:

```rust
pub fn completion_tool_schemas(&mut self) -> Vec<ToolSchema> {
    let names: Vec<String> = self.accounts.keys().cloned().collect();
    names.iter().filter_map(|name| {
        // Check if account has a key (local or from config)
        let has_key = {
            let local_key = self.accounts.get(name).map(|a| a.key.clone()).unwrap_or_default();
            if !local_key.is_empty() {
                true
            } else if name == &self.bootstrap {
                self.config_string("gate/api_key_raw")
                    .or_else(|| self.config_string("gate/api_key"))
                    .is_some()
            } else {
                false
            }
        };
        if !has_key { return None; }
        let account = self.accounts.get(name)?;
        let provider = self.providers.get(&account.provider)?;
        Some(tools::completion_tool_schema(name, provider))
    }).collect()
}
```

Similarly update `create_completion_tools` to `&mut self`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p ox-gate`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-gate/src/lib.rs crates/ox-gate/Cargo.toml
git commit -m "feat(ox-gate): with_config() for config-aware model/key/max_tokens reads"
```

---

### Task 3: Remove ModelProvider, update synthesize_prompt

ModelProvider is removed. `synthesize_prompt()` reads model config from `gate/model` and `gate/max_tokens` instead of `model/id` and `model/max_tokens`.

**Files:**
- Modify: `crates/ox-context/src/lib.rs`

- [ ] **Step 1: Update synthesize_prompt to read gate/ paths**

In the `synthesize_prompt` function, replace the model ID and max_tokens reads:

**Old (remove):**
```rust
// Read model ID
let model_id = {
    let record = reader.read(&path!("model/id"))?.ok_or_else(|| {
        StoreError::store("synthesize_prompt", "read", "model store returned None for id")
    })?;
    // ...
};

// Read max_tokens
let max_tokens = {
    let record = reader.read(&path!("model/max_tokens"))?.ok_or_else(|| {
        StoreError::store("synthesize_prompt", "read", "model store returned None for max_tokens")
    })?;
    // ...
};
```

**New:**
```rust
// Read model ID from gate
let model_id = {
    let record = reader.read(&path!("gate/model"))?.ok_or_else(|| {
        StoreError::store(
            "synthesize_prompt",
            "read",
            "gate store returned None for model",
        )
    })?;
    match record {
        Record::Parsed(Value::String(s)) => s,
        _ => {
            return Err(StoreError::store(
                "synthesize_prompt",
                "read",
                "expected string from gate store for model",
            ));
        }
    }
};

// Read max_tokens from gate
let max_tokens = {
    let record = reader.read(&path!("gate/max_tokens"))?.ok_or_else(|| {
        StoreError::store(
            "synthesize_prompt",
            "read",
            "gate store returned None for max_tokens",
        )
    })?;
    match record {
        Record::Parsed(Value::Integer(n)) => n as u32,
        _ => {
            return Err(StoreError::store(
                "synthesize_prompt",
                "read",
                "expected integer from gate store for max_tokens",
            ));
        }
    }
};
```

- [ ] **Step 2: Remove ModelProvider entirely**

Delete the `ModelProvider` struct, its `impl Reader`, `impl Writer`, and the `with_config()` method. This is roughly lines 389-543.

Remove `ModelProvider` from the doc comment at the top of the file (line 13).

Remove the `pub use ox_kernel::ModelInfo;` if it was only used by ModelProvider (check — it may still be used elsewhere).

- [ ] **Step 3: Update tests**

Remove all `ModelProvider`-specific tests:
- `model_snapshot_read_returns_hash_and_state`
- `model_snapshot_read_state_only`
- `model_snapshot_read_hash_only`
- `model_snapshot_write_restores_state`
- `model_snapshot_write_state_path`
- `model_provider_reads_from_config_handle`
- `model_provider_falls_back_to_local`

Update `build_full_namespace()`:

```rust
fn build_full_namespace() -> Namespace {
    let mut ns = Namespace::new();
    ns.mount(
        "system",
        Box::new(SystemProvider::new("You are helpful.".to_string())),
    );
    ns.mount("tools", Box::new(ToolsProvider::new(vec![])));
    ns.mount("history", Box::new(ox_history::HistoryProvider::new()));
    ns.mount("gate", Box::new(ox_gate::GateStore::new()));
    ns
}
```

Note: No `"model"` mount. GateStore is already mounted at `"gate"`.

Update `namespace_snapshot_discovery_all_stores`:

```rust
#[test]
fn namespace_snapshot_discovery_all_stores() {
    let mut ns = build_full_namespace();

    // Participating stores return Some
    assert!(ns.read(&path!("system/snapshot")).unwrap().is_some());
    assert!(ns.read(&path!("history/snapshot")).unwrap().is_some());
    assert!(ns.read(&path!("gate/snapshot")).unwrap().is_some());

    // Non-participating store returns None
    assert!(ns.read(&path!("tools/snapshot")).unwrap().is_none());
}
```

Remove `namespace_snapshot_roundtrip` test (it uses ModelProvider). Or rewrite it to use GateStore's model convenience path:

```rust
#[test]
fn namespace_snapshot_roundtrip() {
    let mut ns = Namespace::new();
    ns.mount(
        "system",
        Box::new(SystemProvider::new("original".to_string())),
    );
    ns.mount("gate", Box::new(ox_gate::GateStore::new()));

    // Read snapshots
    let sys_snap = unwrap_value(ns.read(&path!("system/snapshot/state")).unwrap().unwrap());
    let gate_snap = unwrap_value(ns.read(&path!("gate/snapshot/state")).unwrap().unwrap());

    // Mutate
    ns.write(
        &path!("system"),
        Record::parsed(Value::String("changed".to_string())),
    )
    .unwrap();
    ns.write(
        &path!("gate/model"),
        Record::parsed(Value::String("gpt-4o".to_string())),
    )
    .unwrap();

    // Restore from snapshots
    ns.write(&path!("system/snapshot/state"), Record::parsed(sys_snap))
        .unwrap();
    ns.write(&path!("gate/snapshot/state"), Record::parsed(gate_snap))
        .unwrap();

    // Verify restoration
    let val = unwrap_value(ns.read(&path!("system")).unwrap().unwrap());
    assert_eq!(val, Value::String("original".to_string()));

    let val = unwrap_value(ns.read(&path!("gate/model")).unwrap().unwrap());
    assert_eq!(val, Value::String("claude-sonnet-4-20250514".to_string()));
}
```

Update `synthesize_prompt_standalone`:

```rust
#[test]
fn synthesize_prompt_standalone() {
    let mut ns = build_full_namespace();
    let user_msg = serde_json::json!({"role": "user", "content": "hello"});
    ns.write(
        &path!("history/append"),
        Record::parsed(structfs_serde_store::json_to_value(user_msg)),
    )
    .unwrap();

    let result = synthesize_prompt(&mut ns).unwrap().unwrap();
    let value = result.as_value().unwrap().clone();
    let json = structfs_serde_store::value_to_json(value);
    let request: CompletionRequest = serde_json::from_value(json).unwrap();
    assert_eq!(request.model, "claude-sonnet-4-20250514");
    assert_eq!(request.system, "You are helpful.");
    assert_eq!(request.messages.len(), 1);
    assert!(request.stream);
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-context`
Expected: All tests pass.

- [ ] **Step 5: Fix downstream compilation**

Run: `cargo check 2>&1 | head -40`

This will likely show errors in:
- `ox-cli` (ThreadNamespace uses ModelProvider)
- `ox-web` (mounts ModelProvider)
- `ox-core` (may re-export ModelProvider)

Fix each:

**ox-cli/src/thread_registry.rs** — remove ModelProvider from ThreadNamespace:

```rust
pub struct ThreadNamespace {
    system: SystemProvider,
    history: HistoryProvider,
    tools: ToolsProvider,
    pub gate: GateStore,
    pub approval: ApprovalStore,
}
```

Update `new_default()`:
```rust
pub fn new_default() -> Self {
    Self {
        system: SystemProvider::new(SYSTEM_PROMPT.to_string()),
        history: HistoryProvider::new(),
        tools: ToolsProvider::new(vec![]),
        gate: GateStore::new(),
        approval: ApprovalStore::new(),
    }
}
```

Update `route()` — remove `"model"` arm:
```rust
fn route(&mut self, path: &Path) -> Option<(&mut dyn Store, Path)> {
    if path.is_empty() {
        return None;
    }
    let prefix = path.components[0].as_str();
    let sub = Path::from_components(path.components[1..].to_vec());
    match prefix {
        "system" => Some((&mut self.system as &mut dyn Store, sub)),
        "history" => Some((&mut self.history as &mut dyn Store, sub)),
        "tools" => Some((&mut self.tools as &mut dyn Store, sub)),
        "gate" => Some((&mut self.gate as &mut dyn Store, sub)),
        _ => None,
    }
}
```

Remove the ModelProvider config handle wiring from `ensure_mounted()` — GateStore handle wiring comes in Task 5.

Update thread_registry tests: change `model/id` reads to `gate/model`:

```rust
// In routes_to_correct_store test:
let model_path = Path::parse("t_a/gate/model").unwrap();
let result = futures_or_poll(reg.read(&model_path)).unwrap();
match result.unwrap().as_value().unwrap() {
    Value::String(s) => assert_eq!(s, "claude-sonnet-4-20250514"),
    other => panic!("expected model string, got {:?}", other),
}
```

Remove the ModelProvider import from thread_registry.

**ox-web/src/lib.rs** — use GateStore instead of ModelProvider:

Remove the `ModelProvider::new(model, max_tokens)` mount. GateStore is already mounted. Set model and max_tokens on GateStore instead:

Where the code currently has:
```rust
context.mount("model", Box::new(ModelProvider::new(model, max_tokens)));
```

Replace with writes to the existing GateStore mount:
```rust
// model and max_tokens are set on GateStore's bootstrap account
// GateStore is already mounted at "gate" — write via namespace
context.write(&path!("gate/model"), Record::parsed(Value::String(model))).ok();
context.write(&path!("gate/max_tokens"), Record::parsed(Value::Integer(max_tokens as i64))).ok();
```

Where `model/id` is read in the agentic loop (line ~804), change to `gate/model`:
```rust
let model_id = {
    let record = context_ref
        .borrow_mut()
        .read(&path!("gate/model"))
        .map_err(|e| e.to_string())?;
    match record {
        Some(Record::Parsed(Value::String(s))) => s,
        _ => String::new(),
    }
};
```

Where `model/id` and `model/max_tokens` are read in debug (lines ~284-293), change to `gate/model` and `gate/max_tokens`.

Where `set_model()` writes to `model/id`, change to `gate/model`.

Remove the `ModelProvider` import from ox-web.

- [ ] **Step 6: Run full check**

Run: `cargo check && cargo check --target wasm32-unknown-unknown -p ox-web`
Expected: Both pass.

- [ ] **Step 7: Run all tests**

Run: `cargo test`
Expected: All pass.

- [ ] **Step 8: Commit**

```bash
git add crates/ox-context/src/lib.rs crates/ox-cli/src/thread_registry.rs crates/ox-web/src/lib.rs
git commit -m "refactor: collapse ModelProvider into GateStore, synthesize_prompt reads gate/ paths"
```

---

### Task 4: ConfigStore persistence via StoreBacking

ConfigStore gains optional `StoreBacking` for persisting the runtime layer. API keys excluded.

**Files:**
- Modify: `crates/ox-ui/src/config_store.rs`
- Modify: `crates/ox-ui/Cargo.toml`

- [ ] **Step 1: Write failing tests**

Add to `crates/ox-ui/src/config_store.rs` tests:

```rust
#[test]
fn save_runtime_persists_to_backing() {
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct CaptureBacking {
        saved: Arc<Mutex<Option<Value>>>,
    }

    impl ox_store_util::StoreBacking for CaptureBacking {
        fn load(&self) -> Result<Option<Value>, StoreError> {
            Ok(None)
        }
        fn save(&self, value: &Value) -> Result<(), StoreError> {
            *self.saved.lock().unwrap() = Some(value.clone());
            Ok(())
        }
    }

    let base = BTreeMap::new();
    let saved = Arc::new(Mutex::new(None));
    let backing = CaptureBacking { saved: saved.clone() };
    let mut config = ConfigStore::new(base);
    config.set_backing(Box::new(backing));

    config
        .write(&path!("gate/model"), Record::parsed(Value::String("gpt-4o".into())))
        .unwrap();

    config.save_runtime().unwrap();

    let saved_val = saved.lock().unwrap().clone().unwrap();
    match saved_val {
        Value::Map(m) => {
            assert_eq!(m.get("gate/model").unwrap(), &Value::String("gpt-4o".into()));
        }
        _ => panic!("expected map"),
    }
}

#[test]
fn save_runtime_excludes_api_key() {
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct CaptureBacking {
        saved: Arc<Mutex<Option<Value>>>,
    }

    impl ox_store_util::StoreBacking for CaptureBacking {
        fn load(&self) -> Result<Option<Value>, StoreError> { Ok(None) }
        fn save(&self, value: &Value) -> Result<(), StoreError> {
            *self.saved.lock().unwrap() = Some(value.clone());
            Ok(())
        }
    }

    let base = BTreeMap::new();
    let saved = Arc::new(Mutex::new(None));
    let backing = CaptureBacking { saved: saved.clone() };
    let mut config = ConfigStore::new(base);
    config.set_backing(Box::new(backing));

    config
        .write(&path!("gate/api_key"), Record::parsed(Value::String("sk-secret".into())))
        .unwrap();
    config
        .write(&path!("gate/model"), Record::parsed(Value::String("gpt-4o".into())))
        .unwrap();

    config.save_runtime().unwrap();

    let saved_val = saved.lock().unwrap().clone().unwrap();
    match saved_val {
        Value::Map(m) => {
            assert!(!m.contains_key("gate/api_key"), "api_key must not be persisted");
            assert!(m.contains_key("gate/model"));
        }
        _ => panic!("expected map"),
    }
}

#[test]
fn with_backing_loads_saved_values_into_base() {
    struct PreloadBacking;
    impl ox_store_util::StoreBacking for PreloadBacking {
        fn load(&self) -> Result<Option<Value>, StoreError> {
            let mut m = BTreeMap::new();
            m.insert("gate/model".to_string(), Value::String("from-disk".into()));
            Ok(Some(Value::Map(m)))
        }
        fn save(&self, _value: &Value) -> Result<(), StoreError> { Ok(()) }
    }

    let mut config = ConfigStore::with_backing(BTreeMap::new(), Box::new(PreloadBacking));

    let record = config.read(&path!("gate/model")).unwrap().unwrap();
    assert_eq!(record.as_value().unwrap(), &Value::String("from-disk".into()));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ox-ui -- save_runtime 2>&1 | head -10`
Expected: compilation error.

- [ ] **Step 3: Add ox-store-util dep to ox-ui**

In `crates/ox-ui/Cargo.toml`:

```toml
ox-store-util = { path = "../ox-store-util" }
```

- [ ] **Step 4: Implement StoreBacking support**

Add to `ConfigStore`:

```rust
pub struct ConfigStore {
    base: BTreeMap<String, Value>,
    runtime: BTreeMap<String, Value>,
    threads: BTreeMap<String, BTreeMap<String, Value>>,
    backing: Option<Box<dyn ox_store_util::StoreBacking>>,
}
```

Update `new()` to init `backing: None`.

Add methods:

```rust
/// Create with base values and a persistence backing.
/// Loads saved values from backing into the base layer.
pub fn with_backing(
    mut base: BTreeMap<String, Value>,
    backing: Box<dyn ox_store_util::StoreBacking>,
) -> Self {
    if let Ok(Some(Value::Map(saved))) = backing.load() {
        for (k, v) in saved {
            base.insert(k, v);
        }
    }
    Self {
        base,
        runtime: BTreeMap::new(),
        threads: BTreeMap::new(),
        backing: Some(backing),
    }
}

/// Set the persistence backing after construction.
pub fn set_backing(&mut self, backing: Box<dyn ox_store_util::StoreBacking>) {
    self.backing = Some(backing);
}

/// Persist the runtime layer to backing. API keys excluded.
pub fn save_runtime(&self) -> Result<(), structfs_core_store::Error> {
    let Some(ref backing) = self.backing else {
        return Ok(());
    };
    let filtered: BTreeMap<String, Value> = self
        .runtime
        .iter()
        .filter(|(k, _)| !k.contains("api_key"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    backing.save(&Value::Map(filtered))
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ox-ui -- config_store`
Expected: All pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-ui/src/config_store.rs crates/ox-ui/Cargo.toml
git commit -m "feat(ox-ui): ConfigStore persistence via StoreBacking"
```

---

### Task 5: TomlFileBacking + figment config types

Create TomlFileBacking for TOML persistence, and figment config types for startup resolution. Config uses a single `[gate]` section — no `[model]` section.

**Files:**
- Create: `crates/ox-cli/src/toml_backing.rs`
- Create: `crates/ox-cli/src/config.rs`
- Modify: `crates/ox-cli/Cargo.toml`
- Modify: `crates/ox-cli/src/main.rs` (add modules)

- [ ] **Step 1: Add dependencies**

In `crates/ox-cli/Cargo.toml`:

```toml
toml = "0.8"
figment = { version = "0.10", features = ["toml", "env"] }
```

- [ ] **Step 2: Create TomlFileBacking with tests**

Create `crates/ox-cli/src/toml_backing.rs`:

```rust
//! TomlFileBacking — persists flat path-keyed BTreeMap as nested TOML.
//!
//! Path keys like "gate/model" become nested TOML:
//! ```toml
//! [gate]
//! model = "claude-sonnet-4-20250514"
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;
use structfs_core_store::{Error as StoreError, Value};

pub struct TomlFileBacking {
    path: PathBuf,
}

impl TomlFileBacking {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl ox_store_util::StoreBacking for TomlFileBacking {
    fn load(&self) -> Result<Option<Value>, StoreError> {
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

    fn save(&self, value: &Value) -> Result<(), StoreError> {
        let Value::Map(flat) = value else {
            return Err(StoreError::store("toml_backing", "save", "expected Value::Map"));
        };

        let mut root = toml::Table::new();
        for (path_key, val) in flat {
            let parts: Vec<&str> = path_key.split('/').collect();
            insert_nested(&mut root, &parts, val);
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

fn insert_nested(table: &mut toml::Table, parts: &[&str], value: &Value) {
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
    if let toml::Value::Table(ref mut sub_table) = sub {
        insert_nested(sub_table, &parts[1..], value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_store_util::StoreBacking;

    #[test]
    fn roundtrip_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let backing = TomlFileBacking::new(path.clone());

        assert!(backing.load().unwrap().is_none());

        let mut map = BTreeMap::new();
        map.insert("gate/model".to_string(), Value::String("gpt-4o".into()));
        map.insert("gate/max_tokens".to_string(), Value::Integer(8192));
        map.insert("gate/provider".to_string(), Value::String("openai".into()));
        backing.save(&Value::Map(map)).unwrap();

        assert!(path.exists());

        let loaded = backing.load().unwrap().unwrap();
        match loaded {
            Value::Map(m) => {
                assert_eq!(m.get("gate/model").unwrap(), &Value::String("gpt-4o".into()));
                assert_eq!(m.get("gate/max_tokens").unwrap(), &Value::Integer(8192));
                assert_eq!(m.get("gate/provider").unwrap(), &Value::String("openai".into()));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn toml_file_is_human_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let backing = TomlFileBacking::new(path.clone());

        let mut map = BTreeMap::new();
        map.insert("gate/model".to_string(), Value::String("gpt-4o".into()));
        map.insert("gate/max_tokens".to_string(), Value::Integer(8192));
        backing.save(&Value::Map(map)).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[gate]"), "expected [gate] section, got:\n{content}");
        assert!(content.contains("gpt-4o"));
    }
}
```

- [ ] **Step 3: Create config.rs with figment types and tests**

Create `crates/ox-cli/src/config.rs`:

```rust
//! Config resolution via figment — defaults → TOML file → env vars → CLI flags.
//!
//! All config lives under the `[gate]` section. There is no separate `[model]` section.
//! Environment variables: OX_GATE_MODEL, OX_GATE_MAX_TOKENS, OX_GATE_PROVIDER, OX_GATE_API_KEY.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use structfs_core_store::Value;

/// Top-level config. Only `gate` section — all LLM config goes through ox-gate.
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

/// CLI flag overrides — only Some values are applied.
#[derive(Debug, Default)]
pub struct CliOverrides {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub max_tokens: Option<i64>,
}

impl OxConfig {
    /// Apply CLI overrides (highest priority).
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

    /// Convert to flat BTreeMap<String, Value> for ConfigStore.
    pub fn to_flat_map(&self) -> BTreeMap<String, Value> {
        let mut map = BTreeMap::new();
        map.insert("gate/model".to_string(), Value::String(self.gate.model.clone()));
        map.insert("gate/max_tokens".to_string(), Value::Integer(self.gate.max_tokens));
        map.insert("gate/provider".to_string(), Value::String(self.gate.provider.clone()));
        if let Some(ref key) = self.gate.api_key {
            map.insert("gate/api_key".to_string(), Value::String(key.clone()));
        }
        map
    }
}

/// Resolve config: defaults → TOML file → env vars → CLI overrides.
pub fn resolve_config(config_dir: &Path, overrides: &CliOverrides) -> OxConfig {
    use figment::Figment;
    use figment::providers::{Env, Toml};

    let toml_path = config_dir.join("config.toml");

    let figment = Figment::new()
        .merge(figment::providers::Serialized::defaults(OxConfig::default()))
        .merge(Toml::file(toml_path))
        .merge(Env::prefixed("OX_").split("_"));

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

        assert_eq!(flat.get("gate/model").unwrap(), &Value::String("claude-sonnet-4-20250514".into()));
        assert_eq!(flat.get("gate/max_tokens").unwrap(), &Value::Integer(4096));
        assert_eq!(flat.get("gate/provider").unwrap(), &Value::String("anthropic".into()));
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

        assert_eq!(flat.get("gate/provider").unwrap(), &Value::String("openai".into()));
        assert_eq!(flat.get("gate/model").unwrap(), &Value::String("gpt-4o".into()));
        assert_eq!(flat.get("gate/api_key").unwrap(), &Value::String("sk-test".into()));
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

        assert_eq!(flat.get("gate/model").unwrap(), &Value::String("from-file".into()));
        assert_eq!(flat.get("gate/max_tokens").unwrap(), &Value::Integer(8192));
        assert_eq!(flat.get("gate/provider").unwrap(), &Value::String("openai".into()));
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
```

- [ ] **Step 4: Register modules in main.rs**

In `crates/ox-cli/src/main.rs`, add:

```rust
mod config;
mod toml_backing;
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ox-cli -- toml_backing config::tests`
Expected: All pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/toml_backing.rs crates/ox-cli/src/config.rs crates/ox-cli/src/main.rs crates/ox-cli/Cargo.toml
git commit -m "feat(ox-cli): TomlFileBacking + figment config types (gate-only)"
```

---

### Task 6: Wire figment + backing into startup

Replace hand-rolled config in main.rs with figment. Wire TomlFileBacking into ConfigStore via broker_setup.

**Files:**
- Modify: `crates/ox-cli/src/main.rs`
- Modify: `crates/ox-cli/src/broker_setup.rs`

- [ ] **Step 1: Update main.rs**

Replace the config resolution and broker setup call. The new `main()` after `let cli = Cli::parse()`:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let workspace =
        std::fs::canonicalize(&cli.workspace).unwrap_or_else(|_| PathBuf::from(&cli.workspace));

    let inbox_root = {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".ox")
    };

    // Resolve config: defaults → TOML → env → CLI flags
    let overrides = config::CliOverrides {
        provider: if cli.provider != "anthropic" {
            Some(cli.provider.clone())
        } else {
            None
        },
        model: cli.model.clone(),
        api_key: cli.api_key.clone(),
        max_tokens: if cli.max_tokens != 4096 {
            Some(cli.max_tokens as i64)
        } else {
            None
        },
    };
    let resolved = config::resolve_config(&inbox_root, &overrides);

    if resolved.gate.api_key.is_none() {
        eprintln!("error: no API key provided");
        eprintln!("  pass --api-key, set OX_GATE_API_KEY, or set ANTHROPIC_API_KEY / OPENAI_API_KEY");
        std::process::exit(1);
    }

    let flat_config = resolved.to_flat_map();

    let theme = theme::Theme::default();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let broker_inbox = ox_inbox::InboxStore::open(&inbox_root)
        .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;
    let broker_bindings = bindings::default_bindings();
    let broker_handle = rt.block_on(broker_setup::setup(
        broker_inbox,
        broker_bindings,
        inbox_root.clone(),
        flat_config,
    ));
    let client = broker_handle.client();

    let mut app = app::App::new(
        workspace,
        inbox_root.clone(),
        cli.no_policy,
        broker_handle.broker.clone(),
        rt.handle().clone(),
    )
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    let mut terminal = ratatui::init();
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture).ok();

    let result = rt.block_on(event_loop::run_async(
        &mut app,
        &client,
        &theme,
        &mut terminal,
    ));

    crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture).ok();
    ratatui::restore();

    result?;
    Ok(())
}
```

- [ ] **Step 2: Update broker_setup::setup signature**

Change to accept `BTreeMap<String, Value>`:

```rust
pub async fn setup(
    inbox: InboxStore,
    bindings: Vec<Binding>,
    inbox_root: std::path::PathBuf,
    config_values: std::collections::BTreeMap<String, structfs_core_store::Value>,
) -> BrokerHandle {
```

Replace the ConfigStore block:

```rust
// Mount ConfigStore with figment-resolved base + TOML file backing
{
    use ox_ui::ConfigStore;

    let toml_path = inbox_root.join("config.toml");
    let backing = crate::toml_backing::TomlFileBacking::new(toml_path);
    let config = ConfigStore::with_backing(config_values, Box::new(backing));

    servers.push(broker.mount(path!("config"), config).await);
}
```

- [ ] **Step 3: Update broker_setup tests**

Update `test_setup()`:

```rust
async fn test_setup() -> BrokerHandle {
    let bindings = crate::bindings::default_bindings();
    let mut config = std::collections::BTreeMap::new();
    config.insert("gate/model".into(), Value::String("claude-sonnet-4-20250514".into()));
    config.insert("gate/provider".into(), Value::String("anthropic".into()));
    config.insert("gate/max_tokens".into(), Value::Integer(4096));
    config.insert("gate/api_key".into(), Value::String("test-key".into()));
    setup(test_inbox(), bindings, test_inbox_root(), config).await
}
```

Update `config_store_mounted_with_defaults` test to use `gate/` paths:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_store_mounted_with_defaults() {
    let handle = test_setup().await;
    let client = handle.client();

    let model = client.read(&path!("config/gate/model")).await.unwrap().unwrap();
    assert_eq!(model.as_value().unwrap(), &Value::String("claude-sonnet-4-20250514".into()));

    let provider = client.read(&path!("config/gate/provider")).await.unwrap().unwrap();
    assert_eq!(provider.as_value().unwrap(), &Value::String("anthropic".into()));
}
```

Update `thread_model_resolves_through_config` to use `gate/model`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_model_resolves_through_config() {
    let handle = test_setup().await;
    let client = handle.client();

    let model = client.read(&path!("threads/t_test/gate/model")).await.unwrap().unwrap();
    assert_eq!(model.as_value().unwrap(), &Value::String("claude-sonnet-4-20250514".into()));

    client
        .write(&path!("config/gate/model"), Record::parsed(Value::String("gpt-4o".into())))
        .await
        .unwrap();

    let model = client.read(&path!("threads/t_test/gate/model")).await.unwrap().unwrap();
    assert_eq!(model.as_value().unwrap(), &Value::String("gpt-4o".into()));
}
```

- [ ] **Step 4: Update view_state.rs config reads**

Change `config/model/id` → `config/gate/model` and `config/gate/provider` stays the same:

```rust
let model = match client.read(&path!("config/gate/model")).await {
    Ok(Some(r)) => match r.as_value() {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    },
    _ => String::new(),
};
```

- [ ] **Step 5: Run all tests**

Run: `cargo test -p ox-cli`
Expected: All pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/main.rs crates/ox-cli/src/broker_setup.rs crates/ox-cli/src/view_state.rs
git commit -m "feat(ox-cli): wire figment + TomlFileBacking into startup"
```

---

### Task 7: Wire GateStore config handle + clean up agent worker

ThreadRegistry wires config handles into GateStore. Agent worker reads config through the thread's GateStore instead of direct broker reads.

**Files:**
- Modify: `crates/ox-cli/src/thread_registry.rs`
- Modify: `crates/ox-cli/src/agents.rs`

- [ ] **Step 1: Wire GateStore config handle in ThreadRegistry**

In `ensure_mounted()`, after creating the thread namespace, wire the config handle:

```rust
if let Some(client) = &self.broker_client {
    // Config handle for GateStore — scoped to thread's config cascade
    let config_client = client.scoped(&format!("config/threads/{thread_id}"));
    let config_adapter = ox_broker::SyncClientAdapter::new(
        config_client,
        tokio::runtime::Handle::current(),
    );
    let read_only = ox_store_util::ReadOnly::new(config_adapter);
    ns.gate = GateStore::new().with_config(Box::new(read_only));
}
```

- [ ] **Step 2: Add integration test**

Add to `broker_setup.rs` tests:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_gate_reads_api_key_from_config() {
    let handle = test_setup().await;
    let client = handle.client();

    let key = client
        .read(&path!("threads/t_gate/gate/accounts/anthropic/key"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(key.as_value().unwrap(), &Value::String("test-key".into()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_gate_reads_model_from_config() {
    let handle = test_setup().await;
    let client = handle.client();

    let model = client
        .read(&path!("threads/t_model/gate/model"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        model.as_value().unwrap(),
        &Value::String("claude-sonnet-4-20250514".into())
    );
}
```

- [ ] **Step 3: Clean up agent worker**

In `agents.rs`, replace the direct broker config reads (lines ~217-244) with reads from the scoped adapter. Remove the `broker_client` variable used for config reads (keep it for inbox writes).

Replace:
```rust
// Read provider and API key from global config (unscoped client)
let provider = tokio::task::block_in_place(|| { ... });
let api_key_for_transport = tokio::task::block_in_place(|| { ... });
```

With:
```rust
// Read provider and API key from thread's GateStore (resolves through config handle)
let bootstrap = match adapter.read(&path!("gate/bootstrap")) {
    Ok(Some(r)) => match r.as_value() {
        Some(Value::String(s)) => s.clone(),
        _ => "anthropic".to_string(),
    },
    _ => "anthropic".to_string(),
};
let provider = match adapter.read(
    &structfs_core_store::Path::from_components(vec![
        "gate".into(), "accounts".into(), bootstrap.clone(), "provider".into(),
    ]),
) {
    Ok(Some(r)) => match r.as_value() {
        Some(Value::String(s)) => s.clone(),
        _ => "anthropic".to_string(),
    },
    _ => "anthropic".to_string(),
};
let api_key_for_transport = match adapter.read(
    &structfs_core_store::Path::from_components(vec![
        "gate".into(), "accounts".into(), bootstrap.clone(), "key".into(),
    ]),
) {
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
```

Also remove the now-unused `_model` and `_max_tokens` reads (lines ~201-214) — those values are only needed by `synthesize_prompt`, which reads them from the thread's GateStore directly.

- [ ] **Step 4: Run all tests**

Run: `cargo test -p ox-cli`
Expected: All pass.

- [ ] **Step 5: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: 13/14 pass (svelte-check binary still missing).

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/thread_registry.rs crates/ox-cli/src/agents.rs crates/ox-cli/src/broker_setup.rs
git commit -m "feat(ox-cli): GateStore config handles in ThreadRegistry, clean up agent worker"
```

---

### Task 8: Format, quality gates, and status doc

**Files:**
- Modify: `docs/design/rfc/structfs-tui-status.md`

- [ ] **Step 1: Run formatter**

Run: `./scripts/fmt.sh`

- [ ] **Step 2: Run quality gates**

Run: `./scripts/quality_gates.sh`
Expected: 13/14 pass. Fix any clippy warnings.

- [ ] **Step 3: Update status doc**

Add after Phase 4a entry in `docs/design/rfc/structfs-tui-status.md`:

```markdown
#### Phase 4b: Config System Last Mile (complete)
- ModelProvider collapsed into GateStore — single source of truth for all LLM config
- GateStore: max_tokens on AccountConfig, convenience model/max_tokens paths, with_config()
- synthesize_prompt reads gate/model and gate/max_tokens (no model/ mount)
- figment integration: defaults → ~/.ox/config.toml → OX_* env vars → CLI flags
- TomlFileBacking persists runtime config to ~/.ox/config.toml (API keys excluded)
- ConfigStore persistence via StoreBacking
- ThreadRegistry wires ReadOnly config handles into GateStore
- Agent worker reads config through thread stores, not direct broker reads
- Config namespace: all paths under gate/ — no model/ section
- **Spec:** `docs/superpowers/specs/2026-04-08-config-completion-design.md`
- **Plan:** `docs/superpowers/plans/2026-04-08-config-completion-b.md`
```

Update "What's Next":

```markdown
## What's Next

### Remaining work:

Config system complete (Phases 4a + 4b). Remaining feature-level work:
completions-as-tools unification, runtime config UI (model/provider switcher
in TUI), per-thread config persistence on save, web platform IndexedDB backing.
```

- [ ] **Step 4: Commit**

```bash
git add docs/design/rfc/structfs-tui-status.md
git commit -m "docs: update status for Phase 4b Config System Last Mile"
```

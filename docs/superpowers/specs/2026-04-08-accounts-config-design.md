# Accounts-First Config Redesign

**Date:** 2026-04-08
**Status:** Draft
**Prereqs:** Phase 4c (ConfigStore S-tier refactor) complete

## Problem

The config system conflates auth identity with session preferences. `AccountConfig` bundles `{ provider, key, model, max_tokens }` — but model and max_tokens aren't properties of an account. You don't have a "Claude Sonnet account" and a "Claude Opus account." You have one Anthropic account and choose a model per completion call.

The system also treats model as a singleton — one "active model" per thread. But the target architecture has agents completing against multiple models in a single turn (Sonnet for fast routing, Opus for deep reasoning, GPT-4o for a second opinion). Config should express what's *available*, not what's *active*.

## Design

### Data Model

**Accounts are auth credentials.** Provider + key. Nothing else.

```rust
pub struct AccountConfig {
    pub provider: String,
    pub key: String,
}
```

**Defaults are session preferences.** Which account to use, what model, what token limit. Overridable per-thread, per-invocation, or per-completion-call.

### Config File

```toml
[gate.accounts.personal]
provider = "anthropic"
key = "sk-ant-personal-..."

[gate.accounts.work]
provider = "anthropic"
key = "sk-ant-work-..."

[gate.accounts.openai]
provider = "openai"
key = "sk-oai-..."

[gate.defaults]
account = "personal"
model = "claude-sonnet-4-20250514"
max_tokens = 4096
```

### Config Paths

```
gate/accounts/{name}/provider    — "anthropic" | "openai"
gate/accounts/{name}/key         — API key string
gate/defaults/account            — name of the default account
gate/defaults/model              — default model ID
gate/defaults/max_tokens         — default token limit (integer)
```

No `gate/model` at the top level. No `gate/api_key`. No `gate/provider`. Those were singleton concepts. The real data is accounts + defaults.

### Environment Variables

figment's `OX_` prefix with `__` separator maps directly to the path structure:

```
OX_GATE__ACCOUNTS__PERSONAL__PROVIDER=anthropic
OX_GATE__ACCOUNTS__PERSONAL__KEY=sk-ant-...
OX_GATE__ACCOUNTS__OPENAI__PROVIDER=openai
OX_GATE__ACCOUNTS__OPENAI__KEY=sk-oai-...
OX_GATE__DEFAULTS__ACCOUNT=personal
OX_GATE__DEFAULTS__MODEL=claude-sonnet-4-20250514
OX_GATE__DEFAULTS__MAX_TOKENS=4096
```

No `ANTHROPIC_API_KEY` legacy compat. No special-case env var mapping. The config shape IS the env var shape.

### CLI Flags

CLI selects and overrides. It does not define accounts.

```
ox --account work                          # select default account
ox --model claude-opus-4-20250514          # override default model
ox --max-tokens 8192                       # override default token limit
ox --account work --model gpt-4o           # combine
```

`--api-key` is removed. Account keys come from config file or env vars.

### figment Types

```rust
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

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
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
```

### CLI Overrides

```rust
#[derive(Debug, Default)]
pub struct CliOverrides {
    pub account: Option<String>,
    pub model: Option<String>,
    pub max_tokens: Option<i64>,
}
```

`apply_overrides` writes to `defaults.*` fields only. No account definition from CLI.

### to_flat_map

Produces flat keys matching the config path namespace:

```rust
fn to_flat_map(&self) -> BTreeMap<String, Value> {
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
    map.insert("gate/defaults/account".into(), Value::String(self.gate.defaults.account.clone()));
    map.insert("gate/defaults/model".into(), Value::String(self.gate.defaults.model.clone()));
    map.insert("gate/defaults/max_tokens".into(), Value::Integer(self.gate.defaults.max_tokens));
    map
}
```

### GateStore Changes

**AccountConfig shrinks:**
```rust
pub struct AccountConfig {
    pub provider: String,
    pub key: String,
}
```

`model` and `max_tokens` are removed from `AccountConfig`.

**GateStore gains a defaults sub-store:**

The `bootstrap` field is renamed to align with config: read from `gate/defaults/account`. The convenience paths change:

- `model` → reads from `gate/defaults/model` via config handle (was bootstrap account's model)
- `max_tokens` → reads from `gate/defaults/max_tokens` via config handle (was bootstrap account's max_tokens)
- `accounts/{name}/key` → reads from `gate/accounts/{name}/key` via config handle (not just bootstrap)

**config_string paths update:**
- `gate/api_key` → removed
- `gate/model` → `gate/defaults/model`
- `gate/max_tokens` → `gate/defaults/max_tokens`
- `gate/accounts/{name}/key` → reads per-account key from config

**Completion tools:** `create_completion_tools` generates one tool per account with a key (from config or local). Each tool accepts `model` and `max_tokens` as parameters, defaulting to `gate/defaults/model` and `gate/defaults/max_tokens`.

### synthesize_prompt Changes

Reads `gate/defaults/model` and `gate/defaults/max_tokens` instead of `gate/model` and `gate/max_tokens`.

### Snapshot Changes

GateStore snapshot saves account auth (provider only, keys excluded) and catalogs. It does NOT save defaults — those come from config. On restore, accounts are rebuilt from snapshot + config keys.

### Per-Thread Overrides

The Cascade primary (`LocalConfig`) holds per-thread default overrides:
- `gate/defaults/model` → thread prefers a different model
- `gate/defaults/account` → thread prefers a different account

These override defaults, not availability. All configured accounts remain accessible as completion tools regardless of thread defaults.

### Agent Worker Changes

The agent worker currently reads provider/api_key through the scoped adapter and creates a temporary GateStore for completion tools. With this change:

1. The thread's GateStore reads all account keys from config
2. Completion tools are generated for every account with a key
3. The `send` function is created per-account (each with its own provider config + key)
4. The main agentic loop uses `gate/defaults/model` for `synthesize_prompt`
5. The agent can call any completion tool to use a different account/model

### ox-web Changes

ox-web currently writes `gate/model` and `gate/max_tokens` to the namespace. Changes to:
- Write `gate/defaults/model` and `gate/defaults/max_tokens`
- Write `gate/accounts/{name}/key` per provider instead of a singleton key

### What This Removes

- `gate/model` top-level path (→ `gate/defaults/model`)
- `gate/max_tokens` top-level path (→ `gate/defaults/max_tokens`)
- `gate/api_key` path (→ per-account `gate/accounts/{name}/key`)
- `gate/provider` path (→ per-account `gate/accounts/{name}/provider`)
- `bootstrap` field on GateStore (→ `gate/defaults/account`)
- `model` and `max_tokens` fields on AccountConfig
- `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` legacy env var mapping
- `--api-key` CLI flag
- `--provider` CLI flag (provider is a property of the account, not a global)

### What This Adds

- `gate/defaults/*` config namespace
- Per-account key resolution from config (any account, not just bootstrap)
- `DefaultsConfig` figment type
- `AccountEntry` figment type
- `--account` CLI flag (replaces `--provider`)

## Execution Order

1. Shrink AccountConfig to `{ provider, key }` — move model/max_tokens to GateStore defaults
2. Add `defaults/` sub-paths to GateStore (account, model, max_tokens)
3. Update figment types: GateConfig with accounts HashMap + DefaultsConfig
4. Update CLI: replace `--provider`/`--api-key` with `--account`
5. Update config paths everywhere: `gate/model` → `gate/defaults/model`, etc.
6. Update synthesize_prompt, ox-web, agent worker
7. Remove legacy env var mapping
8. Update completion tools to accept model/max_tokens parameters
9. Quality gates + status doc

## Testing

- figment: TOML with multiple accounts + defaults resolves correctly
- figment: env vars for multiple accounts resolve correctly
- GateStore: reads account keys from config handle per-account (not just bootstrap)
- GateStore: reads defaults from config handle
- GateStore: completion_tool_schemas includes all accounts with keys
- ConfigStore: flat map with account paths round-trips through TOML backing
- Integration: change default model via broker → thread's GateStore reads new default
- Integration: multiple accounts configured → multiple completion tools registered

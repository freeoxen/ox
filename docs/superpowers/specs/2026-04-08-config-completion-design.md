# Config System Completion

**Date:** 2026-04-08
**Status:** Draft
**Prereqs:** Phase 3 (StoreBacking + ConfigStore) complete

## Overview

Complete the configuration system so that:
1. Stores read config through a path-based Reader handle (no special traits)
2. ConfigStore's namespace is hierarchical (model/id, gate/api_key, etc.)
3. ox-cli uses figment for composable startup config (files + env + flags)
4. ConfigStore persists global settings to ~/.ox/config.toml
5. Per-thread overrides persist to thread config files
6. Gate config (API keys, provider, endpoints) flows through ConfigStore
7. The Phase 3 ThreadRegistry redirect is reverted — stores own their reads

## Architecture

### Config Reader Handle

Stores receive a path-based Reader at construction time. They read
config from it. They don't know where values come from.

**For ox-cli (broker-backed):** The Reader is a `SyncClientAdapter`
scoped to `config/threads/{id}/`. Reads resolve through ConfigStore's
cascade.

**For ox-web (standalone):** The Reader is a `LocalConfig` — a simple
in-memory store implementing Reader. Set values at construction.

**For tests:** Same as ox-web — `LocalConfig` with test values.

No new trait. Just Reader. Stores already know this interface.

### ConfigStore Namespace (Hierarchical)

ConfigStore's paths become hierarchical to match store domains:

```
config/
├── model/
│   ├── id                    "claude-opus-4-20250514"
│   └── max_tokens            8192
├── gate/
│   ├── provider              "anthropic"
│   ├── api_key               "***" (masked on read)
│   ├── api_key_raw           "sk-..." (unmasked, for workers)
│   └── endpoints/
│       ├── anthropic/url     "https://api.anthropic.com"
│       └── openai/url        "https://api.openai.com/v1"
│
├── set                       write: {"path": "model/id", "value": "..."}
├── set_if_unset              write: {"path": "model/id", "value": "..."}
│
├── threads/{id}/
│   ├── model/id              resolved for thread (cascade)
│   ├── gate/provider         resolved for thread
│   ├── set                   write: per-thread override
│   └── ...
│
├── defaults                  read: all system defaults
└── effective                 read: all resolved global values
```

**Key change from Phase 3:** Config keys are paths (`model/id`,
`gate/api_key`) not flat strings (`"model"`, `"api_key"`). The
internal storage uses nested BTreeMaps or a flat map with path
keys.

**Write commands:** Instead of per-field commands (`set_model`,
`set_provider`), a single generic `set` command:

```
write(path!("config/set"), cmd!("path" => "model/id", "value" => "claude-opus-4-20250514"))
write(path!("config/threads/t_abc/set"), cmd!("path" => "model/id", "value" => "gpt-4o", "scope" => "saved"))
```

This is more extensible — adding a new config key doesn't require
a new command.

### figment Integration (ox-cli only)

ox-cli uses figment to compose startup config:

```rust
use figment::{Figment, providers::{Toml, Env, Serialized}};

let figment = Figment::from(Serialized::defaults(SystemDefaults::default()))
    .merge(Toml::file(inbox_root.join("config.toml")))
    .merge(Env::prefixed("OX_").split("_"))
    .merge(Serialized::globals(CliOverrides::from(&cli)));
```

Priority (highest wins): CLI flags > env vars > config file > defaults.

The resolved figment values are extracted into a `BTreeMap<String, Value>`
and passed to ConfigStore as the base layer. ConfigStore doesn't
depend on figment — it receives pre-resolved values.

**Config file:** `~/.ox/config.toml`:
```toml
[model]
id = "claude-opus-4-20250514"
max_tokens = 8192

[gate]
provider = "anthropic"
# api_key NOT stored in file — use OX_GATE_API_KEY env var
```

**Environment variables:**
- `OX_MODEL_ID` → `model/id`
- `OX_MODEL_MAX_TOKENS` → `model/max_tokens`
- `OX_GATE_PROVIDER` → `gate/provider`
- `OX_GATE_API_KEY` → `gate/api_key`

figment's `Env::prefixed("OX_").split("_")` handles the mapping
from `OX_MODEL_ID` to the nested `model.id` path automatically.

### ConfigStore Persistence

**Global settings:** When ConfigStore processes a `set` write
(not `set_if_unset`), it flushes the global layer to
`~/.ox/config.toml` via StoreBacking. API key is excluded from
persistence.

**Per-thread overrides:** Saved per-thread overrides persist to
`~/.ox/threads/{id}/config.json`. ConfigStore lazy-loads these
on first access to a thread's config, same as the current design.

ConfigStore accepts the inbox_root at construction so it knows
where thread directories live.

### Store Integration

Stores gain an optional config Reader:

```rust
impl ModelProvider {
    pub fn with_config(mut self, config: Box<dyn Reader + Send + Sync>) -> Self {
        self.config = Some(config);
        self
    }
}
```

Wait — `Reader` takes `&mut self`. For a shared config source that
may be read from multiple stores, we need interior mutability or
per-store handles. `SyncClientAdapter` already handles this (each
instance is an independent handle to the broker, `&mut` is for the
trait signature not actual mutation).

Each store gets its own `SyncClientAdapter` (ox-cli) or `LocalConfig`
(ox-web) instance. No sharing, no mutability issues.

When reading config-driven values, the store reads from its config
handle:

```rust
impl Reader for ModelProvider {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        match key {
            "id" => {
                if let Some(ref mut config) = self.config {
                    if let Ok(Some(record)) = config.read(&path!("model/id")) {
                        return Ok(Some(record));
                    }
                }
                // Fallback to local field (standalone mode)
                Ok(Some(Record::parsed(Value::String(self.model.clone()))))
            }
            // ...
        }
    }
}
```

Stores keep their local fields as defaults for standalone operation.
Config handle reads take priority when present.

### Revert Phase 3 ThreadRegistry Redirect

Remove from ThreadRegistry:
- `is_config_path()` method
- `resolve_config_read()` method
- Config path routing in AsyncReader/AsyncWriter

Reads to `threads/{id}/model/id` go back to the local ModelProvider.
ModelProvider reads from its config handle (which reads from
ConfigStore via broker). No redirect needed — the config handle
does the resolution.

The broker_client on ThreadRegistry is still useful for the initial
config setup when mounting threads — ThreadRegistry can construct
`SyncClientAdapter` handles scoped to `config/threads/{id}/` and
pass them to stores via `with_config()`.

### GateStore Changes

GateStore keeps its full API — providers, accounts, catalogs, tools.
It gains a config handle like ModelProvider. Config-driven fields
(api_key, provider, model) read from the config handle when present,
fall back to local fields when standalone.

GateStore's snapshot continues to save its full state (for
standalone/ox-web). In ox-cli, the config-driven fields are
redundant with ConfigStore but that's fine — GateStore doesn't
know ConfigStore exists.

### ThreadRegistry Mount Changes

When ThreadRegistry mounts a thread, it:
1. Creates a `SyncClientAdapter` scoped to `config/threads/{id}/`
2. Passes it to ModelProvider via `with_config()`
3. Passes it to GateStore via `with_config()`
4. Stores read config on demand through their handle

For standalone ThreadNamespace (no broker), stores get no config
handle and use their local fields.

## What Changes from Phase 3

| Phase 3 | This Phase |
|---------|-----------|
| Flat config keys (`"model"`, `"api_key"`) | Path-based (`model/id`, `gate/api_key`) |
| Per-field write commands (`set_model`) | Generic `set` command with path field |
| ThreadRegistry redirects config reads | Reverted — stores read from config handle |
| Hand-rolled 4-layer cascade | figment for startup, cascade for runtime |
| No file persistence | TOML file + StoreBacking |
| ConfigStore is ox-ui only | ConfigStore ox-ui + figment integration in ox-cli |
| `CONFIG_KEYS` array | Extensible path namespace |

## Execution Order

1. Refactor ConfigStore to path-based namespace + generic set command
2. Add figment integration in ox-cli (config loading)
3. Add ConfigStore persistence (global TOML, per-thread JSON)
4. Add config Reader handle to ModelProvider
5. Add config Reader handle to GateStore
6. Revert ThreadRegistry redirect, wire config handles at mount
7. Clean up: remove redundant agent config writes
8. Quality gates

## Testing

- ConfigStore: update existing tests for path-based keys + generic set
- figment: integration test for file + env + flag composition
- Config handle: ModelProvider reads from config handle in tests
- ThreadRegistry: mount with config handles, verify store reads
- GateStore: config handle reads for api_key/provider
- End-to-end: change config → worker reads new value

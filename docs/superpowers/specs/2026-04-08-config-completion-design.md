# Config System Completion

**Date:** 2026-04-08
**Status:** Draft
**Prereqs:** Phase 3 (StoreBacking + ConfigStore) complete

## Overview

Complete the configuration system so that:
1. Stores read config through a path-based Reader handle (no special traits)
2. ConfigStore's namespace is hierarchical (model/id, gate/api_key, etc.)
3. Reads and writes use the same paths — no RPC-style "set" commands
4. ox-cli uses figment for composable startup config (files + env + flags)
5. ConfigStore persists runtime changes to ~/.ox/config.toml
6. Per-thread overrides persist to thread config files
7. Gate config (API keys, provider, endpoints) flows through ConfigStore
8. The Phase 3 ThreadRegistry redirect is reverted — stores own their reads

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

### Capability Wrappers

Wrapper stores that restrict capabilities on an inner store. Live
in ox-kernel alongside Reader/Writer/Store — platform-agnostic,
composable.

**`ReadOnly<S: Store>`** — implements Reader (delegates), rejects
all writes with StoreError. Use: give ModelProvider and GateStore
read-only config handles so they can't accidentally write to
ConfigStore.

**`Masked<S: Reader>`** — implements Reader, applies path-based
masking on reads. Returns a mask value (e.g. `"***"`) for masked
paths, passes through for others. Use: mask `gate/api_key` when
exposing config to the TUI/ViewState.

These are generic — they work with any Reader/Store, not just config.

```rust
pub struct ReadOnly<S> {
    inner: S,
}

impl<S: Reader> Reader for ReadOnly<S> {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        self.inner.read(from)
    }
}

impl<S> Writer for ReadOnly<S> {
    fn write(&mut self, _to: &Path, _data: Record) -> Result<Path, StoreError> {
        Err(StoreError::store("ReadOnly", "write", "read-only capability"))
    }
}

pub struct Masked<S> {
    inner: S,
    masked_paths: Vec<Path>,
    mask_value: Value,
}

impl<S: Reader> Reader for Masked<S> {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        if self.masked_paths.iter().any(|p| from.starts_with(p)) {
            return Ok(Some(Record::parsed(self.mask_value.clone())));
        }
        self.inner.read(from)
    }
}
```

**Capability assignment:**

| Consumer | Config handle | Why |
|----------|--------------|-----|
| ModelProvider | `ReadOnly<SyncClientAdapter>` | Reads model config, can't write back |
| GateStore | `ReadOnly<SyncClientAdapter>` | Reads api_key/provider, can't write back |
| ViewState (TUI display) | `Masked<SyncClientAdapter>` | api_key shows as "***" |
| Event loop | Direct broker client write | Full config write access |
| ox-web stores | `LocalConfig` (ReadWrite) | Standalone, no restrictions needed |

### ConfigStore Namespace (Path-Based Read/Write)

Reads and writes use the same paths. No special command paths.

```
config/
├── model/
│   ├── id                    read/write: model identifier
│   └── max_tokens            read/write: integer
├── gate/
│   ├── provider              read/write: "anthropic" | "openai"
│   ├── api_key               read: "***" (masked)
│   ├── api_key_raw           read: unmasked value (for workers)
│   └── endpoints/
│       ├── anthropic/url     read/write: endpoint URL
│       └── openai/url        read/write: endpoint URL
│
├── threads/{id}/
│   ├── model/id              read: resolved for thread (cascade)
│   ├── model/max_tokens      read: resolved for thread
│   ├── gate/provider         read: resolved for thread
│   └── gate/api_key          read: resolved for thread (masked)
│
└── (root read)               read: all resolved global values as map
```

**Writes go to data paths:**

```rust
// Set global model
write(path!("config/model/id"), Record::parsed(Value::String("gpt-4o".into())))

// Set per-thread model override
write(path!("config/threads/t_abc/model/id"), Record::parsed(Value::String("claude-opus-4-20250514".into())))
```

No `set` or `set_if_unset` commands. The path determines the scope
(global vs per-thread). The value is the data.

**Per-thread write scope:** Writes to `config/threads/{id}/...`
default to ephemeral (session-only). To persist, the write data
includes a scope marker:

```rust
// Ephemeral (default) — lost on restart
write(path!("config/threads/t_abc/model/id"), Record::parsed(Value::String("gpt-4o".into())))

// Saved — persists to thread config.json
let mut map = BTreeMap::new();
map.insert("value".to_string(), Value::String("gpt-4o".into()));
map.insert("persist".to_string(), Value::Bool(true));
write(path!("config/threads/t_abc/model/id"), Record::parsed(Value::Map(map)))
```

Or simpler: writes to `config/threads/{id}/...` are always ephemeral.
A separate `config/threads/{id}/save` write flushes all ephemeral
overrides for that thread to disk. This keeps individual writes
simple (just the value) and persistence explicit.

**Recommendation:** Individual writes are just the value. Persistence
is explicit via a separate save operation, matching how thread state
snapshots already work.

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

The resolved figment values are extracted into a nested structure
and converted to StructFS Values, then written into ConfigStore's
global layer. ConfigStore doesn't depend on figment — it receives
values through normal writes.

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

### ConfigStore Internal Storage

ConfigStore stores values in a nested path structure. Internally
this can be a flat `BTreeMap<String, Value>` keyed by path strings
(`"model/id"`, `"gate/provider"`), or a nested `Value::Map`. Flat
is simpler for lookup, nested is better for serialization.

**Recommendation:** Flat map with path-string keys. Conversion to/from
nested TOML/JSON happens at the persistence boundary.

Three layers:
1. **Base** — figment-resolved startup values (immutable after init)
2. **Runtime global** — runtime changes (persisted on explicit save)
3. **Per-thread** — `BTreeMap<String, BTreeMap<String, Value>>` keyed
   by thread_id, then config path

Resolution order: per-thread → runtime global → base.

The Phase 3 "system defaults" layer merges into "base" (figment's
defaults provider IS the system defaults).

### ConfigStore Persistence

**Global settings:** Runtime global changes are flushed to
`~/.ox/config.toml` via StoreBacking when an explicit save is
triggered (or on graceful shutdown). Not on every write — config
changes are frequent during a session, disk writes should be batched.

API key is excluded from persistence (security). A filter at the
persistence boundary strips `gate/api_key` before saving.

**Per-thread overrides:** Flushed to `~/.ox/threads/{id}/config.json`
when the thread save operation triggers (same lifecycle as thread
snapshots). ConfigStore accepts inbox_root at construction for
thread directory paths.

### Store Integration

Stores gain an optional config Reader handle:

```rust
impl ModelProvider {
    pub fn with_config(mut self, config: Box<dyn Store>) -> Self {
        self.config = Some(config);
        self
    }
}
```

Each store gets its own handle instance (SyncClientAdapter for
ox-cli, LocalConfig for ox-web). No sharing, no mutability issues.

When reading config-driven values:

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
                // Standalone fallback
                Ok(Some(Record::parsed(Value::String(self.model.clone()))))
            }
            // ...
        }
    }
}
```

Stores keep local fields as standalone defaults. Config handle
reads take priority when present.

### Revert Phase 3 ThreadRegistry Redirect

Remove from ThreadRegistry:
- `is_config_path()` method
- `resolve_config_read()` method
- Config path routing in AsyncReader/AsyncWriter

Reads to `threads/{id}/model/id` go back to local ModelProvider.
ModelProvider reads from its config handle (which reads from
ConfigStore through the broker). No redirect needed.

ThreadRegistry's broker_client remains — used to construct
SyncClientAdapter handles for stores at mount time.

### GateStore Changes

GateStore keeps its full API (providers, accounts, catalogs, tools).
It gains a config handle like ModelProvider. Config-driven fields
(api_key, provider, model) read from the config handle when present,
fall back to local fields when standalone.

GateStore's snapshot continues to save its full state for
standalone/ox-web compatibility.

### ThreadRegistry Mount Changes

When ThreadRegistry mounts a thread, it:
1. Creates SyncClientAdapter scoped to `config/threads/{id}/`
   using its broker_client
2. Passes it to ModelProvider via `with_config()`
3. Passes it to GateStore via `with_config()`
4. Stores read config on demand through their handle

For standalone ThreadNamespace (no broker), stores use local fields.

### LocalConfig (ox-kernel)

Simple in-memory Reader/Writer for standalone and test use:

```rust
pub struct LocalConfig {
    values: BTreeMap<String, Value>,
}

impl LocalConfig {
    pub fn new() -> Self { ... }
    pub fn set(&mut self, path: &Path, value: Value) { ... }
}

impl Reader for LocalConfig { ... }
impl Writer for LocalConfig { ... }
```

Lives in ox-kernel alongside StoreBacking. Platform-agnostic.

## What Changes from Phase 3

| Phase 3 | This Phase |
|---------|-----------|
| Flat config keys (`"model"`) | Path-based (`model/id`, `gate/api_key`) |
| `set_model`, `set_if_unset` commands | Direct writes to data paths |
| ThreadRegistry redirects config reads | Reverted — stores read from config handle |
| Hand-rolled 4-layer cascade | figment base + runtime + per-thread |
| No file persistence | TOML file via StoreBacking |
| `CONFIG_KEYS` array | Open path namespace |
| ConfigStore is sole reader | Stores read from config handle |

## Execution Order

1. Capability wrappers in ox-kernel (ReadOnly, Masked)
2. LocalConfig in ox-kernel (standalone Reader/Writer for config)
3. Refactor ConfigStore to path-based namespace (replace flat keys)
4. Add figment integration in ox-cli (config loading)
5. Add ConfigStore persistence (global TOML, per-thread JSON)
6. Add config handle to ModelProvider (ReadOnly, with_config builder)
7. Add config handle to GateStore (ReadOnly, with_config builder)
8. Revert ThreadRegistry redirect, wire config handles at mount
9. Update ViewState to use Masked config for display
10. Clean up: remove redundant agent config writes
11. Quality gates + status update

## Testing

- ReadOnly: write rejected, read passes through
- Masked: masked paths return mask value, others pass through
- LocalConfig: unit tests for path-based read/write
- ConfigStore: update existing tests for path-based writes + cascade
- figment: integration test for file + env + flag composition
- ModelProvider: reads from config handle, falls back to local
- GateStore: config handle reads for api_key/provider
- ThreadRegistry: mount with config handles, verify store reads
- End-to-end: change config via broker → worker reads new value

# StoreBacking + ConfigStore

**Date:** 2026-04-08
**Status:** Draft
**Prereqs:** Phase 1 (App Convergence) + Phase 2 (S-Tier Polish) complete

## Overview

Two foundational systems that complete the StructFS TUI architecture:

1. **StoreBacking** — persistence abstraction so stores can load/save
   state through a platform-agnostic interface. CLI uses file-backed
   implementations. Web can add IndexedDB later.

2. **ConfigStore** — single authority for configuration resolution
   across all scopes. Owns global defaults, global user settings,
   per-thread saved overrides, and per-thread ephemeral overrides.
   Resolves reads by cascading through layers (highest priority wins).

These are interconnected: ConfigStore uses StoreBacking for persisting
global settings and per-thread overrides. ThreadRegistry delegates
config-path reads to ConfigStore instead of routing to per-thread
ModelProvider/GateStore.

## StoreBacking Trait

### Location

`ox-kernel` — alongside Reader/Writer/Store traits.

### Interface

```rust
pub trait StoreBacking: Send + Sync {
    /// Load the full persisted state.
    fn load(&self) -> Result<Option<Value>, StoreError>;

    /// Persist the full state (atomic overwrite).
    fn save(&self, value: &Value) -> Result<(), StoreError>;
}
```

Two operations: load and save. Returns `Option<Value>` from load
(None = no persisted state, first run). Save is atomic (write-to-temp
+ rename on file systems).

No `append` — the ledger (HistoryProvider) keeps its own JSONL I/O
in ox-inbox. StoreBacking is for stores that snapshot their full state.

### CLI Implementation

`JsonFileBacking` in `ox-inbox` (where file I/O already lives):

```rust
pub struct JsonFileBacking {
    path: PathBuf,
}

impl StoreBacking for JsonFileBacking {
    fn load(&self) -> Result<Option<Value>, StoreError> {
        // Read file, parse via json_to_value
        // Return None if file doesn't exist
    }
    fn save(&self, value: &Value) -> Result<(), StoreError> {
        // Serialize via value_to_json
        // Write to temp file, rename atomically
    }
}
```

### Store Integration

Stores that want persistence accept an optional backing:

```rust
impl ModelProvider {
    pub fn new(model: &str, max_tokens: u32) -> Self { ... }

    pub fn with_backing(mut self, backing: Box<dyn StoreBacking>) -> Self {
        if let Ok(Some(state)) = backing.load() {
            self.restore_from(&state);
        }
        self.backing = Some(backing);
        self
    }
}
```

On writes that change state, the store calls `self.backing.save()`
with its current snapshot. Stores without a backing stay purely
in-memory (UiStore, InputStore, ApprovalStore).

### Affected Stores

- **ModelProvider** (ox-context): model + max_tokens
- **SystemProvider** (ox-context): system prompt
- **GateStore** (ox-gate): API keys + provider config
- **ToolsProvider** (ox-context): tool schemas
- **ConfigStore** (new): global settings + per-thread overrides

HistoryProvider is NOT affected — it uses its own ledger.jsonl
mechanism via ox-inbox.

## ConfigStore

### Location

`ox-ui` — alongside UiStore, InputStore (broker-mounted stores).

### Architecture

ConfigStore is the **single authority** for all configuration at
all scopes. It owns four layers, resolved in priority order:

```
Priority (highest wins):
1. Ephemeral per-thread  — session-only, lost on restart
2. Saved per-thread      — persisted via StoreBacking
3. Global user setting   — persisted via StoreBacking
4. System default        — hardcoded fallback
```

Every config key resolves through this cascade. A read for thread
`t_abc`'s model:
1. Check ephemeral overrides for t_abc → not set
2. Check saved overrides for t_abc → not set
3. Check global user setting → "claude-opus-4-20250514"
4. Return "claude-opus-4-20250514"

If the user sets a per-thread override for t_abc:
1. Check ephemeral overrides for t_abc → "gpt-4o"
2. Return "gpt-4o"

### Internal State

```rust
pub struct ConfigStore {
    /// Layer 4: hardcoded system defaults.
    defaults: BTreeMap<String, Value>,

    /// Layer 3: global user settings (persisted).
    global: BTreeMap<String, Value>,

    /// Layer 2: per-thread saved overrides (persisted).
    /// Key: thread_id, Value: map of overrides for that thread.
    thread_saved: BTreeMap<String, BTreeMap<String, Value>>,

    /// Layer 1: per-thread ephemeral overrides (session-only).
    thread_ephemeral: BTreeMap<String, BTreeMap<String, Value>>,

    /// Persistence for global + thread_saved layers.
    backing: Option<Box<dyn StoreBacking>>,

    txn_log: TxnLog,
}
```

### Namespace

```
config/
├── model              read: resolved global model
├── provider           read: resolved global provider
├── max_tokens         read: resolved global max_tokens
├── api_key            read: "***" (masked), write: stores key
├── set_model          write: {"value": "..."}
├── set_provider       write: {"value": "..."}
├── set_max_tokens     write: {"value": N}
├── set_api_key        write: {"value": "sk-..."}
│
├── threads/{id}/      per-thread config resolution
│   ├── model          read: resolved for this thread (cascade)
│   ├── provider       read: resolved for this thread
│   ├── max_tokens     read: resolved for this thread
│   ├── set_model      write: {"value": "...", "scope": "saved|ephemeral"}
│   ├── set_provider   write: {"value": "...", "scope": "saved|ephemeral"}
│   └── set_max_tokens write: {"value": N, "scope": "saved|ephemeral"}
│
├── defaults           read: map of all system defaults
└── effective          read: map of resolved global values
```

Reads at `config/model` resolve through layers 3→4 (global user
setting, falling back to system default).

Reads at `config/threads/{id}/model` resolve through layers
1→2→3→4 (ephemeral thread → saved thread → global → default).

Writes at `config/set_model` update layer 3 (global user setting).

Writes at `config/threads/{id}/set_model` update layer 1 or 2
depending on the `scope` field ("ephemeral" or "saved", default
"ephemeral").

### ThreadRegistry Integration

ThreadRegistry currently routes `threads/{id}/model/...` to the
per-thread ModelProvider. This changes:

**Config paths route to ConfigStore.** When ThreadRegistry receives
a read for `threads/{id}/model/id`, it recognizes this as a config
path and reads from `config/threads/{id}/model` through its broker
client instead of routing to the local ModelProvider.

Which paths are config paths? The set is explicit:
- `model/id`, `model/max_tokens` → config resolution
- `gate/accounts/...` → config resolution (API keys)

Non-config paths route to local stores as before:
- `history/messages` → HistoryProvider
- `tools/schemas` → ToolsProvider
- `system/prompt` → SystemProvider (could become config later)

This means **ModelProvider goes away from ThreadNamespace** for
config-resolved fields. ThreadRegistry routes model reads to
ConfigStore. Writes to `threads/{id}/model/id` route to
ConfigStore's per-thread layer.

GateStore config (API keys, provider) similarly routes to ConfigStore.
GateStore retains non-config state (transport state, model catalog).

### Prompt Synthesis

`synthesize_prompt()` in ox-context reads `model/id` and
`model/max_tokens` from its reader. Currently this reader is the
ThreadNamespace. After the change, these reads route through
ThreadRegistry to ConfigStore, returning the resolved effective
value for that thread. synthesize_prompt() doesn't change — the
Reader interface is the same, only the routing behind it changes.

### Worker Spawn

Currently `spawn_worker` in agents.rs writes model/api_key config
to the thread's stores via SyncClientAdapter. After the change:

- Model config writes go to `config/threads/{id}/set_model` etc.
  through the adapter (which is scoped to `threads/{id}/`, so the
  write path is `model/id` → broker resolves to
  `threads/{id}/model/id` → ThreadRegistry recognizes config path →
  routes to ConfigStore per-thread layer).

Actually, simpler: **workers don't write config at spawn.** They
inherit from ConfigStore's resolution. The global model/provider/
api_key are already in ConfigStore. Per-thread overrides are set by
the TUI, not the worker. Workers just read `model/id` during prompt
synthesis and get the resolved value.

The explicit writes in agents.rs lines 229-241 and 244-265 are
deleted. Workers inherit config automatically.

### App Field Removal

`App.model` and `App.provider` move to ConfigStore.

- ViewState reads `config/model` and `config/provider` for display.
- AgentPool::new() no longer takes model/provider/api_key/max_tokens.
  Workers read from ConfigStore through the broker.

App drops to **4 fields**: pool, input_history, history_cursor,
input_draft.

### Initialization

`broker_setup::setup()` gains config parameters:

```rust
pub async fn setup(
    inbox: InboxStore,
    bindings: Vec<Binding>,
    inbox_root: PathBuf,
    provider: String,
    model: String,
    max_tokens: u32,
    api_key: String,
) -> BrokerHandle {
    // ... existing mounts ...

    // ConfigStore with defaults + initial global values
    let config_backing = JsonFileBacking::new(inbox_root.join("config.json"));
    let config = ConfigStore::new(defaults, config_backing);
    // Set initial globals from CLI args (if not already persisted)
    config.set_global_if_unset("provider", provider);
    config.set_global_if_unset("model", model);
    config.set_global("api_key", api_key); // always from CLI/env
    config.set_global_if_unset("max_tokens", max_tokens);

    broker.mount(path!("config"), config).await;
}
```

`set_global_if_unset` respects persisted values — if the user
previously changed the model via runtime config and it was saved,
the CLI default doesn't overwrite it. API key always comes from
CLI/env (not persisted for security).

### Persistence Layout

```
~/.ox/
├── config.json           global settings (layer 3)
├── threads/
│   ├── {id}/
│   │   ├── config.json   per-thread saved overrides (layer 2)
│   │   ├── context.json  thread state snapshot
│   │   ├── ledger.jsonl  message history
│   │   └── view.json     view state
```

### Snapshot Coordinator Changes

The snapshot coordinator currently saves/restores "model" as a
participating mount. After ConfigStore owns model config:

- Remove "model" from PARTICIPATING_MOUNTS
- ConfigStore handles its own persistence via StoreBacking
- Per-thread config.json is saved/loaded by ConfigStore, not the
  snapshot coordinator
- "system" and "gate" stay in PARTICIPATING_MOUNTS (system prompt
  is not config-resolved yet; gate has non-config state)

Actually, "gate" config (API keys, provider) should also move to
ConfigStore. Gate retains only transport-level state (model catalog,
usage tracking). Remove "gate" from PARTICIPATING_MOUNTS for the
config fields; GateStore keeps its own snapshot for non-config state.

## Execution Order

1. **StoreBacking trait** in ox-kernel + JsonFileBacking in ox-inbox
2. **Wire StoreBacking** into ModelProvider, SystemProvider (builder pattern)
3. **ConfigStore** in ox-ui with layered resolution + tests
4. **Mount ConfigStore** in broker_setup with initialization
5. **ThreadRegistry config routing** — model/gate config paths route
   to ConfigStore through broker client
6. **Remove worker config writes** from agents.rs
7. **Remove App.model/App.provider** — read from ConfigStore
8. **Update snapshot coordinator** — remove "model" from participating mounts
9. **Quality gates**

## Testing

- StoreBacking: unit tests for JsonFileBacking (load/save/atomic write)
- ConfigStore: unit tests for cascade resolution (all 4 layers),
  per-thread scoping, read/write paths, set_global_if_unset
- Integration: broker test verifying config resolution through
  ThreadRegistry routing
- All existing tests must continue to pass

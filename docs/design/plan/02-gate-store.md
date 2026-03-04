# Phase 2: Gate as Store + Provider/Account Registry

## Goal

GateStore implements Reader/Writer for provider configs and account state.
API keys migrate from OxAgent HashMap to `gate/accounts/*/key`.

## Checklist

### Create provider/account types
- [ ] `crates/ox-gate/src/provider.rs` — `ProviderConfig { dialect, endpoint, version }`, defaults for anthropic/openai
- [ ] `crates/ox-gate/src/account.rs` — `AccountConfig { provider, key, model }`

### GateStore implementation
- [ ] `crates/ox-gate/Cargo.toml` — add structfs-core-store, structfs-serde-store deps
- [ ] `crates/ox-gate/src/lib.rs` — `GateStore` struct with Reader/Writer impl
- [ ] Read paths: `providers/*`, `accounts/*`, `bootstrap`
- [ ] Write paths: `accounts/*/key`, `accounts/*/model`, `bootstrap`

### ModelInfo migration
- [ ] `crates/ox-kernel/src/lib.rs` — move `ModelInfo` struct here from ox-context
- [ ] `crates/ox-context/src/lib.rs` — import `ModelInfo` from ox-kernel
- [ ] `crates/ox-context/src/lib.rs` — strip `provider` and `catalog` fields from ModelProvider

### ox-web integration
- [ ] Remove `api_keys: HashMap` from OxAgent
- [ ] Mount GateStore at `"gate"` in constructor
- [ ] Rewrite `set_api_key`/`remove_api_key`/`has_api_key` to read/write gate paths
- [ ] Rewrite `set_provider`/`get_provider`
- [ ] Update `run_agentic_loop` API key lookup
- [ ] Update `refresh_models` to write catalogs to gate

## Test Plan

- GateStore read/write roundtrips for providers, accounts, bootstrap, catalogs
- Default provider configs for anthropic and openai
- Account key set/get/remove

## Verification Gates

```bash
cargo check --workspace
cargo check --target wasm32-unknown-unknown -p ox-web
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

## Scratch Space

(implementation notes go here)

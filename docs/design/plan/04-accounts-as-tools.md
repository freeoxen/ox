# Phase 4: Accounts as Tools + Multi-Model Routing

## Goal

Gate generates CompletionTool per account. Shared `complete_via_gate` function
for bootstrap and delegation. Delegation native-only (wasm returns error).

## Checklist

### Completion tools
- [ ] `crates/ox-gate/src/tools.rs` — `CompletionTool` struct (implements `Tool`)
- [ ] `crates/ox-gate/src/tools.rs` — `complete_via_gate` shared function

### Gate integration
- [ ] `crates/ox-gate/src/lib.rs` — `tools/schemas` readable path
- [ ] `crates/ox-gate/src/lib.rs` — `generate_tool_schemas` / `create_completion_tools` methods

### ox-core integration
- [ ] `crates/ox-core/src/lib.rs` — read gate tool schemas, register completion tools in ToolRegistry

### ox-web integration
- [ ] `crates/ox-web/src/lib.rs` — rebuild tools provider with gate completion tools (wasm_mode: true)

## Test Plan

- Schema generation produces correct tool schemas per account
- CompletionTool execute (native mock + wasm error)
- `complete_via_gate` with mock store
- Tool list composition (gate tools + shell tools)

## Verification Gates

```bash
cargo check --workspace
cargo check --target wasm32-unknown-unknown -p ox-web
cargo test --workspace
cargo clippy --workspace -- -D warnings
wasm-pack build crates/ox-web --target web --out-dir ../../target/wasm-pkg
./scripts/quality_gates.sh
```

## Scratch Space

(implementation notes go here)

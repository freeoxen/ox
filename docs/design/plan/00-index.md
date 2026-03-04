# ox-gate Implementation Plan

## Overview

ox-gate extracts transport dispatch, schema translation, and API key management
from ox-web into a StructFS-native block. See `docs/design/ox-gate.md` for the
full design.

## Phase Ordering

| Phase | File | Goal |
|-------|------|------|
| 1 | [01-extract-codecs.md](01-extract-codecs.md) | Move codec functions to new ox-gate crate (pure refactor) |
| 2 | [02-gate-store.md](02-gate-store.md) | GateStore with Reader/Writer for providers + accounts |
| 3 | [03-async-protocol.md](03-async-protocol.md) | Async protocol, HTTP store, kernel changes (biggest phase) |
| 4 | [04-accounts-as-tools.md](04-accounts-as-tools.md) | Accounts as tools + multi-model routing |

Each phase leaves all 14 quality gates passing.

## Cross-Cutting Concerns

- **StructFS Value conversion**: serialize_assistant_message / serialize_tool_results
  produce `serde_json::Value`; converted to StructFS `Value` via `json_to_value`
  at call sites. ox-gate will follow the same pattern.
- **Send+Sync**: ox-gate types must be `Send + Sync` for the kernel. ox-web wraps
  in `Rc<RefCell<>>` as it does today.
- **Edition 2024**: all new crates use `edition.workspace = true`.
- **No behavioral changes in Phase 1**: pure code motion, same tests pass.

## Verification (every phase)

```bash
cargo check --workspace
cargo check --target wasm32-unknown-unknown -p ox-web
cargo test --workspace
cargo clippy --workspace -- -D warnings
wasm-pack build crates/ox-web --target web --out-dir ../../target/wasm-pkg
cd crates/ox-web/ui && bun test
./scripts/quality_gates.sh
```

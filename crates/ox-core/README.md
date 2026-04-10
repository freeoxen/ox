# ox-core

Agent composition for the [ox](https://github.com/freeoxen/ox) framework — the main entry point for native consumers.

## What's in the box

- **`Agent`** — composes a `Kernel`, `Namespace`, and `ToolStore` into a single struct
- **Re-exports** — everything from `ox-kernel`, `ox-context`, `ox-gate`, `ox-history`, and `ox-tools` so you only need one dependency

## Quick start

```rust
use ox_core::Agent;
use ox_tools::ToolStore;

let tool_store = ToolStore::new();
let mut agent = Agent::new("You are helpful.".into(), tool_store);
```

## Architecture

`ox-core` doesn't include any concrete tool implementations — those are provided by the shell layer (`ox-web` for the browser, `ox-cli` for native, or your own harness). This keeps the core portable and dependency-light.

## License

Apache-2.0 — see [LICENSE](../../LICENSE).

# ox-context

Namespace store router and providers for the [ox](https://github.com/freeoxen/ox) agent framework.

## What's in the box

- **`Namespace`** — routes reads and writes to mounted `Store` implementations by path prefix, like a VFS
- **`SystemProvider`** — holds the system prompt string
- **`ToolsProvider`** — read-only snapshot of tool schemas

Model ID and `max_tokens` settings are now managed by `GateStore` (from `ox-gate`).

## How it works

Mount providers into a namespace:

```rust
use ox_context::{Namespace, SystemProvider, ToolsProvider};
use ox_gate::GateStore;

let mut ns = Namespace::new();
ns.mount("system", Box::new(SystemProvider::new("You are helpful.".into())));
ns.mount("tools", Box::new(ToolsProvider::new(vec![])));
ns.mount("gate", Box::new(GateStore::new()));
```

Reading `path!("prompt")` from the namespace synthesizes a complete `CompletionRequest` by collecting state from all mounted providers.

## License

Apache-2.0 — see [LICENSE](../../LICENSE).

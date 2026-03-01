# ox-context

Namespace store router and providers for the [ox](https://github.com/freeoxen/ox) agent framework.

## What's in the box

- **`Namespace`** — routes reads and writes to mounted `Store` implementations by path prefix, like a VFS
- **`SystemProvider`** — holds the system prompt string
- **`ToolsProvider`** — read-only snapshot of tool schemas
- **`ModelProvider`** — model ID and `max_tokens` settings

## How it works

Mount providers into a namespace:

```rust
use ox_context::{Namespace, SystemProvider, ModelProvider, ToolsProvider};

let mut ns = Namespace::new();
ns.mount("system", Box::new(SystemProvider::new("You are helpful.".into())));
ns.mount("model", Box::new(ModelProvider::new("claude-sonnet-4-20250514".into(), 4096)));
ns.mount("tools", Box::new(ToolsProvider::new(vec![])));
```

Reading `path!("prompt")` from the namespace synthesizes a complete `CompletionRequest` by collecting state from all mounted providers.

## License

Apache-2.0 — see [LICENSE](../../LICENSE).

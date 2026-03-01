# ox-core

Agent composition for the [ox](https://github.com/freeoxen/ox) framework — the main entry point for native consumers.

## What's in the box

- **`Agent<T>`** — composes a `Kernel`, `Namespace`, and `ToolRegistry` into a single struct with a `prompt()` method
- **Re-exports** — everything from `ox-kernel`, `ox-context`, and `ox-history` so you only need one dependency

## Quick start

```rust
use ox_core::{Agent, Transport, ToolRegistry};

// Implement Transport for your LLM backend
let transport = MyTransport::new();
let tools = ToolRegistry::new();

let mut agent = Agent::new(
    "You are helpful.".into(),
    "claude-sonnet-4-20250514".into(),
    4096,
    transport,
    tools,
);

agent.subscribe(Box::new(|event| {
    println!("{event:?}");
}));

let reply = agent.prompt("What is 2 + 2?")?;
```

## Architecture

`ox-core` doesn't include any concrete `Transport` or `Tool` implementations — those are provided by the shell layer (`ox-web` for the browser, or your own native harness). This keeps the core portable and dependency-light.

## License

Apache-2.0 — see [LICENSE](../../LICENSE).

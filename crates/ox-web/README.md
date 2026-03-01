# ox-web

Browser Wasm shell for the [ox](https://github.com/freeoxen/ox) agent framework.

## What's in the box

- **`OxAgent`** — Wasm-bindgen export that runs the full agentic loop in the browser
- **JS tool registration** — register tools from JavaScript at runtime
- **SSE parsing** — handles Anthropic Messages API streaming responses
- **Event callbacks** — subscribe to agent lifecycle events from JS

## Build

```bash
wasm-pack build crates/ox-web --target web --out-dir ../../target/wasm-pkg
```

## Usage from JavaScript

```js
import init, { create_agent } from "./ox_web.js";

await init();

const agent = create_agent("You are helpful.", apiKey);

agent.on_event((event) => {
    console.log(event.type, event.data);
});

agent.register_tool(
    "greet",
    "Say hello to someone",
    '{"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}',
    (inputJson) => {
        const { name } = JSON.parse(inputJson);
        return `Hello, ${name}!`;
    }
);

const reply = await agent.prompt("Greet Alice");
```

## License

Apache-2.0 — see [LICENSE](../../LICENSE).

# ox

The ox is the oldest working animal. It does not sprint. It lowers its head, leans into the yoke, and pulls.

**ox** is an agentic AI framework where every capability is a filesystem path. Built on [StructFS](https://github.com/StructFS/structfs), the agent's system prompt, conversation history, tool definitions, and model configuration all live as paths in a virtual namespace. Read a path to inspect state. Write a path to change it.

## Architecture

| Crate | Role | Description |
|-------|------|-------------|
| `ox-kernel` | core | State machine, Tool/Transport traits, ToolRegistry, agentic loop, StructFS re-exports |
| `ox-context` | namespace | Namespace store router, SystemProvider, ToolsProvider, ModelProvider, prompt synthesis |
| `ox-history` | memory | HistoryProvider &mdash; conversation state as a StructFS store |
| `ox-core` | agent | Agent composition, wires kernel + namespace + stores, re-exports all public types |
| `ox-web` | browser | Wasm shell with JS tool registration, theme picker, debug UI |
| `ox-dev-server` | proxy | Axum binary, Anthropic API proxy, serves the Wasm playground |
| `ox-wasi` | stub | WASI target shell, re-exports ox-core |
| `ox-emscripten` | stub | Emscripten target shell, re-exports ox-core |

## Quick Start

```bash
# Prerequisites: Rust (edition 2024), wasm-pack, bun

# Build the Wasm package
wasm-pack build crates/ox-web --target web --out-dir ../../target/wasm-pkg

# Start the dev server
ANTHROPIC_API_KEY=sk-... cargo run -p ox-dev-server

# Open http://localhost:3000
```

## Design System

Seven colors. Twelve themes. Three typefaces. See the [brand book](https://freeoxen.github.io/ox/brand.html) for the full specification.

## Project Structure

```
ox/
├── crates/
│   ├── ox-kernel/       # Core types and agentic loop
│   ├── ox-context/      # Namespace and providers
│   ├── ox-history/      # Conversation history
│   ├── ox-core/         # Agent composition
│   ├── ox-web/          # Browser Wasm shell
│   ├── ox-dev-server/   # Anthropic API proxy
│   ├── ox-wasi/         # WASI stub
│   └── ox-emscripten/   # Emscripten stub
├── site/                # Static landing site (Cloudflare Pages)
├── scripts/             # Quality gates, coverage
├── BRAND_BOOK.md        # Design system specification
└── README.md
```

## Development

```bash
# Full quality gates (fmt, clippy, check, test, wasm-pack)
./scripts/quality_gates.sh

# Coverage (Rust + TypeScript)
./scripts/coverage.sh

# Workspace check
cargo check

# Wasm target check
cargo check --target wasm32-unknown-unknown -p ox-web

# TypeScript tests
cd crates/ox-web/ui && bun test

# Build the site
cd site && bun install && bun run build
```

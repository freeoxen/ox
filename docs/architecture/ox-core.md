# ox-core: Composition & Module Boundary

## Overview

ox-core wires everything together into a usable agent. It composes ox-kernel,
ox-context (with its providers, including ox-history), and (optionally) ox-shell
into a single Agent that platform crates (ox-wasi, ox-web, ox-emscripten) export
as a Wasm module.

ox-core is **not** a platform crate itself — it contains no platform-specific
code. It is the last layer of portable logic before the platform boundary.

## Architecture

```
                              ox-core
                    ┌───────────────────────────┐
                    │                           │
                    │  ┌─────────────────────┐  │
                    │  │      Agent          │  │
                    │  │                     │  │
                    │  │  ┌───────────────┐  │  │
                    │  │  │  ox-kernel    │  │  │
                    │  │  │  (event-      │  │  │
                    │  │  │   driven      │  │  │
                    │  │  │   state       │  │  │
                    │  │  │   machine)    │  │  │
                    │  │  └───────┬───────┘  │  │
                    │  │      r/w │          │  │
                    │  │  ┌───────┴───────┐  │  │
                    │  │  │  ox-context   │  │  │
                    │  │  │  (provider    │  │  │
                    │  │  │   namespace)  │  │  │
                    │  │  │       │       │  │  │
                    │  │  │   providers:  │  │  │
                    │  │  │   history     │  │  │
                    │  │  │   system      │  │  │
                    │  │  │   tools       │  │  │
                    │  │  │   documents   │  │  │
                    │  │  │   session     │  │  │
                    │  │  └──────────────-┘  │  │
                    │  │          │          │  │
                    │  │          ▼          │  │
                    │  │      Transport      │  │
                    │  │    (ox-shell or     │  │
                    │  │     proxy)          │  │
                    │  └─────────────────────┘  │
                    │                           │
                    └───────────────────────────┘
                                │
                    ┌───────────┼───────────┐
                    │           │           │
                  ox-wasi    ox-web    ox-emscripten
                 (wasip1)   (wasm32)   (emscripten)
```

## The Agent

ox-core's primary export. This is the public API that platform crates and
embedders interact with.

### Configuration

The Agent is constructed from a configuration that specifies:

- **Shell mode** — Whether ox-shell is embedded (compiled in, called directly)
  or accessed as a sidecar (via channel-based proxy transport).
- **Providers** — Which ox-context providers to mount, and their configuration
  (e.g. history persistence backend, document sources).
- **Model** — The model descriptor to use for completions.

### Construction

On creation, the Agent:

1. Builds a transport — either a direct transport wrapping an embedded shell
   instance, or a proxy transport wrapping a channel pair.
2. Assembles the ox-context namespace with configured providers (history,
   system, tools, documents, session, and any custom providers).
3. Assembles a kernel connected to the context namespace and transport.

### Public API

The Agent exposes a focused API that delegates to the kernel while managing the
surrounding infrastructure:

| Category | Operations |
|----------|------------|
| **Lifecycle** | Submit event, resume, abort |
| **Steering** | Steer (interrupt), follow-up (queue) |
| **Tools** | Register tool |
| **Context** | Mount/unmount providers, read/write paths |
| **History** | Checkpoint (persist), rewind to entry |
| **Events** | Subscribe to observability events |
| **Inspection** | Query status, last error |

## Build Configurations

ox-core supports two build profiles that control what gets compiled in:

- **shell-embedded** (default) — ox-shell is a dependency. Both embedded and
  sidecar modes are available.
- **shell-sidecar** — ox-shell is not compiled in. Only sidecar mode is
  available. This reduces the binary size of the wasm module.

## Platform Crate Responsibilities

ox-core is target-agnostic. The platform crates are thin wrappers:

| Crate | Target | Responsibilities |
|-------|--------|------------------|
| ox-wasi | wasm32-wasip1 | WASI entry point, WASI HTTP client, WASI filesystem history store, stdio channel for sidecar mode |
| ox-web | wasm32-unknown-unknown | wasm-bindgen exports, fetch-based HTTP client, postMessage channel for sidecar mode, IndexedDB or in-memory history store |
| ox-emscripten | wasm32-unknown-emscripten | Emscripten entry point, emscripten fetch HTTP client, emscripten FS history store |

Each platform crate depends on ox-core and provides:

1. A concrete HTTP client (passed to ox-shell if embedded)
2. Concrete provider backends (e.g. filesystem-backed history store)
3. A concrete channel implementation (if sidecar mode)
4. The module entry point / exported functions

## Dependency Graph

```
ox-wasi ──┐
ox-web ───┼──▶ ox-core ──▶ ox-kernel
ox-emsc ──┘       │            │
                  │            ├──▶ ox-context (provider namespace)
                  │            │       │
                  │            │       └──▶ ox-history (event log provider)
                  │            │
                  │            └──▶ ox-shell  [optional]
                  │
                  ▼
              (composition)
```

All arrows point downward. There are no circular dependencies. ox-kernel
defines the event model and interfaces; ox-context provides the structured
namespace; ox-history is a provider within ox-context; ox-shell implements
transport; ox-core composes them; platform crates provide the entry points and
platform-specific plumbing.

## Portability

ox-core requires only a dynamic memory allocator. All platform-specific
dependencies are isolated in platform crates and behind interface boundaries
(HTTP client, history store, channel sender/receiver).

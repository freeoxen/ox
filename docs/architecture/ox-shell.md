# ox-shell: Communication Plane

## Overview

ox-shell is the communication layer between the kernel and external services —
primarily LLM APIs. Like the rest of Ox, it is structured as a
[StructFS](https://github.com/StructFS/structfs)-style namespace of providers.
The kernel writes completion requests and reads streaming responses through
paths in this namespace. ox-shell translates those reads and writes into
concrete LLM API calls.

The defining constraint of ox-shell is **deployment flexibility**: it must be
embeddable in the same wasm module as the kernel *or* runnable as a separate
sidecar process that the kernel talks to over IPC. The same provider logic is
used in both modes — only the plumbing between kernel and shell changes.

ox-core is the composition layer that decides which mode to use and wires
ox-shell into the kernel. See the [ox-core design doc](ox-core.md).

## Namespace

```
            ox-shell namespace
┌──────────────────────────────────────────────┐
│                                              │
│  /models/*         ──▶  Model Registry       │
│                         (available models,   │
│                          capabilities)       │
│                                              │
│  /credentials/*    ──▶  Credential Store     │
│                         (API keys, tokens)   │
│                                              │
│  /complete         ──▶  Completion Provider  │
│                         (write request,      │
│                          read stream)        │
│                                              │
│  /providers/*      ──▶  LLM API Providers    │
│                         (Anthropic, OpenAI,  │
│                          Google, etc.)        │
│                                              │
└──────────────────────────────────────────────┘
```

The kernel interacts with ox-shell primarily through `/complete`: write a
completion request, read back a stream of events. Everything else in the
namespace supports that operation — model selection, credential lookup, provider
routing.

## Deployment Modes

### Mode 1: In-Process (Embedded)

```
┌──────────────────────────────────────────────────────────────┐
│                        wasm module                           │
│                                                              │
│  ┌──────────────────────────── ox-core ───────────────────┐  │
│  │                                                        │  │
│  │  ┌───────────┐                                         │  │
│  │  │ ox-kernel  │                                        │  │
│  │  └─────┬──────┘                                        │  │
│  │    r/w │                                               │  │
│  │  ┌─────┴──────────┐                                    │  │
│  │  │  ox-context     │                                   │  │
│  │  │  (namespace)    │                                   │  │
│  │  └─────┬──────────┘                                    │  │
│  │    r/w │                                               │  │
│  │  ┌─────┴──────────┐                                    │  │
│  │  │  ox-shell      │─────── HTTP ──────▶ LLM API       │  │
│  │  │  (namespace,   │                                    │  │
│  │  │   in-process)  │                                    │  │
│  │  └────────────────┘                                    │  │
│  └────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────┘
```

ox-shell is compiled into the same module. Reads and writes resolve directly to
shell functions — no serialization, no IPC. This is the simplest mode and the
default.

When to use: the host environment grants the wasm module outbound HTTP (e.g.
WASI with network access, or browser fetch).

### Mode 2: Sidecar (Out-of-Process)

```
┌──────────────────────────────────────┐       ┌──────────────────────┐
│             wasm module              │       │      sidecar         │
│                                      │       │                      │
│  ┌──────────── ox-core ───────────┐  │       │                      │
│  │  ┌───────────┐                 │  │       │                      │
│  │  │ ox-kernel  │                │  │       │                      │
│  │  └─────┬──────┘                │  │  IPC  │  ┌─────────────┐    │
│  │    r/w │                       │  │       │  │  ox-shell   │    │
│  │  ┌─────┴──────────┐           │  │       │  │  (namespace, │    │
│  │  │  ox-context     │──────────┼──┼───────┼──▶│   sidecar)  │────┼──▶ LLM API
│  │  │  (namespace)    │          │  │       │  └─────────────┘    │
│  │  └────────────────┘           │  │       │                      │
│  │  Transport = proxy (IPC)      │  │       │                      │
│  └────────────────────────────────┘  │       │                      │
└──────────────────────────────────────┘       └──────────────────────┘
```

ox-shell runs as a separate process (native binary, another wasm module, a
service). Reads and writes to shell paths are serialized over a channel and
deserialized on the other side.

When to use: the wasm module is sandboxed without network access, or when you
want to share one shell instance across multiple kernel instances, or when the
shell needs capabilities the kernel's sandbox doesn't have.

### The Transport Seam

Both modes present the same StructFS namespace to the kernel. The kernel doesn't
know or care which mode it's in.

- **Direct** — Shell namespace is in-process, reads/writes are function calls.
- **Proxy** — Shell namespace is remote, reads/writes are serialized over a
  channel.

## LLM API Providers

Each LLM API protocol is an ox-shell provider mounted under `/providers`. A
provider handles the full round-trip: serialize the completion request into the
provider's wire format, open the HTTP stream, parse SSE/chunked responses, and
emit normalized stream events.

A provider exposes:
- **Stream** — Accept a completion request, model descriptor, and credentials;
  return a stream of normalized events.
- **API kind** — Which protocol this provider implements (e.g. Anthropic
  Messages, OpenAI Completions, OpenAI Responses, Google GenAI).

Providers are registered at shell construction time. Platform crates wire up
which providers are available (e.g. ox-web might only support providers
reachable via browser fetch).

## Model Registry

Mounted at `/models`. Describes available models: their IDs, provider names,
API protocols, context window sizes, max output tokens, and capability flags
(thinking support, image support).

The shell selects a provider based on the model's provider field (or API kind
for custom endpoints). The kernel never interprets these fields — it passes
them through in the completion request and the shell routes accordingly.

Reading `/models` returns the catalog. Writing to `/models/active` selects the
active model.

## Stream Events

The normalized event stream that flows from shell to kernel when reading back a
completion:

| Event | Description |
|-------|-------------|
| Start | Stream begins |
| TextStart / TextDelta / TextEnd | Text content blocks |
| ThinkingStart / ThinkingDelta / ThinkingEnd | Reasoning blocks |
| ToolCallStart / ToolCallDelta / ToolCallEnd | Tool call blocks with argument fragments |
| Done | Stream complete, carries usage stats and stop reason |
| Error | Error or abort |

Each event carries a content index referencing its position in the response's
content block sequence. Each provider is responsible for mapping its native
streaming format into these events. The kernel consumes only this normalized
form.

## Message Transformation Pipeline

Before a completion request reaches an LLM API provider, it goes through a
two-stage pipeline:

### Stage 1: Universal Transforms (provider-agnostic)

Applied to every request regardless of provider:

- Merge consecutive same-role messages
- Strip empty text/thinking blocks
- Remove errored assistant messages (trailing only)
- Normalize tool call IDs (some providers require specific formats)
- Batch adjacent tool results into provider-expected groupings
- Filter image attachments for text-only models

### Stage 2: Provider-Specific Serialization

Each provider converts the normalized completion request into its native wire
format:

- Anthropic: messages API with content block arrays, tool_use/tool_result block types
- OpenAI: messages with tool_calls arrays, separate tool role messages
- Google: contents with parts, functionCall/functionResponse parts

This pipeline lives entirely in ox-shell. The kernel works only with normalized
event types.

## Credential Management

Mounted at `/credentials`. The credential store maps provider names to
credentials (API key, bearer token, or none). Reading from
`/credentials/{provider}` returns the credentials for that provider.

Implementations vary by platform:
- **Native/WASI sidecar**: read from environment variables or a config file
- **Browser (ox-web)**: injected by the host page, or read from a secure store
- **Embedded**: provided by the host application

The shell never persists credentials itself. It only reads them when
constructing provider requests.

## Sidecar Protocol

When running as a sidecar, reads and writes to the shell namespace are
serialized over a channel abstraction. The wire protocol is simple
request/response-with-streaming.

### Channel Abstraction

The channel is a bidirectional byte pipe. The kernel side has a sender (for
requests) and a receiver (for event streams). The channel abstraction is
intentionally minimal — just send bytes, receive bytes.

### Wire Protocol

Frames are length-prefixed:

```
┌──────────┬──────────────────┐
│ len: u32 │ payload: [u8]    │
└──────────┴──────────────────┘
```

Payload is serialized with a compact format (initially JSON for debuggability,
with a path to msgpack or bare for performance).

Four frame types:

| Frame | Direction | Purpose |
|-------|-----------|---------|
| **Read** | kernel → shell | Read from a path in the shell namespace |
| **Write** | kernel → shell | Write to a path in the shell namespace |
| **Event** | shell → kernel | Streaming response data |
| **Abort** | kernel → shell | Cancellation signal |

Each frame carries a request ID. Request IDs allow multiplexing multiple
concurrent streams over a single channel.

### Channel Implementations

The channel abstraction is intentionally minimal so it can be backed by:

| Platform | Channel Backend |
|----------|-----------------|
| WASI | stdin/stdout pipes, or WASI sockets |
| Browser | postMessage to a Web Worker or Service Worker |
| Native | Unix domain sockets, named pipes, or TCP loopback |
| Embedded | Shared memory ring buffer |

## HTTP Abstraction

LLM API providers need to make HTTP requests. Rather than depending on a
specific HTTP client, ox-shell defines a minimal HTTP interface that platform
crates implement. The interface covers:

- **Request** — Method, URL, headers, optional body.
- **Response** — Status code, header lookup, and a streaming body (chunked
  byte stream).

Platform implementations:
- **ox-web**: wraps browser fetch via wasm-bindgen
- **ox-wasi**: uses WASI HTTP outgoing-request
- **ox-emscripten**: wraps emscripten's fetch API
- **Native sidecar**: uses any native HTTP client

This keeps all provider logic platform-agnostic. Only the HTTP implementation
leaf is swapped per target.

## Portability

ox-shell's core types and provider logic have no platform dependencies beyond a
dynamic memory allocator. The HTTP and channel abstractions bridge to
platform-specific I/O without pulling in platform APIs directly.

Concrete provider implementations that parse SSE streams will need either
standard library support or platform-specific async primitives. The expectation
is that ox-shell is compiled with standard library support in practice, but its
public API surface remains usable from minimal-dependency consumers.

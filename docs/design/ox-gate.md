# ox-gate: StructFS-Native LLM Transport

## Problem

ox-web currently owns three concerns that don't belong in a shell:

1. **Transport dispatch** — `match provider.as_str()` in `run_agentic_loop()`
   chooses between Anthropic and OpenAI fetch paths.
2. **Schema translation** — `translate_to_openai()`, `parse_openai_sse_events()`,
   and `parse_sse_events()` all live in ox-web.
3. **API key management** — `HashMap<String, String>` on `OxAgent`, completely
   outside StructFS.

Any new shell (CLI, native app, different wasm host) must re-implement all of
this. The kernel's `Transport` trait is the right seam, but ox-web bypasses it
with `stream_once()` and a hand-rolled async loop.

## Design

**ox-gate** is an isolatable StructFS Block that owns provider configuration,
account management, request translation, and response parsing. It sits between
the kernel and a shell-provided HTTP store, connected through namespace wiring —
not direct references.

```
┌───────────────────────────────────────────────────────────────────┐
│                          Assembly                                 │
│                                                                   │
│  ┌────────────┐                ┌──────────┐     ┌────────────┐   │
│  │            │ ── services/   │          │ ──  │            │   │
│  │   Kernel   │    gate/* ──▶  │ ox-gate  │  ▶  │ HTTP Store │   │
│  │  (Block)   │                │ (Block)  │     │  (Block)   │   │
│  │            │ ◀── gate/*     │          │ ◀── │            │   │
│  └────────────┘                └──────────┘     └────────────┘   │
│                                                                   │
│  Wiring:                                                          │
│    kernel:/services/gate  → gate                                  │
│    gate:/services/http    → http                                  │
└───────────────────────────────────────────────────────────────────┘
```

Gate is a StructFS server *and* client. It serves completion requests from the
kernel, and internally acts as a client of the HTTP store — writing translated
HTTP requests and reading raw response chunks. All communication is read/write
on paths. Gate never touches `fetch()` or `reqwest`.

### Block isolation

Gate can run as:

- **In-process Store** — mounted directly in a namespace (current ox
  architecture, pre-Isotope). Gate holds a wired reference to the HTTP store.
- **Isolated Block** — in a full Isotope runtime, gate runs in its own wasm
  sandbox with `services/http` wired to the HTTP store block. Complete memory
  isolation.

The design is the same in both cases. Gate reads/writes paths in its namespace.
The wiring determines whether those paths resolve in-process or across block
boundaries.

### What lives where

| Concern | Before | After |
|---------|--------|-------|
| Provider configs (endpoint, dialect) | ox-web hardcoded URLs | `gate/providers/*` |
| API keys | ox-web `HashMap` + localStorage | `gate/accounts/{name}/key` |
| Account management | not modeled | `gate/accounts/*` |
| Anthropic SSE parsing | ox-web `parse_sse_events()` | ox-gate codec |
| OpenAI request translation | ox-web `translate_to_openai()` | ox-gate codec |
| OpenAI SSE parsing | ox-web `parse_openai_sse_events()` | ox-gate codec |
| Model catalogs | ox-web `fetch_*_model_catalog()` | `gate/providers/{name}/models` |
| Transport dispatch | ox-web `match provider.as_str()` | gate resolves from account |
| HTTP fetch | ox-web `fetch_*_completion()` | shell-provided HTTP store block |
| Multi-model routing | not possible | kernel targets accounts via tools |

## Providers and Accounts

A **provider** is protocol knowledge — how to speak a particular LLM API. An
**account** is identity — who you are when speaking it. These are separate
concerns.

```
gate/
  providers/
    anthropic/
      dialect         → "anthropic"
      endpoint        → "https://api.anthropic.com/v1/messages"
      version         → "2023-06-01"
      models/         → catalog (derived state, shared across accounts)
    openai/
      dialect         → "openai"
      endpoint        → "https://api.openai.com/v1/chat/completions"
      models/         → catalog

  accounts/
    personal/
      provider        → "anthropic"       # references gate/providers/anthropic
      key             → "sk-ant-abc..."
      usage/          → per-account usage tracking
    work/
      provider        → "anthropic"
      key             → "sk-ant-xyz..."
      org             → "org-456"         # provider-specific identity metadata
      usage/
    openai-personal/
      provider        → "openai"
      key             → "sk-..."
      usage/
```

Multiple accounts can reference the same provider with different keys, orgs,
and billing contexts. The provider defines *how to talk*. The account defines
*who's talking*.

Adding an OpenAI-compatible local server is a new provider + account:

```
write gate/providers/local { dialect: "openai", endpoint: "http://localhost:8080/v1/chat/completions" }
write gate/accounts/local { provider: "local" }
```

No key needed — the provider-specific codec handles auth header omission when
no key is present.

## StructFS Async Protocol

StructFS has no streams. Every read returns a discrete, coherent value.
"Streaming" is the client repeatedly requesting chunks, iterated through a
logical sequence managed by the providing store.

LLM completion follows the StructFS handle pattern:

```
write gate/accounts/personal/complete { model: "claude-sonnet-4-20250514", ... }
  → "gate/handles/a1b2c3"
```

Writing to an account's `complete` path initiates the request and returns a
handle path. The handle is a StructFS path the client reads to consume the
response.

```
read gate/handles/a1b2c3
  → {
      items: [
        { type: "text_delta", text: "Here's" },
        { type: "text_delta", text: " the answer" }
      ],
      page: { size: 2 },
      links: {
        self: { path: "gate/handles/a1b2c3" },
        next: { path: "gate/handles/a1b2c3/after/2" }
      }
    }
```

Each read returns a coherent page of events accumulated since the cursor. The
client follows `links.next` to get subsequent pages. Gate manages the underlying
HTTP connection internally — the client just does discrete reads.

```
read gate/handles/a1b2c3/after/2
  → {
      items: [
        { type: "text_delta", text: "." },
        { type: "message_stop" }
      ],
      page: { size: 2 },
      links: {
        self: { path: "gate/handles/a1b2c3/after/2" }
      }
    }
```

No `links.next` means the response is complete.

### Concurrent completions

The handle pattern naturally supports concurrent requests to different accounts:

```
write gate/accounts/personal/complete { model: "claude-sonnet-4-20250514", ... }
  → "gate/handles/a1b2c3"

write gate/accounts/openai-personal/complete { model: "gpt-4o", ... }
  → "gate/handles/d4e5f6"
```

Two writes, two independent handles. The kernel reads whichever it wants in
whatever order:

```
read gate/handles/a1b2c3          → page of anthropic events
read gate/handles/d4e5f6          → page of openai events
read gate/handles/a1b2c3/after/3  → next anthropic page
read gate/handles/d4e5f6/after/2  → next openai page
```

The account is in the write path, not in global state. Gate resolves the
provider, key, and dialect from the account at write time. The handle carries
all of that — the kernel doesn't need to know or care which provider backs
which handle after initiation.

### Blocking reads

In the Isotope model, reads can block. A kernel running natively (or in WASI)
reads the handle and the read blocks until the next page of events is available.

In wasm (single-threaded, can't block), the shell drives the loop externally
with async, reading from gate between awaits. The pattern is identical, the
scheduling differs.

## Bootstrap and Delegation

The kernel's agentic loop and model-driven delegation both use the same gate
infrastructure — write to `gate/accounts/*/complete`, read pages from the
handle, accumulate. They differ in call site and what they do with the result,
not in mechanism.

### Bootstrap account

The kernel drives its primary completion directly. The **bootstrap account** is
the account the kernel writes to when synthesizing and sending the prompt at the
start of each turn.

The bootstrap account is configured in the namespace (readable at a well-known
path), not selected by the model:

```
bootstrap_account = read("gate/bootstrap")  → "work"

loop {
    prompt = read("prompt")
    handle = write("gate/accounts/work/complete", prompt)
    content_blocks = read_pages_and_accumulate(handle)

    write assistant message → history/append
    extract tool calls from content_blocks

    if no tool calls → return

    for tool_call in tool_calls {
        let tool = tools.get(tool_call.name);
        let result = tool.execute(tool_call.input, context);
    }

    write tool results → history/append
}
```

The bootstrap is **not** a `Tool::execute` call. The kernel needs
`Vec<ContentBlock>` from the completion (to extract tool calls, text blocks,
and tool_use blocks), but `Tool::execute` returns `Result<String, String>`.
Routing bootstrap through the tool trait would lose structured content or
require the kernel to special-case one tool's return type — both worse than
keeping the call site distinct.

The bootstrap is "unspecial" in mechanism (same gate path, same codecs, same
handle protocol) but necessarily special in role (it's the one whose tool
calls get processed by the kernel's loop).

### Delegation via completion tools

The model can route sub-tasks to other accounts by calling completion tools.
Each account registered in gate produces a delegation tool:

```json
{
  "name": "complete_personal",
  "description": "Send a completion request via the 'personal' account (Anthropic, claude-sonnet-4-20250514)",
  "input_schema": {
    "type": "object",
    "properties": {
      "prompt": { "type": "string", "description": "The prompt to send" },
      "model": { "type": "string", "description": "Model override (optional)" }
    },
    "required": ["prompt"]
  }
}
```

A delegation tool's `execute`:

1. Constructs a minimal CompletionRequest (the prompt as the sole user message,
   no tools — the sub-model doesn't get to call tools)
2. Writes the request to `services/gate/accounts/{name}/complete` via context
3. Reads pages from the returned handle until completion
4. Returns the accumulated **text** as the tool result

Tool calls from the sub-model are not surfaced to the parent kernel — the
delegation tool extracts text only. This is intentional: delegation is a
leaf operation, not recursive agent spawning.

**Platform constraint:** Delegation tools require blocking reads. In native
(and WASI), reads block until data is available — delegation works. In wasm
(pre-Isotope), `Tool::execute` is synchronous but the HTTP fetch is async, so
delegation tools cannot drive the fetch internally. Delegation tools are
registered in the tool list (so the model sees them) but return an error in
wasm: `"Delegation requires native runtime or Isotope"`. This is honest about
the platform constraint and resolves cleanly when Isotope provides blocking
reads across block boundaries.

### Shared completion function

Both paths use the same function:

```rust
fn complete_via_gate(
    context: &mut dyn Store,
    account_path: &Path,
    request: &CompletionRequest,
    emit: &mut dyn FnMut(AgentEvent),
) -> Result<Vec<ContentBlock>, String>
```

The kernel calls it for bootstrap and extracts tool calls. Delegation tools
call it and extract text. Same function, different call sites, different
post-processing.

### Tool execution is StructFS

The `Tool` trait evolves to receive a `&mut dyn Store` — tools become StructFS
clients, not pure functions:

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;

    /// Execute the tool. The store provides access to the namespace for tools
    /// that need to read/write paths (e.g. completion tools that talk to gate).
    fn execute(
        &self,
        input: serde_json::Value,
        context: &mut dyn Store,
    ) -> Result<String, String>;
}
```

Regular tools that don't need StructFS access ignore the `context` parameter.
The kernel passes its namespace to every tool — the wiring determines what each
tool can see.

This aligns with the Isotope model: a Block's tools are StructFS clients in the
Block's namespace. The tool's capabilities are determined by what's wired in,
not by what's captured at construction time.

### Tool generation

Gate generates tool schemas from its account registry. When accounts change
(added, removed, key updated), the tools list updates:

```
read gate/tools/schemas
  → [
      { name: "complete_personal", description: "...", input_schema: {...} },
      { name: "complete_work", description: "...", input_schema: {...} },
      { name: "complete_openai_personal", description: "...", input_schema: {...} }
    ]
```

The shell (or ox-core) reads this and registers the tools in the `ToolRegistry`.

## Translation Codecs

A codec translates between ox's internal format (Anthropic-shaped
`CompletionRequest` + `StreamEvent`) and a provider's wire format. Codecs are
gate-internal — they never cross a block boundary.

```rust
trait Codec: Send + Sync {
    /// Translate a CompletionRequest + account config into an HTTP request.
    fn encode_request(
        &self,
        request: &CompletionRequest,
        account: &AccountConfig,
    ) -> HttpRequest;

    /// Parse a chunk of HTTP response body into stream events.
    /// Stateful — tracks SSE parser state across chunk boundaries.
    fn decode_chunk(
        &mut self,
        body: &str,
    ) -> (Vec<StreamEvent>, Option<UsageInfo>);
}
```

`decode_chunk` takes `&mut self` because SSE parsing is inherently stateful —
partial lines span chunk boundaries. Each gate handle has its own codec
instance. When `decode_chunk` produces `Some(UsageInfo)` (typically from the
final SSE event), gate stores it internally on the handle and serves it at
`gate/handles/{id}/usage`. Gate also aggregates usage into the account's
running total at `gate/accounts/{name}/usage`.

Two codecs ship initially:

- **AnthropicCodec** — identity transform on request, parses Anthropic SSE
- **OpenAiCodec** — translates request to OpenAI shape, parses OpenAI SSE back

`HttpRequest` is a plain struct (url, method, headers, body) — no platform
dependency. Gate writes it to the HTTP store via its `services/http` wiring.

## HTTP Store (Shell-Provided)

The shell provides an HTTP store block that gate writes to and reads from. This
is where platform-specific I/O lives.

### Interface

```
write http/request { url: "...", method: "POST", headers: {...}, body: "..." }
  → "http/handles/xyz789"

read http/handles/xyz789
  → {
      items: [
        { data: "event: message_start\ndata: {...}\n\n" },
        { data: "event: content_block_delta\ndata: {...}\n\n" }
      ],
      page: { size: 2 },
      links: {
        next: { path: "http/handles/xyz789/after/2" }
      }
    }
```

The HTTP store returns raw response chunks — it doesn't parse SSE semantics.
Gate's codec handles that. For non-streaming HTTP (like model catalog fetches),
the response is a single page with no `next` link containing the full body.

Each shell implements this store differently:

- **ox-web**: `web_sys::Request` + `fetch()` under the hood
- **native CLI**: `reqwest` under the hood
- **ox-wasi**: WASI HTTP under the hood

The StructFS interface is the same. Gate doesn't care.

### Wiring

Gate accesses the HTTP store through its namespace wiring, not through a direct
reference:

```yaml
wiring:
  kernel:/services/gate  → gate
  gate:/services/http    → http
```

In the pre-Isotope in-process model, this wiring is expressed as namespace
mounts. Gate's constructor takes a reference to the HTTP store (wrapped in the
platform's sharing primitive — `Arc<Mutex<..>>` native, `Rc<RefCell<..>>` wasm)
as a stand-in for proper block wiring. When ox moves to a full Isotope runtime,
this becomes real block wiring with no code change to gate's internals — gate
just reads/writes `services/http/*` either way.

## Kernel Changes

### `Transport` trait retirement

The `Transport` trait and `EventStream` trait become unnecessary. The kernel
talks StructFS:

```rust
// Before: kernel takes a Transport
pub fn run_turn<T: Transport>(
    &mut self,
    context: &mut dyn Store,
    transport: &T,
    ...
)

// After: kernel writes/reads through the Store
pub fn run_turn(
    &mut self,
    context: &mut dyn Store,
    tools: &ToolRegistry,
    emit: &mut dyn FnMut(AgentEvent),
) -> Result<Vec<ContentBlock>, String>
```

The kernel reads the bootstrap account from the namespace, writes the
synthesized prompt to that account's `complete` path, and reads pages from the
returned handle. Tool execution passes the namespace to each tool.

`accumulate_response` still exists but consumes a `Vec<StreamEvent>` from
paginated reads rather than from an `EventStream` iterator.

### Prompt synthesis and ModelProvider

`ModelProvider` in ox-context loses provider and catalog knowledge — those
belong to gate. What remains is the **selected model and max_tokens for the
bootstrap account** — configuration that feeds into prompt synthesis.

Prompt synthesis still reads `model/id` and `model/max_tokens` from the
namespace to build a `CompletionRequest`. The model name comes from whichever
model is configured for the bootstrap account. Gate can expose this at
`gate/accounts/{name}/model` — ModelProvider becomes a thin proxy that reads
from gate, or gate's account config replaces ModelProvider entirely.

### Wasm async boundary

In wasm (single-threaded, can't block), the HTTP fetch must happen between
gate's write and gate's read. The kernel can't call `run_turn` as a single
synchronous function because there's no async yield point inside it. The
kernel exposes three phase methods that the shell drives with async yields
between them:

```rust
/// Phase 1: Read prompt, write to gate, return the handle path.
/// Shell must drive the HTTP fetch before calling consume_completion.
pub fn initiate_completion(
    &mut self,
    context: &mut dyn Store,
) -> Result<Path, String>

/// Phase 2: Read all pages from a gate handle, accumulate into ContentBlocks.
/// Assumes the HTTP store already has response data (shell delivered it).
pub fn consume_completion(
    &mut self,
    context: &mut dyn Store,
    handle: &Path,
    emit: &mut dyn FnMut(AgentEvent),
) -> Result<Vec<ContentBlock>, String>

/// Phase 3: Write assistant message, execute tool calls, write results.
/// Returns true if tool calls were executed (caller should loop).
pub fn complete_turn(
    &mut self,
    context: &mut dyn Store,
    content: Vec<ContentBlock>,
    tools: &ToolRegistry,
    emit: &mut dyn FnMut(AgentEvent),
) -> Result<bool, String>
```

The shell's async loop:

```rust
loop {
    let handle = kernel.initiate_completion(&mut ctx)?;

    // Shell drives HTTP fetch (async yield point)
    let pending = http_store.take_pending()?;
    let response = fetch(pending).await?;
    http_store.deliver_response(response);

    let content = kernel.consume_completion(&mut ctx, &handle, &mut emit)?;

    if !kernel.complete_turn(&mut ctx, content, &tools, &mut emit)? {
        break;
    }
}
```

`run_turn` for native composes from the same three phases — reads can block,
so no async interleaving is needed:

```rust
pub fn run_turn(&mut self, context, tools, emit) {
    loop {
        let handle = self.initiate_completion(context)?;
        let content = self.consume_completion(context, &handle, emit)?;
        if !self.complete_turn(context, content, tools, emit)? {
            return Ok(content);
        }
    }
}
```

For token-by-token streaming in wasm, the shell delivers HTTP response chunks
incrementally and calls `consume_completion` between deliveries. Each call
returns whatever events have accumulated since the last read — the handle's
cursor tracks position via standard StructFS pagination (`after/{cursor}`).

## Single Mount, Many Topologies

Gate mounts once at `gate/` with sub-paths. The namespace supports
rename-mounts, so any topology is achievable:

**Single mount (default):**
```
gate/providers/anthropic/*
gate/providers/openai/*
gate/accounts/personal/*
gate/accounts/work/*
gate/accounts/personal/complete  ← write here to complete via personal account
```

**Rename-mounted accounts (via namespace):**
```
personal/ → gate/accounts/personal/    (rename mount)
work/     → gate/accounts/work/        (rename mount)
```

The single-mount-with-sub-paths design naturally supports any topology without
special-casing.

## Crate Dependency Graph

```
ox-gate (new)
  ├── ox-kernel (CompletionRequest, StreamEvent, ToolSchema, Tool types)
  ├── structfs-core-store (Store, Reader, Writer, Path, Value, Record)
  └── structfs-serde-store (json_to_value, value_to_json, to_value, from_value)

ox-context
  ├── ox-kernel
  ├── structfs-core-store
  └── structfs-serde-store
  (no dependency on ox-gate — gate is a sibling, both mounted in namespace)

ox-core
  ├── ox-kernel
  ├── ox-context
  ├── ox-gate (new)
  └── structfs-serde-store

ox-web
  ├── ox-core
  ├── ox-kernel (re-exports)
  └── (provides HTTP store block, mounts gate — no translation code)
```

ox-gate has no platform dependencies. It's pure Rust, works on any target.
Platform-specific I/O lives exclusively in the shell's HTTP store block.

## Migration Path

### Phase 1: Extract codecs into ox-gate

Move `translate_to_openai()`, `parse_openai_sse_events()`, `parse_sse_events()`
into ox-gate as codec implementations. ox-web calls gate's codecs instead of
its own functions. Gate is a library, not yet a Store.

**Changes:**
- New: `crates/ox-gate/src/lib.rs`, `crates/ox-gate/src/codecs/anthropic.rs`,
  `crates/ox-gate/src/codecs/openai.rs`
- Modified: `crates/ox-web/src/lib.rs` (call gate codecs, delete local fns)
- Modified: `Cargo.toml` (new workspace member)

### Phase 2: Gate as Store + provider/account registry

Gate becomes a Store. Provider config and account management move from ox-web
to gate's StructFS state. API keys move into accounts.

**Changes:**
- Modified: `crates/ox-gate/src/lib.rs` (implement Store, Reader, Writer)
- New: `crates/ox-gate/src/provider.rs`, `crates/ox-gate/src/account.rs`
- Modified: `crates/ox-web/src/lib.rs` (mount gate, remove api_keys HashMap,
  write keys to gate accounts)
- Modified: `crates/ox-context/src/lib.rs` (ModelProvider loses provider/catalog
  fields, becomes thin proxy or removed)

### Phase 3: Async protocol + HTTP store

Gate implements the write-handle-read protocol. Shell provides HTTP store block.
Kernel drops `Transport` trait, talks StructFS.

**Changes:**
- Modified: `crates/ox-kernel/src/lib.rs` (remove Transport/EventStream traits,
  add initiate_completion/consume_completion/complete_turn phases,
  Tool::execute gains `context` param, run_turn composes from phases,
  bootstrap account in agentic loop)
- New: `crates/ox-web/src/http_store.rs` (platform HTTP store)
- Modified: `crates/ox-web/src/lib.rs` (mount HTTP store, rewire agentic loop
  to use initiate_completion/consume_completion/complete_turn phases)
- Modified: `crates/ox-gate/src/lib.rs` (handle lifecycle, page reads, HTTP
  store wiring)

### Phase 4: Accounts as tools + multi-model routing

Gate generates completion tools from accounts. Kernel routes via tool calls.
Model can fan out to multiple accounts concurrently.

**Changes:**
- New: `crates/ox-gate/src/tools.rs` (completion tool generation from accounts)
- Modified: `crates/ox-core/src/lib.rs` (read gate tool schemas, register in
  ToolRegistry)
- Modified: `crates/ox-kernel/src/lib.rs` (pass context to tool execute)

## Design Decisions

### Gate is block-isolatable

Gate is designed to run as an independent block. It accesses the HTTP store
through namespace wiring (`services/http/*`), not through captured references.
All state is internal. All communication is read/write on paths.

In the current pre-Isotope architecture, this wiring is simulated with shared
references passed at construction. Gate's internal code is identical in both
cases — it reads and writes paths. The transition to real block isolation
requires no changes to gate's logic.

### Tools are StructFS clients

`Tool::execute` receives `&mut dyn Store` (the namespace). This means:

- Completion tools write to `services/gate/accounts/*/complete` and read pages
  from the handle — no captured references needed.
- Regular tools that don't need StructFS access ignore the parameter.
- Tool capabilities are determined by namespace wiring, not construction-time
  captures. A tool can only reach what its Block's namespace exposes.

This is the Isotope model applied to tools: a tool's power comes from its
wiring, not from what it smuggles in at construction time.

### Handles don't need cleanup

A gate handle is a translated view of an HTTP store handle — not a buffer. Gate
holds a mapping (gate handle ID → HTTP handle path) plus codec parser state per
handle. There is no heavyweight resource to release.

When the response completes (no `next` link), the handle becomes inert. Session
teardown sweeps all handles. If memory pressure ever matters, LRU eviction. No
explicit close ceremony, no cleanup API surface.

### Errors are `Err(StoreError)`, not values

Reads return `Result<Option<Record>, StoreError>`. If the HTTP store returns a
401 or the connection drops, the next page read returns
`Err(StoreError::store("gate", "read", "authentication failed"))`.

The client's read loop already handles `Result`. No separate error path, no
inline error objects serialized into the Value domain. Partial success works
naturally: pages 1-5 return `Ok(Some(...))`, page 6 returns `Err(...)`.

### Usage is suffix metadata

`gate/handles/{id}/usage` — readable after the response includes usage data
(typically the last page from the provider). This is the suffix meta pattern:
usage tokens are a property of the completion, the same way file size is a
property of a file.

Per-account usage is aggregated at `gate/accounts/{name}/usage` — total tokens
consumed across all completions for that account.

### Model catalogs are derived state

The catalog at `gate/providers/{name}/models` is a read-only derived view of
external state. Gate internally manages freshness:

- **Key change** — writing a new key to any account referencing this provider
  invalidates the cached catalog.
- **Provider config change** — new endpoint or dialect invalidates likewise.
- **TTL expiry** — gate tracks when it last fetched and re-fetches transparently
  when the cached catalog is stale.
- **First read** — no cached catalog triggers an on-demand fetch.

The client just reads `gate/providers/{name}/models` and gets a catalog. It
never requests a refresh. Whether the data was cached or freshly fetched is the
store's internal concern — consistent with StructFS's principle that a read
returns a coherent value and the store manages how that value is produced.

### Codecs are gate-internal, stateful per-handle

SSE parsing is inherently stateful — partial lines span chunk boundaries. Each
gate handle gets its own codec instance with `&mut self` on `decode_chunk`.
This state is entirely internal to gate's block. It never crosses a block
boundary and never appears in the StructFS interface.

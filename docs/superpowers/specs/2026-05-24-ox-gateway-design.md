# ox-gateway: an OpenAI/Anthropic-shaped LLM gateway on the StructFS substrate

**Date:** 2026-05-24
**Status:** Draft
**Driver:** personal dogfooding

## Overview

A new standalone binary, `ox-gateway`, that exposes a local HTTP API
clients can point at without code changes — Anthropic Messages
(`/v1/messages`), OpenAI Chat Completions (`/v1/chat/completions`),
plus `/v1/models` and Anthropic's `/v1/messages/count_tokens`. The
binary shares `~/.ox/config.toml` and `~/.ox/keys.json` with ox-cli
so accounts, providers, and keys are one set of facts across
processes. Inbound dialect is whatever the client speaks; upstream
dialect is whatever the resolved account uses; cross-dialect
translation falls out of the codec symmetry the gateway requires
ox-gate to grow anyway.

The architectural commitment: **every state read, every state write,
every dispatch the gateway makes goes through StructFS Reader/Writer**.
The binary is a thin axum shell over a tree of mounted Stores that
do the actual work. The dispatch primitive is a new
`CompletionBrokerStore` modeled on `structfs-http`'s `HttpBrokerStore`
pattern, generalized for streaming. There is no parallel code path
that bypasses the substrate: no direct transport function calls from
axum handlers, no env-var key reads, no on-disk file IO outside the
backings the substrate is configured with.

## Scope

**In:**

- `POST /v1/messages` (Anthropic Messages, streaming + non-streaming)
- `POST /v1/chat/completions` (OpenAI Chat Completions, streaming + non-streaming)
- `GET /v1/models` (both Anthropic and OpenAI list shapes; aggregated across accounts)
- `POST /v1/messages/count_tokens` (Anthropic; passthrough to upstream)
- ox-native HTTP shape: `POST /completions` taking a raw `CompletionRequest`
  and yielding raw `StreamEvent`s via SSE
- Loopback-only (127.0.0.1) — no auth
- Usage ledger as an append-only JSONL ledger mounted at `gateway/usage`
- Routing via `model` field: slash-form (`{account}/{model_id}`) or
  named-role lookup against `gate/completions/{name}`

**Out (deferred):**

- Multi-tenant / inbound API-key auth
- Non-loopback binding
- Hot reload of `~/.ox/config.toml` / `~/.ox/keys.json` while the daemon
  runs (the daemon reads at startup; restart picks up changes)
- Upstream cancellation on client disconnect (default: per-request task
  runs to completion, usage is recorded; an `AbortHandle` per inflight
  is a small additive change later)
- Per-tenant fallback chains / load balancing across multiple keys
  (deliberately rejected — `CompletionRole` is a single binding by
  design; fallback can live in a `gateway/fallback/{name}` mount when
  there's a real need)
- Token-counting usage records (`count_tokens` does not write to
  the ledger in v1)
- Wasm-component packaging (the design respects the
  StructFS-interface boundary so this remains additive later)

## Mount tree

```text
broker mounts:
  config/                ConfigStore                    TomlFileBacking(~/.ox/config.toml)   [shared with ox-cli]
  secret/                LocalConfig                    JsonFileBacking(~/.ox/keys.json)     [shared with ox-cli, mode 0600]
  gate/                  GateStore                      .with_config(handle("config"))
                                                        .with_secrets(handle("secret"))      [identical to ox-cli wiring]
  gateway/completions/   CompletionBrokerStore          ClientHandle + SseHttpExecutor
  gateway/usage/         UsageStore                     JsonlFileBacking(~/.ox/usage.jsonl)
```

The first three mounts are byte-identical to ox-cli's setup; two new
ones live under `gateway/`. The substrate is the same file substrate
ox-cli reads; the gateway is one more process binding the same
backings into its own broker.

Construction order: build broker → mount config/secret/gate → take a
root `ClientHandle` from the broker → construct `CompletionBrokerStore`
and `UsageStore` using that handle → mount both under `gateway/` →
start axum on 127.0.0.1.

## §1: Codec symmetry

`ox-gate::codec::{anthropic,openai}` today covers two of four corners:

| | Anthropic | OpenAI |
|---|---|---|
| internal → wire request | `translate_request` ✓ | `translate_request` ✓ |
| wire SSE → internal events | `parse_sse_events` / `SseParser` ✓ | `parse_sse_events` / `SseParser` ✓ |
| **wire request → internal** | **add** `decode_request` | **add** `decode_request` |
| **internal events → wire SSE** | **add** `SseEncoder::encode_sse` | **add** `SseEncoder::encode_sse` |

The kernel only needs the first two (it always speaks the internal
`CompletionRequest`/`StreamEvent` shape and translates outward).
The gateway needs all four because it has to decode inbound (wire →
internal) and re-encode outbound (internal → wire) on both dialects.

### New types (in `ox-gate::codec`)

```rust
pub struct SseEncoder { dialect: String, /* per-dialect state */ }

impl SseEncoder {
    pub fn new(dialect: &str) -> Self;
    /// Encode a single StreamEvent into the appropriate wire SSE line(s)
    /// for this dialect. Returns None when the event has no wire-visible
    /// projection (e.g. an internal event a dialect doesn't expose).
    pub fn encode_sse(&mut self, event: &StreamEvent) -> Option<String>;
    /// Final closing frame the dialect requires (`data: [DONE]` for OpenAI,
    /// nothing for Anthropic). Called after the last event.
    pub fn finish(&mut self) -> Option<String>;
}

pub enum CodecError {
    MissingField(&'static str),
    InvalidShape(String),
    UnsupportedFeature(String),
}

// Per dialect module:
pub fn decode_request(body: &serde_json::Value) -> Result<CompletionRequest, CodecError>;
pub fn encode_response(events: &[StreamEvent]) -> serde_json::Value;  // non-streaming path
```

### Property tests

For each dialect, the load-bearing tests:

```rust
proptest! {
    #[test]
    fn anthropic_request_roundtrips(req in arb_completion_request()) {
        let wire = anthropic::encode_request(&req);
        let back = anthropic::decode_request(&wire)?;
        prop_assert_eq!(canonicalize(&req), canonicalize(&back));
    }

    #[test]
    fn anthropic_sse_roundtrips(events in arb_stream_events()) {
        let mut encoder = SseEncoder::new("anthropic");
        let wire: String = events.iter()
            .filter_map(|e| encoder.encode_sse(e))
            .chain(encoder.finish())
            .collect();
        let mut parser = SseParser::new("anthropic");
        let back: Vec<_> = wire.lines()
            .flat_map(|l| parser.feed(l))
            .collect();
        prop_assert_eq!(canonicalize(&events), canonicalize(&back));
    }
}
```

`canonicalize` drops dialect-irrelevant noise (e.g. `ToolUseInputDelta`
chunking boundaries that aren't load-bearing across encode/decode).

### `StreamEvent` widening + relocation

`StreamEvent` gains two variants and moves from `ox-kernel` to
`ox-types`:

```rust
// ox-types::stream_event (new module)
pub enum StreamEvent {
    TextDelta(String),
    ToolUseStart { id: String, name: String },
    ToolUseInputDelta(String),
    MessageStop,
    Error(String),

    // New — make usage roundtrip through Record::Parsed(Value) so the
    // gateway can re-encode usage blocks on the way out. Replaces the
    // current SseParser::usage side-channel.
    InputUsage  { input_tokens: u32, cache_creation: u32, cache_read: u32 },
    OutputUsage { output_tokens: u32 },
}
```

**Three consumers, one source:** the host-side `SseParser` emits these
as it parses upstream SSE; the guest-side `SseEncoder` consumes them
to re-emit usage blocks on the inbound dialect; `CompletionBrokerStore`
extracts them at terminal status to build a `UsageRecord`. The kernel
ignores both new variants (no behavior change — `accumulate_response`
already treats unknown shapes as no-ops).

The move from `ox-kernel` to `ox-types` is because `StreamEvent` now
crosses the StructFS interface as a typed record. `ox-types` is where
"types that cross the substrate" live (`CompletionRole`, `ModelInfo`,
`CompletionRequest`).

Touches: re-export from `ox-kernel` for compatibility during the
transition; update direct imports in `ox-gate`, `ox-tools`, `ox-web`,
`ox-cli` (mechanical).

## §2: `CompletionBrokerStore`

Modeled on `structfs-http::HttpBrokerStore`, generalized to streaming.
Lives in `ox-gate::completion_broker` (new module, host-only —
`#[cfg(not(target_arch = "wasm32"))]`).

### Path layout (matches `HttpBrokerStore` conventions)

```text
write /                                   CompletionRequest → outstanding/{N}
read  /                                   root descriptor (References)
read  outstanding                         { items: [Reference, ...] }
read  outstanding/{N}                     CompletionStatus
read  outstanding/{N}/request             original CompletionRequest
read  outstanding/{N}/events/from/{S}     Vec<StreamEvent> — BLOCKING
read  outstanding/{N}/events/count        usize — non-blocking, current length
read  outstanding/{N}/usage               UsageInfo (None until Complete)
write outstanding/{N} null                GC
read  meta/queue                          HATEOAS action descriptor
read  meta/outstanding/{N}                handle descriptor w/ references
read  docs                                documentation
```

### Types

```rust
pub struct CompletionBrokerStore<E: SseHttpExecutor = ReqwestSseExecutor> {
    substrate: ox_broker::ClientHandle,        // reads gate/* and secret/*
    executor: Arc<E>,                          // injected; mockable
    handles: HashMap<RequestId, Arc<Inflight>>,
    next_request_id: RequestId,
    usage_writer: ox_broker::ClientHandle,     // scoped to gateway/usage
    runtime: tokio::runtime::Handle,
}

struct Inflight {
    state: tokio::sync::Mutex<InflightState>,
    notify: tokio::sync::Notify,
}

struct InflightState {
    request: CompletionRequest,
    events: Vec<StreamEvent>,
    status: CompletionStatus,
    usage: Option<UsageInfo>,
    started_at_ms: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase", tag = "state")]
pub enum CompletionStatus {
    Pending,
    Streaming { account: String, model_id: String },
    Complete  { account: String, model_id: String, completed_at_ms: u64 },
    Failed    { account: String, model_id: String, reason: String, failed_at_ms: u64 },
}
```

### Trait shape

Implements `ox_broker::AsyncReader + AsyncWriter`. The async-reader
trait's `fn read(&mut self, ...) -> BoxFuture<...>` is the load-bearing
detail: the returned future is `'static + Send` and **does not borrow
self**. The store synchronously consults `&mut self`, clones the
relevant `Arc<Inflight>` out, builds a future that owns it, and
returns. The mount's actor lock is released; the future awaits on
`Notify` without holding the actor.

### `write(/, CompletionRequest)` side effect

1. Decode payload; mint `id`; advance `next_request_id`.
2. Insert `Arc<Inflight { status: Pending, ... }>` into `handles`.
3. Return immediately with `outstanding/{id}` handle path.
4. Spawn `per_request_task(inflight, substrate, executor, usage_writer, request, id)`.

The spawned task:

```text
a. Resolve req.model → CompletionRole:
     if slash:  parse account/model_id
     else:      substrate.read_typed(gate/completions/{model})
   On failure: status = Failed { reason: "no role named X" }, notify, return.

b. Resolve account/provider/key via three substrate reads:
     substrate.read_typed(gate/accounts/{role.account})
     substrate.read_typed(gate/providers/{cfg.provider})
     substrate.read_typed(secret/keys/{role.account})
   On any failure: status = Failed { reason: ... }, notify, return.

c. status = Streaming { account, model_id }; notify.

d. Build HttpRequest via codec::{anthropic,openai}::encode_request
   based on provider.dialect, with auth header per
   provider.resolved_auth() and anthropic-version header if needed.

e. Stream events from executor.execute(http_request, provider.dialect):
     for ev in stream:
         lock state; push ev; release; notify_waiters

f. On stream end clean:
     usage = extract from InputUsage/OutputUsage events buffered
     status = Complete { account, model_id, completed_at_ms }
     notify
     usage_writer.write(&path!("append"), &UsageRecord { ... })

g. On stream error:
     status = Failed { reason }
     notify
     (no usage record on Failed)
```

### `read(outstanding/{N}/events/from/{S})` semantics

```rust
fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
    let inflight = match self.handles.get(&id) {
        Some(arc) => arc.clone(),
        None => return Box::pin(async { Ok(None) }),
    };
    Box::pin(async move {
        loop {
            let state = inflight.state.lock().await;
            if state.events.len() > seq {
                let tail = state.events[seq..].to_vec();
                return Ok(Some(Record::parsed(to_value(&tail)?)));
            }
            if state.status.is_terminal() {
                let tail = state.events[seq..].to_vec();  // possibly empty
                return Ok(Some(Record::parsed(to_value(&tail)?)));
            }
            drop(state);
            inflight.notify.notified().await;
        }
    })
}
```

Pure blocking via `Notify`; no sleeps, no polling. Multiple concurrent
readers on the same handle work — `Notify::notified()` supports
multi-waiter and `notify_waiters()` wakes all of them.

### Mockability

Generic on `E: SseHttpExecutor`. Tests inject a `MockSseExecutor` that
yields canned `StreamEvent` sequences with controllable timing. The
substrate handle in tests is an in-memory `LocalConfig`-backed broker.
No network, no real broker needed.

## `SseHttpExecutor`

Streaming sibling of `structfs_http::HttpExecutor`. Lives in
`ox-gate::transport` (host-only).

```rust
pub trait SseHttpExecutor: Send + Sync + 'static {
    fn execute(
        &self,
        request: HttpRequest,          // structfs_http::HttpRequest
        dialect: String,               // "anthropic" | "openai" — drives the parser
    ) -> BoxStream<'static, Result<StreamEvent, String>>;
}

pub struct ReqwestSseExecutor { client: reqwest::Client, /* timeout, etc */ }

impl SseHttpExecutor for ReqwestSseExecutor {
    fn execute(&self, request: HttpRequest, dialect: String) -> BoxStream<...> {
        // Build the reqwest request from HttpRequest; stream the body
        // line-by-line; feed each line into SseParser; emit each
        // resulting StreamEvent. End-to-end timeout enforced internally:
        // 5 minutes default, configurable via ReqwestSseExecutor::new(timeout).
    }
}
```

`HttpRequest` is reused from `structfs-http` verbatim — `method`,
`path`, `query`, `headers`, `body`. The codec's `encode_request`
returns a `CompletionRequest`-shaped JSON body; the dispatch code
wraps that in an `HttpRequest` with auth headers per `AuthScheme`.

The existing `ox-gate::transport::streaming_fetch` (sync, blocking)
stays in place for the kernel's in-process use. It can later be
reframed as a `ReqwestSseExecutor` implementation that the kernel
also routes through, but that's a separate cleanup.

## `UsageStore`

Mounted at `gateway/usage/`. Backed by `JsonlFileBacking` on
`~/.ox/usage.jsonl`.

### Paths

```text
write append            UsageRecord → one JSON line appended
read  /                 Vec<UsageRecord> (full ledger; reasonable for personal volume)
read  today             aggregated projection over today (sum tokens, cost, count)
read  ?account={n}&since={ts}    filtered projection (later, not v1)
```

### `UsageRecord`

```rust
#[derive(Serialize, Deserialize)]
pub struct UsageRecord {
    pub id: RequestId,
    pub account: String,
    pub model_id: String,
    pub dialect: String,                       // inbound dialect
    pub upstream_dialect: String,              // resolved provider.dialect
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_creation_input_tokens: u32,
    pub cache_read_input_tokens: u32,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub estimated_cost_usd: Option<f64>,       // via ox_gate::pricing::model_pricing
}
```

One line per completion. `estimated_cost_usd` is best-effort —
`pricing::model_pricing` returns `Some` only for known model prefixes;
unknown models record `None` so volume is visible without lying about
cost.

### `JsonlFileBacking`

New `ox_store_util::JsonlFileBacking` — sibling of `TomlFileBacking`
and `JsonFileBacking`. Append-only: writes serialize one `Value` per
line; loads slurp the whole file into a `Vec<Value>`. For personal
scale (single-digit thousands of records over months) this is fine;
projection caching can be added later if it grows.

## §3: Request lifecycle

### Happy path — `POST /v1/messages`, streaming, slash-form model

```http
POST /v1/messages HTTP/1.1
Content-Type: application/json
Accept: text/event-stream

{ "model": "anthropic/claude-sonnet-4-20250514", "stream": true, ... }
```

1. axum router → `routes::messages::post`. 127.0.0.1 only; no auth check.
2. `let req = codec::anthropic::decode_request(&body)?;`
3. `let handle = client.write_typed(&path!("gateway/completions"), &req).await?;`
   — returns `Path("gateway/completions/outstanding/42")`. Per-request
   task begins; status flips Pending → Streaming.
4. axum builds an SSE response body that loops:

```rust
let mut encoder = SseEncoder::new("anthropic");
let mut next = 0usize;
async_stream::stream! {
    loop {
        let events: Vec<StreamEvent> = client
            .read_typed(&handle.join(&path!("events/from")).join(&path!(&next.to_string())))
            .await?
            .unwrap_or_default();
        for ev in &events {
            if let Some(line) = encoder.encode_sse(ev) {
                yield Ok(Event::default().data(line));
            }
        }
        next += events.len();
        let status: CompletionStatus = client.read_typed(&handle).await?.unwrap();
        match status {
            CompletionStatus::Complete { .. } => {
                if let Some(line) = encoder.finish() {
                    yield Ok(Event::default().data(line));
                }
                break;
            }
            CompletionStatus::Failed { reason, .. } => {
                yield Err(/* dialect-shaped SSE error */);
                break;
            }
            _ => continue,
        }
    }
    let _ = client.write(&handle, Record::parsed(Value::Null)).await;  // GC
}
```

Three substrate calls in the loop: `read(events/from/{N})`,
`read(outstanding/{N})`, on terminal `write(outstanding/{N}, null)` to
GC. Usage is appended *inside* `CompletionBrokerStore`'s terminal-status
transition — axum never touches the ledger.

### Variations

- **OpenAI streaming** (`POST /v1/chat/completions`): identical, but
  `codec::openai::decode_request` and `SseEncoder::new("openai")`.
  Upstream dialect is whatever `provider.dialect` resolves to;
  cross-dialect translation falls out of codec symmetry.

- **Named role** (`"model": "fast"`): the resolution step inside
  `per_request_task` reads `gate/completions/fast → CompletionRole`
  instead of parsing a slash. axum doesn't know which path was taken.

- **Non-streaming** (`stream: false` in OpenAI body or
  `Accept: application/json` for Anthropic): same lifecycle through
  the substrate. After receiving terminal status, handler reads
  `outstanding/{N}/events/from/0` once to drain the full buffer and
  uses `codec::*::encode_response(events)` (sibling of `encode_sse`)
  to produce single-shot JSON. No SSE framing.

- **`GET /v1/models`**: does NOT go through `gateway/completions`.
  Handler iterates `gate/accounts/*`, reads each account's provider,
  reads `gate/providers/{provider}/models`, emits `{account}/{model_id}`
  ids in both Anthropic and OpenAI list shapes.

- **`POST /v1/messages/count_tokens`** (Anthropic-only): doesn't
  stream. Resolution is the same. Add a sibling executor method
  `count_tokens(HttpRequest) -> HttpResponse` (single-shot HTTP), or
  call `structfs_http::HttpClientStore` mounted at the account's
  endpoint. No usage record in v1.

### Routing rule (the `model` field)

```rust
fn resolve(model: &str, substrate: &ClientHandle) -> Result<CompletionRole, ResolveError> {
    if let Some((account, model_id)) = model.split_once('/') {
        // Slash form: parse, not lookup. Account validity is checked
        // downstream by reading gate/accounts/{account}.
        return Ok(CompletionRole {
            account: account.into(),
            model_id: model_id.into(),
        });
    }
    // Bare name: look up gate/completions/{name}. Same path the kernel
    // reads for its CompletionRole; `primary` is the existing entry,
    // additional names extend the same namespace.
    let path = oxpath!("gate", "completions", model);
    substrate.read_typed(&path).await?
        .ok_or_else(|| ResolveError::UnknownRole(model.to_string()))
}
```

**Two resolution paths, one return type.** The slash form is a parse;
the bare form is a substrate lookup. No "first account that has this
model" fallback — would be invisible state determining behavior.
Aliases are explicit entries in `gate/completions/*`; the slash form
is for when you don't want to name an alias.

### Error paths

| Where | Cause | Wire response |
|---|---|---|
| `decode_request` → `CodecError` | bad JSON / missing field | 400 with dialect-shaped error |
| `per_request_task` step (a) | unknown role name | 404 with dialect-shaped error |
| step (b) | unknown account / unknown provider | 404 |
| step (b) | missing API key | 401 with hint at `~/.ox/keys.json` |
| `executor.execute` stream yields `Err` | upstream 4xx/5xx/network | propagated mid-stream as dialect-shaped error |
| executor end-to-end timeout | upstream stalled | same |
| axum drops response future | client disconnect | per-request task continues; usage recorded; inflight GC'd by Drop guard |

All error messages reuse the existing
`ox_gate::transport::format_http_error` / `format_network_error`
formatters (account/provider/URL-tagged messages). The handler wraps
these in the dialect's error envelope.

### Cancellation default

Per-request task **runs to completion** on client disconnect. Usage is
recorded. Adding upstream cancellation later is a 5-line change: hold
an `AbortHandle` per inflight, abort on GC.

## §4: Bin layout

```text
crates/ox-gateway/                  (new bin crate)
├── Cargo.toml                      depends on ox-gate, ox-types, ox-broker, ox-store-util,
│                                   structfs-core-store, structfs-serde-store, structfs-http,
│                                   axum, tokio (full), tower-http, async-stream, ulid, tracing
└── src/
    ├── main.rs                     broker assembly + axum serve
    ├── routes/
    │   ├── mod.rs
    │   ├── anthropic.rs            POST /v1/messages, POST /v1/messages/count_tokens
    │   ├── openai.rs               POST /v1/chat/completions
    │   ├── models.rs               GET /v1/models (both dialect shapes via Accept)
    │   └── ox_native.rs            POST /completions (raw CompletionRequest / StreamEvent SSE)
    ├── handle.rs                   small async helper: write trigger, loop reading events,
    │                               encode via SseEncoder, GC on drop
    └── error.rs                    map ResolveError / CodecError / StoreError → dialect
                                    error envelopes
```

`main.rs` skeleton:

```rust
#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let ox_dir = ox_path::ox_dir()?;
    let toml_path = ox_dir.join("config.toml");
    let keys_path = ox_dir.join("keys.json");
    let usage_path = ox_dir.join("usage.jsonl");

    let broker = ox_broker::BrokerStore::new(Duration::from_secs(2));

    // Shared mounts (same shape as ox-cli)
    let base = ox_cli_config::resolve_config(&ox_dir, &Default::default()).to_flat_map();
    let config = ox_ui::config_store::ConfigStore::with_backing(
        base,
        Box::new(ox_store_util::TomlFileBacking::new(&toml_path)?),
    );
    broker.mount(oxpath!("config"), config).await;

    let secret = ox_store_util::LocalConfig::with_backing(
        Default::default(),
        Box::new(ox_store_util::JsonFileBacking::new(&keys_path)?),
    );
    broker.mount(oxpath!("secret"), secret).await;

    let gate = ox_gate::GateStore::new()
        .with_config(Box::new(broker.handle("config")))
        .with_secrets(Box::new(broker.handle("secret")));
    broker.mount(oxpath!("gate"), gate).await;

    // Gateway mounts
    let usage_store = ox_gateway::UsageStore::new(
        Box::new(ox_store_util::JsonlFileBacking::new(&usage_path)?),
    );
    broker.mount(oxpath!("gateway", "usage"), usage_store).await;

    let completions = ox_gate::CompletionBrokerStore::new(
        broker.client(),
        Arc::new(ox_gate::ReqwestSseExecutor::with_default_timeout()?),
    );
    broker.mount(oxpath!("gateway", "completions"), completions).await;

    // axum
    let app = ox_gateway::router(broker.client());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:11343").await?;
    tracing::info!(addr = %listener.local_addr()?, "ox-gateway listening");
    axum::serve(listener, app).await?;
    Ok(())
}
```

Bind port: `11343` (free; not in IANA registered range). Configurable
via `OX_GATEWAY_BIND` env var; loopback-only enforced at startup.

## Test strategy

- **Codec symmetry**: property tests with `proptest` over arbitrary
  `CompletionRequest` and `Vec<StreamEvent>`. One pass per dialect.
- **`CompletionBrokerStore`**: in-memory `LocalConfig` substrate +
  `MockSseExecutor` that emits canned event sequences. Lifecycle
  tests cover happy path, all failure modes, concurrent
  `read(events/from/{N})` from two consumers, GC, supersession (writes
  with same id don't exist by construction — `next_request_id`
  monotonic).
- **Routing**: unit tests on `resolve` with stubbed substrate
  for slash form + named role + unknown.
- **Integration**: spin up the full broker tree against a temp dir,
  hit axum with `reqwest` against a `MockSseExecutor`. Verify wire
  shapes match Anthropic / OpenAI specs for both happy and error
  paths.

## What stays in `ox-gate::transport`

The current sync `streaming_fetch`, `test_connection_async`, and
`fetch_model_catalog_async` continue to exist for kernel and
subscription use. The new `SseHttpExecutor` + `ReqwestSseExecutor`
are added alongside. No changes to existing transport callers; this
is purely additive at the transport layer.

## Out of scope (future work)

These are deliberately deferred. Adding them later is additive — no
existing design decision in this spec needs to be undone:

1. **Inbound API-key auth** — `secret/inbound/{name}` namespace + a
   bearer-token middleware. Required only when binding non-loopback.
2. **Hot reload** — file-watcher subscription on
   `~/.ox/config.toml`/`~/.ox/keys.json` that re-merges changes into
   the running broker's config/secret mounts.
3. **Upstream cancellation on disconnect** — `AbortHandle` per
   inflight, abort on GC.
4. **`gateway/fallback/{name}`** — ordered list of `CompletionRole`s
   for upstream fallback. Lives separate from the canonical
   `gate/completions/{name}` to avoid changing the single-binding
   shape the kernel depends on.
5. **`/v1/messages/count_tokens` ledger** — when there's a reason to
   track metadata-call volume separately.
6. **Projection cache for `gateway/usage`** — if the JSONL grows
   beyond reasonable scan time.
7. **Web/TUI dashboard subscriber** — once `gateway/usage` and
   `gateway/completions/outstanding/*` are substrate-mediated, a
   TUI showing live spend + in-flight requests is mostly Reader work
   on existing paths.

## Open decisions

None blocking the implementation plan. Decisions logged here for
completeness in case revisiting comes up:

- **`StreamEvent` location:** moved from `ox-kernel` → `ox-types`.
  Re-export retained in `ox-kernel` during the transition; can be
  removed once `ox-tools`, `ox-gate`, `ox-web`, `ox-cli` all import
  from `ox-types`.
- **Bind port:** `11343`. Reconsidered if it collides with anything
  in practice.
- **Pricing fallback:** unknown models record `estimated_cost_usd:
  None`. Not an error, not zero — explicit absence of information.

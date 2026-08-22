# ox-gateway on Isotope: Wasm Blocks over StructFS

**Goal:** Restructure ox-gateway from a monolithic native daemon into
Isotope-native form: the gateway's logic runs as Wasm **Blocks** whose only
interface is StructFS reads/writes, composed into an **Assembly** whose
host-side edges (TCP ingress, upstream HTTP egress, file backings) are
ordinary stores wired into Block namespaces.

## Ground truth this spec builds on

- **Isotope is a spec, not yet a runtime.** The StructFS repo carries
  `isotope/spec/00..08` (blocks, assemblies, namespaces, system paths,
  lifecycle, protocol) and examples, but `packages/` contains no Isotope
  runtime crates. `structfs-sys` provides the `/sys/*` OS-primitive stores
  (env, time, random, fs, proc) a Block-hosted program needs.
- **ox already has the pre-Isotope Block harness.** `ox-runtime` is a
  wasmtime host: guests import `store_read` / `store_write` / `store_result`
  from module `"ox"`, export `run() -> i32`, and see the world only through a
  `HostStore` middleware over a StructFS backend. `ox-wasm` (agent.wasm) is
  the existing guest built this way. This is exactly a Block: single-threaded,
  isolated, StructFS-client-only.
- **The gate design doc already anticipates this.** `docs/design/ox-gate.md`
  names "Isolated Block — in a full Isotope runtime, gate runs in its own
  wasm" and resolves the sync/async tension via Isotope's blocking reads.

Therefore **"Isotope native" today means:** structure the gateway as
spec-conformant Blocks running on the existing `ox-runtime` harness, with
namespaces and wiring laid out per the Isotope spec, so that the same
`.wasm` artifacts and assembly manifest drop into a real Isotope runtime
when one exists. No new runtime is invented here.

## Target architecture

```
┌────────────────────────── ox-gateway (Assembly) ──────────────────────────┐
│                                                                           │
│  host: http-in            wasm: codec           wasm: broker              │
│  ┌──────────────┐   ┌──────────────────┐   ┌───────────────────────┐      │
│  │ axum shell — │──▶│ dialect decode / │──▶│ resolve → dispatch →  │      │
│  │ HTTP ⇄ paths │◀──│ encode, meta,    │◀──│ drain; inflight paths │      │
│  └──────────────┘   │ error envelopes  │   └──────────┬────────────┘      │
│                     └──────────────────┘              │                   │
│                                                       ▼                   │
│  host: http-out (SSE executor as store)    host: backings                 │
│  write HttpRequest → blocking-read events  (config/secret/gate/usage/    │
│                                            traffic/ledger — unchanged)   │
└───────────────────────────────────────────────────────────────────────────┘
```

- **codec Block (wasm).** The pure sans-IO core we already have — both
  dialects' `decode_request` / `encode_response` / `SseEncoder` /
  `ResponseMeta`, extras canonicalization, error envelopes. Reads an inbound
  wire body at `req/{n}/wire`, writes the decoded `CompletionRequest`; reads
  buffered `StreamEvent`s, writes wire frames/response bodies. Zero host
  dependencies beyond the store ABI (it already compiles without tokio).
- **broker Block (wasm).** The per-request dispatch state machine: model →
  role → account → provider → key resolution via `gate/*` and `secret/*`
  reads, upstream body construction, event-drain orchestration, usage +
  traffic record emission. One Block instance per request (Isotope's
  pico-process model); the tokio machinery (`Notify`, `spawn`) does not
  cross into wasm — blocking substrate reads replace it, with the host
  parking the read exactly as `gateway/completions/outstanding/{n}/events/
  from/{s}` parks today.
- **http-in (native host store).** The axum shell reduced to a protocol
  adapter: HTTP request ⇄ path writes, SSE response ⇄ blocking path reads.
  No gateway logic — per Isotope's "Blocks wrap protocols; the transport is
  StructFS" principle, HTTP is an edge concern.
- **http-out (native host store).** The `SseHttpExecutor` behind a mount:
  write an `HttpRequest` to `upstream/`, get a handle, blocking-read
  `events/from/{s}`. reqwest stays host-side; the broker Block never sees a
  socket.
- **backings (native, unchanged).** config/secret/gate/usage/traffic mounts
  and the conversation-ledger sink are already substrate stores; they wire
  into Block namespaces as-is.

## Execution model changes

| Today (native) | Isotope-native |
|---|---|
| tokio task per request; `Notify` wakeups | one single-threaded Block instance per request; blocking reads |
| axum handlers call codecs in-process | http-in writes wire bodies to paths; codec Block transforms |
| dispatch holds `Arc<Inflight>` in memory | inflight buffer lives at substrate paths served by the host |
| reqwest inside dispatch | http-out store; dispatch writes a request record |

Streaming latency shape is preserved: the drain loop is already
batch-oriented (`events/from/{n}` returns everything buffered), so the
sync guest ABI costs one host call per batch, not per token.

## Phases (each independently shippable, gateway stays green throughout)

1. **Codec Block.** Extract the codec core into a `no_std`-friendly guest
   crate (`ox-gateway-codec-wasm`) built like agent.wasm; host routes call it
   through `ox-runtime`. Proves the ABI round-trip on the pure logic with
   golden tests asserting byte-identical wire output vs the native codecs.
2. **http-out store.** Mount the SSE executor as `upstream/`; dispatch writes
   request records instead of holding the executor. Native still, but the
   broker's last direct I/O dependency is gone.
3. **Broker Block.** Port dispatch to the guest ABI, block-per-request;
   inflight state moves fully into host-served paths. The native dispatch
   path remains behind a flag until parity tests pass, then is removed.
4. **http-in adapter.** Strip route logic into the codec Block; axum becomes
   the dumb HTTP⇄path edge.
5. **Assembly manifest.** `gateway.assembly.yaml` per Isotope spec 02/08
   (blocks, public block, wiring); a thin loader maps it onto broker mounts
   and namespace-scoped `ClientHandle`s so the manifest — not code — defines
   the wiring.

All five phases shipped (phase 5: 2026-08-22). The Block backings route
every guest read/write through the manifest's wiring table and refuse
undeclared paths, so the manifest is enforced, not documentation;
`OX_GATEWAY_ASSEMBLY` points the loader at an alternate manifest.

The Blocks are the only path: the native dialect routes, native dispatch
task, and the `OX_GATEWAY_WASM_*` opt-in flags were removed once the
wasm path had run clean in production. `CompletionBrokerStore` requires
a Block runner; ox-gate holds only substrate mechanics, and completion
logic exists solely in ox-gateway-wasm.

## Out of scope

- Implementing an Isotope runtime (scheduler, lifecycle, `/iso/*` system
  paths beyond what `structfs-sys` provides). The Assembly loader in phase 5
  is a wiring interpreter, not a runtime.
- Wasm-compiling the HTTP edges. WASI-http could later replace http-in/out;
  the Block boundary is designed so that swap needs no Block changes.
- The dashboard/stats routes stay native in http-in (they are host-edge
  reads of substrate paths, not gateway logic).

## Decisions (settled 2026-08-22)

1. **Block granularity: pooled long-lived instances, scaled with load.**
   Each Block instance remains single-threaded and handles exactly one
   request at a time — no internal multiplexing, so the Isotope
   single-threaded discipline holds — but instances are checked out of a
   pool and reused rather than instantiated per request. The pool grows
   under demand up to a cap and shrinks when idle.
2. **Guest ABI: the existing 3-import sync ABI** (`store_read` /
   `store_write` / `store_result` + `run()`), unchanged from agent.wasm.
   The spec-07 server half (Blocks serving stores) becomes its own later
   phase once assembly wiring needs it.

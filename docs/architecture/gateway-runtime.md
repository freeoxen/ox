# Gateway runtime

The gateway uses published Featherweight 0.2.0 for core Wasm execution and
assembly namespaces. StructFS dependencies are pinned to the coordinated 0.2.0
release; the workspace requires Rust 1.96. The conversation agent runtime in
`ox-runtime` remains a separate implementation.

## Guest and capabilities

`crates/ox-gateway/build.rs` builds `ox-gateway-wasm` and embeds its Wasm bytes.
The guest uses `featherweight_guest::sdk`, exports its manifest and `run`, and
reads its mode from the runtime-provided `config` path. Codec, completion broker,
wire translation, and telemetry modes retain Ox's application protocols.

`crates/ox-gateway/src/assembly.rs` parses the gateway manifest with upstream
`AssemblyDef` and adds startup validation for malformed configuration sections.
The host binds the selected block's imports to Ox broker paths. For each run,
`broker_block.rs` constructs the corresponding Featherweight assembly namespace;
guest reads and writes pass through that namespace to the bound stores.

`codec_block.rs` prepares the embedded module once. A persistent Tokio executor
keeps its engine's epoch ticker alive across individual caller runtimes and
provides a place for cleanup to continue after an HTTP caller disconnects.
Each run reserves an engine session and holds that capacity through cleanup.

## Effects and ownership

Ox retains its provider executors, completion protocols, subscriptions, and
persistence. Featherweight does not replace these application contracts.
`BrokerImport` adapts detached StructFS operations to `ox_broker::ClientHandle`.
Allocating imports use StructFS service ownership to register cleanup before
delivering an outstanding handle to the guest.

The broker's `write_owned` path retains the reply to an accepted write without
applying the ordinary client reply timeout. This matters during cancellation:
discarding a late allocation reply would lose the path needed for cleanup.
HTTP routes use `InflightGc::open` for the same allocation-before-delivery race.
Response streams capture their guard before their first poll.
Upstream handle deletion aborts and joins its HTTP producer, then wakes parked
readers. The broker drops abandoned asynchronous reads while accepted writes
continue to completion.

The host inspects terminal guest outcomes and explicitly shuts down the
instance and provider owner. An incomplete shutdown is reported as an error;
the request supervisor retains ownership while cleanup continues. Completion,
wire, and telemetry stores turn runner failures into terminal consumer state.

## Migration evidence

See the [verified migration plan](../superpowers/plans/2026-09-13-structfs-featherweight-migration.md)
for checks and implementation status, and the
[maintainer feedback document](structfs-featherweight-feedback.md) for upstream
requests and downstream compatibility decisions.

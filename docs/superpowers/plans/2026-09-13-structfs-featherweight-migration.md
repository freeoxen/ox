# StructFS 0.2 and Featherweight gateway migration

## Scope

Use published StructFS 0.2 crates across the workspace, replace duplicated store
combinators, and run gateway guests with Featherweight's standard binding,
assembly namespaces and asynchronous execution. Preserve Ox application behavior
and existing persistence formats. Keep actionable upstream feedback in
`docs/architecture/structfs-featherweight-feedback.md` as issues are encountered.
The conversation agent runtime and application-specific effect/subscription
machinery remain separate from this gateway migration.

## Prerequisites

- [x] **P1. The workspace uses git StructFS, with an older lock revision.**
  Verified with `rg -n 'structfs|80a613' Cargo.toml Cargo.lock`:
  `Cargo.toml:122`, `Cargo.lock:6476`. Published 0.2.0 availability verified by
  reading the Cargo sparse registry for core, serde, http, service, handles,
  state, profiles, Featherweight runtime and guest on September 13.
- [x] **P2. Gateway guests use an Ox-specific ABI.**
  `sed -n '1,90p' crates/ox-gateway-wasm/src/lib.rs` verifies module `ox` and
  three imports at `crates/ox-gateway-wasm/src/lib.rs:22`; `run` at line 69.
  `rg -n 'manifest|block_alloc' ../structfs/featherweight/runtime/src/core_wasm.rs`
  verifies the standard exports at upstream line 5. A guest port is required.
- [x] **P3. Gateway execution uses the agent runtime and blocking bridge.**
  `cat crates/ox-gateway/src/codec_block.rs` verifies AgentModule at line 13;
  `sed -n '1,200p' crates/ox-gateway/src/broker_block.rs` verifies block_on at
  line 72 and run_broker at line 106. Wire/telemetry stores spawn blocking
  runners (`rg -n spawn_blocking crates/ox-gateway/src/*.rs`, wire_store:281,
  telemetry_store:218). Runner lifecycle must migrate with the host bridge.
- [x] **P4. Upstream provides the replacement combinators and runtime APIs.**
  `rg -n 'pub struct|pub fn' ../structfs/packages/core-store/src/combinators.rs`
  verifies ReadOnly:13, Cascade:52, Masked:153. Masked uses PathPattern rather
  than string-prefix matching. `cat ../structfs/featherweight/runtime/src/lib.rs`
  verifies CoreWasmEngine, AssemblyDef, Runtime and async_host_store exports at
  lines 100–114. Adapt call sites and test semantic differences.
- [x] **P5. Value conversion has incompatible contracts.**
  `rg -n 'Unsigned|pub fn value_to_json' ../structfs/packages/core-store/src/value.rs ../structfs/packages/serde-store/src/convert.rs`
  verifies Unsigned at value.rs:31 and fallible conversion at convert.rs:8.
  `cat ../structfs/docs/migration-0.2.md` documents strict typed conversion and
  null/absence. Rust 1.96 is installed (`rustc --version`).
- [x] **P6. Persistence remains owned by Ox.**
  Read data-model.md, life-of-a-log-entry.md and save-and-restore.md first.
  `rg -n 'pub fn append|fn commit|save_config_snapshot|with_durability' crates/ox-kernel/src/log.rs crates/ox-inbox/src/snapshot.rs crates/ox-executor/src/thread_registry.rs`
  verifies commit-before-visibility at log.rs:279, config snapshot at
  snapshot.rs:50, post-restore durability installation at thread_registry.rs:404.
  Migration must preserve these contracts and exercise their existing tests.

All prerequisites are verified. If registry resolution or ABI behavior contradicts
these checks, stop the affected task, record the reproducer, and reassess scope;
allow roughly 1–2 days for an upstream fix rather than silently substituting a
different contract.

## Execution

Execute with one implementation sub-agent at a time; the parent reviews each
task and maintains feedback/verification evidence.

1. [x] Upgrade to registry StructFS 0.2, raise declared MSRV, adapt fallible
   codecs and unsigned values throughout the workspace, preserve storage/wire
   compatibility, replace duplicated combinators, and run focused tests.
2. [x] Port gateway guest and host to Featherweight, use upstream assembly
   parsing/wiring, replace blocking guest runners with async execution and
   explicit cancellation/cleanup ownership. Preserve capability denial and
   completion/wire/stats behavior; run gateway parity and disconnect tests.
3. [x] Review integration and remaining bespoke infrastructure, add necessary
   boundary regressions, update architecture and feedback documents, format and
   run `./scripts/quality_gates.sh` plus relevant remote/durability checks.

### Additional transport prerequisite

- [x] **T1. New upstream error categories need explicit transport mapping.**
  `sed -n '74,143p' ../structfs/packages/core-store/src/error.rs` verifies
  NotFound, Conflict, Overloaded, DeadlineExceeded and ResourceLimit variants.
  `sed -n '219,245p' crates/ox-structfs-transport/src/server.rs` and
  `sed -n '62,77p' crates/ox-structfs-transport/src/client.rs` verify that the
  default server mapping and trait client adapter otherwise flatten them into
  generic Store errors. Preserve categories already represented in wire v1,
  with a carrier round-trip test; do not add new wire discriminants. If variant
  contracts differ in the registry artifact, defer this mapping pending a
  verified contract (roughly half a day).

## Validation record

- `cargo test -p ox-kernel snapshot --offline`: 8 passed, including a golden
  pre-migration JSON hash and rejection of non-JSON snapshot values.
- `cargo test -p ox-store-util --offline`: 28 passed, including rejection of an
  invalid append without changing the existing JSONL file.
- `cargo test -p ox-structfs-transport --test conformance --offline`: 18 passed,
  including the original committed wire fixtures and explicit unsigned-value
  behavior within the closed v1 integer range.
- Registry-only Serde probe reproduced lost enum diagnostics (SF-005) and the
  strict integer-to-float conversion boundary. Scratch work lives in `local/`
  per user instruction; it is not a tracked deliverable.
- Task 1: `cargo check --workspace --all-targets --offline --message-format short`
  passed (`local/structfs-check.log`); 577 base unit tests passed
  (`local/structfs-base-tests.log`). Four usage boundary tests and three TOML
  boundary tests passed. Formatting and diff whitespace checks passed.
- `cargo check -p ox-web --target wasm32-unknown-unknown --offline` passed
  after restricting StructFS HTTP to native dependencies and fixing native
  module/reexport cfg guards in ox-gate. The original Tokio feature failure
  is recorded as SF-007.
- `cargo test -p ox-structfs-transport --test typed_errors --offline`: passed.
  Six existing wire error categories survive both reads and writes; a returned
  NotFound path remains in the caller's namespace.
- First `./scripts/quality_gates.sh` run: 2,323 Rust tests passed across 67
  test binaries (`target/coverage/rust_summary.txt`); UI checks and Wasm build
  passed. Overall 9/13 gates passed. Remaining failures: MSRV-enabled Clippy
  lints, a false-positive parse-fallback match, gateway coverage 67.8% versus
  70%, and the uninstrumented gateway guest being included in native coverage.
  Mechanical Clippy fixes and the parse match are corrected. The guest now
  follows the existing cross-compiled-crate coverage policy; its behavior is
  exercised by gateway parity tests. Native gateway coverage remains enforced.
  A final gate run is required after lifecycle regressions are complete.
- Task 2: `cargo test -p ox-gateway --offline` passed all gateway targets,
  including capability denial, aliased provider handles, cancellation before
  late allocation delivery, strict assembly validation, and native buffered/SSE
  routes. `cargo test -p ox-gate upstream_store --offline`: 4 passed, including
  a regression observing actual producer drop and read completion after GC.
- Final `./scripts/fmt.sh` and `./scripts/quality_gates.sh`: passed, **13/13
  gates** (`local/structfs-featherweight-quality-gates.log`). The final coverage
  run passed **2,332 Rust tests**, including remote, worker, and durability
  integration suites. Rust region coverage is 80.10%; every enforced crate and
  TypeScript threshold passed. Native/browser Clippy, Wasm build, UI checks,
  tests and production UI build passed. `git diff --check` passed.

## Outcome and remaining scope

The gateway now executes on Featherweight with its standard guest ABI and
assembly namespaces. Ox's local combinator implementations and cancellation
token are replaced by upstream types. The separate conversation agent runtime,
application providers, subscriptions, and durability ownership remain in Ox as
scoped above. No local StructFS patches or unpublished crate dependencies are
required. Cargo.lock remains untracked under the repository's existing policy.
See `docs/architecture/gateway-runtime.md` for the resulting architecture and
`docs/architecture/structfs-featherweight-feedback.md` for the running maintainer
feedback draft. No feedback has been sent externally.

## Gateway implementation constraints verified during task 1

- [x] **G1. A cancelled broker caller can lose an accepted write result.**
  `sed -n '245,285p' crates/ox-broker/src/client.rs` verifies a timeout around
  the direct write reply. `cat crates/ox-broker/src/server.rs` verifies that
  asynchronous writes continue in a separate task (line 146). Host cleanup
  must retain accepted open results, including cancellation before delivery.
- [x] **G2. Upstream supports supervising those open operations.**
  `sed -n '385,452p' ../structfs/packages/service/src/owner.rs` verifies
  `OwnerHandle::open`: registers cleanup before opening and observes completion
  independently of result delivery. The adapter must not put a reply-discarding
  timeout inside this operation.
- [x] **G3. Shutdown is explicit and reports remaining work.**
  `sed -n '870,955p' ../structfs/featherweight/runtime/src/runtime.rs` verifies
  provider_owner:872 and shutdown:896. Keep the supervisor/runtime/session
  reservation alive until complete cleanup. Guest interruption cannot be the
  only cleanup mechanism.
- [x] **G4. Guest terminal outcomes are inspectable.**
  `rg -n 'wait_public_terminal|public_cell' ../structfs/featherweight/runtime/src/runtime.rs`
  verifies lines 799 and 819; `rg -n 'last_error|exit_code' ../structfs/featherweight/runtime/src/block.rs`
  verifies diagnostics at line 426 and exit status at line 745. Host runner
  failures must unblock the completion/wire/telemetry consumer, rather than
  merely log an error and leave its read parked.
- [x] **G5. Existing upstream GC does not join its producer.**
  `sed -n '100,270p' crates/ox-gate/src/upstream_store.rs` verified map removal
  at line 156 and an independently spawned producer at line 215, with no
  cancellation or join between them at migration review. The adapter cannot
  claim cleanup completion until the provider stops and parked reads wake.
  Add a regression observing producer cancellation, not just handle removal.
  If joining is incompatible with the executor contract, this cleanup portion
  needs an explicit executor adapter (estimate one day).

If any G prerequisite fails in registry artifacts, the lifecycle portion is
blocked pending a verified adapter or upstream fix (estimate 1–2 days).

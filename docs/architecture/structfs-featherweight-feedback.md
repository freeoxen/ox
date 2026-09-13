# StructFS / Featherweight migration feedback

Running downstream integration notes from Ox, started September 13, 2026.
Target: published 0.2.0 crates. This document is a draft for maintainers; nothing
has been sent upstream. Entries distinguish confirmed problems from requests
and expected migration work. Add reproducers and resolutions as work proceeds.

The sendable draft is [the maintainer letter](structfs-featherweight-maintainer-letter.md).
The [cleanup audit](structfs-ergonomics-audit.md) records what we removed and
which remaining helpers encode compatibility or application policy.

## SF-001 — Published release still described as unpublished

- Type: documentation request. Status: confirmed, open.
- Evidence: Cargo sparse registry has non-yanked 0.2.0 entries for StructFS and
  Featherweight, while the source checkout at `81b37c5` labels CHANGELOG.md and
  docs/migration-0.2.md as an unpublished candidate.
- Impact: downstream readiness assessment initially appears blocked despite
  available artifacts.
- Request: update release status after successful publication, with a link to
  exact release validation and any remaining platform limitations.

## SF-002 — Combinator migration needs a compatibility table

- Type: migration documentation request. Status: confirmed, open.
- Evidence: core-store/src/combinators.rs uses private Cascade fields and
  component-wise PathPattern matching for Masked. Ox's local wrappers expose
  Cascade fields and use string-prefix masking.
- Impact: mechanically changing imports changes API and matching behavior.
- Request: document accessor replacements and exact/subtree pattern examples;
  include ancestor-map redaction expectations. These are intentional contract
  differences, not yet an upstream correctness bug.

## Integration findings

To be extended with build, runtime, codec and cleanup observations during the
migration. Production providers, persistence, and Ox application semantics stay
owned by Ox; their absence upstream is not itself a defect.

## SF-003 — Assembly validation silently accepts malformed sections

- Type: validation request. Status: reproduced against published crates, open.
- Evidence: `featherweight/runtime/src/assembly.rs:258` reads `config` only
  when it is a map and does not validate its block names. The same parser
  correctly rejects non-array wiring. Ox's previous parser rejects malformed
  typed sections and config entries naming an unknown block.
- Reproducer: parse an otherwise valid assembly with
  `config: {ghost: {}}`, or `config: false`. Both succeed upstream and are
  rejected by Ox's additional validation. Covered by
  `gateway_rejects_config_maps_that_upstream_silently_accepts` in
  `crates/ox-gateway/src/assembly.rs`.
- Impact: moving to the upstream parser can hide configuration typos that Ox
  previously caught at startup.
- Request: reject wrong section types and unknown config/failure block names;
  distinguish ignored extension metadata from invalid standard fields.

## FW-001 — Please document adoption over existing effectful services

- Type: integration example request. Status: open.
- Evidence: Ox's broker dispatches writes separately from their awaiting caller;
  a client timeout can discard an accepted handle-open result. Featherweight's
  `OwnerHandle::open` (`packages/service/src/owner.rs:389`) retains an opener
  until completion, but cannot recover a result already discarded by an adapter.
- Impact: simply wrapping an existing timeout-based client in a host service
  is insufficient to guarantee cleanup when a guest disconnects during open.
- Request: include an example for migrating a pre-existing broker: retain
  accepted open results, register cleanup before delivery, keep cleanup writes
  independent of guest cancellation, and join cleanup after traps/timeouts.
  The example should include an open whose result arrives after cancellation.
- Downstream resolution: `crates/ox-gateway/tests/owned_handle.rs` verifies
  cancellation before a delayed allocation reply, including expiry of the
  ordinary broker timeout. The bridge retains the reply and cleans up the
  allocation. Separate provider tests observe the actual HTTP stream dropping
  before GC acknowledges completion. These are Ox integration responsibilities,
  not claims that `OwnerHandle::open` is broken.

## SF-004 — Include Path iteration changes in the migration guide

- Type: migration documentation request. Status: reproduced by compiler, open.
- Evidence: `cargo check -p ox-gateway-wasm --target wasm32-unknown-unknown`
  against registry 0.2.0 rejects Ox/Horns callers that expected `Path::iter()`
  to yield `&String`. It now yields `&str` (horns-core/src/path_serde.rs:14,31;
  install.rs:536; dispatch.rs:227 at migration start).
- Impact: unrelated UI/path-serialization crates break during the value-layer
  upgrade. This was not listed in docs/migration-0.2.md in the inspected source.
- Request: document `Vec<&String>` → `Vec<&str>` and
  `.iter().cloned()` → `.iter().map(str::to_owned)` where owned components
  are needed. Avoid suggesting changes to the serialized path representation.

## SF-005 — Preserve bounded Serde error context

- Type: diagnostics request. Status: reproduced against published crates, open.
- Evidence: published `structfs-serde-store` 0.2.0 `src/limits.rs:48–56`
  implements Serde's `Error::custom` by discarding the diagnostic argument and
  returning `TypeMismatch`. `Failure::core` then reports only that category.
- Impact: schema validation errors, missing fields, and unknown enum variants
  can lose the information needed to repair a request or persisted record.
- Reproducer: deserialize an enum from an unknown string via `from_value`;
  compare the diagnostic with `serde_json::from_value` for the same enum.
  `cargo run --manifest-path local/structfs-migration-probe/Cargo.toml --offline`
  prints `decode failed for format application/x-structfs-value: TypeMismatch`
  versus `unknown variant \`Misspelled\`, expected \`Streaming\``. The scratch
  probe is kept locally; the minimal example below can be sent upstream:

  ```rust
  #[derive(Debug, serde::Deserialize)]
  enum Mode { Streaming }
  let json = serde_json::json!("Misspelled");
  let value = structfs_serde_store::json_to_value(json.clone());
  println!("{}", structfs_serde_store::from_value::<Mode>(value).unwrap_err());
  println!("{}", serde_json::from_value::<Mode>(json).unwrap_err());
  ```
- Request: preserve the Serde diagnostic within `max_diagnostic_bytes`; retain
  the structured error category separately. Add nested field/index context if
  feasible, so strict-conversion migration failures identify the offending value.

## SF-006 — Path macro reexports require a direct dependency

- Type: macro ergonomics / migration request. Status: reproduced by downstream
  workspace compilation, open.
- Evidence: `packages/path-macro/src/lib.rs:104` expands to the absolute crate
  path `::structfs_core_store::Path`. Ox-core previously invoked the public
  `ox_kernel::path!("history/append")` reexport without a direct core-store
  dependency. That invocation stopped compiling after the upgrade.
- Downstream workaround: use `ox_kernel::Path::parse` for the single affected
  test; no serialized path changes.
- Request: support renamed dependencies and reexports through a hygienic public
  wrapper, or clearly document the required direct dependency at every call site.

## Resolved downstream compatibility decisions

- ReadOnly, Cascade and Masked now use upstream implementations. Existing local
  wrapper tests pass; Masked has no production callers in Ox.
- JSON snapshots retain their existing hash bytes for representable values.
  Bytes and non-finite floats fail explicitly before persistence writes.
- Usage ledger ingestion keeps JSON's integer syntax for floating-point costs
  through an explicit JSON decoding boundary; typed StructFS conversion stays
  strict elsewhere. Existing records with a cost of `0` remain readable.
- Remote wire v1 retains its original signed integer range and canonical bytes.
  Small `Unsigned` values normalize; values above `i64::MAX` are rejected.
  A future protocol upgrade can expand that range deliberately.

## SF-007 — HTTP/handles pull a native Tokio runtime into Wasm builds

- Type: feature/platform request. Status: reproduced, downstream workaround.
- Reproducer: `cargo check -p ox-web --target wasm32-unknown-unknown --offline`
  failed in Tokio with `Only features sync,macros,io-util,rt,time are supported
  on wasm.` `cargo tree -p ox-web --target wasm32-unknown-unknown -e features
  -i tokio --offline` traced `rt-multi-thread` to published `structfs-http`
  and `structfs-handles` 0.2.0.
- Cause: unconditional dependencies inherit workspace Tokio features including
  `rt-multi-thread`. Disabling default crate features cannot subtract those
  explicitly enabled dependency features.
- Downstream fix: Ox uses StructFS HTTP only in native modules, so its HTTP
  dependency now lives in the matching native target dependency section.
- Request: gate native executor features by target or provide a portable
  types/primitives feature. Document the supported targets for HTTP and handles;
  the portable guest SDK alone does not establish portability of these crates.

## SF-008 — Complete the detached-store ergonomics

- Type: API request. Status: confirmed by published source inspection, open.
- Evidence: `structfs-serde-store-0.2.0/src/async_typed.rs:44,106` extends
  borrowing AsyncReader/AsyncWriter; no detached typed extension is exported.
  `structfs-core-store-0.2.0/src/async_traits.rs:216–258` implements detached
  forwarding for references, boxes and Shared, but not ReadOnly, Rooted,
  Cascade or Masked. Their synchronous versions live in `combinators.rs`.
- Impact: the broker can delete its private detached traits, yet still needs
  typed read/write helpers and manually scoped async adapters. Borrowing a
  store until a parked read finishes is not an equivalent replacement.
- Request: DetachedTypedReader/Writer helpers and detached-capable wrappers.
  Construction must return a Send + 'static future without retaining the
  store borrow. Specify codec/raw-record handling and result-path translation.
  Read-only enforcement must reject before starting the underlying write.

## SF-009 — Provide an explicit component-array Path Serde adapter

- Type: compatibility/ergonomics request. Status: confirmed, open.
- Evidence: upstream `core-store/src/serde_impls.rs:23–33` serializes Path as
  a string. Existing Ox/Horns records encode `["settings", "accounts"]`.
  `horns-core/src/path_serde.rs` now holds the single shared compatibility
  adapter, with a golden-shape and invalid-component regression.
- Request: an opt-in `serde(with = ...)` adapter for Path and Option<Path>
  component arrays, with validation. Keep the new default representation;
  document the two forms so consumers can migrate deliberately.

## SF-010 — Make suffix-pattern middle length configurable

- Type: expressiveness request. Status: reproduced, open.
- Evidence: `PathPattern::prefix_suffix(path!("accounts"), path!("provider"))`
  matches `accounts/provider` in published 0.2.0. Ox subscription patterns
  require at least one component in between, to select account instances.
  Existing regressions in `horns-core/src/subscription.rs` enforce this.
- Impact: aliasing the upstream enum would broaden subscriptions and change
  their serialized shape. Rebuilding an owned pattern on every match would
  introduce path cloning into the current allocation-free matching path.
- Request: configurable minimum middle length (zero versus one is sufficient
  for this use case), plus a documented stable/opt-in Serde representation.
  Keep the current zero-middle behavior as the default.

## FW-002 — Ship a complete embedding lifecycle example

- Type: embedding API/documentation request. Status: open.
- Evidence: `ox-gateway/src/codec_block.rs` retains the engine's executor and
  per-run session reservation; `broker_block.rs` coordinates guest outcome,
  cancellation, instance shutdown and a separate provider owner. Incomplete
  cleanup reports to the caller while a retained task keeps joining work.
- Request: a small embedding example covering prepared-module reuse, explicit
  capacity reservation, external broker imports, cancellation during an open,
  nonzero guest exit, and incomplete cleanup. Demonstrate that engine ticking,
  provider ownership and reservations survive caller-runtime teardown. A helper
  API would be useful if it can preserve these explicit lifecycle decisions.
- This does not claim that the existing APIs cannot support embedding; Ox's
  gateway uses them successfully. It identifies the integration code we still
  have to assemble and maintain.

## SF-011 — Enforce the documented path-macro expression type

- Type: API contract bug. Status: reproduced in a release build, open.
- Evidence: `structfs-path-macro-0.2.0/src/lib.rs:88–96` calls
  `expression.validated_str()` without constraining its receiver to PathComponent.
  The generated `Path::from_validated_components` validates only in debug builds
  (`core-store/src/path.rs:148–152`). A different type with a matching method
  compiles and can produce a path that `Path::parse` rejects in release mode.
- Reproducer: `cargo run --manifest-path local/structfs-migration-probe/Cargo.toml
  --bin macro_contract --release --offline` prints
  `macro accepted non-PathComponent: safe/bad-name` and parser acceptance `false`
  (`local/structfs-macro-contract.log`).
- Request: generate an explicitly typed borrow or use a helper constrained to
  PathComponent, preserving component reuse; add a compile-fail case for an
  unrelated type exposing `validated_str`. Bare String/str rejection alone does
  not establish the documented nominal type contract.
- Ox action: current migrated callers use validated components. Keep the upstream
  macro and report this discrepancy; do not recreate a private macro merely to
  change behavior for caller types we do not use.

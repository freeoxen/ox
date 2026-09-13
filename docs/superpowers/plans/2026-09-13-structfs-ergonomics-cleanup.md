# StructFS 0.2 ergonomics cleanup

## Scope

Extend the completed dependency/gateway migration by removing redundant
infrastructure supported by published 0.2.0. Preserve current application
protocols and persisted shapes. Audit remaining candidates against source and
write a Markdown letter with actionable maintainer requests. Scratch work stays
in `local/`; no message is sent externally.

## Prerequisites

- [x] **P1. Upstream path construction covers the local macro's argument forms.**
  `cat crates/ox-path/src/lib.rs` verifies local literal/integer/validated
  expression branches at lines 48–85. `sed -n '1,120p'
  ../structfs/packages/path-macro/src/lib.rs` verifies matching upstream forms
  at lines 42–108, plus slash-separated literals. Core reexports it at
  `../structfs/packages/core-store/src/lib.rs:77`. Compile against the registry
  version; if expansion differs, defer removal pending a compatibility fix
  (roughly half a day).
- [x] **P2. Broker async traits return detached futures.**
  `cat crates/ox-broker/src/async_store.rs` verifies local reader/writer at
  lines 11–20; these futures do not borrow the store. Upstream's borrowing
  AsyncReader is not interchangeable (`sed -n '1,120p'
  ../structfs/packages/core-store/src/async_traits.rs`, line 37). Evaluate the
  separately exported DetachedReader/DetachedWriter before changing call sites.
  If detached bounds or lifetimes differ, keep the adapter and document the
  contract (roughly half a day to assess).
- [x] **P3. LocalConfig has observable flat-map semantics.**
  `sed -n '1,190p' crates/ox-store-util/src/local_config.rs` verifies set's
  direct insertion at line 24, overlapping-leaf errors at line 56, and null
  deletion at line 131. `sed -n '1,120p'
  ../structfs/packages/core-store/src/memory_store.rs` verifies a nested Value
  representation and deletion/replacement through Value methods. Do not alias
  these stores without contract tests. Any mismatch limits replacement to
  proven-compatible consumers; broader policy changes require separate work
  (roughly one day).

## Execution

Additional verified seams for task 2:

- [x] **P4. Upstream detached traits match the broker's future ownership.**
  `sed -n '180,245p' ../structfs/packages/core-store/src/async_traits.rs`
  verifies DetachedFuture:186 and DetachedReader/Writer:199/207. Methods are
  renamed `read_detached`/`write_detached`; store lifetime constraints belong on
  spawned broker servers. A byte comparison against Cargo registry source
  `structfs-core-store-0.2.0/src/async_traits.rs` verified identical contents.
  If a caller relies on borrowing, retain its borrowing adapter (half-day audit).
- [x] **P5. Shared MemoryStore supports codec job/result leaves.**
  `sed -n '1,160p' crates/ox-gateway/src/codec_block.rs` verifies JobStore:57
  and job/result access:109/138. `sed -n '105,148p'
  ../structfs/packages/core-store/src/combinators.rs` verifies Shared's cloned
  mutex ownership and Reader/Writer forwarding; MemoryStore source verified
  in P3. Both files match registry artifacts byte-for-byte. Preserve codec
  parity; if guest accesses depend on flat lookup semantics, retain that store
  pending adaptation (half a day).
- [x] **P6. Path Serde adapters are duplicated but preserve a legacy shape.**
  `cat crates/ox-types/src/path_serde.rs crates/horns-core/src/path_serde.rs`
  verifies identical component-array serialization at lines 13–45 in each.
  `sed -n '1,45p' ../structfs/packages/core-store/src/serde_impls.rs`
  verifies upstream string serialization at line 23 (identical to registry).
  Consolidate one compatibility adapter; direct derive would change wire and
  persistence shapes. A schema migration is separate work, not assumed here
  (at least one day).
  `sed -n '27,56p' crates/ox-cli/src/settings/commands/navigation.rs` and
  `sed -n '133,153p' crates/ox-gate/src/subscriptions/util.rs` also verify
  identical component-array Value encoders at lines 30 and 137. Consolidate
  these through the shared adapter without changing their existing exports.
- [x] **P7. Local subscription suffix patterns require a nonempty middle.**
  `sed -n '45,105p' crates/horns-core/src/subscription.rs` verifies the
  serialized enum and length guard at lines 55/85. Upstream
  `core-store/src/path_pattern.rs:56` permits an empty middle. Retain this enum
  and matching: rebuilding an owned upstream pattern per match adds path cloning
  to the subscription hot path, while aliasing changes semantics and Serde shape.
  A configurable middle length and compatible Serde adapter would enable removal
  (half-day follow-up once available).
- [x] **P8. Broker mount lookup is a longest-prefix search over channels.**
  `sed -n '1,145p' crates/ox-broker/src/broker.rs` verifies sorted insertion:45,
  exact unmount:53, first-match lookup:65 and shutdown clearing:139. Stable sort
  preserves the first registered mount when prefixes duplicate. Upstream
  `core-store/src/path_trie.rs:80,86,146` provides insert, exact removal and
  deepest-ancestor lookup (byte-identical to registry). Store a vector per node
  to retain duplicate precedence and add regression coverage. If lookup differs,
  retain existing routing pending an adapter (half a day).

Use one implementation sub-agent at a time; parent audits adjacent candidates,
reviews changes, maintains evidence, and writes the letter.

1. [x] Replace oxpath call sites with upstream path construction and remove the
   local proc-macro crate/dependencies. Verify compile-time argument safety and
   native/browser compilation.
2. [x] Audit and migrate other proven duplicates: detached traits, path serde,
   routing, in-memory stores and wrappers. Record each retained contract and
   why a direct replacement is unsuitable; add prerequisites before changes.
3. [x] Write maintainer letter, update feedback/architecture records, format and
   run canonical quality gates.


## Task 1 validation

- Published `structfs-path-macro-0.2.0/src/lib.rs:42–108` was read from the
  Cargo registry cache; runtime expressions still call `validated_str()`.
- `cargo check --workspace --all-targets --offline --message-format short`
  passed (`local/path-cleanup-check.log`).
- `cargo check -p ox-web -p ox-wasm -p ox-gateway-wasm --target
  wasm32-unknown-unknown --offline --message-format short` passed
  (`local/path-cleanup-wasm-check.log`).
- `cargo test -p ox-kernel --lib path_component::tests --offline`: 15 passed,
  including integer literals, reusable validated components, Unicode and roots.
- `cargo test -p ox-kernel --test path_compile_tests --offline`: five checked
  compile-fail fixtures passed, including unvalidated String and &str arguments.
  Upstream intentionally accepts an empty literal as root, so the obsolete local
  empty-literal rejection fixture was replaced with an explicit root assertion.
- `rg -n 'oxpath|ox_path|ox-path' crates Cargo.toml coverage.toml` found no
  remaining live references. Historical plan documents retain historical names.

## Task 2 validation

- Registry-only probe reproduced empty-map, overlapping leaf/child, suffix
  middle-length and Path Serde differences (`local/structfs-ergonomics-probe.log`).
- Detached-trait migration passed `cargo check --workspace --all-targets
  --offline`, browser checks and standalone broker tests. Explicitly enabled
  core-store's async feature in the broker so isolated builds do not depend on
  workspace feature unification.
- `cargo test -p ox-gateway -p ox-broker -p ox-structfs-transport --offline`
  passed. The mounted-client regression constructs an independent write while
  a read is parked, drops the original client, then completes both futures.
  This verifies detached lifetime and concurrency behavior through the broker.
- PathTrie routing retains duplicate precedence, descendant mounts during exact
  unmount, and cleanup of empty transient branches. Codec parity validates
  Shared<MemoryStore> in place of the private job store.
- One component-array Serde adapter and Value encoder now serve both Ox and
  Horns callers; compatibility shapes remain explicit. Remaining candidates and
  their contracts are recorded in `docs/architecture/structfs-ergonomics-audit.md`.
- The release-mode macro probe reproduced SF-011: an unrelated type exposing
  `validated_str` is accepted and can bypass the normal path grammar. The letter
  includes the minimal reproducer; migrated Ox expressions use validated
  components and the bare String/str compile-fail checks remain enforced.

## Final validation

- `./scripts/fmt.sh` completed successfully.
- `./scripts/quality_gates.sh` passed all 13 gates, including native and browser
  Clippy, Rust tests and coverage, wasm build, UI checks/tests and production
  build (`local/structfs-featherweight-quality-gates.log`). The Rust test summary
  reports 2,335 passed tests and 80.17% region coverage.
- The first coverage reports included the deleted ox-path crate because an old
  instrumented object remained in trybuild's separate cache. Verbose
  `cargo llvm-cov report --summary-only -v` identified the object. Obsolete
  artifacts were moved to `local/coverage-stale-ox-path/`; the final canonical
  gate run excludes the deleted crate without changing coverage policy.
- The maintainer letter is drafted in
  `docs/architecture/structfs-featherweight-maintainer-letter.md`; supporting
  reproductions and retained contracts are in the feedback log and cleanup
  audit. No correspondence was sent externally.

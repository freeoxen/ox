# StructFS / Featherweight 0.3 adoption

Use the coordinated published 0.3.0 packages, remove superseded compatibility
code, preserve persisted/wire formats, and record remaining maintainer requests.

## Prerequisites

Ox pre-upgrade evidence is pinned to baseline commit `00561a1fdd857355ea0cee6fbfb7279df3b9820f`.
The corresponding `git show` commands were rechecked after implementation;
upstream references are the inspected 0.3 registry sources, identical to the
referenced sibling files.

- [x] **The seven direct dependencies are published at 0.3.0.**
  `cargo info <crate>@0.3.0` succeeded for core-store, serde-store, service,
  handles, http, featherweight-runtime and featherweight-guest. Current pins:
  `Cargo.toml:101`, `crates/ox-gate/Cargo.toml:42`,
  `crates/ox-gateway-wasm/Cargo.toml:15`. If resolution fails, retain 0.2 pins
  until publication is consistent (external release dependency).
- [x] **Upstream provides the same component-array Serde contract.**
  `nl -ba ../structfs/packages/core-store/src/path_serde.rs` verifies required
  and optional adapters at lines 17/28; `git show 00561a1:crates/horns-core/src/path_serde.rs`
  verifies pre-upgrade adapters at lines 19/31. Registry 0.3 files were compared
  byte-for-byte with the checkout using Python pathlib. If golden shapes or
  invalid-component checks differ, retain the adapter (half-day investigation).
- [x] **Assembly standard-section validation is now upstream.**
  `git show 00561a1:crates/ox-gateway/src/assembly.rs` verifies duplicate validation at
  line 33. `nl -ba ../structfs/featherweight/runtime/src/assembly.rs` and the
  registry byte comparison verify upstream validation (including block fields
  at line 180). Keep rejection tests when deleting the local parser; a mismatch
  requires retaining the affected check (half-day investigation).
- [x] **Detached typed helpers release input/store borrows.**
  `cat ../structfs/packages/serde-store/src/detached_typed.rs` verifies reader
  bounds and raw-record rejection at lines 12/37 and writer conversion before
  effect construction at line 49; registry file matches byte-for-byte.
  `git show 00561a1:crates/ox-broker/src/client.rs` (lines 123–155) verifies the old typed
  facade: raw reads silently return absence. Preserve shared-handle ergonomics,
  explicitly test stricter raw rejection and diagnostics. If callers require
  non-Send outputs, retain their conversion boundary (half-day investigation).
- [x] **Subscription patterns retain a distinct public/persisted enum.**
  `sed -n '45,105p' crates/horns-core/src/subscription.rs` verifies array paths
  and struct-shaped suffix at line 62. `cat
  ../structfs/packages/core-store/src/path_pattern.rs` verifies upstream's
  different representation and owned constructor at lines 44/74. Retain the
  compatibility type unless migration removes work without per-match cloning;
  schema conversion is separate work (at least one day).

- [x] **The stricter macro exposes Ox's duplicate component type.**
  `cargo check --workspace --all-targets --offline --message-format short`
  failed at `crates/ox-kernel/src/run.rs:764`: expected upstream PathComponent.
  `git show 00561a1:crates/ox-kernel/src/path_component.rs` and `sed -n '338,410p'
  ../structfs/packages/core-store/src/path.rs` verify equivalent validation and
  accessors at upstream line 346; its constructor returns PathError instead of
  Ox's wrapped StoreError. Reexport upstream and compile all callers, preserving
  validation tests. If error consumers require wrapping, adapt those boundaries
  (half-day investigation).

## Execution

Use one implementation sub-agent at a time, as prescribed by AGENTS.md.
The parent updates dependencies, audits runtime/features and handles validation
and feedback while each bounded implementation task runs.

1. [x] Upgrade all coordinated pins and remove duplicated path/assembly code.
2. [x] Adopt detached typed access where its contract fits; audit remaining
   composition, pattern, platform and synchronous runtime opportunities.
3. [x] Run formatting and canonical quality gates; update the feedback log and
   sendable maintainer draft with verified resolutions and remaining requests.


## Implementation notes

- Direct and transitive StructFS/Featherweight packages are registry-pinned at
  0.3.0. Cargo also resolved tempfile's existing compatible getrandom dependency
  to 0.4.1; no unrelated package version was deliberately upgraded.
- Broker typed facades preserve the shared async handle and synchronous bridge.
  Async output types require Send + 'static. Both facades now reject raw records
  instead of silently returning absence, and preserve structured codec failures.
- Regenerated agent.wasm and codec_block.wasm with
  `scripts/build-wasm-artifacts.sh`. Independent review checked every provenance
  input and both artifact hashes against current files.
- Final review found no further drop-in 0.3 replacement that removes code while
  retaining current contracts. Suffix matching and the CLI runtime remain
  explicitly tracked in the feedback log.

## Validation

- `./scripts/fmt.sh` passed (`local/structfs-03-fmt.log`). Bun needed access
  outside the sandbox to its temporary directory.
- Native all-target workspace check and the explicit browser/guest check passed
  (`local/structfs-03-check.log`, `local/structfs-03-wasm-check.log`). Final
  canonical gates also compiled the broker changes made after the initial check.
- Broker tests passed: 47 unit tests and one integration test, including shared
  typed facades, structured errors, failed serialization and parked detached reads
  (`local/structfs-03-broker-tests.log`).
- The six path compile-fail fixtures pass. Two expected diagnostics changed
  from missing methods to nominal type mismatch, and the new impostor fixture
  demonstrates the upstream fix. Initial expected-output refresh used
  `TRYBUILD=overwrite`; the final canonical suite verified them without overwrite.
- `scripts/build-wasm-artifacts.sh` rebuilt both guests and provenance
  (`local/structfs-03-artifacts.log`). Browser compilation includes ox-web,
  ox-wasm and ox-gateway-wasm.
- `./scripts/quality_gates.sh` passed **13/13** gates, including **2,342 Rust
  tests**, **80.24% region coverage**, native/browser Clippy, wasm-pack and UI
  checks/tests/build (`local/structfs-03-quality-gates.log`). The initial
  sandboxed attempt failed only on Bun temporary files and gateway loopback
  sockets; the unrestricted rerun passed without bypasses or policy changes.
- `git diff --check` passed. The feedback log and maintainer draft identify
  resolved 0.2 requests, current 0.3 limitations and inspected-but-not-executed
  upstream example coverage. No upstream correspondence was sent.

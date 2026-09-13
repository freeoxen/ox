# CLI and Horns migration against published 0.2.0

Migrate where the upstream implementation preserves useful behavior and removes
work. The user explicitly permits retaining a better Ox implementation and
recording the reason for maintainers. Do not introduce compatibility machinery
solely to count a subsystem as migrated. Use `local/` for probes and logs.

## Prerequisites

Registry paths below are relative to
`/Users/alex/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`.

- [x] **P1. Horns renderer routing is an exact/deepest-ancestor index.**
  `nl -ba crates/horns-core/src/render.rs | sed -n '110,205p'` verifies
  HashMap storage:118, replacing registration:129, exact lookup:134,
  deepest rendering:143, ancestor lookup:158 and strict ascent:170–198.
  Keep AscendRule and public behavior. If parity fails, retain the index
  pending investigation (half a day).
- [x] **P2. Upstream PathTrie supplies the required index operations.**
  `sed -n '75,175p' <registry>/structfs-core-store-0.2.0/src/path_trie.rs`
  verifies replacing insert:80, exact get:108, membership:130 and deepest
  ancestor:146, including root and the unmatched suffix. Strict ascent starts
  at the cursor's parent; root has no strict parent. If those contracts change,
  stop the replacement and revisit P1 (half a day).
- [x] **P3. The current CLI execution seam is synchronous and returns effects.**
  `sed -n '2070,2140p' crates/ox-executor/src/agents.rs` verifies
  run_one_turn:2086, run_with_cancellation:2128, and returned backend/effects
  extraction:2131–2132. `sed -n '20,150p' crates/ox-runtime/src/host_store.rs`
  verifies HostEffects:24 and synchronous effect interception:56/93.
  Replacing this seam needs explicit lifetime, cancellation and accepted-effect
  parity, not a trait rename. If changed, re-audit execution before porting
  (one day).
- [x] **P4. Ox shares compiled modules and exposes per-turn resource policy.**
  `sed -n '80,210p' crates/ox-runtime/src/engine.rs` verifies config:85,
  remote limits:93, compilation at load:151/162 and reusable AgentModule:178.
  `sed -n '203,217p' crates/ox-runtime/src/engine.rs` verifies memory cap
  with trap_on_grow_failure. Current regression tests at :563–669 cover memory,
  fuel, timeout and independent cancellation. A replacement must preserve
  these policies or document why it is declined; if the policy changes,
  re-scope tests and execution migration (one day).
- [x] **P5. Featherweight's two core execution APIs have different contracts.**
  `sed -n '850,889p' <registry>/featherweight-runtime-0.2.0/src/core_wasm.rs`
  verifies synchronous execution rejects prepared artifacts:864 and compiles
  a fresh module:879. `sed -n '980,1010p'` verifies run consumes its store and
  returns only an exit code, using default limits:998. `sed -n '272,335p'`
  verifies prepared memory caps and default grow-failure behavior:326;
  `sed -n '697,727p'` verifies borrowing async stores:708 and fixed 10 ms
  prepared epoch interval:716. Probe the published API before declaring these
  runtime gaps. If a suitable API was missed, reconsider the retention decision
  (half a day).
- [x] **P6. CLI snapshot storage is not a drop-in MemoryStore alias.**
  `sed -n '13,49p' crates/ox-cli/src/settings/snapshot.rs` verifies LocalConfig
  and unrestricted flat insertion:36. `sed -n '345,380p'
  crates/ox-broker/src/client.rs` verifies subtree flattening preserves Null
  leaves. `sed -n '75,110p'
  <registry>/structfs-core-store-0.2.0/src/memory_store.rs` verifies Null writes
  delete and tree descent is validated. Retain current storage unless callers
  are explicitly migrated to the new semantics (one day if needed).

The data-model, life-of-a-log-entry, and save-and-restore architecture documents
were read before this plan. No durability or approval ownership changes are
authorized by an implementation shortcut.

## Tasks

Execute one implementation sub-agent task at a time; parent reviews and probes
adjacent contracts. Create new commits at validated milestones; keep hooks.

1. [x] Replace the Horns renderer index with PathTrie. Preserve exact lookup,
   deepest selection, replacing registration and strict-parent AscendRule.
   Test root behavior and component boundaries as well as existing UI cases.
2. [x] Exercise published Featherweight APIs against CLI requirements. Adopt a
   replacement only if it improves the current implementation; otherwise write
   concrete maintainers' requests and a clear retained-runtime decision.
3. [x] Update architecture/letter, format, run canonical quality gates and commit
   the validated result.

## Decisions and focused validation

- Horns renderer selection now uses PathTrie. Parent reviewed the implementation
  and root/exact-match/component-boundary regressions. `cargo test -p horns-core
  --locked --offline` passed 139 unit and three integration tests
  (`local/horns-pathtrie-tests.log`).
- The published-runtime probe completed successfully:
  `CARGO_TARGET_DIR=target cargo run --manifest-path
  local/structfs-migration-probe/Cargo.toml --bin cli_runtime --offline`
  (`local/cli-featherweight-runtime-probe.log`). It confirms prepared synchronous
  rejection, absent synchronous memory-cap controls, non-trapping denied growth
  in the prepared async API, and its fixed epoch interval.
- Retain the CLI conversation runner. A raw synchronous replacement loses
  compiled-module reuse and memory-cap configuration. A prepared async port is
  possible, but needs additional host-effects lifetime/cancellation machinery
  and does not expose Ox's grow-failure policy. FW-003 records the specific
  request; this decision does not claim async agent execution is impossible.
- Retain CLI snapshot/flat scratch-store semantics and Horns shareable writer
  rather than adding compatibility wrappers. SF-012 records those opportunities.
- The original completed gateway/utility work was committed as `d51df81`; the
  verified follow-up plan was committed separately as `2017d50`. No hooks were
  bypassed and no correspondence was sent.

## Final validation

- `./scripts/fmt.sh` passed.
- `./scripts/quality_gates.sh` passed all 13 gates
  (`local/cli-horns-quality-gates.log`), including 2,338 Rust tests, native and
  browser lint, wasm build, UI checks/tests and production build. Region coverage
  is 80.19% in `target/coverage/rust_summary.txt`.
- `git diff --check` passed. Implementation and the retention decision are
  committed together after validation; no local probe artifacts are staged.

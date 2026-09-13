# Prepare Ox for crates.io

Prepare and validate release artifacts; do not upload packages. Keep all scratch,
isolated registries and logs under `local/`. Commit validated milestones with
hooks enabled. Initial release version remains 0.1.0 unless registry evidence
requires a different version/name.

## Prerequisites

- [x] **P1. Workspace dependencies are path-only.**
  `nl -ba Cargo.toml | sed -n '66,106p'` verifies workspace version:67 and
  internal dependencies:79–99. Packaging ox-cli fails on the unversioned
  horns-core dependency (`local/ox-cli-package-audit.log`). Add matching registry
  versions to internal dependencies; if registry names conflict, publication is
  blocked pending ownership or naming decisions (one day plus owner response).
- [x] **P2. The CLI helper is a separate package binary.**
  `cat crates/ox-tools/Cargo.toml` verifies the ox-tool-exec binary target:12.
  `sed -n '1,90p' crates/ox-tools/src/bin/ox-tool-exec.rs` verifies entry point:35
  and existing request protocol. `sed -n '1425,1435p'
  crates/ox-executor/src/agents.rs` verifies sibling executable discovery:1429.
  Move implementation into the tools library and provide a CLI binary shim.
  Retain the tools binary for existing tests/deployment. If behavior differs,
  stop helper packaging until sandbox parity is restored (one day).
- [x] **P3. Embedded guests currently require sibling source packages.**
  `cat crates/ox-executor/build.rs crates/ox-gateway/build.rs` verifies workspace
  traversal:6 and nested cargo guest builds:12/14. `rg -n 'include_bytes!'
  crates/ox-executor/src crates/ox-gateway/src` verifies consumers at
  agents.rs:94 and codec_block.rs:10. Generate package-local Wasm plus provenance
  before release; package build scripts consume those artifacts without nested
  Cargo. If guest parity fails, packaging is blocked until rebuilt (one day).
- [x] **P4. CLI publication requires its internal library closure.**
  `python3.12` with tomllib over `crates/*/Cargo.toml` (audit above) verifies
  19 packages in CLI normal dependency closure, including disabled ox-types,
  ox-executor and Horns packages. The gateway and worker add public executables.
  Set a deliberate release allowlist; leave browser/dev/stub/guest packages
  private. Validate full dependency closure including target and dev edges;
  if cycles prevent packaging, resolve the cycle before release (one day).
- [x] **P5. Existing CI validates only the checkout.**
  `cat .github/workflows/ci.yml` verifies the sole quality-gates job:10 and
  workspace script:32. `.gitignore:2` ignores Cargo.lock. Add a tracked release
  lockfile, isolated package verification and installed-binary smoke tests.
  If isolated builds fail, publication stays blocked until fixed (one day).

## Execution

One implementation sub-agent task at a time; parent works on release tooling,
reviews changes, verifies packaging and maintains release documentation.

1. [ ] Share the tool-exec implementation and install it with ox-cli; test parity.
2. [ ] Version/publish the release dependency closure, supply metadata, and track
   the lockfile. Check registry name/version availability where accessible.
3. [ ] Generate reproducible package-local Wasm artifacts with provenance and
   remove package-build dependence on the checkout layout.
4. [ ] Add release staging, dependency-order verification and isolated install
   smoke checks. Verify actual archives rather than path-patched source trees.
5. [ ] Document installation/release procedure and add Linux/macOS release CI.
   Run formatting, quality gates, package verification and commit results.

Publication, remote pushes and release tags are outside this preparation task.

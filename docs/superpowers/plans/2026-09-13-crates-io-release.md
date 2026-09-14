# Prepare Ox for crates.io

Prepare and validate release artifacts; do not upload packages. Keep all scratch,
isolated registries and logs under `local/`. Commit validated milestones with
hooks enabled. Initial release version remains 0.1.0 unless registry evidence
requires a different version/name.

## Prerequisites

P1–P3 and P5 record the verified pre-implementation state at `7d9e94e`;
their requested changes have since shipped in `642b067`. P4 below reflects
the implemented release allowlist.

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
  `nl -ba crates/ox-cli/Cargo.toml` verifies internal normal dependencies at
  lines 29–44 and 52; `nl -ba release.json` verifies the 22-package allowlist
  at lines 3–8. `bash scripts/release.sh order` checks the full normal/build
  closure, including target edges, against Cargo metadata and publish flags.
  Browser/dev/stub/guest packages stay private; path-only internal dev edges
  are omitted by Cargo from published manifests to avoid first-release cycles;
  if cycles prevent packaging, resolve the cycle before release (one day).
- [x] **P5. Existing CI validates only the checkout.**
  `cat .github/workflows/ci.yml` verifies the sole quality-gates job:10 and
  workspace script:32. `.gitignore:2` ignores Cargo.lock. Add a tracked release
  lockfile, isolated package verification and installed-binary smoke tests.
  If isolated builds fail, publication stays blocked until fixed (one day).

## Execution

One implementation sub-agent task at a time; parent works on release tooling,
reviews changes, verifies packaging and maintains release documentation.

1. [x] Share the tool-exec implementation and install it with ox-cli; test parity.
2. [x] Version/publish the release dependency closure, supply metadata, and track
   the lockfile. Check registry name/version availability where accessible.
3. [x] Generate reproducible package-local Wasm artifacts with provenance and
   remove package-build dependence on the checkout layout.
4. [x] Add release staging, dependency-order verification and isolated install
   smoke checks. Verify actual archives rather than path-patched source trees.
5. [x] Document installation/release procedure and add Linux/macOS release CI.
   Run formatting, quality gates, package verification and commit results.

Publication, remote pushes and release tags are outside this preparation task.

## Validation and implementation decisions

- `642b067` contains helper installation, package metadata/licenses/READMEs,
  tracked lockfile/toolchain and self-contained Wasm artifacts. Helper parity
  checks passed (60 tools tests plus the CLI-owned helper integration test).
- Independent archive builds exposed a missing normal Tokio `macros` feature
  in ox-broker. The broker and CLI now explicitly declare the features needed
  outside workspace test feature unification.
- Cargo 1.96 batch verification hits `no hash listed` on unpublished internal
  dependencies. The release checker assembles archives with `--no-verify`,
  then independently builds every actual extracted archive using a checksummed
  directory source. No path patches refer back to workspace sources.
- At the user's request, release tooling uses Bash 3.2 and standard command-line
  tools plus jq. Cargo parses manifests; the release allowlist is JSON. The
  original Python implementation is replaced, including CI prerequisites.
- The Bash release check reproduced all 22 independent archive builds, the CLI
  archive's 19 integration tests, installation of all three products, and the
  installed smoke checks (`local/release-check.log`). ShellCheck and Bash syntax
  checks pass for all three scripts; the Wasm bytes are unchanged. Both the
  missing `--execute` publication guard and dirty-checkout check guard reject
  the invocation as intended. Registry checks again report all 22 names absent.
- The initial concurrent gate run timed out in `test_account_progresses_status`;
  the subsequent nonconcurrent run passed that test. Gateway smoke tests require
  localhost binding permission; the restricted attempt failed, and the permitted
  rerun passed. No test assertions or hooks were bypassed.
- Final workspace quality gates pass 13/13 (`local/release-quality-gates.log`),
  including the complete Rust coverage run. After committing, run the release
  checker without `--allow-dirty` to bind the final receipt to the clean commit.
- Linux/macOS release CI is added but has not been run remotely. Local validation
  is on macOS ARM64. Publication still requires both CI results and explicit
  authorization; this preparation performs no uploads.

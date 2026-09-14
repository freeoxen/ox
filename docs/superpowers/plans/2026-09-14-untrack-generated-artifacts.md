# Generate Wasm artifacts without tracking them

## Prerequisites

- [x] **Four generated packaging files are tracked.** `git ls-files '*.wasm'
  '*.provenance.json'` lists the two files and sidecars under
  `crates/ox-executor/artifacts/` and `crates/ox-gateway/artifacts/`.
  `cat crates/ox-executor/build.rs crates/ox-gateway/build.rs` verifies both
  consumers copy artifacts into OUT_DIR at line 8. Preserve generation before
  host compilation; otherwise source builds fail (same-day build-flow fix).
- [x] **Package inclusion is explicit rather than dependent on Git tracking.**
  `head -12 crates/ox-gateway/Cargo.toml` and the executor manifest show
  `artifacts/**` in `include` at line 3. Validate actual archive contents after
  untracking; if Cargo excludes them, packaging is blocked until explicit
  inclusion works (same-day packaging fix).
- [x] **Release preparation currently assumes committed generated files.**
  `cat scripts/release.sh` verifies freshness-only generation at line 123;
  `cat docs/releasing.md` requests committing outputs at lines 33–37.
  Generate before packaging and retain freshness checks before publication.
  If artifacts cannot be reproduced, publication remains blocked (external
  toolchain/dependency availability).
- [x] **The launcher already generates artifacts, but quality gates do not.**
  `cat scripts/run_cli.sh` verifies preparation at line 16;
  `cat scripts/quality_gates.sh` shows direct Cargo checks. CI delegates to this
  gate script (`cat .github/workflows/ci.yml`, final step). Add generation to
  the gate entry point and document preparation for direct Cargo commands.
  Missing preparation blocks clean-checkout validation (same-day fix).

## Execution

1. [x] Parent untracks generated outputs, adds ignores, updates generation and
   release/quality entry points, and verifies packaging from generated inputs.
2. [x] One sub-agent audits remaining build entry points and updates developer
   documentation to describe source preparation and ignored package artifacts.
3. [x] Parent verifies absent-artifact regeneration, launcher, package contents,
   formatting and canonical quality gates. No publication or history rewrite.

## Validation and results

- `git ls-files '*.wasm' '*.wasm.provenance.json'` returns no files. All four
  generated packaging files are ignored; their deletions are staged for commit.
- Moved both local artifact directories to `local/untracked-artifacts-backup`,
  then ran `scripts/run_cli.sh --help`. It recreated the missing guests and
  launched successfully (`local/untracked-artifacts-launcher.log`).
- `scripts/release.sh package --offline --allow-dirty` assembled all 22 archives
  (`local/untracked-artifacts-package.log`). Python tarfile/hashlib checks verified
  that both host archives contain Wasm identical to the regenerated local files
  and that their included provenance hashes match. Nothing was published.
- `./scripts/fmt.sh`, shell syntax checks and `git diff --check` passed.
- `./scripts/quality_gates.sh` passed all **15/15** gates
  (`local/untracked-artifacts-quality-gates.log`), including new checks for
  untracked Wasm and source generation before host compilation.
- Documentation and native development/remote/container entry points now prepare
  guests before direct host builds. Container and live-cloud execution were not
  run; shell contracts and the local native build were checked.
- Audited source-tree `.stderr` files: six deliberate trybuild expectations
  under `tests/compile_fail` are test inputs. Four stale scratch diagnostics
  under `ox-kernel/wip` were moved to ignored `local/trybuild-wip-2026-09-14`.
  Added a workspace ignore for future trybuild `wip` output.

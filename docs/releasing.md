# Releasing Ox on crates.io

The release set is explicit in `release.json`: 22 coordinated packages at 0.1.0.
`ox-cli` installs **both** `ox` and `ox-tool-exec`. `ox-gateway` and `ox-worker`
are separately installable. Horns' three packages can be used independently.
Initial supported native platforms are Linux and macOS; Windows is not a
supported release target. Rust 1.96 and a native C/C++ build toolchain are required.

Browser, development-server, stub and guest packages remain private. `ox-core`
is also private: that name already exists on crates.io and is not needed by the
native product dependency closure. Do not publish it without resolving ownership
or choosing a different name. The other 22 names returned HTTP 404 from the
sparse registry index during preparation; this is not a reservation.

## Prepare a release

Use Bash 3.2+, `jq`, `curl`, `shasum`, `tar`, and the checked-in Rust toolchain.
The release scripts use Cargo to parse manifests and jq for JSON; no Python is
required. Ordinary package consumers do not need these release tools, Bun,
wasm-pack or the wasm32 target. Contributors rebuilding
the guest artifacts do need the wasm32 target declared in rust-toolchain.toml.

1. Update the coordinated workspace version, internal dependency requirements
   and `release.json` together. Write the changelog. Internal normal/build
   dependencies use path plus version; internal integration-test dependencies
   deliberately use path only so Cargo omits them from registry manifests and
   first publication is not blocked by test-only dependency cycles.
2. Update and commit `Cargo.lock`. Do not publish yanked dependency selections.
   Keep the targeted `allocative` compatibility constraint until its upstream
   Starlark dependency supports newer versions.
3. Run `./scripts/build-wasm-artifacts.sh`. This builds locked release
   guests and records their source inputs, compiler and SHA256 hashes beside
   the Wasm files in `ox-executor/artifacts` and `ox-gateway/artifacts`.
   These generated files are ignored by Git; commit their source inputs only.
   Cargo manifests explicitly include `artifacts/**` in the published archives.
   Build scripts only copy these packaged artifacts; they never build sibling
   workspace crates. A source checkout needs this preparation before direct
   Cargo builds or tests.
4. Run `./scripts/fmt.sh` and `./scripts/quality_gates.sh`. Commit the changes.
5. Run `./scripts/release.sh registry` to recheck registry names, then
   `./scripts/release.sh check`. Use `--offline` when all dependencies are
   cached. `--allow-dirty` is only a development aid: its receipt cannot authorize
   the publishing command.

`check` builds the guests from locked source inputs and packages the complete
release set. Quality gates and `scripts/run_cli.sh` also prepare artifacts
automatically. Release verification vendors external registry dependencies and
the **actual generated `.crate`
archives**, builds every extracted package independently with `--locked`, then
compiles CLI remount/approval/helper tests from its extracted archive.
Internal dependencies resolve from archive contents, with no path
patches back to this repository. It installs CLI, gateway and worker with
`--locked --offline` and runs `release-smoke.sh` against those installed binaries.
Logs, isolated state and `verified.json` live under `local/release/`.

Cargo 1.96's batch package verifier currently fails on unpublished internal
dependencies with `no hash listed for horns-core v0.1.0`. Assembly therefore
uses `cargo package --no-verify`, followed by the mandatory independent builds
above. This is not a waiver: no successful receipt is written unless every
archive builds and all installation/tests pass. Individual `cargo publish`
verification remains enabled once preceding dependencies are in the registry.

The smoke suite checks helper file/shell behavior and limits, CLI/worker startup,
and an isolated gateway using embedded Wasm. It needs no model credentials.
Existing archive tests cover durable save/remount and approval recovery. This
does not replace a manual interactive first-run check or live-provider validation.

The Linux/macOS release workflow runs the same preparation checks. Both platform
jobs must be green for the release commit. Linux sandbox enforcement remains
part of the workspace quality/remote tests, not an inferred consequence of
compiling successfully on macOS.

## Review and publish

Before uploading, review the archive file lists, changelog and platform results.
Confirm crates.io ownership for existing names, configure the publishing account
and credential, and grant the intended maintainers access. Do not put credentials
in the repository or scripts. Registry name checks cannot establish ownership.

After explicit approval to publish this version:

```sh
./scripts/release.sh publish --execute
```

Without `--execute`, no upload occurs. The command requires a clean checkout and
a successful verification receipt for the same commit, checks generated artifact
freshness and saved archive hashes, rechecks the index, and invokes
`cargo publish --locked` in dependency
order. Each Cargo invocation performs its own package verification. Publication
can partially succeed; if interrupted, inspect registry versions before retrying
and publish only the remaining packages. Never increment versions blindly or
try to overwrite an already-published version.

Create/push the release tag and changelog only as part of the approved release.
This preparation does not upload packages, push branches, or create remote tags.

## Consumer installation

Once the version is published:

```sh
cargo install ox-cli --version 0.1.0 --locked
cargo install ox-gateway --version 0.1.0 --locked  # optional gateway
cargo install ox-worker --version 0.1.0 --locked   # optional remote worker
```

Do not use `--bin ox` when installing the CLI: that omits its required helper.
Installing an `ox-tools` library dependency alone does not install its binary.
The tools package still provides a standalone helper for existing deployments;
the CLI package is the normal owner of both executables for user installations.

## Sources

- [Cargo publishing](https://doc.rust-lang.org/cargo/reference/publishing.html)
- [Cargo dependency locations](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html)
- [Cargo installation and lockfiles](https://doc.rust-lang.org/cargo/commands/cargo-install.html)

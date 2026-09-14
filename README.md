# Ox

Ox is an agentic coding assistant with a terminal UI, durable conversations,
sandboxed tools, and a local LLM gateway. Its stores and capability boundaries
use [StructFS](https://github.com/StructFS/structfs); the gateway runs on
Featherweight 0.3.

## Install

The first crates.io release, **0.1.0**, is being prepared and is not yet published.
Once available, on Linux or macOS with Rust 1.96+ and native build tools:

```sh
cargo install ox-cli --version 0.1.0 --locked
ox --version
ox --workspace /path/to/project
```

Install the complete package: it supplies both `ox` and its required
`ox-tool-exec` helper. Do not select only `--bin ox`. The packaged application
includes its Wasm agent; installation does not require the repository checkout,
Bun, wasm-pack, or the wasm32 Rust target.

On first launch Ox guides you through provider/account setup. Run `ox init` to
open setup explicitly. Configuration, keys, logs and durable conversations live
under `~/.ox`. See `ox --help` and `ox remote --help` for available commands.
Normal execution uses policy checks and approvals; `--no-policy` explicitly
disables that enforcement.

Until publication, build from this repository with the pinned Rust toolchain,
including its wasm32 target, Bash and jq:

```sh
./scripts/build-wasm-artifacts.sh
cargo build --locked -p ox-cli
./target/debug/ox --workspace /path/to/project
```

## Optional services

After publication:

```sh
cargo install ox-gateway --version 0.1.0 --locked
ox-gateway
```

The gateway serves Anthropic/OpenAI-compatible APIs at `127.0.0.1:11343`, using
Ox's configured accounts. `OX_GATEWAY_BIND` changes the listener and `OX_DIR`
selects a separate state directory. Its compiled Featherweight guests and
assembly manifest ship inside the package.

`ox-worker` supplies the headless remote worker. Install `ox-cli` (or the
standalone `ox-tools` helper package) alongside it so `ox-tool-exec` is present.
See [worker deployment](deploy/ox-worker/README.md) before exposing a worker.

## Libraries

| Packages | Purpose |
| --- | --- |
| `horns`, `horns-core`, `horns-ratatui` | Path-based UI framework and terminal renderer |
| `ox-kernel`, `ox-types` | Conversation state machine and shared data types |
| `ox-broker`, `ox-context`, `ox-store-util` | Store routing and application adapters |
| `ox-config`, `ox-gate`, `ox-codec` | Configuration, providers and wire translation |
| `ox-history`, `ox-inbox` | Conversation projections and durable storage |
| `ox-tools`, `ox-runtime`, `ox-executor` | Tool effects and conversation execution |
| `ox-structfs-transport`, `ox-remote`, `ox-worker` | Remote execution and transport |
| `ox-ui`, `ox-cli`, `ox-gateway` | Applications and their supporting stores |

The browser playground, development server, guest build crates and the legacy
`ox-core` umbrella remain workspace-only. Their absence from the release set does
not prevent installing the native applications.

## Development and releases

Use the pinned Rust toolchain, Bash, jq, Bun and wasm-pack for full workspace
checks. The native packages do not need the frontend toolchain to build.

```sh
./scripts/build-wasm-artifacts.sh  # before direct Cargo builds/tests
./scripts/fmt.sh
./scripts/quality_gates.sh
./scripts/release.sh check        # clean commit; isolated package checks
```

Wasm binaries and their provenance files are generated locally and ignored by
Git. Run the artifact builder before direct Cargo commands on a fresh checkout
or after changing guest inputs. `scripts/run_cli.sh`, quality gates and release
checks prepare them automatically. Published packages include these generated
files, so package consumers do not need to build the guests.

[Release preparation and publication](docs/releasing.md) documents package order,
artifact provenance, installation tests, and the explicit publication step.
[Architecture](docs/architecture/data-model.md) documents the data contracts.

Licensed under [Apache-2.0](LICENSE).

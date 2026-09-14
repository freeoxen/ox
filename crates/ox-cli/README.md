# ox-cli

Ox is a terminal coding assistant with durable conversations, provider setup,
approvals, sandboxed file/shell tools, and remote conversation management.

The 0.1.0 release is being prepared. Once published:

```sh
cargo install ox-cli --version 0.1.0 --locked
ox --version
ox --workspace /path/to/project
```

Requires Linux or macOS, Rust 1.96+, and native build tools. Installation supplies
both `ox` and `ox-tool-exec`; do not use `--bin ox`, which omits the required
helper. Embedded Wasm ships with the package, so no wasm32 target or source
checkout is needed.

First launch opens provider/account setup; `ox init` opens it explicitly.
Configuration and durable conversation data live under `~/.ox`.
Use `ox --help` or `ox remote --help` for commands. Normal execution retains
policy enforcement and approvals.

See the [project](https://github.com/freeoxen/ox) and
[release guide](https://github.com/freeoxen/ox/blob/main/docs/releasing.md).
Licensed under Apache-2.0.

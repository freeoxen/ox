fn main() {
    // agent.wasm is built by scripts/run.sh (or scripts/build-agent.sh)
    // before `cargo build -p ox-cli`. We just verify it exists so
    // include_bytes! in agents.rs doesn't fail with a confusing error.
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let wasm = workspace_root.join("target/agent.wasm");
    if !wasm.exists() {
        panic!(
            "target/agent.wasm not found — run ./scripts/run_cli.sh or ./scripts/build-agent.sh first"
        );
    }
    // Re-run when the wasm artifact changes so include_bytes! picks up the new binary.
    println!("cargo:rerun-if-changed={}", wasm.display());
}

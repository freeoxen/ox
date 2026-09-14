use std::path::PathBuf;

fn main() {
    let artifact = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("artifacts/agent.wasm");
    let destination =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set")).join("agent.wasm");
    println!("cargo:rerun-if-changed=artifacts/agent.wasm");
    std::fs::copy(&artifact, &destination).unwrap_or_else(|error| {
        panic!(
            "cannot copy packaged Wasm artifact {}: {error}; repository contributors should run python3 scripts/build-wasm-artifacts.py",
            artifact.display()
        )
    });
}

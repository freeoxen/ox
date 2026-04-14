use std::path::Path;
use std::process::Command;

fn main() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.join("../..");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let out_wasm = Path::new(&out_dir).join("agent.wasm");

    // Build ox-wasm for wasm32-unknown-unknown
    let status = Command::new("cargo")
        .args([
            "build",
            "--target",
            "wasm32-unknown-unknown",
            "--release",
            "-p",
            "ox-wasm",
        ])
        .current_dir(&workspace_root)
        .status()
        .expect("failed to invoke cargo for ox-wasm build");

    if !status.success() {
        panic!("ox-wasm build failed");
    }

    // Copy to OUT_DIR
    let built = workspace_root.join("target/wasm32-unknown-unknown/release/ox_wasm.wasm");
    std::fs::copy(&built, &out_wasm).unwrap_or_else(|e| {
        panic!(
            "failed to copy {} to {}: {e}",
            built.display(),
            out_wasm.display()
        )
    });

    // Rebuild when ox-wasm source changes
    let wasm_src = workspace_root.join("crates/ox-wasm/src");
    println!("cargo:rerun-if-changed={}", wasm_src.display());
    // Also rebuild when kernel/runtime change (ox-wasm depends on them)
    for dep in &[
        "ox-kernel",
        "ox-runtime",
        "ox-core",
        "ox-context",
        "ox-history",
        "ox-gate",
    ] {
        let dep_src = workspace_root.join(format!("crates/{dep}/src"));
        println!("cargo:rerun-if-changed={}", dep_src.display());
    }
}

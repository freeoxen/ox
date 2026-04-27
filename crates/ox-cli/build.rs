use std::path::Path;
use std::process::Command;

fn main() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.join("../..");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let out_wasm = Path::new(&out_dir).join("agent.wasm");

    // Build ox-wasm for wasm32-unknown-unknown
    let output = Command::new("cargo")
        .args([
            "build",
            "--target",
            "wasm32-unknown-unknown",
            "--release",
            "-p",
            "ox-wasm",
        ])
        .current_dir(&workspace_root)
        .output()
        .expect("failed to invoke cargo for ox-wasm build");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Re-emit the inner cargo's stderr through cargo:warning so it
        // shows up directly in the parent build output instead of being
        // hidden inside the build-script's own stderr buffer.
        for line in stderr.lines() {
            println!("cargo:warning={line}");
        }
        let hint = if stderr.contains("wasm32-unknown-unknown")
            && stderr.contains("may not be installed")
        {
            "\n\nhint: install the wasm target with:\n    rustup target add wasm32-unknown-unknown\n"
        } else {
            ""
        };
        panic!("ox-wasm build failed (see cargo warnings above){hint}");
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

use std::path::Path;
use std::process::Command;

fn main() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.join("../..");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let out_wasm = Path::new(&out_dir).join("agent.wasm");

    // Build ox-wasm for wasm32-unknown-unknown
    let mut cmd = Command::new("cargo");
    cmd.args([
        "build",
        "--target",
        "wasm32-unknown-unknown",
        "--release",
        "-p",
        "ox-wasm",
    ])
    .current_dir(&workspace_root)
    // Coverage instrumentation from the outer build must not leak into
    // the wasm32 build: that target has no profiler_builtins, and the
    // agent module is embedded data, never the thing being measured.
    // cargo-llvm-cov injects `-C instrument-coverage` two ways — via
    // RUSTFLAGS-family vars and via a rustc wrapper driven by
    // __CARGO_LLVM_COV_* vars — so scrub both channels.
    .env_remove("RUSTFLAGS")
    .env_remove("CARGO_ENCODED_RUSTFLAGS");
    if std::env::var_os("CARGO_LLVM_COV").is_some() {
        // Only llvm-cov's own wrapper is dropped; an unrelated wrapper
        // (e.g. sccache) stays in place.
        cmd.env_remove("RUSTC_WRAPPER");
        cmd.env_remove("RUSTC_WORKSPACE_WRAPPER");
    }
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().contains("LLVM_COV") {
            cmd.env_remove(&key);
        }
    }
    let output = cmd
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

    // Copy to OUT_DIR. The inner cargo inherits CARGO_TARGET_DIR from
    // this process (cargo-llvm-cov, for one, redirects it), so the
    // artifact lands wherever that points — not necessarily `target/`.
    // A relative CARGO_TARGET_DIR resolves against the inner cargo's
    // cwd, which is the workspace root; `join` also handles the
    // absolute case.
    let target_root = std::env::var_os("CARGO_TARGET_DIR")
        .map(|dir| workspace_root.join(dir))
        .unwrap_or_else(|| workspace_root.join("target"));
    let built = target_root.join("wasm32-unknown-unknown/release/ox_wasm.wasm");
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

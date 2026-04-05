use std::process::Command;

fn main() {
    // Rebuild agent.wasm when ox-wasm source changes
    println!("cargo:rerun-if-changed=../ox-wasm/src/lib.rs");
    println!("cargo:rerun-if-changed=../ox-wasm/Cargo.toml");

    let status = Command::new("cargo")
        .args([
            "build",
            "--target",
            "wasm32-unknown-unknown",
            "--release",
            "-p",
            "ox-wasm",
        ])
        .status();

    match status {
        Ok(s) if s.success() => {
            // Copy to target/agent.wasm for include_bytes!
            let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            let src = workspace_root.join("target/wasm32-unknown-unknown/release/ox_wasm.wasm");
            let dst = workspace_root.join("target/agent.wasm");
            if let Err(e) = std::fs::copy(&src, &dst) {
                panic!(
                    "failed to copy agent.wasm: {e}\n  from: {}\n  to: {}",
                    src.display(),
                    dst.display()
                );
            }
        }
        Ok(s) => panic!("ox-wasm build failed with status: {s}"),
        Err(e) => panic!("failed to run cargo: {e}"),
    }
}

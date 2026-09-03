use std::path::Path;
use std::process::Command;

fn main() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.join("../..");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let out_wasm = Path::new(&out_dir).join("codec_block.wasm");

    // Build the codec Block for wasm32-unknown-unknown. Coverage
    // instrumentation from the outer build must not leak into the wasm32
    // build (no profiler_builtins there) — same scrub ox-cli's build.rs does.
    let mut cmd = Command::new("cargo");
    cmd.args([
        "build",
        "--target",
        "wasm32-unknown-unknown",
        "--release",
        "-p",
        "ox-gateway-wasm",
    ])
    .current_dir(&workspace_root)
    .env_remove("RUSTFLAGS")
    .env_remove("CARGO_ENCODED_RUSTFLAGS");
    if std::env::var_os("CARGO_LLVM_COV").is_some() {
        cmd.env_remove("RUSTC_WRAPPER");
        cmd.env_remove("RUSTC_WORKSPACE_WRAPPER");
    }
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().contains("LLVM_COV") {
            cmd.env_remove(&key);
        }
    }
    let output = cmd.output().expect("running cargo for ox-gateway-wasm");
    if !output.status.success() {
        panic!(
            "wasm build failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let built = workspace_root.join("target/wasm32-unknown-unknown/release/ox_gateway_wasm.wasm");
    std::fs::copy(&built, &out_wasm).expect("copying codec_block.wasm");

    println!("cargo:rerun-if-changed=../ox-gateway-wasm/src");
    println!("cargo:rerun-if-changed=../ox-codec/src");
}

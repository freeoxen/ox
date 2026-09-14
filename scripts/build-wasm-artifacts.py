#!/usr/bin/env python3
"""Build the Wasm shipped in registry packages, or verify it with --check.

Requires Python 3.11+, the repository's pinned Rust toolchain, and its wasm32
target. Consumer package builds only copy these artifacts; they never run Cargo.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tomllib


ROOT = Path(__file__).resolve().parents[1]
TARGET = "wasm32-unknown-unknown"
GUESTS = {
    "ox-wasm": ("ox-executor", "agent.wasm"),
    "ox-gateway-wasm": ("ox-gateway", "codec_block.wasm"),
}


def load_manifest(path):
    return tomllib.loads(path.read_text())


def source_inputs(guest):
    """Include internal normal/build dependency closure, including target tables.

    Including every target's dependencies is conservative: a target cfg change
    must invalidate provenance even if today's wasm build excludes that edge.
    """
    workspace = load_manifest(ROOT / "Cargo.toml")["workspace"]["dependencies"]
    manifests = {path.parent.name: path for path in (ROOT / "crates").glob("*/Cargo.toml")}
    pending = [manifests[guest]]
    visited = set()
    inputs = {ROOT / "Cargo.toml", ROOT / "Cargo.lock", Path(__file__).resolve()}
    for config in (ROOT / "rust-toolchain.toml", ROOT / ".cargo" / "config.toml"):
        if config.exists():
            inputs.add(config)
    while pending:
        manifest = pending.pop().resolve()
        if manifest in visited:
            continue
        visited.add(manifest)
        inputs.add(manifest)
        source = manifest.parent / "src"
        inputs.update(path for path in source.rglob("*") if path.is_file())
        build_script = manifest.parent / "build.rs"
        if build_script.exists():
            inputs.add(build_script)
        data = load_manifest(manifest)
        tables = [data, *data.get("target", {}).values()]
        for table in tables:
            for kind in ("dependencies", "build-dependencies"):
                for name, spec in table.get(kind, {}).items():
                    if not isinstance(spec, dict):
                        continue
                    base = manifest.parent
                    if spec.get("workspace"):
                        spec = workspace[name]
                        base = ROOT
                    if isinstance(spec, dict) and "path" in spec:
                        pending.append(base / spec["path"] / "Cargo.toml")
    return sorted(inputs)


def provenance(guest, artifact, rustc):
    files = {
        path.relative_to(ROOT).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in source_inputs(guest)
    }
    digest = hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()
    return {
        "schema_version": 1,
        "guest_package": guest,
        "target": TARGET,
        "profile": "release",
        "rustc": rustc,
        "artifact_sha256": hashlib.sha256(artifact).hexdigest(),
        "source_sha256": digest,
        "inputs": files,
    }


def build_environment():
    env = dict(os.environ)
    coverage = any("LLVM_COV" in key for key in env)
    for key in list(env):
        if "LLVM_COV" in key or key.endswith("RUSTFLAGS") or key == "LLVM_PROFILE_FILE":
            env.pop(key)
    if coverage:
        env.pop("RUSTC_WRAPPER", None)
        env.pop("RUSTC_WORKSPACE_WRAPPER", None)
    cargo_home = Path(env.get("CARGO_HOME", Path.home() / ".cargo")).resolve()
    env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join([
        f"--remap-path-prefix={ROOT}=/ox",
        f"--remap-path-prefix={cargo_home}=/cargo",
    ])
    env["CARGO_TARGET_DIR"] = str(ROOT / "local" / "wasm-artifacts" / "target")
    return env


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="rebuild and fail if tracked artifacts or provenance differ")
    args = parser.parse_args()
    staging = ROOT / "local" / "wasm-artifacts"
    staging.mkdir(parents=True, exist_ok=True)
    env = build_environment()
    rustc = subprocess.check_output(["rustc", "--version"], cwd=ROOT, env=env, text=True).strip()
    command = ["cargo", "build", "--release", "--locked", "--target", TARGET]
    for guest in GUESTS:
        command.extend(["-p", guest])
    with (staging / "build.log").open("w") as log:
        result = subprocess.run(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
    if result.returncode:
        raise SystemExit(f"Wasm build failed; see {staging / 'build.log'}")
    stale = []
    for guest, (package, filename) in GUESTS.items():
        artifact = (Path(env["CARGO_TARGET_DIR"]) / TARGET / "release" / f"{guest.replace('-', '_')}.wasm").read_bytes()
        record = provenance(guest, artifact, rustc)
        outputs = {
            filename: artifact,
            filename + ".provenance.json": (json.dumps(record, indent=2, sort_keys=True) + "\n").encode(),
        }
        destination = ROOT / "crates" / package / "artifacts"
        if not args.check:
            destination.mkdir(parents=True, exist_ok=True)
        for name, content in outputs.items():
            path = destination / name
            if args.check:
                if not path.exists() or path.read_bytes() != content:
                    stale.append(str(path.relative_to(ROOT)))
            else:
                path.write_bytes(content)
        print(f"{guest}: {len(artifact)} bytes, SHA256 {record['artifact_sha256']}")
    if stale:
        raise SystemExit("Stale packaged Wasm; run python3 scripts/build-wasm-artifacts.py:\n" + "\n".join(stale))
    print("Packaged Wasm artifacts verified." if args.check else "Packaged Wasm artifacts updated.")


if __name__ == "__main__":
    main()

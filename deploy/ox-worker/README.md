# ox-worker image

The worker image packages `ox-worker` and `ox-tool-exec`. The agent Wasm is
embedded by `ox-executor`'s existing build script, so the CLI and worker use the
same module and runtime. The container entrypoint is the long-lived service;
SSH sessions only run the stateless `structfs-stdio` bridge and cannot own or
terminate agent turns.

Production startup requires `OX_NODE_ID`, `OX_NODE_ATTEMPT_ID`, and the exact
digest-pinned image reference in `OX_WORKER_IMAGE_DIGEST`. `WorkerService`
performs an allowed-write, forbidden-write, and forbidden-network sandbox probe
before it binds its socket or reports `health.status = ready`.

Use `scripts/build-worker-image.sh`. It requires digest-pinned build and runtime
base images and emits the built image ID/digest. Push/release builds fail closed
unless both Syft and Trivy are installed. Trivy also fails the build for high or
critical findings; override that set with `OX_WORKER_VULN_SEVERITY`. Local
non-push builds may opt into the same scanner requirement with
`OX_WORKER_REQUIRE_SCANNERS=1`. Set `OX_WORKER_PUSH=1` to push and resolve the
registry digest that belongs in `~/.ox/remote.toml`.

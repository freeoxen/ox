#!/bin/sh
set -eu

: "${OX_WORKER_BUILD_IMAGE:?set OX_WORKER_BUILD_IMAGE to a digest-pinned Rust image}"
: "${OX_WORKER_RUNTIME_IMAGE:?set OX_WORKER_RUNTIME_IMAGE to a digest-pinned runtime image}"
: "${OX_WORKER_IMAGE:?set OX_WORKER_IMAGE to the output image name and tag}"

case "$OX_WORKER_BUILD_IMAGE" in *@sha256:*) ;; *) echo "build image must be digest-pinned" >&2; exit 2 ;; esac
case "$OX_WORKER_RUNTIME_IMAGE" in *@sha256:*) ;; *) echo "runtime image must be digest-pinned" >&2; exit 2 ;; esac

command -v docker >/dev/null 2>&1 || { echo "docker is required" >&2; exit 2; }

require_scanners=${OX_WORKER_REQUIRE_SCANNERS:-0}
if [ "${OX_WORKER_PUSH:-0}" = 1 ]; then
  require_scanners=1
fi
if [ "$require_scanners" = 1 ]; then
  command -v syft >/dev/null 2>&1 || { echo "syft is required for release/push builds" >&2; exit 2; }
  command -v trivy >/dev/null 2>&1 || { echo "trivy is required for release/push builds" >&2; exit 2; }
fi

output_dir=${OX_WORKER_OUTPUT_DIR:-target/ox-worker-image}
mkdir -p "$output_dir"

output_flag=--load
if [ "${OX_WORKER_PUSH:-0}" = 1 ]; then
  output_flag=--push
fi

docker buildx build \
  --file deploy/ox-worker/Containerfile \
  --build-arg "BUILD_IMAGE=$OX_WORKER_BUILD_IMAGE" \
  --build-arg "RUNTIME_IMAGE=$OX_WORKER_RUNTIME_IMAGE" \
  --tag "$OX_WORKER_IMAGE" \
  --provenance=true \
  --sbom=true \
  --iidfile "$output_dir/image-id.txt" \
  "$output_flag" \
  .

if [ "${OX_WORKER_PUSH:-0}" = 1 ]; then
  docker buildx imagetools inspect "$OX_WORKER_IMAGE" --format '{{json .Manifest.Digest}}' > "$output_dir/registry-digest.json"
fi

if command -v syft >/dev/null 2>&1; then
  syft "$OX_WORKER_IMAGE" -o cyclonedx-json="$output_dir/sbom.cdx.json"
elif [ "$require_scanners" = 1 ]; then
  echo "syft is required" >&2
  exit 2
else
  echo "syft not installed; SBOM scan skipped" >&2
fi

if command -v trivy >/dev/null 2>&1; then
  trivy image \
    --severity "${OX_WORKER_VULN_SEVERITY:-HIGH,CRITICAL}" \
    --exit-code 1 \
    --format json \
    --output "$output_dir/vulnerabilities.json" \
    "$OX_WORKER_IMAGE"
elif [ "$require_scanners" = 1 ]; then
  echo "trivy is required" >&2
  exit 2
else
  echo "trivy not installed; vulnerability scan skipped" >&2
fi

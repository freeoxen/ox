#!/bin/sh
set -eu

: "${OX_NODE_ID:?OX_NODE_ID is required}"
: "${OX_NODE_ATTEMPT_ID:?OX_NODE_ATTEMPT_ID is required}"
: "${OX_WORKER_IMAGE_DIGEST:?OX_WORKER_IMAGE_DIGEST is required}"

exec /usr/local/bin/ox-worker serve \
  --root /var/lib/ox \
  --socket /run/ox/worker.sock \
  --node-id "$OX_NODE_ID" \
  --attempt-id "$OX_NODE_ATTEMPT_ID"

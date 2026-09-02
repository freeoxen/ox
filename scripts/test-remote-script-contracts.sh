#!/bin/sh
set -eu

sh -n scripts/test-remote-live.sh
sh -n scripts/soak-remote-worker.sh
sh -n scripts/test-remote-gates.sh

set +e
env -u OX_REMOTE_LIVE sh scripts/test-remote-live.sh >/dev/null 2>&1
live_status=$?
env -u OX_REMOTE_SOAK sh scripts/soak-remote-worker.sh >/dev/null 2>&1
soak_status=$?
set -e

[ "$live_status" -eq 2 ] || {
  echo "live script must refuse mutation unless explicitly enabled" >&2
  exit 1
}
[ "$soak_status" -eq 2 ] || {
  echo "soak script must refuse mutation unless explicitly enabled" >&2
  exit 1
}

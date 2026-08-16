#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

cleanup() {
  local status=$?
  trap - EXIT
  if ! "$ROOT/scripts/regtest.sh" stop; then
    echo "error: failed to stop the regtest stack" >&2
    if (( status == 0 )); then
      status=1
    fi
  fi
  exit "$status"
}

interrupt() {
  exit 130
}

terminate() {
  exit 143
}

trap cleanup EXIT
trap interrupt INT
trap terminate TERM

"$ROOT/scripts/regtest.sh" start
"$ROOT/build-regtest.sh"
node "$ROOT/scripts/e2e-regtest.mjs"

#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WASM_OUT_DIR=pkg-regtest WASM_FEATURES=regtest-e2e exec "$ROOT/build.sh"

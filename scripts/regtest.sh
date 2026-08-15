#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
REGTEST="$ROOT/regtest/regtest.mjs"
ARKD_COMMIT=8b34e352859595cc03ba22ffa35088ab88b87fd9
ARKD_IMAGE=arkd-local:8b34e35
ARKD_WALLET_IMAGE=arkd-wallet-local:8b34e35

usage() {
  cat <<'EOF'
usage: ./scripts/regtest.sh <command> [args]

  start                         build missing images and start base + ark
  stop                          stop containers, preserving data
  clean                         remove containers and volumes
  build-images [arkd-source]    build the pinned arkd and arkd-wallet images
  fund <ark-address> <sats>     send seeded offchain sats to a browser wallet
  balance                       show the seeded Ark CLI wallet balance
  vtxos                         show the seeded Ark CLI wallet VTXOs
  info                          print local /v1/info
  mine [blocks]                 mine regtest blocks
  ark <args...>                 Ark CLI passthrough
  arkd <args...>                arkd CLI passthrough
EOF
}

require_regtest() {
  if [[ ! -f "$REGTEST" ]]; then
    echo "error: regtest submodule is missing; run git submodule update --init" >&2
    exit 1
  fi
}

build_images() {
  local source=${1:-${ARKD_SOURCE:-$ROOT/.cache/arkd}}
  if [[ ! -d "$source/.git" ]]; then
    mkdir -p "$(dirname "$source")"
    git clone --filter=blob:none "https://github.com/arkade-os/arkd.git" "$source"
  fi
  git -C "$source" fetch --depth 1 origin "$ARKD_COMMIT"
  git -C "$source" checkout --detach "$ARKD_COMMIT"
  docker build \
    --build-arg "VERSION=$ARKD_COMMIT" \
    --file "$source/Dockerfile" \
    --tag "$ARKD_IMAGE" \
    "$source"
  docker build \
    --build-arg "VERSION=$ARKD_COMMIT" \
    --file "$source/arkdwallet.Dockerfile" \
    --tag "$ARKD_WALLET_IMAGE" \
    "$source"
}

ensure_images() {
  if ! docker image inspect "$ARKD_IMAGE" >/dev/null 2>&1 \
    || ! docker image inspect "$ARKD_WALLET_IMAGE" >/dev/null 2>&1; then
    build_images
  fi
}

command=${1:-}
shift || true

case "$command" in
  start)
    require_regtest
    ensure_images
    node "$REGTEST" start --profile ark
    ;;
  stop|clean)
    require_regtest
    node "$REGTEST" "$command"
    ;;
  build-images)
    build_images "${1:-}"
    ;;
  fund)
    require_regtest
    address=${1:-}
    sats=${2:-}
    if [[ -z "$address" || ! "$sats" =~ ^[0-9]+$ || "$sats" == 0 ]]; then
      echo "usage: ./scripts/regtest.sh fund <ark-address> <positive-sats>" >&2
      exit 1
    fi
    node "$REGTEST" ark send --to "$address" --amount "$sats" --password secret
    ;;
  balance|vtxos)
    require_regtest
    node "$REGTEST" ark "$command"
    ;;
  info)
    curl --fail --silent --show-error http://127.0.0.1:7070/v1/info
    printf '\n'
    ;;
  mine)
    require_regtest
    node "$REGTEST" mine "${1:-1}"
    ;;
  ark|arkd)
    require_regtest
    node "$REGTEST" "$command" "$@"
    ;;
  *)
    usage >&2
    exit 1
    ;;
esac

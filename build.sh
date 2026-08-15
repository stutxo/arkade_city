#!/usr/bin/env bash
# Build the browser WASM bundle. Requires wasm-pack and a clang able to target
# wasm32 (secp256k1-sys compiles C). Clang is auto-detected from, in order:
#   1. $CC_wasm32_unknown_unknown (explicit override)
#   2. ./.toolchain/llvm/usr/bin/clang-* (repo-local toolchain)
#   3. clang / clang-N from $PATH (apt install clang)
set -euo pipefail

if [[ -z "${CC_wasm32_unknown_unknown:-}" ]]; then
  # plain "clang-N" only (not clang-cpp-N); resolve to an absolute path
  # because cargo invokes the compiler from crate build directories.
  repo_clang=$(find .toolchain/llvm/usr/bin -maxdepth 1 -name 'clang-[0-9]*' 2>/dev/null | sort -V | tail -1 || true)
  if [[ -n "$repo_clang" && -x "$repo_clang" ]]; then
    export CC_wasm32_unknown_unknown="$(realpath "$repo_clang")"
  else
    for c in clang clang-21 clang-20 clang-19 clang-18 clang-17 clang-16 clang-15; do
      if command -v "$c" >/dev/null 2>&1; then
        export CC_wasm32_unknown_unknown="$(command -v "$c")"
        break
      fi
    done
  fi
fi

if [[ -z "${CC_wasm32_unknown_unknown:-}" ]]; then
  echo "error: no clang found. Install one (apt install clang) or set CC_wasm32_unknown_unknown." >&2
  exit 1
fi

echo "using CC_wasm32_unknown_unknown=$CC_wasm32_unknown_unknown"
rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
wasm-pack build --target web --release
rm -f pkg/.gitignore
node --input-type=module -e 'import fs from "node:fs"; const path = "pkg/package.json"; const pkg = JSON.parse(fs.readFileSync(path, "utf8")); pkg.private = true; fs.writeFileSync(path, `${JSON.stringify(pkg, null, 2)}\n`);'

echo "built pkg/arkade_duel.js + pkg/arkade_duel_bg.wasm"

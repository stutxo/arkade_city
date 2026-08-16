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
out_dir=${WASM_OUT_DIR:-pkg}
features=${WASM_FEATURES:-}
if [[ "$out_dir" == "pkg" && -z "$features" ]]; then
  : # Public Mutinynet build.
elif [[ "$out_dir" == "pkg-regtest" && "$features" == "regtest-e2e" ]]; then
  : # Isolated local E2E build.
else
  echo "error: supported WASM builds are pkg without features or pkg-regtest with regtest-e2e" >&2
  exit 1
fi
wasm_args=(build --target web --release --no-pack --out-dir "$out_dir")
if [[ -n "$features" ]]; then
  wasm_args+=(-- --features "$features")
fi
wasm-pack "${wasm_args[@]}"
cp README.md "$out_dir/README.md"
rm -f "$out_dir/.gitignore"
OUT_DIR="$out_dir" node --input-type=module -e 'import fs from "node:fs"; const out = process.env.OUT_DIR; const files = fs.readdirSync(out); const main = files.find((file) => file.endsWith(".js") && !file.endsWith("_bg.js")); if (!main) throw new Error("generated JS entry point missing"); const stem = main.slice(0, -3); const pkg = { name: stem.replaceAll("_", "-"), type: "module", version: "3.0.0", license: "MIT", files: [`${stem}_bg.wasm`, main, `${stem}.d.ts`], main, types: `${stem}.d.ts`, sideEffects: ["./snippets/*"], private: true }; fs.writeFileSync(`${out}/package.json`, `${JSON.stringify(pkg, null, 2)}\n`);'

echo "built Arkade City WASM in $out_dir"

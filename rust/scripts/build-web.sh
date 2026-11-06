#!/usr/bin/env bash
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if ! command -v wasm-bindgen >/dev/null 2>&1; then
  echo "wasm-bindgen CLI is required. Install the version matching the locked wasm-bindgen crate." >&2
  exit 2
fi

cargo build --locked --release --target wasm32-unknown-unknown -p eagler-client-web
rm -rf dist
mkdir -p dist/pkg
wasm-bindgen \
  target/wasm32-unknown-unknown/release/eagler_client_web.wasm \
  --target web \
  --out-dir dist/pkg \
  --out-name eagler_client_web
cp web/index.html dist/index.html
echo "Rust browser package written to $root/dist"

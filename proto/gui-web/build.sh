#!/bin/sh
# Prototype 2 (web) built: the two wasm crates (cargo, -j1), their JS glue (wasm-bindgen into web/src/wasm-pkg), the
# screens (vite into web/dist). WASM_BINDGEN: the wasm-bindgen CLI (the same version as wasm/Cargo.lock's, 0.2.130).
#   ./build.sh          everything
#   ./build.sh wasm     only the wasm side
set -eu
here=$(cd "$(dirname "$0")" && pwd)
bindgen=${WASM_BINDGEN:-wasm-bindgen}
cd "$here/wasm"
cargo build --release -j1 --target wasm32-unknown-unknown
for c in view engine; do
  "$bindgen" --target web --out-dir "$here/web/src/wasm-pkg/$c" "target/wasm32-unknown-unknown/release/wsk_web_$c.wasm"
done
ls -l "$here"/web/src/wasm-pkg/*/*.wasm
[ "${1:-}" = wasm ] && exit 0
cd "$here/web"
[ -d node_modules ] || npm ci
npm run build

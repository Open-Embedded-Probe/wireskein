#!/usr/bin/env bash
# Prototype 3 (iced) for the browser (bench/notes/2026-11-25-gui-study.ja.md §15): the wasm, wasm-bindgen's glue, the
# page and a font with Japanese, into web-out/ (or OUT).
#
#   web/build.sh [OUT]
#
# The font: FONT=/path/to/font.(ttf|otf|ttc) FONT_FAMILY='its family' (e.g. on Windows C:/Windows/Fonts/YuGothR.ttc
# 'Yu Gothic'), FONT_MONO='its monospace family' (else the collection's "Mono ... JP" one, else FONT_FAMILY); else
# fontconfig's choice for Japanese (fc-match). It is copied next to the page, not into the program.
# wasm-bindgen: the CLI of the same version as Cargo.lock's wasm-bindgen (cargo install --locked wasm-bindgen-cli
# --version 0.2.130), on PATH or as WASM_BINDGEN.
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$here/web-out}
cd "$here"
cargo build --release -j"${JOBS:-1}" --target wasm32-unknown-unknown --bin wsk-iced
target=${CARGO_TARGET_DIR:-$here/target}
mkdir -p "$out/fonts"
"${WASM_BINDGEN:-wasm-bindgen}" --target web --no-typescript --out-dir "$out" \
  "$target/wasm32-unknown-unknown/release/wsk-iced.wasm"
cp web/index.html "$out/"
font=${FONT:-$(fc-match -f '%{file}' 'sans-serif:lang=ja')}
family=${FONT_FAMILY:-$(fc-query -f '%{family[0]}\n' "$font" | head -1)}
mono=${FONT_MONO:-$(fc-query -f '%{family[0]}\n' "$font" | grep -m1 'Mono.*JP' || echo "$family")}
ext=${font##*.}
cp "$font" "$out/fonts/ja.$ext"
printf '{"file": "fonts/ja.%s", "family": "%s", "mono": "%s"}\n' "$ext" "$family" "$mono" > "$out/fonts/font.json"
ls -l "$out" "$out/fonts"
echo "font: $family, monospace $mono ($font)"

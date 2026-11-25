#!/usr/bin/env bash
# Prototype 4 (Makepad 1.0) for the browser (bench/notes/2026-11-25-gui-study.ja.md §16): the wasm, Makepad's own JS
# and the resources (fonts) of the Makepad crates, and the page, into web-out/ (or OUT).
#
#   web/build.sh [OUT]
#
# What `cargo makepad wasm build` does (cargo-makepad 1.0.0, GitHub tools/cargo_makepad at the "1.0.0" commit
# 6a6466341; crates.io has only 0.4.0), done by hand on stable Rust: cargo-makepad builds with nightly, -Z build-std
# and the atomics (Makepad's threads are Web Workers on a shared memory); this program needs no threads on the web
# (model.rs runs the work in place), so a plain wasm32-unknown-unknown build is enough. The JS comes from the crates
# in cargo's registry (makepad-platform's src/os/web, makepad-wasm-bridge's wasm_bridge.js), the resources from each
# Makepad crate's resources/ to <crate_name>/resources/ (where Makepad's web build fetches them: crate://x/... is
# /x/... on the web). No wasm-bindgen.
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$here/web-out}
cd "$here"
# --allow-undefined: Makepad 1.0's own imports (js_console_log, js_time_now, ...) are plain extern "C" without a
# wasm_import_module, and today's Rust no longer lets the wasm linker leave them undefined (they become imports of
# `env` with it, as when Makepad 1.0 was made). Given to the program's own link only (cargo rustc), so the
# dependencies are not rebuilt.
cargo rustc --locked --release -j"${JOBS:-1}" --target wasm32-unknown-unknown -p proto-makepad --bin wsk-makepad -- \
  -C link-arg=--allow-undefined
target=${CARGO_TARGET_DIR:-$here/target}
mkdir -p "$out"
# the Makepad crates' directories as cargo resolved them for this lock
cargo metadata --locked --format-version 1 --filter-platform wasm32-unknown-unknown |
  python3 -c '
import json, sys
m = json.load(sys.stdin)
for p in m["packages"]:
    if p["name"].startswith("makepad-"):
        print(p["name"], p["manifest_path"].rsplit("/", 1)[0])
' | while read -r name dir; do
  case $name in
    makepad-wasm-bridge)
      mkdir -p "$out/makepad_wasm_bridge"
      cp "$dir/src/wasm_bridge.js" "$out/makepad_wasm_bridge/" ;;
    makepad-platform)
      mkdir -p "$out/makepad_platform"
      for f in web_gl.js web.js web_worker.js audio_worklet.js auto_reload.js full_canvas.css; do
        cp "$dir/src/os/web/$f" "$out/makepad_platform/"
      done ;;
  esac
  if [ -d "$dir/resources" ]; then
    dst="$out/${name//-/_}/resources"
    mkdir -p "$dst"
    cp -r "$dir/resources/." "$dst/"
  fi
done
cp "$target/wasm32-unknown-unknown/release/wsk-makepad.wasm" "$out/"
cp web/index.html "$out/"
du -sh "$out"/* | sort -h

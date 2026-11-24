"""Runs the core's scale example as WASM under wasmtime (bench/notes/2026-11-24-rust-plan.ja.md §10):
    uv run --no-project --with wasmtime python bench/rust_wasm_scale_wasmtime.py target/wasm32-wasip1/release/examples/scale.wasm 10000 100000
"""
import sys, wasmtime
eng = wasmtime.Engine()
store = wasmtime.Store(eng)
cfg = wasmtime.WasiConfig(); cfg.argv = ["scale", *sys.argv[2:]]; cfg.inherit_stdout(); cfg.inherit_stderr()
store.set_wasi(cfg)
linker = wasmtime.Linker(eng); linker.define_wasi()
inst = linker.instantiate(store, wasmtime.Module.from_file(eng, sys.argv[1]))
inst.exports(store)["_start"](store)

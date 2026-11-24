// Runs the core's scale example as WASM under node's WASI (bench/notes/2026-11-24-rust-plan.ja.md §6):
//   cargo build --release --example scale --target wasm32-wasip1
//   node bench/rust_wasm_scale.mjs target/wasm32-wasip1/release/examples/scale.wasm 10000 100000
import { readFile } from "node:fs/promises";
import { WASI } from "node:wasi";
import { argv } from "node:process";

const wasi = new WASI({ version: "preview1", args: ["scale", ...argv.slice(3)] });
const mod = await WebAssembly.compile(await readFile(argv[2]));
const inst = await WebAssembly.instantiate(mod, wasi.getImportObject());
wasi.start(inst);

// Runs the SAME wasm_plugin.wasm under V8 (Node; same engine as Chrome) to check
// browser portability. Usage (from plugin-lang-bench):
//   node scripts/wasm/run-node.mjs ../../corpus/work/zerocopy i2cdb-sht30-1b9dbf
// Browser: identical code with fetch() + WebAssembly.instantiateStreaming; samples
// must be copied into memory.buffer (no mmap in a browser).
import fs from "node:fs";
Error.stackTraceLimit = 64; // V8 default 10 hides inner/outer behind std panic frames
const [dir, id] = process.argv.slice(2);
const url = new URL("../../wasm-plugin/target/wasm32-unknown-unknown/release/wasm_plugin.wasm", import.meta.url);
let mem, panicMsg = null;
const { instance } = await WebAssembly.instantiate(fs.readFileSync(url), {
  host: {
    inc: (x) => x + 1,
    panic_msg: (p, n) => { panicMsg = new TextDecoder().decode(new Uint8Array(mem.buffer, p, n)); },
  },
});
const e = instance.exports; mem = e.memory; e.init();
const raw = fs.readFileSync(`${dir}/${id}.u16`);
let t = performance.now();
const p = e.alloc(raw.length);
new Uint8Array(mem.buffer, p, raw.length).set(raw);
const copyMs = performance.now() - t;
const n = Math.min(raw.length / 2, 2_000_000);
// busiest channel = largest edge file (same as Capture::busiest)
let ch = 0, best = -1;
for (let k = 0; fs.existsSync(`${dir}/${id}.ch${k}.u32`); k++) { const s = fs.statSync(`${dir}/${id}.ch${k}.u32`).size; if (s > best) { best = s; ch = k; } }
t = performance.now();
const c = e.scan(p, n, ch);
const scanMs = performance.now() - t;
t = performance.now();
let x = 0; for (let i = 0; i < 100000; i++) x = e.inc_export(x);
const callUs = (performance.now() - t) / 100;
t = performance.now(); e.host_inc_loop(100000); const hostUs = (performance.now() - t) / 100;
let err;
try { e.error_test(); } catch (ex) { err = `panic: ${panicMsg}\n${ex.stack}`; }
const full = e.scan(p, raw.length / 2, ch), edgeFile = best / 4;
console.log(JSON.stringify({ engine: "wasm-v8", capture: id, full_scan_ok: +(full === edgeFile), copy_ms: copyMs, ch, scan_edges: c, scan_ms: scanMs, t4_script_call_us: callUs, t4_host_call_us: hostUs, t5_error: err }));

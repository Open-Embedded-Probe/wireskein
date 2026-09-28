// Separate-process plugin host: loads plugins.js and serves requests on stdio.
// Request:  u32 LE header length, header JSON, then raw little-endian buffers
//           described by header.buffers = [{name, dtype: "u8"|"i32"|"i64"|"f64", n}].
//           header = {plugin, fields: {...}, buffers: [...]}; buffers become fields.
// Response: u32 LE length, JSON.
import fs from "node:fs";
import vm from "node:vm";
const here = new URL(".", import.meta.url).pathname;
vm.runInThisContext(fs.readFileSync(here + "plugins.js", "utf8") + "\nglobalThis.WS = WS;");
const WS = globalThis.WS;
const CT = { u8: Uint8Array, i32: Int32Array, i64: BigInt64Array, f64: Float64Array };

let buf = Buffer.alloc(0);
process.stdin.on("data", (chunk) => {
  buf = buf.length ? Buffer.concat([buf, chunk]) : chunk;
  for (;;) {
    if (buf.length < 4) return;
    const hl = buf.readUInt32LE(0);
    if (buf.length < 4 + hl) return;
    const header = JSON.parse(buf.subarray(4, 4 + hl).toString("utf8"));
    let need = 4 + hl;
    for (const b of header.buffers || []) need += b.n * CT[b.dtype].BYTES_PER_ELEMENT;
    if (buf.length < need) return;
    const fields = header.fields || {};
    let off = 4 + hl;
    for (const b of header.buffers || []) {
      const T = CT[b.dtype], bytes = b.n * T.BYTES_PER_ELEMENT;
      // copy into an aligned buffer (the stdin chunk offset is arbitrary)
      const ab = new ArrayBuffer(bytes);
      new Uint8Array(ab).set(buf.subarray(off, off + bytes));
      let arr = new T(ab);
      if (b.dtype === "i64") arr = Float64Array.from(arr, Number);
      fields[b.name] = arr;
      off += bytes;
    }
    buf = buf.subarray(need);
    let out;
    try { out = WS[header.plugin](fields); } catch (e) { out = { error: String(e) }; }
    const body = Buffer.from(JSON.stringify(out), "utf8");
    const len = Buffer.alloc(4); len.writeUInt32LE(body.length, 0);
    process.stdout.write(Buffer.concat([len, body]));
  }
});

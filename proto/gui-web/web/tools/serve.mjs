#!/usr/bin/env node
// A small static server for prototype 2 (no dependencies): the built screens with the headers that make the page
// cross-origin isolated (COOP / COEP: precise timers, memory measuring; SharedArrayBuffer if the wasm ever runs
// threads), .wasm as application/wasm, and capture files from other directories mounted under /files/ etc.
//
//   node tools/serve.mjs [--root dist] [--port 8080] [--files DIR] [--mount PREFIX=DIR ...] [--host 127.0.0.1]
//
// e.g. (in web/) node tools/serve.mjs --files ../../../tests/data --mount scratch=/path/to/captures
//      http://127.0.0.1:8080/?file=files/examples/mixed-1.wireskein

import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
let root = path.join(here, '..', 'dist');
let port = 8080;
let host = '127.0.0.1';
const mounts = {};
for (let i = 0; i < args.length; i++) {
  const a = args[i];
  const v = () => args[++i];
  if (a === '--root') root = path.resolve(v());
  else if (a === '--port') port = Number(v());
  else if (a === '--host') host = v();
  else if (a === '--files') mounts.files = path.resolve(v());
  else if (a === '--mount') {
    const [p, d] = v().split('=');
    mounts[p] = path.resolve(d);
  } else {
    console.error(`unknown argument ${a}`);
    process.exit(2);
  }
}

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.wasm': 'application/wasm',
  '.json': 'application/json',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.ico': 'image/x-icon',
};

function resolve(urlPath) {
  const p = decodeURIComponent(urlPath.split('?')[0]);
  const parts = p.split('/').filter(Boolean);
  if (parts.length > 1 && mounts[parts[0]]) {
    const base = mounts[parts[0]];
    const f = path.resolve(base, ...parts.slice(1));
    return f.startsWith(base + path.sep) ? f : null;
  }
  const f = path.resolve(root, ...parts);
  if (f !== root && !f.startsWith(root + path.sep)) return null;
  return f;
}

const server = http.createServer((req, res) => {
  let f = resolve(req.url ?? '/');
  if (f && fs.existsSync(f) && fs.statSync(f).isDirectory()) f = path.join(f, 'index.html');
  const headers = {
    'Cross-Origin-Opener-Policy': 'same-origin',
    'Cross-Origin-Embedder-Policy': 'require-corp',
    'Cross-Origin-Resource-Policy': 'same-origin',
    'Cache-Control': 'no-cache',
  };
  if (!f || !fs.existsSync(f)) {
    res.writeHead(404, { ...headers, 'Content-Type': 'text/plain' });
    res.end('not found\n');
    return;
  }
  const st = fs.statSync(f);
  res.writeHead(200, {
    ...headers,
    'Content-Type': TYPES[path.extname(f).toLowerCase()] ?? 'application/octet-stream',
    'Content-Length': st.size,
  });
  if (req.method === 'HEAD') return res.end();
  fs.createReadStream(f).pipe(res);
});
server.listen(port, host, () => {
  console.log(`serving ${root} at http://${host}:${port}/`);
  for (const [k, d] of Object.entries(mounts)) console.log(`  /${k}/ -> ${d}`);
});

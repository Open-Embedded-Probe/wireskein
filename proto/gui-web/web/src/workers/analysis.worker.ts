// The analysis worker: a capture's bytes analysed (wireskein-core / wireskein-analysis in wasm; a long part read in
// pieces), the layers handed to the screen's thread as typed arrays, and the search run here over the rows (the
// screen never waits for either).

import init, { Analysis } from '../wasm-pkg/engine/wsk_web_engine.js';
import { readSource } from './source';

const ready = init();
const post = (m: unknown, t: Transferable[] = []) => (self as unknown as Worker).postMessage(m, t);
let an: Analysis | null = null;

function send(ms: number) {
  const a = an!;
  const meta = JSON.parse(a.meta());
  const layers: unknown[] = [];
  const unread: unknown[] = [];
  const tr: Transferable[] = [];
  meta.claims.forEach((c: { layers: { name: string; columns: string[] }[]; unread: number }, ci: number) => {
    c.layers.forEach((ly, li) => {
      const s = a.layer_s(ci, li);
      const e = a.layer_e(ci, li);
      const status = a.layer_status(ci, li);
      const buf = a.layer_buf(ci, li);
      const off = a.layer_off(ci, li);
      tr.push(s.buffer, e.buffer, status.buffer, buf.buffer, off.buffer);
      const columns = ly.columns.map((name, k) => {
        const kind = a.column_kind(ci, li, k);
        if (kind === 0) {
          const data = a.column_num(ci, li, k);
          tr.push(data.buffer);
          return { name, kind, data };
        }
        if (kind === 1) {
          const data = a.column_bool(ci, li, k);
          tr.push(data.buffer);
          return { name, kind, data };
        }
        return { name, kind, data: a.column_str(ci, li, k) };
      });
      layers.push({ c: ci, l: li, name: ly.name, s, e, status, buf, off, columns });
    });
    if (c.unread > 0) {
      const s = a.unread_s(ci);
      const e = a.unread_e(ci);
      tr.push(s.buffer, e.buffer);
      unread.push({ c: ci, s, e });
    }
  });
  post({ type: 'analysis', meta, layers, unread, ms }, tr);
}

let memTimer = 0;
self.onmessage = async (ev: MessageEvent) => {
  const exp = await ready;
  if (!memTimer) {
    memTimer = setInterval(() => post({ type: 'mem', bytes: exp.memory.buffer.byteLength }), 1000) as unknown as number;
  }
  const m = ev.data;
  try {
    if (m.type === 'analyze') {
      const bytes = await readSource(m.src);
      const t = performance.now();
      an?.free();
      an = Analysis.run(bytes, m.part);
      send(performance.now() - t);
    } else if (m.type === 'fake') {
      const t = performance.now();
      an?.free();
      an = Analysis.fake(m.n, m.nTicks);
      send(performance.now() - t);
    } else if (m.type === 'search') {
      if (!an) return;
      const t = performance.now();
      const total = an.search(m.q);
      const pairs = an.hit_layers();
      const layers = [];
      for (let k = 0; k < pairs.length / 2; k++) layers.push({ c: pairs[2 * k], l: pairs[2 * k + 1], rows: an.hit_rows(k) });
      post({ type: 'search', id: m.id, q: m.q, total, layers, ticks: an.hit_ticks(), ms: performance.now() - t });
    }
  } catch (e) {
    post({ type: 'error', error: String(e) });
  }
};

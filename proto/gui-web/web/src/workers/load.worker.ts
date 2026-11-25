// The loading worker: a capture file read (wireskein-io in wasm) and handed to the screen's thread line by line,
// then each line's summary built a block at a time (view-core's level 0, the levels above built by the screen);
// or one made-up line's summary (the screen asks line after line, two of these workers at once).

import init, { AnalogParts, Capture, LodJob } from '../wasm-pkg/engine/wsk_web_engine.js';
import { readSource } from './source';

const ready = init();
const post = (m: unknown, t: Transferable[] = []) => (self as unknown as Worker).postMessage(m, t);

/** A job's buckets handed over about every 40 ms (and at its end). */
function runLod(i: number, job: LodJob) {
  post({ type: 'lod-begin', i, shift0: job.shift0() });
  let last = performance.now();
  for (;;) {
    const done = job.step(1);
    const now = performance.now();
    if (done || now - last > 40) {
      const counts = job.take_counts();
      const ends = job.take_ends();
      post({ type: 'lod', i, counts, ends, last: done, built: job.built_until() }, [counts.buffer, ends.buffer]);
      last = now;
    }
    if (done) break;
  }
  job.free();
}

/** An analog line with its summary built here (the screen only takes the arrays). */
function postAnalog(i: number, p: AnalogParts) {
  const values = p.take_values();
  const levels = p.take_levels();
  const lens = p.lens();
  p.free();
  post({ type: 'analog', i, values, levels, lens }, [values.buffer, levels.buffer, lens.buffer]);
}

let memTimer = 0;
self.onmessage = async (ev: MessageEvent) => {
  const exp = await ready;
  if (!memTimer) {
    memTimer = setInterval(() => post({ type: 'mem', bytes: exp.memory.buffer.byteLength }), 1000) as unknown as number;
  }
  const m = ev.data;
  try {
    if (m.type === 'open') {
      const t0 = performance.now();
      const bytes = await readSource(m.src);
      const t1 = performance.now();
      const cap = new Capture(bytes, m.part);
      const t2 = performance.now();
      const n = cap.n_lines();
      const lines = [];
      let total = 0;
      for (let i = 0; i < n; i++) {
        lines.push({ name: cap.line_name(i), initial: cap.line_initial(i) });
        total += cap.line_n_edges(i);
      }
      const analog = [];
      for (let i = 0; i < cap.n_analog(); i++) {
        analog.push({ name: cap.analog_name(i), unit: cap.analog_unit(i), t0: cap.analog_t0(i), dt: cap.analog_dt(i) });
      }
      post({
        type: 'meta', tickHz: cap.tick_hz(), nTicks: cap.n_ticks(), nParts: cap.n_parts(), lines, analog, total,
        complete: cap.complete(), lost: cap.lost(), fetchMs: t1 - t0, parseMs: t2 - t1, bytes: bytes.length,
      });
      for (let i = 0; i < n; i++) {
        const edges = cap.line_edges(i);
        post({ type: 'edges', i, initial: lines[i].initial, edges }, [edges.buffer]);
      }
      for (let i = 0; i < analog.length; i++) postAnalog(i, AnalogParts.of(cap.analog_values(i)));
      const t3 = performance.now();
      for (let i = 0; i < n; i++) runLod(i, cap.lod_job(i, 32));
      cap.free();
      post({ type: 'lod-done', ms: performance.now() - t3 });
    } else if (m.type === 'synth-analog') {
      const t = performance.now();
      postAnalog(0, AnalogParts.synth(m.n, m.dt));
      post({ type: 'analog-ms', ms: performance.now() - t });
    } else if (m.type === 'synth-line') {
      runLod(m.i, LodJob.synth(m.line, m.nTicks, m.period, m.segLen, 32));
      post({ type: 'line-done', i: m.i });
    }
  } catch (e) {
    post({ type: 'error', error: String(e) });
  }
};

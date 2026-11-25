// The bench (`?bench=1`): prototype 1's script on the first waveform view: 8 zooms (the whole capture, then 10x
// steps) x 96 frames each panning 7 px a frame, then a zoom sweep from the whole capture in at 6 % a frame (new
// tiles all the time). The frame times are the animation frames' (vsync-bound in a browser: 60 fps at most on a
// 60 Hz screen); "waveform CPU" is the time to make and submit a frame (wasm + GPU commands + the text canvas).
// The result: shown in the speed panel, logged, and in window.__bench (tools/bench.mjs reads it).

import { analysis, analyzing, cap, frameLog, lod, onFrame, perf, report, requestDraw, scene, setContinuous, viewMemory } from './state';
import { type Viewport, fit, zoomed, clamped } from './viewport';
import { timings } from './engine';
import { createSignal } from 'solid-js';

/** The view the bench forces on one waveform panel (the first drawn). */
export const forced = { panel: null as number | null, vp: null as Viewport | null, width: 1000, reset: false };

export const [benchText, setBenchText] = createSignal('');

type Phase = { k: 'wait' } | { k: 'zoom'; zi: number; f: number } | { k: 'sweep'; f: number } | { k: 'done' };

export function startBench() {
  let phase: Phase = { k: 'wait' };
  const zooms = [1, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7];
  let samples: number[] = [];
  let wave: number[] = [];
  let wasm: number[] = [];
  const lines: string[] = [];
  const t0 = performance.now();
  setContinuous(true);
  requestDraw();
  const pct = (s: number[], q: number) => {
    const a = [...s].sort((x, y) => x - y);
    return a[Math.floor((a.length - 1) * q)] ?? NaN;
  };
  const avg = (s: number[]) => s.reduce((a, b) => a + b, 0) / Math.max(1, s.length);
  onFrame((_now, dt) => {
    const c = cap();
    const sc = scene;
    if (!c || !sc || phase.k === 'done') return;
    const n = c.nTicks;
    const w = forced.width;
    const lastWave = frameLog.wave[frameLog.wave.length - 1] ?? 0;
    const lastWasm = frameLog.wasm[frameLog.wasm.length - 1] ?? 0;
    if (phase.k === 'wait') {
      const ready = lod().ms !== null && !analyzing();
      if (ready || performance.now() - t0 > 600_000) {
        lines.push(
          `# ${c.title} (${c.lines.length} lines, ${c.total} changes, ${c.nTicks} ticks): first paint ${perf.firstPaint?.toFixed(0)} ms, ` +
            `read ${timings.readMs?.toFixed(0) ?? '-'} ms + parse ${timings.parseMs?.toFixed(0) ?? '-'} ms, summaries ${((timings.lodMs ?? NaN) / 1e3).toFixed(2)} s, ` +
            `analysis ${analysis() ? (analysis()!.seconds).toFixed(2) + ' s (in the worker; ' + ((timings.analysisMs ?? NaN) / 1e3).toFixed(2) + ' s to the screen)' : '-'}, renderer ${perf.renderer}`,
        );
        lines.push('| ticks/px | fps (median) | frame ms p50 / p95 | waveform CPU ms (of which wasm) |');
        lines.push('|---|---|---|---|');
        phase = { k: 'zoom', zi: 0, f: 0 };
        console.log(`bench: ${lines[0]}`);
      }
      return;
    }
    if (phase.k === 'zoom') {
      const { zi, f } = phase;
      if (f > 5) {
        samples.push(dt);
        wave.push(lastWave);
        wasm.push(lastWasm);
      }
      const z = zooms[zi];
      let vp = zoomed(fit(n, w), z, w * 0.37);
      vp = { ...vp, t0: vp.t0 + f * 7 * vp.tpp };
      forced.vp = clamped(vp, n);
      if (f >= 95) {
        lines.push(`| ${vp.tpp.toFixed(3)} | ${(1000 / pct(samples, 0.5)).toFixed(0)} | ${pct(samples, 0.5).toFixed(1)} / ${pct(samples, 0.95).toFixed(1)} | ${avg(wave).toFixed(2)} (${avg(wasm).toFixed(2)}) |`);
        console.log(`bench: ${lines[lines.length - 1]}`);
        samples = [];
        wave = [];
        wasm = [];
        phase = zi + 1 < zooms.length && zooms[zi + 1] <= (n * 20) / w ? { k: 'zoom', zi: zi + 1, f: 0 } : { k: 'sweep', f: 0 };
      } else phase = { k: 'zoom', zi, f: f + 1 };
      return;
    }
    if (phase.k === 'sweep') {
      const { f } = phase;
      if (f > 3) {
        samples.push(dt);
        wave.push(lastWave);
      }
      const vp = zoomed(fit(n, w), Math.pow(1.06, f), w * 0.37);
      forced.vp = clamped(vp, n);
      if (f >= 240 || vp.tpp < 0.05) {
        lines.push(`zoom sweep (6 %/frame): frame ms p50 ${pct(samples, 0.5).toFixed(1)}, p95 ${pct(samples, 0.95).toFixed(1)}, max ${Math.max(...samples).toFixed(1)}; waveform CPU ms ${avg(wave).toFixed(2)} avg, ${Math.max(...wave).toFixed(2)} max`);
        const wm = Object.entries(perf.workerMem).map(([k, v]) => `${k} ${(v / 1e6).toFixed(0)}`).join(', ');
        const heap = (performance as unknown as { memory?: { usedJSHeapSize: number } }).memory?.usedJSHeapSize;
        lines.push(
          `memory: scene ${(sc.mem_bytes() / 1e6).toFixed(0)} MB, view wasm ${((viewMemory?.buffer.byteLength ?? 0) / 1e6).toFixed(0)} MB, ` +
            `workers' wasm MB: ${wm || '-'}, JS heap ${heap ? (heap / 1e6).toFixed(0) + ' MB' : '-'}`,
        );
        forced.vp = null;
        forced.reset = true;
        phase = { k: 'done' };
        setContinuous(false);
        const text = lines.join('\n') + '\n';
        console.log(text);
        setBenchText(text);
        (window as unknown as { __bench: string }).__bench = text;
        report('bench', { text });
      } else phase = { k: 'sweep', f: f + 1 };
    }
  });
}

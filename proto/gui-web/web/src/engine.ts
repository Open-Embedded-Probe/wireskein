// Opening a capture: the workers started (a loading worker or two summary workers, an analysis worker), what they
// hand over put into the view's wasm scene and the shared state. Nothing heavy runs on the screen's thread: it only
// receives lines, buckets and layers (typed arrays moved, not copied, between the threads).

import initView, { Scene, synth_plan } from './wasm-pkg/view/wsk_web_view.js';
import {
  type AnalysisData, type ClaimData, type Column, type LayerData, type Source,
  cap, setAnalysis, setAnalyzing, setCap, setCapGen, capGen, setLod, setPerf, setScene, scene, openedAt,
  requestDraw, search, setSearch, select, setSel, setMeasure,
} from './state';
import { batch } from 'solid-js';
import { NativeWorker, fileHref, frameIpc, nativeEngine } from './tauri';
import { invoke } from '@tauri-apps/api/core';

export const viewReady = initView();

type AnyWorker = Worker | NativeWorker;
let workers: AnyWorker[] = [];
let analysisWorker: AnyWorker | null = null;
// prototype 2b (Tauri, ?engine=native): the shell does the workers' work natively
const newWorker = (kind: 'load' | 'analysis'): Worker =>
  nativeEngine
    ? (new NativeWorker(kind) as unknown as Worker)
    : kind === 'load'
    ? new Worker(new URL('./workers/load.worker.ts', import.meta.url), { type: 'module' })
    : new Worker(new URL('./workers/analysis.worker.ts', import.meta.url), { type: 'module' });

function stopWorkers() {
  for (const w of workers) w.terminate();
  workers = [];
  analysisWorker = null;
}

/** A row asked for (?select=ROW) once the analysis is in. */
let pendingSelect: number | null = null;
export function selectWhenReady(row: number) {
  pendingSelect = row;
}

export const timings = { lodStart: 0, analysisStart: 0, analysisMs: null as number | null, readMs: null as number | null, parseMs: null as number | null, lodMs: null as number | null, bytes: 0 };

export async function open(src: Source, part = 0) {
  const exp = await viewReady;
  stopWorkers();
  batch(() => {
    setScene(null, exp.memory);
    setCap(null);
    setAnalysis(null);
    setAnalyzing(false);
    setSel(null);
    setMeasure(null);
    setCapGen(capGen() + 1);
    setLod({ done: 0, total: 0, ms: null });
    setPerf({ firstPaint: null, readMs: null });
    setSearch({ total: 0, layers: [], cur: 0 });
  });
  openedAt.t = performance.now();
  Object.assign(timings, { lodStart: performance.now(), analysisStart: performance.now(), analysisMs: null, readMs: null, parseMs: null, lodMs: null, bytes: 0 });
  const aw = newWorker('analysis');
  workers.push(aw);
  analysisWorker = aw;
  aw.onmessage = (ev) => onAnalysis(ev.data);
  setAnalyzing(true);
  if (src.kind === 'synth') openSynth(src.total, src.items, src);
  else openFile(src, part);
  requestDraw();
}

function workerMem(name: string, bytes: number) {
  setPerf('workerMem', name, bytes);
}

// ------------------------------------------------------------------------------------------------ files

function openFile(src: Source & { kind: 'file' | 'url' }, part: number) {
  const s = src.kind === 'file' ? src.file : fileHref(src.url);
  const title = src.kind === 'file' ? src.file.name : decodeURIComponent(src.url.split('/').pop() ?? src.url);
  const lw = newWorker('load');
  workers.push(lw);
  analysisWorker!.postMessage({ type: 'analyze', src: s, part });
  lw.postMessage({ type: 'open', src: s, part });
  let sc: Scene | null = null;
  let nLines = 0;
  let got = 0;
  let pendingAnalog: { name: string; unit: string; t0: number; dt: number }[] = [];
  let lodDone = 0;
  lw.onmessage = (ev) => {
    const m = ev.data;
    if (m.type === 'mem') return workerMem('読む', m.bytes);
    if (m.type === 'error') {
      console.error(m.error);
      alert(m.error);
      return;
    }
    if (m.type === 'meta') {
      timings.readMs = m.fetchMs;
      timings.parseMs = m.parseMs;
      timings.bytes = m.bytes;
      setPerf('readMs', m.fetchMs + m.parseMs);
      sc = new Scene(m.nTicks);
      nLines = m.lines.length;
      pendingAnalog = m.analog;
      setLod({ done: 0, total: nLines, ms: null });
      setCap({
        title, tickHz: m.tickHz, nTicks: m.nTicks, lines: m.lines.map((l: { name: string }) => l.name),
        analog: m.analog.map((a: { name: string; unit: string }) => ({ name: a.name, unit: a.unit })),
        nParts: m.nParts, part, total: m.total, complete: m.complete, lost: m.lost === '[]' ? '' : m.lost, synth: false, source: src,
      });
      if (nLines === 0 && pendingAnalog.length === 0) setScene(sc, null);
    } else if (m.type === 'edges') {
      sc!.add_vec_line(m.initial, m.edges);
      got++;
      // drawn from the changes themselves (two searches a pixel) before the summaries are built
      if (got === nLines && pendingAnalog.length === 0) setScene(sc, null);
      requestDraw();
    } else if (m.type === 'analog') {
      const a = pendingAnalog[m.i];
      sc!.add_analog_parts(a.name, a.unit, a.t0, a.dt, m.values, m.levels, m.lens);
      if (m.i === pendingAnalog.length - 1 && got === nLines) setScene(sc, null);
      requestDraw();
    } else if (m.type === 'lod-begin') {
      sc!.lod_begin(m.i, m.shift0);
    } else if (m.type === 'lod') {
      sc!.lod_append(m.i, m.counts, m.ends, m.last);
      if (m.last) {
        lodDone++;
        setLod({ done: lodDone, total: nLines, ms: null });
      }
      requestDraw();
    } else if (m.type === 'lod-done') {
      timings.lodMs = m.ms;
      setLod({ done: nLines, total: nLines, ms: m.ms });
      // its wasm memory (the file's bytes and lines, never given back) let go
      setTimeout(() => lw.terminate(), 1500);
      requestDraw();
    }
  };
}

// ------------------------------------------------------------------------------------------------ made up

function openSynth(total: number, items: number, src: Source) {
  const plan = synth_plan(total, 16);
  if (frameIpc) invoke('scene_synth', { total });
  const n = plan[0];
  const sc = new Scene(n);
  const names = [];
  for (let i = 0; i < 16; i++) {
    sc.add_synth_line(i, plan[1 + 2 * i], plan[2 + 2 * i]);
    names.push(`D${i}`);
  }
  // the analog line: made here (4 million samples; its summary built here too, ~0.1 s)
  const nS = 4_000_000;
  const dt = n / nS;
  setScene(sc, null);
  setCap({
    title: `synth ${total.toExponential(0)}`, tickHz: 1e9, nTicks: n, lines: names, analog: [{ name: 'A0', unit: 'V' }],
    nParts: 1, part: 0, total, complete: true, lost: '', synth: true, source: src,
  });
  setPerf('readMs', 0);
  analysisWorker!.postMessage({ type: 'fake', n: items, nTicks: n });
  // two summary workers take the lines one after another
  const queue = Array.from({ length: 16 }, (_, i) => i);
  let done = 0;
  const t0 = performance.now();
  const give = (w: Worker) => {
    const i = queue.shift();
    if (i === undefined) return;
    w.postMessage({ type: 'synth-line', i, line: i, nTicks: n, period: plan[1 + 2 * i], segLen: plan[2 + 2 * i] });
  };
  for (let k = 0; k < 2; k++) {
    const w = newWorker('load');
    workers.push(w);
    w.onmessage = (ev) => {
      const m = ev.data;
      if (m.type === 'mem') return workerMem(`要約 ${k + 1}`, m.bytes);
      if (scene !== sc) return;
      if (m.type === 'lod-begin') sc.lod_begin(m.i, m.shift0);
      else if (m.type === 'lod') {
        sc.lod_append(m.i, m.counts, m.ends, m.last);
        requestDraw();
      } else if (m.type === 'line-done') {
        done++;
        const ms = done === 16 ? performance.now() - t0 : null;
        if (ms !== null) timings.lodMs = ms;
        setLod({ done, total: 16, ms });
        give(w);
      }
    };
    give(w);
  }
  setLod({ done: 0, total: 16, ms: null });
  // the analog line and its summary made in a worker of its own, the arrays taken here
  const aw = newWorker('load');
  workers.push(aw);
  aw.onmessage = (ev) => {
    const m = ev.data;
    if (m.type === 'analog' && scene === sc) {
      const t = performance.now();
      sc.add_analog_parts('A0', 'V', 0, dt, m.values, m.levels, m.lens);
      console.log(`analog: taken in ${(performance.now() - t).toFixed(0)} ms on the screen's thread`);
      requestDraw();
    } else if (m.type === 'analog-ms') {
      console.log(`analog: made in ${m.ms.toFixed(0)} ms in a worker`);
      aw.terminate();
    }
  };
  aw.postMessage({ type: 'synth-analog', n: nS, dt });
}

// ------------------------------------------------------------------------------------------------ the analysis

function onAnalysis(m: any) {
  if (m.type === 'mem') return workerMem('解析', m.bytes);
  if (m.type === 'error') {
    console.error(m.error);
    setAnalysis({ claims: [], error: m.error, fake: false, split: false, seconds: 0 });
    setAnalyzing(false);
    return;
  }
  if (m.type === 'search') return onSearch(m);
  if (m.type !== 'analysis') return;
  const sc = scene;
  const t = performance.now();
  const claims: ClaimData[] = m.meta.claims.map((c: any) => ({
    protocol: c.protocol, verdict: c.verdict, score: c.score, roles: c.roles, anchor: c.anchor ?? null, bus: c.bus ?? null,
    layers: [] as LayerData[], unread: c.unread,
  }));
  sc?.clear_analysis();
  for (const ly of m.layers) {
    const d: LayerData = {
      name: ly.name, n: ly.s.length, s: ly.s, e: ly.e, status: ly.status, buf: ly.buf, off: ly.off,
      columns: ly.columns as Column[],
    };
    claims[ly.c].layers[ly.l] = d;
    sc?.set_track(ly.c, ly.l, ly.name, ly.s, ly.e, ly.status, ly.buf, ly.off);
  }
  for (const u of m.unread) sc?.set_unread(u.c, u.s, u.e);
  const data: AnalysisData = { claims, error: m.meta.error ?? null, fake: m.meta.fake, split: m.meta.split, seconds: m.ms / 1e3 };
  timings.analysisMs = performance.now() - timings.analysisStart;
  console.log(`analysis: ${(m.ms / 1e3).toFixed(2)} s in the worker, ${(performance.now() - t).toFixed(0)} ms to take it in, ${claims.length} claims, ${m.layers.reduce((a: number, l: any) => a + l.s.length, 0)} rows`);
  batch(() => {
    setAnalysis(data);
    setAnalyzing(false);
  });
  if (search.query.trim()) runSearch(search.query);
  if (pendingSelect !== null && claims.length && claims[0].layers.length) {
    select({ c: 0, l: 0, row: Math.min(pendingSelect, claims[0].layers[0].n - 1) }, 999_999);
    pendingSelect = null;
  }
  requestDraw();
}

let searchId = 0;
let searchSent = 0;
export function runSearch(q: string) {
  setSearch('query', q);
  if (!analysisWorker) return;
  searchSent = performance.now();
  analysisWorker.postMessage({ type: 'search', q, id: ++searchId });
}

function onSearch(m: any) {
  if (m.id !== searchId) return;
  const sc = scene;
  sc?.clear_hits();
  for (const h of m.layers) sc?.set_hits(h.c, h.l, h.rows);
  sc?.set_hit_ticks(m.ticks);
  setSearch({ total: m.total, ms: m.ms, layers: m.layers, cur: 0 });
  console.log(`search "${m.q}": ${m.total} hits, ${m.ms.toFixed(1)} ms in the worker, ${(performance.now() - searchSent).toFixed(1)} ms round trip`);
  requestDraw();
}

export function reopenPart(part: number) {
  const c = cap();
  if (c) open(c.source, part);
}

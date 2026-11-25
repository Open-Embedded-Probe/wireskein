// What every panel shares (prototype 1's `Shared`): the capture's description, the analysis' layers, the choice,
// markers, the cursor's measure, the search, the theme, the speed numbers. The big arrays are plain (not reactive);
// a signal tells their version.

import { createSignal } from 'solid-js';
import { createStore } from 'solid-js/store';
import type { Scene } from './wasm-pkg/view/wsk_web_view.js';
import { DARK, LIGHT, paletteFloats, type Pal } from './gpu';

export interface CapMeta {
  title: string;
  tickHz: number;
  nTicks: number;
  lines: string[];
  analog: { name: string; unit: string }[];
  nParts: number;
  part: number;
  total: number;
  complete: boolean;
  lost: string;
  synth: boolean;
  /** what opened it, to open another part */
  source: Source;
}

export type Source = { kind: 'file'; file: File } | { kind: 'url'; url: string } | { kind: 'synth'; total: number; items: number };

export type Column =
  | { name: string; kind: 0; data: Float64Array }
  | { name: string; kind: 1; data: Uint8Array }
  | { name: string; kind: 2; data: string[] };

export interface LayerData {
  name: string;
  n: number;
  s: Float64Array;
  e: Float64Array;
  status: Uint8Array;
  buf: Uint8Array;
  off: Uint32Array;
  columns: Column[];
}

export interface ClaimData {
  protocol: string;
  verdict: string;
  score: number;
  roles: [string, string][];
  anchor: number | null;
  bus: string | null;
  layers: LayerData[];
  unread: number;
}

export interface AnalysisData {
  claims: ClaimData[];
  error: string | null;
  fake: boolean;
  split: boolean;
  seconds: number;
}

const dec = new TextDecoder();
export function itemText(l: LayerData, i: number): string {
  return dec.decode(l.buf.subarray(l.off[2 * i], l.off[2 * i + 1]));
}

export interface Sel {
  c: number;
  l: number;
  row: number;
}

export interface Measure {
  line: number;
  t: number;
  level: number;
  prev: number;
  next: number;
  next2: number;
}

// ------------------------------------------------------------------------------------------------ signals

export const [cap, setCap] = createSignal<CapMeta | null>(null);
/** bumped for each capture opened (views start over) */
export const [capGen, setCapGen] = createSignal(0);
export const [analysis, setAnalysis] = createSignal<AnalysisData | null>(null);
export const [analyzing, setAnalyzing] = createSignal(false);

export const [sel, setSel] = createSignal<(Sel & { src: number; gen: number }) | null>(null);
let selGen = 0;
export function select(s: Sel, src: number) {
  setSel({ ...s, src, gen: ++selGen });
  requestDraw();
}

export const [markers, setMarkers] = createSignal<[number | null, number | null]>([null, null]);
export const [measure, setMeasure] = createSignal<Measure | null>(null);

export interface SearchState {
  query: string;
  total: number;
  ms: number;
  /** per layer with hits: claim, layer, rows (sorted) */
  layers: { c: number; l: number; rows: Uint32Array }[];
  cur: number;
}
export const [search, setSearch] = createStore<SearchState>({ query: '', total: 0, ms: 0, layers: [], cur: 0 });

export function hitsOf(c: number, l: number): Uint32Array | null {
  return search.layers.find((h) => h.c === c && h.l === l)?.rows ?? null;
}

/** The search's hit `k` over the layers in order. */
export function hitAt(k: number): Sel | null {
  for (const h of search.layers) {
    if (k < h.rows.length) return { c: h.c, l: h.l, row: h.rows[k] };
    k -= h.rows.length;
  }
  return null;
}

const params = new URLSearchParams(location.search);
export const [dark, setDark] = createSignal(params.get('light') !== '1');
export const pal = (): Pal => (dark() ? DARK : LIGHT);
export const palFloats = () => paletteFloats(pal());

export const [tilesOn, setTilesOn] = createSignal(params.get('notiles') !== '1');
export const [continuous, setContinuous] = createSignal(params.has('bench'));

// ------------------------------------------------------------------------------------------------ the scene

/** The view's wasm scene of the capture open (null before one is). */
export let scene: Scene | null = null;
export let viewMemory: WebAssembly.Memory | null = null;
export function setScene(s: Scene | null, mem: WebAssembly.Memory | null) {
  scene?.free();
  scene = s;
  if (mem) viewMemory = mem;
}

export interface LodProgress {
  done: number;
  total: number;
  ms: number | null;
}
export const [lod, setLod] = createSignal<LodProgress>({ done: 0, total: 0, ms: null });

// ------------------------------------------------------------------------------------------------ speed

export interface Perf {
  fps: number;
  frameMs: number;
  worstMs: number;
  waveMs: number;
  wasmMs: number;
  shapes: number;
  inst: number;
  tiles: [number, number, number];
  firstPaint: number | null;
  readMs: number | null;
  renderer: string;
  workerMem: Record<string, number>;
}
export const [perf, setPerf] = createStore<Perf>({
  fps: 0, frameMs: 0, worstMs: 0, waveMs: 0, wasmMs: 0, shapes: 0, inst: 0, tiles: [0, 0, 0],
  firstPaint: null, readMs: null, renderer: '', workerMem: {},
});
export const openedAt = { t: performance.now() };

/** Frame times kept here (not reactive) and given to the store a few times a second. */
export const frameLog = { frames: [] as [number, number][], wave: [] as number[], wasm: [] as number[] };
export function logFrame(now: number, dtMs: number, waveMs: number, wasmMs: number) {
  const f = frameLog.frames;
  f.push([now, dtMs]);
  while (f.length && now - f[0][0] > 1000) f.shift();
  frameLog.wave.push(waveMs);
  frameLog.wasm.push(wasmMs);
  if (frameLog.wave.length > 120) frameLog.wave.shift();
  if (frameLog.wasm.length > 120) frameLog.wasm.shift();
}
setInterval(() => {
  const f = frameLog.frames;
  const avg = (a: number[]) => a.reduce((x, y) => x + y, 0) / Math.max(1, a.length);
  setPerf({
    fps: f.length,
    frameMs: avg(f.map((x) => x[1])),
    worstMs: f.reduce((m, x) => Math.max(m, x[1]), 0),
    waveMs: avg(frameLog.wave),
    wasmMs: avg(frameLog.wasm),
  });
}, 500);

// ------------------------------------------------------------------------------------------------ drawing

/** A view's drawing: its CPU time [whole ms, of which in wasm ms]. */
type Drawer = () => [number, number] | null;
const drawers = new Set<Drawer>();
let drawQueued = false;
let lastFrame = 0;
const frameHooks: ((now: number, dt: number) => void)[] = [];
/** Every waveform view drawn on the next animation frame. */
export function requestDraw() {
  if (drawQueued) return;
  drawQueued = true;
  requestAnimationFrame((now) => {
    drawQueued = false;
    const dt = lastFrame ? now - lastFrame : 0;
    lastFrame = now;
    // hooks first (the bench sets the view it measures)
    for (const h of frameHooks) h(now, dt);
    let w = 0;
    let m = 0;
    for (const d of drawers) {
      const r = d();
      if (r) {
        w += r[0];
        m += r[1];
      }
    }
    // (a frame after an idle while is not counted in the frame times)
    if (dt > 0 && dt < 250) logFrame(now, dt, w, m);
    if (continuous()) requestDraw();
  });
}
export function addDrawer(f: Drawer) {
  drawers.add(f);
  return () => drawers.delete(f);
}
export function onFrame(f: (now: number, dt: number) => void) {
  frameHooks.push(f);
}

/** `?report=1`: what the page measured sent to tools/serve.mjs (POST /report), for browsers whose console is out
 * of reach. */
export function report(what: string, data: object) {
  if (params.get('report') !== '1') return;
  const body = JSON.stringify({ what, ua: navigator.userAgent, webgpu: 'gpu' in navigator, ...data });
  fetch('./report', { method: 'POST', body }).catch(() => {});
}

// for profiling from a driver (tools/ and the console)
(window as unknown as { __wsk: object }).__wsk = {
  get scene() {
    return scene;
  },
  cap,
  analysis,
};

// The waveform panel (prototype 1's wave.rs): minimap, ruler, digital lines (view-core's summaries by tiles, in
// wasm), analog bands, annotation lanes (3 layers a claim), unread stretches, the cursor's measure, two markers, and
// the mouse and keys. The shapes come from wasm (Scene.frame) and go to the GPU in one draw; text goes on a 2D
// canvas above (the browser's fonts: Japanese without carrying a font).

import { frameIpc, lastFrame } from './tauri';
import { createSignal, onCleanup, onMount, Show } from 'solid-js';
import {
  analysis, addDrawer, cap, capGen, markers, measure, openedAt, pal, palFloats, perf, requestDraw, scene, select,
  sel, setMarkers, setMeasure, setPerf, tilesOn, viewMemory, lod, report,
} from './state';
import { type Renderer, makeRenderer, verdictColor, PALETTE_KEYS, gpuInfo, addGpuShot } from './gpu';
import { type Viewport, clamped, fit, tOf, xOf, zoomed } from './viewport';
import { fmtFreq, fmtRuler, fmtTime, niceStep } from './fmt';
import { forced } from './bench';

export const LABEL_W = 132;
const MINIMAP_H = 26;
const RULER_H = 22;
const DIG_H = 26;
const ANN_H = 19;
const UNREAD_H = 6;
const ANA_H = 64;
const LABEL_FONT = '11px ui-monospace, "Noto Sans Mono CJK JP", "DejaVu Sans Mono", monospace';
const UI_FONT = '"Noto Sans CJK JP", "Noto Sans JP", system-ui, sans-serif';

type Lane = { kind: 0 | 1 | 2 | 3; a: number; b: number; h: number };

function lanes(): Lane[] {
  const c = cap();
  const sc = scene;
  if (!c || !sc) return [];
  const out: Lane[] = [];
  const claims = analysis()?.claims ?? [];
  const addClaim = (ci: number) => {
    const cl = claims[ci];
    for (let li = 0; li < Math.min(3, cl.layers.length); li++) out.push({ kind: 2, a: ci, b: li, h: ANN_H });
    if (cl.unread > 0) out.push({ kind: 3, a: ci, b: 0, h: UNREAD_H });
  };
  const nl = Math.min(sc.n_lines(), c.lines.length);
  for (let i = 0; i < nl; i++) {
    out.push({ kind: 0, a: i, b: 0, h: DIG_H });
    claims.forEach((cl, ci) => cl.anchor === i && addClaim(ci));
  }
  claims.forEach((cl, ci) => cl.anchor === null && addClaim(ci));
  for (let i = 0; i < sc.n_analog(); i++) out.push({ kind: 1, a: i, b: 0, h: ANA_H });
  return out;
}

export interface WaveParams {
  follow: boolean;
}

export function WavePanel(props: { id: number; params: WaveParams; onParams: (p: WaveParams) => void }) {
  let host!: HTMLDivElement;
  let gpuCanvas!: HTMLCanvasElement;
  let over!: HTMLCanvasElement;
  const [follow, setFollow] = createSignal(props.params.follow);
  const [scaleText, setScaleText] = createSignal('');
  const [menu, setMenu] = createSignal<{ x: number; y: number; t: number } | null>(null);
  const [err, setErr] = createSignal('');
  let renderer: Renderer | null = null;
  const st = {
    vp: null as Viewport | null,
    vel: 0,
    dragHist: [] as [number, number][],
    drag: null as null | { kind: 'pan' | 'mm' | 'sel'; x0: number; lastX: number; moved: boolean; button: number },
    scrollY: 0,
    seenGen: 0,
    capGen: -1,
    hover: null as { x: number; y: number } | null,
    w: 0,
    h: 0,
    dpr: 1,
    last: 0,
    zoomDone: false,
    charPx: 6.6,
    laneRects: [] as { lane: Lane; y: number }[],
    lastScale: '',
  };
  const empty = new Float32Array(0);

  const plotW = () => Math.max(10, st.w - LABEL_W);

  function resize() {
    const r = host.getBoundingClientRect();
    st.w = Math.floor(r.width);
    st.h = Math.floor(r.height);
    st.dpr = window.devicePixelRatio || 1;
    for (const cv of [gpuCanvas, over]) {
      cv.width = Math.max(1, Math.round(st.w * st.dpr));
      cv.height = Math.max(1, Math.round(st.h * st.dpr));
      cv.style.width = `${st.w}px`;
      cv.style.height = `${st.h}px`;
    }
    requestDraw();
  }

  // ------------------------------------------------------------------------------------------------ drawing

  function draw(): [number, number] | null {
    if (!renderer || st.w < 2 || st.h < 2) return null;
    const t0 = performance.now();
    const c = cap();
    const sc = scene;
    const pf = palFloats();
    const g = over.getContext('2d')!;
    g.setTransform(st.dpr, 0, 0, st.dpr, 0, 0);
    g.clearRect(0, 0, st.w, st.h);
    const P = pal();
    if (!c || !sc || sc.n_lines() < c.lines.length) {
      renderer.draw(empty, 0, st.w, st.h, pf);
      g.fillStyle = P.dim;
      g.font = `13px ${UI_FONT}`;
      g.textAlign = 'center';
      g.fillText(c ? '読み込み中…' : 'ファイルを開いてください（上の「開く」か、ここへドラッグ）', st.w / 2, st.h / 2);
      g.textAlign = 'left';
      return null;
    }
    if (st.capGen !== capGen()) {
      Object.assign(st, { vp: null, vel: 0, scrollY: 0, seenGen: sel()?.gen ?? 0, capGen: capGen(), zoomDone: false });
    }
    const n = c.nTicks;
    const plotL = LABEL_W;
    const pw = plotW();
    let vp = st.vp ?? fit(n, pw);
    if (Math.abs(vp.width - pw) > 0.5) vp = { t0: vp.t0, tpp: (vp.tpp * vp.width) / pw, width: pw };
    // ?zoom=Z&at=F: the view asked for at the start
    const q = new URLSearchParams(location.search);
    if (!st.zoomDone && q.has('zoom')) {
      st.zoomDone = true;
      const z = Number(q.get('zoom'));
      const at = Number(q.get('at') ?? 0.5);
      const span = n / z;
      vp = { t0: at * n - span / 2, tpp: span / pw, width: pw };
    }
    // follow the list's choice
    const s = sel();
    if (s && s.gen !== st.seenGen) {
      st.seenGen = s.gen;
      if (follow() && s.src !== props.id) {
        const it = sc.item(s.c, s.l, s.row);
        if (it.length) {
          const span = vp.tpp * vp.width;
          const w = it[1] - it[0];
          if (w * 3 > span || w * 2000 < span) {
            const tpp = Math.max((w * 4) / vp.width, 0.01);
            vp = { t0: it[0] - w * 1.5, tpp, width: vp.width };
          } else vp = { ...vp, t0: (it[0] + it[1]) / 2 - span / 2 };
        }
      }
    }
    // inertia after a drag
    const now = performance.now();
    const dt = st.last ? Math.min(0.1, (now - st.last) / 1e3) : 0;
    st.last = now;
    if (st.vel !== 0 && !st.drag) {
      vp = { ...vp, t0: vp.t0 + st.vel * dt };
      st.vel *= Math.exp(-dt * 4);
      if (Math.abs(st.vel / vp.tpp) < 5) st.vel = 0;
    }
    if (forced.panel === props.id && forced.vp) vp = { ...forced.vp, width: pw };
    if (forced.panel === props.id && forced.reset) {
      forced.reset = false;
      vp = fit(n, pw);
    }
    vp = clamped(vp, n);
    st.vp = vp;
    if (forced.panel === null) forced.panel = props.id;
    forced.width = pw;

    // the ruler's steps
    const tick = c.tickHz;
    const step = niceStep(((vp.tpp / tick) * 90));
    const s0 = vp.t0 / tick;
    const grid: number[] = [];
    const gridS: number[] = [];
    for (let k = Math.floor(s0 / step); ; k++) {
      const x = plotL + (k * step * tick - vp.t0) / vp.tpp;
      if (x > st.w) break;
      if (x >= plotL) {
        grid.push(x);
        gridS.push(k * step);
      }
    }
    // lanes
    const lanesTop = MINIMAP_H + RULER_H + 2;
    const ls = lanes();
    const total = ls.reduce((a, l) => a + l.h, 0);
    st.scrollY = Math.max(0, Math.min(st.scrollY, total - (st.h - lanesTop) + 20));
    const arr: number[] = [];
    st.laneRects = [];
    let y = lanesTop - st.scrollY;
    ls.forEach((l, i) => {
      st.laneRects.push({ lane: l, y });
      arr.push(l.kind, l.a, l.b, y, l.h, i % 2 === 1 && (l.kind === 0 || l.kind === 1) ? 1 : 0);
      y += l.h;
    });
    const selArr = s ? [s.c, s.l, s.row] : [];
    const vpArr = [vp.t0, vp.tpp, plotL, pw, lanesTop, st.h, plotL, 0, pw, MINIMAP_H, st.charPx, tilesOn() ? 1 : 0];
    if (frameIpc && forced.panel === props.id) lastFrame.args = { vp: vpArr, lanes: arr, sel: selArr, grid };
    const tw = performance.now();
    const ni = sc.frame(vpArr, arr, selArr, grid);
    const wasmMs = performance.now() - tw;
    const inst = new Float32Array(viewMemory!.buffer, sc.inst_ptr(), ni * 8);
    renderer.draw(inst, ni, st.w, st.h, pf);
    overlay(g, vp, ls, gridS, grid, lanesTop);
    const scaleTxt = `${fmtTime(vp.tpp / tick)} / px  ·  幅 ${fmtTime((vp.tpp * vp.width) / tick)}`;
    if (scaleTxt !== st.lastScale) {
      st.lastScale = scaleTxt;
      setScaleText(scaleTxt);
    }
    if (perf.firstPaint === null) {
      const ms = performance.now() - openedAt.t;
      setPerf('firstPaint', ms);
      console.log(`first paint: ${ms.toFixed(0)} ms (${renderer.kind})`);
      report('first-paint', { ms, renderer: renderer.kind + (gpuInfo ? ` (${gpuInfo})` : '') });
    }
    if ((frameNo++ & 15) === 0) setPerf({ shapes: sc.shapes, inst: ni, tiles: Array.from(sc.tiles()) as [number, number, number] });
    if (st.vel !== 0 || lod().ms === null) requestDraw();
    return [performance.now() - t0, wasmMs];
  }
  let frameNo = 0;

  function overlay(g: CanvasRenderingContext2D, vp: Viewport, ls: Lane[], gridS: number[], grid: number[], lanesTop: number) {
    const c = cap()!;
    const sc = scene!;
    const P = pal();
    const plotL = LABEL_W;
    const tick = c.tickHz;
    const a = analysis();
    // the title beside the minimap
    g.font = `12px ${UI_FONT}`;
    g.fillStyle = P.text;
    g.textBaseline = 'middle';
    g.fillText(c.title, 6, MINIMAP_H / 2, LABEL_W - 10);
    // the ruler
    g.font = `10.5px ${LABEL_FONT.slice(5)}`;
    g.fillStyle = P.dim;
    g.strokeStyle = P.dim;
    g.textBaseline = 'top';
    const step = gridS.length > 1 ? gridS[1] - gridS[0] : 1;
    g.beginPath();
    grid.forEach((x, i) => {
      g.moveTo(x + 0.5, MINIMAP_H + RULER_H - 6);
      g.lineTo(x + 0.5, MINIMAP_H + RULER_H);
      g.fillText(fmtRuler(gridS[i], step), x + 3, MINIMAP_H + 2);
    });
    g.stroke();
    // lane names
    g.save();
    g.beginPath();
    g.rect(0, lanesTop, st.w, st.h - lanesTop);
    g.clip();
    g.textBaseline = 'middle';
    for (const { lane: l, y } of st.laneRects) {
      if (y + l.h < lanesTop || y > st.h) continue;
      const cy = y + l.h / 2;
      if (l.kind === 0) {
        g.font = `12.5px ${UI_FONT}`;
        g.fillStyle = P.text;
        g.fillText(c.lines[l.a], 8, cy, LABEL_W - 12);
        const built = sc.lod_built(l.a);
        if (built < c.nTicks) {
          g.font = `9.5px ${UI_FONT}`;
          g.fillStyle = P.dim;
          g.textBaseline = 'bottom';
          g.fillText(`要約 ${((built / Math.max(1, c.nTicks)) * 100).toFixed(0)}%`, 8, y + l.h - 2);
          g.textBaseline = 'middle';
        }
      } else if (l.kind === 1) {
        const an = c.analog[l.a];
        g.font = `12.5px ${UI_FONT}`;
        g.fillStyle = P.text;
        g.fillText(`${an?.name ?? 'A' + l.a} [${an?.unit ?? ''}]`, 8, cy, LABEL_W - 12);
      } else if (l.kind === 2 && a) {
        const cl = a.claims[l.a];
        const ly = cl.layers[l.b];
        g.font = `11px ${UI_FONT}`;
        g.fillStyle = P.dim;
        if (l.b === 0) {
          g.fillText(`${cl.protocol} ${ly.name}`, 14, cy, LABEL_W - 16);
          g.fillStyle = verdictColor(cl.verdict);
          g.beginPath();
          g.arc(7, cy, 3.5, 0, Math.PI * 2);
          g.fill();
        } else g.fillText(`  ${ly.name}`, 14, cy, LABEL_W - 16);
      } else if (l.kind === 3) {
        g.font = `8.5px ${UI_FONT}`;
        g.fillStyle = P.dim;
        g.fillText('読めなかった所', 14, cy);
      }
    }
    g.restore();
    // the annotations' texts (wasm told where and which)
    const nl = sc.n_labels();
    if (nl > 0) {
      const lab = new Float32Array(viewMemory!.buffer, sc.labels_ptr(), nl * 3);
      const texts = sc.label_text().split('\u001f');
      g.save();
      g.beginPath();
      g.rect(plotL, lanesTop, st.w - plotL, st.h - lanesTop);
      g.clip();
      g.font = LABEL_FONT;
      g.textAlign = 'center';
      g.textBaseline = 'middle';
      const colors = PALETTE_KEYS.map((k) => P[k]);
      for (let i = 0; i < nl; i++) {
        g.fillStyle = colors[lab[3 * i + 2]];
        g.fillText(texts[i], lab[3 * i], lab[3 * i + 1]);
      }
      g.restore();
      g.textAlign = 'left';
    }
    g.strokeStyle = P.grid;
    g.beginPath();
    g.moveTo(plotL + 0.5, lanesTop);
    g.lineTo(plotL + 0.5, st.h);
    g.stroke();
    // markers
    const [m1, m2] = markers();
    [m1, m2].forEach((m, k) => {
      if (m === null) return;
      const x = plotL + xOf(vp, m);
      if (x < plotL || x > st.w) return;
      g.strokeStyle = k === 0 ? P.marker1 : P.marker2;
      g.lineWidth = 1.5;
      g.beginPath();
      g.moveTo(x, MINIMAP_H);
      g.lineTo(x, st.h);
      g.stroke();
      g.lineWidth = 1;
      g.fillStyle = g.strokeStyle;
      g.font = `11px ${UI_FONT}`;
      g.textBaseline = 'bottom';
      g.fillText(`M${k + 1}`, x + 3, MINIMAP_H + RULER_H - 1);
    });
    if (m1 !== null && m2 !== null) {
      const xa = plotL + xOf(vp, m1);
      const xb = plotL + xOf(vp, m2);
      const yb = MINIMAP_H + RULER_H / 2;
      g.strokeStyle = P.accent;
      g.beginPath();
      g.moveTo(xa, yb);
      g.lineTo(xb, yb);
      g.stroke();
      const d = Math.abs(m2 - m1) / tick;
      g.fillStyle = P.accent;
      g.textAlign = 'center';
      g.textBaseline = 'bottom';
      g.fillText(`Δ ${fmtTime(d)}  (${fmtFreq(1 / Math.max(d, 1e-15))})`, (xa + xb) / 2, yb);
      g.textAlign = 'left';
    }
    // the range being chosen
    if (st.drag?.kind === 'sel' && st.hover) {
      const xa = Math.min(st.drag.x0, st.hover.x);
      const xb = Math.max(st.drag.x0, st.hover.x);
      g.fillStyle = P.accent;
      g.globalAlpha = 0.15;
      g.fillRect(xa, lanesTop, xb - xa, st.h - lanesTop);
      g.globalAlpha = 1;
    }
    // the cursor: its line, the pulse under it measured, the item under it
    const h = st.hover;
    if (h && h.x >= plotL && !st.drag) {
      const t = tOf(vp, h.x - plotL);
      g.strokeStyle = P.dim;
      g.globalAlpha = 0.6;
      g.beginPath();
      g.moveTo(h.x + 0.5, MINIMAP_H);
      g.lineTo(h.x + 0.5, st.h);
      g.stroke();
      g.globalAlpha = 1;
      g.font = `10.5px ${LABEL_FONT.slice(5)}`;
      g.fillStyle = P.text;
      g.textBaseline = 'top';
      g.fillText(fmtTime(t / tick), h.x + 4, MINIMAP_H);
      const lr = st.laneRects.find((r) => h.y >= r.y && h.y < r.y + r.lane.h && h.y >= lanesTop);
      let tip: string | null = null;
      let tipY = h.y + 14;
      if (lr && lr.lane.kind === 0) {
        const m = sc.measure(lr.lane.a, t);
        if (m.length) {
          const mm = { line: lr.lane.a, t, level: m[0], prev: m[1], next: m[2], next2: m[3] };
          const old = measure();
          if (!old || old.line !== mm.line || old.prev !== mm.prev || old.next !== mm.next || Math.abs(old.t - t) > vp.tpp) setMeasure(mm);
          if (!Number.isNaN(mm.prev) && !Number.isNaN(mm.next)) {
            g.strokeStyle = P.accent;
            g.lineWidth = 2;
            g.beginPath();
            g.moveTo(plotL + xOf(vp, mm.prev), lr.y + 2);
            g.lineTo(plotL + xOf(vp, mm.next), lr.y + 2);
            g.stroke();
            g.lineWidth = 1;
          }
          tip = measureText(mm, tick);
          tipY = lr.y + lr.lane.h + 2;
        }
      } else if (lr && lr.lane.kind === 2 && a) {
        const idx = sc.hit(lr.lane.a, lr.lane.b, t);
        if (idx >= 0) {
          const it = sc.item(lr.lane.a, lr.lane.b, idx);
          tip = `${sc.item_text(lr.lane.a, lr.lane.b, idx)}\n${fmtTime(it[0] / tick)} – ${fmtTime(it[1] / tick)}`;
        }
      }
      if (tip) {
        g.font = `11.5px ${UI_FONT}`;
        const rows = tip.split('\n');
        const tw = Math.max(...rows.map((r) => g.measureText(r).width));
        const th = rows.length * 15;
        const x = Math.min(h.x + 12, st.w - tw - 8);
        g.fillStyle = P.bg;
        g.globalAlpha = 0.94;
        g.fillRect(x - 4, tipY - 3, tw + 8, th + 6);
        g.globalAlpha = 1;
        g.strokeStyle = P.grid;
        g.strokeRect(x - 4.5, tipY - 3.5, tw + 9, th + 7);
        g.fillStyle = P.text;
        g.textBaseline = 'top';
        rows.forEach((r, i) => g.fillText(r, x, tipY + i * 15));
      }
    }
  }

  // ------------------------------------------------------------------------------------------------ input

  const local = (e: MouseEvent) => {
    const r = over.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    const vp = st.vp;
    if (!vp) return;
    const p = local(e);
    const dy = e.deltaMode === 1 ? e.deltaY * 16 : e.deltaY;
    const dx = e.deltaMode === 1 ? e.deltaX * 16 : e.deltaX;
    if (p.x < LABEL_W && p.y > MINIMAP_H + RULER_H) {
      st.scrollY = Math.max(0, st.scrollY + dy);
    } else if (e.shiftKey && !e.ctrlKey) {
      st.vp = { ...vp, t0: vp.t0 + (dy + dx) * vp.tpp };
    } else {
      const f = Math.exp(-dy * 0.003);
      const at = p.y < MINIMAP_H ? vp.width / 2 : p.x - LABEL_W;
      st.vp = zoomed(vp, f, at);
      if (dx) st.vp = { ...st.vp, t0: st.vp.t0 + dx * vp.tpp };
      st.vel = 0;
    }
    requestDraw();
  }

  function minimapJump(x: number) {
    const c = cap();
    const vp = st.vp;
    if (!c || !vp) return;
    const span = vp.tpp * vp.width;
    st.vp = { ...vp, t0: ((x - LABEL_W) / plotW()) * c.nTicks - span / 2 };
  }

  function onDown(e: PointerEvent) {
    over.focus();
    setMenu(null);
    const p = local(e);
    over.setPointerCapture(e.pointerId);
    if (p.y < MINIMAP_H && p.x >= LABEL_W && e.button === 0) {
      st.drag = { kind: 'mm', x0: p.x, lastX: p.x, moved: false, button: 0 };
      minimapJump(p.x);
    } else if (e.button === 2 || (e.button === 0 && e.shiftKey)) {
      st.drag = { kind: 'sel', x0: p.x, lastX: p.x, moved: false, button: e.button };
    } else if (e.button === 0) {
      st.drag = { kind: 'pan', x0: p.x, lastX: p.x, moved: false, button: 0 };
      st.vel = 0;
      st.dragHist = [];
    }
    requestDraw();
  }

  function onMove(e: PointerEvent) {
    const p = local(e);
    st.hover = p;
    const d = st.drag;
    if (d && st.vp) {
      if (Math.abs(p.x - d.x0) > 3) d.moved = true;
      if (d.kind === 'mm') minimapJump(p.x);
      else if (d.kind === 'pan') {
        const dx = p.x - d.lastX;
        st.vp = { ...st.vp, t0: st.vp.t0 - dx * st.vp.tpp };
        st.dragHist.push([performance.now(), dx]);
        if (st.dragHist.length > 6) st.dragHist.shift();
      }
      d.lastX = p.x;
    }
    requestDraw();
  }

  function onUp(e: PointerEvent) {
    const d = st.drag;
    st.drag = null;
    const vp = st.vp;
    const p = local(e);
    if (!d || !vp) return;
    if (d.kind === 'sel') {
      const a = Math.min(d.x0, p.x) - LABEL_W;
      const b = Math.max(d.x0, p.x) - LABEL_W;
      if (b - a > 4) {
        const ta = tOf(vp, a);
        const tb = tOf(vp, b);
        st.vp = { t0: ta, tpp: (tb - ta) / vp.width, width: vp.width };
      } else if (d.button === 2) {
        setMenu({ x: p.x, y: p.y, t: tOf(vp, p.x - LABEL_W) });
      }
    } else if (d.kind === 'pan') {
      if (!d.moved) click(p, false);
      else if (st.dragHist.length >= 2) {
        const t0 = st.dragHist[0][0];
        const t1 = st.dragHist[st.dragHist.length - 1][0];
        const sum = st.dragHist.slice(1).reduce((s, x) => s + x[1], 0);
        if (t1 > t0 && performance.now() - t1 < 80) st.vel = (-sum / ((t1 - t0) / 1e3)) * vp.tpp;
      }
      st.dragHist = [];
    }
    requestDraw();
  }

  /** A click on an annotation chooses it (the lists follow); a double click zooms to it. */
  function click(p: { x: number; y: number }, dbl: boolean) {
    const vp = st.vp;
    const sc = scene;
    if (!vp || !sc || p.x < LABEL_W) return;
    const lr = st.laneRects.find((r) => p.y >= r.y && p.y < r.y + r.lane.h);
    if (!lr || lr.lane.kind !== 2) return;
    const t = tOf(vp, p.x - LABEL_W);
    const idx = sc.hit(lr.lane.a, lr.lane.b, t);
    if (idx < 0) return;
    select({ c: lr.lane.a, l: lr.lane.b, row: idx }, props.id);
    if (dbl) {
      const it = sc.item(lr.lane.a, lr.lane.b, idx);
      const w = it[1] - it[0];
      st.vp = { t0: it[0] - w * 0.25, tpp: (w * 1.5) / vp.width, width: vp.width };
    }
    requestDraw();
  }

  function jumpBad(fwd: boolean) {
    const a = analysis();
    const sc = scene;
    const vp = st.vp;
    if (!a || !sc || !vp) return;
    const center = tOf(vp, vp.width / 2);
    let best: { s: number; c: number; row: number } | null = null;
    a.claims.forEach((cl, ci) => {
      if (!cl.layers.length) return;
      const cs = sel();
      const cur = cs && cs.c === ci && cs.l === 0 ? cs.row : -1;
      const i = sc.next_bad(ci, 0, cur, center, fwd);
      if (i < 0) return;
      const s = sc.item(ci, 0, i)[0];
      if (!best || (fwd && s < best.s) || (!fwd && s > best.s)) best = { s, c: ci, row: i };
    });
    if (best) {
      const b = best as { s: number; c: number; row: number };
      const span = vp.tpp * vp.width;
      st.vp = { ...vp, t0: b.s - span / 2 };
      select({ c: b.c, l: 0, row: b.row }, props.id);
    }
    requestDraw();
  }

  function zoomBy(f: number) {
    if (st.vp) st.vp = zoomed(st.vp, f, st.vp.width / 2);
    requestDraw();
  }

  function onKey(e: KeyboardEvent) {
    if (!st.hover || !st.vp) return;
    const tg = e.target as HTMLElement;
    if (tg && (tg.tagName === 'INPUT' || tg.tagName === 'TEXTAREA' || tg.tagName === 'SELECT')) return;
    const c = cap();
    if (!c) return;
    const vp = st.vp;
    const span = vp.tpp * vp.width;
    const t = tOf(vp, st.hover.x - LABEL_W);
    switch (e.key) {
      case 'f':
      case 'F':
        st.vp = fit(c.nTicks, plotW());
        break;
      case '1':
        setMarkers([t, markers()[1]]);
        break;
      case '2':
        setMarkers([markers()[0], t]);
        break;
      case 'n':
      case 'N':
        jumpBad(!e.shiftKey);
        break;
      case 'ArrowLeft':
        st.vp = { ...vp, t0: vp.t0 - span * 0.2 };
        break;
      case 'ArrowRight':
        st.vp = { ...vp, t0: vp.t0 + span * 0.2 };
        break;
      case '+':
      case '=':
        zoomBy(2);
        break;
      case '-':
        zoomBy(0.5);
        break;
      default:
        return;
    }
    e.preventDefault();
    requestDraw();
  }

  onMount(async () => {
    const ro = new ResizeObserver(resize);
    ro.observe(host);
    over.addEventListener('wheel', onWheel, { passive: false });
    const win = host.ownerDocument.defaultView ?? window;
    win.addEventListener('keydown', onKey);
    try {
      renderer = await makeRenderer(gpuCanvas);
      setPerf('renderer', renderer.kind + (renderer.kind === 'WebGPU' && gpuInfo ? ` (${gpuInfo})` : ''));
    } catch (e) {
      setErr(String(e));
      report('error', { message: `renderer: ${e}` });
    }
    const g = over.getContext('2d')!;
    g.font = LABEL_FONT;
    st.charPx = g.measureText('0000000000').width / 10;
    resize();
    const off = addDrawer(draw);
    // a WebGPU frame read back and laid over the canvas (screenshots of headless Chrome miss WebGPU canvases)
    const offShot = addGpuShot(
      () =>
        new Promise<void>((res) => {
          const r = renderer as unknown as { probe?: ((px: { w: number; h: number; data: Uint8Array }) => void) | null };
          if (!r || !('probe' in r)) return res();
          r.probe = (px) => {
            host.querySelector('canvas.frozen')?.remove();
            const c2 = host.ownerDocument.createElement('canvas');
            c2.width = px.w;
            c2.height = px.h;
            c2.className = 'gpu frozen';
            c2.style.width = gpuCanvas.style.width;
            c2.style.height = gpuCanvas.style.height;
            c2.getContext('2d')!.putImageData(new ImageData(new Uint8ClampedArray(px.data.buffer as ArrayBuffer), px.w, px.h), 0, 0);
            gpuCanvas.after(c2);
            res();
          };
          requestDraw();
        }),
    );
    onCleanup(() => {
      off();
      offShot();
      ro.disconnect();
      win.removeEventListener('keydown', onKey);
      renderer?.dispose();
      if (forced.panel === props.id) forced.panel = null;
    });
  });

  const setF = (v: boolean) => {
    setFollow(v);
    props.onParams({ follow: v });
  };

  return (
    <div class="wave">
      <div class="toolbar">
        <label title="一覧で選んだ項目へこの波形が動く">
          <input type="checkbox" checked={follow()} onChange={(e) => setF(e.currentTarget.checked)} />
          一覧と連動
        </label>
        <button title="F" onClick={() => { st.vp = null; requestDraw(); }}>全体</button>
        <button onClick={() => zoomBy(0.5)}>−</button>
        <button onClick={() => zoomBy(2)}>+</button>
        <button title="Shift+N" onClick={() => jumpBad(false)}>前の誤り</button>
        <button title="N" onClick={() => jumpBad(true)}>次の誤り</button>
        <span class="dim">{scaleText()}</span>
        <Show when={err()}>
          <span class="err">{err()}</span>
        </Show>
      </div>
      <div class="wave-body" ref={host}>
        <canvas ref={gpuCanvas} class="gpu" />
        <canvas
          ref={over}
          class="over"
          tabindex="0"
          onPointerDown={onDown}
          onPointerMove={onMove}
          onPointerUp={onUp}
          onPointerLeave={() => {
            st.hover = null;
            requestDraw();
          }}
          onDblClick={(e) => click(local(e), true)}
          onContextMenu={(e) => e.preventDefault()}
        />
        <Show when={menu()}>
          {(m) => (
            <div class="ctx" style={{ left: `${m().x}px`, top: `${m().y}px` }}>
              <button onClick={() => { setMarkers([m().t, markers()[1]]); setMenu(null); requestDraw(); }}>マーカー 1 をここに（1）</button>
              <button onClick={() => { setMarkers([markers()[0], m().t]); setMenu(null); requestDraw(); }}>マーカー 2 をここに（2）</button>
              <button onClick={() => { setMarkers([null, null]); setMenu(null); requestDraw(); }}>マーカーを消す</button>
              <button onClick={() => { st.vp = null; setMenu(null); requestDraw(); }}>全体を表示（F）</button>
            </div>
          )}
        </Show>
      </div>
    </div>
  );
}

export function measureText(m: { level: number; prev: number; next: number; next2: number }, hz: number): string {
  const lv = m.level === 1 ? 'H' : 'L';
  if (Number.isNaN(m.prev) || Number.isNaN(m.next)) return `水準 ${lv}`;
  const w = (m.next - m.prev) / hz;
  let s = `${lv} 幅 ${fmtTime(w)}`;
  if (!Number.isNaN(m.next2)) {
    const per = (m.next2 - m.prev) / hz;
    const duty = m.level === 1 ? w / per : 1 - w / per;
    s += `\n周期 ${fmtTime(per)} · ${fmtFreq(1 / per)} · デューティ ${(duty * 100).toFixed(1)}%`;
  }
  return s;
}


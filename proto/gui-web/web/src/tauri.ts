// Prototype 2b: the same screens in a Tauri shell (proto/gui-web/tauri, bench/notes/2026-11-25-gui-study.ja.md §11).
// Nothing here runs in a browser (isTauri is false there).
//
// - Files for the wasm workers: `files/...` URLs turned into the shell's `wskfile` scheme.
// - `?engine=native`: NativeWorker stands where a Web Worker stood (postMessage / onmessage / terminate, the same
//   messages); the work runs natively in the shell, the answers come through a Tauri channel, `?ipc=json|raw|chunk|fetch`.
// - `&frameipc=1`: the bench asks the shell's native scene for the same frames (the shapes brought over by IPC).
// - report(): printed by the shell; console.log too, when `report=1`.

import { Channel, convertFileSrc, invoke } from '@tauri-apps/api/core';

export const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
const q = new URLSearchParams(typeof location !== 'undefined' ? location.search : '');
export const nativeEngine = isTauri && q.get('engine') === 'native';
export const ipcMode = q.get('ipc') ?? 'chunk';
export const frameIpc = nativeEngine && q.get('frameipc') === '1';

/** A capture's URL as the workers fetch it: in the shell, through its `wskfile` scheme. */
export function fileHref(url: string): string {
  const abs = new URL(url, location.href);
  if (!isTauri) return abs.href;
  return convertFileSrc(decodeURIComponent(abs.pathname.replace(/^\//, '')), 'wskfile');
}

/** What the IPC cost on the screen's thread (decode: bytes into typed arrays; handle: the message taken in). */
export const ipcStats = { msgs: 0, bytes: 0, decodeMs: 0, maxDecodeMs: 0, handleMs: 0, maxHandleMs: 0, maxHandleType: '' };

const ARR: Record<string, any> = {
  i64: BigInt64Array, f64: Float64Array, f32: Float32Array, u32: Uint32Array, u8: Uint8Array,
};
const dec = new TextDecoder();

function setPath(o: any, p: (string | number)[], v: unknown) {
  for (let i = 0; i < p.length - 1; i++) o = o[p[i]];
  o[p[p.length - 1]] = v;
}

/** A binary message: `[u32 head length][JSON head][arrays]`, the arrays viewed in place (no copy). */
function decodeRaw(buf: ArrayBuffer): any {
  const n = new DataView(buf).getUint32(0, true);
  const head = JSON.parse(dec.decode(new Uint8Array(buf, 4, n)));
  for (const a of head.$a ?? []) setPath(head, a.p, new ARR[a.t](buf, a.o, a.n));
  delete head.$a;
  return head;
}

/** A JSON message: its arrays (numbers) made typed. */
function decodeJson(head: any): any {
  for (const a of head.$j ?? []) {
    const T = ARR[a.t];
    setPath(head, a.p, T === BigInt64Array ? BigInt64Array.from(a.v as number[], (x) => BigInt(x)) : T.from(a.v));
  }
  delete head.$j;
  return head;
}

let nextId = 1;

/** A Web Worker's stand-in: the shell runs the work (a thread with a queue), the answers come in order. */
export class NativeWorker {
  onmessage: ((ev: { data: any }) => void) | null = null;
  private id = nextId++;
  private ch = new Channel<any>();
  private ready: Promise<unknown>;
  private sendQ: Promise<unknown> = Promise.resolve();
  private recvQ: Promise<unknown> = Promise.resolve();
  private piece: Uint8Array | null = null;
  private pieceGot = 0;
  private dead = false;

  constructor(kind: 'load' | 'analysis') {
    this.ch.onmessage = (p) => this.receive(p);
    this.ready = invoke('worker_new', { id: this.id, kind, mode: ipcMode, frameipc: frameIpc, on: this.ch });
  }

  postMessage(m: any) {
    this.sendQ = this.sendQ.then(async () => {
      await this.ready;
      const msg = { ...m };
      if (msg.src instanceof File) {
        const b = new Uint8Array(await msg.src.arrayBuffer());
        msg.src = await invoke<string>('put_bytes', b);
      } else if (typeof msg.src === 'string') {
        msg.src = decodeURIComponent(new URL(msg.src, location.href).pathname);
      }
      await invoke('worker_post', { id: this.id, msg });
    });
  }

  terminate() {
    this.dead = true;
    invoke('worker_end', { id: this.id });
  }

  private deliver(data: any, bytes: number, decodeMs: number) {
    if (this.dead || !data) return;
    ipcStats.msgs++;
    ipcStats.bytes += bytes;
    ipcStats.decodeMs += decodeMs;
    ipcStats.maxDecodeMs = Math.max(ipcStats.maxDecodeMs, decodeMs);
    const t = performance.now();
    this.onmessage?.({ data });
    const h = performance.now() - t;
    ipcStats.handleMs += h;
    if (h > ipcStats.maxHandleMs) {
      ipcStats.maxHandleMs = h;
      ipcStats.maxHandleType = data.type;
    }
  }

  private receive(p: any) {
    if (p instanceof ArrayBuffer || ArrayBuffer.isView(p)) {
      const buf: ArrayBuffer = p instanceof ArrayBuffer ? p : ((p as Uint8Array).slice().buffer as ArrayBuffer);
      if (ipcMode === 'chunk') {
        const dv = new DataView(buf);
        const total = dv.getUint32(0, true);
        const off = dv.getUint32(4, true);
        const t = performance.now();
        if (off === 0) {
          this.piece = new Uint8Array(total);
          this.pieceGot = 0;
        }
        this.piece!.set(new Uint8Array(buf, 8), off);
        this.pieceGot += buf.byteLength - 8;
        if (this.pieceGot >= total) {
          const whole = this.piece!.buffer as ArrayBuffer;
          this.piece = null;
          const m = decodeRaw(whole);
          this.deliver(m, total, performance.now() - t);
        }
        return;
      }
      const t = performance.now();
      const m = decodeRaw(buf);
      this.deliver(m, buf.byteLength, performance.now() - t);
      return;
    }
    // JSON (ipc=json, or the small messages and the notices of ipc=fetch)
    const head = typeof p === 'string' ? JSON.parse(p) : p;
    if (head.$blob !== undefined) {
      // fetched in order (the messages after it wait)
      this.recvQ = this.recvQ.then(async () => {
        const r = await fetch(convertFileSrc(String(head.$blob), 'wskblob'));
        const buf = await r.arrayBuffer();
        const t = performance.now();
        const m = decodeRaw(buf);
        this.deliver(m, buf.byteLength, performance.now() - t);
      });
      return;
    }
    this.recvQ = this.recvQ.then(() => {
      const t = performance.now();
      const m = decodeJson(head);
      this.deliver(m, 0, performance.now() - t);
    });
  }
}

// ------------------------------------------------------------------------------------------------ frames by IPC

/** The arguments of the last frame the first waveform made (the bench asks the native scene for the same). */
export const lastFrame = { args: null as null | { vp: number[]; lanes: number[]; sel: number[]; grid: number[] } };

export const frameProbe = { busy: false, rt: [] as number[], native: [] as number[], bytes: [] as number[], n: [] as number[] };

/** One frame asked of the native scene (not drawn: what it costs to bring the shapes over). */
export function probeFrame() {
  const a = lastFrame.args;
  if (!frameIpc || !a || frameProbe.busy) return;
  frameProbe.busy = true;
  const f = [...a.vp, a.lanes.length, ...a.lanes, a.sel.length, ...a.sel, ...a.grid];
  const t = performance.now();
  const done = (n: number, ms: number, bytes: number) => {
    frameProbe.rt.push(performance.now() - t);
    frameProbe.native.push(ms);
    frameProbe.bytes.push(bytes);
    frameProbe.n.push(n);
    frameProbe.busy = false;
  };
  if (ipcMode === 'json') {
    invoke<any>('frame_json', { f }).then((r) => {
      if (!r) return void (frameProbe.busy = false);
      const inst = new Float32Array(r.inst);
      done(r.n, r.ms, JSON.stringify(r).length + inst.length * 0);
    });
  } else {
    invoke<ArrayBuffer>('frame', new Uint8Array(new Float64Array(f).buffer)).then((buf) => {
      if (!buf || buf.byteLength < 16) return void (frameProbe.busy = false);
      const dv = new DataView(buf);
      const n = dv.getUint32(0, true);
      const inst = new Float32Array(buf, 16, n * 8);
      done(n, dv.getFloat32(8, true), buf.byteLength + inst.length * 0);
    });
  }
}

/** The probe's numbers since the last call, as a table cell. */
export function takeProbe(): string {
  const p = frameProbe;
  if (!p.rt.length) return '';
  const avg = (s: number[]) => s.reduce((a, b) => a + b, 0) / s.length;
  const s = `${avg(p.rt).toFixed(2)} (${avg(p.native).toFixed(2)}) ${(avg(p.bytes) / 1e3).toFixed(0)} kB`;
  p.rt = [];
  p.native = [];
  p.bytes = [];
  p.n = [];
  return s;
}

/** The shell's processes' memory (Linux: the WebKitGTK processes too). */
export async function shellMem(): Promise<string> {
  if (!isTauri) return '';
  const m = await invoke<any>('mem');
  return `process tree ${(m.total / 1e6).toFixed(0)} MB (${m.procs.map((p: [string, number]) => `${p[0]} ${(p[1] / 1e6).toFixed(0)}`).join(', ')})`;
}

export function tauriReport(what: string, data: object) {
  invoke('report', { what, data }).catch(() => {});
}

/** The same frame asked of the native scene 40 times in a row with nothing else running, raw and JSON (the IPC's own
 * round trip, not bound to the animation frames as during the bench). */
export async function idleProbe(): Promise<string> {
  const a = lastFrame.args;
  if (!frameIpc || !a) return '';
  const f = [...a.vp, a.lanes.length, ...a.lanes, a.sel.length, ...a.sel, ...a.grid];
  const body = new Uint8Array(new Float64Array(f).buffer);
  const med = (s: number[]) => [...s].sort((x, y) => x - y)[s.length >> 1];
  const out: string[] = [];
  for (const kind of ['raw', 'json'] as const) {
    const rt: number[] = [];
    const nat: number[] = [];
    let bytes = 0;
    let n = 0;
    for (let k = 0; k < 40; k++) {
      const t = performance.now();
      if (kind === 'raw') {
        const buf = await invoke<ArrayBuffer>('frame', body);
        const dv = new DataView(buf);
        n = dv.getUint32(0, true);
        nat.push(dv.getFloat32(8, true));
        bytes = buf.byteLength;
      } else {
        const r = await invoke<any>('frame_json', { f });
        new Float32Array(r.inst);
        n = r.n;
        nat.push(r.ms);
        bytes = JSON.stringify(r).length;
      }
      rt.push(performance.now() - t);
    }
    out.push(`${kind}: ${n} shapes, ${(bytes / 1e3).toFixed(0)} kB, round trip median ${med(rt).toFixed(2)} ms (native frame ${med(nat).toFixed(2)} ms)`);
  }
  return `native frame by IPC, idle, 40 in a row: ${out.join('; ')}`;
}

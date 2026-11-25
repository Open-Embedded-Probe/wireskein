// The waveform's shapes drawn on the GPU: WebGPU where the browser has it, else WebGL2. Every shape is one instance
// of 8 floats [x0, y0, x1, y1, color, alpha, kind, width] (wasm/view: kind 0 a rectangle, 1 a segment `width` px
// thick), in CSS px; colors are indices into the palette (a uniform), so dark / light is a uniform change.

// the order of wasm/view's color indices
export const PALETTE_KEYS = [
  'bg', 'laneAlt', 'grid', 'text', 'dim', 'line', 'analog', 'accent', 'marker1', 'marker2',
  'ok', 'note', 'warn', 'error', 'unread', 'sel',
] as const;
export type PalKey = (typeof PALETTE_KEYS)[number];
export type Pal = Record<PalKey, string>;

// prototype 1's colors (theme.rs)
export const DARK: Pal = {
  bg: '#16181d', laneAlt: '#1b1e24', grid: '#2a2e36', text: '#dde1e8', dim: '#8a93a3', line: '#4cd19b',
  analog: '#6cb4ff', accent: '#ffd14a', marker1: '#ff6bd6', marker2: '#5fe3ff', ok: '#2f5d8c', note: '#8a6d1f',
  warn: '#a0561a', error: '#b0303a', unread: '#5a5a66', sel: '#ffffff',
};
export const LIGHT: Pal = {
  bg: '#fbfbfc', laneAlt: '#f1f3f6', grid: '#dce0e6', text: '#1d222b', dim: '#6a7280', line: '#0b8a5c',
  analog: '#1d6fd6', accent: '#d98c00', marker1: '#d02fa8', marker2: '#008eb0', ok: '#c9dcf2', note: '#f3e1a6',
  warn: '#f6c699', error: '#f2aeb3', unread: '#c8c8d0', sel: '#101010',
};
export const VERDICT: Record<string, string> = { confirmed: '#3fb950', likely: '#e3b341' };
export const verdictColor = (v: string) => VERDICT[v] ?? '#f07a3a';

function hex(c: string): [number, number, number] {
  const n = parseInt(c.slice(1), 16);
  return [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255];
}

export function paletteFloats(p: Pal): Float32Array {
  const out = new Float32Array(16 * 4);
  PALETTE_KEYS.forEach((k, i) => {
    const [r, g, b] = hex(p[k]);
    out.set([r, g, b, 1], i * 4);
  });
  return out;
}

export interface Renderer {
  readonly kind: 'WebGPU' | 'WebGL2';
  /** `n` instances of `inst` drawn over a canvas `w` x `h` CSS px (the canvas' own size is set by the caller). */
  draw(inst: Float32Array, n: number, w: number, h: number, pal: Float32Array): void;
  dispose(): void;
}

const WGSL = /* wgsl */ `
struct U { size: vec2f, pad: vec2f, pal: array<vec4f, 16> };
@group(0) @binding(0) var<uniform> u: U;
struct O { @builtin(position) p: vec4f, @location(0) c: vec4f };
@vertex fn vs(@builtin(vertex_index) vi: u32, @location(0) a: vec4f, @location(1) b: vec4f) -> O {
  var corners = array<vec2f, 6>(vec2f(0., 0.), vec2f(1., 0.), vec2f(0., 1.), vec2f(0., 1.), vec2f(1., 0.), vec2f(1., 1.));
  let k = corners[vi];
  var pos: vec2f;
  if (b.z < 0.5) {
    pos = mix(a.xy, a.zw, k);
  } else {
    let d = a.zw - a.xy;
    let len = max(length(d), 1e-3);
    let nrm = vec2f(-d.y, d.x) / len * (b.w * 0.5);
    pos = mix(a.xy, a.zw, k.x) + nrm * (k.y * 2. - 1.);
  }
  let ndc = vec2f(pos.x / u.size.x * 2. - 1., 1. - pos.y / u.size.y * 2.);
  let c = u.pal[u32(b.x)];
  let al = c.a * b.y;
  var o: O;
  o.p = vec4f(ndc, 0., 1.);
  o.c = vec4f(c.rgb * al, al);
  return o;
}
@fragment fn fs(o: O) -> @location(0) vec4f { return o.c; }
`;

let devicePromise: Promise<{ device: GPUDevice; info: string } | null> | null = null;

/** One GPU device for every canvas (null: no WebGPU here). */
export function gpuDevice() {
  if (!devicePromise) {
    devicePromise = (async () => {
      if (!('gpu' in navigator)) return null;
      try {
        const adapter = await navigator.gpu.requestAdapter({ powerPreference: 'high-performance' });
        if (!adapter) return null;
        const device = await adapter.requestDevice();
        const i = adapter.info;
        const info = [i?.vendor, i?.architecture, i?.description].filter((x) => x).join(' ') || 'adapter';
        return { device, info };
      } catch (e) {
        console.warn('WebGPU:', e);
        return null;
      }
    })();
  }
  return devicePromise;
}

export let gpuInfo = '';

class GpuRenderer implements Renderer {
  readonly kind = 'WebGPU' as const;
  private ctx: GPUCanvasContext;
  private pipeline: GPURenderPipeline;
  private ubuf: GPUBuffer;
  private bind: GPUBindGroup;
  private ibuf: GPUBuffer | null = null;
  private cap = 0;
  private bg = [0, 0, 0];
  private format: GPUTextureFormat = 'bgra8unorm';
  /** asked to read the next frame back: its pixels as RGBA rows */
  probe: ((px: { w: number; h: number; data: Uint8Array }) => void) | null = null;
  constructor(private device: GPUDevice, canvas: HTMLCanvasElement) {
    this.ctx = canvas.getContext('webgpu') as GPUCanvasContext;
    const format = navigator.gpu.getPreferredCanvasFormat();
    this.format = format;
    // COPY_SRC: `?gpucheck=1` reads a frame back (headless Chrome on SwiftShader does not composite WebGPU canvases
    // into its screenshots, so the drawing is checked this way)
    this.ctx.configure({ device, format, alphaMode: 'opaque', usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC });
    const module = device.createShaderModule({ code: WGSL });
    this.pipeline = device.createRenderPipeline({
      layout: 'auto',
      vertex: {
        module,
        entryPoint: 'vs',
        buffers: [
          {
            arrayStride: 32,
            stepMode: 'instance',
            attributes: [
              { shaderLocation: 0, offset: 0, format: 'float32x4' },
              { shaderLocation: 1, offset: 16, format: 'float32x4' },
            ],
          },
        ],
      },
      fragment: {
        module,
        entryPoint: 'fs',
        targets: [
          {
            format,
            blend: {
              color: { srcFactor: 'one', dstFactor: 'one-minus-src-alpha', operation: 'add' },
              alpha: { srcFactor: 'one', dstFactor: 'one-minus-src-alpha', operation: 'add' },
            },
          },
        ],
      },
      primitive: { topology: 'triangle-list' },
    });
    this.ubuf = device.createBuffer({ size: 16 + 16 * 16, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
    this.bind = device.createBindGroup({
      layout: this.pipeline.getBindGroupLayout(0),
      entries: [{ binding: 0, resource: { buffer: this.ubuf } }],
    });
  }
  draw(inst: Float32Array, n: number, w: number, h: number, pal: Float32Array) {
    const d = this.device;
    const bytes = n * 32;
    if (bytes > this.cap) {
      this.ibuf?.destroy();
      this.cap = Math.max(bytes, this.cap * 2, 1 << 16);
      this.ibuf = d.createBuffer({ size: this.cap, usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST });
    }
    if (n > 0) d.queue.writeBuffer(this.ibuf!, 0, inst.buffer, inst.byteOffset, bytes);
    const u = new Float32Array(4 + 64);
    u.set([w, h, 0, 0]);
    u.set(pal, 4);
    d.queue.writeBuffer(this.ubuf, 0, u);
    this.bg = [pal[0], pal[1], pal[2]];
    const enc = d.createCommandEncoder();
    const pass = enc.beginRenderPass({
      colorAttachments: [
        {
          view: this.ctx.getCurrentTexture().createView(),
          clearValue: { r: this.bg[0], g: this.bg[1], b: this.bg[2], a: 1 },
          loadOp: 'clear',
          storeOp: 'store',
        },
      ],
    });
    if (n > 0) {
      pass.setPipeline(this.pipeline);
      pass.setBindGroup(0, this.bind);
      pass.setVertexBuffer(0, this.ibuf!);
      pass.draw(6, n);
    }
    pass.end();
    let rb: GPUBuffer | null = null;
    const tex = this.ctx.getCurrentTexture();
    const bpr = Math.ceil((tex.width * 4) / 256) * 256;
    if (this.probe) {
      rb = d.createBuffer({ size: bpr * tex.height, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
      enc.copyTextureToBuffer({ texture: tex }, { buffer: rb, bytesPerRow: bpr }, [tex.width, tex.height]);
    }
    d.queue.submit([enc.finish()]);
    if (rb && this.probe) {
      const done = this.probe;
      this.probe = null;
      const [w, h, bgra] = [tex.width, tex.height, this.format.startsWith('bgra')];
      rb.mapAsync(GPUMapMode.READ).then(() => {
        const src = new Uint8Array(rb!.getMappedRange());
        const data = new Uint8Array(w * h * 4);
        for (let y = 0; y < h; y++)
          for (let x = 0; x < w; x++) {
            const i = y * bpr + x * 4;
            const o = (y * w + x) * 4;
            data[o] = src[i + (bgra ? 2 : 0)];
            data[o + 1] = src[i + 1];
            data[o + 2] = src[i + (bgra ? 0 : 2)];
            data[o + 3] = 255;
          }
        rb!.unmap();
        rb!.destroy();
        done({ w, h, data });
      });
    }
  }
  dispose() {
    this.ibuf?.destroy();
    this.ubuf.destroy();
    this.ctx.unconfigure();
  }
}

const VS = `#version 300 es
in vec4 a; in vec4 b;
uniform vec2 size; uniform vec4 pal[16];
out vec4 c;
void main() {
  vec2 corners[6] = vec2[6](vec2(0., 0.), vec2(1., 0.), vec2(0., 1.), vec2(0., 1.), vec2(1., 0.), vec2(1., 1.));
  vec2 k = corners[gl_VertexID];
  vec2 pos;
  if (b.z < 0.5) { pos = mix(a.xy, a.zw, k); }
  else {
    vec2 d = a.zw - a.xy; float len = max(length(d), 1e-3);
    vec2 n = vec2(-d.y, d.x) / len * (b.w * 0.5);
    pos = mix(a.xy, a.zw, k.x) + n * (k.y * 2. - 1.);
  }
  gl_Position = vec4(pos.x / size.x * 2. - 1., 1. - pos.y / size.y * 2., 0., 1.);
  vec4 p = pal[int(b.x)]; float al = p.a * b.y;
  c = vec4(p.rgb * al, al);
}`;
const FS = `#version 300 es
precision mediump float; in vec4 c; out vec4 o; void main() { o = c; }`;

class GlRenderer implements Renderer {
  readonly kind = 'WebGL2' as const;
  private gl: WebGL2RenderingContext;
  private prog: WebGLProgram;
  private vao: WebGLVertexArrayObject;
  private buf: WebGLBuffer;
  private uSize: WebGLUniformLocation;
  private uPal: WebGLUniformLocation;
  constructor(canvas: HTMLCanvasElement) {
    const gl = canvas.getContext('webgl2', { antialias: false, premultipliedAlpha: true, alpha: false });
    if (!gl) throw new Error('WebGL2 is not available');
    this.gl = gl;
    const sh = (t: number, src: string) => {
      const s = gl.createShader(t)!;
      gl.shaderSource(s, src);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s) ?? 'shader');
      return s;
    };
    const p = gl.createProgram()!;
    gl.attachShader(p, sh(gl.VERTEX_SHADER, VS));
    gl.attachShader(p, sh(gl.FRAGMENT_SHADER, FS));
    gl.linkProgram(p);
    if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p) ?? 'link');
    this.prog = p;
    this.vao = gl.createVertexArray()!;
    this.buf = gl.createBuffer()!;
    gl.bindVertexArray(this.vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buf);
    const la = gl.getAttribLocation(p, 'a');
    const lb = gl.getAttribLocation(p, 'b');
    gl.enableVertexAttribArray(la);
    gl.vertexAttribPointer(la, 4, gl.FLOAT, false, 32, 0);
    gl.vertexAttribDivisor(la, 1);
    gl.enableVertexAttribArray(lb);
    gl.vertexAttribPointer(lb, 4, gl.FLOAT, false, 32, 16);
    gl.vertexAttribDivisor(lb, 1);
    gl.bindVertexArray(null);
    this.uSize = gl.getUniformLocation(p, 'size')!;
    this.uPal = gl.getUniformLocation(p, 'pal')!;
  }
  draw(inst: Float32Array, n: number, w: number, h: number, pal: Float32Array) {
    const gl = this.gl;
    gl.viewport(0, 0, gl.drawingBufferWidth, gl.drawingBufferHeight);
    gl.clearColor(pal[0], pal[1], pal[2], 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    if (n === 0) return;
    gl.useProgram(this.prog);
    gl.uniform2f(this.uSize, w, h);
    gl.uniform4fv(this.uPal, pal);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
    gl.bindVertexArray(this.vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buf);
    gl.bufferData(gl.ARRAY_BUFFER, inst.subarray(0, n * 8), gl.STREAM_DRAW);
    gl.drawArraysInstanced(gl.TRIANGLES, 0, 6, n);
    gl.bindVertexArray(null);
  }
  dispose() {
    this.gl.getExtension('WEBGL_lose_context')?.loseContext();
  }
}

/** `?gpu=webgl2` forces WebGL2; else WebGPU when the browser gives a device. */
export async function makeRenderer(canvas: HTMLCanvasElement): Promise<Renderer> {
  const want = new URLSearchParams(location.search).get('gpu');
  if (want !== 'webgl2') {
    const g = await gpuDevice();
    if (g) {
      gpuInfo = g.info;
      return new GpuRenderer(g.device, canvas);
    }
  }
  return new GlRenderer(canvas);
}

const gpuShots = new Set<() => Promise<void>>();
/** A view's way to lay its last WebGPU frame over its canvas (`window.__gpuShot()` runs them all). */
export function addGpuShot(f: () => Promise<void>) {
  gpuShots.add(f);
  return () => gpuShots.delete(f);
}
(window as unknown as { __gpuShot: () => Promise<unknown> }).__gpuShot = () => Promise.all([...gpuShots].map((f) => f()));

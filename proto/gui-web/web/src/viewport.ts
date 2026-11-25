// view-core's Viewport in TS (the screen moves it on every input; the wasm side only reads it in `frame`).

export interface Viewport {
  t0: number;
  tpp: number;
  width: number;
}

export const fit = (n: number, width: number): Viewport => ({ t0: 0, tpp: Math.max(n, 1) / Math.max(width, 1), width });
export const t1 = (v: Viewport) => v.t0 + v.tpp * v.width;
export const xOf = (v: Viewport, t: number) => (t - v.t0) / v.tpp;
export const tOf = (v: Viewport, x: number) => v.t0 + x * v.tpp;

/** Zoomed by `factor` (>1: in) about the pixel x. */
export function zoomed(v: Viewport, factor: number, x: number): Viewport {
  const t = tOf(v, x);
  const tpp = Math.max(v.tpp / factor, 1e-3);
  return { t0: t - x * tpp, tpp, width: v.width };
}

/** Kept inside [0, n) as far as the zoom allows. */
export function clamped(v: Viewport, n: number): Viewport {
  const span = v.tpp * v.width;
  const r = { ...v };
  if (span >= n) {
    r.tpp = Math.min(r.tpp, n / Math.max(v.width, 1));
    const s2 = r.tpp * r.width;
    r.t0 = s2 >= n ? 0 : (n - s2) / 2;
    return r;
  }
  r.t0 = Math.min(Math.max(r.t0, 0), n - span);
  return r;
}

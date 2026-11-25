// Times, frequencies and the list's cells written as prototype 1 wrote them.

export function fmtTime(s: number): string {
  const a = Math.abs(s);
  if (a === 0) return '0 s';
  if (a >= 1) return `${s.toFixed(6)} s`;
  if (a >= 1e-3) return `${(s * 1e3).toFixed(4)} ms`;
  if (a >= 1e-6) return `${(s * 1e6).toFixed(4)} µs`;
  return `${(s * 1e9).toFixed(2)} ns`;
}

export function fmtFreq(f: number): string {
  if (f >= 1e6) return `${(f / 1e6).toFixed(4)} MHz`;
  if (f >= 1e3) return `${(f / 1e3).toFixed(4)} kHz`;
  return `${f.toFixed(3)} Hz`;
}

export function niceStep(minS: number): number {
  const p = Math.pow(10, Math.floor(Math.log10(minS)));
  for (const m of [1, 2, 5, 10]) if (p * m >= minS) return p * m;
  return p * 10;
}

export function fmtRuler(s: number, step: number): string {
  const [div, unit] = step >= 1 ? [1, 's'] : step >= 1e-3 ? [1e-3, 'ms'] : step >= 1e-6 ? [1e-6, 'µs'] : [1e-9, 'ns'];
  const digits = Math.max(0, Math.ceil(Math.log10(div / step)));
  return `${(s / div).toFixed(digits)} ${unit}`;
}

const HEX_COLS = new Set(['addr', 'id', 'reg', 'data', 'value', 'byte']);

/** A number column's cell (addresses and bytes in hex). */
export function numCell(col: string, v: number): string {
  if (Number.isNaN(v)) return '';
  if (HEX_COLS.has(col) && Number.isInteger(v)) return '0x' + v.toString(16).padStart(2, '0');
  return String(v);
}

export function boolCell(v: number): string {
  return v === 1 ? '✓' : v === 0 ? '×' : '';
}

export function mb(bytes: number): string {
  return `${(bytes / 1e6).toFixed(1)} MB`;
}

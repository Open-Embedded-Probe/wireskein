// The main page's side of independent list windows (list.html): a BroadcastChannel. A window says hello and gets the
// analysis' layers (copied: structured clone of the typed arrays), then the choice, the hits and the theme as they
// change; its own choices come back as `select`. Unlike a dockview popout (the same page's objects in another
// window), such a window lives on its own: it outlives a reload of the main page and asks again.

import { createEffect, on } from 'solid-js';
import { analysis, cap, dark, search, sel, select } from './state';

export const CHANNEL = 'wsk-gui-web';
let ch: BroadcastChannel | null = null;

function sendData() {
  const a = analysis();
  const c = cap();
  ch?.postMessage({ type: 'data', title: c?.title ?? '', tickHz: c?.tickHz ?? 1, claims: a?.claims ?? null, dark: dark() });
  sendHits();
  const s = sel();
  if (s) ch?.postMessage({ type: 'sel', sel: s });
}
function sendHits() {
  ch?.postMessage({ type: 'hits', layers: search.layers.map((h) => ({ c: h.c, l: h.l, rows: h.rows })) });
}

export function startChannel() {
  ch = new BroadcastChannel(CHANNEL);
  ch.onmessage = (ev) => {
    const m = ev.data;
    if (m.type === 'hello') sendData();
    else if (m.type === 'select') select(m.sel, m.src);
  };
  createEffect(on(analysis, () => sendData(), { defer: true }));
  createEffect(on(dark, (d) => ch?.postMessage({ type: 'theme', dark: d }), { defer: true }));
  createEffect(on(sel, (s) => s && ch?.postMessage({ type: 'sel', sel: s }), { defer: true }));
  createEffect(on(() => search.layers, () => sendHits(), { defer: true }));
}

let count = 0;
export function openListWindow() {
  count++;
  const w = 620;
  const h = 760;
  const left = window.screenX + window.outerWidth - 60 - 30 * count;
  window.open(`./list.html?n=${count}`, `wsk-list-${count}`, `popup,width=${w},height=${h},left=${left},top=${window.screenY + 40 + 30 * count}`);
}

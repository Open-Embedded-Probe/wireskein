// An independent list window (list.html): the same list component, its data and choices over a BroadcastChannel
// from the main page (channel.ts). Nothing here reads files or runs wasm.

import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import { ListView, type ListSource } from './list';
import type { ClaimData, Sel } from './state';
import { CHANNEL } from './channel';
import './style.css';

const ID = 2_000_000 + Math.floor(Math.random() * 1000);
const ch = new BroadcastChannel(CHANNEL);
const [claims, setClaims] = createSignal<ClaimData[] | null>(null);
const [tickHz, setTickHz] = createSignal(1);
const [title, setTitle] = createSignal('');
const [sel, setSel] = createSignal<(Sel & { src: number; gen: number }) | null>(null);
const [hits, setHits] = createSignal<{ c: number; l: number; rows: Uint32Array }[]>([]);
const [connected, setConnected] = createSignal(false);
let gen = 1e9;

ch.onmessage = (ev) => {
  const m = ev.data;
  if (m.type === 'data') {
    setConnected(true);
    setTickHz(m.tickHz);
    setTitle(m.title);
    setClaims(m.claims);
    document.documentElement.dataset.theme = m.dark ? 'dark' : 'light';
    document.title = `結果の一覧 · ${m.title}（独立した窓）`;
  } else if (m.type === 'sel') setSel(m.sel);
  else if (m.type === 'hits') setHits(m.layers);
  else if (m.type === 'theme') document.documentElement.dataset.theme = m.dark ? 'dark' : 'light';
};
ch.postMessage({ type: 'hello' });

const src: ListSource = {
  claims,
  pending: () => (connected() ? '解析の結果はまだありません' : 'メインの窓につないでいます…（BroadcastChannel）'),
  tickHz,
  sel,
  select: (s, from) => {
    setSel({ ...s, src: from, gen: ++gen });
    ch.postMessage({ type: 'select', sel: s, src: from });
  },
  hits: (c, l) => hits().find((h) => h.c === c && h.l === l)?.rows ?? null,
  hitsGen: hits,
  indexAt: (c, l, t) => {
    const ly = claims()?.[c]?.layers[l];
    if (!ly) return -1;
    let a = 0;
    let b = ly.n;
    while (a < b) {
      const m = (a + b) >> 1;
      if (ly.s[m] <= t) a = m + 1;
      else b = m;
    }
    return a - 1;
  },
};

render(
  () => (
    <div class="listwin">
      <ListView
        id={ID}
        src={src}
        params={{ claim: 0, layer: 0, hidden: [], follow: true }}
        onParams={() => {}}
        note={title() ? `· ${title()}（独立した窓）` : undefined}
      />
    </div>
  ),
  document.getElementById('app')!,
);

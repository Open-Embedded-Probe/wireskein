// The results list apart from the waveform (prototype 1's list.rs): one claim's one layer as rows, only the rows in
// view made (a pool of row elements filled on scroll; past ~8 million px the scroll is scaled), columns chosen and
// resized, the claim and the layer switched, following the waveform's choice or not. The same component runs in a
// dock panel, in a popped-out window (dockview moves the element, the state is the same objects) and in an
// independent window (list.html: its data and choices come over a BroadcastChannel).

import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount, on } from 'solid-js';
import { type ClaimData, type LayerData, type Sel, itemText } from './state';
import { boolCell, fmtTime, numCell } from './fmt';

export interface ListSource {
  claims: () => ClaimData[] | null;
  pending: () => string | null;
  tickHz: () => number;
  sel: () => (Sel & { src: number; gen: number }) | null;
  select: (s: Sel, src: number) => void;
  hits: (c: number, l: number) => Uint32Array | null;
  /** a version that changes when the hits do */
  hitsGen: () => unknown;
  /** the row of layer (c, l) at tick t */
  indexAt: (c: number, l: number, t: number) => number;
}

export interface ListParams {
  claim: number;
  layer: number;
  hidden: string[];
  follow: boolean;
}

const ROW_H = 20;
const MAX_PX = 8_000_000;

export function ListView(props: {
  id: number;
  src: ListSource;
  params: ListParams;
  onParams: (p: ListParams) => void;
  onPopout?: () => void;
  onWindow?: () => void;
  note?: string;
}) {
  const [claim, setClaim] = createSignal(props.params.claim);
  const [layerI, setLayerI] = createSignal(props.params.layer);
  const [hidden, setHidden] = createSignal<string[]>(props.params.hidden);
  const [follow, setFollow] = createSignal(props.params.follow);
  const [widths, setWidths] = createSignal<Record<string, number>>({});
  const save = () => props.onParams({ claim: claim(), layer: layerI(), hidden: hidden(), follow: follow() });

  const claims = () => props.src.claims();
  const cl = createMemo(() => {
    const cs = claims();
    if (!cs || !cs.length) return null;
    return cs[Math.min(claim(), cs.length - 1)];
  });
  const ci = () => Math.min(claim(), (claims()?.length ?? 1) - 1);
  const layer = createMemo<LayerData | null>(() => {
    const c = cl();
    if (!c || !c.layers.length) return null;
    return c.layers[Math.min(layerI(), c.layers.length - 1)] ?? null;
  });
  const li = () => Math.min(layerI(), (cl()?.layers.length ?? 1) - 1);
  const cols = createMemo(() => (layer()?.columns ?? []).filter((c) => !hidden().includes(c.name)));

  let body!: HTMLDivElement;
  let head!: HTMLDivElement;
  let spacer!: HTMLDivElement;
  let rowsEl!: HTMLDivElement;
  let pool: HTMLDivElement[] = [];
  let scrollToRow: number | null = null;
  let lastGen = 0;

  const colW = (name: string, def: number) => widths()[name] ?? def;
  const template = createMemo(() =>
    [colW('#', 64), colW('時刻', 118), colW('長さ', 92), ...cols().map((c) => colW(c.name, c.kind === 2 ? 150 : 70))]
      .map((w) => `${w}px`)
      .join(' ') + ' 1fr',
  );

  const geometry = () => {
    const n = layer()?.n ?? 0;
    const ch = body?.clientHeight ?? 400;
    const vis = Math.ceil(ch / ROW_H) + 1;
    const v = n * ROW_H;
    const h = Math.min(v, MAX_PX);
    return { n, ch, vis, v, h, scaled: v > MAX_PX };
  };

  function render() {
    const l = layer();
    if (!body || !l) {
      if (rowsEl) rowsEl.replaceChildren();
      pool = [];
      return;
    }
    const g = geometry();
    spacer.style.height = `${g.h}px`;
    if (scrollToRow !== null) {
      const r = scrollToRow;
      scrollToRow = null;
      if (!g.scaled) body.scrollTop = Math.max(0, r * ROW_H - g.ch / 2 + ROW_H / 2);
      else body.scrollTop = (Math.max(0, r - g.vis / 2) / Math.max(1, g.n - g.vis)) * (g.h - g.ch);
    }
    const st = body.scrollTop;
    let first: number;
    let offset: number;
    if (!g.scaled) {
      first = Math.floor(st / ROW_H);
      offset = -(st % ROW_H);
    } else {
      first = Math.floor((st / Math.max(1, g.h - g.ch)) * Math.max(0, g.n - g.vis + 1));
      offset = 0;
    }
    rowsEl.style.top = `${st + offset}px`;
    const cs = cols();
    const nCells = 3 + cs.length;
    while (pool.length < g.vis + 1) {
      const d = body.ownerDocument.createElement('div');
      d.className = 'lrow';
      rowsEl.appendChild(d);
      pool.push(d);
    }
    const hz = props.src.tickHz();
    const s = props.src.sel();
    const selRow = s && s.c === ci() && s.l === li() ? s.row : -1;
    const hits = props.src.hits(ci(), li());
    const isHit = (i: number) => {
      if (!hits) return false;
      let a = 0;
      let b = hits.length;
      while (a < b) {
        const m = (a + b) >> 1;
        if (hits[m] < i) a = m + 1;
        else b = m;
      }
      return a < hits.length && hits[a] === i;
    };
    pool.forEach((d, k) => {
      const i = first + k;
      if (i >= l.n) {
        d.style.display = 'none';
        return;
      }
      d.style.display = '';
      if (d.childElementCount !== nCells + 1) {
        d.replaceChildren(...Array.from({ length: nCells + 1 }, () => body.ownerDocument.createElement('span')));
      }
      const c = d.children;
      d.dataset.row = String(i);
      const st = l.status[i];
      d.className = `lrow${i % 2 ? ' odd' : ''}${i === selRow ? ' sel' : ''}${isHit(i) ? ' hit' : ''}${st >= 1 && st <= 3 ? ` st${st}` : ''}`;
      c[0].textContent = String(i);
      c[1].textContent = fmtTime(l.s[i] / hz);
      c[2].textContent = fmtTime((l.e[i] - l.s[i]) / hz);
      cs.forEach((col, k2) => {
        let t: string;
        if (col.kind === 0) t = numCell(col.name, col.data[i]);
        else if (col.kind === 1) t = boolCell(col.data[i]);
        else t = col.data[i] ?? '';
        c[3 + k2].textContent = t;
      });
    });
  }

  // the choice followed (from the waveform, another list, the search)
  createEffect(
    on([() => props.src.sel(), layer], ([s]) => {
      if (!s || s.gen === lastGen) return render();
      lastGen = s.gen;
      if (follow() && s.src !== props.id) {
        if (s.c === ci() && s.l === li()) scrollToRow = s.row;
        else {
          const other = claims()?.[s.c]?.layers[s.l];
          if (other) {
            const r = props.src.indexAt(ci(), li(), other.s[s.row]);
            if (r >= 0) scrollToRow = r;
          }
        }
      }
      render();
    }),
  );
  createEffect(on([cols, () => props.src.hitsGen(), template], () => render()));

  function onClick(e: MouseEvent) {
    const row = (e.target as HTMLElement).closest('.lrow') as HTMLElement | null;
    if (!row?.dataset.row) return;
    props.src.select({ c: ci(), l: li(), row: Number(row.dataset.row) }, props.id);
    body.focus();
  }
  function onKey(e: KeyboardEvent) {
    const l = layer();
    const s = props.src.sel();
    if (!l || (e.key !== 'ArrowDown' && e.key !== 'ArrowUp' && e.key !== 'PageDown' && e.key !== 'PageUp')) return;
    e.preventDefault();
    const cur = s && s.c === ci() && s.l === li() ? s.row : -1;
    const step = e.key.startsWith('Page') ? geometry().vis - 1 : 1;
    const next = Math.max(0, Math.min(l.n - 1, cur + (e.key.endsWith('Down') ? step : -step)));
    props.src.select({ c: ci(), l: li(), row: next }, props.id);
    // keep it in view
    const g = geometry();
    const top = g.scaled ? -1 : body.scrollTop / ROW_H;
    if (top >= 0 && (next < top || next > top + g.vis - 2)) scrollToRow = next;
    else if (top < 0) scrollToRow = next;
    render();
  }

  // a column's width dragged on its header's right edge
  function startResize(name: string, def: number, e: PointerEvent) {
    e.preventDefault();
    e.stopPropagation();
    const x0 = e.clientX;
    const w0 = colW(name, def);
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => setWidths({ ...widths(), [name]: Math.max(32, w0 + ev.clientX - x0) });
    const up = () => {
      el.removeEventListener('pointermove', move);
      el.removeEventListener('pointerup', up);
    };
    el.addEventListener('pointermove', move);
    el.addEventListener('pointerup', up);
  }

  onMount(() => {
    const onScroll = () => {
      head.scrollLeft = body.scrollLeft;
      render();
    };
    body.addEventListener('scroll', onScroll, { passive: true });
    body.addEventListener('click', onClick);
    body.addEventListener('keydown', onKey);
    // the size from the element's own window (it may be moved to another one)
    let ro: ResizeObserver | null = null;
    let watched: Document | null = null;
    const watch = () => {
      ro?.disconnect();
      watched = body.ownerDocument;
      const W = (watched.defaultView ?? window) as unknown as { ResizeObserver: typeof ResizeObserver };
      ro = new W.ResizeObserver(() => render());
      ro.observe(body);
    };
    watch();
    const mo = setInterval(() => {
      // moved to another document (popped out or back): watched from there, its rows made there
      if (watched !== body.ownerDocument) {
        watch();
        pool = [];
        rowsEl.replaceChildren();
        render();
      }
    }, 300);
    onCleanup(() => {
      clearInterval(mo);
      ro?.disconnect();
    });
    render();
  });

  const headCells = () => [
    { name: '#', def: 64 },
    { name: '時刻', def: 118 },
    { name: '長さ', def: 92 },
    ...cols().map((c) => ({ name: c.name, def: c.kind === 2 ? 150 : 70 })),
  ];

  return (
    <div class="list" style={{ '--cols': template() }}>
      <div class="toolbar">
        <Show when={claims()?.length} fallback={<span class="dim">{props.src.pending() ?? '解析の結果はまだありません'}</span>}>
          <select
            value={ci()}
            onChange={(e) => {
              setClaim(Number(e.currentTarget.value));
              setLayerI(0);
              save();
            }}
          >
            <For each={claims()!}>
              {(c, i) => <option value={i()}>{`${c.protocol} (${c.verdict}) ${c.roles.map(([r, n]) => `${r}=${n}`).join(' ')}`}</option>}
            </For>
          </select>
          <select
            value={li()}
            onChange={(e) => {
              setLayerI(Number(e.currentTarget.value));
              save();
            }}
          >
            <For each={cl()?.layers ?? []}>{(l, i) => <option value={i()}>{`${l.name} (${l.n.toLocaleString()})`}</option>}</For>
          </select>
          <details class="menu">
            <summary>列</summary>
            <div class="menu-body">
              <For each={layer()?.columns ?? []}>
                {(c) => (
                  <label>
                    <input
                      type="checkbox"
                      checked={!hidden().includes(c.name)}
                      onChange={(e) => {
                        setHidden(e.currentTarget.checked ? hidden().filter((x) => x !== c.name) : [...hidden(), c.name]);
                        save();
                      }}
                    />
                    {c.name}
                  </label>
                )}
              </For>
            </div>
          </details>
          <label title="波形で選んだ項目へこの一覧が動く">
            <input
              type="checkbox"
              checked={follow()}
              onChange={(e) => {
                setFollow(e.currentTarget.checked);
                save();
              }}
            />
            波形と連動
          </label>
          <Show when={props.onPopout}>
            <button title="このパネルをブラウザの別の窓に出す（同じページの中の状態をそのまま使う）" onClick={() => props.onPopout!()}>
              別の窓へ
            </button>
          </Show>
          <Show when={props.onWindow}>
            <button title="独立した窓（list.html）を開く。窓どうしは BroadcastChannel で受け渡す" onClick={() => props.onWindow!()}>
              独立した窓
            </button>
          </Show>
          <span class="dim">{`${(layer()?.n ?? 0).toLocaleString()} 行`}</span>
          <Show when={props.note}>
            <span class="dim">{props.note}</span>
          </Show>
        </Show>
      </div>
      <div class="list-head" ref={head}>
        <div class="lrow head">
          <For each={headCells()}>
            {(h) => (
              <span>
                {h.name}
                <i class="grip" onPointerDown={(e) => startResize(h.name, h.def, e)} />
              </span>
            )}
          </For>
          <span />
        </div>
      </div>
      <div class="list-body" ref={body} tabindex="0">
        <div class="spacer" ref={spacer}>
          <div class="rows" ref={rowsEl} />
        </div>
      </div>
    </div>
  );
}

/** A layer's item text (the search panel's rows). */
export { itemText };

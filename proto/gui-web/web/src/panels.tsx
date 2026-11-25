// The small panels (prototype 1's panels.rs): measure (the cursor's pulse, the two markers), search (run in the
// analysis worker; the hits marked on the waveform and in the lists), claims, speed.

import { For, Show, createMemo, createSignal } from 'solid-js';
import {
  analysis, analyzing, cap, hitAt, itemText, lod, markers, measure, pal, perf, scene, search, select, setSearch,
  tilesOn, setTilesOn, continuous, setContinuous, requestDraw, viewMemory,
} from './state';
import { fmtFreq, fmtTime, mb } from './fmt';
import { measureText } from './wave';
import { runSearch, timings } from './engine';
import { verdictColor } from './gpu';
import { benchText } from './bench';

export const SEARCH_ID = 1_000_000;

export function MeasurePanel() {
  const counts = createMemo(() => {
    const [a, b] = markers();
    const c = cap();
    if (a === null || b === null || !c || !scene) return null;
    const lo = Math.min(a, b);
    const hi = Math.max(a, b);
    return c.lines.map((name, i) => ({ name, n: scene!.count_edges(i, lo, hi, 1_000_000) }));
  });
  return (
    <div class="pad">
      <h3>カーソル</h3>
      <Show when={measure() && cap()} fallback={<p class="dim">デジタルの線にマウスを載せると、幅・周期・周波数・デューティを測る</p>}>
        <p>{`線 ${cap()!.lines[measure()!.line]}  ·  ${fmtTime(measure()!.t / cap()!.tickHz)}`}</p>
        <p class="pre">{measureText(measure()!, cap()!.tickHz)}</p>
      </Show>
      <hr />
      <h3>マーカー</h3>
      <p class="dim">波形の上で 1 / 2 キー、または右クリック</p>
      <table class="grid">
        <tbody>
          <For each={[0, 1]}>
            {(k) => (
              <tr>
                <td style={{ color: k === 0 ? pal().marker1 : pal().marker2 }}>{`M${k + 1}`}</td>
                <td>{markers()[k] !== null && cap() ? fmtTime(markers()[k]! / cap()!.tickHz) : '–'}</td>
              </tr>
            )}
          </For>
          <Show when={markers()[0] !== null && markers()[1] !== null && cap()}>
            {(_) => {
              const d = () => Math.abs(markers()[1]! - markers()[0]!) / cap()!.tickHz;
              return (
                <tr>
                  <td>Δ</td>
                  <td>{`${fmtTime(d())}  ·  ${fmtFreq(1 / Math.max(d(), 1e-15))}`}</td>
                </tr>
              );
            }}
          </Show>
        </tbody>
      </table>
      <Show when={counts()}>
        <hr />
        <p>マーカーの間の変化の数</p>
        <table class="grid striped">
          <tbody>
            <For each={counts()!}>
              {(r) => (
                <tr>
                  <td>{r.name}</td>
                  <td>{r.n < 0 ? '> 1,000,000' : r.n.toLocaleString()}</td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </Show>
    </div>
  );
}

function step(fwd: boolean) {
  const n = search.total;
  if (!n) return;
  const cur = fwd ? (search.cur + 1) % n : (search.cur + n - 1) % n;
  setSearch('cur', cur);
  const s = hitAt(cur);
  if (s) select(s, SEARCH_ID);
}

export function SearchPanel() {
  let timer = 0;
  let composing = false;
  const [text, setText] = createSignal(search.query);
  const go = (q: string) => {
    clearTimeout(timer);
    timer = setTimeout(() => runSearch(q), 120) as unknown as number;
  };
  const shown = createMemo(() => {
    const a = analysis();
    if (!a) return [];
    const out: { k: number; c: number; l: number; row: number }[] = [];
    let k = 0;
    for (const h of search.layers) {
      for (let i = 0; i < h.rows.length && out.length < 500; i++) out.push({ k: k + i, c: h.c, l: h.l, row: h.rows[i] });
      k += h.rows.length;
    }
    return out;
  });
  return (
    <div class="pad search">
      <div class="row">
        <span>検索</span>
        <input
          type="search"
          value={text()}
          placeholder="例: 0x48 書き込み / addr=0x50 nack / エラー"
          onCompositionStart={() => (composing = true)}
          onCompositionEnd={(e) => {
            composing = false;
            setText(e.currentTarget.value);
            go(e.currentTarget.value);
          }}
          onInput={(e) => {
            setText(e.currentTarget.value);
            if (!composing) go(e.currentTarget.value);
          }}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.isComposing) step(!e.shiftKey);
          }}
        />
      </div>
      <div class="row">
        <span class="dim">{`${search.total.toLocaleString()} 件（${search.ms.toFixed(1)} ms、解析の Worker で）`}</span>
        <button onClick={() => step(false)}>前へ</button>
        <button onClick={() => step(true)}>次へ（Enter）</button>
      </div>
      <p class="dim small">言葉はすべて合うもの: key=値（0x.. は数で比べる）、nack、エラー / 誤り、疑わしい、書き込み、読み出し、ほかは項目の文字の中から</p>
      <hr />
      <div class="hits">
        <For each={shown()}>
          {(h) => {
            const a = analysis()!;
            const ly = a.claims[h.c].layers[h.l];
            return (
              <div
                class={`hit-row${h.k === search.cur ? ' sel' : ''}`}
                onClick={() => {
                  setSearch('cur', h.k);
                  select({ c: h.c, l: h.l, row: h.row }, SEARCH_ID);
                }}
              >
                <span class="small dim">{`${a.claims[h.c].protocol}.${ly.name}`}</span>
                <span class="mono">{fmtTime(ly.s[h.row] / (cap()?.tickHz ?? 1))}</span>
                <span class="mono">{itemText(ly, h.row)}</span>
              </div>
            );
          }}
        </For>
        <Show when={search.total > 500}>
          <p class="dim small">{`（先頭の 500 件。ほかは「次へ」で）`}</p>
        </Show>
      </div>
    </div>
  );
}

export function InfoPanel() {
  return (
    <div class="pad">
      <Show when={cap()}>
        {(c) => (
          <>
            <h3>{c().title}</h3>
            <p>
              {`${c().lines.length} 本の線 · ${c().total.toLocaleString()} の変化 · ${fmtTime(c().nTicks / c().tickHz)} · tick ${c().tickHz} Hz · 区画 ${c().part}/${c().nParts}${c().complete ? '' : ' · 不完全（データ落ち）'}`}
            </p>
            <Show when={c().lost}>
              <p class="err">{`データ落ち: ${c().lost}`}</p>
            </Show>
          </>
        )}
      </Show>
      <hr />
      <Show when={analysis()} fallback={<Show when={analyzing()}><p><span class="spin" /> 解析中…（Web Worker の中。画面は止まらない）</p></Show>}>
        {(a) => (
          <>
            <p>{`解析 ${a().seconds.toFixed(2)} s${a().fake ? '（作り物の項目）' : a().split ? '（wireskein-analysis、分割読み）' : '（wireskein-core、丸ごと）'}`}</p>
            <Show when={a().error}>
              <p class="err">{a().error}</p>
            </Show>
            <For each={a().claims}>
              {(c) => (
                <div class="claim">
                  <div>
                    <span style={{ color: verdictColor(c.verdict) }}>●</span> <b>{c.protocol}</b>{' '}
                    <span style={{ color: verdictColor(c.verdict) }}>{c.verdict}</span> <span>{`score ${c.score.toFixed(3)}`}</span>
                    <Show when={c.bus}>
                      <span>{` bus ${c.bus}`}</span>
                    </Show>
                  </div>
                  <div class="dim">{'   ' + c.roles.map(([r, n]) => `${r}=${n}`).join('  ')}</div>
                  <div class="dim">{'   ' + c.layers.map((l) => `${l.name} ${l.n.toLocaleString()}`).join(' · ')}</div>
                </div>
              )}
            </For>
          </>
        )}
      </Show>
    </div>
  );
}

export function PerfPanel() {
  const heap = () => (performance as unknown as { memory?: { usedJSHeapSize: number } }).memory?.usedJSHeapSize;
  const [tick, setTick] = createSignal(0);
  setInterval(() => setTick((x) => x + 1), 1000);
  return (
    <div class="pad">
      <table class="grid striped">
        <tbody>
          <tr><td>fps（直近 1 秒）</td><td>{`${perf.fps}  （平均 ${perf.frameMs.toFixed(1)} ms、最悪 ${perf.worstMs.toFixed(1)} ms）`}</td></tr>
          <tr><td>波形を作る時間（CPU）</td><td>{`${perf.waveMs.toFixed(2)} ms（うち wasm ${perf.wasmMs.toFixed(2)} ms） · 図形 ${perf.shapes.toLocaleString()} · GPU へ ${perf.inst.toLocaleString()} 個`}</td></tr>
          <tr><td>タイル</td><td>{`${perf.tiles[0]} 個 · 作った ${perf.tiles[1]} · 使い回し ${perf.tiles[2]}`}</td></tr>
          <tr><td>描画</td><td>{perf.renderer || '–'}</td></tr>
          <tr><td>最初の絵まで</td><td>{perf.firstPaint !== null ? `${perf.firstPaint.toFixed(0)} ms` : '–'}</td></tr>
          <tr><td>ファイルを読む（Worker）</td><td>{timings.readMs !== null ? `取る ${timings.readMs.toFixed(0)} ms + 読む ${timings.parseMs?.toFixed(0)} ms（${mb(timings.bytes)}）` : perf.readMs === 0 ? '（合成）' : '–'}</td></tr>
          <tr><td>要約（全部の線、Worker）</td><td>{lod().ms !== null ? `${(lod().ms! / 1e3).toFixed(2)} s` : `作成中 ${lod().done}/${lod().total}`}</td></tr>
          <tr><td>解析（Worker）</td><td>{analysis() ? `${analysis()!.seconds.toFixed(2)} s` : analyzing() ? '解析中…' : '–'}</td></tr>
          <tr><td>記録と要約のメモリ（view の wasm）</td><td>{(tick(), scene ? `${mb(scene.mem_bytes())} · wasm のメモリ ${mb(viewMemory?.buffer.byteLength ?? 0)}` : '–')}</td></tr>
          <tr><td>Worker の wasm のメモリ</td><td>{Object.entries(perf.workerMem).map(([k, v]) => `${k} ${mb(v)}`).join(' · ') || '–'}</td></tr>
          <tr><td>JS のヒープ</td><td>{(tick(), heap() ? mb(heap()!) : '–')}</td></tr>
        </tbody>
      </table>
      <label>
        <input type="checkbox" checked={tilesOn()} onChange={(e) => { setTilesOn(e.currentTarget.checked); requestDraw(); }} />
        タイルを使う（256 px、2 の冪の倍率ごと）
      </label>
      <br />
      <label>
        <input type="checkbox" checked={continuous()} onChange={(e) => { setContinuous(e.currentTarget.checked); requestDraw(); }} />
        毎フレーム描く（fps を測る）
      </label>
      <Show when={benchText()}>
        <hr />
        <pre class="small">{benchText()}</pre>
      </Show>
    </div>
  );
}

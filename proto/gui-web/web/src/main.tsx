// Prototype 2 of WireSkein's screens: the web (TypeScript + Solid + dockview + WebGPU / WebGL2; view-core in wasm
// on the screen's thread, wireskein-io / -core / -analysis in wasm in Web Workers). A throwaway
// (bench/notes/2026-11-25-gui-study.ja.md §10).
//
// URL: ?file=URL [&part=K] | ?synth=1e8 [&items=5e5]; &light=1, &layout=NAME, &zoom=Z&at=F, &search=Q, &select=ROW,
// &gpu=webgl2, &notiles=1, &bench=1 (the speed script; the result in the speed panel and window.__bench),
// &popout=1 (the list in a browser window of its own at the start), &listwin=1 (an independent list window).

import { render } from 'solid-js/web';
import { For, Show, createEffect, createSignal, onMount } from 'solid-js';
import './style.css';
import {
  analysis, analyzing, cap, dark, lod, pal, perf, setDark, requestDraw, search, report,
} from './state';
import { open, reopenPart, runSearch, selectWhenReady } from './engine';
import { PRESETS, addPanel, deleteLayout, loadLayout, mountDock, popout, presetLayout, saveLayout, savedLayouts, dockApi } from './dock';
import { openListWindow, startChannel } from './channel';
import { startBench } from './bench';
import { mb } from './fmt';

const q = new URLSearchParams(location.search);

const EXAMPLES: [string, string][] = [
  ['mixed-1（例）', 'files/examples/mixed-1.wireskein'],
  ['I2C 400k NACK（VCD）', 'files/p4x-i1/i2c-controller-400k-nack.vcd'],
  ['RVSWD 4k（.sr）', 'files/rvswd_long_truth/linke_v203_read_flash_4k.sr'],
  ['h417_slow stream（3 億 tick）', 'files/i2c_bench/h417_slow/stream/impl-100000000-g0.wireskein'],
];

function Menu(props: { label: string; children: any }) {
  let d!: HTMLDetailsElement;
  onMount(() => {
    d.addEventListener('click', (e) => {
      if ((e.target as HTMLElement).tagName === 'BUTTON') d.open = false;
    });
    document.addEventListener('pointerdown', (e) => {
      if (!d.contains(e.target as Node)) d.open = false;
    });
  });
  return (
    <details class="menu" ref={d}>
      <summary>{props.label}</summary>
      <div class="menu-body">{props.children}</div>
    </details>
  );
}

function App() {
  let dockEl!: HTMLDivElement;
  let fileIn!: HTMLInputElement;
  const [layoutName, setLayoutName] = createSignal(q.get('layout') ?? '標準');
  const [saved, setSaved] = createSignal(Object.keys(savedLayouts()));
  const [dragging, setDragging] = createSignal(false);

  createEffect(() => {
    document.documentElement.dataset.theme = dark() ? 'dark' : 'light';
    requestDraw();
  });
  // for tools/bench.mjs: the first picture, the summaries and the analysis done
  let reported = false;
  createEffect(() => {
    const ready = !!cap() && perf.firstPaint !== null && lod().ms !== null && !analyzing();
    (window as unknown as { __ready: boolean }).__ready = ready;
    if (ready && !reported) {
      reported = true;
      report('ready', {
        renderer: perf.renderer, firstPaintMs: perf.firstPaint, lodMs: lod().ms, analysisS: analysis()?.seconds,
        sinceLoadMs: performance.now(),
      });
    }
  });

  onMount(() => {
    mountDock(dockEl, layoutName());
    startChannel();
    window.addEventListener('dragover', (e) => {
      e.preventDefault();
      setDragging(true);
    });
    window.addEventListener('dragleave', (e) => {
      if (!e.relatedTarget) setDragging(false);
    });
    window.addEventListener('drop', (e) => {
      e.preventDefault();
      setDragging(false);
      const f = e.dataTransfer?.files[0];
      if (f) open({ kind: 'file', file: f });
    });
    if (q.get('search')) runSearch(q.get('search')!);
    if (q.get('select')) selectWhenReady(Number(q.get('select')));
    if (q.get('synth')) open({ kind: 'synth', total: Number(q.get('synth')), items: Number(q.get('items') ?? 5e5) });
    else if (q.get('file')) open({ kind: 'url', url: q.get('file')! }, Number(q.get('part') ?? 0));
    if (q.has('bench')) startBench();
    if (q.get('popout') === '1') {
      setTimeout(() => {
        const l = dockApi?.panels.find((p) => p.id.startsWith('list-'));
        if (l) popout(l.id);
      }, 1500);
    }
    if (q.get('listwin') === '1') setTimeout(openListWindow, 1500);
  });

  return (
    <div class="app">
      <div class="bar">
        <b>WireSkein 試作 2（web）</b>
        <span class="sep" />
        <button onClick={() => fileIn.click()} title="ファイルを選ぶ（ここへドラッグでも開く）">開く</button>
        <input
          ref={fileIn}
          type="file"
          accept=".wireskein,.sr,.vcd,.zip"
          style={{ display: 'none' }}
          onChange={(e) => {
            const f = e.currentTarget.files?.[0];
            if (f) open({ kind: 'file', file: f });
            e.currentTarget.value = '';
          }}
        />
        <Menu label="例">
          <For each={EXAMPLES}>{([label, url]) => <button onClick={() => open({ kind: 'url', url })}>{label}</button>}</For>
          <p class="dim small">（tools/serve.mjs の --files で配る所から）</p>
        </Menu>
        <Menu label="合成">
          <For each={[1e7, 1e8, 1e9]}>
            {(n) => <button onClick={() => open({ kind: 'synth', total: n, items: 5e5 })}>{`${n.toExponential(0)} の変化`}</button>}
          </For>
        </Menu>
        <Show when={(cap()?.nParts ?? 1) > 1}>
          <select value={cap()!.part} onChange={(e) => reopenPart(Number(e.currentTarget.value))}>
            <For each={Array.from({ length: cap()!.nParts }, (_, i) => i)}>{(k) => <option value={k}>{`区画 ${k}`}</option>}</For>
          </select>
        </Show>
        <span class="sep" />
        <Menu label="パネル">
          <button onClick={() => addPanel('wave')}>波形を足す</button>
          <button onClick={() => addPanel('list')}>一覧を足す</button>
          <button onClick={() => addPanel('measure')}>測定</button>
          <button onClick={() => addPanel('search')}>検索</button>
          <button onClick={() => addPanel('info')}>主張</button>
          <button onClick={() => addPanel('perf')}>速さ</button>
          <hr />
          <button onClick={() => openListWindow()}>一覧を独立した窓に</button>
        </Menu>
        <Menu label={`レイアウト: ${layoutName()}`}>
          <p class="dim small">既定</p>
          <For each={PRESETS}>
            {(p) => (
              <button
                onClick={() => {
                  presetLayout(p);
                  setLayoutName(p);
                }}
              >
                {p}
              </button>
            )}
          </For>
          <Show when={saved().length}>
            <hr />
            <p class="dim small">保存したもの</p>
            <For each={saved()}>
              {(n) => (
                <div class="row">
                  <button
                    onClick={() => {
                      loadLayout(n);
                      setLayoutName(n);
                    }}
                  >
                    {n}
                  </button>
                  <button
                    class="small"
                    onClick={(e) => {
                      e.stopPropagation();
                      deleteLayout(n);
                      setSaved(Object.keys(savedLayouts()));
                    }}
                  >
                    消す
                  </button>
                </div>
              )}
            </For>
          </Show>
          <hr />
          <div class="row" onClick={(e) => e.stopPropagation()}>
            <input value={layoutName()} onInput={(e) => setLayoutName(e.currentTarget.value)} size={12} />
            <button
              onClick={() => {
                saveLayout(layoutName());
                setSaved(Object.keys(savedLayouts()));
              }}
            >
              今の形を保存
            </button>
          </div>
        </Menu>
        <span class="sep" />
        <div class="seg">
          <button classList={{ on: dark() }} onClick={() => setDark(true)}>ダーク</button>
          <button classList={{ on: !dark() }} onClick={() => setDark(false)}>ライト</button>
        </div>
      </div>
      <div class="dock" ref={dockEl} />
      <div class="status">
        <span>{`${perf.fps} fps · ${perf.frameMs.toFixed(1)} ms · 波形 ${perf.waveMs.toFixed(2)} ms`}</span>
        <Show when={lod().total && lod().ms === null}>
          <span><span class="spin" />{` 要約を作成中 ${lod().done}/${lod().total}`}</span>
        </Show>
        <Show when={cap()}>
          <span>{`${cap()!.lines.length} 本 · ${cap()!.total.toExponential(1)} の変化`}</span>
        </Show>
        <Show when={analyzing()}>
          <span><span class="spin" /> 解析中…（Worker）</span>
        </Show>
        <Show when={analysis()}>
          <span>{`主張 ${analysis()!.claims.length} · 項目 ${analysis()!.claims.reduce((a, c) => a + (c.layers[0]?.n ?? 0), 0).toLocaleString()} · 解析 ${analysis()!.seconds.toFixed(2)} s`}</span>
        </Show>
        <Show when={search.total}>
          <span>{`検索 ${search.total.toLocaleString()} 件`}</span>
        </Show>
        <span>{perf.renderer}</span>
        <span>{Object.entries(perf.workerMem).length ? `Worker ${mb(Object.values(perf.workerMem).reduce((a, b) => a + b, 0))}` : ''}</span>
      </div>
      <Show when={dragging()}>
        <div class="drop" style={{ 'border-color': pal().accent }}>ここに落とすと開く（.wireskein / .sr / VCD）</div>
      </Show>
    </div>
  );
}

render(() => <App />, document.getElementById('app')!);

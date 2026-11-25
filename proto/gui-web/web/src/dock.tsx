// The panels' dock (dockview-core, MIT): split, tabs, sizes, floating groups, several of a kind, a group popped out
// to a browser window of its own (the same page's state: dockview moves the elements; Solid's delegated events are
// set up in the new document), layouts saved by name (localStorage) and switched.

import { createDockview, themeDark, themeLight, type DockviewApi, type IContentRenderer, type DockviewPanelApi } from 'dockview-core';
import { delegateEvents, render } from 'solid-js/web';
import { createEffect } from 'solid-js';
import { WavePanel, type WaveParams } from './wave';
import { ListView, type ListParams, type ListSource } from './list';
import { InfoPanel, MeasurePanel, PerfPanel, SearchPanel } from './panels';
import { analysis, analyzing, cap, dark, hitsOf, scene, search, sel, select } from './state';
import { openListWindow } from './channel';
import dockviewCss from 'virtual:dockview-css';

export let dockApi: DockviewApi | null = null;
let nextId = 0;

export const mainListSource: ListSource = {
  claims: () => analysis()?.claims ?? null,
  pending: () => (analyzing() ? '解析中…（Web Worker の中。終わると一覧が出ます）' : null),
  tickHz: () => cap()?.tickHz ?? 1,
  sel,
  select,
  hits: hitsOf,
  hitsGen: () => search.layers,
  indexAt: (c, l, t) => scene?.index_at(c, l, t) ?? -1,
};

const TITLES: Record<string, string> = { wave: '波形', list: '一覧', measure: '測定', search: '検索', info: '主張', perf: '速さ' };
const DELEGATED = ['click', 'input', 'keydown', 'pointerdown', 'pointermove', 'pointerup', 'dblclick', 'contextmenu', 'compositionstart', 'compositionend'];

function idNum(id: string) {
  return Number(id.split('-')[1] ?? 0);
}

function makePanel(name: string): IContentRenderer {
  const element = document.createElement('div');
  element.className = 'panel';
  let dispose: (() => void) | null = null;
  return {
    element,
    init(p) {
      const api = p.api as DockviewPanelApi;
      const id = idNum(api.id);
      const params = p.params as Record<string, unknown>;
      const upd = (x: object) => api.updateParameters({ ...x });
      dispose = render(() => {
        switch (name) {
          case 'wave':
            return <WavePanel id={id} params={{ follow: (params.follow as boolean) ?? true }} onParams={(x: WaveParams) => upd(x)} />;
          case 'list':
            return (
              <ListView
                id={id}
                src={mainListSource}
                params={{
                  claim: (params.claim as number) ?? 0,
                  layer: (params.layer as number) ?? 0,
                  hidden: (params.hidden as string[]) ?? [],
                  follow: (params.follow as boolean) ?? true,
                }}
                onParams={(x: ListParams) => upd(x)}
                onPopout={() => popout(api.id)}
                onWindow={() => openListWindow()}
              />
            );
          case 'measure':
            return <MeasurePanel />;
          case 'search':
            return <SearchPanel />;
          case 'info':
            return <InfoPanel />;
          default:
            return <PerfPanel />;
        }
      }, element);
    },
    dispose() {
      dispose?.();
    },
  };
}

export function popout(panelId: string) {
  const p = dockApi?.getPanel(panelId);
  if (!p || !dockApi) return;
  dockApi.addPopoutGroup(p, {
    popoutUrl: './popout.html',
    position: { left: window.screenX + window.outerWidth - 40, top: window.screenY + 40, width: 620, height: 760 },
    onDidOpen: (e) => {
      delegateEvents(DELEGATED, e.window.document);
      e.window.document.documentElement.dataset.theme = dark() ? 'dark' : 'light';
      e.window.document.title = '結果の一覧（別の窓）';
    },
  });
}

export function addPanel(kind: string, params: object = {}, position?: object) {
  if (!dockApi) return;
  nextId++;
  const id = `${kind}-${nextId}`;
  const p = dockApi.addPanel({
    id,
    component: kind,
    title: `${TITLES[kind]}${kind === 'wave' || kind === 'list' ? ' ' + nextId : ''}`,
    params: kind === 'wave' || kind === 'list' ? { follow: true, ...params } : params,
    ...(position ? { position } : {}),
  } as Parameters<DockviewApi['addPanel']>[0]);
  return p;
}

export const PRESETS = ['標準', '一覧を大きく', '波形 2 つ'] as const;

export function presetLayout(kind: string) {
  const api = dockApi!;
  api.clear();
  nextId = 0;
  const w = api.width || window.innerWidth;
  const h = api.height || window.innerHeight;
  if (kind === '一覧を大きく') {
    const wave = addPanel('wave')!;
    const list = addPanel('list', {}, { referencePanel: wave.id, direction: 'below' })!;
    const rest = addPanel('search', {}, { referencePanel: list.id, direction: 'right' })!;
    addPanel('measure', {}, { referenceGroup: rest.group });
    addPanel('info', {}, { referenceGroup: rest.group });
    addPanel('perf', {}, { referenceGroup: rest.group });
    rest.group.api.setSize({ width: w * 0.38 });
    wave.group.api.setSize({ height: h * 0.45 });
    rest.api.setActive();
  } else if (kind === '波形 2 つ') {
    const w1 = addPanel('wave', { follow: false })!;
    const w2 = addPanel('wave', {}, { referencePanel: w1.id, direction: 'below' })!;
    const list = addPanel('list', {}, { referencePanel: w1.id, direction: 'right' })!;
    addPanel('search', {}, { referenceGroup: list.group });
    list.group.api.setSize({ width: w * 0.3 });
    w1.group.api.setSize({ height: h * 0.42 });
    list.api.setActive();
    void w2;
  } else {
    const wave = addPanel('wave')!;
    const list = addPanel('list', {}, { referencePanel: wave.id, direction: 'right' })!;
    addPanel('search', {}, { referenceGroup: list.group });
    const m = addPanel('measure', {}, { referencePanel: list.id, direction: 'below' })!;
    addPanel('info', {}, { referenceGroup: m.group });
    addPanel('perf', {}, { referenceGroup: m.group });
    list.group.api.setSize({ width: w * 0.32 });
    m.group.api.setSize({ height: h * 0.38 });
    list.api.setActive();
    m.api.setActive();
  }
}

const KEY = 'wsk-gui-web-layouts';
export function savedLayouts(): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem(KEY) ?? '{}');
  } catch {
    return {};
  }
}
export function saveLayout(name: string) {
  if (!dockApi) return;
  const all = savedLayouts();
  all[name] = JSON.stringify(dockApi.toJSON());
  localStorage.setItem(KEY, JSON.stringify(all));
}
export function deleteLayout(name: string) {
  const all = savedLayouts();
  delete all[name];
  localStorage.setItem(KEY, JSON.stringify(all));
}
export function loadLayout(name: string): boolean {
  const j = savedLayouts()[name];
  if (!j || !dockApi) return false;
  dockApi.fromJSON(JSON.parse(j));
  nextId = Math.max(0, ...dockApi.panels.map((p) => idNum(p.id))) + 1;
  return true;
}

export function mountDock(el: HTMLElement, layout: string) {
  const style = document.createElement('style');
  style.textContent = dockviewCss;
  document.head.prepend(style);
  dockApi = createDockview(el, {
    createComponent: (o) => makePanel(o.name),
    theme: dark() ? themeDark : themeLight,
    popoutUrl: './popout.html',
  });
  createEffect(() => dockApi!.updateOptions({ theme: dark() ? themeDark : themeLight }));
  if (!loadLayout(layout)) presetLayout(layout);
  return dockApi;
}

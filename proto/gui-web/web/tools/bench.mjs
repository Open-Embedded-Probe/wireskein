#!/usr/bin/env node
// Prototype 2 driven in a headless Chrome (puppeteer-core, the Chrome installed on the machine): a screenshot and /
// or the bench (?bench=1) of a URL, the console, the time to "ready" (summaries and analysis done) and the memory of
// Chrome's processes (RSS of the whole tree: the page, its workers, the GPU process).
//
//   node tools/bench.mjs URL [--shot OUT.png] [--bench OUT.txt] [--size 1600x960] [--chrome PATH] [--headful]
//                            [--wait-ms N] [--click SELECTOR] [--type SELECTOR=TEXT] [--popout OUT2.png]
//
// The machine this was written on has no GPU for Chrome: WebGPU runs on SwiftShader (the CPU), so its fps are not a
// GPU's. On a screen with a GPU open the same URL with &bench=1 in a normal Chrome and read the speed panel.

import puppeteer from 'puppeteer-core';
import fs from 'node:fs';
import { execSync } from 'node:child_process';

const args = process.argv.slice(2);
const url = args[0];
let shot = null;
let bench = null;
let size = [1600, 960];
let chrome = process.env.CHROME ?? '/usr/bin/google-chrome';
let headful = false;
let waitMs = 0;
let popoutShot = null;
let timeoutS = 900;
let noReady = false;
let noGpuShot = false;
const clicks = [];
const types = [];
for (let i = 1; i < args.length; i++) {
  const a = args[i];
  const v = () => args[++i];
  if (a === '--shot') shot = v();
  else if (a === '--bench') bench = v();
  else if (a === '--size') size = v().split('x').map(Number);
  else if (a === '--chrome') chrome = v();
  else if (a === '--headful') headful = true;
  else if (a === '--wait-ms') waitMs = Number(v());
  else if (a === '--click') clicks.push(v());
  else if (a === '--type') types.push(v());
  else if (a === '--popout') popoutShot = v();
  else if (a === '--timeout') timeoutS = Number(v());
  else if (a === '--no-ready') noReady = true;
  else if (a === '--no-gpu-shot') noGpuShot = true;
}

function treeRss(pid) {
  // every process under Chrome's: RSS in MB, by kind
  const out = execSync('ps -eo pid=,ppid=,rss=,args=').toString().trim().split('\n');
  const rows = out.map((l) => {
    const m = l.trim().match(/^(\d+)\s+(\d+)\s+(\d+)\s+(.*)$/);
    return { pid: +m[1], ppid: +m[2], rss: +m[3], args: m[4] };
  });
  const kids = new Map();
  for (const r of rows) (kids.get(r.ppid) ?? kids.set(r.ppid, []).get(r.ppid)).push(r);
  const by = {};
  let total = 0;
  const walk = (p) => {
    for (const r of kids.get(p) ?? []) {
      const t = (r.args.match(/--type=([a-z-]+)/) ?? [null, 'browser'])[1];
      const k = t === 'utility' ? 'utility' : t;
      by[k] = (by[k] ?? 0) + r.rss / 1024;
      total += r.rss / 1024;
      walk(r.pid);
    }
  };
  const self = rows.find((r) => r.pid === pid);
  if (self) {
    by.browser = (by.browser ?? 0) + self.rss / 1024;
    total += self.rss / 1024;
  }
  walk(pid);
  return { total, by };
}

const browser = await puppeteer.launch({
  executablePath: chrome,
  headless: !headful,
  defaultViewport: { width: size[0], height: size[1] },
  args: [
    `--window-size=${size[0]},${size[1]}`,
    '--enable-unsafe-webgpu',
    '--enable-features=Vulkan',
    '--use-angle=swiftshader',
    '--enable-unsafe-swiftshader',
    '--disable-popup-blocking',
    '--enable-precise-memory-info',
    '--no-first-run',
  ],
});
setTimeout(async () => {
  console.log(`timed out after ${timeoutS} s`);
  await browser.close();
  process.exit(1);
}, timeoutS * 1000).unref();
const page = await browser.newPage();
const t0 = Date.now();
page.on('console', (m) => console.log(`[page ${((Date.now() - t0) / 1e3).toFixed(1)} s] ${m.text()}`));
page.on('pageerror', (e) => console.log(`[page error] ${e.message}`));
await page.goto(url, { waitUntil: 'load' });
// ready: the first picture, the summaries and the analysis
if (!noReady) await page.waitForFunction('window.__ready === true', { timeout: 900_000, polling: 250 });
const readyS = (Date.now() - t0) / 1e3;
console.log(`ready after ${readyS.toFixed(1)} s`);
for (const t of types) {
  const [sel, text] = t.split(/=(.*)/s);
  await page.click(sel);
  await page.keyboard.type(text);
}
for (const c of clicks) await page.click(c);
if (waitMs) await new Promise((r) => setTimeout(r, waitMs));
console.log(`status: ${await page.evaluate("document.querySelector('.status')?.textContent")}`);
if (bench) {
  await page.waitForFunction('typeof window.__bench === "string"', { timeout: 900_000, polling: 500 });
  const text = await page.evaluate('window.__bench');
  const mem = treeRss(browser.process().pid);
  const by = Object.entries(mem.by).map(([k, v]) => `${k} ${v.toFixed(0)}`).join(', ');
  const all = `${text}Chrome processes RSS ${mem.total.toFixed(0)} MB (${by})\n`;
  fs.writeFileSync(bench, all);
  console.log(all);
}
if (shot) {
  await new Promise((r) => setTimeout(r, 800));
  // WebGPU canvases are missing from headless screenshots: their frame read back and laid over them
  if (!noGpuShot) await page.evaluate('window.__gpuShot && window.__gpuShot()');
  await page.screenshot({ path: shot });
  console.log(`screenshot: ${shot}`);
}
if (popoutShot) {
  const pages = await browser.pages();
  const pop = pages.find((p) => p !== page && p.url() !== 'about:blank');
  if (pop) {
    await pop.setViewport({ width: 620, height: 760 });
    await new Promise((r) => setTimeout(r, 1500));
    await pop.screenshot({ path: popoutShot });
    console.log(`popout window (${pop.url()}): ${popoutShot}`);
  } else console.log('no other window');
}
const mem = treeRss(browser.process().pid);
console.log(`Chrome processes RSS ${mem.total.toFixed(0)} MB: ${Object.entries(mem.by).map(([k, v]) => `${k} ${v.toFixed(0)}`).join(', ')}`);
await browser.close();

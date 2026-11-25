import { defineConfig, type Plugin } from 'vite';
import solid from 'vite-plugin-solid';
import fs from 'node:fs';
import { createRequire } from 'node:module';

// COOP / COEP: the page cross-origin isolated (precise timers, measureUserAgentSpecificMemory; SharedArrayBuffer
// if the wasm ever runs threads). tools/serve.mjs sends the same headers for the built files.
const isolate = {
  'Cross-Origin-Opener-Policy': 'same-origin',
  'Cross-Origin-Embedder-Policy': 'require-corp',
};

// dockview-core 8.9.1's ESM build ships without its stylesheet (only the UMD build injects it): the CSS taken out of
// the UMD file as a module `virtual:dockview-css` (a string).
function dockviewCss(): Plugin {
  const id = 'virtual:dockview-css';
  return {
    name: 'dockview-css',
    resolveId: (s) => (s === id ? '\0' + id : null),
    load(s) {
      if (s !== '\0' + id) return null;
      const req = createRequire(import.meta.url);
      const umd = fs.readFileSync(req.resolve('dockview-core/dist/dockview-core.js'), 'utf8');
      const m = umd.match(/s\.textContent = ("(?:[^"\\]|\\.)*");/);
      if (!m) throw new Error('dockview-core: no stylesheet in the UMD build');
      return `export default ${m[1]};`;
    },
  };
}

export default defineConfig({
  base: './',
  plugins: [solid(), dockviewCss()],
  worker: { format: 'es' },
  build: {
    target: 'es2022',
    rollupOptions: { input: { main: 'index.html', list: 'list.html' } },
  },
  server: { headers: isolate },
  preview: { headers: isolate },
});

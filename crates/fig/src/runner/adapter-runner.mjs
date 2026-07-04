// fig adapter runner — drives a SiteGeneratorAdapter (the SAME contract the
// fhir-ig-editor uses) over the wasm_api Session, outside the browser.
//
// One contract, three hosts: browser worker / fig runner / user scripts. This
// harness mirrors the editor's App.tsx `adapter.init(ctx)` construction and the
// engine.worker.ts EngineClient surface + `unwrap` envelope check, so an adapter
// driven here behaves identically to its browser/worker run.
//
// Env (set by crates/fig/src/runner.rs):
//   FIG_ADAPTER       path to the adapter .mjs (default export or named adapter)
//   FIG_WASM_DIR      dir with the NODEJS-target wasm_api.js + wasm_api_bg.wasm
//   FIG_PROJECT_JSON  { projectId, config, files, predefined, siteFiles, buildEpochSecs }
//   FIG_BUNDLES_JSON  [{ label, files: { name: b64 } }]  (Session.init shape)
//   FIG_OUT_DIR       output dir for rendered pages
//   FIG_EDITOR_APP    (optional) fhir-ig-editor/app dir; when set, adapters that
//                     reach into the editor's preview/render (cycle) can resolve it

import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { pathToFileURL } from 'node:url';

const env = process.env;
const OUT = env.FIG_OUT_DIR;

// ---- load the wasm module (nodejs target: self-initializes on import) --------
const wasmMod = await import(pathToFileURL(join(env.FIG_WASM_DIR, 'wasm_api.js')).href);
// nodejs target self-inits; web target needs default(bytes). Support both.
if (typeof wasmMod.default === 'function') {
  try { await wasmMod.default(join(env.FIG_WASM_DIR, 'wasm_api_bg.wasm')); } catch { /* nodejs target: already init */ }
}
const session = new wasmMod.Session();

// ---- the ONE envelope check (engine.worker.ts `unwrap`) ----------------------
function unwrap(envelopeJson) {
  const e = JSON.parse(envelopeJson);
  if (e.apiVersion !== 1) throw new Error(`unsupported engine apiVersion ${e.apiVersion}`);
  if (!e.ok) throw new Error(`${e.op}: ${e.error.message}`);
  return e.result;
}

// ---- inputs ------------------------------------------------------------------
const project = JSON.parse(readFileSync(env.FIG_PROJECT_JSON, 'utf8'));
const bundlesJson = readFileSync(env.FIG_BUNDLES_JSON, 'utf8');
unwrap(session.init(bundlesJson));

// ---- EngineClient shim — the exact engine.worker.ts op surface ---------------
// site.db rows held here, so renderPage/assetBytes render on demand (worker
// keeps this in `lastRows`).
let lastRows = null;
let editorRender = null; // lazily-imported editor preview/render (cycle path)

async function ensureEditorRender() {
  if (editorRender) return editorRender;
  if (!env.FIG_EDITOR_APP) {
    throw new Error(
      'this adapter uses the editor preview/render pipeline (buildSite/renderPage); ' +
      'set FIG_EDITOR_APP to the fhir-ig-editor/app dir so the runner can resolve it');
  }
  editorRender = await import(pathToFileURL(join(env.FIG_EDITOR_APP, 'src/preview/render')).href);
  return editorRender;
}

const engine = {
  // stock-template render surface (thin Session wrappers) —
  async mountSite(files, options) {
    return unwrap(session.mountSite(JSON.stringify(files), options ? JSON.stringify(options) : ''));
  },
  async listSitePages() { return unwrap(session.listPages()); },
  async renderSitePage(name) {
    const t0 = performance.now();
    const { html } = unwrap(session.renderPage(name));
    return { html, renderMs: performance.now() - t0 };
  },
  async renderFragment(ref, kind) { return unwrap(session.renderFragment(ref, kind)); },
  async renderLiquid(source, data) {
    return unwrap(session.renderLiquid(source, data ? JSON.stringify(data) : ''));
  },
  async renderMarkdown(md, opts) {
    return unwrap(session.renderMarkdown(md, opts ? JSON.stringify(opts) : ''));
  },
  async compile(config, files, predefined) {
    return unwrap(session.compile(JSON.stringify(files), config, JSON.stringify(predefined)));
  },
  async snapshot(url) { return unwrap(session.snapshot(url)); },
  async expandValueSet(vsJson, resourcesJson) { return unwrap(session.expandValueSet(vsJson, resourcesJson)); },
  async resolveProject(config, versionIndex) { return unwrap(session.resolveProject(config, versionIndex ?? '')); },

  // cycle-generator preview path (buildSite → editor preview/render) —
  async buildSite(config, files, predefined, siteFiles, buildEpochSecs) {
    const input = { config, fsh: files, predefined, site_files: siteFiles, build_epoch_secs: buildEpochSecs };
    lastRows = unwrap(session.buildSiteDb(JSON.stringify(input)));
    const render = await ensureEditorRender();
    render.setEngineContent({
      renderLiquid: (source, dataJson) => unwrap(session.renderLiquid(source, dataJson)).html,
      mountSite: (filesJson, optionsJson) => { unwrap(session.mountSite(filesJson, optionsJson)); },
    });
    render.mountEngineSite(lastRows);
    const pages = render.listPages(lastRows);
    const assets = lastRows.assets.map((a) => ({ name: a.Name, mime: a.Mime }));
    return { pages, assets };
  },
  async renderPage(file) {
    if (!lastRows) throw new Error('renderPage before buildSite');
    const render = await ensureEditorRender();
    const { html } = render.renderPage(lastRows, file);
    return { file, html };
  },
  async assetBytes(name) {
    if (!lastRows) return null;
    const a = lastRows.assets.find((x) => x.Name === name);
    return a ? { name: a.Name, mime: a.Mime, base64: a.Content } : null;
  },
};

// ---- the AdapterContext (exactly App.tsx:125-140) ----------------------------
const ctx = {
  engine,
  fragments: { fragment: (ref, kind) => engine.renderFragment(ref, kind).then((r) => r.html) },
  content: {
    renderLiquid: (src, data) => engine.renderLiquid(src, data).then((r) => r.html),
    renderMarkdown: (md, opts) => engine.renderMarkdown(md, opts).then((r) => r.html),
  },
  project,
};

// ---- load + drive the adapter ------------------------------------------------
const adapterMod = await import(pathToFileURL(env.FIG_ADAPTER).href);
const adapter =
  adapterMod.default ??
  adapterMod.adapter ??
  Object.values(adapterMod).find((v) => v && typeof v.init === 'function' && typeof v.renderPage === 'function');
if (!adapter) throw new Error(`no SiteGeneratorAdapter exported from ${env.FIG_ADAPTER}`);

await adapter.init(ctx);
const pages = await adapter.listPages();
mkdirSync(OUT, { recursive: true });
const written = [];
for (const p of pages) {
  const file = p.file ?? p.slug ?? p.name ?? p;
  const { html } = await adapter.renderPage(file);
  const dest = join(OUT, file);
  mkdirSync(dirname(dest), { recursive: true });
  writeFileSync(dest, html);
  written.push(file);
}
writeFileSync(join(OUT, '.fig-pages.json'), JSON.stringify(written));
console.error(`fig runner: adapter '${adapter.id ?? '?'}' rendered ${written.length} pages`);

// WASM P2 — engine Web Worker.
//
// Owns the wasm-bindgen module (wasm32-unknown-unknown, web target). Speaks the
// worker protocol from docs/fhir-ig-editor-spec.md §4:
//   {type:'init'}                     -> mount package bundles
//   {type:'compile'}                  -> {resources[], diagnostics[], buildMs}
//   {type:'snapshot', url}            -> {snapshot, messages, snapshotMs}
// The UI thread never blocks; all engine work is here.

let wasm = null;
let manifest = null;

// ---- gzip + tar inflation (mirrors package_acquisition::read_bundle) --------

async function gunzip(buf) {
  const ds = new DecompressionStream('gzip');
  const stream = new Response(buf).body.pipeThrough(ds);
  return new Uint8Array(await new Response(stream).arrayBuffer());
}

function untar(tar) {
  const files = {};
  let off = 0;
  const dec = new TextDecoder();
  while (off + 512 <= tar.length) {
    const header = tar.subarray(off, off + 512);
    let name = '';
    for (let i = 0; i < 100 && header[i] !== 0; i++) name += String.fromCharCode(header[i]);
    if (name === '') break;
    let sizeStr = '';
    for (let i = 124; i < 136 && header[i] !== 0 && header[i] !== 0x20; i++)
      sizeStr += String.fromCharCode(header[i]);
    const size = parseInt(sizeStr.trim(), 8) || 0;
    const typeflag = header[156];
    off += 512;
    const data = tar.subarray(off, off + size);
    off += Math.ceil(size / 512) * 512;
    if (typeflag === 0x30 || typeflag === 0) {
      const base = name.replace(/^package\//, '').replace(/^\.\//, '');
      files[base] = base64(data);
    }
  }
  return files;
}

function base64(bytes) {
  let bin = '';
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
}

// ---- protocol ---------------------------------------------------------------

async function ensureWasm() {
  if (wasm) return;
  wasm = await import('./data/pkg/wasm_api.js');
  await wasm.default(); // instantiate
}

async function doInit() {
  await ensureWasm();
  manifest = await (await fetch('./data/manifest.json')).json();
  const t0 = performance.now();
  const bundles = [];
  for (const b of manifest.bundles) {
    const tgz = new Uint8Array(await (await fetch('./data/' + b.tgz)).arrayBuffer());
    const files = untar(await gunzip(tgz));
    bundles.push({ label: b.label, files });
  }
  const n = wasm.init(JSON.stringify(bundles));
  const ms = performance.now() - t0;
  return { mounted: n, version: JSON.parse(wasm.version()), initMs: ms };
}

async function fetchText(rel) {
  return await (await fetch('./data/' + rel)).text();
}
async function fetchJson(rel) {
  return await (await fetch('./data/' + rel)).json();
}

async function doCompile() {
  await ensureWasm();
  const config = await fetchText(manifest.config);
  const files = {};
  for (const f of manifest.fsh) files[f.replace(/^cycle\//, '')] = await fetchText(f);
  const predefined = {};
  for (const p of manifest.predefined) predefined[p.replace(/^cycle\//, '')] = await fetchJson(p);

  const t0 = performance.now();
  const out = JSON.parse(
    wasm.compile(JSON.stringify(files), config, JSON.stringify(predefined))
  );
  const buildMs = performance.now() - t0;
  return { ...out, buildMs, fileCount: Object.keys(files).length };
}

async function doSnapshot(url) {
  await ensureWasm();
  const t0 = performance.now();
  const out = JSON.parse(wasm.generate_snapshot(url));
  const snapshotMs = performance.now() - t0;
  return { ...out, snapshotMs };
}

self.onmessage = async (e) => {
  const { id, type, url } = e.data;
  try {
    let result;
    if (type === 'init') result = await doInit();
    else if (type === 'compile') result = await doCompile();
    else if (type === 'snapshot') result = await doSnapshot(url);
    else throw new Error('unknown message type: ' + type);
    self.postMessage({ id, ok: true, result });
  } catch (err) {
    self.postMessage({ id, ok: false, error: String(err && err.stack ? err.stack : err) });
  }
};

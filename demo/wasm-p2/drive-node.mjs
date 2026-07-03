// Headless driver for the P2 demo: runs the SAME init -> compile -> snapshot
// flow the Web Worker does, but under Node against the nodejs-target module, so
// timings can be captured in CI without a browser. (The browser page/worker use
// the identical wasm + the identical typed surface.)
//
//   node drive-node.mjs <wasm-nodejs-dir> <bundle-dir> <cycle-dir>

import fs from 'node:fs';
import path from 'node:path';
import zlib from 'node:zlib';
import { performance } from 'node:perf_hooks';

const [WASM_DIR, BUNDLE_DIR, CYCLE_DIR] = process.argv.slice(2);
const mod = await import(path.join(WASM_DIR, 'wasm_api.js'));

function untarGz(tgz) {
  const tar = zlib.gunzipSync(fs.readFileSync(tgz));
  const files = {};
  let off = 0;
  while (off + 512 <= tar.length) {
    const h = tar.subarray(off, off + 512);
    let name = '';
    for (let i = 0; i < 100 && h[i] !== 0; i++) name += String.fromCharCode(h[i]);
    if (name === '') break;
    let s = '';
    for (let i = 124; i < 136 && h[i] !== 0 && h[i] !== 0x20; i++) s += String.fromCharCode(h[i]);
    const size = parseInt(s.trim(), 8) || 0;
    const type = h[156];
    off += 512;
    const data = tar.subarray(off, off + size);
    off += Math.ceil(size / 512) * 512;
    if (type === 0x30 || type === 0) files[name.replace(/^package\//, '')] = Buffer.from(data).toString('base64');
  }
  return files;
}

const LABELS = [
  'hl7.fhir.r4.core#4.0.1',
  'hl7.fhir.uv.tools.r4#1.1.2',
  'hl7.terminology.r4#7.2.0',
  'hl7.fhir.uv.extensions.r4#5.3.0',
  'hl7.fhir.r5.core#5.0.0',
];

// init
let t = performance.now();
const bundles = LABELS.map((label) => ({ label, files: untarGz(path.join(BUNDLE_DIR, `${label}.tgz`)) }));
const mounted = mod.init(JSON.stringify(bundles));
const initMs = performance.now() - t;
console.log(`engine: ${mod.version()}`);
console.log(`init: mounted ${mounted} packages in ${initMs.toFixed(1)} ms`);

// compile
function walk(dir, exts) {
  const out = [];
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) out.push(...walk(p, exts));
    else if (exts.includes(path.extname(e.name).slice(1).toLowerCase())) out.push(p);
  }
  return out.sort();
}
const config = fs.readFileSync(path.join(CYCLE_DIR, 'sushi-config.yaml'), 'utf8');
const files = {};
for (const f of walk(path.join(CYCLE_DIR, 'input/fsh'), ['fsh']))
  files[path.relative(CYCLE_DIR, f)] = fs.readFileSync(f, 'utf8');
const predefined = {};
const resDir = path.join(CYCLE_DIR, 'input/resources');
if (fs.existsSync(resDir))
  for (const f of walk(resDir, ['json']))
    predefined[path.relative(CYCLE_DIR, f)] = JSON.parse(fs.readFileSync(f, 'utf8'));

t = performance.now();
const compiled = JSON.parse(mod.compile(JSON.stringify(files), config, JSON.stringify(predefined)));
const buildMs = performance.now() - t;
console.log(`compile: ${compiled.resources.length} resources from ${Object.keys(files).length} FSH files in ${buildMs.toFixed(1)} ms`);

// snapshot every profile
const profiles = compiled.resources.filter((r) => r.resourceType === 'StructureDefinition' && r.url);
let total = 0;
for (const p of profiles) {
  t = performance.now();
  const snap = JSON.parse(mod.generate_snapshot(p.url));
  const ms = performance.now() - t;
  total += ms;
  const n = snap.snapshot && snap.snapshot.snapshot ? snap.snapshot.snapshot.element.length : 0;
  console.log(`  snapshot ${p.id.padEnd(28)} ${ms.toFixed(1).padStart(6)} ms  ${n} elements${snap.snapshot ? '' : '  ERROR: ' + snap.messages.join('; ')}`);
}
console.log(`snapshot total: ${profiles.length} profiles in ${total.toFixed(1)} ms`);
console.log(`compute total (compile + ${profiles.length} snapshots): ${(buildMs + total).toFixed(1)} ms`);

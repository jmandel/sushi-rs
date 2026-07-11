// WASM parity driver.
//
// Runs the 17-rung fixture ladder + the ips/mcode/sdc corpus gates AGAINST THE
// WASM BUILD (wasm32-unknown-unknown + wasm-bindgen, loaded under Node — no
// browser), comparing each generated snapshot's `snapshot.element` to the SAME
// goldens the native gates use. Byte parity native<->wasm, proven not assumed.
//
// Inputs (paths relative to the repo root, passed as argv):
//   argv[2] = repo root
//   argv[3] = dir with the wasm-bindgen nodejs output (wasm_api.js + _bg.wasm)
//   argv[4] = dir with prebuilt package bundles ({label}.tgz.b64 + files)
//
// The bundle dir layout is produced by wasm-parity.sh: for each package label it
// writes `<label>/<file>` raw bytes (already-inflated bundle entries). We read
// them, base64 them, and hand them to `init`.

import fs from 'node:fs';
import path from 'node:path';
import zlib from 'node:zlib';

const REPO = process.argv[2];
const WASM_DIR = process.argv[3];
const BUNDLE_DIR = process.argv[4];

const mod = await import(path.join(WASM_DIR, 'wasm_api.js'));
// The Session surface (the only wasm API since the free-function wrappers were
// deleted). Envelopes: { apiVersion, ok, op, result | error }.
const session = new mod.Session();
function unwrap(envJson) {
  const env = JSON.parse(envJson);
  if (env.apiVersion !== 1) throw new Error(`bad apiVersion ${env.apiVersion}`);
  if (!env.ok) throw new Error(`${env.op}: ${env.error.message}`);
  return env.result;
}

// ---- bundle loading -------------------------------------------------------
// Each package bundle is a gzipped tar (built by `rust_sushi bundle`). We inflate
// it here in Node (mirroring the browser's read_bundle path) into a
// {filename: base64bytes} map, which is what `init` consumes.

function untarGz(tgzPath) {
  const gz = fs.readFileSync(tgzPath);
  const tar = zlib.gunzipSync(gz);
  const files = {};
  let off = 0;
  while (off + 512 <= tar.length) {
    const header = tar.subarray(off, off + 512);
    // name: bytes 0..100
    let name = '';
    for (let i = 0; i < 100 && header[i] !== 0; i++) name += String.fromCharCode(header[i]);
    if (name === '') break; // two zero blocks = end
    // size: octal at 124..136
    let sizeStr = '';
    for (let i = 124; i < 136 && header[i] !== 0 && header[i] !== 0x20; i++)
      sizeStr += String.fromCharCode(header[i]);
    const size = parseInt(sizeStr.trim(), 8) || 0;
    const typeflag = header[156];
    off += 512;
    const data = tar.subarray(off, off + size);
    off += Math.ceil(size / 512) * 512;
    // typeflag '0' or '\0' = regular file. Strip a leading "package/" if present.
    if (typeflag === 0x30 || typeflag === 0) {
      const base = name.replace(/^package\//, '').replace(/^\.\//, '');
      files[base] = Buffer.from(data).toString('base64');
    }
  }
  return files;
}

function mountBundles(labels) {
  const bundles = labels.map((label) => {
    const tgz = path.join(BUNDLE_DIR, `${label}.tgz`);
    return { label, files: untarGz(tgz) };
  });
  const { mounted } = unwrap(session.init(JSON.stringify(bundles)));
  if (mounted !== labels.length)
    throw new Error(`init mounted ${mounted}, expected ${labels.length}`);
}

// ---- comparison helpers ---------------------------------------------------

function stable(v) {
  if (Array.isArray(v)) return v.map(stable);
  if (v && typeof v === 'object') {
    const out = {};
    for (const k of Object.keys(v).sort()) out[k] = stable(v[k]);
    return out;
  }
  return v;
}
function snapshotElements(sd) {
  return stable((sd && sd.snapshot && sd.snapshot.element) || []);
}
function eq(a, b) {
  return JSON.stringify(a) === JSON.stringify(b);
}

function readJson(p) {
  return JSON.parse(fs.readFileSync(p, 'utf8'));
}

// ---- ladder gate ----------------------------------------------------------

const LADDER = [
  'r4-patient-card-ms',
  'r5-patient-min',
  'r5-patient-card-ms',
  'r5-patient-card-ms-unsorted',
  'r5-patient-binding-overlay',
  'r5-patient-fixed-pattern',
  'r5-patient-merge-additive',
  'r5-patient-choice-type',
  'r5-patient-nested-child',
  'r5-patient-simple-slice',
  'r5-patient-slice-child',
  'r5-patient-reslice',
  'r5-patient-type-unfold',
  'r5-extension-simple',
  'r5-observation-reference-profile',
  'r5-real-moneyquantity',
  'r5-questionnaire-content-reference',
];

function runLadder() {
  let ok = 0;
  const failures = [];
  const fixturesDir = path.join(REPO, 'snapshot/fixtures');

  // The native `walk_parity`/`bundle_ladder` gate uses a SEPARATE context per
  // core: the `r4-` rung against an r4.core-only context, the `r5-` rungs against
  // an r5.core-only context (mounting both would let an r5 base win a shared URL).
  // We mirror that by re-`init`ing between the two groups.
  const groups = [
    { pkg: 'hl7.fhir.r4.core#4.0.1', rungs: LADDER.filter((n) => n.startsWith('r4-')) },
    { pkg: 'hl7.fhir.r5.core#5.0.0', rungs: LADDER.filter((n) => !n.startsWith('r4-')) },
  ];
  for (const { pkg, rungs } of groups) {
    mountBundles([pkg]);
    const locals = {};
    for (const name of rungs) {
      const fp = path.join(fixturesDir, `${name}.json`);
      if (fs.existsSync(fp)) locals[`${name}.json`] = readJson(fp);
    }
    unwrap(session.setLocalResources(JSON.stringify(locals)));
    for (const name of rungs) {
      const goldenPath = path.join(REPO, 'snapshot/goldens', `${name}.snapshot.json`);
      const fixturePath = path.join(fixturesDir, `${name}.json`);
      if (!fs.existsSync(goldenPath) || !fs.existsSync(fixturePath)) {
        failures.push(`${name}: missing fixture/golden`);
        continue;
      }
      const input = fs.readFileSync(fixturePath, 'utf8');
      const res = unwrap(session.snapshot(input));
      if (!res.snapshot) {
        failures.push(`${name}: engine error: ${res.messages.join('; ')}`);
        continue;
      }
      const expected = readJson(goldenPath);
      if (eq(snapshotElements(expected), snapshotElements(res.snapshot))) ok++;
      else failures.push(`${name}: snapshot.element mismatch`);
    }
  }
  return { ok, total: LADDER.length, failures };
}

// ---- corpus gate ----------------------------------------------------------
// Per-IG package lists = the oracle context from snapshot/AGENTS.md, verbatim.

const CORPUS = {
  ips: {
    packages: [
      'hl7.fhir.r4.core#4.0.1',
      'hl7.fhir.uv.ipa#1.1.0',
      'hl7.fhir.uv.extensions.r4#5.3.0',
    ],
  },
  mcode: {
    packages: [
      'hl7.fhir.r4.core#4.0.1',
      'hl7.fhir.us.core#6.1.0',
      'hl7.fhir.uv.genomics-reporting#2.0.0',
      'hl7.fhir.uv.extensions.r4#5.3.0',
    ],
  },
  sdc: {
    packages: [
      'hl7.fhir.r4.core#4.0.1',
      'hl7.fhir.uv.xver-r5.r4#0.1.0',
      'hl7.fhir.r4.examples#4.0.1',
      'hl7.fhir.uv.extensions.r4#5.3.0',
    ],
  },
};

function runCorpus(igKey) {
  const { packages } = CORPUS[igKey];
  mountBundles(packages);

  const igDir = path.join(REPO, 'snapshot/harvested/r4', igKey);
  const fixturesDir = path.join(igDir, 'fixtures');
  const goldensDir = path.join(igDir, 'goldens');

  // --local-dir resolution, matching check-harvested-r4.sh exactly: use the
  // manifest's `resourcesDir` (the full `fsh-generated/resources` — profiles
  // reference sibling local SDs not all present in the harvested `fixtures/`
  // subset) when it exists, else fall back to `fixtures/`.
  let localDir = fixturesDir;
  const manifestPath = path.join(igDir, 'manifest.json');
  if (fs.existsSync(manifestPath)) {
    const rd = readJson(manifestPath).resourcesDir;
    if (rd && fs.existsSync(rd) && fs.statSync(rd).isDirectory()) localDir = rd;
  }
  const locals = {};
  for (const f of fs.readdirSync(localDir)) {
    if (f.endsWith('.json')) {
      try {
        locals[f] = readJson(path.join(localDir, f));
      } catch {
        /* skip non-JSON / unreadable, as the native loader does */
      }
    }
  }
  unwrap(session.setLocalResources(JSON.stringify(locals)));

  let ok = 0;
  const failures = [];
  const goldens = fs.readdirSync(goldensDir).filter((f) => f.endsWith('.snapshot.json'));
  for (const g of goldens.sort()) {
    const name = g.replace(/\.snapshot\.json$/, '');
    const fixturePath = path.join(fixturesDir, `${name}.json`);
    if (!fs.existsSync(fixturePath)) {
      failures.push(`${name}: missing fixture`);
      continue;
    }
    const input = fs.readFileSync(fixturePath, 'utf8');
    const res = unwrap(session.snapshot(input));
    if (!res.snapshot) {
      failures.push(`${name}: engine error: ${res.messages.join('; ')}`);
      continue;
    }
    const expected = readJson(path.join(goldensDir, g));
    if (eq(snapshotElements(expected), snapshotElements(res.snapshot))) ok++;
    else failures.push(`${name}: snapshot.element mismatch`);
  }
  return { ok, total: goldens.length, failures };
}

// ---- main -----------------------------------------------------------------

let allPass = true;
function report(label, r, expected) {
  const pass = r.failures.length === 0 && r.ok === r.total && r.total === expected;
  allPass = allPass && pass;
  console.log(`${pass ? 'PASS' : 'FAIL'}  ${label}: ${r.ok}/${r.total} (expected ${expected})`);
  for (const f of r.failures.slice(0, 8)) console.log(`        - ${f}`);
}

console.log(`engine: ${mod.Session.version()}`);
report('ladder', runLadder(), 17);
report('ips', runCorpus('ips'), 29);
report('mcode', runCorpus('mcode'), 46);
report('sdc', runCorpus('sdc'), 73);

console.log('');
console.log(allPass ? 'WASM PARITY GATE: PASS' : 'WASM PARITY GATE: FAIL');
process.exit(allPass ? 0 : 1);

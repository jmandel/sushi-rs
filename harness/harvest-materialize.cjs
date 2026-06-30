#!/usr/bin/env node
// Materialize harvested snippets into a permanent corpus layout:
//   <root>/<slug>/sushi-config.yaml
//   <root>/<slug>/input/fsh/snip.fsh
// and a manifest.json (slug, sourceFile, describe, it, callee).
// The sushi-config mirrors fshToFhir()'s baked config: canonical http://example.org,
// FSHOnly true, fhirVersion 4.0.1, and NO version (fshToFhir leaves version undefined).
const fs = require('fs');
const path = require('path');

const SNIPPETS = process.argv[2];
const ROOT = process.argv[3];
if (!SNIPPETS || !ROOT) { console.error('usage: materialize.cjs <snippets.json> <root>'); process.exit(2); }

const snippets = JSON.parse(fs.readFileSync(SNIPPETS, 'utf8'));

// short tag per source file
function fileTag(sf) {
  const b = path.basename(sf).replace(/\.test\.ts$/, '');
  return b.replace(/^FSHImporter\.?/, '').replace(/^FshToFhir$/, 'FshToFhir').replace(/^FSHErrorListener$/, 'ErrorListener') || 'misc';
}
function slugify(s) {
  return (s || '').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 40) || 'case';
}

// Minimal config mirroring fshToFhir() defaults. id/name/status are required by the
// CLI scaffold but are inert under FSHOnly (no IG resource emitted).
const CONFIG = [
  'id: harvest',
  'canonical: http://example.org',
  'name: Harvest',
  'status: active',
  'version: 0.1.0',
  'fhirVersion: 4.0.1',
  'FSHOnly: true',
  ''
].join('\n');

fs.rmSync(ROOT, { recursive: true, force: true });
fs.mkdirSync(ROOT, { recursive: true });

const counters = {};
const manifest = [];
for (const sn of snippets) {
  const tag = fileTag(sn.sourceFile);
  counters[tag] = (counters[tag] || 0) + 1;
  const idx = String(counters[tag]).padStart(3, '0');
  const slug = `${slugify(tag)}-${idx}`;
  const dir = path.join(ROOT, slug);
  fs.mkdirSync(path.join(dir, 'input', 'fsh'), { recursive: true });
  fs.writeFileSync(path.join(dir, 'sushi-config.yaml'), CONFIG);
  fs.writeFileSync(path.join(dir, 'input', 'fsh', 'snip.fsh'), sn.fsh);
  manifest.push({ slug, sourceFile: sn.sourceFile, describe: sn.describe, it: sn.it, callee: sn.callee });
}
fs.writeFileSync(path.join(ROOT, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
console.error(`materialized ${manifest.length} cases into ${ROOT}`);

#!/usr/bin/env node
const fs = require('fs');
const path = require('path');

function usage() {
  console.error('usage: node snapshot/harvest-r4-sushi.cjs [--include-local-base] [--limit N] <resources-dir> <harvest-dir>');
  process.exit(2);
}

let includeLocalBase = false;
let limit = 0;
const args = process.argv.slice(2);
while (args[0] && args[0].startsWith('--')) {
  const arg = args.shift();
  if (arg === '--include-local-base') {
    includeLocalBase = true;
  } else if (arg === '--limit') {
    limit = Number(args.shift());
    if (!Number.isInteger(limit) || limit < 0) usage();
  } else {
    usage();
  }
}
if (args.length !== 2) usage();

const resourcesDir = args[0];
const harvestDir = args[1];
const fixturesDir = path.join(harvestDir, 'fixtures');
fs.mkdirSync(fixturesDir, { recursive: true });

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, 'utf8'));
}

function sanitize(s) {
  return String(s || 'unknown').replace(/[^A-Za-z0-9._-]+/g, '-');
}

const files = fs.readdirSync(resourcesDir)
  .filter(f => /^StructureDefinition-.*\.json$/.test(f))
  .sort();
const structures = files.map(file => {
  const full = path.join(resourcesDir, file);
  return { file, full, json: readJson(full) };
});
const localUrls = new Set(structures.map(s => s.json.url).filter(Boolean));

const entries = [];
const skipped = [];
for (const source of structures) {
  const sd = source.json;
  const id = sd.id || path.basename(source.file, '.json').replace(/^StructureDefinition-/, '');
  function skip(reason) {
    skipped.push({ id, source: source.file, reason });
  }

  if (sd.resourceType !== 'StructureDefinition') {
    skip('not StructureDefinition');
    continue;
  }
  if (!String(sd.fhirVersion || '').startsWith('4')) {
    skip(`not R4 fhirVersion: ${sd.fhirVersion || '<missing>'}`);
    continue;
  }
  if (sd.derivation !== 'constraint') {
    skip(`not derivation=constraint: ${sd.derivation || '<missing>'}`);
    continue;
  }
  if (!sd.baseDefinition) {
    skip('missing baseDefinition');
    continue;
  }
  if (!includeLocalBase && localUrls.has(sd.baseDefinition)) {
    skip(`local baseDefinition: ${sd.baseDefinition}`);
    continue;
  }
  if (!sd.differential || !Array.isArray(sd.differential.element)) {
    skip('missing differential.element');
    continue;
  }

  const fixture = structuredClone(sd);
  delete fixture.snapshot;
  delete fixture.text;
  const filename = `${sanitize(id)}.json`;
  fs.writeFileSync(path.join(fixturesDir, filename), JSON.stringify(fixture, null, 2) + '\n');
  entries.push({
    id,
    url: sd.url || null,
    baseDefinition: sd.baseDefinition,
    source: source.file,
    fixture: `fixtures/${filename}`,
    differentialElements: sd.differential.element.length
  });
  if (limit && entries.length >= limit) break;
}

const manifest = {
  resourcesDir,
  includeLocalBase,
  limit,
  counts: {
    scanned: structures.length,
    harvested: entries.length,
    skipped: skipped.length
  },
  entries,
  skipped
};
fs.writeFileSync(path.join(harvestDir, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
console.log(`HARVESTED ${entries.length}/${structures.length} R4 constraint StructureDefinitions into ${fixturesDir}`);
if (skipped.length) {
  const byReason = new Map();
  for (const s of skipped) byReason.set(s.reason, (byReason.get(s.reason) || 0) + 1);
  for (const [reason, count] of [...byReason.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))) {
    console.log(`SKIPPED ${count}: ${reason}`);
  }
}

#!/usr/bin/env node
const fs = require('fs');
const path = require('path');

function usage() {
  console.error('usage: node snapshot/harvest-r4-package.cjs [--cache <packages-dir>] [--exclude-local-base] [--limit N] <pkg#ver> <harvest-dir>');
  process.exit(2);
}

const repo = path.resolve(__dirname, '..');
let cache = process.env.FHIR_CACHE || path.join(repo, 'temp/fhir-home/.fhir/packages');
let includeLocalBase = true;
let limit = 0;
const args = process.argv.slice(2);
while (args[0] && args[0].startsWith('--')) {
  const arg = args.shift();
  if (arg === '--cache') {
    cache = args.shift();
    if (!cache) usage();
  } else if (arg === '--exclude-local-base') {
    includeLocalBase = false;
  } else if (arg === '--limit') {
    limit = Number(args.shift());
    if (!Number.isInteger(limit) || limit < 0) usage();
  } else {
    usage();
  }
}
if (args.length !== 2) usage();

const packageSpec = args[0];
const harvestDir = args[1];
cache = path.resolve(cache);
const packageDir = path.join(cache, packageSpec, 'package');
const packageJsonFile = path.join(packageDir, 'package.json');
if (!fs.existsSync(packageJsonFile)) {
  throw new Error(`package is not installed in cache: ${packageSpec}`);
}

const fixturesDir = path.join(harvestDir, 'fixtures');
fs.mkdirSync(fixturesDir, { recursive: true });

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, 'utf8'));
}

function sanitize(s) {
  return String(s || 'unknown').replace(/[^A-Za-z0-9._-]+/g, '-');
}

const packageJson = readJson(packageJsonFile);
const files = fs.readdirSync(packageDir)
  .filter(file => file.endsWith('.json') && !file.startsWith('.'))
  .sort();
const structures = [];
for (const file of files) {
  const full = path.join(packageDir, file);
  let json;
  try {
    json = readJson(full);
  } catch {
    continue;
  }
  if (json.resourceType === 'StructureDefinition') {
    structures.push({ file, full, json });
  }
}
const localUrls = new Set(structures.map(s => s.json.url).filter(Boolean));
const packageFhirVersions = Array.isArray(packageJson.fhirVersions)
  ? packageJson.fhirVersions.map(String)
  : [];
const packageIsR4 = packageFhirVersions.some(v => v.startsWith('4.'));

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
  const fhirVersion = String(sd.fhirVersion || '');
  if (!(fhirVersion.startsWith('4') || (!fhirVersion && packageIsR4))) {
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
  source: 'package',
  package: packageSpec,
  packageDir,
  resourcesDir: packageDir,
  includeLocalBase,
  limit,
  packageJson: {
    name: packageJson.name || null,
    version: packageJson.version || null,
    canonical: packageJson.canonical || null,
    fhirVersions: packageJson.fhirVersions || null,
    dependencies: packageJson.dependencies || {}
  },
  counts: {
    scanned: structures.length,
    harvested: entries.length,
    skipped: skipped.length
  },
  entries,
  skipped
};
fs.writeFileSync(path.join(harvestDir, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
console.log(`HARVESTED ${entries.length}/${structures.length} R4 constraint StructureDefinitions from ${packageSpec} into ${fixturesDir}`);
if (skipped.length) {
  const byReason = new Map();
  for (const s of skipped) byReason.set(s.reason, (byReason.get(s.reason) || 0) + 1);
  for (const [reason, count] of [...byReason.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))) {
    console.log(`SKIPPED ${count}: ${reason}`);
  }
}

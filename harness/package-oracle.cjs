#!/usr/bin/env node
/*
 * package_store oracle (Phase 1): for a SUSHI project, resolve+load the FHIR
 * dependency graph exactly as stock SUSHI does, then dump what `fishForFHIR` /
 * `fishForMetadata` resolve for a set of canonical/id/name queries. This is the
 * golden for the Rust `package_store`.
 *
 * MUST run with HOME pointed at the ISOLATED cache (never real ~/.fhir):
 *   HOME=<repo>/temp/fhir-home node harness/package-oracle.cjs <project-dir> [query ...]
 *
 * Output JSON: { packages:[...], queries:[ {query, fhir:{resourceType,id,url,
 *   version,sha256}, meta:{...fishForMetadata...} } ] }
 * With no queries, dumps the loaded package list + index counts only.
 */
'use strict';
const fs = require('fs');
const crypto = require('crypto');

const SUSHI_ROOT = process.env.SUSHI_ROOT || '/home/jmandel/periodicity/node_modules/fsh-sushi';
const sushi = require(SUSHI_ROOT);
try { sushi.utils.logger.silent = true; } catch (_) {}
const proc = require(require('path').join(SUSHI_ROOT, 'dist/utils/Processing.js'));
const { Type } = sushi.utils;
const ALL_TYPES = [Type.Resource, Type.Type, Type.Profile, Type.Extension, Type.ValueSet, Type.CodeSystem, Type.Logical];

function sha256(v) {
  return v == null ? null : crypto.createHash('sha256').update(JSON.stringify(v)).digest('hex').slice(0, 16);
}

(async () => {
  const [projectDir, ...queries] = process.argv.slice(2);
  if (!projectDir) {
    console.error('usage: HOME=<iso> package-oracle.cjs <project-dir> [query ...]');
    process.exit(2);
  }
  if (!process.env.HOME || !process.env.HOME.includes('temp/fhir-home')) {
    console.error('REFUSING: HOME must point at the isolated cache (temp/fhir-home). Got: ' + process.env.HOME);
    process.exit(99);
  }
  const cfgPath = require('path').join(projectDir, 'sushi-config.yaml');
  const cfg = sushi.sushiImport.importConfiguration(fs.readFileSync(cfgPath, 'utf8'), cfgPath);
  const defs = await sushi.fhirdefs.createFHIRDefinitions();
  await proc.loadAutomaticDependencies(cfg.fhirVersion[0], cfg.dependencies || [], defs);
  await proc.loadExternalDependencies(defs, cfg);

  // Loaded package list (best-effort; FHIRDefinitions tracks package metadata).
  let packages = [];
  try {
    if (typeof defs.listPackages === 'function') packages = defs.listPackages();
    else if (Array.isArray(defs.packages)) packages = defs.packages;
  } catch (_) {}

  const out = { packages, queries: [] };
  for (const q of queries) {
    const fhir = defs.fishForFHIR(q, ...ALL_TYPES);
    const meta = defs.fishForMetadata(q, ...ALL_TYPES);
    out.queries.push({
      query: q,
      fhir: fhir ? { resourceType: fhir.resourceType, id: fhir.id, url: fhir.url, version: fhir.version, sha256: sha256(fhir) } : null,
      meta: meta || null,
    });
  }
  process.stdout.write(JSON.stringify(out, null, 2) + '\n');
})().catch((e) => { console.error('ERR', e.message); process.exit(1); });

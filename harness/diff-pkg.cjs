#!/usr/bin/env node
/*
 * Compare package-store fishing results (oracle vs rust). Ignores volatile
 * fields: fhir.sha256, meta.resourcePath (absolute path), and the packages list.
 * Compares per-query: fhir.{resourceType,id,url,version} and the meta object
 * (minus resourcePath). Exit 0 = parity.
 *
 * Usage: diff-pkg.cjs <oracle.json> <rust.json>
 */
'use strict';
const fs = require('fs');
const a = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const b = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'));

function cleanMeta(m) {
  if (!m) return m;
  const { resourcePath, ...rest } = m;
  return rest;
}
function cleanFhir(f) {
  if (!f) return f;
  return { resourceType: f.resourceType ?? null, id: f.id ?? null, url: f.url ?? null, version: f.version ?? null };
}
function byQuery(o) {
  const m = {};
  for (const q of o.queries || []) m[q.query] = { fhir: cleanFhir(q.fhir), meta: cleanMeta(q.meta) };
  return m;
}
const A = byQuery(a), B = byQuery(b);
const keys = new Set([...Object.keys(A), ...Object.keys(B)]);
let diffs = 0;
for (const q of keys) {
  const sa = JSON.stringify(A[q]), sb = JSON.stringify(B[q]);
  if (sa !== sb) {
    diffs++;
    console.log(`DIFF query=${q}`);
    console.log('  oracle:', sa);
    console.log('  rust  :', sb);
  }
}
if (diffs === 0) { console.log(`[diff-pkg] PARITY: ${keys.size} queries identical ✓`); process.exit(0); }
console.log(`[diff-pkg] ${diffs}/${keys.size} queries differ ✗`);
process.exit(1);

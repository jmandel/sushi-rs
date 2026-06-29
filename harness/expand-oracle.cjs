#!/usr/bin/env node
/*
 * Insert-expansion oracle (Phase 3): import FSH, build the FSHTank, run stock
 * SUSHI's applyInsertRules on every entity in FHIRExporter order, then dump the
 * POST-EXPANSION import AST as stable JSON (same shape as parse-oracle.cjs:
 * __type / __map / __bigint, plus appliedFile/appliedLocation stamped on inserted
 * rules). This is the golden for the Rust compiler's insert expansion.
 *
 * Usage: node expand-oracle.cjs <file.fsh ...> | --dir <dir>   > expanded.json
 */
'use strict';
const fs = require('fs');
const path = require('path');
const SUSHI_ROOT = process.env.SUSHI_ROOT || '/home/jmandel/periodicity/node_modules/fsh-sushi';
const sushi = require(SUSHI_ROOT);
try { sushi.utils.logger.silent = true; } catch (_) {}
const { importText, RawFSH, FSHTank } = sushi.sushiImport;
const { applyInsertRules } = require(path.join(SUSHI_ROOT, 'dist/fhirtypes/common.js'));

function toPlain(v, seen) {
  if (typeof v === 'bigint') return { __bigint: v.toString() };
  if (v === null || typeof v !== 'object') return v;
  if (seen.has(v)) return { __cycle: true };
  seen.add(v);
  let out;
  if (v instanceof Map) {
    out = { __map: {} };
    for (const [k, val] of v) out.__map[String(k)] = toPlain(val, seen);
  } else if (Array.isArray(v)) {
    out = v.map((x) => toPlain(x, seen));
  } else {
    out = {};
    const ctor = v.constructor && v.constructor.name;
    if (ctor && ctor !== 'Object') out.__type = ctor;
    for (const k of Object.keys(v)) out[k] = toPlain(v[k], seen);
  }
  seen.delete(v);
  return out;
}

// A minimal Configuration so FSHTank construction is happy.
const MIN_CONFIG = { canonical: 'http://example.org', FSHOnly: true, fhirVersion: ['4.0.1'] };

function main() {
  const args = process.argv.slice(2);
  let files = [];
  if (args[0] === '--dir') {
    const walk = (d) => fs.readdirSync(d, { withFileTypes: true }).forEach((e) => {
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p); else if (e.name.endsWith('.fsh')) files.push(p);
    });
    walk(args[1]); files.sort();
  } else files = args;
  if (!files.length) { console.error('usage: expand-oracle.cjs <file.fsh ...> | --dir <dir>'); process.exit(2); }

  const docs = importText(files.map((f) => new RawFSH(fs.readFileSync(f, 'utf8'), f)));
  const tank = new FSHTank(docs, MIN_CONFIG);

  // FHIRExporter order: invariants, then SDs (profiles++extensions++logicals++
  // resources), then codeSystems, valueSets, instances, mappings.
  const inOrder = [
    ...tank.getAllInvariants(),
    ...tank.getAllStructureDefinitions(),
    ...tank.getAllCodeSystems(),
    ...tank.getAllValueSets(),
    ...tank.getAllInstances(),
    ...tank.getAllMappings(),
  ];
  for (const entity of inOrder) {
    try { applyInsertRules(entity, tank); } catch (e) { /* leave; diagnostics not gated */ }
  }

  const plain = docs.map((d) => toPlain(d, new WeakSet()));
  process.stdout.write(JSON.stringify(plain, null, 2) + '\n');
}
main();

#!/usr/bin/env node
/*
 * Generate deterministic package-fishing queries from a materialized package
 * cache. Emits one query per line. Only fishable conformance resources are used:
 * StructureDefinition, ValueSet, and CodeSystem.
 *
 * Usage: gen-pkg-queries.cjs <cache-dir> [--max N]
 */
'use strict';

const fs = require('fs');
const path = require('path');

const args = process.argv.slice(2);
const cache = args[0];
if (!cache) {
  console.error('usage: gen-pkg-queries.cjs <cache-dir> [--max N]');
  process.exit(2);
}
let max = Number.POSITIVE_INFINITY;
const maxIndex = args.indexOf('--max');
if (maxIndex >= 0) {
  max = Number(args[maxIndex + 1]);
  if (!Number.isFinite(max) || max < 1) {
    console.error('--max must be a positive number');
    process.exit(2);
  }
}

const fishable = new Set(['StructureDefinition', 'ValueSet', 'CodeSystem']);
const queries = new Set();

function sortedDir(dir) {
  try {
    return fs.readdirSync(dir).sort();
  } catch {
    return [];
  }
}

for (const packageLabel of sortedDir(cache)) {
  const packageDir = path.join(cache, packageLabel, 'package');
  for (const filename of sortedDir(packageDir)) {
    if (filename.startsWith('.') || !filename.toLowerCase().endsWith('.json')) continue;
    if (filename === 'package.json') continue;
    let json;
    try {
      json = JSON.parse(fs.readFileSync(path.join(packageDir, filename), 'utf8'));
    } catch {
      continue;
    }
    if (!fishable.has(json.resourceType)) continue;
    for (const key of ['id', 'name', 'url']) {
      if (typeof json[key] === 'string' && json[key].length > 0) {
        queries.add(json[key]);
      }
    }
    if (typeof json.url === 'string' && json.url.length > 0 && typeof json.version === 'string' && json.version.length > 0) {
      queries.add(`${json.url}|${json.version}`);
    }
  }
}

queries.add('__definitely_not_a_real_fhir_artifact__');

let out = [...queries].sort();
if (out.length > max) {
  const sampled = [];
  if (max === 1) {
    sampled.push(out[0]);
  } else {
    for (let i = 0; i < max; i++) {
      sampled.push(out[Math.floor((i * (out.length - 1)) / (max - 1))]);
    }
  }
  out = [...new Set(sampled)];
}

process.stdout.write(out.join('\n') + (out.length ? '\n' : ''));

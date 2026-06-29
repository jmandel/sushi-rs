#!/usr/bin/env node
/*
 * Semantic AST comparator: compares two import-AST JSON dumps for equality,
 * ignoring object key order (significant: array order, types, values) and
 * normalizing every "file" field to its basename (path-portable). Returns
 * exit 0 on match, 1 on difference (printing the first divergence path).
 *
 * Usage: cmp-ast.cjs <oracle.json> <rust.json>
 */
'use strict';
const fs = require('fs');
const path = require('path');

function norm(v) {
  if (Array.isArray(v)) return v.map(norm);
  if (v && typeof v === 'object') {
    const o = {};
    for (const k of Object.keys(v)) {
      o[k] = k === 'file' && typeof v[k] === 'string' ? path.basename(v[k]) : norm(v[k]);
    }
    return o;
  }
  return v;
}

function diff(a, b, p) {
  if (a === b) return null;
  const ta = Array.isArray(a) ? 'array' : a === null ? 'null' : typeof a;
  const tb = Array.isArray(b) ? 'array' : b === null ? 'null' : typeof b;
  if (ta !== tb) return `${p}: type ${ta} != ${tb}`;
  if (ta === 'array') {
    if (a.length !== b.length) return `${p}: array len ${a.length} != ${b.length}`;
    for (let i = 0; i < a.length; i++) {
      const d = diff(a[i], b[i], `${p}[${i}]`);
      if (d) return d;
    }
    return null;
  }
  if (ta === 'object') {
    const ka = Object.keys(a).sort(), kb = Object.keys(b).sort();
    if (ka.join(',') !== kb.join(',')) return `${p}: keys {${ka}} != {${kb}}`;
    for (const k of ka) {
      const d = diff(a[k], b[k], `${p}.${k}`);
      if (d) return d;
    }
    return null;
  }
  return `${p}: ${JSON.stringify(a)} != ${JSON.stringify(b)}`;
}

const a = norm(JSON.parse(fs.readFileSync(process.argv[2], 'utf8')));
const b = norm(JSON.parse(fs.readFileSync(process.argv[3], 'utf8')));
const d = diff(a, b, '$');
if (d) {
  console.log(d);
  process.exit(1);
}
process.exit(0);

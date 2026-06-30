#!/usr/bin/env node
const fs = require('fs');

if (process.argv.length !== 4) {
  console.error('usage: node snapshot/diff-snapshot.cjs <expected.json> <actual.json>');
  process.exit(2);
}

const read = p => JSON.parse(fs.readFileSync(p, 'utf8'));

function stable(v) {
  if (Array.isArray(v)) return v.map(stable);
  if (v && typeof v === 'object') {
    return Object.fromEntries(Object.keys(v).sort().map(k => [k, stable(v[k])]));
  }
  return v;
}

function snapshotElements(v) {
  return stable(v?.snapshot?.element ?? []);
}

function diff(a, b, path = '$') {
  if (JSON.stringify(a) === JSON.stringify(b)) return null;
  if (Array.isArray(a) && Array.isArray(b)) {
    if (a.length !== b.length) return `${path}.length expected ${a.length} actual ${b.length}`;
    for (let i = 0; i < a.length; i++) {
      const d = diff(a[i], b[i], `${path}[${i}]`);
      if (d) return d;
    }
    return null;
  }
  if (a && b && typeof a === 'object' && typeof b === 'object') {
    const keys = [...new Set([...Object.keys(a), ...Object.keys(b)])].sort();
    for (const k of keys) {
      if (!(k in a)) return `${path}.${k} missing from expected`;
      if (!(k in b)) return `${path}.${k} missing from actual`;
      const d = diff(a[k], b[k], `${path}.${k}`);
      if (d) return d;
    }
    return null;
  }
  return `${path} expected ${JSON.stringify(a)} actual ${JSON.stringify(b)}`;
}

const expected = snapshotElements(read(process.argv[2]));
const actual = snapshotElements(read(process.argv[3]));
const d = diff(expected, actual);
if (d) {
  console.error(d);
  process.exit(1);
}
console.log('SNAPSHOT PARITY');


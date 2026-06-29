#!/usr/bin/env node
/*
 * Parser oracle: dump stock SUSHI's import AST (FSHDocument) for a .fsh file as
 * stable JSON. This is the golden the Rust Phase-2 parser is tested against.
 *
 * Usage: node parse-oracle.cjs <file.fsh> [more.fsh ...]   > ast.json
 *        node parse-oracle.cjs --dir <dir-of-fsh>          > ast.json
 *
 * Uses the installed stock fsh-sushi (v3.20.0), which matches submodule sushi-ts.
 */
'use strict';
const fs = require('fs');
const path = require('path');

const SUSHI_ROOT =
  process.env.SUSHI_ROOT || '/home/jmandel/periodicity/node_modules/fsh-sushi';
const sushi = require(SUSHI_ROOT);
const { sushiImport } = sushi;
const { importText, RawFSH } = sushiImport;

// Silence SUSHI's winston logger so stdout carries ONLY the JSON golden.
try {
  if (sushi.utils && sushi.utils.logger) sushi.utils.logger.silent = true;
} catch (_) { /* best effort */ }

// Recursively convert SUSHI's class/Map graph into plain, order-preserving JSON.
// Tags non-plain objects with __type so structural differences are visible.
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

function main() {
  const args = process.argv.slice(2);
  let files = [];
  if (args[0] === '--dir') {
    const dir = args[1];
    const walk = (d) => {
      for (const e of fs.readdirSync(d, { withFileTypes: true })) {
        const p = path.join(d, e.name);
        if (e.isDirectory()) walk(p);
        else if (e.name.endsWith('.fsh')) files.push(p);
      }
    };
    walk(dir);
    files.sort();
  } else {
    files = args;
  }
  if (!files.length) {
    console.error('usage: parse-oracle.cjs <file.fsh ...> | --dir <dir>');
    process.exit(2);
  }
  const raws = files.map((f) => new RawFSH(fs.readFileSync(f, 'utf8'), f));
  const docs = importText(raws);
  const plain = docs.map((d) => toPlain(d, new WeakSet()));
  process.stdout.write(JSON.stringify(plain, null, 2) + '\n');
}

main();

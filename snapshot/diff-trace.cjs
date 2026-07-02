#!/usr/bin/env node
// Compare two snapshot decision-trace JSONL files (oracle vs Rust walk engine).
//
// Records are aligned by `seq` (see snapshot/specs/trace-schema.md). The tool
// walks both traces in seq order, reports the FIRST divergence with context,
// and prints per-branch-label counts for each side plus a delta.
//
// Usage:
//   node snapshot/diff-trace.cjs <expected.trace.jsonl> <actual.trace.jsonl>
//
// Exit code 0 == traces are decision-identical; 1 == divergence (or read error).
// Deterministic: no timestamps, no randomness, stable ordering.

'use strict';
const fs = require('fs');

function usage(msg) {
  if (msg) process.stderr.write('diff-trace: ' + msg + '\n');
  process.stderr.write('usage: node snapshot/diff-trace.cjs <expected.jsonl> <actual.jsonl>\n');
  process.exit(2);
}

function readTrace(path) {
  let text;
  try {
    text = fs.readFileSync(path, 'utf8');
  } catch (e) {
    process.stderr.write('diff-trace: cannot read ' + path + ': ' + e.message + '\n');
    process.exit(2);
  }
  const recs = [];
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (!line.trim()) continue;
    let obj;
    try {
      obj = JSON.parse(line);
    } catch (e) {
      process.stderr.write('diff-trace: ' + path + ':' + (i + 1) + ': invalid JSON: ' + e.message + '\n');
      process.exit(2);
    }
    recs.push(obj);
  }
  return recs;
}

// Stable JSON for the `x` extras: sort object keys recursively so key order does
// not create false divergences.
function canon(v) {
  if (Array.isArray(v)) return v.map(canon);
  if (v && typeof v === 'object') {
    const out = {};
    for (const k of Object.keys(v).sort()) out[k] = canon(v[k]);
    return out;
  }
  return v;
}

// The comparable projection of a record. `seq` and `d` are alignment/metadata,
// not decision content, so they are compared structurally but not as "fields".
function keyFields(r) {
  return {
    fn: r.fn === undefined ? null : r.fn,
    branch: r.branch === undefined ? null : r.branch,
    base: r.base === undefined ? null : r.base,
    diff: r.diff === undefined ? null : r.diff,
    x: r.x === undefined ? null : canon(r.x),
  };
}

function fieldDiffs(a, b) {
  const ka = keyFields(a);
  const kb = keyFields(b);
  const diffs = [];
  for (const f of ['fn', 'branch', 'base', 'diff']) {
    if (ka[f] !== kb[f]) diffs.push({ field: f, expected: ka[f], actual: kb[f] });
  }
  const xa = JSON.stringify(ka.x);
  const xb = JSON.stringify(kb.x);
  if (xa !== xb) diffs.push({ field: 'x', expected: ka.x, actual: kb.x });
  return diffs;
}

function branchCounts(recs) {
  const c = new Map();
  for (const r of recs) {
    const b = r.branch === undefined ? '(no-branch)' : r.branch;
    c.set(b, (c.get(b) || 0) + 1);
  }
  return c;
}

function main() {
  const args = process.argv.slice(2);
  if (args.length !== 2) usage('need exactly two files');
  const [expPath, actPath] = args;

  const exp = readTrace(expPath);
  const act = readTrace(actPath);

  // Per-branch-label counts (printed regardless of pass/fail).
  const ce = branchCounts(exp);
  const ca = branchCounts(act);
  const allLabels = Array.from(new Set([...ce.keys(), ...ca.keys()])).sort();

  process.stdout.write('== trace diff ==\n');
  process.stdout.write('expected: ' + expPath + '  (' + exp.length + ' records)\n');
  process.stdout.write('actual:   ' + actPath + '  (' + act.length + ' records)\n\n');

  process.stdout.write('per-branch counts (expected | actual | delta):\n');
  let anyCountDelta = false;
  for (const label of allLabels) {
    const e = ce.get(label) || 0;
    const a = ca.get(label) || 0;
    const delta = a - e;
    if (delta !== 0) anyCountDelta = true;
    const mark = delta === 0 ? '   ' : (delta > 0 ? ' +>' : ' -<');
    process.stdout.write(
      '  ' + String(e).padStart(5) + ' | ' + String(a).padStart(5) +
      ' | ' + (delta > 0 ? '+' : '') + String(delta).padStart(4) + mark + ' ' + label + '\n');
  }
  process.stdout.write('\n');

  // First divergence, aligned by position in seq order.
  // (Records are emitted in seq order; we align by index and also sanity-check seq.)
  const n = Math.min(exp.length, act.length);
  let firstDiv = -1;
  let divInfo = null;
  for (let i = 0; i < n; i++) {
    const diffs = fieldDiffs(exp[i], act[i]);
    const seqMismatch = (exp[i].seq !== act[i].seq);
    if (diffs.length > 0 || seqMismatch) {
      firstDiv = i;
      divInfo = { diffs, seqMismatch };
      break;
    }
  }

  if (firstDiv === -1 && exp.length === act.length) {
    process.stdout.write('RESULT: OK — traces are decision-identical (' + exp.length + ' records)\n');
    process.exit(0);
  }

  process.stdout.write('RESULT: DIVERGENCE\n');

  if (firstDiv !== -1) {
    process.stdout.write('\nfirst divergence at record index ' + firstDiv +
      ' (expected seq=' + exp[firstDiv].seq + ', actual seq=' + act[firstDiv].seq + '):\n');
    if (divInfo.seqMismatch) {
      process.stdout.write('  ! seq mismatch: expected ' + exp[firstDiv].seq +
        ', actual ' + act[firstDiv].seq + ' (traces drifted out of alignment)\n');
    }
    for (const d of divInfo.diffs) {
      process.stdout.write('  field "' + d.field + '":\n');
      process.stdout.write('    expected: ' + JSON.stringify(d.expected) + '\n');
      process.stdout.write('    actual:   ' + JSON.stringify(d.actual) + '\n');
    }
    // A few records of context on each side.
    const lo = Math.max(0, firstDiv - 2);
    const hi = Math.min(n, firstDiv + 3);
    process.stdout.write('\ncontext (index ' + lo + '..' + (hi - 1) + '):\n');
    for (let i = lo; i < hi; i++) {
      const mark = i === firstDiv ? '>>' : '  ';
      process.stdout.write(mark + ' [' + i + '] expected: ' + JSON.stringify(keyFields(exp[i])) + '\n');
      process.stdout.write(mark + ' [' + i + '] actual:   ' + JSON.stringify(keyFields(act[i])) + '\n');
    }
  } else if (exp.length !== act.length) {
    // All aligned records matched but lengths differ.
    const longer = exp.length > act.length ? 'expected' : 'actual';
    process.stdout.write('\nall ' + n + ' aligned records match, but ' + longer +
      ' has ' + Math.abs(exp.length - act.length) + ' extra trailing record(s):\n');
    const extra = exp.length > act.length ? exp : act;
    for (let i = n; i < Math.min(extra.length, n + 5); i++) {
      process.stdout.write('  [' + i + '] ' + longer + ': ' + JSON.stringify(keyFields(extra[i])) + '\n');
    }
  }

  if (anyCountDelta) {
    process.stdout.write('\n(see per-branch delta above for aggregate label drift)\n');
  }
  process.exit(1);
}

main();

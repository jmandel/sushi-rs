#!/usr/bin/env node
/*
 * diff-fragment.cjs — normalize-free byte diff between two fragment/page files.
 *
 * Compares two files byte-for-byte (NO normalization of any kind: whitespace,
 * line endings, entity encoding, attribute order are all significant). On the
 * first divergence it reports the byte offset, the line/column, and a window of
 * context around the divergence in both files with the differing byte marked.
 *
 * This is the oracle-diff primitive for the Rust fragment/page renderer: a
 * golden and a candidate must be byte-identical to pass. Any intentional
 * normalization must be applied to BOTH inputs upstream and documented — this
 * tool never hides a difference.
 *
 * Usage:
 *   node diff-fragment.cjs <golden> <candidate> [--context N] [--quiet]
 *
 * Exit codes:
 *   0  identical
 *   1  differ (prints first-divergence context unless --quiet)
 *   2  usage / IO error
 */
'use strict';

const fs = require('fs');

function parseArgs(argv) {
  const out = { context: 60, quiet: false, files: [] };
  for (let i = 2; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--context') { out.context = parseInt(argv[++i], 10) || 60; }
    else if (a === '--quiet') { out.quiet = true; }
    else if (a === '-h' || a === '--help') { out.help = true; }
    else { out.files.push(a); }
  }
  return out;
}

function readFileOr(pathname) {
  try {
    return fs.readFileSync(pathname);
  } catch (e) {
    process.stderr.write(`diff-fragment: cannot read ${pathname}: ${e.message}\n`);
    process.exit(2);
  }
}

// Map a byte offset to 1-based line/column (counting bytes, LF = new line).
function lineColOf(buf, offset) {
  let line = 1, col = 1;
  for (let i = 0; i < offset && i < buf.length; i++) {
    if (buf[i] === 0x0a) { line++; col = 1; } else { col++; }
  }
  return { line, col };
}

// Render a byte window as a printable string; non-printables shown as \xNN.
function renderWindow(buf, start, end, markAt) {
  const parts = [];
  for (let i = start; i < end && i < buf.length; i++) {
    const b = buf[i];
    let s;
    if (b === 0x0a) s = '\\n';
    else if (b === 0x09) s = '\\t';
    else if (b === 0x0d) s = '\\r';
    else if (b >= 0x20 && b < 0x7f) s = String.fromCharCode(b);
    else s = '\\x' + b.toString(16).padStart(2, '0');
    if (i === markAt) s = '[' + s + ']';
    parts.push(s);
  }
  return parts.join('');
}

function main() {
  const args = parseArgs(process.argv);
  if (args.help || args.files.length !== 2) {
    process.stdout.write('usage: diff-fragment.cjs <golden> <candidate> [--context N] [--quiet]\n');
    process.exit(args.help ? 0 : 2);
  }
  const [pa, pb] = args.files;
  const a = readFileOr(pa);
  const b = readFileOr(pb);

  const min = Math.min(a.length, b.length);
  let diffAt = -1;
  for (let i = 0; i < min; i++) {
    if (a[i] !== b[i]) { diffAt = i; break; }
  }
  if (diffAt === -1 && a.length === b.length) {
    process.exit(0); // identical
  }
  if (diffAt === -1) {
    // one is a prefix of the other
    diffAt = min;
  }

  if (args.quiet) process.exit(1);

  const ctx = args.context;
  const start = Math.max(0, diffAt - ctx);
  const lcA = lineColOf(a, diffAt);
  const lcB = lineColOf(b, diffAt);

  process.stdout.write(`DIFFER at byte ${diffAt}\n`);
  process.stdout.write(`  golden    : ${pa} (${a.length} bytes) line ${lcA.line} col ${lcA.col}\n`);
  process.stdout.write(`  candidate : ${pb} (${b.length} bytes) line ${lcB.line} col ${lcB.col}\n`);
  const gByte = diffAt < a.length ? a[diffAt] : null;
  const cByte = diffAt < b.length ? b[diffAt] : null;
  const fmt = (x) => x === null ? '<EOF>' : `0x${x.toString(16).padStart(2, '0')}`;
  process.stdout.write(`  byte      : golden=${fmt(gByte)} candidate=${fmt(cByte)}\n`);
  process.stdout.write(`  --- golden    [${start}..${Math.min(a.length, diffAt + ctx)}) ---\n`);
  process.stdout.write(`  ${renderWindow(a, start, diffAt + ctx, diffAt)}\n`);
  process.stdout.write(`  --- candidate [${start}..${Math.min(b.length, diffAt + ctx)}) ---\n`);
  process.stdout.write(`  ${renderWindow(b, start, diffAt + ctx, diffAt)}\n`);
  process.exit(1);
}

main();

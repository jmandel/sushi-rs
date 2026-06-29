#!/usr/bin/env node
/*
 * Diagnostic parity tool. SUSHI prints diagnostics via winston as:
 *   <level> <message...>
 *     File: <path>
 *     Line: <startLine> - <endLine>
 *     [Applied in File: <path>]
 *     [Applied on Line: <a> - <b>]
 * (see sushi-ts/src/utils/FSHLogger.ts:16-114). This normalizes a captured
 * console log into ordered JSON records, and diffs two logs. Ordering is part of
 * parity, so the diff is order-sensitive.
 *
 * Usage:
 *   node diag.cjs normalize <console.log> [--levels error,warn]   > diags.json
 *   node diag.cjs diff <stock.log> <candidate.log> [--levels error,warn]
 */
'use strict';
const fs = require('fs');

const ANSI = /\x1b\[[0-9;]*m/g;
const LEVELS = new Set(['error', 'warn', 'warning', 'info', 'debug']);

function stripAnsi(s) {
  return s.replace(ANSI, '');
}

function normalize(logText, levelFilter) {
  const lines = stripAnsi(logText).split('\n');
  const records = [];
  let cur = null;
  const flush = () => {
    if (!cur) return;
    // Pull structured footers out of the accumulated body.
    const body = cur._body;
    const rec = { level: cur.level, message: '', file: null, startLine: null, endLine: null };
    const msgLines = [];
    for (const ln of body) {
      let m;
      if ((m = ln.match(/^\s{2,}File:\s(.*)$/))) rec.file = require('path').basename(m[1]);
      else if ((m = ln.match(/^\s{2,}Line:\s(\d+)(?:\s-\s(\d+))?/))) {
        rec.startLine = +m[1];
        rec.endLine = m[2] ? +m[2] : +m[1];
      } else if ((m = ln.match(/^\s{2,}Applied in File:\s(.*)$/))) rec.appliedFile = require('path').basename(m[1]);
      else if ((m = ln.match(/^\s{2,}Applied on Line:\s(\d+)(?:\s-\s(\d+))?/))) {
        rec.appliedStartLine = +m[1];
      } else msgLines.push(ln);
    }
    rec.message = msgLines.join('\n').trim();
    records.push(rec);
    cur = null;
  };
  for (const raw of lines) {
    const m = raw.match(/^(error|warn|warning|info|debug)\s(.*)$/i);
    if (m && LEVELS.has(m[1].toLowerCase())) {
      flush();
      cur = { level: m[1].toLowerCase().replace('warning', 'warn'), _body: [m[2]] };
    } else if (cur) {
      cur._body.push(raw);
    }
  }
  flush();
  if (levelFilter) return records.filter((r) => levelFilter.has(r.level));
  return records;
}

function parseLevels(args) {
  const i = args.indexOf('--levels');
  if (i === -1) return null;
  return new Set(args[i + 1].split(',').map((s) => s.trim().replace('warning', 'warn')));
}

function main() {
  const args = process.argv.slice(2);
  const cmd = args[0];
  const levels = parseLevels(args);
  if (cmd === 'normalize') {
    const recs = normalize(fs.readFileSync(args[1], 'utf8'), levels);
    process.stdout.write(JSON.stringify(recs, null, 2) + '\n');
  } else if (cmd === 'diff') {
    const a = normalize(fs.readFileSync(args[1], 'utf8'), levels);
    const b = normalize(fs.readFileSync(args[2], 'utf8'), levels);
    const n = Math.max(a.length, b.length);
    let diffs = 0;
    for (let i = 0; i < n; i++) {
      const x = a[i], y = b[i];
      if (JSON.stringify(x) !== JSON.stringify(y)) {
        diffs++;
        console.log(`#${i} DIFF`);
        console.log('  stock:', JSON.stringify(x));
        console.log('  cand :', JSON.stringify(y));
      }
    }
    if (diffs === 0) {
      console.log(`[diag] PARITY: ${a.length} diagnostics identical ✓`);
      process.exit(0);
    } else {
      console.log(`[diag] DIVERGENCE: ${diffs} differing of ${n} ✗`);
      process.exit(1);
    }
  } else {
    console.error('usage: diag.cjs normalize <log> | diff <a> <b>  [--levels error,warn]');
    process.exit(2);
  }
}
main();

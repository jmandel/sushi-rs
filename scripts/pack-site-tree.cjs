#!/usr/bin/env node
// Pack a staged site dir (template statics / _includes / _data / pagecontent /
// txcache) into the Session.mountSite files JSON:
//   { "<rel path>": "<utf8 text>" | { "b64": "<base64>" } }
// Usage: node scripts/pack-site-tree.cjs <dir> [--prefix <p>] > site.json
// Multiple trees can be merged by the caller (later keys win) — e.g. a stock
// template bundle overlaid with the IG's staged pagecontent.
const fs = require('fs');
const path = require('path');

const args = process.argv.slice(2);
const dir = args[0];
if (!dir || !fs.statSync(dir).isDirectory()) {
  console.error('usage: pack-site-tree.cjs <dir> [--prefix <p>]');
  process.exit(2);
}
const pi = args.indexOf('--prefix');
const prefix = pi >= 0 ? args[pi + 1].replace(/\/+$/, '') + '/' : '';

const out = {};
(function walk(d) {
  for (const e of fs.readdirSync(d, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    const p = path.join(d, e.name);
    if (e.isDirectory()) walk(p);
    else if (e.isFile()) {
      const rel = prefix + path.relative(dir, p).split(path.sep).join('/');
      const bytes = fs.readFileSync(p);
      const text = bytes.toString('utf8');
      // Round-trippable UTF-8 ships as text; anything else as b64.
      out[rel] = Buffer.from(text, 'utf8').equals(bytes) ? text : { b64: bytes.toString('base64') };
    }
  }
})(dir);
process.stdout.write(JSON.stringify(out));
console.error(`packed ${Object.keys(out).length} files from ${dir}`);

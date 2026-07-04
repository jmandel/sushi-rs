#!/usr/bin/env node
// snapshot/package-deps.cjs — transitive R4 context closure for a published IG
// package (the MOUNT set snapshot generation needs).
//
// DRY (task #32, retired in Consolidation Pass 1): the resolution logic lives in
// ONE place — Rust (`package_store::resolve::context_closure_for_root`, exposed
// as `rust_sushi resolve --cache <dir> --root <id#ver>`). This script is now a
// PURE SHIM that shells out to that native resolver: there is NO second
// implementation. (A Node reimplementation was retained as an offline fallback
// while the Rust-vs-Node parity gate soaked; the gate has soaked green for the
// full 8-IG set across #32, so the fallback is deleted here — one resolver,
// nothing to drift.) The 8-IG gate (snapshot/package-deps-gate.sh) is kept as a
// regression test of THIS shim's wiring: it asserts the shim's stdout equals a
// direct `rust_sushi resolve` invocation.
//
//   node snapshot/package-deps.cjs [--cache <packages-dir>] <pkg#ver>
//
// Env:
//   RUST_SUSHI_BIN  override the binary path (default: <repo>/target/release/rust_sushi)

const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');

function usage() {
  console.error('usage: node snapshot/package-deps.cjs [--cache <packages-dir>] <pkg#ver>');
  process.exit(2);
}

const repo = path.resolve(__dirname, '..');
let cache = process.env.FHIR_CACHE || path.join(repo, 'temp/fhir-home/.fhir/packages');
let root = null;
const args = process.argv.slice(2);
while (args.length) {
  const arg = args.shift();
  if (arg === '--cache') {
    cache = args.shift();
    if (!cache) usage();
  } else if (arg.startsWith('-')) {
    usage();
  } else if (root == null) {
    root = arg;
  } else {
    usage();
  }
}
if (root == null) usage();
cache = path.resolve(cache);

// The native Rust resolver is the single source of truth for the context closure.
const bin = process.env.RUST_SUSHI_BIN || path.join(repo, 'target/release/rust_sushi');
if (!fs.existsSync(bin)) {
  console.error(
    `FATAL: rust_sushi binary not found at ${bin}; build it: cargo build --release -p rust_sushi ` +
      `(or set RUST_SUSHI_BIN). This shim has no Node fallback — Rust is the only resolver.`,
  );
  process.exit(2);
}

const res = spawnSync(bin, ['resolve', '--cache', cache, '--root', root], {
  encoding: 'utf8',
  maxBuffer: 64 * 1024 * 1024,
});
if (res.status !== 0) {
  process.stderr.write(res.stderr || `rust_sushi resolve failed (${res.status})\n`);
  process.exit(res.status || 1);
}
process.stdout.write(res.stdout);

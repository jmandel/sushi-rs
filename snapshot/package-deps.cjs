#!/usr/bin/env node
// snapshot/package-deps.cjs — transitive R4 context closure for a published IG
// package (the MOUNT set snapshot generation needs).
//
// DRY (task #32): the resolution logic now lives in ONE place — Rust
// (`package_store::resolve::context_closure_for_root`, exposed as
// `rust_sushi resolve --cache <dir> --root <id#ver>`). This script is a THIN
// SHIM that shells out to that native resolver so there is no second
// implementation to drift. The former Node reimplementation is retained ONLY as
// an offline fallback (when the release binary is not built), and the two are
// gated byte-for-byte equal on 8 published IGs by `snapshot/package-deps-gate.sh`.
//
//   node snapshot/package-deps.cjs [--cache <packages-dir>] <pkg#ver>
//
// Env:
//   RUST_SUSHI_BIN         override the binary path (default: <repo>/target/release/rust_sushi)
//   PACKAGE_DEPS_NATIVE=1  REQUIRE the native resolver (fail instead of falling back)

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

// ---- Preferred path: the native Rust resolver (single source of truth). ----
function tryNative() {
  const bin = process.env.RUST_SUSHI_BIN || path.join(repo, 'target/release/rust_sushi');
  if (!fs.existsSync(bin)) return null;
  const res = spawnSync(bin, ['resolve', '--cache', cache, '--root', root], {
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  });
  if (res.status !== 0) {
    if (process.env.PACKAGE_DEPS_NATIVE === '1') {
      process.stderr.write(res.stderr || `rust_sushi resolve failed (${res.status})\n`);
      process.exit(res.status || 1);
    }
    return null;
  }
  return res.stdout;
}

const native = tryNative();
if (native != null) {
  process.stdout.write(native);
  process.exit(0);
}
if (process.env.PACKAGE_DEPS_NATIVE === '1') {
  console.error(
    'FATAL: PACKAGE_DEPS_NATIVE=1 but rust_sushi binary not found; build it: cargo build --release -p rust_sushi',
  );
  process.exit(2);
}

// ---- Offline fallback: the ORIGINAL Node algorithm (kept byte-parity-gated). ----
// This must stay identical in behavior to context_closure_for_root; the gate
// (snapshot/package-deps-gate.sh) fails CI if they ever diverge.

function parseSpec(spec) {
  const hash = spec.lastIndexOf('#');
  if (hash <= 0 || hash === spec.length - 1) {
    throw new Error(`package spec must be pkg#version: ${spec}`);
  }
  return { id: spec.slice(0, hash), version: spec.slice(hash + 1), spec };
}

function needsVersionResolution(version) {
  return version === 'latest' || version === 'current' || /(^|[.])x($|[.])|\*/i.test(version);
}

function canonicalVersion(id, version) {
  if (id === 'hl7.fhir.r4.core' && version === '4.0.0') return '4.0.1';
  return version;
}

function versionMatches(version, pattern) {
  if (pattern === 'latest' || pattern === 'current') return true;
  const parts = pattern.split('.');
  const versionParts = version.split('.');
  for (let i = 0; i < parts.length; i++) {
    const part = parts[i].toLowerCase();
    if (part === 'x' || part === '*') return true;
    if (versionParts[i] !== parts[i]) return false;
  }
  return true;
}

function compareVersions(l, r) {
  const lp = l.split(/[.-]/);
  const rp = r.split(/[.-]/);
  const len = Math.max(lp.length, rp.length);
  for (let i = 0; i < len; i++) {
    const a = lp[i] || '0';
    const b = rp[i] || '0';
    const an = /^\d+$/.test(a) ? Number(a) : null;
    const bn = /^\d+$/.test(b) ? Number(b) : null;
    if (an != null && bn != null && an !== bn) return an - bn;
    if (an != null && bn == null) return 1;
    if (an == null && bn != null) return -1;
    if (a !== b) return a.localeCompare(b);
  }
  return 0;
}

function resolveSpec(id, version) {
  version = canonicalVersion(id, version);
  if (!needsVersionResolution(version)) return `${id}#${version}`;
  const prefix = `${id}#`;
  const matches = fs.readdirSync(cache)
    .filter(name => name.startsWith(prefix))
    .map(name => name.slice(prefix.length))
    .filter(candidate => versionMatches(candidate, version))
    .sort(compareVersions);
  if (matches.length === 0) {
    throw new Error(`no cached version of ${id} matches ${version}; run install-fhir-package first`);
  }
  return `${id}#${matches[matches.length - 1]}`;
}

function readPackageJson(spec) {
  const file = path.join(cache, spec, 'package', 'package.json');
  return JSON.parse(fs.readFileSync(file, 'utf8'));
}

function isR4CompatiblePackage(json) {
  if (!Array.isArray(json.fhirVersions) || json.fhirVersions.length === 0) return true;
  return json.fhirVersions.some(version => String(version).startsWith('4.'));
}

const seen = new Set([root]);
const out = [];

function add(spec) {
  if (seen.has(spec)) return;
  seen.add(spec);
  const json = readPackageJson(spec);
  if (!isR4CompatiblePackage(json)) return;
  out.push(spec);
  for (const [id, version] of Object.entries(json.dependencies || {})) {
    add(resolveSpec(id, version));
  }
}

const rootJson = readPackageJson(root);
for (const [id, version] of Object.entries(rootJson.dependencies || {})) {
  add(resolveSpec(id, version));
}

if (!out.some(spec => parseSpec(spec).id === 'hl7.fhir.r4.core')) {
  const fhirVersions = rootJson.fhirVersions || [];
  if (Array.isArray(fhirVersions) && fhirVersions.some(v => String(v).startsWith('4.'))) {
    out.unshift('hl7.fhir.r4.core#4.0.1');
  }
}

out.sort((l, r) => {
  const li = parseSpec(l).id;
  const ri = parseSpec(r).id;
  if (li === 'hl7.fhir.r4.core' && ri !== 'hl7.fhir.r4.core') return -1;
  if (ri === 'hl7.fhir.r4.core' && li !== 'hl7.fhir.r4.core') return 1;
  return 0;
});

process.stdout.write(out.join('\n'));
if (out.length) process.stdout.write('\n');

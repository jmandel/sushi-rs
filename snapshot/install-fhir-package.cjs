#!/usr/bin/env node
const childProcess = require('child_process');
const fs = require('fs');
const https = require('https');
const os = require('os');
const path = require('path');

function usage() {
  console.error('usage: node snapshot/install-fhir-package.cjs [--cache <packages-dir>] <pkg#ver>...');
  process.exit(2);
}

const repo = path.resolve(__dirname, '..');
let cache = process.env.FHIR_CACHE || path.join(repo, 'temp/fhir-home/.fhir/packages');
const roots = [];
const args = process.argv.slice(2);
while (args.length) {
  const arg = args.shift();
  if (arg === '--cache') {
    cache = args.shift();
    if (!cache) usage();
  } else if (arg.startsWith('-')) {
    usage();
  } else {
    roots.push(arg);
  }
}
if (roots.length === 0) usage();

cache = path.resolve(cache);
const tempRoot = path.resolve(repo, 'temp');
if (!(cache === tempRoot || cache.startsWith(tempRoot + path.sep))) {
  throw new Error(`refusing to write package cache outside repo temp/: ${cache}`);
}
fs.mkdirSync(cache, { recursive: true });

const seen = new Set();

function parseSpec(spec) {
  const hash = spec.lastIndexOf('#');
  if (hash <= 0 || hash === spec.length - 1) {
    throw new Error(`package spec must be pkg#version: ${spec}`);
  }
  return { id: spec.slice(0, hash), version: spec.slice(hash + 1), spec };
}

function packageJsonPath(spec) {
  return path.join(cache, spec, 'package', 'package.json');
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

function download(url, out, redirects = 0) {
  if (redirects > 8) {
    return Promise.reject(new Error(`too many redirects for ${url}`));
  }
  return new Promise((resolve, reject) => {
    const req = https.get(url, res => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        const next = new URL(res.headers.location, url).toString();
        download(next, out, redirects + 1).then(resolve, reject);
        return;
      }
      if (res.statusCode !== 200) {
        res.resume();
        reject(new Error(`GET ${url} returned ${res.statusCode}`));
        return;
      }
      const file = fs.createWriteStream(out);
      res.pipe(file);
      file.on('finish', () => file.close(resolve));
      file.on('error', reject);
    });
    req.on('error', reject);
  });
}

function fetchJson(url, redirects = 0) {
  if (redirects > 8) {
    return Promise.reject(new Error(`too many redirects for ${url}`));
  }
  return new Promise((resolve, reject) => {
    const req = https.get(url, res => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        const next = new URL(res.headers.location, url).toString();
        fetchJson(next, redirects + 1).then(resolve, reject);
        return;
      }
      if (res.statusCode !== 200) {
        res.resume();
        reject(new Error(`GET ${url} returned ${res.statusCode}`));
        return;
      }
      let body = '';
      res.setEncoding('utf8');
      res.on('data', chunk => body += chunk);
      res.on('end', () => {
        try {
          resolve(JSON.parse(body));
        } catch (err) {
          reject(err);
        }
      });
    });
    req.on('error', reject);
  });
}

async function resolveVersion(id, version) {
  version = canonicalVersion(id, version);
  if (!needsVersionResolution(version)) return version;
  const metadata = await fetchJson(`https://packages2.fhir.org/packages/${encodeURIComponent(id)}`);
  if ((version === 'latest' || version === 'current') && metadata['dist-tags']?.latest) {
    return metadata['dist-tags'].latest;
  }
  const matches = Object.keys(metadata.versions || {})
    .filter(candidate => versionMatches(candidate, version))
    .sort(compareVersions);
  if (matches.length === 0) {
    throw new Error(`no published version of ${id} matches ${version}`);
  }
  return matches[matches.length - 1];
}

async function install(specText) {
  let { id, version } = parseSpec(specText);
  version = await resolveVersion(id, version);
  const spec = `${id}#${version}`;
  if (seen.has(spec)) return;
  seen.add(spec);

  const pkgJson = packageJsonPath(spec);
  if (!fs.existsSync(pkgJson)) {
    const target = path.join(cache, spec);
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'fhir-package-'));
    const tgz = path.join(tmp, `${id}-${version}.tgz`);
    const url = `https://packages2.fhir.org/packages/${encodeURIComponent(id)}/${encodeURIComponent(version)}`;
    console.error(`DOWNLOAD ${spec}`);
    await download(url, tgz);
    const unpack = path.join(tmp, 'unpack');
    fs.mkdirSync(unpack);
    childProcess.execFileSync('tar', ['-xzf', tgz, '-C', unpack], { stdio: 'inherit' });
    const packageDir = path.join(unpack, 'package');
    if (!fs.existsSync(packageDir)) {
      throw new Error(`downloaded package has no package/ directory: ${spec}`);
    }
    fs.rmSync(target, { recursive: true, force: true });
    fs.mkdirSync(target, { recursive: true });
    fs.cpSync(packageDir, path.join(target, 'package'), { recursive: true });
    fs.rmSync(tmp, { recursive: true, force: true });
  } else {
    console.error(`CACHE ${spec}`);
  }

  const json = JSON.parse(fs.readFileSync(pkgJson, 'utf8'));
  for (const [depId, depVersion] of Object.entries(json.dependencies || {})) {
    await install(`${depId}#${depVersion}`);
  }
}

(async () => {
  for (const root of roots) {
    await install(root);
  }
})().catch(err => {
  console.error(`FATAL: ${err.message}`);
  process.exit(1);
});

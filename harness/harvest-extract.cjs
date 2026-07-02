#!/usr/bin/env node
// Harvest self-contained FSH snippets from stock SUSHI's own unit tests.
// Sources: sushi-ts/test/import/*.test.ts and test/run/FshToFhir.test.ts
// Strategy: find template literals passed to the FSH-wrapping helpers
// (leftAlign / fshToFhir / RawFSH / importSingleText) and the enclosing it()/describe().
const fs = require('fs');
const path = require('path');

// Reference stock SUSHI test sources (the sushi-ts submodule). Override with
// SUSHI_TS_TEST=/path/to/sushi/test if checked out elsewhere.
const TS_TEST = process.env.SUSHI_TS_TEST || '/home/jmandel/hobby/sushi-rs/sushi-ts/test';
const OUT = process.argv[2] || 'snippets.json';

// Exact copy of sushi-ts/test/utils/leftAlign.ts
function leftAlign(inputFSH) {
  const lineArray = inputFSH.split(/\r?\n/);
  let offsetAmount;
  const letterRegex = /[a-z]/i;
  for (const line of lineArray) {
    if (letterRegex.test(line)) { offsetAmount = line.search(/\S/); break; }
  }
  return lineArray.map(l => (l.length >= offsetAmount ? l.slice(offsetAmount) : l)).join('\n');
}

// Unescape a JS template-literal raw body (assumes no ${} substitutions).
function unescapeTemplate(raw) {
  let out = '';
  for (let i = 0; i < raw.length; i++) {
    const c = raw[i];
    if (c !== '\\') { out += c; continue; }
    const n = raw[i + 1];
    if (n === undefined) { out += '\\'; break; }
    switch (n) {
      case 'n': out += '\n'; i++; break;
      case 'r': out += '\r'; i++; break;
      case 't': out += '\t'; i++; break;
      case 'b': out += '\b'; i++; break;
      case 'f': out += '\f'; i++; break;
      case 'v': out += '\v'; i++; break;
      case '0': out += '\0'; i++; break;
      case '\\': out += '\\'; i++; break;
      case '`': out += '`'; i++; break;
      case '$': out += '$'; i++; break;
      case "'": out += "'"; i++; break;
      case '"': out += '"'; i++; break;
      case '\n': i++; break; // line continuation
      case 'x': { const h = raw.substr(i + 2, 2); if (/^[0-9a-fA-F]{2}$/.test(h)) { out += String.fromCharCode(parseInt(h, 16)); i += 3; } else { out += n; i++; } break; }
      case 'u': {
        if (raw[i + 2] === '{') {
          const end = raw.indexOf('}', i + 3);
          const h = raw.substring(i + 3, end);
          if (/^[0-9a-fA-F]+$/.test(h)) { out += String.fromCodePoint(parseInt(h, 16)); i = end; }
          else { out += n; i++; }
        } else {
          const h = raw.substr(i + 2, 4);
          if (/^[0-9a-fA-F]{4}$/.test(h)) { out += String.fromCharCode(parseInt(h, 16)); i += 5; }
          else { out += n; i++; }
        }
        break;
      }
      default: out += n; i++; break;
    }
  }
  return out;
}

// Tokenize-aware scan: collect template literals (with raw body, hasSubstitution flag,
// preceding-callee identifier) AND it/describe name positions.
function scanFile(src) {
  const literals = [];   // {start, raw, hasSub, callee}
  const labels = [];     // {pos, kind:'it'|'describe', name}
  let i = 0;
  const n = src.length;
  while (i < n) {
    const c = src[i];
    // line comment
    if (c === '/' && src[i + 1] === '/') { while (i < n && src[i] !== '\n') i++; continue; }
    // block comment
    if (c === '/' && src[i + 1] === '*') { i += 2; while (i < n && !(src[i] === '*' && src[i + 1] === '/')) i++; i += 2; continue; }
    // single/double quoted string
    if (c === "'" || c === '"') {
      const q = c; i++;
      while (i < n && src[i] !== q) { if (src[i] === '\\') i++; i++; }
      i++; continue;
    }
    // template literal
    if (c === '`') {
      const start = i; i++;
      let raw = ''; let hasSub = false;
      while (i < n) {
        if (src[i] === '\\') { raw += src[i] + (src[i + 1] ?? ''); i += 2; continue; }
        if (src[i] === '`') { i++; break; }
        if (src[i] === '$' && src[i + 1] === '{') {
          hasSub = true; raw += '${'; i += 2; let depth = 1;
          while (i < n && depth > 0) { if (src[i] === '{') depth++; else if (src[i] === '}') depth--; if (depth > 0) raw += src[i]; i++; }
          raw += '}'; continue;
        }
        raw += src[i]; i++;
      }
      // find preceding callee identifier: skip ws back to '(', then identifier
      let j = start - 1;
      while (j >= 0 && /\s/.test(src[j])) j--;
      let callee = null;
      if (src[j] === '(') {
        j--; while (j >= 0 && /\s/.test(src[j])) j--;
        let end = j; while (j >= 0 && /[A-Za-z0-9_$]/.test(src[j])) j--;
        callee = src.substring(j + 1, end + 1);
      }
      literals.push({ start, raw, hasSub, callee });
      continue;
    }
    // it / describe / test label
    if (/[A-Za-z]/.test(c)) {
      const m = /^(describe|it|test)(\.(skip|only|each))?\s*\(\s*(['"`])/.exec(src.substring(i, i + 60));
      if (m && (i === 0 || !/[A-Za-z0-9_$.]/.test(src[i - 1]))) {
        const kind = m[1] === 'describe' ? 'describe' : 'it';
        // read the name string
        let k = i + m[0].length - 1; const q = src[k]; k++;
        let name = '';
        while (k < n && src[k] !== q) { if (src[k] === '\\') { name += src[k + 1] ?? ''; k += 2; } else { name += src[k]; k++; } }
        labels.push({ pos: i, kind, name });
        i = k + 1; continue;
      }
    }
    i++;
  }
  return { literals, labels };
}

function nearestLabel(labels, pos, kind) {
  let best = null;
  for (const l of labels) { if (l.pos < pos && l.kind === kind) { if (!best || l.pos > best.pos) best = l; } }
  return best;
}

const FSH_CALLEES = new Set(['leftAlign', 'fshToFhir', 'RawFSH', 'importSingleText', 'importText']);
const ENTITY_RE = /^\s*(Profile|Extension|Instance|ValueSet|CodeSystem|Logical|Resource)\s*:/m;

const files = [];
for (const f of fs.readdirSync(path.join(TS_TEST, 'import'))) if (f.endsWith('.test.ts')) files.push(['import', f]);
files.push(['run', 'FshToFhir.test.ts']);

const snippets = [];
let stats = { totalLiterals: 0, fshLiterals: 0, withSub: 0, noEntity: 0, dup: 0 };
const seen = new Map();

for (const [sub, fname] of files) {
  const full = path.join(TS_TEST, sub, fname);
  const src = fs.readFileSync(full, 'utf8');
  const { literals, labels } = scanFile(src);
  for (const lit of literals) {
    stats.totalLiterals++;
    if (lit.hasSub) { stats.withSub++; continue; }
    let text = unescapeTemplate(lit.raw);
    // Faithful to stock's test: only leftAlign() calls strip indentation;
    // assignment-style templates (const input = `...`) are fed verbatim.
    if (lit.callee === 'leftAlign') text = leftAlign(text);
    // Accept any literal that declares a FHIR entity (call- or assignment-style).
    if (!ENTITY_RE.test(text)) { stats.noEntity++; continue; }
    stats.fshLiterals++;
    const itL = nearestLabel(labels, lit.start, 'it');
    const descL = nearestLabel(labels, lit.start, 'describe');
    const norm = text.replace(/\s+$/g, '');
    if (seen.has(norm)) { stats.dup++; continue; }
    seen.set(norm, true);
    snippets.push({
      sourceFile: `test/${sub}/${fname}`,
      describe: descL ? descL.name : null,
      it: itL ? itL.name : null,
      callee: lit.callee,
      fsh: text
    });
  }
}

fs.writeFileSync(OUT, JSON.stringify(snippets, null, 2));
console.error('files:', files.length);
console.error('stats:', JSON.stringify(stats));
console.error('harvested snippets (>=1 entity, deduped):', snippets.length);

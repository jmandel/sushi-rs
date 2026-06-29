#!/usr/bin/env node
/*
 * Lexer oracle: dump the exact ANTLR token stream stock SUSHI produces for a
 * .fsh file (including hidden-channel whitespace; skipped comments are absent,
 * as in SUSHI). This is the lexer-level golden the Rust lexer is tested against.
 *
 * Usage: node lex-oracle.cjs <file.fsh>      > tokens.json
 *        node lex-oracle.cjs --text 'FSH...'  > tokens.json
 *
 * Output: JSON array of { type, channel, text, line, col, start, stop } where
 *   type    = symbolic token name (e.g. "KW_PROFILE", "STAR", "SEQUENCE", "EOF")
 *   channel = 0 (default) or "HIDDEN"
 *   line    = 1-based (ANTLR token.line)
 *   col     = 0-based UTF-16 column (ANTLR token.column)
 *   start/stop = 0-based inclusive char (UTF-16) offsets
 */
'use strict';
const fs = require('fs');
const SUSHI_ROOT =
  process.env.SUSHI_ROOT || '/home/jmandel/periodicity/node_modules/fsh-sushi';
const path = require('path');
// antlr4 may be hoisted above fsh-sushi; resolve from several candidate roots.
const antlr4 = require(
  require.resolve('antlr4', { paths: [SUSHI_ROOT, path.join(SUSHI_ROOT, '..'), path.join(SUSHI_ROOT, '../..')] })
);
const FSHLexer = require(path.join(SUSHI_ROOT, 'dist/import/generated/FSHLexer')).default;

const HIDDEN = antlr4.Token.HIDDEN_CHANNEL;

function tokenName(type) {
  if (type === antlr4.Token.EOF || type === -1) return 'EOF';
  const n = FSHLexer.symbolicNames[type];
  return n || `T${type}`;
}

function main() {
  const args = process.argv.slice(2);
  let input;
  if (args[0] === '--text') input = args[1];
  else if (args[0]) input = fs.readFileSync(args[0], 'utf8');
  else {
    console.error('usage: lex-oracle.cjs <file.fsh> | --text <fsh>');
    process.exit(2);
  }
  // Match the importer: append a newline if the content does not end in one
  // (FSHImporter.import appends \n so a trailing line comment tokenizes).
  if (!input.endsWith('\n')) input = input + '\n';

  const chars = new antlr4.InputStream(input);
  const lexer = new FSHLexer(chars);
  // Silence lexer error output to stdout; collect via default listener removal.
  lexer.removeErrorListeners();
  const ts = new antlr4.CommonTokenStream(lexer);
  ts.fill();
  const out = ts.tokens.map((t) => ({
    type: tokenName(t.type),
    channel: t.channel === HIDDEN ? 'HIDDEN' : 0,
    text: t.text,
    line: t.line,
    col: t.column,
    start: t.start,
    stop: t.stop,
  }));
  process.stdout.write(JSON.stringify(out, null, 2) + '\n');
}

main();

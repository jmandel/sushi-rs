#!/usr/bin/env bash
# Regenerate lexer goldens from the ANTLR oracle for every fixture.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$HERE" rev-parse --show-toplevel)"
F="$REPO/crates/fsh_lexer_parser/tests/fixtures/lex"
G="$REPO/crates/fsh_lexer_parser/tests/goldens/lex"
mkdir -p "$G"
for f in "$F"/*.fsh; do
  base="$(basename "$f" .fsh)"
  node "$HERE/lex-oracle.cjs" "$f" > "$G/$base.tokens.json"
  echo "  $base.tokens.json"
done
echo "[gen-lex-goldens] done"

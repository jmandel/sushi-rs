#!/usr/bin/env bash
# Regenerate AST goldens from the import oracle for every lex fixture.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$HERE" rev-parse --show-toplevel)"
F="$REPO/crates/fsh_lexer_parser/tests/fixtures/lex"
G="$REPO/crates/fsh_lexer_parser/tests/goldens/ast"
mkdir -p "$G"
for f in "$F"/*.fsh; do
  base="$(basename "$f" .fsh)"
  node "$HERE/parse-oracle.cjs" "$f" > "$G/$base.ast.json"
  echo "  $base.ast.json"
done
echo "[gen-ast-goldens] done"

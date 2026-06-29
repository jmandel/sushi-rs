#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$HERE" rev-parse --show-toplevel)"
F="$REPO/crates/compiler/tests/fixtures/expand"
G="$REPO/crates/compiler/tests/goldens/expand"
mkdir -p "$G"
for f in "$F"/*.fsh; do
  base="$(basename "$f" .fsh)"
  node "$HERE/expand-oracle.cjs" "$f" > "$G/$base.expand.json"
  echo "  $base.expand.json"
done
echo "[gen-expand-goldens] done"

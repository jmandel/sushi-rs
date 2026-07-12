#!/usr/bin/env bash
# examples-gate — execute the self-contained transport examples against the
# real fig binary. The package-backed Publisher lifecycle is certified by the
# integration gate in the editor's Pages workflow; placeholder paths in prose
# are deliberately not misreported as executable examples here.
#
# Env:
#   FIG_BIN        fig binary (default: target/release/fig; built if absent)
set -uo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
FIG_BIN="${FIG_BIN:-$REPO/target/release/fig}"
# Work under target/ (a full render writes thousands of files; keep it off a
# small/quota-capped /tmp).
WORK="$REPO/target/examples-gate.$$"
mkdir -p "$WORK"
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0; skip=0
ok()   { echo "  PASS $1"; pass=$((pass+1)); }
bad()  { echo "  FAIL $1"; fail=$((fail+1)); }
skp()  { echo "  SKIP $1 ($2)"; skip=$((skip+1)); }

if [ ! -x "$FIG_BIN" ]; then
  echo "[examples-gate] building fig --release"
  ( cd "$REPO" && cargo build --release -p fig >/dev/null ) || { echo "FATAL: fig build failed"; exit 2; }
fi
HAVE_PY=0; command -v python3 >/dev/null 2>&1 && HAVE_PY=1

echo "== example: envelope schema =="
if [ "$HAVE_PY" = 1 ]; then
  if python3 "$REPO/examples/envelope/check.py" "$FIG_BIN"; then ok envelope; else bad envelope; fi
else skp envelope "no python3"; fi

echo "== example: shell-to-fig (non-JS host) =="
if [ "$HAVE_PY" = 1 ]; then
  if python3 "$REPO/examples/shell-to-fig/render.py" "$FIG_BIN"; then ok shell-to-fig; else bad shell-to-fig; fi
else skp shell-to-fig "no python3"; fi

echo
echo "=== examples-gate: $pass pass, $fail fail, $skip skip ==="
[ "$fail" -eq 0 ]

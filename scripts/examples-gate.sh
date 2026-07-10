#!/usr/bin/env bash
# examples-gate — execute every docs example against the real fig binary so the
# docs in docs/hosting.md and README.md cannot rot. Each example
# lives in examples/ and is run here; a failure fails CI.
#
# Env:
#   FIG_BIN        fig binary (default: target/release/fig; built if absent)
#   F0_DIR         F0 build root (default: ../sushi-rs-snapshot-f0-builds); the
#                  fig-render / fragment examples SKIP with a note if absent
set -uo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
FIG_BIN="${FIG_BIN:-$REPO/target/release/fig}"
F0_DIR="${F0_DIR:-$REPO/../sushi-rs-snapshot-f0-builds}"
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
  # Render one fragment if an F0 build is present; else just the version + error path.
  BUILD="$F0_DIR/us-core"
  if [ -d "$BUILD/temp/pages" ]; then
    if python3 "$REPO/examples/shell-to-fig/render.py" "$FIG_BIN" "$BUILD" \
        StructureDefinition-us-core-patient snapshot; then ok shell-to-fig; else bad shell-to-fig; fi
  else
    if python3 "$REPO/examples/shell-to-fig/render.py" "$FIG_BIN"; then ok shell-to-fig-lite; else bad shell-to-fig; fi
  fi
else skp shell-to-fig "no python3"; fi

echo "== example: cli quickstart (fig render) =="
BUILD="$F0_DIR/plan-net"
if [ -d "$BUILD/temp/pages" ]; then
  OUT="$WORK/site"
  if "$FIG_BIN" render "$BUILD" -o "$OUT" --active-tables >/dev/null 2>&1 \
     && [ -f "$OUT/en/index.html" ]; then
    # spot byte-check vs the golden (the page-corpus oracle).
    if cmp -s "$OUT/en/index.html" "$BUILD/output/en/index.html"; then ok cli-quickstart; else bad "cli-quickstart (byte mismatch)"; fi
  else bad cli-quickstart; fi
else skp cli-quickstart "no F0 plan-net build at $BUILD"; fi

echo "== example: template-as-data (fig render, zero-code) =="
# Same render path over a DIFFERENT build tree — one engine, template as data.
BUILD="$F0_DIR/us-core"
if [ -d "$BUILD/temp/pages" ]; then
  OUT="$WORK/uscore"
  if "$FIG_BIN" render "$BUILD" -o "$OUT" >/dev/null 2>&1 && [ -f "$OUT/index.html" ]; then
    ok template-as-data; else bad template-as-data; fi
else skp template-as-data "no F0 us-core build"; fi

echo
echo "=== examples-gate: $pass pass, $fail fail, $skip skip ==="
[ "$fail" -eq 0 ]

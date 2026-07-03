#!/usr/bin/env bash
# WASM P0 demo — data preparation script.
#
# Assembles everything the static browser page needs into demo/wasm-p0/data/
# (which is .gitignored — nothing heavy is committed):
#   data/rust_sushi.wasm       the FSH compiler, built for wasm32-wasip1
#   data/snapshot_gen.wasm     the snapshot walk engine, built for wasm32-wasip1
#   data/vfs.json              a manifest describing the virtual filesystem:
#                              the cycle IG sources + a MINIMAL FHIR package cache
#                              subset (only the resource files the build/snapshot
#                              actually read, plus each package's index metadata).
#   data/vfs/**                the raw files referenced by vfs.json.
#
# The minimal package subset is derived by tracing which files native
# rust_sushi/snapshot_gen open during a real cycle build (see README.md §"How the
# minimal cache is chosen"). It is a strict subset of temp/fhir-home; the full
# cache is never shipped.
#
# Requirements: a wasm32-wasip1 toolchain (see README.md), and a populated
# package cache at temp/fhir-home/.fhir/packages (already present in this repo).
#
# Usage:  demo/wasm-p0/prepare.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
cd "$REPO"

CYCLE="${CYCLE_DIR:-/home/jmandel/hobby/periodicity-impl/cycle}"
CACHE="${FHIR_CACHE:-temp/fhir-home/.fhir/packages}"
DATA="$HERE/data"
VFS="$DATA/vfs"

# The exact package closure the cycle IG needs (verified by strace):
#   build:    r4.core, uv.tools.r4, terminology.r4, uv.extensions.r4
#   snapshot: r5.core  (R4->R5 base resolution)
PKGS=(
  "hl7.fhir.r4.core#4.0.1"
  "hl7.fhir.uv.tools.r4#1.1.2"
  "hl7.terminology.r4#7.2.0"
  "hl7.fhir.uv.extensions.r4#5.3.0"
  "hl7.fhir.r5.core#5.0.0"
)

# ---- 0. sanity ----
[ -d "$CYCLE" ]  || { echo "cycle IG not found at $CYCLE (set CYCLE_DIR)"; exit 2; }
[ -d "$CACHE" ]  || { echo "FHIR package cache not found at $CACHE (set FHIR_CACHE)"; exit 2; }

# ---- 1. build the two wasm binaries (wasm32-wasip1) ----
# The caller must have a wasm32-wasip1-capable toolchain on PATH. We do NOT pin
# it here so the script works with either rustup (rustup target add wasm32-wasip1)
# or a scratch toolchain. See README.md.
echo "[1/4] building wasm32-wasip1 binaries ..."
cargo build --release --target wasm32-wasip1 --bin rust_sushi --bin snapshot_gen
WASM_OUT="target/wasm32-wasip1/release"

rm -rf "$DATA"
mkdir -p "$DATA" "$VFS"
cp "$WASM_OUT/rust_sushi.wasm"   "$DATA/rust_sushi.wasm"
cp "$WASM_OUT/snapshot_gen.wasm" "$DATA/snapshot_gen.wasm"

# ---- 2. cycle IG sources (full input tree so IG-export page/example scan matches native) ----
echo "[2/4] copying cycle IG sources ..."
mkdir -p "$VFS/cycle"
cp "$CYCLE/sushi-config.yaml" "$VFS/cycle/"
mkdir -p "$VFS/cycle/input"
[ -f "$CYCLE/input/ignoreWarnings.txt" ] && cp "$CYCLE/input/ignoreWarnings.txt" "$VFS/cycle/input/"
for d in fsh pagecontent resources; do
  [ -d "$CYCLE/input/$d" ] && cp -r "$CYCLE/input/$d" "$VFS/cycle/input/"
done

# ---- 3. minimal package cache subset ----
# For each needed package: always ship .index.json + .derived-index.json +
# package.json (metadata the store parses eagerly), then ship the union of
# resource files that native rust_sushi + snapshot_gen actually open. The read
# set is deterministic (driven by name/url fishing, identical code + inputs).
echo "[3/4] tracing minimal package file set (native build+snapshot under strace) ..."
NRES="$DATA/.native-out/fsh-generated/resources"
STRACE_BUILD="$DATA/.strace-build.log"
STRACE_SNAP="$DATA/.strace-snap.log"

BUILD_BIN="target/release/rust_sushi"
SNAP_BIN="target/release/snapshot_gen"
[ -x "$BUILD_BIN" ] || cargo build --release --bin rust_sushi
[ -x "$SNAP_BIN" ]  || cargo build --release --bin snapshot_gen

if ! command -v strace >/dev/null; then
  echo "  strace not available — falling back to shipping full package dirs (large)."
  for p in "${PKGS[@]}"; do
    mkdir -p "$VFS/packages/$p"
    cp -r "$CACHE/$p/package" "$VFS/packages/$p/"
  done
else
  mkdir -p "$DATA/.native-out"
  strace -f -e trace=openat -qq "$BUILD_BIN" build "$CYCLE" -o "$DATA/.native-out" --cache "$CACHE" \
    2>"$STRACE_BUILD" >/dev/null
  : > "$STRACE_SNAP"
  for f in "$NRES"/StructureDefinition-*.json; do
    strace -f -e trace=openat -qq "$SNAP_BIN" --cache "$CACHE" \
      --package hl7.fhir.r5.core#5.0.0 --local-dir "$NRES" "$f" 2>>"$STRACE_SNAP" >/dev/null
  done

  # Ship eager metadata for each package.
  for p in "${PKGS[@]}"; do
    mkdir -p "$VFS/packages/$p/package"
    for meta in .index.json .derived-index.json package.json; do
      [ -f "$CACHE/$p/package/$meta" ] && cp "$CACHE/$p/package/$meta" "$VFS/packages/$p/package/"
    done
  done
  # Ship the union of resource files actually opened.
  grep -hoE "$CACHE/[^\"]+\.json" "$STRACE_BUILD" "$STRACE_SNAP" 2>/dev/null \
    | grep -vE "ENOENT" | sort -u | while read -r src; do
      rel="${src#$CACHE/}"
      [ -f "$src" ] || continue
      mkdir -p "$VFS/packages/$(dirname "$rel")"
      cp "$src" "$VFS/packages/$rel"
    done
fi
rm -rf "$DATA/.native-out" "$STRACE_BUILD" "$STRACE_SNAP"

# ---- 4. emit vfs.json manifest (relative paths the page fetch()es) ----
echo "[4/4] writing vfs.json manifest ..."
( cd "$VFS" && find . -type f | sed 's|^\./||' | sort ) > "$DATA/.filelist.txt"
python3 - "$DATA/.filelist.txt" > "$DATA/vfs.json" <<'PY'
import json, sys
files = [l.rstrip("\n") for l in open(sys.argv[1]) if l.strip()]
print(json.dumps({"root": "vfs", "files": files}, indent=0))
PY
rm -f "$DATA/.filelist.txt"

echo ""
echo "done. data/ assembled:"
du -sh "$DATA/vfs" "$DATA"/*.wasm | sed 's|.*/data/|  data/|'
echo "  files in vfs: $(python3 -c "import json;print(len(json.load(open('$DATA/vfs.json'))['files']))")"
echo ""
echo "serve with:  (cd $HERE && python3 -m http.server 8000)"
echo "then open :  http://localhost:8000/"

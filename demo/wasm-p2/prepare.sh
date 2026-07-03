#!/usr/bin/env bash
# WASM P2 demo — data preparation.
#
# Assembles everything the static page + Web Worker need into demo/wasm-p2/data/
# (gitignored — nothing heavy committed):
#   data/pkg/wasm_api.js, wasm_api_bg.wasm   the wasm-bindgen (web target) module
#   data/bundles/<label>.tgz + bundle-manifest.json   the cycle IG package closure
#   data/cycle/**                             the cycle IG sources (config + FSH +
#                                             input/resources)
#   data/manifest.json                        what the worker fetches: FSH file
#                                             list, predefined list, bundle labels
#
# This is the PRODUCTION shape (P2): wasm32-unknown-unknown + wasm-bindgen +
# BundleSource — no WASI shim, no vendored FS. The worker calls the typed
# init/compile/generate_snapshot surface.
#
# Requires: a wasm32-unknown-unknown toolchain (see demo/wasm-p0/README.md for the
# scratch-toolchain recipe; point WASM_RUSTUP_HOME/WASM_CARGO_HOME at it) and
# wasm-bindgen (set WASM_BINDGEN or have it on PATH). Plus a populated cache at
# temp/fhir-home/.fhir/packages and the cycle IG (default: the P0 demo copy).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
cd "$REPO"

CACHE="${FHIR_CACHE:-$REPO/temp/fhir-home/.fhir/packages}"
CYCLE="${CYCLE_DIR:-$REPO/demo/wasm-p0/data/vfs/cycle}"
DATA="$HERE/data"

[ -d "$CACHE" ] || { echo "FATAL: cache not found at $CACHE"; exit 2; }
[ -d "$CYCLE" ] || { echo "FATAL: cycle IG not found at $CYCLE (set CYCLE_DIR; or run demo/wasm-p0/prepare.sh first)"; exit 2; }

if [ -n "${WASM_RUSTUP_HOME:-}" ]; then export RUSTUP_HOME="$WASM_RUSTUP_HOME"; fi
if [ -n "${WASM_CARGO_HOME:-}" ]; then export CARGO_HOME="$WASM_CARGO_HOME"; export PATH="$CARGO_HOME/bin:$PATH"; fi
WASM_BINDGEN="${WASM_BINDGEN:-wasm-bindgen}"
command -v "$WASM_BINDGEN" >/dev/null || { echo "FATAL: wasm-bindgen not found (set WASM_BINDGEN)"; exit 2; }
COMMIT="$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo unknown)"

rm -rf "$DATA"
mkdir -p "$DATA/pkg" "$DATA/bundles" "$DATA/cycle"

echo "[1/4] build wasm_api (wasm32-unknown-unknown, release) + wasm-bindgen (web) ..."
WASM_API_GIT_COMMIT="$COMMIT" cargo build -p wasm_api --target wasm32-unknown-unknown --release
"$WASM_BINDGEN" --target web --out-dir "$DATA/pkg" --out-name wasm_api \
  "$REPO/target/wasm32-unknown-unknown/release/wasm_api.wasm"
if command -v wasm-opt >/dev/null; then
  wasm-opt -Oz "$DATA/pkg/wasm_api_bg.wasm" -o "$DATA/pkg/wasm_api_bg.wasm.opt" \
    && mv "$DATA/pkg/wasm_api_bg.wasm.opt" "$DATA/pkg/wasm_api_bg.wasm"
fi

echo "[2/4] build cycle package bundles ..."
# The cycle IG's closure (build + snapshot). r5.core is needed for R4->R5 base
# resolution during snapshot generation.
LABELS=(
  "hl7.fhir.r4.core#4.0.1"
  "hl7.fhir.uv.tools.r4#1.1.2"
  "hl7.terminology.r4#7.2.0"
  "hl7.fhir.uv.extensions.r4#5.3.0"
  "hl7.fhir.r5.core#5.0.0"
)
cargo build --release -p rust_sushi >/dev/null 2>&1 || cargo build --release -p rust_sushi
"$REPO/target/release/rust_sushi" bundle --cache "$CACHE" --out "$DATA/bundles" "${LABELS[@]}" >/dev/null

echo "[3/4] copy cycle IG sources ..."
cp "$CYCLE/sushi-config.yaml" "$DATA/cycle/"
mkdir -p "$DATA/cycle/input/fsh" "$DATA/cycle/input/resources"
cp -r "$CYCLE/input/fsh/." "$DATA/cycle/input/fsh/" 2>/dev/null || true
[ -d "$CYCLE/input/resources" ] && cp -r "$CYCLE/input/resources/." "$DATA/cycle/input/resources/" 2>/dev/null || true

echo "[4/4] emit manifest.json (FSH files, predefined, bundle labels) ..."
python3 - "$DATA" "${LABELS[@]}" > "$DATA/manifest.json" <<'PY'
import json, os, sys
data = sys.argv[1]
labels = sys.argv[2:]
def rel_sorted(root, sub, exts):
    base = os.path.join(data, "cycle", sub)
    out = []
    for dirpath, _dirs, files in os.walk(base):
        for f in files:
            if os.path.splitext(f)[1].lstrip(".").lower() in exts:
                full = os.path.join(dirpath, f)
                out.append(os.path.relpath(full, os.path.join(data, "cycle")))
    return sorted(out)
fsh = rel_sorted(data, "input/fsh", {"fsh"})
predefined = rel_sorted(data, "input/resources", {"json"})
print(json.dumps({
    "config": "cycle/sushi-config.yaml",
    "fsh": ["cycle/" + p for p in fsh],
    "predefined": ["cycle/" + p for p in predefined],
    "bundles": [{"label": l, "tgz": "bundles/%s.tgz" % l} for l in labels],
}, indent=2))
PY

echo ""
echo "done. data/ assembled:"
du -sh "$DATA/pkg" "$DATA/bundles" "$DATA/cycle" 2>/dev/null | sed "s|$DATA/|  data/|"
echo "  wasm: $(du -h "$DATA/pkg/wasm_api_bg.wasm" | cut -f1)"
echo ""
echo "serve:  (cd $HERE && python3 -m http.server 8020)   then open http://localhost:8020/"

#!/usr/bin/env bash
# WASM parity harness: build the wasm_api module
# (wasm32-unknown-unknown + wasm-bindgen), then run — under Node, no browser —
# the 17-rung fixture ladder + the ips/mcode/sdc corpus gates AGAINST THE WASM
# BUILD, comparing every generated snapshot to the SAME goldens the native gates
# use. Byte parity native<->wasm proven, not assumed.
#
#   bash scripts/wasm-parity.sh            # build + run + verdict
#
# Requires:
#   - a wasm32-unknown-unknown toolchain (upstream rustup matching the repo rustc;
#     see demo/wasm-p0/README.md for the scratch-toolchain recipe). Point at it
#     with WASM_RUSTUP_HOME / WASM_CARGO_HOME if it is not the default rustup.
#   - wasm-bindgen (0.2.x matching the crate). Set WASM_BINDGEN to its path, else
#     it must be on PATH.
#   - node (for the driver) and a populated package cache at
#     temp/fhir-home/.fhir/packages.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
cd "$REPO"

CACHE="${FHIR_CACHE:-$REPO/temp/fhir-home/.fhir/packages}"
[ -d "$CACHE" ] || { echo "FATAL: FHIR package cache not found at $CACHE (set FHIR_CACHE)"; exit 2; }

# Isolated, gitignored work dir (nothing heavy in git).
WORK="${WASM_PARITY_WORK:-$REPO/target/wasm-parity}"
WASM_OUT="$WORK/pkg"
BUNDLE_DIR="$WORK/bundles"
mkdir -p "$WASM_OUT" "$BUNDLE_DIR"

# ---- toolchain --------------------------------------------------------------
# Honor an explicit scratch rustup/cargo home for the wasm target.
if [ -n "${WASM_RUSTUP_HOME:-}" ]; then export RUSTUP_HOME="$WASM_RUSTUP_HOME"; fi
if [ -n "${WASM_CARGO_HOME:-}" ]; then
  export CARGO_HOME="$WASM_CARGO_HOME"
  export PATH="$CARGO_HOME/bin:$PATH"
fi
WASM_BINDGEN="${WASM_BINDGEN:-wasm-bindgen}"
command -v "$WASM_BINDGEN" >/dev/null || { echo "FATAL: wasm-bindgen not found (set WASM_BINDGEN)"; exit 2; }

COMMIT="$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo unknown)"

echo "[1/4] cargo build -p wasm_api --target wasm32-unknown-unknown --release ..."
WASM_API_GIT_COMMIT="$COMMIT" \
  cargo build -p wasm_api --target wasm32-unknown-unknown --release
RAW_WASM="$REPO/target/wasm32-unknown-unknown/release/wasm_api.wasm"
[ -f "$RAW_WASM" ] || { echo "FATAL: $RAW_WASM not built"; exit 2; }

echo "[2/4] wasm-bindgen (nodejs target) ..."
"$WASM_BINDGEN" --target nodejs --out-dir "$WASM_OUT" --out-name wasm_api "$RAW_WASM"
BG_WASM="$WASM_OUT/wasm_api_bg.wasm"
echo "      raw:        $(du -h "$RAW_WASM" | cut -f1)"
echo "      bindgen wasm: $(du -h "$BG_WASM" | cut -f1)"
# Optional wasm-opt pass (size only; behavior-neutral). Skipped if absent.
if command -v wasm-opt >/dev/null; then
  wasm-opt -Oz "$BG_WASM" -o "$BG_WASM.opt" && mv "$BG_WASM.opt" "$BG_WASM"
  echo "      wasm-opt -Oz: $(du -h "$BG_WASM" | cut -f1)"
fi

echo "[3/4] building package bundles for ladder + ips/mcode/sdc ..."
# The union of every package the ladder + the three corpus gates need. `rust_sushi
# bundle` emits one <label>.tgz per package + a bundle-manifest.json.
LABELS=(
  "hl7.fhir.r4.core#4.0.1"
  "hl7.fhir.r5.core#5.0.0"
  "hl7.fhir.uv.ipa#1.1.0"
  "hl7.fhir.uv.extensions.r4#5.3.0"
  "hl7.fhir.us.core#6.1.0"
  "hl7.fhir.uv.genomics-reporting#2.0.0"
  "hl7.fhir.uv.xver-r5.r4#0.1.0"
  "hl7.fhir.r4.examples#4.0.1"
)
# Build the native rust_sushi CLI (host target) for the bundle builder.
cargo build --release -p rust_sushi >/dev/null 2>&1 || cargo build --release -p rust_sushi
"$REPO/target/release/rust_sushi" bundle --cache "$CACHE" --out "$BUNDLE_DIR" "${LABELS[@]}" >/dev/null
echo "      bundles: $(ls "$BUNDLE_DIR"/*.tgz | wc -l) packages, $(du -sh "$BUNDLE_DIR" | cut -f1) total"

echo "[4/4] running ladder + corpus gates under Node against the wasm build ..."
echo ""
node "$HERE/wasm-parity-driver.mjs" "$REPO" "$WASM_OUT" "$BUNDLE_DIR"

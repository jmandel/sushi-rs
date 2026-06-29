#!/usr/bin/env bash
# Phase 0 oracle: run stock TypeScript SUSHI on an IG and capture
#   - generated resources (byte oracle)
#   - console log (diagnostic oracle)
#   - wall timing (perf baseline)
#
# SAFETY: always runs under an ISOLATED FHIR home (temp/fhir-home by default),
# never the user's real ~/.fhir. The isolated cache is seeded by HARDLINKING
# from the real cache (instant, no extra disk, no writes to source). Pre/post
# guards fail loud if anything would touch real ~/.fhir.
#
# Usage: run-stock.sh <ig-project-dir> <out-dir>
#   FHIR_HOME=<dir>  override isolated home (must be under repo temp/)
#   NO_SEED=1        do not hardlink-seed from real cache (cold start)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/_guard.sh"
REPO="$(guard_repo_root)"

IG_DIR="${1:?usage: run-stock.sh <ig-project-dir> <out-dir>}"
OUT_DIR="${2:?usage: run-stock.sh <ig-project-dir> <out-dir>}"

SUSHI_APP="${SUSHI_APP:-/home/jmandel/periodicity/node_modules/fsh-sushi/dist/app.js}"

# --- isolate the FHIR home (capture real home BEFORE override) ---
REAL_HOME="$HOME"
FHIR_HOME="${FHIR_HOME:-$REPO/temp/fhir-home}"
assert_isolated_fhir_home "$FHIR_HOME" "$REAL_HOME"
mkdir -p "$FHIR_HOME/.fhir/packages"

# --- seed isolated cache by hardlinking from the real cache (read-only src) ---
REAL_CACHE="$REAL_HOME/.fhir/packages"
ISO_CACHE="$FHIR_HOME/.fhir/packages"
if [[ -z "${NO_SEED:-}" && -d "$REAL_CACHE" ]]; then
  # cp -al: hardlinks, instant, zero extra disk, does NOT modify source files.
  # Only links packages not already present in the isolated cache.
  shopt -s nullglob
  for pkg in "$REAL_CACHE"/*; do
    base="$(basename "$pkg")"
    [[ -e "$ISO_CACHE/$base" ]] && continue
    cp -al "$pkg" "$ISO_CACHE/$base" 2>/dev/null || cp -a "$pkg" "$ISO_CACHE/$base"
  done
  shopt -u nullglob
fi

export HOME="$FHIR_HOME"

mkdir -p "$OUT_DIR"
LOG="$OUT_DIR/sushi-console.log"
TIMING="$OUT_DIR/timing.json"

echo "[run-stock] ig=$IG_DIR out=$OUT_DIR"
echo "[run-stock] FHIR home (isolated)=$FHIR_HOME"

STAMP_EPOCH="$(date +%s)"
START="$(date +%s.%N)"
set +e
node "$SUSHI_APP" build "$IG_DIR" -o "$OUT_DIR" > "$LOG" 2>&1
RC=$?
set -e
END="$(date +%s.%N)"
WALL="$(awk "BEGIN{printf \"%.3f\", $END-$START}")"

# --- post-guard: assert real ~/.fhir untouched ---
assert_real_fhir_untouched "$REAL_HOME" "$STAMP_EPOCH"

RES_COUNT="$(find "$OUT_DIR/fsh-generated/resources" -name '*.json' 2>/dev/null | wc -l | tr -d ' ')"
cat > "$TIMING" <<JSON
{
  "ig": "$IG_DIR",
  "out": "$OUT_DIR",
  "fhir_home": "$FHIR_HOME",
  "exit_code": $RC,
  "wall_seconds": $WALL,
  "resource_count": $RES_COUNT
}
JSON
echo "[run-stock] exit=$RC wall=${WALL}s resources=$RES_COUNT (real ~/.fhir untouched ✓)"
exit 0

#!/usr/bin/env bash
# Phase 0 oracle: run stock TypeScript SUSHI on an IG and capture
#   - generated resources (byte oracle)
#   - console log (diagnostic oracle)
#   - wall/user/sys timing (perf baseline)
#
# Usage: run-stock.sh <ig-project-dir> <out-dir>
set -euo pipefail

IG_DIR="${1:?usage: run-stock.sh <ig-project-dir> <out-dir>}"
OUT_DIR="${2:?usage: run-stock.sh <ig-project-dir> <out-dir>}"

# Stock SUSHI (matches submodule sushi-ts@v3.20.0).
SUSHI_APP="${SUSHI_APP:-/home/jmandel/periodicity/node_modules/fsh-sushi/dist/app.js}"

# Cache isolation. By default we inherit the real ~/.fhir (the plan's primary
# "warm, already-indexed" benchmark scenario). Set RUN_HOME=<dir> to give the
# run its own HOME (and thus its own <RUN_HOME>/.fhir/packages cache) for
# cold-cache / reproducibility testing. fhir-package-loader keys its cache off
# os.homedir(), which honors $HOME on Linux.
if [[ -n "${RUN_HOME:-}" ]]; then
  mkdir -p "$RUN_HOME/.fhir/packages"
  export HOME="$RUN_HOME"
  echo "[run-stock] isolated HOME=$RUN_HOME (cache=$RUN_HOME/.fhir/packages)"
fi

mkdir -p "$OUT_DIR"
LOG="$OUT_DIR/sushi-console.log"
TIMING="$OUT_DIR/timing.json"

echo "[run-stock] ig=$IG_DIR out=$OUT_DIR"
START=$(date +%s.%N)
set +e
node "$SUSHI_APP" build "$IG_DIR" -o "$OUT_DIR" \
    > "$LOG" 2>&1
RC=$?
set -e
END=$(date +%s.%N)
WALL=$(awk "BEGIN{printf \"%.3f\", $END-$START}")

RES_COUNT=$(find "$OUT_DIR/fsh-generated/resources" -name '*.json' 2>/dev/null | wc -l | tr -d ' ')
cat > "$TIMING" <<JSON
{
  "ig": "$IG_DIR",
  "out": "$OUT_DIR",
  "exit_code": $RC,
  "wall_seconds": $WALL,
  "resource_count": $RES_COUNT
}
JSON
echo "[run-stock] exit=$RC wall=${WALL}s resources=$RES_COUNT -> $OUT_DIR"
exit 0

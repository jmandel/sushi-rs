#!/usr/bin/env bash
# Regenerate the stock-SUSHI-CLI oracle (expected/*.json) for the harvest corpus.
# Oracle = the EXACT bytes stock `sushi build` writes to fsh-generated/resources/.
# This is the parity target for our rust port (which is itself a CLI), so the oracle
# is produced by the real stock CLI with the same minimal sushi-config.yaml the port reads.
#
# Run on demand only (the committed expected/ is the cached oracle). Slow (~minutes).
# SAFETY: runs under an ISOLATED HOME; asserts the real ~/.fhir is untouched.
#
# Usage: [JOBS=N] [ISO_HOME=<dir>] [SUSHI_APP=<app.js>] harness/harvest-oracle.sh [corpus-dir]
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$HERE" rev-parse --show-toplevel)"
CORPUS="${1:-$REPO/tests/sushi-harvest}"
APP="${SUSHI_APP:-/home/jmandel/periodicity/node_modules/fsh-sushi/dist/app.js}"
ISO_HOME="${ISO_HOME:-$REPO/temp/harvest-home}"
JOBS="${JOBS:-10}"
[ -f "$APP" ] || { echo "stock SUSHI app.js not found at $APP"; exit 2; }
[ -d "$CORPUS" ] || { echo "no corpus at $CORPUS"; exit 2; }

REAL_HOME="$HOME"
_ih="$(readlink -f "$ISO_HOME")"; _rh="$(readlink -f "$REAL_HOME")"
[ -n "$_ih" ] && [ "$_ih" != "$_rh" ] || { echo "FATAL: ISO_HOME is the real HOME"; exit 99; }
mkdir -p "$ISO_HOME/.fhir/packages"
STAMP=$(date +%s)

gen_one() {
  local case="$1" APP="$2" ISO_HOME="$3"
  local tmp; tmp="$(mktemp -d)"
  HOME="$ISO_HOME" node "$APP" build "$case" -o "$tmp" >"$tmp/console.log" 2>&1
  rm -rf "$case/expected"; mkdir -p "$case/expected"
  [ -d "$tmp/fsh-generated/resources" ] && cp "$tmp/fsh-generated/resources/"*.json "$case/expected/" 2>/dev/null
  local n; n="$(ls "$case/expected/"*.json 2>/dev/null | wc -l | tr -d ' ')"
  local box errs warns
  box="$(grep -oE '[0-9]+ Error[s]?[[:space:]]+[0-9]+ Warning' "$tmp/console.log" | tail -1)"
  errs="$(echo "$box" | grep -oE '^[0-9]+')"; warns="$(echo "$box" | grep -oE '[0-9]+ Warning' | grep -oE '^[0-9]+')"
  printf '{"resourceCount": %s, "errors": %s, "warnings": %s}\n' "${n:-0}" "${errs:-0}" "${warns:-0}" > "$case/oracle-meta.json"
  rm -rf "$tmp"
  echo "  $(basename "$case")  res=$n err=${errs:-0}"
}
export -f gen_one

find "$CORPUS" -mindepth 1 -maxdepth 1 -type d | sort | \
  xargs -I{} -P "$JOBS" bash -c 'gen_one "$@"' _ {} "$APP" "$ISO_HOME"

touched="$(find "$REAL_HOME/.fhir" -type f -newermt "@$STAMP" 2>/dev/null | head -3)"
[ -n "$touched" ] && { echo "FATAL: real ~/.fhir modified: $touched"; exit 98; }
echo "oracle regenerated under $CORPUS (real ~/.fhir untouched)"

#!/usr/bin/env bash
# Permanent regression gate for the SUSHI-harvest corpus (tests/sushi-harvest/).
# For each case: build the minimal IG with our rust_sushi, then byte-diff every
# resource against the stock-SUSHI-CLI oracle in <case>/expected/.
#
# Oracle = exact bytes stock `sushi build` emits (regenerate with:
#   ISO_HOME=<isolated-home> harness/harvest-oracle.sh tests/sushi-harvest ).
# This script NEVER regenerates the oracle and NEVER touches the real ~/.fhir.
#
# Usage:  [OUT=/tmp/hg] [FHIR_CACHE=...] [JOBS=N] harness/harvest-gate.sh [case-slug ...]
#         no args => all cases.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$HERE" rev-parse --show-toplevel)"; cd "$REPO"
source "$HERE/_guard.sh"

BIN="$REPO/target/release/rust_sushi"
[ -x "$BIN" ] || { echo "build rust_sushi --release first (cargo build --release)"; exit 2; }
CORPUS="$REPO/tests/sushi-harvest"
[ -d "$CORPUS" ] || { echo "no corpus at $CORPUS"; exit 2; }

# Isolated FHIR cache (read-only). Default to the shared harvest home under temp/.
# (temp/ is a symlink into the main repo, so resolve both sides before the whitelist.)
FHIR_HOME="${FHIR_HOME:-$REPO/temp/harvest-home}"
REAL_HOME="$HOME"
_fh="$(readlink -f "$FHIR_HOME")"; _rh="$(readlink -f "$REAL_HOME")"; _tmp="$(readlink -f "$REPO/temp")"
[ -n "$_fh" ] && [ "$_fh" != "$_rh" ] && [ "$_fh" != "$_rh/.fhir" ] || { echo "FATAL: FHIR home is the real home"; exit 99; }
case "$_fh" in "$_tmp"/*) : ;; *) echo "FATAL: FHIR home ($_fh) not under $_tmp"; exit 99 ;; esac
CACHE="${FHIR_CACHE:-$FHIR_HOME/.fhir/packages}"
OUT="${OUT:-/tmp/harvest-gate-$$}"; mkdir -p "$OUT"
JOBS="${JOBS:-10}"
STAMP=$(date +%s)

sel=("$@")
if [ ${#sel[@]} -eq 0 ]; then
  mapfile -t sel < <(find "$CORPUS" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | sort)
fi

run_one() {
  local slug="$1" BIN="$2" CORPUS="$3" CACHE="$4" OUT="$5" FHIR_HOME="$6"
  local case="$CORPUS/$slug" out="$OUT/$slug"
  rm -rf "$out"
  FHIR_CACHE="$CACHE" HOME="$FHIR_HOME" "$BIN" build "$case" -o "$out" >"$OUT/$slug.log" 2>&1
  local exp="$case/expected" got="$out/fsh-generated/resources"
  local p=0 f=0 m=0 x=0
  shopt -s nullglob
  for ef in "$exp"/*.json; do
    local bn; bn="$(basename "$ef")"
    if [ ! -f "$got/$bn" ]; then m=$((m+1)); echo "    MISS  $slug/$bn" >>"$OUT/fails.txt"
    elif diff -q "$ef" "$got/$bn" >/dev/null 2>&1; then p=$((p+1))
    else f=$((f+1)); echo "    DIFF  $slug/$bn" >>"$OUT/fails.txt"; fi
  done
  # files rust emitted that the oracle did NOT (extra resources, e.g. stray IG)
  for gf in "$got"/*.json; do
    local bn; bn="$(basename "$gf")"
    [ -f "$exp/$bn" ] || { x=$((x+1)); echo "    EXTRA $slug/$bn" >>"$OUT/fails.txt"; }
  done
  shopt -u nullglob
  printf '%s %d %d %d %d\n' "$slug" "$p" "$f" "$m" "$x" >>"$OUT/tallies.txt"
}
export -f run_one
: >"$OUT/tallies.txt"; : >"$OUT/fails.txt"

printf '%s\n' "${sel[@]}" | \
  xargs -I{} -P "$JOBS" bash -c 'run_one "$@"' _ {} "$BIN" "$CORPUS" "$CACHE" "$OUT" "$FHIR_HOME"

assert_real_fhir_untouched "$REAL_HOME" "$STAMP"

# Aggregate
awk '{p+=$2; f+=$3; m+=$4; x+=$5;
      if($3==0&&$4==0&&$5==0) okcases++; else badcases++; total++}
  END{
    printf "\n==== SUSHI-harvest gate ====\n";
    printf "cases:           %d  (clean: %d, diverging: %d)\n", total, okcases, badcases;
    printf "resources match: %d\n", p;
    printf "resource DIFF:   %d\n", f;
    printf "resource MISS:   %d  (oracle had, rust did not emit)\n", m;
    printf "resource EXTRA:  %d  (rust emitted, oracle did not)\n", x;
    tot=p+f+m;
    printf "byte-identical:  %d / %d  (%.1f%%)\n", p, tot, tot?p/tot*100:0;
    printf "case parity:     %d / %d  (%.1f%%)\n", okcases, total, total?okcases/total*100:0;
  }' "$OUT/tallies.txt"

if [ -s "$OUT/fails.txt" ]; then
  echo "--- divergences (first 60) ---"
  sort "$OUT/fails.txt" | head -60
  echo "(full list: $OUT/fails.txt)"
fi

#!/usr/bin/env bash
# Concurrency-safe single-purpose parity gate for parallel worktrees.
# Builds the requested IGs to a PRIVATE output root and byte-diffs each resource
# vs its stock oracle. Unlike full-dashboard.sh it never writes under temp/, so
# multiple worktrees can run it at once without clobbering each other.
#
# Oracle inputs (cache, IG sources, *-stock dirs) are READ-ONLY and shared from
# the MAIN worktree's temp/ (set MAIN_REPO, or symlink temp/ into the worktree).
#
# Usage:  [MAIN_REPO=/abs/main] [OUT=/tmp/mine] harness/gate1.sh ips epi carinbb ...
#         default IG set = the 4 corpus IGs (the 665 non-regression floor).
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$HERE" rev-parse --show-toplevel)"; cd "$REPO"
MAIN="${MAIN_REPO:-$REPO}"                 # where the shared oracle/cache lives
BIN="$REPO/target/release/rust_sushi"
[ -x "$BIN" ] || { echo "build rust_sushi --release first"; exit 2; }
CACHE="${FHIR_CACHE:-$MAIN/temp/fhir-home/.fhir/packages}"
OUT="${OUT:-/tmp/gate1-$$}"; mkdir -p "$OUT"

# Intentional divergences (docs/compat-breaks.json): gated vs golden, never counted as fail.
declare -A DIVERGE
if [ -f "$MAIN/docs/compat-breaks.json" ]; then
  while IFS= read -r key; do DIVERGE["$key"]=1; done < <(
    python3 -c "import json
for d in json.load(open('$MAIN/docs/compat-breaks.json'))['divergences']: print(d['ig']+'/'+d['file'])" 2>/dev/null)
fi

declare -A IG
for ig in ips epi mcode crd; do
  IG[$ig]="/home/jmandel/periodicity/temp/$ig-ig|$MAIN/temp/$ig-stock"
done
for ig in carinbb sdc pas dtr genomics ecr cmc ndh; do
  IG[$ig]="$MAIN/temp/holdout/$ig|$MAIN/temp/holdout/$ig-stock"
done

sel=("$@"); [ ${#sel[@]} -eq 0 ] && sel=(ips epi mcode crd)

printf "%-9s %6s %6s %6s %6s %s\n" IG STOCK PASS FAIL MISS STATUS
echo "------------------------------------------------------------"
GT=0; GP=0
for ig in "${sel[@]}"; do
  IFS='|' read -r src stock <<< "${IG[$ig]:-}"
  [ -n "$src" ] && [ -d "$stock/fsh-generated/resources" ] || { printf "%-9s  (missing src/stock)\n" "$ig"; continue; }
  out="$OUT/$ig"; rm -rf "$out"
  status="ok"; FHIR_CACHE="$CACHE" "$BIN" build "$src" -o "$out" >"$OUT/$ig.log" 2>&1 || status="BUILD-ERR"
  A="$stock/fsh-generated/resources"; B="$out/fsh-generated/resources"
  p=0; f=0; m=0
  for sf in "$A"/*.json; do
    bn="$(basename "$sf")"
    # intentional divergence (docs/compat-breaks.json): the stock-vs-ours diff must
    # EQUAL the recorded expected diff exactly (pins the specific accepted difference).
    if [ -n "${DIVERGE[$ig/$bn]:-}" ]; then
      exp="$MAIN/tests/compat-golden/$ig/$bn.diff"
      if [ -f "$B/$bn" ] && [ -f "$exp" ] && [ "$(diff "$sf" "$B/$bn")" = "$(cat "$exp")" ]; then :;
      else f=$((f+1)); echo "  COMPAT-UNEXPECTED $ig/$bn (divergence != recorded diff)"; fi
      continue
    fi
    if [ ! -f "$B/$bn" ]; then m=$((m+1));
    elif diff -q "$sf" "$B/$bn" >/dev/null 2>&1; then p=$((p+1)); else f=$((f+1)); fi
  done
  tot=$((p+f+m)); GT=$((GT+tot)); GP=$((GP+p))
  printf "%-9s %6s %6s %6s %6s %s\n" "$ig" "$tot" "$p" "$f" "$m" "$status"
done
echo "------------------------------------------------------------"
printf "TOTAL byte-identical: %s / %s (%.1f%%)  [out=%s]\n" "$GP" "$GT" "$(awk "BEGIN{print $GT?$GP/$GT*100:0}")" "$OUT"
# List per-resource failures for the LAST ig (handy when gating one IG).
last="${sel[${#sel[@]}-1]}"; IFS='|' read -r _ stock <<< "${IG[$last]:-}"
if [ -n "${stock:-}" ] && [ -d "$stock/fsh-generated/resources" ]; then
  A="$stock/fsh-generated/resources"; B="$OUT/$last/fsh-generated/resources"
  echo "--- $last failing files (first 25) ---"
  n=0; for sf in "$A"/*.json; do bn="$(basename "$sf")"
    if [ -f "$B/$bn" ] && ! diff -q "$sf" "$B/$bn" >/dev/null 2>&1; then echo "  $bn"; n=$((n+1)); [ $n -ge 25 ] && break; fi
  done
fi

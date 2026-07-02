#!/usr/bin/env bash
# Parity scorecard for the 6 FSH-buildable top-20 IGs NOT in the 12-IG corpus.
# These are built SELF-RELIANTLY (rust_sushi --materialize: acquire deps from the
# registry -> CAS -> materialize), then byte-diffed vs the stock oracle captured in
# temp/top20/<slug>-stock. Honors docs/compat-breaks.json (diff-based) like the 12-IG gate.
#
# Usage: dashboard-top20.sh [slug ...]   (default: all 6)
# First run downloads dependency closures into the shared CAS (network); later runs are fast.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$HERE" rev-parse --show-toplevel)"; cd "$REPO"
BIN=target/release/rust_sushi
[ -x "$BIN" ] || { echo "build rust_sushi --release first"; exit 2; }
CAS="$REPO/temp/top20cas"
CACHE="$REPO/temp/top20-cache"
GOLDEN="$REPO/tests/compat-golden"; BREAKS="$REPO/docs/compat-breaks.json"

declare -A DIVERGE
if [ -f "$BREAKS" ]; then
  while IFS= read -r key; do DIVERGE["$key"]=1; done < <(
    python3 -c "import json
for d in json.load(open('$BREAKS'))['divergences']: print(d['ig']+'/'+d['file'])" 2>/dev/null)
fi

sel=("${@:-}"); [ -z "${sel[*]}" ] && sel=(bulk pdex plannet formulary cdshooks subscriptions)

printf "%-14s %6s %6s %6s %6s %5s %s\n" IG STOCK PASS FAIL MISS DIV STATUS
echo "--------------------------------------------------------------------------"
GT=0; GP=0; GD=0; ALERTS=()
for ig in "${sel[@]}"; do
  src="$REPO/temp/top20/$ig"; stock="$REPO/temp/top20/$ig-stock"
  [ -d "$src" ] && [ -d "$stock/fsh-generated/resources" ] || { printf "%-14s (missing src/stock)\n" "$ig"; continue; }
  out="$REPO/temp/top20/$ig-rust"; rm -rf "$out"
  status="ok"; "$BIN" build "$src" -o "$out" --materialize --cas "$CAS" --cache "$CACHE" --lock "$REPO/temp/top20/$ig.lock" >/tmp/d20-$ig.log 2>&1 || status="BUILD-ERR"
  A="$stock/fsh-generated/resources"; B="$out/fsh-generated/resources"
  p=0; f=0; m=0; d=0
  for sf in "$A"/*.json; do
    bn="$(basename "$sf")"
    if [ -n "${DIVERGE[$ig/$bn]:-}" ]; then
      exp="$GOLDEN/$ig/$bn.diff"
      if [ -f "$B/$bn" ] && [ -f "$exp" ] && [ "$(diff "$sf" "$B/$bn")" = "$(cat "$exp")" ]; then d=$((d+1)); else f=$((f+1)); ALERTS+=("$ig/$bn: divergence != recorded"); fi
      continue
    fi
    if [ ! -f "$B/$bn" ]; then m=$((m+1));
    elif diff -q "$sf" "$B/$bn" >/dev/null 2>&1; then p=$((p+1)); else f=$((f+1)); fi
  done
  tot=$((p+f+m+d)); GT=$((GT+tot)); GP=$((GP+p)); GD=$((GD+d))
  printf "%-14s %6s %6s %6s %6s %5s %s\n" "$ig" "$tot" "$p" "$f" "$m" "$d" "$status"
done
echo "--------------------------------------------------------------------------"
EQ=$((GP+GD))
printf "TOP-20 (new 6) byte-identical: %s / %s (%.1f%%)" "$GP" "$GT" "$(awk "BEGIN{print $GT?$GP/$GT*100:0}")"
[ "$GD" -gt 0 ] && printf "  + %s tracked divergence(s)" "$GD"; echo
printf "EQUIVALENT: %s / %s (%.1f%%)\n" "$EQ" "$GT" "$(awk "BEGIN{print $GT?$EQ/$GT*100:0}")"
[ ${#ALERTS[@]} -gt 0 ] && { echo "--- alerts ---"; printf '  %s\n' "${ALERTS[@]}"; }

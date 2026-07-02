#!/usr/bin/env bash
# Combined parity scorecard across the 4 tuning IGs + the 8 holdout IGs (12 total).
# Builds each with rust_sushi and byte-diffs every resource vs its stock oracle.
# Anti-overfitting gate: a fix must not regress ANY of the 12.
#
# COMPAT-BREAKS: files listed in docs/compat-breaks.json are INTENTIONAL divergences
# (stock emits invalid/buggy output; we emit correct output). They do NOT count as
# failures. Instead they are gated against OUR golden (tests/compat-golden/<ig>/<file>)
# and reported in the DIV column. Each run re-verifies: our output still == golden AND
# still != stock. If our output drifts from the golden -> DRIFT (counts as fail). If
# stock now == our golden -> RESOLVED (divergence obsolete; drop the entry).
#
# Usage: full-dashboard.sh [ig ...]   (default: all 12)
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$HERE" rev-parse --show-toplevel)"; cd "$REPO"
BIN=target/release/rust_sushi
[ -x "$BIN" ] || { echo "build rust_sushi --release first"; exit 2; }
CACHE="$REPO/temp/fhir-home/.fhir/packages"
GOLDEN="$REPO/tests/compat-golden"
BREAKS="$REPO/docs/compat-breaks.json"

# Allowlist of intentional divergences: "ig/file" -> 1
declare -A DIVERGE
if [ -f "$BREAKS" ]; then
  while IFS= read -r key; do DIVERGE["$key"]=1; done < <(
    python3 -c "import json,sys
for d in json.load(open('$BREAKS'))['divergences']: print(d['ig']+'/'+d['file'])" 2>/dev/null)
fi

# name -> "src_dir|stock_dir"
declare -A IG
for ig in ips epi mcode crd; do
  IG[$ig]="/home/jmandel/periodicity/temp/$ig-ig|$REPO/temp/$ig-stock"
done
for ig in carinbb sdc pas dtr genomics ecr cmc ndh; do
  IG[$ig]="$REPO/temp/holdout/$ig|$REPO/temp/holdout/$ig-stock"
done

sel=("${@:-}"); [ -z "${sel[*]}" ] && sel=(ips epi mcode crd carinbb sdc pas dtr genomics ecr cmc ndh)

printf "%-9s %6s %6s %6s %6s %5s %s\n" IG STOCK PASS FAIL MISS DIV STATUS
echo "----------------------------------------------------------------------"
GT=0; GP=0; GD=0; ALERTS=()
for ig in "${sel[@]}"; do
  IFS='|' read -r src stock <<< "${IG[$ig]:-}"
  [ -n "$src" ] && [ -d "$stock/fsh-generated/resources" ] || { printf "%-9s  (missing src/stock)\n" "$ig"; continue; }
  out="temp/holdout/$ig-rust"; [[ "$ig" =~ ^(ips|epi|mcode|crd)$ ]] && out="temp/rust-$ig"
  rm -rf "$out"
  status="ok"; FHIR_CACHE="$CACHE" "$BIN" build "$src" -o "$out" >/tmp/fd-$ig.log 2>&1 || status="BUILD-ERR"
  A="$stock/fsh-generated/resources"; B="$out/fsh-generated/resources"
  p=0; f=0; m=0; d=0
  for sf in "$A"/*.json; do
    bn="$(basename "$sf")"
    if [ -n "${DIVERGE[$ig/$bn]:-}" ]; then
      # intentional divergence: the CURRENT stock-vs-ours diff must equal the
      # recorded expected diff EXACTLY. Any other change (regression or a new
      # divergence) makes the diff differ -> flagged. This pins the specific
      # accepted difference, not the whole file.
      exp="$GOLDEN/$ig/$bn.diff"
      if [ ! -f "$B/$bn" ]; then f=$((f+1)); ALERTS+=("$ig/$bn: file missing from our output");
      elif [ ! -f "$exp" ]; then f=$((f+1)); ALERTS+=("$ig/$bn: no expected-diff recorded");
      else
        cur="$(diff "$sf" "$B/$bn")"
        if [ -z "$cur" ]; then d=$((d+1)); ALERTS+=("$ig/$bn: RESOLVED — stock now matches ours; drop the compat-break entry");
        elif [ "$cur" = "$(cat "$exp")" ]; then d=$((d+1));   # exactly the accepted divergence
        else f=$((f+1)); ALERTS+=("$ig/$bn: UNEXPECTED DIFF — divergence changed vs recorded; re-review (regression or new diff)"); fi
      fi
      continue
    fi
    if [ ! -f "$B/$bn" ]; then m=$((m+1));
    elif diff -q "$sf" "$B/$bn" >/dev/null 2>&1; then p=$((p+1)); else f=$((f+1)); fi
  done
  tot=$((p+f+m+d)); GT=$((GT+tot)); GP=$((GP+p)); GD=$((GD+d))
  printf "%-9s %6s %6s %6s %6s %5s %s\n" "$ig" "$tot" "$p" "$f" "$m" "$d" "$status"
done
echo "----------------------------------------------------------------------"
printf "TOTAL byte-identical: %s / %s (%.1f%%)" "$GP" "$GT" "$(awk "BEGIN{print $GT?$GP/$GT*100:0}")"
[ "$GD" -gt 0 ] && printf "   + %s intentional divergence(s) [stock invalid; gated vs golden]" "$GD"
echo
# Equivalent = byte-identical OR a tracked intentional divergence matching its golden.
EQ=$((GP+GD)); printf "EQUIVALENT (parity + tracked divergences): %s / %s (%.1f%%)\n" "$EQ" "$GT" "$(awk "BEGIN{print $GT?$EQ/$GT*100:0}")"
if [ ${#ALERTS[@]} -gt 0 ]; then
  echo "--- compat-break alerts ---"; printf '  %s\n' "${ALERTS[@]}"
fi

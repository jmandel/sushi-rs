#!/usr/bin/env bash
# Combined parity scorecard across the 4 tuning IGs + the 8 holdout IGs (12 total).
# Builds each with rust_sushi and byte-diffs every resource vs its stock oracle.
# Anti-overfitting gate: a fix must not regress ANY of the 12.
#
# Usage: full-dashboard.sh [ig ...]   (default: all 12)
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$HERE" rev-parse --show-toplevel)"; cd "$REPO"
BIN=target/release/rust_sushi
[ -x "$BIN" ] || { echo "build rust_sushi --release first"; exit 2; }
CACHE="$REPO/temp/fhir-home/.fhir/packages"

# name -> "src_dir|stock_dir"
declare -A IG
for ig in ips epi mcode crd; do
  IG[$ig]="/home/jmandel/periodicity/temp/$ig-ig|$REPO/temp/$ig-stock"
done
for ig in carinbb sdc pas dtr genomics ecr cmc ndh; do
  IG[$ig]="$REPO/temp/holdout/$ig|$REPO/temp/holdout/$ig-stock"
done

sel=("${@:-}"); [ -z "${sel[*]}" ] && sel=(ips epi mcode crd carinbb sdc pas dtr genomics ecr cmc ndh)

printf "%-9s %6s %6s %6s %6s %s\n" IG STOCK PASS FAIL MISS STATUS
echo "------------------------------------------------------------"
GT=0; GP=0
for ig in "${sel[@]}"; do
  IFS='|' read -r src stock <<< "${IG[$ig]:-}"
  [ -n "$src" ] && [ -d "$stock/fsh-generated/resources" ] || { printf "%-9s  (missing src/stock)\n" "$ig"; continue; }
  out="temp/holdout/$ig-rust"; [[ "$ig" =~ ^(ips|epi|mcode|crd)$ ]] && out="temp/rust-$ig"
  rm -rf "$out"
  status="ok"; FHIR_CACHE="$CACHE" "$BIN" build "$src" -o "$out" >/tmp/fd-$ig.log 2>&1 || status="BUILD-ERR"
  A="$stock/fsh-generated/resources"; B="$out/fsh-generated/resources"
  p=0; f=0; m=0
  for sf in "$A"/*.json; do
    bn="$(basename "$sf")"
    if [ ! -f "$B/$bn" ]; then m=$((m+1));
    elif diff -q "$sf" "$B/$bn" >/dev/null 2>&1; then p=$((p+1)); else f=$((f+1)); fi
  done
  tot=$((p+f+m)); GT=$((GT+tot)); GP=$((GP+p))
  printf "%-9s %6s %6s %6s %6s %s\n" "$ig" "$tot" "$p" "$f" "$m" "$status"
done
echo "------------------------------------------------------------"
printf "TOTAL byte-identical: %s / %s (%.1f%%)\n" "$GP" "$GT" "$(awk "BEGIN{print $GT?$GP/$GT*100:0}")"

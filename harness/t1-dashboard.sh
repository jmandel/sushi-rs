#!/usr/bin/env bash
# Collision-free dashboard: writes rust output to a PRIVATE dir (not shared temp/),
# so concurrent agents building into temp/ don't corrupt our measurement.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$HERE" rev-parse --show-toplevel)"; cd "$REPO"
BIN=target/release/rust_sushi
[ -x "$BIN" ] || { echo "build rust_sushi --release first"; exit 2; }
CACHE="$REPO/temp/fhir-home/.fhir/packages"
OUTROOT="${T1_OUT:-/tmp/t1-dash}"
mkdir -p "$OUTROOT"

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
  out="$OUTROOT/$ig"; rm -rf "$out"
  status="ok"; FHIR_CACHE="$CACHE" "$BIN" build "$src" -o "$out" >/tmp/t1-$ig.log 2>&1 || status="BUILD-ERR"
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

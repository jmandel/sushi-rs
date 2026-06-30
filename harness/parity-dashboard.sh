#!/usr/bin/env bash
# Phase 8 scorecard: build each IG with rust_sushi and report per-resource-type
# byte-parity vs the stock oracle. Read-only against stock; never touches ~/.fhir.
#
# Usage: parity-dashboard.sh [ig1 ig2 ...]   (default: ips epi mcode crd)
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$HERE" rev-parse --show-toplevel)"
cd "$REPO"

BIN=target/release/rust_sushi
[ -x "$BIN" ] || { echo "build rust_sushi --release first"; exit 2; }

IGS=("${@:-}")
[ -z "${IGS[*]}" ] && IGS=(ips epi mcode crd)

declare -A IGPATH=(
  [ips]=/home/jmandel/periodicity/temp/ips-ig
  [epi]=/home/jmandel/periodicity/temp/epi-ig
  [mcode]=/home/jmandel/periodicity/temp/mcode-ig
  [crd]=/home/jmandel/periodicity/temp/crd-ig
)

# count of stock json files matching a prefix
cnt() { ls "$1"/${2}*.json 2>/dev/null | wc -l | tr -d ' '; }

printf "%-7s %-18s %6s %6s %6s\n" IG TYPE STOCK PASS FAIL
echo "--------------------------------------------------------"
GTOT=0; GPASS=0
for ig in "${IGS[@]}"; do
  src="${IGPATH[$ig]:-}"
  stock="temp/${ig}-stock"
  [ -d "$src" ] && [ -d "$stock/fsh-generated/resources" ] || { echo "skip $ig (missing src/stock)"; continue; }
  out="temp/rust-${ig}"
  rm -rf "$out"
  "$BIN" build "$src" -o "$out" >/dev/null 2>&1
  A="$stock/fsh-generated/resources"; B="$out/fsh-generated/resources"
  for pfx in StructureDefinition ValueSet CodeSystem __OTHER__; do
    if [ "$pfx" = "__OTHER__" ]; then
      mapfile -t files < <(ls "$A"/*.json 2>/dev/null | grep -vE '/(StructureDefinition|ValueSet|CodeSystem)-' )
      label="Instance/other"
    else
      mapfile -t files < <(ls "$A/$pfx"*.json 2>/dev/null)
      label="$pfx"
    fi
    [ "${#files[@]}" -eq 0 ] && continue
    p=0; f=0
    for sf in "${files[@]}"; do
      bn="$(basename "$sf")"
      if [ -f "$B/$bn" ] && diff -q "$sf" "$B/$bn" >/dev/null 2>&1; then p=$((p+1)); else f=$((f+1)); fi
    done
    printf "%-7s %-18s %6s %6s %6s\n" "$ig" "$label" "${#files[@]}" "$p" "$f"
    GTOT=$((GTOT+${#files[@]})); GPASS=$((GPASS+p))
  done
done
echo "--------------------------------------------------------"
printf "TOTAL byte-identical: %s / %s\n" "$GPASS" "$GTOT"

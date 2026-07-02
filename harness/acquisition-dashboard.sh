#!/usr/bin/env bash
# Compare a stock-cache Rust build with an acquisition-materialized Rust build.
# This isolates package acquisition effects from known compiler holdout diffs:
# for each IG, the two Rust outputs should be byte-identical.
#
# Usage: acquisition-dashboard.sh [ig ...]   (default: 4 tuning IGs + 8 holdouts)
# Env:
#   FHIR_CACHE=<isolated stock-seeded package cache>
#   HOLDOUT_ROOT=<dir containing carinbb/sdc/pas/dtr/genomics/ecr/cmc/ndh>
#   FHIR_CAS=<CAS dir>
#   ACQ_DASHBOARD_WORK=<scratch dir for locks/caches/outputs>
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$HERE" rev-parse --show-toplevel)"
cd "$REPO"

BIN=target/release/rust_sushi
[ -x "$BIN" ] || { echo "build rust_sushi --release first"; exit 2; }

default_stock_cache="$REPO/temp/fhir-home/.fhir/packages"
if [ ! -d "$default_stock_cache" ] && [ -d /home/jmandel/hobby/sushi-rs/temp/fhir-home/.fhir/packages ]; then
  default_stock_cache=/home/jmandel/hobby/sushi-rs/temp/fhir-home/.fhir/packages
fi
STOCK_CACHE="${FHIR_CACHE:-$default_stock_cache}"
[ -d "$STOCK_CACHE" ] || { echo "missing isolated stock cache: $STOCK_CACHE"; exit 2; }

default_holdout_root="$REPO/temp/holdout"
if [ ! -d "$default_holdout_root" ] && [ -d /home/jmandel/hobby/sushi-rs/temp/holdout ]; then
  default_holdout_root=/home/jmandel/hobby/sushi-rs/temp/holdout
fi
HOLDOUT_ROOT="${HOLDOUT_ROOT:-$default_holdout_root}"

WORK="${ACQ_DASHBOARD_WORK:-$REPO/temp/acq-dashboard}"
CAS="${FHIR_CAS:-$WORK/cas}"
mkdir -p "$WORK"

declare -A IG
for ig in ips epi mcode crd; do
  IG[$ig]="/home/jmandel/periodicity/temp/$ig-ig"
done
for ig in carinbb sdc pas dtr genomics ecr cmc ndh; do
  IG[$ig]="$HOLDOUT_ROOT/$ig"
done

sel=("${@:-}")
[ -z "${sel[*]}" ] && sel=(ips epi mcode crd carinbb sdc pas dtr genomics ecr cmc ndh)

printf "%-9s %7s %7s %7s %7s %s\n" IG STOCK PASS DIFF MISS+EXTRA STATUS
echo "----------------------------------------------------------------"
GT=0; GP=0; GBAD=0
for ig in "${sel[@]}"; do
  src="${IG[$ig]:-}"
  if [ -z "$src" ] || [ ! -d "$src" ]; then
    printf "%-9s  (missing src: %s)\n" "$ig" "${src:-unknown}"
    continue
  fi

  stock_out="$WORK/$ig-stock-cache-rust"
  acq_out="$WORK/$ig-acq-rust"
  cache="$WORK/$ig-cache"
  lock="$WORK/$ig-fhir-deps.lock"
  rm -rf "$stock_out" "$acq_out" "$cache"

  status="ok"
  FHIR_CACHE="$STOCK_CACHE" "$BIN" build "$src" -o "$stock_out" >"$WORK/$ig-stock-cache.log" 2>&1 || status="STOCK-BUILD-ERR"
  "$BIN" build "$src" -o "$acq_out" --materialize --cas "$CAS" --lock "$lock" --cache "$cache" >"$WORK/$ig-acq.log" 2>&1 || {
    if [ "$status" = "ok" ]; then status="ACQ-BUILD-ERR"; else status="$status+ACQ-BUILD-ERR"; fi
  }

  A="$stock_out/fsh-generated/resources"
  B="$acq_out/fsh-generated/resources"
  p=0; f=0; m=0; e=0
  if [ -d "$A" ]; then
    for sf in "$A"/*.json; do
      [ -e "$sf" ] || continue
      bn="$(basename "$sf")"
      if [ ! -f "$B/$bn" ]; then
        m=$((m+1))
      elif diff -q "$sf" "$B/$bn" >/dev/null 2>&1; then
        p=$((p+1))
      else
        f=$((f+1))
      fi
    done
  fi
  if [ -d "$B" ]; then
    for bf in "$B"/*.json; do
      [ -e "$bf" ] || continue
      bn="$(basename "$bf")"
      [ -f "$A/$bn" ] || e=$((e+1))
    done
  fi
  tot=$((p+f+m))
  bad=$((f+m+e))
  GT=$((GT+tot)); GP=$((GP+p)); GBAD=$((GBAD+bad))
  printf "%-9s %7s %7s %7s %7s %s\n" "$ig" "$tot" "$p" "$f" "$((m+e))" "$status"
done
echo "----------------------------------------------------------------"
printf "TOTAL acquisition matches stock-cache Rust output: %s / %s (%s mismatches)\n" "$GP" "$GT" "$GBAD"

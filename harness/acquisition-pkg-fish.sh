#!/usr/bin/env bash
# Package-fishing parity against acquisition-materialized caches.
#
# For each IG, this script:
#   1. resolves/materializes dependencies through the CAS,
#   2. derives deterministic id/name/url queries from the materialized cache,
#   3. runs stock SUSHI's package oracle under an isolated HOME backed by the
#      same materialized cache, and
#   4. diffs that against `rust_sushi pkg-fish`.
#
# Usage: acquisition-pkg-fish.sh [ig ...]   (default: 4 tuning IGs + 8 holdouts)
# Env:
#   HOLDOUT_ROOT=<dir containing carinbb/sdc/pas/dtr/genomics/ecr/cmc/ndh>
#   FHIR_CAS=<CAS dir>
#   ACQ_PKG_FISH_WORK=<scratch dir for locks/caches/query/oracle files>
#   ACQ_PKG_FISH_ORACLE_HOME_ROOT=<isolated HOME root for stock oracle caches>
#   PKG_FISH_MAX=<max queries per IG, default 400>
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$HERE" rev-parse --show-toplevel)"
cd "$REPO"

BIN=target/release/rust_sushi
[ -x "$BIN" ] || { echo "build rust_sushi --release first"; exit 2; }

default_holdout_root="$REPO/temp/holdout"
if [ ! -d "$default_holdout_root" ] && [ -d /home/jmandel/hobby/sushi-rs/temp/holdout ]; then
  default_holdout_root=/home/jmandel/hobby/sushi-rs/temp/holdout
fi
HOLDOUT_ROOT="${HOLDOUT_ROOT:-$default_holdout_root}"

WORK="${ACQ_PKG_FISH_WORK:-$REPO/temp/acq-pkg-fish}"
CAS="${FHIR_CAS:-$WORK/cas}"
ORACLE_HOME_ROOT="${ACQ_PKG_FISH_ORACLE_HOME_ROOT:-$REPO/temp/fhir-home-acq-pkg-fish}"
MAX="${PKG_FISH_MAX:-400}"
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

printf "%-9s %7s %s\n" IG QUERIES STATUS
echo "--------------------------------"
total=0
failed=0
for ig in "${sel[@]}"; do
  src="${IG[$ig]:-}"
  if [ -z "$src" ] || [ ! -d "$src" ]; then
    printf "%-9s  (missing src: %s)\n" "$ig" "${src:-unknown}"
    failed=$((failed+1))
    continue
  fi

  cache="$WORK/$ig-cache"
  lock="$WORK/$ig-fhir-deps.lock"
  queries="$WORK/$ig-queries.txt"
  oracle="$WORK/$ig-oracle.json"
  rust="$WORK/$ig-rust.json"
  diff="$WORK/$ig-diff.log"
  oracle_home="$ORACLE_HOME_ROOT/$ig"
  rm -rf "$cache"
  rm -rf "$oracle_home"

  status="ok"
  "$BIN" deps lock --project "$src" --lock "$lock" --cas "$CAS" >"$WORK/$ig-lock.log" 2>&1 || status="LOCK-ERR"
  if [ "$status" = "ok" ]; then
    "$BIN" materialize --lock "$lock" --out "$cache" --cas "$CAS" >"$WORK/$ig-materialize.log" 2>&1 || status="MATERIALIZE-ERR"
  fi
  if [ "$status" = "ok" ]; then
    mkdir -p "$oracle_home/.fhir"
    cp -al "$cache" "$oracle_home/.fhir/packages" || status="ORACLE-CACHE-ERR"
  fi
  if [ "$status" = "ok" ]; then
    node harness/gen-pkg-queries.cjs "$cache" --max "$MAX" >"$queries" || status="QUERY-ERR"
  fi

  qcount=0
  if [ "$status" = "ok" ]; then
    mapfile -t q <"$queries"
    qcount="${#q[@]}"
    total=$((total+qcount))
    HOME="$oracle_home" node harness/package-oracle.cjs "$src" "${q[@]}" >"$oracle" 2>"$WORK/$ig-oracle.err" || status="ORACLE-ERR"
  fi
  if [ "$status" = "ok" ]; then
    "$BIN" pkg-fish "$src" "$cache" "${q[@]}" >"$rust" 2>"$WORK/$ig-rust.err" || status="RUST-ERR"
  fi
  if [ "$status" = "ok" ]; then
    node harness/diff-pkg.cjs "$oracle" "$rust" >"$diff" 2>&1 || status="DIFF"
  fi
  if [ "$status" != "ok" ]; then
    failed=$((failed+1))
  fi
  printf "%-9s %7s %s\n" "$ig" "$qcount" "$status"
done
echo "--------------------------------"
printf "TOTAL pkg-fish queries: %s; failures: %s\n" "$total" "$failed"
exit "$failed"

#!/usr/bin/env bash
# Two-phase performance harness for the 31-IG corpus:
#   1. materialize: CAS + lock -> .fhir/packages-style cache
#   2. build:       materialized cache -> fsh-generated output
#
# Defaults write only under temp/perf31. The timed benchmark path uses
# --offline by default, so it measures local CAS/materialization + compiler work.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$HERE" rev-parse --show-toplevel)"
cd "$REPO"

MAIN="${MAIN_REPO:-$REPO}"
BIN="${BIN:-$REPO/target/release/rust_sushi}"
WORK="${PERF31_WORK:-$REPO/temp/perf31}"
CAS="${FHIR_CAS:-$WORK/cas}"
RUNS="${RUNS:-3}"
OFFLINE="${OFFLINE:-1}"
FREQ="${FREQ:-997}"

ALL_IGS=(
  ips epi mcode crd
  carinbb sdc pas dtr genomics ecr cmc ndh
  bulk pdex plannet formulary cdshooks subscriptions
  application-feature be-vaccination ccda-cda deid eu-eps eu-mpd mhd pacio-toc ph-query radiation-dose safr tw-pas vhl
)

declare -A SRC STOCK
for ig in ips epi mcode crd; do
  SRC[$ig]="/home/jmandel/periodicity/temp/$ig-ig"
  STOCK[$ig]="$MAIN/temp/$ig-stock"
done
for ig in carinbb sdc pas dtr genomics ecr cmc ndh; do
  SRC[$ig]="$MAIN/temp/holdout/$ig"
  STOCK[$ig]="$MAIN/temp/holdout/$ig-stock"
done
for ig in bulk pdex plannet formulary cdshooks subscriptions; do
  SRC[$ig]="$MAIN/temp/top20/$ig"
  STOCK[$ig]="$MAIN/temp/top20/$ig-stock"
done
for ig in application-feature be-vaccination deid eu-eps eu-mpd mhd pacio-toc ph-query radiation-dose safr tw-pas vhl; do
  SRC[$ig]="$MAIN/temp/next20/$ig"
  STOCK[$ig]="$MAIN/temp/next20/$ig-stock"
done
SRC[ccda-cda]="$MAIN/temp/next20/ccda-cda/fsh-tank"
STOCK[ccda-cda]="$MAIN/temp/next20/ccda-cda-stock"

usage() {
  cat <<'EOF'
Usage:
  harness/perf31.sh list
  harness/perf31.sh prepare [ig...]
  harness/perf31.sh bench [ig...]
  harness/perf31.sh summarize [results.csv]
  harness/perf31.sh profile <materialize|build> <ig>

Environment:
  MAIN_REPO=/abs/repo-with-temp     stock oracles/source roots for worktrees
  PERF31_WORK=temp/perf31           locks, logs, materialized caches, outputs
FHIR_CAS=$PERF31_WORK/cas         package CAS; never defaults to ~/.fhir
RUNS=3                            timed iterations for bench
OFFLINE=1                         bench/profile materialize with --offline
 PREPARE_TIMEOUT=180              per-IG setup timeout in seconds; 0 disables
BIN=target/release/rust_sushi     binary to time
FREQ=997                          perf sample frequency

Workflow:
  OFFLINE=0 harness/perf31.sh prepare       # create locks, populate CAS
  harness/perf31.sh bench                   # CAS -> materialized, then build
  harness/perf31.sh profile build mcode     # perf record one focused run
EOF
}

ensure_bin() {
  if [ ! -x "$BIN" ]; then
    echo "building release rust_sushi..."
    cargo build --release -q || exit $?
  fi
}

lock_path() {
  printf '%s/locks/%s.lock' "$WORK" "$1"
}

selected_igs() {
  if [ "$#" -eq 0 ]; then
    printf '%s\n' "${ALL_IGS[@]}"
    return
  fi
  local ig
  for ig in "$@"; do
    if [ -z "${SRC[$ig]:-}" ]; then
      echo "unknown IG: $ig" >&2
      exit 2
    fi
    printf '%s\n' "$ig"
  done
}

lock_package_count() {
  python3 - "$1" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
    lock = json.load(f)
print(len(lock.get("packages", [])))
PY
}

tree_stats() {
  local path="$1"
  if [ -d "$path" ]; then
    local files bytes
    files="$(find -L "$path" -type f | wc -l | tr -d ' ')"
    bytes="$(du -sbL "$path" | awk '{print $1}')"
    printf '%s,%s' "$files" "$bytes"
  else
    printf '0,0'
  fi
}

csv_append() {
  python3 - "$@" <<'PY'
import csv, sys
path = sys.argv[1]
row = sys.argv[2:]
with open(path, "a", newline="") as f:
    csv.writer(f).writerow(row)
PY
}

run_timed() {
  local log="$1"; shift
  mkdir -p "$(dirname "$log")"
  local start end
  start="$(date +%s%N)"
  "$@" >"$log" 2>&1
  TIMED_STATUS=$?
  end="$(date +%s%N)"
  TIMED_SECONDS="$(python3 - "$start" "$end" <<'PY'
import sys
start = int(sys.argv[1])
end = int(sys.argv[2])
print(f"{(end - start) / 1_000_000_000:.6f}")
PY
)"
}

list_igs() {
  local ig
  printf "%-22s %s\n" IG SOURCE
  printf "%-22s %s\n" -- ------
  for ig in "${ALL_IGS[@]}"; do
    printf "%-22s %s\n" "$ig" "${SRC[$ig]}"
  done
}

prepare_one() {
  local ig="$1"
  local src="${SRC[$ig]}"
  local lock
  lock="$(lock_path "$ig")"
  mkdir -p "$(dirname "$lock")" "$CAS"
  local flags=(deps lock --project "$src" --lock "$lock" --cas "$CAS")
  if [ "${OFFLINE:-0}" = "1" ]; then
    flags+=(--offline)
  fi
  echo "prepare $ig -> $lock"
  if [ "${PREPARE_TIMEOUT:-180}" != "0" ] && command -v timeout >/dev/null 2>&1; then
    timeout "${PREPARE_TIMEOUT:-180}" "$BIN" "${flags[@]}" >"$WORK/logs/prepare-$ig.log" 2>&1
  else
    "$BIN" "${flags[@]}" >"$WORK/logs/prepare-$ig.log" 2>&1
  fi
  local status=$?
  if [ "$status" -ne 0 ]; then
    echo "  FAILED; see $WORK/logs/prepare-$ig.log"
    return "$status"
  fi
  echo "  packages=$(lock_package_count "$lock")"
}

bench_one() {
  local ig="$1"
  local src="${SRC[$ig]}"
  local lock
  lock="$(lock_path "$ig")"
  if [ ! -f "$lock" ]; then
    echo "missing lock for $ig; run OFFLINE=0 harness/perf31.sh prepare $ig" >&2
    return 2
  fi
  local packages
  packages="$(lock_package_count "$lock")"
  local ig_root="$BENCH_ROOT/$ig"
  mkdir -p "$ig_root"
  local i
  for ((i=1; i<=RUNS; i++)); do
    local cache="$ig_root/cache-$i"
    local out="$ig_root/out-$i"
    local mlog="$ig_root/materialize-$i.log"
    local blog="$ig_root/build-$i.log"
    rm -rf "$cache" "$out"

    local flags=(materialize --lock "$lock" --cas "$CAS" --out "$cache")
    if [ "$OFFLINE" = "1" ]; then
      flags+=(--offline)
    fi
    run_timed "$mlog" "$BIN" "${flags[@]}"
    local status="$TIMED_STATUS"
    local seconds="$TIMED_SECONDS"
    local stats
    stats="$(tree_stats "$cache")"
    IFS=',' read -r files bytes <<< "$stats"
    csv_append "$RESULTS" "$ig" "$i" materialize "$status" "$seconds" "$packages" "$files" "$bytes" "$src" "$cache" "$out" "$mlog"
    printf "%-22s iter=%s materialize %8ss status=%s files=%s\n" "$ig" "$i" "$seconds" "$status" "$files"
    if [ "$status" -ne 0 ]; then
      continue
    fi

    run_timed "$blog" "$BIN" build "$src" -o "$out" --cache "$cache"
    status="$TIMED_STATUS"
    seconds="$TIMED_SECONDS"
    stats="$(tree_stats "$out/fsh-generated/resources")"
    IFS=',' read -r files bytes <<< "$stats"
    csv_append "$RESULTS" "$ig" "$i" build "$status" "$seconds" "$packages" "$files" "$bytes" "$src" "$cache" "$out" "$blog"
    printf "%-22s iter=%s build       %8ss status=%s resources=%s\n" "$ig" "$i" "$seconds" "$status" "$files"
  done
}

summarize() {
  local results="$1"
  if [ ! -f "$results" ]; then
    echo "missing results CSV: $results" >&2
    return 2
  fi
  python3 - "$results" <<'PY'
import csv, statistics, sys
from collections import defaultdict

rows = []
with open(sys.argv[1], newline="") as f:
    for row in csv.DictReader(f):
        rows.append(row)

groups = defaultdict(list)
failures = []
for row in rows:
    if row["status"] != "0":
        failures.append(row)
        continue
    groups[(row["ig"], row["phase"])].append(float(row["seconds"]))

igs = []
for row in rows:
    if row["ig"] not in igs:
        igs.append(row["ig"])

print(f"results={sys.argv[1]}")
print(f"{'IG':22} {'mat-med':>9} {'mat-min':>9} {'build-med':>10} {'build-min':>10} {'total-med':>10}")
print("-" * 76)
total_mat = 0.0
total_build = 0.0
totals = []
for ig in igs:
    mat = groups.get((ig, "materialize"), [])
    build = groups.get((ig, "build"), [])
    mat_med = statistics.median(mat) if mat else None
    mat_min = min(mat) if mat else None
    build_med = statistics.median(build) if build else None
    build_min = min(build) if build else None
    if mat_med is not None:
        total_mat += mat_med
    if build_med is not None:
        total_build += build_med
    def fmt(v):
        return f"{v:.3f}" if v is not None else "ERR"
    total = mat_med + build_med if mat_med is not None and build_med is not None else None
    if total is not None:
        totals.append((ig, mat_med, build_med, total))
    print(f"{ig:22} {fmt(mat_med):>9} {fmt(mat_min):>9} {fmt(build_med):>10} {fmt(build_min):>10} {fmt(total):>10}")
print("-" * 76)
print(f"{'TOTAL median-sum':22} {total_mat:9.3f} {'':9} {total_build:10.3f} {'':10} {total_mat + total_build:10.3f}")
if totals:
    def tail(label, key):
        print()
        print(label)
        for ig, mat, build, total in sorted(totals, key=key, reverse=True)[:8]:
            print(f"  {ig:22} mat={mat:.3f}s build={build:.3f}s total={total:.3f}s")
    tail("Top total tails:", lambda row: row[3])
    tail("Top build tails:", lambda row: row[2])
    tail("Top materialize tails:", lambda row: row[1])
if failures:
    print()
    print("Failures:")
    for row in failures:
        print(f"  {row['ig']} iter={row['iter']} phase={row['phase']} status={row['status']} log={row['log']}")
PY
}

bench() {
  ensure_bin
  local stamp
  stamp="$(date +%Y%m%d-%H%M%S)"
  BENCH_ROOT="$WORK/runs/$stamp"
  RESULTS="$BENCH_ROOT/results.csv"
  export BENCH_ROOT RESULTS
  mkdir -p "$BENCH_ROOT"
  printf 'ig,iter,phase,status,seconds,packages,files,bytes,src,cache,out,log\n' >"$RESULTS"
  {
    echo "git=$(git rev-parse --short HEAD)"
    echo "bin=$BIN"
    echo "work=$WORK"
    echo "cas=$CAS"
    echo "runs=$RUNS"
    echo "offline=$OFFLINE"
    echo "started=$stamp"
  } >"$BENCH_ROOT/metadata.txt"

  local ig
  while IFS= read -r ig; do
    bench_one "$ig" || true
  done < <(selected_igs "$@")
  summarize "$RESULTS" | tee "$BENCH_ROOT/summary.txt"
}

profile_one() {
  local phase="${1:-}"
  local ig="${2:-}"
  if [ "$phase" != "materialize" ] && [ "$phase" != "build" ]; then
    usage
    return 2
  fi
  if [ -z "${SRC[$ig]:-}" ]; then
    echo "unknown IG: $ig" >&2
    return 2
  fi
  ensure_bin
  local lock
  lock="$(lock_path "$ig")"
  if [ ! -f "$lock" ]; then
    echo "missing lock for $ig; run OFFLINE=0 harness/perf31.sh prepare $ig" >&2
    return 2
  fi
  local root="$WORK/profile/$phase-$ig-$(date +%Y%m%d-%H%M%S)"
  local cache="$root/cache"
  local out="$root/out"
  local cmd=()
  mkdir -p "$root"
  if [ "$phase" = "materialize" ]; then
    rm -rf "$cache"
    cmd=("$BIN" materialize --lock "$lock" --cas "$CAS" --out "$cache")
    if [ "$OFFLINE" = "1" ]; then
      cmd+=(--offline)
    fi
  else
    "$BIN" materialize --lock "$lock" --cas "$CAS" --out "$cache" --offline >"$root/materialize.log" 2>&1 || return $?
    rm -rf "$out"
    cmd=("$BIN" build "${SRC[$ig]}" -o "$out" --cache "$cache")
  fi

  printf '%q ' "${cmd[@]}" >"$root/command.txt"
  printf '\n' >>"$root/command.txt"
  if command -v perf >/dev/null 2>&1; then
    echo "perf record -> $root/perf.data"
    perf record -g -F "$FREQ" -o "$root/perf.data" -- "${cmd[@]}" >"$root/run.log" 2>&1
    local status=$?
    perf report -i "$root/perf.data" --stdio >"$root/perf-report.txt" 2>"$root/perf-report.err" || true
    echo "status=$status"
    echo "report=$root/perf-report.txt"
    return "$status"
  fi
  echo "perf not found; running command without profiler"
  "${cmd[@]}" >"$root/run.log" 2>&1
}

cmd="${1:-}"
shift || true
case "$cmd" in
  list)
    list_igs
    ;;
  prepare)
    ensure_bin
    mkdir -p "$WORK/logs" "$WORK/locks" "$CAS"
    prep_status=0
    while IFS= read -r ig; do
      prepare_one "$ig" || prep_status=$?
    done < <(selected_igs "$@")
    exit "$prep_status"
    ;;
  bench)
    bench "$@"
    ;;
  summarize)
    summarize "${1:-$WORK/latest-results.csv}"
    ;;
  profile)
    profile_one "$@"
    ;;
  -h|--help|help|"")
    usage
    ;;
  *)
    echo "unknown command: $cmd" >&2
    usage
    exit 2
    ;;
esac

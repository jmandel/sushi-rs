#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: snapshot/check-harvested-r4.sh <harvest-dir> [pkg#ver ...]" >&2
  exit 2
}

[[ $# -ge 1 ]] || usage
HARVEST_DIR="$1"
shift
PACKAGES=("$@")
if [[ ${#PACKAGES[@]} -eq 0 ]]; then
  PACKAGES=("hl7.fhir.r4.core#4.0.1")
fi

REPO="$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)"
FHIR_CACHE="${FHIR_CACHE:-$REPO/temp/fhir-home/.fhir/packages}"
FIXTURES="$HARVEST_DIR/fixtures"
GOLDENS="$HARVEST_DIR/goldens"
ACTUALS="$HARVEST_DIR/rust"
LOGS="$HARVEST_DIR/logs"
mkdir -p "$ACTUALS" "$LOGS"

if [[ ! -d "$FIXTURES" || ! -d "$GOLDENS" ]]; then
  echo "FATAL: expected fixtures and goldens under $HARVEST_DIR" >&2
  exit 2
fi
LOCAL_DIR="$FIXTURES"
if [[ -f "$HARVEST_DIR/manifest.json" ]]; then
  manifest_resources="$(node -e "const fs=require('fs'); const p='$HARVEST_DIR/manifest.json'; const j=JSON.parse(fs.readFileSync(p,'utf8')); if (j.resourcesDir) process.stdout.write(j.resourcesDir)")"
  if [[ -n "$manifest_resources" && -d "$manifest_resources" ]]; then
    LOCAL_DIR="$manifest_resources"
  fi
fi

cargo build -p snapshot_gen >/dev/null

PACKAGE_ARGS=()
for pkg in "${PACKAGES[@]}"; do
  PACKAGE_ARGS+=(--package "$pkg")
done

total=0
ok=0
failed=0
while IFS= read -r golden; do
  total=$((total + 1))
  name="$(basename "$golden" .snapshot.json)"
  fixture="$FIXTURES/$name.json"
  actual="$ACTUALS/$name.snapshot.json"
  run_log="$LOGS/$name.rust.log"
  diff_log="$LOGS/$name.diff.log"
  if [[ ! -f "$fixture" ]]; then
    echo "FAIL missing fixture for $name" >&2
    failed=$((failed + 1))
    continue
  fi
  if "$REPO/target/debug/snapshot_gen" --native-r5 --local-dir "$LOCAL_DIR" --cache "$FHIR_CACHE" "${PACKAGE_ARGS[@]}" "$fixture" >"$actual" 2>"$run_log" \
      && node "$REPO/snapshot/diff-snapshot.cjs" "$golden" "$actual" >"$diff_log" 2>&1; then
    ok=$((ok + 1))
    echo "OK rust $name"
  else
    failed=$((failed + 1))
    echo "FAIL rust $name (see $run_log and $diff_log)" >&2
  fi
done < <(find "$GOLDENS" -maxdepth 1 -name '*.snapshot.json' | sort)

echo "R4 HARVEST CHECK: ok=$ok failed=$failed total=$total"
[[ $failed -eq 0 ]]

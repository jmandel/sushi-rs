#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: snapshot/gen-harvested-r4-goldens.sh <harvest-dir> [pkg#ver ...]" >&2
  echo "       env ORACLE_OUTPUT=native-r5|r4 (default native-r5)" >&2
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
FIXTURES="$HARVEST_DIR/fixtures"
GOLDENS="$HARVEST_DIR/goldens"
LOGS="$HARVEST_DIR/logs"
mkdir -p "$GOLDENS" "$LOGS"
ORACLE_OUTPUT="${ORACLE_OUTPUT:-native-r5}"
case "$ORACLE_OUTPUT" in
  native-r5|r5) OUTPUT_ARG="--native-r5" ;;
  r4) OUTPUT_ARG="--output-r4" ;;
  *) echo "FATAL: ORACLE_OUTPUT must be native-r5 or r4, got: $ORACLE_OUTPUT" >&2; exit 2 ;;
esac

if [[ ! -d "$FIXTURES" ]]; then
  echo "FATAL: no fixtures dir: $FIXTURES" >&2
  exit 2
fi
LOCAL_DIR="$FIXTURES"
if [[ -f "$HARVEST_DIR/manifest.json" ]]; then
  manifest_resources="$(node -e "const fs=require('fs'); const p='$HARVEST_DIR/manifest.json'; const j=JSON.parse(fs.readFileSync(p,'utf8')); if (j.resourcesDir) process.stdout.write(j.resourcesDir)")"
  if [[ -n "$manifest_resources" && -d "$manifest_resources" ]]; then
    LOCAL_DIR="$manifest_resources"
  fi
fi

if [[ "${ORACLE_BATCH:-1}" != "0" ]]; then
  batch="$LOGS/batch.tsv"
  : >"$batch"
  while IFS= read -r fixture; do
    name="$(basename "$fixture" .json)"
    out="$GOLDENS/$name.snapshot.json"
    msg="$GOLDENS/$name.snapshot.json.tmp.messages.json"
    rm -f "$out" "$msg"
    printf '%s\t%s\t%s\n' "$fixture" "$out" "$msg" >>"$batch"
  done < <(find "$FIXTURES" -maxdepth 1 -name '*.json' | sort)
  bash "$REPO/snapshot/oracle/gen-snapshot.sh" --r4 "$OUTPUT_ARG" --sort --local-dir "$LOCAL_DIR" --batch-list "$batch" "${PACKAGES[@]}" 2>&1 | tee "$LOGS/batch.oracle.log"
  exit 0
fi

total=0
ok=0
failed=0
while IFS= read -r fixture; do
  total=$((total + 1))
  name="$(basename "$fixture" .json)"
  out="$GOLDENS/$name.snapshot.json"
  tmp="$out.tmp"
  log="$LOGS/$name.oracle.log"
  msg="$GOLDENS/$name.snapshot.messages.json"
  rm -f "$tmp" "$msg"
  if bash "$REPO/snapshot/oracle/gen-snapshot.sh" --r4 "$OUTPUT_ARG" --sort --local-dir "$LOCAL_DIR" "$fixture" "$tmp" "${PACKAGES[@]}" >"$log" 2>&1; then
    mv "$tmp" "$out"
    ok=$((ok + 1))
    echo "OK oracle $name"
  else
    rm -f "$tmp"
    failed=$((failed + 1))
    echo "FAIL oracle $name (see $log)" >&2
  fi
done < <(find "$FIXTURES" -maxdepth 1 -name '*.json' | sort)

echo "R4 ORACLE GOLDENS ($ORACLE_OUTPUT): ok=$ok failed=$failed total=$total"
[[ $failed -eq 0 ]]

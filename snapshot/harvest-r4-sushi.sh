#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: snapshot/harvest-r4-sushi.sh <ig-key> <ig-dir-or-resources-dir> [pkg#ver ...]" >&2
  echo "       env LIMIT=N to cap harvested fixtures; env INCLUDE_LOCAL_BASE=1 to keep local-derived profiles" >&2
  echo "       env ORACLE_OUTPUT=native-r5|r4 controls golden output (default native-r5)" >&2
  exit 2
}

[[ $# -ge 2 ]] || usage
KEY="$1"
INPUT="$2"
shift 2
PACKAGES=("$@")
if [[ ${#PACKAGES[@]} -eq 0 ]]; then
  PACKAGES=("hl7.fhir.r4.core#4.0.1")
fi

REPO="$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)"
HARVEST_DIR="$REPO/snapshot/harvested/r4/$KEY"

if [[ -d "$INPUT" ]] && compgen -G "$INPUT/StructureDefinition-*.json" >/dev/null; then
  RESOURCES="$INPUT"
elif [[ -d "$INPUT/fsh-generated/resources" ]]; then
  RESOURCES="$INPUT/fsh-generated/resources"
elif [[ -d "$INPUT" ]]; then
  STOCK_OUT="$REPO/temp/snapshot-harvest/$KEY/sushi"
  bash "$REPO/harness/run-stock.sh" "$INPUT" "$STOCK_OUT"
  RESOURCES="$STOCK_OUT/fsh-generated/resources"
else
  echo "FATAL: input is not a directory: $INPUT" >&2
  exit 2
fi

HARVEST_ARGS=()
if [[ "${INCLUDE_LOCAL_BASE:-0}" == "1" ]]; then
  HARVEST_ARGS+=(--include-local-base)
fi
if [[ -n "${LIMIT:-}" ]]; then
  HARVEST_ARGS+=(--limit "$LIMIT")
fi

rm -rf "$HARVEST_DIR/fixtures" "$HARVEST_DIR/goldens" "$HARVEST_DIR/rust"
mkdir -p "$HARVEST_DIR"
node "$REPO/snapshot/harvest-r4-sushi.cjs" "${HARVEST_ARGS[@]}" "$RESOURCES" "$HARVEST_DIR"
bash "$REPO/snapshot/gen-harvested-r4-goldens.sh" "$HARVEST_DIR" "${PACKAGES[@]}"

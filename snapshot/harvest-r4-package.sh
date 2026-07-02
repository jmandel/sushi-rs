#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: snapshot/harvest-r4-package.sh <ig-key> <pkg#ver>" >&2
  echo "       env LIMIT=N to cap harvested fixtures; env EXCLUDE_LOCAL_BASE=1 to skip local-derived profiles" >&2
  echo "       env ORACLE_OUTPUT=native-r5|r4 controls golden output (default native-r5)" >&2
  exit 2
}

[[ $# -eq 2 ]] || usage
KEY="$1"
PACKAGE="$2"

REPO="$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)"
FHIR_CACHE="${FHIR_CACHE:-$REPO/temp/fhir-home/.fhir/packages}"
HARVEST_DIR="$REPO/snapshot/harvested/r4/$KEY"

node "$REPO/snapshot/install-fhir-package.cjs" --cache "$FHIR_CACHE" "$PACKAGE"
mapfile -t PACKAGES < <(node "$REPO/snapshot/package-deps.cjs" --cache "$FHIR_CACHE" "$PACKAGE")

HARVEST_ARGS=(--cache "$FHIR_CACHE")
if [[ "${EXCLUDE_LOCAL_BASE:-0}" == "1" ]]; then
  HARVEST_ARGS+=(--exclude-local-base)
fi
if [[ -n "${LIMIT:-}" ]]; then
  HARVEST_ARGS+=(--limit "$LIMIT")
fi

rm -rf "$HARVEST_DIR/fixtures" "$HARVEST_DIR/goldens" "$HARVEST_DIR/rust"
mkdir -p "$HARVEST_DIR"
node "$REPO/snapshot/harvest-r4-package.cjs" "${HARVEST_ARGS[@]}" "$PACKAGE" "$HARVEST_DIR"
bash "$REPO/snapshot/gen-harvested-r4-goldens.sh" "$HARVEST_DIR" "${PACKAGES[@]}"

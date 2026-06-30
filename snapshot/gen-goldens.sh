#!/usr/bin/env bash
set -euo pipefail

REPO="$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)"
OUT_DIR="$REPO/snapshot/goldens"
mkdir -p "$OUT_DIR"

for fixture in "$REPO"/snapshot/fixtures/*.json; do
  name="$(basename "$fixture" .json)"
  version_arg="--r5"
  if [[ "$name" == r4-* ]]; then
    version_arg="--r4"
  fi
  bash "$REPO/snapshot/oracle/gen-snapshot.sh" "$version_arg" --sort "$fixture" "$OUT_DIR/$name.snapshot.json"
done

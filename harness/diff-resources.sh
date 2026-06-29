#!/usr/bin/env bash
# Phase 0 byte-parity gate: compare generated resources of two SUSHI runs.
# The target is NO diff output. Per the plan, diffs must be classified, never
# silently normalized.
#
# Usage: diff-resources.sh <stock-out> <candidate-out>
set -euo pipefail

STOCK="${1:?usage: diff-resources.sh <stock-out> <candidate-out>}"
CAND="${2:?usage: diff-resources.sh <stock-out> <candidate-out>}"

A="$STOCK/fsh-generated/resources"
B="$CAND/fsh-generated/resources"

if [[ ! -d "$A" ]]; then echo "[diff] missing stock resources: $A" >&2; exit 2; fi
if [[ ! -d "$B" ]]; then echo "[diff] missing candidate resources: $B" >&2; exit 2; fi

echo "[diff] stock=$A"
echo "[diff] cand =$B"
if diff -rq "$A" "$B"; then
  echo "[diff] PARITY: byte-identical ✓"
  exit 0
else
  echo "[diff] DIVERGENCE: classify every diff above before proceeding ✗" >&2
  exit 1
fi

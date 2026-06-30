#!/usr/bin/env bash
# Instance byte-parity: diff all generated resources EXCEPT SD/VS/CS (i.e. the
# instance resources). Reports pass/fail/missing + first divergence diff.
#
# Usage: diff-instances.sh <stock-out> <cand-out>
set -uo pipefail
STOCK="${1:?}"; CAND="${2:?}"
A="$STOCK/fsh-generated/resources"; B="$CAND/fsh-generated/resources"
pass=0; fail=0; missing=0; firstfail=""
for f in "$A"/*.json; do
  bn="$(basename "$f")"
  case "$bn" in StructureDefinition-*|ValueSet-*|CodeSystem-*) continue;; esac
  if [ ! -f "$B/$bn" ]; then missing=$((missing+1)); [ -z "$firstfail" ] && firstfail="$bn (MISSING)"; continue; fi
  if diff -q "$f" "$B/$bn" >/dev/null 2>&1; then pass=$((pass+1)); else fail=$((fail+1)); [ -z "$firstfail" ] && firstfail="$bn"; fi
done
echo "[diff-instances] PASS=$pass FAIL=$fail MISSING=$missing"
if [ -n "$firstfail" ]; then
  echo "[diff-instances] first: $firstfail"
  bn="${firstfail%% *}"; [ -f "$B/$bn" ] && diff "$A/$bn" "$B/$bn" | head -40
  exit 1
fi
echo "[diff-instances] PARITY ✓"

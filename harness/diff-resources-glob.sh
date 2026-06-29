#!/usr/bin/env bash
# Per-resource byte-parity for a subset of resources (by filename glob prefix).
# Reports pass/fail counts and shows the first divergence. Target: all pass.
#
# Usage: diff-resources-glob.sh <stock-out> <cand-out> <prefix1> [prefix2 ...]
#   e.g. diff-resources-glob.sh temp/ips-stock temp/rust-ips ValueSet CodeSystem
set -euo pipefail
STOCK="${1:?}"; CAND="${2:?}"; shift 2
A="$STOCK/fsh-generated/resources"; B="$CAND/fsh-generated/resources"
prefixes=("$@"); [ ${#prefixes[@]} -eq 0 ] && prefixes=("")

pass=0; fail=0; missing=0; firstfail=""
for pfx in "${prefixes[@]}"; do
  for f in "$A/$pfx"*.json; do
    [ -e "$f" ] || continue
    base="$(basename "$f")"
    if [ ! -f "$B/$base" ]; then missing=$((missing+1)); [ -z "$firstfail" ] && firstfail="$base (MISSING in candidate)"; continue; fi
    if diff -q "$f" "$B/$base" >/dev/null 2>&1; then pass=$((pass+1));
    else fail=$((fail+1)); [ -z "$firstfail" ] && firstfail="$base"; fi
  done
done
echo "[diff-glob] prefixes='${prefixes[*]}' PASS=$pass FAIL=$fail MISSING=$missing"
if [ -n "$firstfail" ]; then
  echo "[diff-glob] first divergence: $firstfail"
  bn="${firstfail%% *}"
  [ -f "$B/$bn" ] && diff "$A/$bn" "$B/$bn" | head -40
  exit 1
fi
echo "[diff-glob] PARITY ✓"

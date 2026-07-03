#!/usr/bin/env bash
# WASM P0 byte-match gate — native side.
#
# Produces the reference hash manifest (data/native-hashes.json) that the
# browser page's exported wasm-hashes.json must match. Runs the SAME inputs the
# browser runs, with native rust_sushi + snapshot_gen:
#   - build the cycle IG                        -> hash every resource  (build/<file>)
#   - snapshot each produced profile against r5 -> hash each snapshot    (snapshot/<file>)
#
# Then, if a browser export is provided as $1 (the downloaded wasm-hashes.json),
# diffs the two manifests and reports PASS/FAIL.
#
# Usage:
#   demo/wasm-p0/check-native.sh                 # just (re)build native-hashes.json
#   demo/wasm-p0/check-native.sh wasm-hashes.json  # compare a browser export
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
cd "$REPO"

CYCLE="${CYCLE_DIR:-/home/jmandel/hobby/periodicity-impl/cycle}"
CACHE="${FHIR_CACHE:-temp/fhir-home/.fhir/packages}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

SUSHI="$REPO/target/release/rust_sushi"
SNAP="$REPO/target/release/snapshot_gen"
[ -x "$SUSHI" ] || cargo build --release --bin rust_sushi
[ -x "$SNAP" ]  || cargo build --release --bin snapshot_gen

RES="$WORK/out/fsh-generated/resources"
"$SUSHI" build "$CYCLE" -o "$WORK/out" --cache "$CACHE" >/dev/null

SNAPDIR="$WORK/snap"; mkdir -p "$SNAPDIR"
for f in "$RES"/StructureDefinition-*.json; do
  "$SNAP" --cache "$CACHE" --package hl7.fhir.r5.core#5.0.0 --local-dir "$RES" "$f" \
    > "$SNAPDIR/$(basename "$f")"
done

python3 - "$RES" "$SNAPDIR" > "$HERE/data/native-hashes.json" <<'PY'
import sys, hashlib, os, json, glob
res, snap = sys.argv[1], sys.argv[2]
h = {}
for f in glob.glob(res + "/*.json"):
    h["build/" + os.path.basename(f)] = hashlib.sha256(open(f, "rb").read()).hexdigest()
for f in glob.glob(snap + "/*.json"):
    h["snapshot/" + os.path.basename(f)] = hashlib.sha256(open(f, "rb").read()).hexdigest()
print(json.dumps(h, indent=2, sort_keys=True))
PY
echo "wrote data/native-hashes.json ($(python3 -c "import json;print(len(json.load(open('$HERE/data/native-hashes.json'))))") entries)"

if [ "${1:-}" != "" ]; then
  python3 - "$HERE/data/native-hashes.json" "$1" <<'PY'
import json, sys
n = json.load(open(sys.argv[1]))
b = json.load(open(sys.argv[2]))
nk, bk = set(n), set(b)
mism = [k for k in nk & bk if n[k] != b[k]]
print(f"native={len(nk)} browser={len(bk)} keys_match={nk==bk} mismatches={len(mism)}")
for k in sorted(nk - bk): print("  only-native:", k)
for k in sorted(bk - nk): print("  only-browser:", k)
for k in mism: print("  DIFF:", k)
ok = nk == bk and not mism
print("BYTE-MATCH GATE:", "PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
PY
fi

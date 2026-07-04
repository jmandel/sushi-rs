#!/usr/bin/env bash
# End-to-end gate (task #32 gate iv): an IG whose closure is NOT prepinned loads,
# its closure RESOLVES, its packages are FETCHED (here: bundled to .tgz standing in
# for the CDN), MOUNTED, COMPILED, and SNAPSHOTTED — all driven by the Rust
# resolver's ResolutionStep.
#
# Uses fhir-ips (deps hl7.fhir.uv.ipa#1.1.0 + hl7.fhir.uv.extensions.r4#5.3.0 — a
# closure distinct from the baked cycle 5-package set), resolved + built entirely
# from the resolver's output.
#
#   IG_DIR=<path-to-fhir-ips> FHIR_CACHE=<packages-dir> snapshot/arbitrary-ig-e2e.sh
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
CACHE="${FHIR_CACHE:-$REPO/temp/fhir-home/.fhir/packages}"
BIN="${RUST_SUSHI_BIN:-$REPO/target/release/rust_sushi}"
SNAP="${SNAPSHOT_GEN_BIN:-$REPO/target/release/snapshot_gen}"
IG_DIR="${IG_DIR:?set IG_DIR to a checked-out fhir-ips IG (sushi-config.yaml + input/fsh)}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

[ -d "$CACHE" ] || { echo "FATAL: cache not found: $CACHE"; exit 2; }
# rust_sushi still carries build/resolve/bundle for one release (dev-oracle
# surface). snapshot_gen is now the `fig` alias shim → build fig for it.
[ -x "$BIN" ]  || ( cd "$REPO" && cargo build --release -p rust_sushi >/dev/null )
[ -x "$SNAP" ] || ( cd "$REPO" && cargo build --release -p fig >/dev/null )

echo "== 1. RESOLVE the project closure (Rust resolver) =="
"$BIN" resolve --cache "$CACHE" --project "$IG_DIR" > "$WORK/step.json"
python3 - "$WORK/step.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
assert d["satisfied"], f"not satisfied; missing={d['missing']}"
print("  compile_set    :", [f"{p['package_id']}#{p['version']}" for p in d["compile_set"]])
print("  context_closure:", [f"{p['package_id']}#{p['version']}" for p in d["context_closure"]])
PY

# Extract the context closure labels (the snapshot mount set).
mapfile -t CLOSURE < <(python3 -c "import json,sys; [print(f\"{p['package_id']}#{p['version']}\") for p in json.load(open('$WORK/step.json'))['context_closure']]")

echo "== 2. FETCH: bundle the closure to .tgz (stands in for the CDN / registry) =="
"$BIN" bundle --cache "$CACHE" --out "$WORK/bundles" "${CLOSURE[@]}" >/dev/null
echo "  bundled ${#CLOSURE[@]} packages -> $WORK/bundles"
ls "$WORK/bundles"/*.tgz | wc -l | sed 's/^/  tgz count: /'

echo "== 3. MOUNT: inflate the bundles into a fresh cache (browser BundleSource analogue) =="
MOUNT="$WORK/mounted"
for tgz in "$WORK/bundles"/*.tgz; do
  label="$(basename "$tgz" .tgz)"
  dest="$MOUNT/$label/package"
  mkdir -p "$dest"
  tar -xzf "$tgz" -C "$dest"   # bundle entries are package-relative filenames
done
echo "  mounted into $MOUNT"

echo "== 4. COMPILE the IG over the resolved closure =="
FHIR_CACHE="$MOUNT" "$BIN" build "$IG_DIR" -o "$WORK/out" >/dev/null
N="$(ls "$WORK/out/fsh-generated/resources" | wc -l)"
echo "  compiled $N resources"
[ "$N" -gt 0 ] || { echo "FATAL: no resources compiled"; exit 2; }

echo "== 5. SNAPSHOT a profile using ONLY the resolved closure =="
# Pick a profile the IG produced, snapshot it against the closure packages + the
# just-compiled locals. Proves the closure is sufficient for validation-grade
# snapshotting (no reliance on the baked cycle set).
PROFILE="$(ls "$WORK/out/fsh-generated/resources"/StructureDefinition-*.json | head -1)"
PKG_ARGS=()
for l in "${CLOSURE[@]}"; do PKG_ARGS+=(--package "$l"); done
# r5.core is needed by the R5-internal walk engine for R4 bases (see packages.list).
if [ -d "$CACHE/hl7.fhir.r5.core#5.0.0" ]; then
  # copy r5.core into the mounted cache so the walk engine can read it.
  if [ ! -d "$MOUNT/hl7.fhir.r5.core#5.0.0" ]; then
    cp -al "$CACHE/hl7.fhir.r5.core#5.0.0" "$MOUNT/" 2>/dev/null || \
      cp -r "$CACHE/hl7.fhir.r5.core#5.0.0" "$MOUNT/"
  fi
  PKG_ARGS+=(--package "hl7.fhir.r5.core#5.0.0")
fi
"$SNAP" --cache "$MOUNT" "${PKG_ARGS[@]}" \
  --local-dir "$WORK/out/fsh-generated/resources" \
  "$PROFILE" > "$WORK/snap.json" 2>"$WORK/snap.err" || {
    echo "FATAL: snapshot failed"; cat "$WORK/snap.err"; exit 2; }
ELEMS="$(python3 -c "import json; d=json.load(open('$WORK/snap.json')); print(len(d.get('snapshot',{}).get('element',[])))")"
echo "  snapshotted $(basename "$PROFILE"): $ELEMS snapshot elements"
[ "$ELEMS" -gt 0 ] || { echo "FATAL: empty snapshot"; exit 2; }

echo "== PASS: arbitrary IG resolved -> fetched -> mounted -> compiled ($N) -> snapshotted ($ELEMS elems) =="

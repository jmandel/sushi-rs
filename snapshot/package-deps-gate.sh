#!/usr/bin/env bash
# DRY gate (task #32): the transitive R4 context closure produced by the native
# Rust resolver (`rust_sushi resolve --root`) MUST equal the Node fallback in
# snapshot/package-deps.cjs, byte-for-byte, on a set of published IGs. A drift
# here means the two implementations diverged — fix the Node fallback (Rust is
# the source of truth) until they agree.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
CACHE="${FHIR_CACHE:-$REPO/temp/fhir-home/.fhir/packages}"
BIN="${RUST_SUSHI_BIN:-$REPO/target/release/rust_sushi}"
CJS="$REPO/snapshot/package-deps.cjs"

[ -d "$CACHE" ] || { echo "FATAL: cache not found: $CACHE (set FHIR_CACHE)"; exit 2; }
if [ ! -x "$BIN" ]; then
  echo "[package-deps-gate] building rust_sushi --release"
  ( cd "$REPO" && cargo build --release -p rust_sushi >/dev/null )
fi

# Published IGs present in the isolated cache with non-trivial closures.
IGS=(
  hl7.fhir.uv.ips#1.1.0
  hl7.fhir.us.davinci-dtr#2.1.0
  hl7.fhir.us.core#7.0.0
  hl7.fhir.us.pacio-toc#1.0.0
  hl7.fhir.us.qicore#6.0.0
  hl7.fhir.be.vaccination#1.1.3
  hl7.fhir.us.carin-bb#2.1.0
  hl7.fhir.uv.sdc#4.0.0
)

pass=0; fail=0
for root in "${IGS[@]}"; do
  # Rust native (the source of truth).
  rust_out="$("$BIN" resolve --cache "$CACHE" --root "$root")"
  # Node FALLBACK path: force it by pointing the shim at a non-existent binary,
  # so we compare the retained JS algorithm (not the shim re-invoking Rust).
  cjs_out="$(RUST_SUSHI_BIN=/nonexistent/rust_sushi node "$CJS" --cache "$CACHE" "$root")"
  if [ "$rust_out" = "$cjs_out" ]; then
    n="$(printf '%s\n' "$rust_out" | grep -c .)"
    echo "PASS $root ($n pkgs)"; pass=$((pass+1))
  else
    echo "FAIL $root"; diff <(printf '%s' "$cjs_out") <(printf '%s' "$rust_out") | head -20
    fail=$((fail+1))
  fi
done

echo "=== package-deps DRY parity: $pass pass, $fail fail ==="
[ "$fail" -eq 0 ]

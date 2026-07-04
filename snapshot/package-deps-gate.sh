#!/usr/bin/env bash
# Regression gate for the package-deps.cjs SHIM WIRING (task #32; Node fallback
# retired in Consolidation Pass 1). There is now ONE resolver — Rust
# (`rust_sushi resolve --root`); package-deps.cjs is a pure shim over it. This
# gate asserts, on a set of published IGs, that the SHIM's stdout is byte-for-byte
# identical to a DIRECT `rust_sushi resolve` invocation — i.e. the shim's arg
# parsing / cache-path resolution / stdout passthrough is intact (no swallowed
# lines, no reordering, no trailing-newline drift).
#
# (Before the fallback was deleted, this compared the retained Node algorithm to
# Rust; that parity soaked green across #32. It now guards the wiring instead.)
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
  # Direct native resolver (the source of truth).
  rust_out="$(RUST_SUSHI_BIN="$BIN" "$BIN" resolve --cache "$CACHE" --root "$root")"
  # The shim path: node package-deps.cjs (which shells out to the same binary).
  shim_out="$(RUST_SUSHI_BIN="$BIN" node "$CJS" --cache "$CACHE" "$root")"
  if [ "$rust_out" = "$shim_out" ]; then
    n="$(printf '%s\n' "$rust_out" | grep -c .)"
    echo "PASS $root ($n pkgs)"; pass=$((pass+1))
  else
    echo "FAIL $root"; diff <(printf '%s' "$shim_out") <(printf '%s' "$rust_out") | head -20
    fail=$((fail+1))
  fi
done

echo "=== package-deps shim-wiring parity: $pass pass, $fail fail ==="
[ "$fail" -eq 0 ]

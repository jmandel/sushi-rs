#!/usr/bin/env bash
# liquid-diff.sh — differential gate for render_liquid (F1c).
#
# Renders each fixture / corpus page through BOTH the Ruby oracle
# (scripts/liquid-oracle.rb = Jekyll 4.4.1 / Liquid 4.0.4) and the Rust engine
# (crates/render_liquid render bin), then diffs byte-for-byte.
#
# Usage:
#   liquid-diff.sh fixtures         # synthetic per-construct fixtures
#   liquid-diff.sh corpus           # corpus liquid-bearing pages (mock context)
#   liquid-diff.sh one T C [I] [D]  # single: template, context, includes, data
#
# Exit 0 iff every comparison is byte-identical. Prints a per-case PASS/DIFF
# and a summary. DIFFs dump a unified diff.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ORACLE="$ROOT/scripts/liquid-oracle.rb"
RENDER="${LIQUID_RENDER_BIN:-$ROOT/crates/render_liquid/target/debug/render}"
[ -x "$RENDER" ] || { echo "render_liquid binary not found: $RENDER (build it or set LIQUID_RENDER_BIN)" >&2; exit 2; }
FIX="$ROOT/crates/render_liquid/tests/fixtures"

pass=0; diff=0; err=0
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

run_one() {
  local tpl="$1" ctx="$2" inc="${3:-}" data="${4:-}" quirk="${5:-}"
  local name; name="$(basename "$tpl")"
  local args=(--template "$tpl")
  [ -n "$ctx" ] && args+=(--context "$ctx")
  [ -n "$inc" ] && args+=(--includes-dir "$inc")
  [ -n "$data" ] && args+=(--data-dir "$data")
  [ -n "$quirk" ] && args+=(--publisher-raw-quirk)

  ruby "$ORACLE" "${args[@]}" > "$tmp/oracle.out" 2> "$tmp/oracle.err"
  local orc=$?
  "$RENDER" "${args[@]}" > "$tmp/rust.out" 2> "$tmp/rust.err"

  if [ $orc -ne 0 ] && [ ! -s "$tmp/oracle.out" ]; then
    echo "  ERR   $name (oracle rc=$orc: $(head -1 "$tmp/oracle.err"))"
    err=$((err+1)); return
  fi
  if diff -q "$tmp/oracle.out" "$tmp/rust.out" >/dev/null; then
    pass=$((pass+1))
    [ "${VERBOSE:-}" = "1" ] && echo "  PASS  $name"
  else
    diff=$((diff+1))
    echo "  DIFF  $name"
    if [ "${SHOW:-1}" = "1" ]; then
      diff "$tmp/oracle.out" "$tmp/rust.out" | sed 's/^/        /' | head -30
    fi
  fi
}

case "${1:-fixtures}" in
  one)
    run_one "$2" "$3" "${4:-}" "${5:-}" "${6:-}"
    ;;
  fixtures)
    echo "== synthetic fixtures =="
    for tpl in "$FIX"/*.liquid; do
      [ -e "$tpl" ] || continue
      ctx="${tpl%.liquid}.json"
      [ -e "$ctx" ] || ctx=""
      run_one "$tpl" "$ctx" "$FIX/_includes"
    done
    ;;
  corpus)
    # corpus manifest: lines of  TEMPLATE<TAB>CONTEXT<TAB>INCLUDES_DIR<TAB>DATA_DIR
    manifest="$FIX/../corpus/manifest.tsv"
    echo "== corpus pages =="
    while IFS=$'\t' read -r tpl ctx inc data; do
      [ -z "$tpl" ] && continue
      case "$tpl" in \#*) continue;; esac
      run_one "$tpl" "$ctx" "$inc" "$data"
    done < "$manifest"
    ;;
esac

echo "----"
echo "PASS=$pass DIFF=$diff ERR=$err"
# A residual DIFF/ERR is only ACCEPTED on the gate if it is an explained
# out-of-scope class (see docs / the classifier below). The `one` and
# `fixtures`/`corpus` callers treat any DIFF/ERR as a failure; the corpus gate's
# EXPLAINED accounting is done by the harness that invokes this per-file and
# classifies (see scripts/liquid-classify.sh / report). Here we just exit
# non-zero on any residual so CI is honest.
[ $diff -eq 0 ] && [ $err -eq 0 ]

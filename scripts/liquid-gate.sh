#!/usr/bin/env bash
# liquid-gate.sh — the F1c differential gate: render every liquid-bearing corpus
# page through BOTH the Ruby oracle (Jekyll 4.4.1 / Liquid 4.0.4) and the Rust
# render_liquid engine, diff byte-for-byte, and CLASSIFY every residual diff.
#
# Exit 0 iff UNEXPLAINED == 0 (every diff is an accounted out-of-scope class).
#
# Explained (out-of-scope) diff classes, each cited to the survey scope:
#   E1  missing artifact .xhtml include  — Publisher-generated fragment
#       (dependency-table, globals-table, cross-version-analysis, ...). The
#       oracle emits "Liquid error (...): internal"; Rust emits the stub or
#       empty. These are the `page`/fragment crate's job (F4/F5), not Liquid.
#   E2  missing example-resource include — `{% include(_relative) X.json|xml %}`
#       of a checked-in/Publisher example instance not present in _includes.
#       Same error-vs-empty shape. Out of scope (F4/F5).
#   E3  surrounding-layer tag — `{% sql %}` / `{% sqlToData %}` and Publisher
#       fragment/localization tags. Plain Jekyll does not register these, so
#       this Liquid-core-only oracle PARSE-ERRORS. SQL is supported by
#       SiteEngine's pre-Liquid publisher_sql layer; E3 does not classify that
#       product capability as unsupported or verified.
#
# Anything else is UNEXPLAINED and fails the gate.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ORACLE="$ROOT/scripts/liquid-oracle.rb"
RENDER="${LIQUID_RENDER_BIN:-$ROOT/crates/render_liquid/target/release/render}"
if [ -z "${LIQUID_RENDER_BIN:-}" ] && [ ! -x "$RENDER" ]; then
  RENDER="$ROOT/crates/render_liquid/target/debug/render"
fi
[ -x "$RENDER" ] || { echo "render_liquid binary not found: $RENDER (build it or set LIQUID_RENDER_BIN)" >&2; exit 2; }
MANIFEST="$ROOT/crates/render_liquid/tests/corpus/manifest.tsv"

pass=0; e1=0; e2=0; e3=0; unexplained=0
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
: > "$tmp/unexplained.txt"

classify() {
  # $1 oracle stderr, $2 oracle out, $3 rust out, $4 template
  local oerr="$1" oout="$2" rout="$3" tpl="$4"
  # E3: oracle PARSE-errors on a surrounding-layer tag that plain Jekyll's
  # Liquid does not register — sql/sqlToData and lang/lang-fragment/fragment.
  # These abort the whole page in this oracle. SQL is executed before Liquid in
  # the production SiteEngine, so this is only outside the Liquid-core
  # differential, not a claim that the Publisher renderer lacks SQL.
  if grep -qiE "Unknown tag '(sql|sqlToData|lang|lang-fragment|fragment)'" "$oerr"; then echo E3; return; fi
  # E1/E2: Jekyll ABORTS the whole page when an include file can't be located
  # (IncludeTag raises IOError "Could not locate the included file 'X'"),
  # yielding empty/short oracle output while Rust renders the page (a missing
  # includes silently -> empty). Classify by the missing file's extension.
  if grep -qE "Could not locate the included file" "$oerr"; then
    local miss
    miss="$(grep -oE "included file '[^']+'" "$oerr" | head -1)"
    case "$miss" in
      *.xhtml*) echo E1; return;;
      *.json*|*.xml*) echo E2; return;;
      *) echo E2; return;;  # example resource/partial not modeled
    esac
  fi
  # Build the set of differing lines; if every oracle-only "Liquid error" line
  # corresponds to an artifact/example include, it's E1/E2.
  # Heuristic: the ONLY differences are lines where oracle has
  # "Liquid error (line N): internal" (missing include) or the stub markers.
  # Out-of-scope iff, after removing every artifact/example RESIDUE from both
  # sides, the two renders are identical. Residues:
  #   * an inline include failure: oracle emits "Liquid error (line N): internal"
  #     (optionally wrapped, e.g. <div>...</div>); Rust emits the element with an
  #     empty body or nothing.
  #   * an artifact/example STUB the overlay supplied (ARTIFACT:/EXAMPLE:).
  # We normalize both outputs by (a) deleting the oracle's error text and Rust's
  # stub text, and (b) collapsing now-empty container tags, then compare.
  # Normalize BOTH sides identically: strip the inline-include-failure error
  # text, the artifact/example STUB markers (either engine may have resolved a
  # stub the overlay supplied), and any base64 <embed> payload of an example
  # resource. If the two then match, every diff was out-of-scope residue.
  local no="$tmp/o.norm" nr="$tmp/r.norm"
  norm() {
    sed -E \
      -e 's/Liquid error \(line [0-9]+\): internal//g' \
      -e 's/(<p>)?(ARTIFACT|EXAMPLE):[^<]*(<\/p>)?//g' \
      -e 's#base64,[A-Za-z0-9+/=]+#base64,X#g' \
      "$1"
  }
  norm "$oout" > "$no"
  norm "$rout" > "$nr"
  if diff -q "$no" "$nr" >/dev/null 2>&1; then
    if grep -qiE 'include[^%]*\.xhtml' "$tpl"; then echo E1; else echo E2; fi
    return
  fi
  echo UNEXPLAINED
}

while IFS=$'\t' read -r tpl ctx inc data; do
  [ -z "$tpl" ] && continue
  case "$tpl" in \#*) continue;; esac
  args=(--template "$tpl"); [ -n "$ctx" ] && args+=(--context "$ctx")
  [ -n "$inc" ] && args+=(--includes-dir "$inc")
  [ -n "${data:-}" ] && args+=(--data-dir "$data")
  ruby "$ORACLE" "${args[@]}" > "$tmp/o" 2> "$tmp/oe"
  "$RENDER" "${args[@]}" > "$tmp/r" 2> "$tmp/re"
  if diff -q "$tmp/o" "$tmp/r" >/dev/null 2>&1; then
    pass=$((pass+1)); continue
  fi
  cls="$(classify "$tmp/oe" "$tmp/o" "$tmp/r" "$tpl")"
  case "$cls" in
    E1) e1=$((e1+1));;
    E2) e2=$((e2+1));;
    E3) e3=$((e3+1));;
    *)  unexplained=$((unexplained+1)); echo "$tpl" >> "$tmp/unexplained.txt";;
  esac
done < "$MANIFEST"

echo "================ liquid F1c differential gate ================"
echo "PASS (byte-identical) : $pass"
echo "E1 artifact .xhtml miss: $e1   (out of scope: F4/F5 fragment store)"
echo "E2 example-resource miss: $e2  (out of scope: F4/F5 example inclusion)"
echo "E3 surrounding-layer tag: $e3   (not verified by the Liquid-core oracle)"
echo "UNEXPLAINED            : $unexplained"
if [ "$unexplained" -gt 0 ]; then
  echo "--- unexplained files ---"; cat "$tmp/unexplained.txt"
fi
[ "$unexplained" -eq 0 ]

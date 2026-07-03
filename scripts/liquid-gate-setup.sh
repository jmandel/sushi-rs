#!/usr/bin/env bash
# liquid-gate-setup.sh — materialize the differential-gate corpus for
# render_liquid (F1c): per-IG merged render contexts (site.data via Jekyll's
# exact CSV/YAML coercion + a small mock of the always-present site.data.fhir /
# page surfaces), include-dir overlays (real includes + deterministic stubs for
# Publisher-generated artifact `.xhtml` includes), and a manifest.tsv that
# `liquid-diff.sh corpus` consumes.
#
# The corpus INPUTS are READ-ONLY (the staged IG source trees under
# temp/{top20,holdout}); everything generated here lands under
# crates/render_liquid/tests/corpus/gen/ so the gate is reproducible.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CORPUS="$ROOT/crates/render_liquid/tests/corpus"
GEN="$CORPUS/gen"
TEMP="${LIQUID_CORPUS_ROOT:-/home/jmandel/hobby/sushi-rs/temp}"
MOCK="$CORPUS/mock-ctx.json"

rm -rf "$GEN"; mkdir -p "$GEN/ctx" "$GEN/inc"
: > "$CORPUS/manifest.tsv"

# The Publisher-generated artifact includes we stub (not present in repos):
ARTIFACTS="cross-version-analysis dependency-table expansion-params globals-table \
ip-statements list-capabilitystatements list-requirements \
list-simple-operationdefinitions summary-observations table-profiles \
table-valuesets table-codesystems dependency-table-short globals-table-list"

build_ig() {
  local root="$1" tag="$2"
  [ -d "$root" ] || return 0
  local inc="$root/input/includes"
  local data="$root/input/data"

  # context
  local ctx="$GEN/ctx/$tag.json"
  local ndata=0
  if [ -d "$data" ]; then
    ndata=$(find "$data" -maxdepth 1 -type f \
      \( -name '*.csv' -o -name '*.tsv' -o -name '*.yml' -o -name '*.yaml' -o -name '*.json' \) \
      | wc -l)
  fi
  if [ "$ndata" -gt 0 ]; then
    ruby "$ROOT/scripts/liquid-build-context.rb" --data-dir "$data" --base "$MOCK" > "$ctx"
  else
    cp "$MOCK" "$ctx"
  fi

  # include overlay (symlink real includes incl. subdirs + artifact stubs)
  local ovl="$GEN/inc/$tag"
  if [ -d "$inc" ]; then
    mkdir -p "$ovl"
    (cd "$inc" && find . -mindepth 1 -maxdepth 1 -print0) | while IFS= read -r -d '' e; do
      ln -sf "$inc/${e#./}" "$ovl/${e#./}"
    done
    for a in $ARTIFACTS; do
      [ -e "$ovl/$a.xhtml" ] || printf '<p>ARTIFACT:%s</p>' "$a" > "$ovl/$a.xhtml"
    done
  else
    ovl=""
  fi

  # emit manifest rows for every liquid-bearing authored page + include
  local subs="pagecontent pages intro-notes includes"
  for sub in $subs; do
    local dir="$root/input/$sub"
    [ -d "$dir" ] || continue
    while IFS= read -r -d '' f; do
      # only files with liquid
      grep -qE '\{%|\{\{' "$f" || continue
      printf '%s\t%s\t%s\t\n' "$f" "$ctx" "$ovl" >> "$CORPUS/manifest.tsv"
    done < <(find "$dir" -type f \( -name '*.md' -o -name '*.xml' -o -name '*.html' \) -print0)
  done
}

# US Core (the T2 heart) + the other IGs that carry T2 constructs.
build_ig "$TEMP/top20/uscore"   uscore
build_ig "$TEMP/top20/aucore"   aucore
build_ig "$TEMP/top20/cdex"     cdex
build_ig "$TEMP/top20/ipa"      ipa
build_ig "$TEMP/top20/pdex"     pdex
build_ig "$TEMP/top20/plannet"  plannet
build_ig "$TEMP/top20/smart"    smart
build_ig "$TEMP/top20/bulk"     bulk
build_ig "$TEMP/holdout/pas"    pas
build_ig "$TEMP/holdout/genomics" genomics

echo "manifest rows: $(wc -l < "$CORPUS/manifest.tsv")"
echo "contexts: $(ls "$GEN/ctx" | wc -l)  overlays: $(ls "$GEN/inc" | wc -l)"

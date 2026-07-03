#!/usr/bin/env bash
#
# harvest-render-goldens.sh — build the GOLDEN CORPUS of real IG Publisher
# fragment/page outputs for the Rust fragment/page renderer (plan task F0).
#
# For each IG it:
#   1. copies the (read-only) IG source into an isolated build dir,
#   2. runs the pinned IG Publisher jar (2.2.10) with an ISOLATED HOME so the
#      real ~/.fhir cache is never touched (tx.fhir.org is the authoritative
#      terminology path and IS contacted),
#   3. harvests temp/pages/_includes/*.xhtml (the fragment corpus, EXCLUDING
#      the -en language duplicates) into render-goldens/<ig>/fragments/,
#   4. harvests the final rendered page HTML (output/*.html) into
#      render-goldens/<ig>/pages/ if Jekyll produced them,
#   5. writes render-goldens/<ig>/PIN.md provenance.
#
# The fragment corpus is the PRIMARY deliverable and does not require Jekyll.
# Page goldens require Jekyll; if the publisher's Jekyll step fails, fragments
# are still harvested and the page harvest is skipped (documented in PIN.md).
#
# PERFORMANCE NOTE (measured 2026-07-03): the dominant cost on a COLD cache is
# the publisher's cross-version comparison. For IGs with a long path-history
# (US Core: ~8 prior versions; plan-net: several), the publisher downloads AND
# SQLite-indexes every prior version's terminology deps — us.nlm.vsac#* packages
# take 4-9 MINUTES EACH to index, and there are many. plan-net finished in ~42
# min; US Core's comparison chain is far deeper (it walked back through
# vsac 0.24→0.3 etc.), pushing well past 60 min. This is NOT a hang — the JVM is
# RUNNABLE in org.sqlite.core.NativeDB.step throughout. The comparison produces
# the `comparison-v*` fragments/pages, which are explicitly OUT OF SCOPE for the
# Rust renderer (cross-version-analysis is "not derivable from package.db").
# To harvest the primary fragment corpus faster, the comparison can be disabled
# by removing the prior-version entries from the IG's package-list.json /
# path-history parameters BEFORE the run (do NOT edit the pristine survey copy;
# edit only the $BUILD copy). Left ON here to keep goldens faithful to a real
# stock run; the preserved isolated cache (see below) makes re-runs fast.
#
# Usage:
#   harvest-render-goldens.sh <ig-slug>
# where <ig-slug> is one of: us-core sdc plan-net
#
# Environment (all have sane defaults):
#   PUBLISHER_JAR   path to publisher.jar (default: cycle input-cache 2.2.10)
#   SCRATCH         scratchpad root for isolated builds
#   GOLDENS_DIR     destination render-goldens/ dir (default: repo render-goldens)
#   MAX_FRAGMENT_BYTES  fragments larger than this are excluded + logged (default 1048576 = 1MiB).
#     Rationale: the only fragments that blow past this are the *-{xml,json,ttl}-html
#     full-instance payload dumps (a serialized example resource echoed as
#     syntax-highlighted HTML) — low renderer value and size-pathological. The
#     large ANALYSIS fragments the renderer must reproduce (snapshot-all, dict,
#     maps, expansion) are all well under 1MiB and ARE kept. A 100KiB cap would
#     wrongly drop those, so 1MiB is the justified line.
#
# This script is repeatable: it wipes and recreates the per-IG build dir and
# the per-IG render-goldens/<ig> output on each run.

set -uo pipefail

SLUG="${1:-}"
if [[ -z "$SLUG" ]]; then
  echo "usage: $0 <us-core|sdc|plan-net>" >&2
  exit 2
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRATCH="${SCRATCH:-/tmp/claude-1000/-home-jmandel-hobby/33fc8265-3f9a-4a4b-8eaf-39a38ad53b3d/scratchpad}"
PUBLISHER_JAR="${PUBLISHER_JAR:-/home/jmandel/hobby/periodicity-impl/cycle/input-cache/publisher.jar}"
GOLDENS_DIR="${GOLDENS_DIR:-$REPO_ROOT/render-goldens}"
MAX_FRAGMENT_BYTES="${MAX_FRAGMENT_BYTES:-1048576}"
IG_SURVEY="$SCRATCH/ig-survey"
IG_SURVEY_MINE="$SCRATCH/ig-survey-mine"
# Build scratch defaults to a DISK-backed location (NOT the small tmpfs
# scratchpad): the IG Publisher's cross-version comparison packages and package
# cache are large (US Core alone unpacks ~8 historical comparison packages).
# Override with BUILD_ROOT= if needed. Kept OUTSIDE the repo so it is never
# committed. IMPORTANT: the path MUST be normalized (no "..") — the publisher's
# PublisherIGLoader.loadPrePages does path-substring arithmetic that throws
# StringIndexOutOfBoundsException when the build dir path contains "..".
BUILD_ROOT="$(realpath -m "${BUILD_ROOT:-$REPO_ROOT/../sushi-rs-snapshot-f0-builds}")"

# Map slug -> read-only source dir, publisher target (ig.ini path relative to src)
case "$SLUG" in
  us-core)  SRC="$IG_SURVEY_MINE/US-Core"; REMOTE="https://github.com/HL7/US-Core.git" ;;
  sdc)      SRC="$IG_SURVEY/sdc"; REMOTE="https://github.com/HL7/sdc.git" ;;
  plan-net) SRC="$IG_SURVEY_MINE/davinci-pdex-plan-net"; REMOTE="https://github.com/HL7/davinci-pdex-plan-net.git" ;;
  *) echo "unknown ig slug: $SLUG" >&2; exit 2 ;;
esac

if [[ ! -d "$SRC" ]]; then
  echo "source dir not found: $SRC" >&2
  exit 2
fi

BUILD="$BUILD_ROOT/$SLUG"
HOME_ISO="$BUILD/.home"
OUT="$GOLDENS_DIR/$SLUG"

echo "=== F0 harvest: $SLUG ==="
echo "src        : $SRC"
echo "build      : $BUILD"
echo "isolated H : $HOME_ISO"
echo "publisher  : $PUBLISHER_JAR"
echo "goldens    : $OUT"

# 1. Fresh copy of the source (leave the read-only survey dir untouched).
#    Preserve a previously-populated isolated FHIR package cache across re-runs
#    (cold-cache package downloads for US Core take ~15 min). If a saved cache
#    exists at $BUILD_ROOT/.cache-keep/<slug>-home, restore it into the fresh
#    build; otherwise start with an empty isolated home (still never touches
#    the real ~/.fhir).
CACHE_KEEP="$BUILD_ROOT/.cache-keep/$SLUG-home"
SAVED_HOME=""
if [[ -d "$HOME_ISO/.fhir/packages" ]]; then
  SAVED_HOME="$BUILD/.home.saved"
  rm -rf "$SAVED_HOME"; mv "$HOME_ISO" "$SAVED_HOME"
elif [[ -d "$CACHE_KEEP/.fhir/packages" ]]; then
  SAVED_HOME="$CACHE_KEEP"
fi
rm -rf "$BUILD"
mkdir -p "$BUILD"
if [[ -n "$SAVED_HOME" && -d "$SAVED_HOME" ]]; then
  mv "$SAVED_HOME" "$HOME_ISO"
  echo "restored isolated FHIR cache from $SAVED_HOME"
else
  mkdir -p "$HOME_ISO"
fi
# Copy source but drop any pre-existing publisher scratch to force a clean run.
cp -a "$SRC/." "$BUILD/"
rm -rf "$BUILD/temp" "$BUILD/output" "$BUILD/template" "$BUILD/input-cache/txcache"

# Optional: strip cross-version comparison from the BUILD copy (never the
# pristine survey). Set NO_COMPARISON=1 to remove version-comparison /
# ipa-comparison / ips-comparison params from every IG JSON under the build.
# The comparison produces only the out-of-scope `comparison-v*` fragments/pages
# but multiplies build time 5-10x (deep VSAC index chain). Off by default to
# keep goldens faithful to a real stock run.
if [[ "${NO_COMPARISON:-0}" == "1" ]]; then
  echo "NO_COMPARISON=1 → stripping version/ipa/ips-comparison params from build IG JSONs"
  while IFS= read -r -d '' igjson; do
    python3 - "$igjson" <<'PY'
import json, sys
p = sys.argv[1]
try:
    d = json.load(open(p))
except Exception:
    sys.exit(0)
defn = d.get("definition")
if not isinstance(defn, dict): sys.exit(0)
params = defn.get("parameter")
if not isinstance(params, list): sys.exit(0)
STRIP = {"version-comparison", "version-comparison-master", "ipa-comparison", "ips-comparison"}
def code_of(pr):
    c = pr.get("code")
    return c.get("code") if isinstance(c, dict) else c
kept = [pr for pr in params if code_of(pr) not in STRIP]
if len(kept) != len(params):
    defn["parameter"] = kept
    json.dump(d, open(p, "w"), indent=2)
    print("  stripped %d comparison params from %s" % (len(params)-len(kept), p))
PY
  done < <(find "$BUILD" -maxdepth 3 -name 'ImplementationGuide-*.json' -print0 2>/dev/null)
fi

SRC_COMMIT="$(git -C "$SRC" rev-parse HEAD 2>/dev/null || echo unknown)"
SRC_DATE="$(git -C "$SRC" log -1 --format=%ci 2>/dev/null || echo unknown)"

# 2. Run the publisher with an isolated HOME. tx.fhir.org allowed.
#    -ig <dir> lets the publisher find ig.ini in the build dir.
#    The publisher resolves the FHIR package cache / tx cache via the JVM
#    "user.home" property, NOT $HOME, so we set BOTH to the isolated home to
#    guarantee the real ~/.fhir is never read or written.
CMD=(java -Xmx6g "-Duser.home=$HOME_ISO" -jar "$PUBLISHER_JAR" -ig "$BUILD")
echo "+ HOME=$HOME_ISO user.home=$HOME_ISO ${CMD[*]}"
RUN_START=$(date +%s)
HOME="$HOME_ISO" XDG_CACHE_HOME="$HOME_ISO/.cache" "${CMD[@]}" \
  > "$BUILD/publisher.stdout.log" 2>&1
PUB_RC=$?
RUN_END=$(date +%s)
RUN_SECS=$((RUN_END - RUN_START))
echo "publisher exit=$PUB_RC in ${RUN_SECS}s"

INCLUDES="$BUILD/temp/pages/_includes"
if [[ ! -d "$INCLUDES" ]]; then
  echo "FATAL: no fragments produced ($INCLUDES missing). See $BUILD/publisher.stdout.log" >&2
  tail -40 "$BUILD/publisher.stdout.log" >&2
  exit 1
fi

# 3. Harvest fragments: base *.xhtml only (exclude -en language dupes), skip
#    outliers over MAX_FRAGMENT_BYTES, and (unless KEEP_PAYLOAD_DUMPS=1) skip the
#    -{xml,json,ttl}-html fragments. Those are NOT analysis views — they are the
#    source resource serialized and echoed as syntax-highlighted HTML (a
#    prism/pre block). The Rust renderer's job is the DERIVED views (tables,
#    dicts, expansions, narrative); the payload dumps are trivially reproducible
#    from the resource JSON/XML/TTL and are excluded to keep the corpus under the
#    committed-size budget. Set KEEP_PAYLOAD_DUMPS=1 to include them.
rm -rf "$OUT"
mkdir -p "$OUT/fragments"
EXCL_LOG="$OUT/excluded-fragments.txt"
: > "$EXCL_LOG"
frag_total=0
frag_kept=0
frag_en=0
frag_big=0
frag_payload=0
while IFS= read -r -d '' f; do
  base="$(basename "$f")"
  frag_total=$((frag_total + 1))
  # exclude -en language duplicates: <name>-en.xhtml
  if [[ "$base" == *-en.xhtml ]]; then
    frag_en=$((frag_en + 1))
    continue
  fi
  # exclude serialized-resource payload dumps (xml-html/json-html/ttl-html)
  if [[ "${KEEP_PAYLOAD_DUMPS:-0}" != "1" && ( "$base" == *-xml-html.xhtml || "$base" == *-json-html.xhtml || "$base" == *-ttl-html.xhtml ) ]]; then
    frag_payload=$((frag_payload + 1))
    continue
  fi
  sz=$(stat -c%s "$f")
  if (( sz > MAX_FRAGMENT_BYTES )); then
    frag_big=$((frag_big + 1))
    echo "$base	$sz" >> "$EXCL_LOG"
    continue
  fi
  cp "$f" "$OUT/fragments/$base"
  frag_kept=$((frag_kept + 1))
done < <(find "$INCLUDES" -maxdepth 1 -name '*.xhtml' -print0)

echo "fragments: total=$frag_total kept=$frag_kept excluded_en=$frag_en excluded_payloaddump=$frag_payload excluded_big=$frag_big"

# 4. Harvest page HTML if Jekyll produced final output.
#    Pages are the F5 (page-parity) golden set; they are large (full IG chrome
#    inlined per page). To respect the committed-size budget, page harvest is
#    OPT-IN via HARVEST_PAGES=1 and always excludes the qa*.html reports (the
#    HTMLInspector output — qa-tx.html alone is ~100MB and is not an IG page).
JEKYLL_OK=no
page_count=0
if [[ -d "$BUILD/output" ]]; then
  jekyll_total=$(find "$BUILD/output" -maxdepth 1 -name '*.html' | wc -l)
  if (( jekyll_total > 0 )); then
    JEKYLL_OK=yes
    if [[ "${HARVEST_PAGES:-0}" == "1" ]]; then
      mkdir -p "$OUT/pages"
      while IFS= read -r -d '' p; do
        pb="$(basename "$p")"
        [[ "$pb" == qa*.html ]] && continue   # skip HTMLInspector QA reports
        cp "$p" "$OUT/pages/$pb"
        page_count=$((page_count + 1))
      done < <(find "$BUILD/output" -maxdepth 1 -name '*.html' -print0)
    fi
  fi
fi
echo "pages: jekyll_ok=$JEKYLL_OK harvested=$page_count (HARVEST_PAGES=${HARVEST_PAGES:-0})"

# 5. Provenance PIN.md
PUB_LINE="$(grep -m1 'FHIR IG Publisher Version' "$BUILD/publisher.stdout.log" 2>/dev/null || echo 'unknown')"
frag_du="$(du -sh "$OUT/fragments" 2>/dev/null | cut -f1)"
page_du="$(du -sh "$OUT/pages" 2>/dev/null | cut -f1 || echo 0)"
cat > "$OUT/PIN.md" <<EOF
# Render golden PIN — $SLUG

- **IG source repo**: $REMOTE
- **Source commit**: $SRC_COMMIT
- **Source commit date**: $SRC_DATE
- **Publisher jar**: $PUBLISHER_JAR
- **Publisher version line**: $PUB_LINE
- **Terminology server**: http://tx.fhir.org (authoritative, contacted live)
- **Isolated HOME**: yes (never touched real ~/.fhir; -Duser.home + \$HOME both set to isolated)
- **Harvest date**: $(date -u +%Y-%m-%dT%H:%M:%SZ)
- **Command**: \`HOME=<isolated> java -Xmx6g -Duser.home=<isolated> -jar publisher.jar -ig <build-dir>\`
- **Cross-version comparison**: $([[ "${NO_COMPARISON:-0}" == "1" ]] && echo "DISABLED (version/ipa/ips-comparison params stripped from build IG JSON — the out-of-scope comparison-v* fragments/pages are intentionally ABSENT; done to avoid the multi-hour VSAC index chain)" || echo "enabled (stock)")
- **Publisher exit code**: $PUB_RC
- **Publisher wall time**: ${RUN_SECS}s

## Corpus
- Fragments (base *.xhtml): **$frag_kept** kept ($frag_du)
  - total emitted: $frag_total; -en language dupes excluded: $frag_en;
    payload-dump (-{xml,json,ttl}-html) excluded: $frag_payload; >${MAX_FRAGMENT_BYTES}B excluded: $frag_big
- Pages (output/*.html, qa*.html excluded): **$page_count** ($page_du), jekyll_ok=$JEKYLL_OK, HARVEST_PAGES=${HARVEST_PAGES:-0}

Excluded oversized fragments (if any) listed in \`excluded-fragments.txt\`.
Payload-dump fragments are excluded by policy (serialized-resource echoes, not
analysis views); pages are opt-in (HARVEST_PAGES=1) and re-derivable via the
repeatable harvest script.
EOF

echo "=== done $SLUG: fragments=$frag_kept pages=$page_count jekyll=$JEKYLL_OK (${RUN_SECS}s) ==="

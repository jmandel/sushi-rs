#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: snapshot/oracle/gen-snapshot.sh [--r4|--r5] [--sort|--direct] [--native-r5|--output-r5|--output-r4] [--local-dir <dir>] [--batch-list <tsv>] <input.json> <out.json> [pkg#ver ...]" >&2
  exit 2
}

MODE="sort"
FHIR_VERSION="r5"
OUTPUT_MODE=""
LOCAL_DIR=""
BATCH_LIST=""
while [[ "${1:-}" == --* ]]; do
  case "$1" in
    --r4) FHIR_VERSION="r4"; shift ;;
    --r5) FHIR_VERSION="r5"; shift ;;
    --sort) MODE="sort"; shift ;;
    --direct) MODE="direct"; shift ;;
    --native-r5|--output-r5) OUTPUT_MODE="r5"; shift ;;
    --output-r4) OUTPUT_MODE="r4"; shift ;;
    --local-dir) LOCAL_DIR="${2:-}"; [[ -n "$LOCAL_DIR" ]] || usage; shift 2 ;;
    --batch-list) BATCH_LIST="${2:-}"; [[ -n "$BATCH_LIST" ]] || usage; shift 2 ;;
    *) usage ;;
  esac
done

if [[ -n "$BATCH_LIST" ]]; then
  [[ "$FHIR_VERSION" == "r4" ]] || { echo "FATAL: --batch-list is currently supported only with --r4" >&2; exit 2; }
  [[ $# -ge 0 ]] || usage
  IN=""
  OUT=""
else
  [[ $# -ge 2 ]] || usage
  IN="$1"; OUT="$2"; shift 2
fi
PACKAGES=("$@")
if [[ ${#PACKAGES[@]} -eq 0 ]]; then
  if [[ "$FHIR_VERSION" == "r4" ]]; then
    PACKAGES=("hl7.fhir.r4.core#4.0.1")
  else
    PACKAGES=("hl7.fhir.r5.core#5.0.0")
  fi
fi

REPO="$(git -C "$(dirname "$0")/../.." rev-parse --show-toplevel)"
REAL_HOME="${REAL_HOME:-$HOME}"
FHIR_HOME="${FHIR_HOME:-$REPO/temp/fhir-home}"
FHIR_CACHE="${FHIR_CACHE:-$FHIR_HOME/.fhir/packages}"
FHIR_CORE_REPO="${FHIR_CORE_REPO:-/home/jmandel/hobby/fhir-perf/repos/fhir-core}"
if [[ -n "$OUT" ]]; then
  MSG="${OUT%.json}.messages.json"
else
  MSG=""
fi

source "$REPO/harness/_guard.sh"
before="$(date +%s)"
assert_isolated_fhir_home "$FHIR_HOME" "$REAL_HOME"

mkdir -p "$FHIR_CACHE" "$REPO/temp/snapshot-oracle/classes"
if [[ -n "$OUT" ]]; then
  mkdir -p "$(dirname "$OUT")"
fi
if [[ "${NO_SEED:-0}" != "1" && -d "$REAL_HOME/.fhir/packages" ]]; then
  while IFS= read -r pkg; do
    base="$(basename "$pkg")"
    [[ -e "$FHIR_CACHE/$base" ]] && continue
    cp -al "$pkg" "$FHIR_CACHE/$base" 2>/dev/null || cp -a "$pkg" "$FHIR_CACHE/$base"
  done < <(find "$REAL_HOME/.fhir/packages" -mindepth 1 -maxdepth 1 -type d | sort)
fi

if [[ ! -d "$FHIR_CORE_REPO" ]]; then
  echo "FATAL: FHIR_CORE_REPO does not exist: $FHIR_CORE_REPO" >&2
  exit 2
fi

CP_FILE="$REPO/temp/snapshot-oracle/classpath.txt"
{
  find "$FHIR_CORE_REPO" -path '*/target/*.jar' -type f \
    ! -name '*-sources.jar' ! -name '*-javadoc.jar' | sort
  if [[ -d "$REAL_HOME/.m2/repository" ]]; then
    find "$REAL_HOME/.m2/repository" -type f -name '*.jar' | sort
  fi
} > "$CP_FILE"
CP="$REPO/temp/snapshot-oracle/classes"
while IFS= read -r jar; do
  CP="$CP:$jar"
done < "$CP_FILE"

if [[ "$FHIR_VERSION" == "r4" ]]; then
  JAVA_CLASS="SnapOracleR4"
  JAVA_SOURCE="$REPO/snapshot/oracle/SnapOracleR4.java"
else
  JAVA_CLASS="SnapOracle"
  JAVA_SOURCE="$REPO/snapshot/oracle/SnapOracle.java"
fi

javac -cp "$CP" -d "$REPO/temp/snapshot-oracle/classes" "$JAVA_SOURCE"

JAVA_ARGS=()
if [[ "$MODE" == "sort" ]]; then
  JAVA_ARGS+=(--sort)
fi
if [[ "$FHIR_VERSION" == "r4" ]]; then
  case "$OUTPUT_MODE" in
    ""|"r5") JAVA_ARGS+=(--output-r5) ;;
    "r4") JAVA_ARGS+=(--output-r4) ;;
  esac
elif [[ -n "$OUTPUT_MODE" && "$OUTPUT_MODE" != "r5" ]]; then
  echo "FATAL: --output-r4 is only valid with --r4" >&2
  exit 2
fi
if [[ -n "$LOCAL_DIR" ]]; then
  JAVA_ARGS+=(--local-dir "$LOCAL_DIR")
fi
if [[ -n "$BATCH_LIST" ]]; then
  JAVA_ARGS+=(--batch-list "$BATCH_LIST" "$FHIR_CACHE")
  JAVA_ARGS+=("${PACKAGES[@]}")
else
  JAVA_ARGS+=(--messages "$MSG" "$FHIR_CACHE")
  JAVA_ARGS+=("${PACKAGES[@]}" "$IN" "$OUT")
fi

HOME="$FHIR_HOME" java -cp "$CP" "$JAVA_CLASS" "${JAVA_ARGS[@]}"
assert_real_fhir_untouched "$REAL_HOME" "$before"

#!/usr/bin/env bash
# Defensive guards: NEVER let any run touch the user's real ~/.fhir cache.
# Sourced by harness scripts. Fail loud, never silently fall back to real home.

# Resolve the repo root once.
guard_repo_root() {
  git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel
}

# assert_isolated_fhir_home <fhir_home_dir> <real_home>
# Verifies the chosen FHIR home is NOT the real home and lives under the repo's
# temp/ (whitelist). Aborts otherwise.
assert_isolated_fhir_home() {
  local fhir_home="$1" real_home="$2" repo
  repo="$(guard_repo_root)"
  local abs; abs="$(readlink -f "$fhir_home" 2>/dev/null || echo "$fhir_home")"
  local real_abs; real_abs="$(readlink -f "$real_home" 2>/dev/null || echo "$real_home")"

  if [[ -z "$abs" ]]; then
    echo "FATAL: FHIR home is empty" >&2; exit 99
  fi
  if [[ "$abs" == "$real_abs" ]]; then
    echo "FATAL: FHIR home ($abs) is the REAL home. Refusing to use real ~/.fhir." >&2; exit 99
  fi
  if [[ "$abs" == "$real_abs/.fhir" || "$abs/.fhir" == "$real_abs/.fhir" ]]; then
    echo "FATAL: would write to real ~/.fhir cache. Aborting." >&2; exit 99
  fi
  case "$abs" in
    "$repo"/temp/*) : ;;  # OK: under repo temp
    *) echo "FATAL: FHIR home ($abs) is not under $repo/temp. Refusing (defensive whitelist)." >&2; exit 99 ;;
  esac
}

# assert_real_fhir_untouched <real_home> <epoch_before>
# After a run, fail loud if ANY file under real ~/.fhir was modified since the
# stamp. Catches accidental writes (incl. via shared hardlink inodes).
assert_real_fhir_untouched() {
  local real_home="$1" since="$2"
  local cache="$real_home/.fhir"
  [[ -d "$cache" ]] || return 0
  local touched
  touched="$(find "$cache" -type f -newermt "@$since" 2>/dev/null | head -5)"
  if [[ -n "$touched" ]]; then
    echo "FATAL: real ~/.fhir was MODIFIED during the run. Files:" >&2
    echo "$touched" >&2
    echo "This must never happen. Investigate before continuing." >&2
    exit 98
  fi
}

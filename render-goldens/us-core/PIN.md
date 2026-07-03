# Render golden PIN — us-core

- **IG**: hl7.fhir.us.core v9.0.0 (STU 9), fhirVersion 4.0.1
- **IG source repo**: https://github.com/HL7/US-Core.git
- **Source commit**: 37d2928ba0a7b5cd3d17bb9c44828dfa9fe04498 (2026-06-02)
- **Publisher jar**: /home/jmandel/hobby/periodicity-impl/cycle/input-cache/publisher.jar
- **Publisher version**: FHIR IG Publisher Version 2.2.10 (Git# 37a39a2cca2d), built 2026-06-25
- **Terminology server**: http://tx.fhir.org (authoritative, contacted live)
- **Isolated HOME**: yes (never touched real ~/.fhir; -Duser.home + $HOME both set to isolated)
- **Harvest date**: 2026-07-03T17:28:49Z
- **Command**: `NO_COMPARISON=1 HOME=<isolated> java -Xmx6g -Duser.home=<isolated> -jar publisher.jar -ig <build-dir>`
- **Publisher exit code**: 0
- **Publisher wall time**: 454s (7.6 min, comparison disabled)

## Cross-version comparison: DISABLED
The IG's `version-comparison` / `ipa-comparison` / `ips-comparison` parameters
(comparing against US Core 8.0.1→3.1.1 + IPA/IPS) were stripped from the BUILD
copy of the ImplementationGuide JSON (never the pristine survey source). With
them ON, the publisher spends 60+ min downloading and SQLite-indexing every
prior version's terminology deps (us.nlm.vsac#* packages take 4-9 min EACH to
index). Those runs produce only the `comparison-v*` fragments/pages, which are
explicitly OUT OF SCOPE for the Rust renderer (cross-version-analysis is "not
derivable from package.db"). So the comparison-v* goldens are intentionally
ABSENT. All in-scope per-resource + IG-aggregate fragments ARE present.

## Corpus
- Fragments (base *.xhtml): **13387** kept (190M)
  - Excluded: -en language dupes (US Core produces none — single-language IG);
    payload-dump fragments (-{xml,json,ttl}-html serialized-resource echoes, ~1321);
    10 fragments >1MiB (listed in excluded-fragments.txt).
- Pages: NOT committed (US Core's 2660 real page HTMLs = ~237MB, over the
  committed-size budget). Re-derivable at any time via
  `HARVEST_PAGES=1 scripts/harvest-render-goldens.sh us-core` (jekyll VERIFIED
  working: the run produced 2660 output pages, qa-tx.html excluded).

Excluded oversized fragments listed in `excluded-fragments.txt`.

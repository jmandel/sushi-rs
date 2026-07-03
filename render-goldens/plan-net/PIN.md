# Render golden PIN — plan-net

- **IG**: hl7.fhir.us.davinci-pdex-plan-net v1.2.0, fhirVersion 4.0.1
- **IG source repo**: https://github.com/HL7/davinci-pdex-plan-net.git
- **Source commit**: 11da5dcd4f2c750338a7a820dcbd6affb1cb89e0 (2026-06-26)
- **Publisher jar**: /home/jmandel/hobby/periodicity-impl/cycle/input-cache/publisher.jar
- **Publisher version**: FHIR IG Publisher Version 2.2.10 (Git# 37a39a2cca2d), built 2026-06-25
- **Terminology server**: http://tx.fhir.org (authoritative, contacted live)
- **Isolated HOME**: yes (never touched real ~/.fhir; -Duser.home + $HOME both set to isolated)
- **Harvest date**: 2026-07-03 (see file mtimes)
- **Command**: `HOME=<isolated> java -Xmx6g -Duser.home=<isolated> -jar publisher.jar -ig <build-dir>`
- **Cross-version comparison**: enabled (stock) — ran the full comparison chain (~42 min build)
- **Template**: hl7.davinci.template#current (custom da Vinci template, fetched live)
- **Publisher exit code**: 1 (normal QA-failure exit; corpus intact)
- **Publisher wall time**: 2550s (~42 min, comparison ON)

## Corpus
- Fragments (base *.xhtml): **8097** kept (66M)
  - Excluded: -en language dupes (8599); payload-dump fragments
    (-{xml,json,ttl}-html serialized-resource echoes, 498); 4 fragments >1MiB
    (large VSAC-backed ValueSet expansions, listed in excluded-fragments.txt).
- Pages (output/*.html, qa*.html excluded): **1173** (4.8M), jekyll VERIFIED working.
  (plan-net's pages are mostly tiny .xml/.ttl/.json.html redirect stubs; kept as
  the F5 page-parity sample since they fit the size budget.)

Excluded oversized fragments listed in `excluded-fragments.txt`.

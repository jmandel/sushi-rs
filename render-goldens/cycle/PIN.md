# Render golden PIN — cycle

Fragments copied from the pre-existing real IG Publisher run at
`/home/jmandel/hobby/periodicity-impl/cycle/temp/pages/_includes` (this repo did
not re-run the publisher for cycle; it reuses cycle's committed build output).

- **IG**: me.fhir.period-tracking-mvp (cycle)
- **IG source**: /home/jmandel/hobby/periodicity-impl/cycle
- **Source commit**: e4a44e34a7a4ddeaa4d859a917b42e1169f91487 (2026-07-03)
- **Publisher jar**: /home/jmandel/hobby/periodicity-impl/cycle/input-cache/publisher.jar
- **Publisher version**: FHIR IG Publisher Version 2.2.10 (Git# 37a39a2cca2d), built 2026-06-25
- **Terminology server**: http://tx.fhir.org
- **Template**: fhir2.base.template (materialized near-clone of stock fhir.base.template)
- **Harvest date**: 2026-07-03T15:55:11Z
- **Command (original cycle run)**: `java -jar publisher.jar -ig ig-gh-actions.ini`

## Corpus
- Fragments (base *.xhtml, -en dupes excluded): **5459** kept (43M)
  - total emitted (incl -en): 10924; -en excluded: 5462; >1MiB excluded: 3
- Pages: not harvested here (cycle's full page HTML lives in its own temp/pages;
  page goldens for cycle can be taken from that dir directly if needed).

Excluded oversized fragments (the 3 full-instance payload dumps of the
longitudinal example Bundle: -json-html / -xml-html / -ttl-html) are listed in
`excluded-fragments.txt`. The 1MiB cap keeps all analysis fragments
(snapshot-all, dict, maps, expansion) which are the renderer's real targets.

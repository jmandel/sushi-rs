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
- **Harvest date**: 2026-07-03

## Corpus
- Fragments (base *.xhtml): **5411** kept (34M)
  - Excluded: -en language dupes (5462); payload-dump fragments
    (-{xml,json,ttl}-html serialized-resource echoes, 48); 3 fragments >1MiB
    (the longitudinal-example Bundle's -{json,xml,ttl}-html dumps, in
    excluded-fragments.txt).
- Pages: not committed for cycle (cycle's full page HTML lives in its own
  temp/pages and can be taken from there directly if a cycle page golden is needed).

Excluded oversized fragments listed in `excluded-fragments.txt`.

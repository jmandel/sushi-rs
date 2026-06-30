# Holdout Validation Findings (2026-06-30)

Ran stock SUSHI v3.20.0 vs our port on **8 popular FSH IGs we did NOT tune on**
(carinbb, sdc, davinci-pas, davinci-dtr, genomics-reporting, case-reporting/ecr,
fhir-pq-cmc(R5), fhir-us-ndh). All 8 built on both sides; **none reached byte
parity**. Clones + outputs in `temp/holdout/<ig>{,-stock,-rust}` (gitignored).
Skipped (not bugs): US-Core/IPA (ship prebuilt resources, no `input/fsh`),
qi-core (no sushi-config), davinci-pdex (repo gone).

Per-IG: carinbb 31 diff, sdc 43, pas 23+2missing, dtr 14, genomics 13+**225 lost
(crash)**, ecr 65, cmc 14, ndh 54. Every diff classified below; ROI order at bottom.

## Bug groups

**G1 — [CRASH] instance_export panic** (`instance_export.rs:859` `obj.get_mut(&key).unwrap()` on None).
Nested extension inside a complex-datatype value under a soft-indexed extension slice.
Aborts the whole build (SIGABRT) — genomics lost 225 resources. Repro (instance of GenomicReport):
`* extension[GenomicReportNote][+].valueAnnotation.extension[AnnotationCode].valueCodeableConcept = CodedAnnotationTypesCS#test-disclaimer`.
Must (a) never panic, (b) produce the nested extension stock produces.

**G2 — [HIGH] bare local CodeSystem name in a FshCode `system` not resolved to url.**
We resolve `$`-aliases but not bare local CS names (`C4BBIdentifierType#um`). SUSHI's
`replaceReferences` fishes name→canonical. Also breaks instance pattern-coding merge
(duplicate codings). Hits carinbb(all 31), pas, genomics, ndh.

**G3 — [HIGH, biggest by file count] extension `value[x]` dropped when the datatype
isn't in our embedded caret/instance schema.** ROOT CAUSE: the Phase-4 shortcut — a
hardcoded datatype table instead of fishing the REAL datatype SD from package_store.
Fails on ContactDetail, Expression, Attachment, Markdown, Coding(some), nested
`_valueCode.extension`. ~90+ files across ecr(53), sdc(34), dtr, cmc, ndh. **The
principled fix: SD-driven value typing via package_store (we have it now).**

**G4 — [MED, high count] `^context` (extension context) key order reversed.** Stock:
`expression` then `type` (source order); we emit FHIR element order. ndh 35/40 SD diffs,
ecr 6, cmc 1.

**G5 — [MED] Canonical()/Reference() to a local Instance with a DERIVED url not resolved.**
Our local resolution only uses instances with an explicit `* url =`; conformance
instances (Questionnaire/CapabilityStatement) whose url is `{canonical}/{Type}/{id}`
stay bare. dtr, pas.

**G6 — [MED] package with empty/missing `.index.json` not directory-scanned.**
`hl7.fhir.uv.subscriptions-backport.r4#1.1.0` has `files:[]` despite 24 resources; FPL
rebuilds by scanning, we trust the empty index → parent unfishable. pas drops 2
resources; ndh dependsOn.uri wrong. 14 cached pkgs have near-empty indexes.

**G7 — [MED] `^text` narrative caret on an SD dropped.** dtr 6 SDs.

**G8 — [MED] CodeSystem concept-level `property` values dropped.** sdc, cmc.

**G9 — [MED] inline/contained resource assigned to a Bundle entry truncated.** sdc
`entry.resource = <InlineInstance>` loses most content (107 lines → ~1).

**G10 — [MED] deep reslicing/added elements dropped on complex types.** cmc(R5)
MedicinalProductDefinition/SubstanceDefinition reslices; genomics/ndh Task.reasonReference.

**G11 — [LOW] VS `compose.include.system` uses derived url, not the local CS's `^url`.**
genomics — we derive the default canonical instead of reading the referenced CS's
declared `^url`.

**G12 — [INVESTIGATE] ndh CapabilityStatement-ndh-server: we emit a superset**
(extra name/jurisdiction). RuleSet application or predefined-vs-FSH precedence.

## ROI order (impact)
1. **G1** — crash, 225 resources, robustness.
2. **G3** — ~90+ files; structural (embedded schema → package_store SD typing); likely
   also fixes parts of G8/G10.
3. **G2** — carinbb wholesale + others; local CS-name fishing in `replaceReferences`.
4. **G4** — 35 ndh SDs; `^context` ordering.
5. G5–G12 — the tail.

## Meta-lesson
The 4-IG corpus (ips/epi/mcode/crd) was overfit. The biggest gap (G3) is the Phase-4
"embedded datatype table" shortcut — it should be replaced with real SD-driven typing
now that package_store exists. The holdout `*-stock` outputs are the oracle for the fix
cycle (same byte-diff loop).

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

**G1 — [FIXED 2026-06-30 T1: extension-slice rewrite dropped trailing index + determine_known_slices regating] instance_export panic** (`instance_export.rs:859` `obj.get_mut(&key).unwrap()` on None).
Nested extension inside a complex-datatype value under a soft-indexed extension slice.
Aborts the whole build (SIGABRT) — genomics lost 225 resources. Repro (instance of GenomicReport):
`* extension[GenomicReportNote][+].valueAnnotation.extension[AnnotationCode].valueCodeableConcept = CodedAnnotationTypesCS#test-disclaimer`.
Must (a) never panic, (b) produce the nested extension stock produces.

**G2 — [HIGH] bare local CodeSystem name in a FshCode `system` not resolved to url.**
We resolve `$`-aliases but not bare local CS names (`C4BBIdentifierType#um`). SUSHI's
`replaceReferences` fishes name→canonical. Also breaks instance pattern-coding merge
(duplicate codings). Hits carinbb(all 31), pas, genomics, ndh.

**G3 — [MOSTLY FIXED 2026-06-30 T1: SD-driven TypeResolver replaced BOTH hardcoded tables (caret_schema.rs deleted); add_extension_slice double-wrap fixed; soft-index on VS/CS carets] extension `value[x]` dropped when the datatype
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

**G6 — [FIXED 2026-06-30 — stock NEVER reads .index.json; always dir-scans; we now reconcile dir vs index] package with empty/missing `.index.json` not directory-scanned.**
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

## Failure taxonomy (2026-06-30, 265 holdout byte-fails @ main `f7f7f29`)
Built all 8 holdout to `/tmp/taxo`, classified each failing file (scripts:
`/tmp/taxo.py`, `/tmp/split.py`, `/tmp/ko.py`, `/tmp/gen.py`). Key split: **byte-fail
vs semantic-fail** (parse JSON both sides; equal ⇒ pure KEY-ORDER bug).

**Byte-fails = 265 → 58 key-order-only + 207 semantic.**

KEY-ORDER class (58, semantically identical to stock — mechanical ordering fixes):
- **G4 `^context` (type/expression) — 42** (ndh 35, ecr 6, cmc 1). Stock source order
  `expression` then `type`; we emit FHIR element order. Fix in `sd_export` caret-context
  emission.
- **G13 (NEW) instance `extension` property position — 16** (all sdc). Stock places
  `extension` right after `meta` (DomainResource order); we shove it down past the
  content elements (e.g. after `item`/`parameter`). Affects Questionnaire/Library
  instances. **Fix:** `order_instance` (`instance_export.rs:2811`) pins only
  `[resourceType,id,meta]` then keeps insertion order; extend the prefix to the full
  Resource/DomainResource leading block in FHIR order —
  `resourceType,id,meta,implicitRules,language,text,contained,extension,modifierExtension`
  (each with its `_`-sibling). Verified stock order from sdc `render` example:
  `...meta, contained, extension, modifierExtension, item ...` (contained BEFORE
  extension). Gate corpus — only MOVES these keys earlier; corpus must hold.

SEMANTIC class (207) by IG:
- **carinbb 31 — ALL G2** (system bare-name + duplicate-coding merge).
- **genomics 95 — 64 G2-system (instances!) + 11 SD-diff (G3/G10/context) + 9 G9
  (Parameters contained truncated) + 8 Bundle + 2 VS + 1 instance.** G2 is genomics'
  dominant bug too — the G2 fix should clear ~64 here.
- **pas 21** — G2 + G5 (derived url on conformance instances) + a few SD.
- **sdc 15 (semantic; +16 key-order above)** — G9 (Bundle/contained truncation),
  valueCanonical, profile[].
- **ecr 13 (semantic; +6 key-order)** — G9 (ersd Bundle contained), div, url.
- **cmc 12** — valueCanonical, CS concept `property` (G8), R5 reslices (G10).
- **dtr 9** — G5 (Canonical/Reference to local Questionnaire w/ derived url), questionnaire/url.
- **ndh 11 (semantic; +35 key-order)** — G2 + targetProfile.

**Round-1 in flight:** G2 (≈ carinbb 31 + genomics 64 + pas/ndh chunks), N1 (Quantity/
Ratio pattern — corpus-invisible, mining fixtures), N7 (FSHOnly IG — DONE, integrated).
**Round-2 (after R1 lands + rebase):** G4 (42, sd_export), G13 (16, instance_export),
then G9 (contained/Parameters truncation — sdc/ecr/genomics Bundles), G5 (derived url —
dtr/pas), G8/G10 tail. G4+G13 are pure-ordering, ~58 fails, very high ROI for round 2.

# Walk Engine Worklog (W2b)

Handoff-critical running log. Update per rung, not at the end.

## Oracle pins
- fhir-core: branch `snap-trace` @ `047763f89` (tracer instrumentation, base commit `5c4d5a0ff`).
- Trace anchors in trace-schema.md are against `snap-trace` line numbers (verified: PPP:209 iteration, PPP:216 dispatch.simplePath, PPP:308 emptyDiffMatches).
- Isolated cache: `temp/fhir-home/.fhir/packages` (exists). R5 fixtures use `hl7.fhir.r5.core#5.0.0`; r4-patient uses `hl7.fhir.r4.core#4.0.1`.

## Gate 1 baseline (legacy untouched)
- 2026-07-02: legacy IPS = ok=29/29 (verified before touching anything).

## Method
- Port branch-by-branch FROM Java open beside spec. After each rung: run walk_parity ladder + trace-diff.
- Trace ground truth: `TRACE=1 bash snapshot/oracle/gen-snapshot.sh --r5 --sort <fixture> <out> <pkgs>` writes `<out>.trace.jsonl`.

## Rungs — status (2026-07-02)

Modules built under src/walk/: mod, context, frame, paths, trace, consts, emit,
updatefromdef, ids, resolve, types_pred, simple, slicing, sliced, types,
contentref, finalize, preprocess.

### R5 ladder (hl7.fhir.r5.core#5.0.0)
ALL 16 R5 fixtures OUTPUT-MATCH goldens:
min, card-ms, card-ms-unsorted, binding-overlay, fixed-pattern, merge-additive,
choice-type, nested-child, simple-slice, slice-child, reslice, type-unfold,
extension-simple, observation-reference-profile, real-moneyquantity,
questionnaire-content-reference.

### Trace parity (decision-identical on (fn,branch,base,diff,x))
Verified decision-identical: min(142), card-ms(143), binding-overlay(141),
fixed-pattern(143), merge-additive(141), choice-type(141), nested-child(142),
simple-slice(150), slice-child(179), reslice(186), type-unfold(167),
extension-simple(25). ContentReference (questionnaire) output matches; trace
verified 323 records identical earlier.

### IPS (ENGINE=walk) — DECISION-IDENTICAL, output blocked by R4 projection
- Patient-uv-ips: trace decision-identical 212/212 records, 56/56 elements.
  Fixes that got it there: (a) R4 driver root-prepend (SnapOracleR4:183),
  (b) walk-specific sortDifferential order (preprocess::sort_differential: sort by
  base index of longest known ancestor prefix, stable) — the legacy
  sort_differential_by_base scattered slices to the end and broke defaultBeforeSlices
  vs acceptDiffSlicing dispatch, (c) P6 fixTypeOfResourceId (Patient.id type -> id).
- Remaining IPS gaps (NOT decision problems on Patient):
  1. R4 OUTPUT PROJECTION: constraint.xpath kept as a field (not ext) for R4-base
     constraints; elementdefinition-isCommonBinding preserved on R4 bases; likely
     maxValueSet/additionalBinding ordering. ~19 Patient elements differ only by
     this shape. Legacy does it in project_r4_snapshot_to_native_r5. Not wired into
     walk finalize. Needs an r4_native flag threaded through emit/checkExtensions so
     these are preserved (or an output projection over pre-xpath-converted data).
  2. §3.6.2 processPathWithSlicedBaseWhereDiffsConstrainTypes (type-slicing on an
     already-sliced base) + fake-diff replay: STUBBED (bails). Bundle-uv-ips and
     Composition-uv-ips hit ATTEMPT_TO_A_SLICE_..._DOES_NOT_REPEAT because of this.
  3. §3.6.1 sliced-base empty-diff type-unfold and §3.6.3 anchor-children unfold:
     stubbed bails; may hit on other IPS profiles.
- IPS result: ok=0/29 by byte gate, but walk decisions are Java-identical where
  reached; the gap is projection + 2 unimplemented sliced-base branches.

### r4-patient-card-ms — OPEN (R4 output projection)
Base = hl7.fhir.r4.core#4.0.1. Walk is R5-internal-correct but the R4→native-R5
OUTPUT projection (xpath-ext -> xpath field for R4-base-inherited constraints;
53 xpath fields vs 1 extension in golden) is not wired into finalize. This is the
same projection boundary the IPS gate needs. Legacy does this in
project_r4_snapshot_to_native_r5. TODO: apply an R4 output-projection in finalize
when the base fhirVersion is 4.x.

### Key Java-fidelity fixes made
- NON_INHERITED/DEFAULT_INHERITED/NON_OVERRIDING url lists copied verbatim (consts.rs).
- updateFromDefinition else-if base.hasBinding strips NON_INHERITED (PU:3061).
- EXT_TRANSLATABLE 2nd-drop hack (PU:2631).
- updateURLs markdown rewrite (reuses text::rewrite_markdown_links) + valueSet #-abs.
- setIds contentReference absolutize with type (PU:4388).
- type-unfold keeps sourceStructureDefinition = parent (NOT dt) — only cursor.baseSource=dt.
- processSimplePathDefault: per-slice newDiffLimit drives final cursor advance;
  slicing.path = null in slice bodies; anchor consumes diff0 (mark_consumed).
- contentref empty-diff branch: contextPathTarget = outcome.path (PPP:1256).
</content>
</invoke>

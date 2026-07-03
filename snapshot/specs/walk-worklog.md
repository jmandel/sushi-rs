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

## Increment 2 (post faf603b) — IPS 29/29

### Gates (2026-07-02, all re-verified after final change)
- ENGINE=walk IPS: **ok=29 failed=0 total=29**.
- Legacy IPS (default engine): ok=29/29 (untouched).
- R5 ladder: 16/16 output-match; walk_parity + snapshot_parity + convert_parity green.
- Trace parity (decision-identical incl. `x` payloads): all 14 R5 fixtures
  (25-323 records each), IPS Patient(212), AllergyIntolerance(280),
  Observation-results-radiology(341), MedicationRequest(398),
  Composition(1796), **Bundle(9681 — 27 nested generations, 188 preprocessor
  inserts, 32 preprocessor merges)**.

### What landed in this increment
1. **Real sortDifferential port** (`sort.rs`): ElementDefinitionHolder tree
   (placeholder nodes for sparse paths), ElementDefinitionComparer with
   `find` (contentReference re-anchoring, [x] both-ways, restart-at-1 quirk),
   getComparer's 8 branches (backbone-continue, Resource-profiled walk-up-to-
   specialization, Extension-profile, single-type, [x]-suffix type, Reference,
   Element fallback), stable sort by baseIndex, writeElements skipping
   placeholders, compareDiffs "out of order"/count errors (comparer-internal
   find errors NOT collected: debug-only under oracle, PU:3945).
2. **R4 load-path split** (`resolve.rs`): package-loaded R4 SDs are read the
   way the oracle context reads them — R5-parser-lenient (R4-only props
   DROPPED: constraint.xpath, SD contextType/string-context) — while the input
   profile and local-dir resources get the full VersionConvertor conversion
   (SnapOracleR4 runOne + loadLocalR4CanonicalResources). This resolved the
   whole xpath/isCommonBinding "projection" question: there IS no output
   projection; the loaded base simply never has those R4 leftovers, and
   NON_INHERITED stripping handles isCommonBinding identically in both paths.
   Answers REWORK-PLAN §8 seed question (package vs local-dir conversion paths).
3. **updateFromDefinition profile-root doco override** (PU:2648-2717) — restores
   extension/resource profile root short/definition/comment/alias/mapping after
   checkExtensionDoco; resource-root template constraints cleared (PPP:836-840);
   profile resolution happens BEFORE the Extension/Resource gate so nested
   generation fires exactly where Java's does (PPP:778-811).
4. **Preprocessor processSlices push-down** (SGPP:693-746): SliceInfo tree,
   isExtensionSlicing (incl. the "modiferExtension" typo, verbatim),
   complexity-guard early return (skips markExtensions too), backward-pass
   mergeElements with fill-missing merge (SGPP:1003) and injected rows
   (determineInsertionPoint/comesAfterThis via ElementAnalysis-lite child-order
   from type SDs), SNAPSHOT_PREPROCESS_INJECTED exemption from PC-1; plus the
   final markExtensions pass stamping obligation `snapshot-source` with the
   DERIVED versioned url (found via oracle experiment temp/exp/oblig-test).
5. **processSimplePathWhereDiffsConstrainTypes rewritten** to PPP:554-748:
   diffMatches[1..] loop (not typeList), live-diff sliceName/type coherence
   mutation, slicerElement min=0 for multi-type, fixedType pruning,
   allowed-types OPEN relaxation, last-slice newDiffLimit cursor advance.
6. **One-match slicer max clamp** (PPP:911) — LIVE (I had it wrongly dead).
7. Q8 finalize: mapping.map trim + relative constraint.source absolutization.
8. OVERRIDING_ED_URLS set-value-on-existing (PU:3239) + full OVERRIDING list.
9. Driver root-prepend + driver sort are TOP-LEVEL ONLY (nested generateSnapshot
   gets neither); generateSnapshot.begin trace fires before preprocess.
10. spec_url per VersionUtilities.getSpecUrl (4.0→/R4/, 4.3→/R4B/, 5.0→/R5/).
11. `java_hashset_order` for the overwriteSlicingToOpen unusedTypes trace
    payload (Java serializes a HashSet; contents identical, order emulated).

### r4-patient-card-ms — golden is STALE (transitional shape)
Verified with single AND batch oracle runs: the current pinned oracle does NOT
reproduce the committed golden (constraint[0].xpath as plain field), and the
walk output is byte-identical (SNAPSHOT PARITY) to the fresh oracle output.
Skip documented in walk_parity.rs; re-gate at next re-pin/golden regen.

### Remaining stubs (not exercised by any current gate)
- §3.6.2 processPathWithSlicedBaseWhereDiffsConstrainTypes + fake-diff replay
  (sliced.rs bails) — Bundle/Composition no longer route here after the real
  sort landed; still needed for corpus scale-out.
- §3.6.1 sliced-base empty-diff type-unfold; §3.6.3 anchor-children unfold.
- Cross-SD contentReference frame (contentref.rs same-SD only).
- Preprocessor additional-base path (SGPP:137-152, DEAD under oracle).

### IPS (ENGINE=walk) — DECISION-IDENTICAL, output blocked by R4 projection (SUPERSEDED — see Increment 2 above)
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

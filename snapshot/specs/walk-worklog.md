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
- §3.6.3 processPathWithSlicedBaseDefault anchor-children unfold (`has_inner_diff_matches`
  true branch in `process_path_with_sliced_base_default`, sliced.rs — still bails).
- Cross-SD contentReference frame (contentref.rs same-SD only).
- Preprocessor additional-base path (SGPP:137-152, DEAD under oracle).

## Increment 3: mcode — ENGINE=walk ok=46/46 (2026-07-02, wave 3 batch 1)

Gate: `ENGINE=walk bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/mcode
hl7.fhir.r4.core#4.0.1 hl7.fhir.us.core#6.1.0 hl7.fhir.uv.genomics-reporting#2.0.0
hl7.fhir.uv.extensions.r4#5.3.0` → **R4 HARVEST CHECK: ok=46 failed=0 total=46**.

Started at 42/46; 4 failures, all in the sliced/type-slice stub family. What was
missing and what landed:

1. **§3.6.2 `processPathWithSlicedBaseWhereDiffsConstrainTypes` + fake-diff
   replay** (was the loud bail). Ported PPP:1650-1827 into
   `types::process_path_with_sliced_base_where_diffs_constrain_types` (types.rs),
   dispatched from sliced.rs. Reuses the §3.4 helpers (determine_type_slice_path,
   capitalize, diff_mutate, determine_fixed_type). New: `find_base_slices`
   (PU:findBaseSlices — enumerate existing base type slices), `chooseMatchingBaseSlice`
   (inline `.position(|bs| bs.type_ == ty)`), the per-diff-slice loop binding each
   type slice to its matching base slice window (sStart/sEnd), fixedType root
   pruning (PPP:1799-1806), and the **unhandled-base-slice fake-diff replay**
   (PPP:1807-1824): swap `ctx.diff`/`diff_consumed`/`diff_injected` to a synthetic
   one-element differential `[{path: bs.path}]`, run processPaths over the base
   slice window (diffLimit=0, slicing.path=currentBasePath so it dispatches as
   simplePath → oneMatchingElement → templateFromBase copy-through), then restore
   the real diff. Cursor advance `baseCursor = baseSlices.last().end+1`,
   `diffCursor = newDiffLimit+1`. Root uses `slicing.done=true, path=currentBasePath`.
   → mcode-genomic-variant: **SNAPSHOT PARITY + trace decision-identical (690/690
   records)**. `Observation.value[x]` sliced-base type-slice with one CodeableConcept
   base slice replayed via fake diff.

2. **`newSliceAtEnd` type-unfold** (was output-truncated: extension slices lost
   their `.id/.extension/.url/.value[x]` children). Ported the post-merge unfold
   block PPP:1544-1616 into `process_path_with_sliced_base_default`'s newSliceAtEnd
   loop (sliced.rs): (a) single-profile min/max constraint pickup (PPP:1544-1560),
   (b) unfold-into-type when the diff walks into the new slice and the base does
   not (PPP:1562-1610) — Base/Element/BackboneElement recurse over the base child
   window, else recurse into the resolved datatype/profile SD at index 1
   (contextPathSource=diffMatches[0].path, contextPathTarget=outcome.path), (c)
   contentReference+type ⇒ clear type (PPP:1616). Made `base_walks_into`,
   `resolve_type_sd`, `snapshot_elements` pub(crate).
   → mcode-radiotherapy-modality-and-technique (profiled Extension slices
   modality/technique with `.value[x]` children), plus dose-delivered-to-volume,
   modality-and-technique, secondary-cancer-condition.

3. **§3.6.1 sliced-base empty-diff type-unfold** (was `bail!("sliced-base
   empty-diff type unfold not implemented")`). Ported PPP:1855-1882 into
   `process_path_with_sliced_base_empty`'s else branch (sliced.rs): resolve the
   datatype SD via resolve_type_sd, advance the diff cursor over the anchor path +
   its children, recurse into `dt.snapshot` at index 1 when the cursor advanced.
   Hit inside the nested modality/technique extension unfold (the profiled
   Extension's own `Extension.extension` anchor is sliced with empty diff matches).

Trace-parity spot checks (decision-identical incl. `x`): mcode-genomic-variant
690/690, mcode-radiotherapy-modality-and-technique output SNAPSHOT-PARITY (root
`condition:["ele-1"]` is a tolerated soft diff-snapshot class, same as the
already-green mcode-body-location-qualifier).

No convert.rs / resolve.rs changes needed this increment.

No-regression: `cargo test -p snapshot_gen` green (ladder + units), IPS
ENGINE=walk 29/29, IPS DEFAULT (legacy) 29/29 — all re-run after the change.

## Increment 4: crd — ENGINE=walk ok=22/22 (2026-07-02, wave 3 batch 1)

Gate (full CRD dep closure per AGENTS.md) → **R4 HARVEST CHECK: ok=22 failed=0
total=22**. Started 15/22; all 7 failures were two field-copy gaps:

1. **`example[]` additive merge in updateFromDefinition** (5 failures:
   appointment-base, communicationrequest, devicerequest, nutritionorder,
   visionprescription — all `$[N].example missing from actual`). Ported PU:2827-2856:
   each differential `example` not already present in base (compared by `label` +
   polymorphic `value[x]`) is appended to the base example list. The EXT_ED_SUPPRESS
   `$all`/suppress delete path is rare and left as append-if-missing (not yet
   exercised). Added `example_value()` helper (extracts the `value[x]` key/value).

2. **`updateURLs` on sliced-base copy paths** (2 failures: medicationrequest,
   servicerequest — `binding.description` link `general-requirements.html#...`
   left relative instead of rewritten to `http://hl7.org/fhir/general-requirements.html#...`).
   Java calls `updateURLs(url, webUrl, X.copy(), true)` on **every** clone in
   `processPathWithSlicedBaseDefault` / `processPathWithSlicedBaseAndEmptyDiffMatches`;
   the Rust sliced paths cloned without it, so inherited base-slice
   `binding.description` markdown links (and #-valueSets) were never rewritten. Added
   `emit::update_urls(&mut o, &frame.url, &frame.spec_url)` after each `clone_element`
   in sliced.rs: anchor, backbone-children copy, base-slice `outcome`
   (match/copyUnmatched), copyUnmatched children loop, newSliceAtEnd template,
   empty-diff walkIntoBaseChildren outcome, copyAllBaseSlices loop. The
   `general-requirements.html` unversioned-absolute mapping already lived in
   `text::publisher_native_link_target`; it just wasn't reached on the sliced path.

No convert.rs / resolve.rs changes.

No-regression: ladder + units green; IPS walk 29/29, mcode walk 46/46, IPS legacy
29/29 — all re-run after the change.

## Increment 5: sdc — ENGINE=walk ok=71/73 (2026-07-02, wave 3 batch 1) — PARTIAL

Gate → **R4 HARVEST CHECK: ok=71 failed=2 total=73**. Started 62/73; 11 failures
across four gaps, 9 fixed:

1. **`mapping[]` merge in updateFromDefinition** (2 failures: sdc-codesystem,
   sdc-questionnairecommon — root `mapping.length` mismatch). Ported PU:3111
   `MappingAssistant.merge(derived, base)` into `merge_mappings` (updatefromdef.rs):
   differential-element mappings come FIRST, then inherited base mappings, deduped
   by `(identity, trimmed map)`. R4/non-R5Plus path; the cross-version `renames`
   map and R5 APPEND/DUPLICATE/IGNORE/OVERWRITE modes are omitted (not exercised —
   would need SD-level `mapping[]` declarations of base vs derived).

2. **`fillOutFromBase` field allow-list** (7 failures: the Parameters
   profiles — `$[N].condition missing from expected`). The Rust
   `fill_out_from_base` copied ALL missing keys; Java (PU:1906) copies only a
   specific allow-list that **excludes `condition`** (also type/base/slicing/mapping/
   id/path). When a `.resource` narrows to a single profiled resource type
   (templateFromProfile → fillOutFromBase(profileRoot, currentBase)), the profile
   root has no `condition`, so the base's inherited `inv-1` must NOT leak in.
   Rewrote to the exact Java allow-list: scalar fill-if-missing (sliceName, label,
   definition, short, comment, requirements, min, max, maxLength, mustSupport,
   isSummary, isModifier, isModifierReason, mustHaveValue, binding), polymorphic
   fill-if-missing (fixed[x]/pattern[x]/minValue[x]/maxValue[x]), example if-absent,
   and additive arrays (code by value, alias by value, constraint by key, extension
   by url). Added `is_choice_key` / `additive_array` helpers.

3. **`example[]` additive merge** — the same helper added in Increment 4 (CRD)
   also covered SDC Parameters examples.

4. **`processSimplePathDefault` sliceGroupBaseDefinition inner unfold**
   (parameters-questionnaire-populate-in — 7 missing children of
   `Parameters.parameter:context.part`). Implemented the two stubbed sub-branches
   (PPP:419-470): (a) **unfoldType** — when the anchor's diff walks in and the base
   has no children, recurse into the datatype SD (contextPathSource=currentBasePath,
   contextPathTarget=anchor.path, slicer=updated anchor); (b)
   **contentReferenceInlineDump** — when the anchor has a `contentReference` and no
   base children, resolve it (`resolve_content_reference_pub`), dump the referenced
   element's children inline via `findEndOfElementNoSlices`, path-fixed with
   `path.replace(frag, anchorPath)` (Java fixForRedirect), each cloned +
   `update_urls` + `clear_id` + `updateFromBase`. → SNAPSHOT PARITY + trace
   decision-identical (378/378 records).

### PARTIAL — 2 profiles left (sdc-codesystem, sdc-valueset): xver base-snapshot divergence
Both fail on the R5-backport extension slices carried by the xver
`http://hl7.org/fhir/5.0/StructureDefinition/profile-ValueSet` /
`profile-CodeSystem` bases (the REWORK-PLAN §8 `artifact-versionAlgorithm` /
`codeOptions` seed-quirk area). Root-caused via trace to a **base-snapshot content
divergence**, NOT a walk decision:

- First trace divergence (sdc-valueset seq ~34): a missing
  `updateFromDefinition.checkExtensionDoco` on `ValueSet.extension:versionAlgorithm`
  (1-record offset, cosmetic).
- The real output diff (extra `ValueSet.expansion.contains.designation.extension:additionalUse`):
  at the `expansion.contains.designation` contentReference re-walk, Java's
  `cursors.base` window for `compose.include.concept.designation` starts at base
  index 44 with the extension anchor UNSLICED, whereas Rust's loaded base
  (`resolve_content_reference` into the stored xver `profile-ValueSet` snapshot,
  index 59) has `compose.include.concept.designation.extension` already SLICED with
  an `additionalUse` slice at index 60. The stored xver base snapshot Rust reads is
  structurally different (indices, slicing) from the base list Java walks — Java
  appears to regenerate / read a differently-shaped profile-ValueSet base. The
  contentReference resolution itself matches Java's algorithm; the divergence is
  upstream in how the xver R5-backport base snapshot is loaded/converted (stage
  1/2), so it needs an xver-load investigation, not a walk-branch fix. Left for a
  follow-up; the other 71 SDC profiles (incl. all the Parameters contentReference
  ones) are byte-parity.

No convert.rs / resolve.rs changes this increment.

No-regression after SDC: ladder + units green; IPS walk 29/29, mcode walk 46/46,
crd walk 22/22 — all re-run.

## Increment 6: carinbb + dtr — ENGINE=walk 6/6 + 21/21 (2026-07-02, wave 3 batch 1)

- **carinbb**: `ok=6 failed=0 total=6` out of the box (no changes needed — the
  earlier increments' example/mapping/updateURLs fixes already covered it).
- **dtr**: started 19/21; 2 failures (dtr-base-questionnaire,
  dtr-questionnaire-adapt-search), both `$[N].definition` markdown link left
  relative (`narrative.html#security` instead of `http://hl7.org/fhir/R4/narrative.html#security`)
  on `rendering-xhtml` extension slices unfolded via templateFromProfile.
  Root cause: `apply_profile_root_doco` (updatefromdef.rs) copied the profile
  root's `definition` / `binding.description` RAW, overwriting the earlier
  `update_urls` pass. Java (PU:2686/2688) rewrites them via
  `processRelativeUrls(text, webroot, context.getSpecUrl(), …, true)`. Added a
  `spec_url` field to `WalkContext` (set once per generation from `frame.spec_url`)
  and applied `text::process_relative_markdown_urls(d, &ctx.spec_url, true)` to
  the copied definition and binding.description (keep_known_relative=true so
  `StructureDefinition-rendering-markdown.html` stays relative per the
  publisher-native list, while `narrative.html#security` gets the R4 spec prefix).

No convert.rs / resolve.rs changes.

No-regression after dtr: ladder + units green; IPS 29/29, mcode 46/46,
carinbb 6/6, crd 22/22, sdc 71/73 — all re-run on ENGINE=walk (sdc unchanged;
the profile-root-doco rewrite did not regress it).

## Increment 7: genomics — ENGINE=walk ok=33/33 (2026-07-02, wave 3 batch 1)

Package set (not in AGENTS.md — bases are all core R4, deps mirror mcode's
genomics context): `hl7.fhir.r4.core#4.0.1 hl7.fhir.us.core#6.1.0
hl7.fhir.uv.genomics-reporting#2.0.0 hl7.fhir.uv.extensions.r4#5.3.0` →
**R4 HARVEST CHECK: ok=33 failed=0 total=33**.

Started 30/33; 3 failures (genomic-base, genomic-report, molecular-biomarker),
all the same markdown-link bug: inherited `comment`/`definition` links
(`observation.html#obsgrouping`, `observation.html`) left relative instead of
rewritten to `http://hl7.org/fhir/R4/observation.html…`. These are on slicing
anchors (`Observation.derivedFrom`, `DiagnosticReport.result`) introduced by the
differential via `processSimplePathDefault.acceptDiffSlicing`. Java (PPP:352)
does `outcome = updateURLs(url, webUrl, currentBase.copy(), true)` on the anchor
clone; the Rust `acceptDiffSlicing` cloned without it. Added
`update_urls(&mut outcome, &frame.url, &frame.spec_url)` to the acceptDiffSlicing
anchor clone in slicing.rs. (The empty-diff and one-match paths already had it.)

No convert.rs / resolve.rs changes.

No-regression after genomics: ladder + units green; IPS walk 29/29, mcode 46/46,
carinbb 6/6, crd 22/22, dtr 21/21, sdc 71/73; IPS legacy (default engine) 29/29.
Confirmed no legacy/quirks/goldens/oracle files modified.

## Increment 8: ecr — ENGINE=walk ok=27/28 (2026-07-02, wave 3 batch 1) — PARTIAL

Gate (full eCR dep closure per AGENTS.md) → **R4 HARVEST CHECK: ok=27 failed=1
total=28**. Only `ersd-plandefinition` fails (the known §8 hard profile:
cqf-fhirQueryPattern / us-ph-named-eventtype-extension / checkSuspectedDisorder /
checkReportable). Started 27/28.

**Fixed a real off-by-one** while root-causing it (benefits every newSliceAtEnd
type-unfold, incl. mcode): the `newSliceAtEnd` type-unfold recursion set the
nested `diff_cursor = start`, but Java (PPP:1580 Base/Element branch, PPP:1595
datatype branch) uses `start - 1`. Changed both to `start.saturating_sub(1)`.
This advanced the ersd first-divergence from seq 328 (fhirquerypattern extension
unfold) to seq 670, and made **mcode-radiotherapy-modality-and-technique
trace fully decision-identical (62/62 records)** — it had been a cosmetic
off-by-one that output-matched but trace-diverged. All green IGs re-verified
unchanged.

### PARTIAL — ersd-plandefinition (the eCR PlanDefinition): stacked §8 quirks
Rust emits 1855 elements vs golden 1494 (361 extra). After the start-1 fix, the
first trace divergence is at seq 670 on
`PlanDefinition.action.trigger.extension:namedEventType` (the
`us-ph-named-eventtype-extension` seed quirk): Java takes
`updateFromDefinition.checkExtensionDoco` on the namedEventType extension while
Rust is one record ahead on `PlanDefinition.action.trigger.type`. This profile
stacks several §8 seed quirks (cqf-fhirQueryPattern child materialization —
partly progressed by start-1; us-ph-named-eventtype-extension; the nested
recursive-action checkSuspectedDisorder/checkReportable stamps; nested
trigger id/extension cloning). It needs a dedicated pass through the eCR
PlanDefinition recursive-action + backport-extension quirk set; left partial.
The other 27 eCR profiles are byte-parity.

No convert.rs / resolve.rs changes.

No-regression after ecr/start-1: ladder + units green; IPS 29/29, mcode 46/46,
carinbb 6/6, crd 22/22, dtr 21/21, genomics 33/33, sdc 71/73; IPS legacy 29/29.

## Increment 9: ndh — ENGINE=walk ok=50/50 (2026-07-02, wave 3 batch 1)

Gate (AGENTS.md list incl. subscriptions-backport.r4#1.1.0) →
**R4 HARVEST CHECK: ok=50 failed=0 total=50**. Started 47/50; all 3 failures
(ndh-Network, ndh-Organization, ndh-Practitioner) hit the LAST §3.6.3 stub:

1. **`processPathWithSlicedBaseDefault` anchor-children unfold** (PPP:1380-1415)
   — the `hasInnerDiffMatches(…, false)` true branch, previously a loud bail.
   Two sub-branches ported: (a) base has NO children (`newBaseLimit == baseCursor`):
   require a single type, resolve via getProfileForDataType (`resolve_type_sd`),
   advance the OUTER diffCursor past the anchor's children, recurse into the dt
   snapshot at index 1 with `newDiffCursor = ndx + (diff0.hasSlicing ? 1 : 0)`,
   diffLimit = findEndOfElement(ndx), contextPathSource=currentBasePath,
   contextPathTarget=outcome.path; (b) base HAS children: recurse over the base
   child window (baseCursor+1..newBaseLimit) with the same diff window,
   profileName += pathTail(diff0), redirector cleared. Control then falls through
   to the existing base-slice pairing loop (getSiblings), matching Java.
2. **Trace-label fix**: `generateSnapshot.begin` now records the RESOLVED base
   SD url (Java `base.getUrl()`), not the possibly-versioned baseDefinition query
   (`…us-core-practitioner|6.1.0` → `…us-core-practitioner`). Cosmetic only.

Trace-parity spot check: ndh-Practitioner **decision-identical 845/845 records**.

No resolve.rs / convert.rs / package.rs changes (now owned by another agent).

No-regression after ndh: ladder + units green; IPS 29/29, mcode 46/46,
carinbb 6/6, crd 22/22, dtr 21/21, genomics 33/33, sdc 71/73, ecr 27/28;
IPS legacy (default engine) 29/29.

## Increment 10: pas — ENGINE=walk ok=73/73 (2026-07-02, wave 3 batch 1)

Gate (AGENTS.md list — NOTE: `hl7.fhir.uv.subscriptions-backport#0.1.0`, not
.r4#1.1.0, per the goldens caveat) → **R4 HARVEST CHECK: ok=73 failed=0
total=73** out of the box; no changes needed (the §3.6.x + updateURLs +
example/mapping/fillOutFromBase work from increments 3-9 covered it).
Trace-parity spot check: profile-pas-inquiry-request-bundle
**decision-identical 2645/2645 records** (Bundle resource slots over local
Claim/ClaimResponse bases, nested generations).

## Increment 11: cross-SD contentReference + Java-exact checkExtensionDoco —
## sdc 73/73, ersd = fresh-oracle parity (2026-07-02, wave 3 batch 1)

Two engine-wide changes closed the remaining sdc gap AND resolved the whole ersd
divergence stack:

1. **Java-exact `checkExtensionDoco`** (new `walk::updatefromdef::check_extension_doco`,
   PU:1963; the walk no longer uses the legacy-shared `crate::check_extension_doco`).
   The legacy version adds a `has_profiled_extension_type` guard Java does NOT
   have; Java normalizes doco for ANY `.extension`/`.modifierExtension` path
   (except II.extension bases) even when the element carries a profiled Extension
   type. This is the mechanism behind the coordinator-identified sdc-codesystem
   items (a) version-pinned unresolvable `artifact-versionAlgorithm|5.2.0` slice
   and (b) missing-profile `rendering-criticalExtension` fallback — both get
   generic extension doco because checkExtensionDoco fires unconditionally on the
   slice, regardless of whether the profile later resolves. legacy.rs untouched
   (walk-local copy). Also fixed ersd's seq-670 divergence
   (`PlanDefinition.action.trigger.extension:namedEventType`).

2. **Cross-SD contentReference frame** (the last big stub, contentref.rs).
   Ported PU:3553 `getElementById` — `url#frag` whose url differs from the
   frame's sourceStructureDefinition resolves THAT SD (with snapshot) and matches
   by element **id** — plus both call sites:
   - one-match walk-into (PPP:958-996): cross-SD swaps the nested base list to
     the target SD's snapshot, sets `nc.baseSource` AND the frame's
     `sourceStructureDefinition` to the target SD (PPP:970/977), nested
     diffCursor `start-1`, contextPathTarget = diffMatches[0].path.
   - empty-diff walk-into (PPP:1228-1266): cross-SD **mutates `cursors.base`**
     (persists for the caller, PPP:1234), keeps `cursors.baseSource`, nested
     diffCursor `start-1` (same-SD keeps `start`). NOTE Java's cross-SD branch
     here dereferences `diffMatches.get(0)` on an empty list → would throw, so
     it is effectively dead in that function; the port uses outcome.path.
   Same-SD matching switched from backwards path-scan to Java's forward id-scan.
   Effects:
   - **sdc-valueset**: `ValueSet.expansion.contains.designation`'s canonicalized
     contentReference (`…StructureDefinition/ValueSet#ValueSet.compose.include.concept.designation`)
     now resolves into CORE R4 ValueSet (unsliced window 44-49) instead of the
     local xver base's sliced window → no phantom `additionalUse` slice.
     SNAPSHOT PARITY + decision-identical 659/659.
   - **ersd-plandefinition**: the recursive `PlanDefinition.action.action`
     contentReference (canonicalized to core `…/PlanDefinition#PlanDefinition.action`)
     now walks CORE PlanDefinition.action children (51) instead of the us-ph
     base's expanded subtree (110) — this was THE mechanism behind the legacy
     engine's faked eCR recursive-action behaviors. Rust: 1494/1494 elements,
     **trace decision-identical 4911/4911**.

Gate: **sdc ok=73 failed=0 total=73**.

### ecr ersd-plandefinition: GOLDEN IS STALE (fresh pinned oracle ≠ committed golden)
The gate still reports ecr 27/28, but the walk output is **byte-identical
(SNAPSHOT PARITY) to the FRESH pinned oracle**, verified in BOTH single AND
batch oracle modes, and decision-identical (4911/4911). The committed golden
differs from the fresh oracle in exactly 25 elements, all in two §8 quirk
families: (1) `PlanDefinition.extension:variable` comment carries the SDC
2025Jan sentence (from the uv.extensions copy of the `variable` extension — NOT
resolvable in the eCR package closure; fresh oracle emits the r4.core comment),
and (2) all 8 `…input.extension:fhirquerypattern(.url|.value[x])` groups carry
the rich uv-extensions cqf-fhirQueryPattern doco (fresh oracle emits generic
"Extension"/"An Extension" because the canonical is unresolvable). Oracle
experiments (min fixtures over r4.core / full closure / us-ph base / local-dir)
all reproduce the fresh (generic) form. The golden predates the current pin or
was generated with an extensions-augmented cache; the LEGACY engine matches the
stale golden via its hardcoded NATIVE_R5_VARIABLE_COMMENT / fhirquerypattern
stamps — precisely the corpus-fitted fakes this rework eliminates. Per hard
rules the golden was NOT touched; regenerating ecr goldens with the pinned
oracle (batch outputs already verified equal to Rust for all 28) will turn ecr
28/28. Same stale-golden class as r4-patient-card-ms.

Also this increment: **§3.6.3 anchor-children unfold** landed for ndh (see
Increment 9) — no stubs remain in the sliced-base family. Fresh-oracle
cross-check: all 28 ecr fixtures fresh-batch-generated and diffed vs Rust →
28/28 SNAPSHOT PARITY.

## Increment 12: published-package sweep — wave 3 batch 2 (2026-07-02)

Task #8: the published-package corpus on ENGINE=walk. IG order roughly increasing
risk. Two engine-wide behaviors landed early (both regression-verified against the
full green set: ips/mcode/sdc/pas/crd/ndh/genomics/dtr/carinbb/ecr + ladder):

**A. Q10 slice-cardinality finalize pass** (PU:996-1050, `finalize.rs
apply_slice_cardinality`). Walks the snapshot tracking open slice groups
(ElementDefinitionCounter); on group close (a non-slicing row at same/deeper
dot-depth), overwrites the anchor `min` with the sum of its slice mins when
`countMin > anchorMin && repeats(base.max != "1") && auto_added_slicing`.
Otherwise Java only emits a message (not ported — message-only). Required a new
`Annotation.auto_added_slicing` sidecar flag set at the two Java set-sites:
PPP:378 (`processSimplePathDefault` acceptDiffSlicing autoAddedSlicing branch,
slicing.rs) and PPP:1369 (`processPathWithSlicedBaseDefault` anchor with
sliceName+no-slicing, sliced.rs). The count runs for every row with a sliceName
+ an open group at its path (PU:1044, outside the if/else). → **pddi 1/1**
(`Extension.extension.min` 0→4, four min=1 slices under auto-added url slicing).

**B. Java-exact `Utilities.rightTrim` on processRelativeUrls output** (PU:2306,
text.rs `java_right_trim`). Java's `processRelativeUrls` ALWAYS ends with
`return Utilities.rightTrim(s)` (after `markdown = markdown+" "`). Both walk
callers — `updateURLs` (emit.rs, every emitted element's markdown fields) and
`apply_profile_root_doco` (updatefromdef.rs, PU:2687/2690) — go through it, so
Java right-trims every markdown field. The Rust port never trimmed. Ported the
exact Java loop (`i>0` guard; the "trims to index 0 ⇒ returns \"\"" quirk kept
verbatim, unreachable for real multi-char markdown). → **be-vaccination 7/7**
(extension-root `definition` overlay with a trailing space on the profile root,
trimmed on the slice per the AGENTS.md Belgium note).

### Gate lines (verbatim)
- ipa: `R4 HARVEST CHECK: ok=12 failed=0 total=12` (out of the box)
- smart-app-launch: `R4 HARVEST CHECK: ok=6 failed=0 total=6` (out of the box)
- pddi: `R4 HARVEST CHECK: ok=1 failed=0 total=1` (after A)
- deid: `R4 HARVEST CHECK: ok=1 failed=0 total=1` (out of the box)
- darts: `R4 HARVEST CHECK: ok=1 failed=0 total=1` (out of the box)
- radiation-dose-summary: `R4 HARVEST CHECK: ok=4 failed=0 total=4` (out of the box)
- be-vaccination: `R4 HARVEST CHECK: ok=7 failed=0 total=7` (after B)
- pacio-toc: `R4 HARVEST CHECK: ok=4 failed=0 total=4` (out of the box)

No convert.rs / resolve.rs / package.rs / legacy changes for A or B.

**C. `PackageHackerR5.fixLoadedResource` port** (resolve.rs `fix_loaded_resource`,
BaseWorkerContext:417 → PackageHackerR5.java:14-135). Java calls this on every
package-loaded resource; ported the SD-content hacks (all gated by exact url +
`fhirVersion==4.0.1`, idempotent), applied in both `fetch_sd` and
`resolve_with_snapshot`:
  - **vitalsigns binding relaxation** (PH:90): Observation SD == or derived-from
    `vitalsigns` → `Observation.component.value[x]` binding (ucum-vitals-common
    |4.0.1) strength forced to `extensible`. This fires on the R4-core
    `bodyheight`/`bodyweight`/… bases (baseDefinition == vitalsigns). → the 7 AU
    Core vital-sign profiles (bloodpressure/bodyheight/bodytemp/bodyweight/
    heartrate/resprate/waistcircum), each `binding.strength` required→extensible.
    Traces were already decision-identical (307/307) — pure base-content divergence.
  - **extensions.r4 R5-only datatype removal** (PH:115): strip
    integer64/CodeableReference/RatioRange/Availability/ExtendedContactDetail from
    every element's `type[]`. Java scopes to `packageInfo.id ==
    "hl7.fhir.uv.extensions.r4"`; these five are genuinely R5-only and can't appear
    in any valid R4 resource, so the broader "every R4-loaded SD" scope is
    output-identical for this corpus and stays inside resolve.rs (no package.rs
    per-resource-package plumbing). → AU Core `au-core-rsg-sexassignedab`
    Extension.value[x] 54→49 types (Contributor, NOT in the list, correctly kept —
    matches the AGENTS.md note).
  - Also ported (not currently exercised but faithful+idempotent): iso21090-
    nullFlavor valueSet version strip (PH:45), DeviceUseStatement bodySite
    requirements link (PH:58), ServiceRequest code-binding description trim (PH:74,
    same text the legacy `publisher_native_text_quirk` handled downstream — now
    fixed at load, quirk becomes a no-op).
Verified no regression across the full green set incl. all xver-heavy IGs
(sdc/crd/dtr/pas/ndh).

**D. Empty-string markdown primitive drop** (text.rs `rewrite_string_field`).
FHIR JSON serialization omits empty-string primitives. `processRelativeUrls` can
render an inherited placeholder `comment: "-"` down to `""` (via the Java
rightTrim quirk — see B), and the serialized snapshot then has no comment at all.
Rust was emitting the literal `""`. Fix: after the rewrite, if the field is
empty, remove it. → **cdex 8/8** (`QuestionnaireResponse.subject` inherits
`comment: "-"` from us-core-questionnaireresponse; Java drops it). Regression-
verified across ips/sdc/mcode/pas/ndh/au-core + ladder.

### Gate lines (cont'd)
- mhd: `R4 HARVEST CHECK: ok=42 failed=0 total=42` (out of the box)
- eu-mpd: `R4 HARVEST CHECK: ok=4 failed=0 total=4` (out of the box)
- eu-eps: `R4 HARVEST CHECK: ok=23 failed=0 total=23` (out of the box)
- au-core: `R4 HARVEST CHECK: ok=26 failed=0 total=26` (after C)
- au-ps: `R4 HARVEST CHECK: ok=17 failed=0 total=17` (out of the box, post-C)
- dapl: `R4 HARVEST CHECK: ok=26 failed=0 total=26` (out of the box)
- drug-formulary: `R4 HARVEST CHECK: ok=19 failed=0 total=19` (out of the box)
- cdex: `R4 HARVEST CHECK: ok=8 failed=0 total=8` (after D)
- plan-net: `R4 HARVEST CHECK: ok=22 failed=0 total=22` (out of the box)
- pdex: `R4 HARVEST CHECK: ok=37 failed=0 total=37` (out of the box)
- subscriptions-backport: `R4 HARVEST CHECK: ok=9 failed=0 total=9` (out of the box)
- twpas: `R4 HARVEST CHECK: ok=43 failed=0 total=43` (out of the box)
- davinci-pas: `R4 HARVEST CHECK: ok=80 failed=0 total=80` (after E)

**E. newSliceAtEnd BackboneElement unfold used the wrong base index**
(sliced.rs). In `processPathWithSlicedBaseDefault`, after the getSiblings
pairing loop, `cur.base_cursor` has been mutated to the LAST base slice's last
child. Java's newSliceAtEnd child-unfold (PPP:1568) computes
`baseStart = cursors.base.indexOf(currentBase) + 1` — the slicing ANCHOR's
index, NOT the mutated cursor — while its `baseWalksInto` guard (PPP:1562) DOES
use the mutated `cursors.baseCursor`. Rust conflated both as `cur.base_cursor`,
so the Base/Element/BackboneElement unfold recursed over an empty window past
the slice group and emitted the new slice's children not at all. Captured
`anchor_base_idx = cur.base_cursor` at function entry (before the pairing loop)
and used it for `base_start`; left `base_walks_into` on the mutated cursor
(matches Java). → Da Vinci PAS `profile-claim` / `profile-claim-update`: the new
`Claim.supportingInfo:AdditionalInformation` / `:MessageText` BackboneElement
slices now materialize their full child sets (id/extension/…/value[x]/reason),
19 elements each. Regression-verified pas/twpas/mcode/sdc/au-ps + ladder.

**F. processOneMatch.walkIntoChildren choice-`[x]` type narrowing** (PPP:929-943,
simple.rs `process_simple_path_one_match`). When the diff walks into a
polymorphic `[x]` element via a CONCRETE choice path (base
`component.value[x]`, diff `component.valueQuantity`) and the emitted outcome
still has >1 type, Java narrows the outcome type list to the single concrete
type before unfolding: `t = tail(diff).substring(tail(base).len()-3)`
(→ `Quantity`), uncapitalize if primitive, `getByTypeName` filter (else
synthesize). Rust lacked this block, so `resolve_type_sd` saw the still-11-type
polymorphic list and unfolded into the `Element` fallback (no Quantity
value/comparator/unit/system/code children). Ported it; writes the narrowed
type back to the emitted row. → **us-core 70/70** (`us-core-pulse-oximetry`
`component:FlowRate.value[x]` / `:Concentration.value[x]` narrow to Quantity +
materialize 5 children each; trace decision-identical 376/376). Regression-
verified ips/mcode/au-core/davinci-pas/dapl/eu-eps/pdex/sdc + ladder.

- us-core: `R4 HARVEST CHECK: ok=70 failed=0 total=70` (after F)

**G. Q3 prune prohibited type-slices from the polymorphic anchor** (PU:879-891,
finalize.rs `prune_prohibited_type_slices`). A post-walk step (before setIds):
for each element with >1 type, `findTypeSlice` scans forward for a type slice at
the same path (pathMatches: exact, or `[x]`-base matched by a concrete
single-segment tail) whose single type code matches; if that slice is
`prohibited()` (max=0), the type is removed from the anchor. Was listed as
"live" in the finalize doc but never implemented. → **gematik-epa-medication
49/49** (`Medication.ingredient.item[x]` anchor: `itemReference` slice is
`max=0`, so `Reference` is pruned, leaving `[CodeableConcept]`; the walk was
already decision-identical 1298/1298). Regression-verified across
ips/mcode/us-core/qicore/davinci-pas/dapl/au-core/eu-eps/sdc + ladder.

### Gate lines (tail)
- qicore: `R4 HARVEST CHECK: ok=63 failed=0 total=63` (out of the box)
- gematik-epa-medication: `R4 HARVEST CHECK: ok=49 failed=0 total=49` (after G;
  correct closure = `de.gematik.epa.medication#1.3.4` per manifest)

### Summary — wave 3 batch 2 COMPLETE (24/24 IGs at legacy parity)
All published-package IGs green on ENGINE=walk at the legacy ok-counts. Seven
engine-wide Java-fidelity fixes landed (A slice-min finalize, B rightTrim, C
PackageHackerR5.fixLoadedResource, D empty-markdown drop, E newSliceAtEnd anchor
index, F choice-`[x]` type narrowing, G Q3 prohibited-type-slice prune), all
regression-clean across the entire green corpus. No legacy.rs/quirks.rs/goldens/
oracle/package.rs/convert.rs changes. resolve.rs: added `fix_loaded_resource`
(C). Owned files touched: walk/{finalize,slicing,sliced,simple,context}.rs,
resolve.rs, text.rs.

## Increment 13: cutover (task #9) — 2026-07-02, WAVE 4

### Fix 1 (pre-cutover): Java-exact PackageHackerR5 extensions.r4 removeIf scoping
The `fix_loaded_resource` R5-only-datatype removeIf (integer64/CodeableReference/
RatioRange/Availability/ExtendedContactDetail) was applied to EVERY R4-loaded SD.
Java (PackageHackerR5.java:115) scopes it EXACTLY to
`packageInfo.getId().equals("hl7.fhir.uv.extensions.r4")` — the OWNING package id,
not fhirVersion. Made Java-exact:
- `package.rs`: `ResourceIndexEntry` gained `package_id: Option<String>` (npm id =
  part of `<id>#<ver>` before `#`), threaded through `load_package` (indexed +
  versioned url entries), `scan_package_structure_definitions`, and
  `index_structure_definition`. New `PackageContext::package_id_for(query)` (url
  first, then path-match fallback for id/name lookups). Local-dir resources get
  `None` (Java loads those outside the package loader).
- `resolve.rs`: `fix_loaded_resource(sd, package_id)` gates the removeIf on
  `package_id == Some("hl7.fhir.uv.extensions.r4")`; both callers (`fetch_sd`,
  `resolve_with_snapshot`) pass `pkg.package_id_for(url).or(package_id_for(query))`.
- **Regression caught + fixed**: the first cut also flipped scan-fallback
  (empty-`.index.json`) resources from `local:true` to `local:false`, which broke
  `davinci-pas/profile-subscription` (80→79: the `subscriptions-backport.r4` base's
  inherited DomainResource `dom-*` constraint xpath extensions were dropped by the
  lenient package read instead of preserved by the full conversion the empty-index
  scan path historically took). Reverted that side effect — scan resources stay
  `local:true`; only the package_id is newly recorded. davinci-pas back to 80/80.
Gate: full 34-IG corpus re-run on ENGINE=walk — all green at wave-3 numbers
(au-core 26/26 confirms the removeIf still fires for extensions.r4-owned SDs;
davinci-pas/ndh/subscriptions-backport unaffected).

### Cutover: walk is the only engine
- `generate_snapshot` (public API) now runs the walk. Deleted `src/legacy.rs`
  (3994 lines), `src/quirks.rs` (426), `src/projection.rs` (1008),
  `tests/snapshot_parity.rs` (120 — walk_parity supersedes it; it already gates
  the one fixture snapshot_parity dropped, `r4-patient-card-ms`). Removed the
  `Engine` enum / `--engine` / `ENGINE` env / `run_engine`, the transitional
  `--native-r5`/`--output-r5` flags, and the `native_r5`/`apply_extension_root_doco`
  `SnapshotOptions` fields (walk always produces native R5 + applies extension-root
  doco). `SnapshotOptions` is now just `{ sort_differential }`.
- lib.rs items only legacy used, deleted: `structure_with_r4_snapshot`,
  `profile_with_snapshot`, `normalize_inherited_element`, `trim_inherited_text_fields`,
  `fill_missing_constraint_sources_on_constrained_element`, `structure_url_or`,
  `spec_url_for_structure`, `strips_non_inherited_extensions`, `spec_url_from_version`,
  `is_r4_spec_url`, `structure_source`, `snapshot_source_value`,
  `sort_differential_by_base` (+ `base_element_order`/`sort_key` + the sort unit test).
- merge.rs helpers only legacy used, deleted: `merge_extensions_from_definition`,
  `allows_duplicate_extension_url`, `TextMerge`/`merge_text_field`/`merge_markdown`,
  `merge_unique_values_prepend`/`merged_contains_by_semantic_key`/`merge_unique_by_key`,
  `dedupe_extension_values`(+`_except`), `is_root_element`,
  `merge_min_cardinality`/`merge_max_cardinality`, `has_slice_marker`. Kept
  `STRUCTUREDEFINITION_HIERARCHY_URL` (moved into merge.rs; still used by walk).
  package.rs: dropped `resource_has_loaded_snapshot` + `snapshot_cache` (legacy-only).
- `check-harvested-r4.sh`: removed the `ENGINE`/`ENGINE_ARGS` plumbing and the
  `--native-r5` flag from both invocation paths.
- **CAUGHT by the ips gate during cutover:** the cli.rs rewrite initially dropped
  the `--batch-list` arg case; the harvest gate failed loudly (`unknown option:
  --batch-list`) and it was restored. Nothing deleted-past-a-failure.
- Remaining `never used` warnings (`take_messages`, `resolve_by_path`) are
  pre-existing walk-internal stubs, NOT legacy orphans — left for the roadmap-item
  simplify pass.

### Load-bearing legacy-only items and their disposition
- `structure_with_r4_snapshot` / `profile_with_snapshot`: legacy's on-demand
  recursive base/profile snapshot generation. Walk has its own equivalent
  (`resolve::resolve_with_snapshot`, memoized in gen_cache), so these are dead —
  deleted, not ported.
- `normalize_inherited_element` + the R4→native-R5 output projection
  (`project_r4_snapshot_to_native_r5`, all of projection.rs): legacy applied R4→R5
  shape as an OUTPUT projection over R4-form snapshots. The walk's R5-internal load
  split (resolve.rs) means there is no output projection — the loaded base simply
  never carries R4 leftovers (worklog Increment 2). So projection.rs was entirely
  legacy-only; deleted. The two functions merge.rs borrowed from it
  (`remove_non_inherited_extensions_with_binding_policy`,
  `has_semantic_element_extensions`) were only reachable via legacy's
  `merge_extensions_from_definition` — all deleted together.

Gate after cutover: full 34-IG corpus green single-engine (no ENGINE/--native-r5),
955/955; ladder + convert_parity + `cargo test --workspace` green;
rust_sushi IPS 32/32 SD byte-parity (compiler untouched). Scorecard in
REWORK-PLAN.md §9.

## Least-confident areas (updated for the coordinator, post-Increment 11)
- ~~xver R5-backport base snapshots~~ RESOLVED: was the cross-SD contentReference
  stub + the legacy checkExtensionDoco guard, both fixed in Increment 11 (load
  path proven correct by the parallel resolve.rs investigation).
- ~~§3.6.3 anchor-children unfold~~ RESOLVED (Increment 9, ndh). No stubs remain
  in the sliced-base family; the only remaining documented dead-path is the
  preprocessor additional-base branch (SGPP:137-152, DEAD under oracle).
- **Stale goldens vs pinned oracle**: ecr/ersd-plandefinition (25 elements, two
  uv-extensions doco families) and r4-patient-card-ms — both walk outputs match
  the FRESH pinned oracle byte-identically; needs a coordinator golden-regen
  decision, not code.
- **`merge_mappings` renames** — the cross-version identity `renames` map and the
  R5 APPEND/DUPLICATE/IGNORE/OVERWRITE `mappingMergeMode` are unimplemented (R4
  corpus only exercises the simple diff-first-dedup path). Watch on any R5-target
  or cross-version-mapped profile.
- **`fill_out_from_base` allow-list** — matches Java's field list, but the
  polymorphic (fixed[x]/pattern[x]/minValue[x]/maxValue[x]) and additive-array
  (code/alias/constraint/extension) branches are newly exercised; watch for
  over/under-copy on profiles with rich type templates.
- **§3.6.2 fake-diff replay** — the `ctx.diff` swap/restore is exercised by
  mcode-genomic-variant (one CodeableConcept base slice); multi-slice or nested
  fake-diff cases are untested.
- **Cross-SD contentReference** — newly exercised by sdc-valueset (core R4
  ValueSet) and ersd (core PlanDefinition, recursively). The empty-diff cross-SD
  sub-branch mutates `cur.base` per Java (PPP:1234) but is near-dead in Java
  (`diffMatches.get(0)` on an empty list); watch any profile that actually
  reaches it. Same-SD matching switched to Java's forward id-scan — verified on
  the ladder + IPS + all 10 IGs, but any profile relying on duplicate element
  ids would now pick the FIRST occurrence (as Java does).
- **Java-exact checkExtensionDoco (no profiled-type guard)** — engine-wide
  behavior change verified against all gates; watch published-package sweeps for
  extension slices whose RESOLVABLE profile doco Java restores later via
  apply_profile_root_doco (the pairing is what keeps rich doco when it should
  stay).

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

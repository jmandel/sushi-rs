# Snapshot Decision-Trace Schema

This is the authoritative schema for the JSONL decision trace emitted by the
Java oracle when snapshot generation runs under the `snap-trace` instrumentation.
It is the gate substrate for the Rust walk engine: the Rust engine emits the
same records, and `snapshot/diff-trace.cjs` aligns the two by `seq` and reports
the first divergence. Output parity alone is not sufficient — two engines can
agree on output while disagreeing on decisions. See `REWORK-PLAN.md` §4.

## Producer

- fhir-core checkout: `/home/jmandel/hobby/fhir-perf/repos/fhir-core`,
  branch **`snap-trace`** off the pinned oracle commit `5c4d5a0ff`
  (jar `org.hl7.fhir.r5-6.9.10-SNAPSHOT.jar`).
- Tracer: `org.hl7.fhir.r5.conformance.profile.SnapshotTracer` (dependency-free,
  hand-rolled JSON, zero-overhead when disabled).
- Enable: JVM system property `-Dsnapshot.trace=<path>`. The file is truncated
  on first open; one JVM run == one trace file, appended one JSON object per line.
- Oracle wiring: `snapshot/oracle/gen-snapshot.sh` with env `TRACE=1` sets
  `-Dsnapshot.trace=<out>.trace.jsonl` (override with `TRACE_OUT=<path>`).
  Default behavior (TRACE unset) is unchanged.

**Hard invariant:** tracing is observation only. Snapshot output is
byte-identical with tracing on or off (verified by diffing two runs). No branch
label is invented; every label names an actual Java branch and follows the code
structure.

## Record shape

Each line is one JSON object:

```json
{"seq":N,"fn":"<method>","branch":"<label>","base":"<element id|null>","diff":"<element id|null>","x":{...optional extras}}
```

| Field    | Type            | Meaning |
|----------|-----------------|---------|
| `seq`    | integer         | Monotonic 0-based sequence number, assigned in emission order within a single JVM run. Alignment key for `diff-trace.cjs`. |
| `d`      | integer (opt)   | Nesting depth. Present and > 0 only while a *nested* snapshot generation runs (e.g. a dependency extension's snapshot generated on demand mid-walk). Omitted at top level (depth 0). Currently reserved: the depth counter is wired in the tracer (`enter()`/`exit()`) but the oracle entry point does not yet bracket nested generation, so in practice `d` is currently always absent. Rust may leave it absent. |
| `fn`     | string          | The Java method the decision was taken in. |
| `branch` | string          | The branch label. Enumerated below with its Java `file:line`. |
| `base`   | string \| null  | The current *base* ElementDefinition's id (falls back to path if no id). Null when no base element is in scope. |
| `diff`   | string \| null  | The current *differential* ElementDefinition's id (falls back to path). Null when no diff element is in scope. |
| `x`      | object (opt)    | Branch-specific extras. Present only when a label carries them; keys are documented per-label below. Omitted (not `{}`) when empty. |

`base`/`diff` use `ElementDefinition.getId()`, falling back to `getPath()` when
the element has no id (`SnapshotTracer.id(...)`). Ids are the stable identity FHIR
uses for elements/slices (e.g. `AllergyIntolerance.extension:abatement`).

### String escaping

Hand-rolled: `"` `\` `\b` `\f` `\n` `\r` `\t` escaped; control chars `< 0x20`
as `\u00xx`; everything else (incl. non-ASCII UTF-8) passed through literally.

## Branch labels

All line numbers are on branch `snap-trace` (base commit `5c4d5a0ff`), in
`org.hl7.fhir.r5/src/main/java/org/hl7/fhir/r5/conformance/profile/`. Each label
is placed at the trace-call site, which sits immediately inside the Java branch
it names; the cited line is the `trc.rec(...)` call.

### Orchestration — `ProfileUtilities.java`

| Label | file:line | Meaning | `x` keys |
|-------|-----------|---------|----------|
| `generateSnapshot.begin` | ProfileUtilities.java:827 | Top of a snapshot generation, right after the differential clone + before preprocessing. `base`/`diff` are the base/derived **profile URLs** (not element ids). | `baseElements`, `diffElements`, `derivation` |
| `generateSnapshot.walkComplete` | ProfileUtilities.java:848 | The recursive `ProfilePathProcessor.processPaths` walk returned; post-passes about to run. `diff` = derived profile URL. | `snapshotElements` |
| `generateSnapshot.diffNotConsumed` | ProfileUtilities.java:936 | A differential element has no `SNAPSHOT_GENERATED_IN_SNAPSHOT` back-pointer, i.e. it was **not consumed** by the walk (the "No match found …" error path). `diff` = the orphan element id. | `diffIndex`, `hasId`, `path` |

### Per-element merge — `ProfileUtilities.java`

| Label | file:line | Meaning | `x` keys |
|-------|-----------|---------|----------|
| `updateFromDefinition.entry` | ProfileUtilities.java:2614 | Entry to `updateFromDefinition(dest, source, …)` — the per-element merge of one diff element onto one base element. `base`=dest, `diff`=source. | `fromSlicer`, `trimDifferential`, `isSliceRoot` (source has a sliceName), `srcSD` |
| `updateFromDefinition.checkExtensionDoco` | ProfileUtilities.java:2620 | Fires only when `checkExtensionDoco(base)` returned true (this element is an extension/modifierExtension root whose doco got normalized: short=`Extension`, definition=`An Extension`, comment/req/alias/mapping cleared). Emitted right after the `checkExtensionDoco` call at PU:2622. | — |
| `replaceFromContentReference` | ProfileUtilities.java:1888 | A `contentReference` is being expanded: `contentReference` cleared and the target element's `type[]` copied in. `base`=contentReference target, `diff`=the referring outcome element. | `contentReference` (the referring element's original `#…` value), `targetPath` |

### Differential preprocessing — `SnapshotGenerationPreProcessor.java`

`ProfileUtilities.generateSnapshot` invokes `new SnapshotGenerationPreProcessor(this).process(diff, derived)` (PU:832) on the **cloned** differential before the walk.

| Label | file:line | Meaning | `x` keys |
|-------|-----------|---------|----------|
| `preprocess.mergeElements.insert` | SnapshotGenerationPreProcessor.java:811 | A missing slice element is injected positionally into the differential list (`elements.add(index, edc)`), marked `SNAPSHOT_PREPROCESS_INJECTED`. `diff` = the injected element's remapped id. | `index`, `path`, `sourceId` |
| `preprocess.merge.setField` | SnapshotGenerationPreProcessor.java:1006 | `merge(focus, base)` — an existing differential element (`focus`) absorbs missing fields from a base slice element. In-place field mutation, no structural change. `base`=source, `diff`=focus. One record per `merge` call (not per field). | — |
| `preprocess.insertMissingSparseElements.insertRoot` | SnapshotGenerationPreProcessor.java:1140 | A synthetic root element is prepended (`list.add(0, ed)`) because the list was empty or its first element had a dotted path. `diff` = the type name given to the new root. Additional-base path only. | — |
| `preprocess.insertMissingSparseElements.insertPathNode` | SnapshotGenerationPreProcessor.java:1170 | A missing intermediate path node is inserted (`list.add(i, root)`) to un-sparse the differential. `diff` = new node id. Additional-base path only. | `index`, `path` |
| `preprocess.mergeElementsFromAdditionalBase.replaceElementList` | SnapshotGenerationPreProcessor.java:177 | The whole differential element list is replaced with a freshly merged `output` list (`sourceSD.getDifferential().setElement(output)`). Fires only when the profile carries the `EXT_ADDITIONAL_BASE` extension. `base`/`diff` null. | `sourceSD`, `baseSD`, `outputElements` |

> Note on the additional-base path: `insertMissingSparseElements.*` and
> `mergeElementsFromAdditionalBase.replaceElementList` run only inside the
> `if (srcWrapper.hasExtension(EXT_ADDITIONAL_BASE))` branch (SGPP:137–152). On
> the normal path only `mergeElements.insert` and `merge.setField` fire.

### The walk — `ProfilePathProcessor.java`

#### Main cursor loop — `processPaths`

| Label | file:line | Meaning | `x` keys |
|-------|-----------|---------|----------|
| `processPaths.iteration` | ProfilePathProcessor.java:209 | One iteration of the main `while (baseCursor <= baseLimit …)` loop. Emitted before branch dispatch. `base`=current base element; `diff`=element at the diff cursor. | `basePath`, `baseCursor`, `baseLimit`, `diffCursor`, `diffLimit`, `diffMatches`, `baseHasSlicing`, `slicingDone`, `diffMatchIds` (ids of all diff matches for this base path) |
| `processPaths.dispatch.simplePath` | ProfilePathProcessor.java:216 | The `!currentBase.hasSlicing() || path == slicing.path` branch: base is unsliced → `processSimplePath`. | — |
| `processPaths.dispatch.slicedBase` | ProfilePathProcessor.java:225 | The else branch: base is already sliced → `processPathWithSlicedBase`. | — |

#### `processSimplePath` (4-way dispatch)

Mirrors `processSimplePath` PPP:293–303.

| Label | file:line | Meaning |
|-------|-----------|---------|
| `processSimplePath.emptyDiffMatches` | ProfilePathProcessor.java:308 | `diffMatches.isEmpty()` — the differential says nothing → copy base in (`processSimplePathWithEmptyDiffMatches`). |
| `processSimplePath.oneMatchingElement` | ProfilePathProcessor.java:312 | `oneMatchingElementInDifferential(...)` true → single diff element merge (`processSimplePathWithOneMatchingElementInDifferential`). |
| `processSimplePath.diffsConstrainTypes` | ProfilePathProcessor.java:315 | `diffsConstrainTypes(...)` true → the diff introduces a type slice (`processSimplePathWhereDiffsConstrainTypes`). |
| `processSimplePath.default` | ProfilePathProcessor.java:318 | Fallthrough → the diff introduces value slicing (`processSimplePathDefault`). |

#### `processSimplePathDefault` (introduce slicing on unsliced base)

| Label | file:line | Meaning | `x` keys |
|-------|-----------|---------|----------|
| `processSimplePathDefault.defaultBeforeSlices` | ProfilePathProcessor.java:344 | The "there's a default set before the slices" branch (PPP:322): the first diff match sets up an unnamed default before the named slices; it is recursively processed as the slice base. | `sliceGroupSize`, `sliceGroupIds` |
| `processSimplePathDefault.acceptDiffSlicing` | ProfilePathProcessor.java:365 | The else branch (PPP:337): accept the differential slicing at face value; clone the base element as the slicing anchor. | `sliceGroupSize`, `sliceGroupIds`, `cloneSource` (source SD url) |
| `processSimplePathDefault.autoAddedSlicing` | ProfilePathProcessor.java:375 | `!diffMatches.get(0).hasSlicing()` → the anchor gets synthetic extension slicing (`makeExtensionSlicing`, `SNAPSHOT_auto_added_slicing`). | — |
| `processSimplePathDefault.copyDiffSlicing` | ProfilePathProcessor.java:382 | The anchor copies the differential's own `slicing` block. | — |
| `processSimplePathDefault.sliceGroupBaseDefinition` | ProfilePathProcessor.java:412 | The first slice-group element has **no** sliceName → treated as the base definition of the slice; `updateFromDefinition` applied to the anchor. | — |
| `processSimplePathDefault.unfoldType` | ProfilePathProcessor.java:429 | Under the unnamed-anchor case, the diff walks into a new type → recurse into the type SD (`getTypeForElement`). | `typeSD` |
| `processSimplePathDefault.contentReferenceInlineDump` | ProfilePathProcessor.java:455 | The anchor has a `contentReference` and no base children → the referenced element's children are cloned inline into the output. | `contentReference`, `count` |
| `processSimplePathDefault.sliceGroupNamedFirst.checkExtensionDoco` | ProfilePathProcessor.java:477 | The else at PPP:460: the first slice-group element **has** a sliceName → the anchor only gets `checkExtensionDoco` (no full updateFromDefinition). | — |
| `processSimplePathDefault.processSlice` | ProfilePathProcessor.java:495 | Per-slice loop body (PPP:467): recursively process one named slice against the base scope. | `sliceIndex`, `sliceName` |

#### `processSimplePathWhereDiffsConstrainTypes` (introduce type slicing)

| Label | file:line | Meaning | `x` keys |
|-------|-----------|---------|----------|
| `processSimplePathWhereDiffsConstrainTypes.entry` | ProfilePathProcessor.java:564 | Entry; `shortCut` = the "dived straight into a type slice" form. | `shortCut`, `typeSlices`, `basePath` |
| `processSimplePathWhereDiffsConstrainTypes.processTypeSlice` | ProfilePathProcessor.java:689 | Per-type-slice loop body: process one type slice. | `sliceIndex`, `sliceName` |
| `processSimplePathWhereDiffsConstrainTypes.overwriteSlicingToOpen` | ProfilePathProcessor.java:738 | Not all allowed types are covered by slices and this is **not** the `extension.value` shortcut → slicing rules overwritten to OPEN (PPP:665). | `unusedTypes` (allowed types with no slice) |

#### `processSimplePathWithOneMatchingElementInDifferential`

| Label | file:line | Meaning | `x` keys |
|-------|-----------|---------|----------|
| `processOneMatch.templateFromReslice` | ProfilePathProcessor.java:766 | Reslice case (`lid.contains("/")`): the merge template is fetched from the already-built result snapshot by base id. | — |
| `processOneMatch.templateFromProfile` | ProfilePathProcessor.java:846 | The diff constrains to a profiled Extension/Resource type → template is the profile snapshot's first element merged with the slicer. | `profileSD`, `srcElement` |
| `processOneMatch.templateFromBase` | ProfilePathProcessor.java:864 | Default template = a copy of the current base element. | `cloneSource` (base source SD url) |
| `processOneMatch.applySliceName` | ProfilePathProcessor.java:887 | The diff element has a sliceName → slice-name/min handling + `checkToSeeIfSlicingExists`. | `sliceName` |
| `processOneMatch.walkIntoChildren` | ProfilePathProcessor.java:927 | The merged element has children in the diff and the base does not walk into them → descend (into contentReference or type). | `hasContentReference`, `typeCount` |
| `processOneMatch.unfoldType` | ProfilePathProcessor.java:1026 | The non-contentReference sub-branch of walk-into: recurse into the resolved datatype SD to unfold its children. | `typeSD` |

#### `processSimplePathWithEmptyDiffMatches`

| Label | file:line | Meaning | `x` keys |
|-------|-----------|---------|----------|
| `processEmptyDiffMatches.walkIntoBaseChildren` | ProfilePathProcessor.java:1188 | The diff walks into this element and the base has children (not a new type) → recurse over base children. | — |
| `processEmptyDiffMatches.unfoldType` | ProfilePathProcessor.java:1273 | The diff walks in but base has no children → unfold from the datatype SD. | `typeSD` |

#### `processPathWithSlicedBase` (3-way dispatch)

| Label | file:line | Meaning | `x` keys |
|-------|-----------|---------|----------|
| `processPathWithSlicedBase.emptyDiffMatches` | ProfilePathProcessor.java:1328 | `diffMatches.isEmpty()` → copy the inherited slice group (`…AndEmptyDiffMatches`). | — |
| `processPathWithSlicedBase.diffsConstrainTypes` | ProfilePathProcessor.java:1333 | The diff adds type slices onto an already-sliced base (`…WhereDiffsConstrainTypes`). | — |
| `processPathWithSlicedBase.default` | ProfilePathProcessor.java:1338 | Default: the base slicing is **inherited**; the diff constrains/extends existing slices (`…Default`). | `diffMatches`, `diffMatchIds`, `inheritedSlicing` (always true here — marks that this slice group came from the base) |

#### `processPathWithSlicedBaseDefault`

| Label | file:line | Meaning | `x` keys |
|-------|-----------|---------|----------|
| `processSlicedBaseDefault.copyBackboneChildren` | ProfilePathProcessor.java:1422 | Base is a BackboneElement → copy its children before touching slices. | `count` |
| `processSlicedBaseDefault.matchExistingSlice` | ProfilePathProcessor.java:1448 | A base slice matches a diff slice by sliceName → recursively merge the diff onto it. | `sliceName`, `diffpos` |
| `processSlicedBaseDefault.copyUnmatchedBaseSlice` | ProfilePathProcessor.java:1474 | A base slice has no matching diff slice → copied through unchanged (plus its base children). | `sliceName`, `cloneSource` |
| `processSlicedBaseDefault.newSliceAtEnd` | ProfilePathProcessor.java:1513 | A remaining diff slice is **new** (introduced at the end of the slice group). | `sliceName`, `diffpos` |

#### `processPathWithSlicedBaseWhereDiffsConstrainTypes`

| Label | file:line | Meaning | `x` keys |
|-------|-----------|---------|----------|
| `processPathWithSlicedBaseWhereDiffsConstrainTypes.entry` | ProfilePathProcessor.java:1660 | Entry; type slicing over an already-sliced base. | `shortCut`, `typeSlices`, `basePath` |
| `processPathWithSlicedBaseWhereDiffsConstrainTypes.processTypeSlice` | ProfilePathProcessor.java:1778 | Per-type-slice loop body; may bind to an existing base type slice. | `sliceIndex`, `type`, `matchedBaseSlice` |
| `processPathWithSlicedBaseWhereDiffsConstrainTypes.unhandledBaseSliceFakeDiff` | ProfilePathProcessor.java:1807 | A base type slice was not matched by any diff slice → replayed against an empty ("fake") differential to copy it through. | `basePath` |

#### `processPathWithSlicedBaseAndEmptyDiffMatches`

| Label | file:line | Meaning | `x` keys |
|-------|-----------|---------|----------|
| `processSlicedBaseEmptyDiffMatches.walkIntoBaseChildren` | ProfilePathProcessor.java:1847 | The diff walks into a slice group and the base has children → recurse. | — |
| `processSlicedBaseEmptyDiffMatches.unfoldType` | ProfilePathProcessor.java:1859 | The diff walks in but base has no children → unfold from datatype SD. | `typeSD` |
| `processSlicedBaseEmptyDiffMatches.copyAllBaseSlices` | ProfilePathProcessor.java:1891 | The diff says nothing about this slice group → copy the whole group (all slices + children) through. | `cloneSource` |

## Slice-group start/end signal

There is no dedicated "slice group start/end" record. A slice group is delimited
implicitly by the walk:

- **Group start / introduced-vs-inherited:** `processSimplePathDefault.acceptDiffSlicing`
  (or `.defaultBeforeSlices`) marks a group **introduced** by the derived profile
  on a previously-unsliced base; `processPathWithSlicedBase.default` (with
  `x.inheritedSlicing=true`) marks a group **inherited** from the base. The `x`
  extras carry `sliceGroupIds` / `diffMatchIds` = the diff ids in the group.
- **Per-slice members:** `processSimplePathDefault.processSlice`,
  `processSlicedBaseDefault.{matchExistingSlice,newSliceAtEnd,copyUnmatchedBaseSlice}`,
  and the `processTypeSlice` labels emit one record per slice with `sliceName`/`sliceIndex`.
- **Group end:** the next `processPaths.iteration` at the sliced base's sibling
  path (the walk advances `baseCursor` past the group). Consumers detect the end
  by watching `basePath` change.

## Element cloning / unfold / contentReference — quick index

- **Base children cloned into output:** `processSlicedBaseDefault.copyBackboneChildren`,
  `processSlicedBaseDefault.copyUnmatchedBaseSlice`,
  `processSlicedBaseEmptyDiffMatches.copyAllBaseSlices`,
  `processSimplePathDefault.contentReferenceInlineDump`. Source SD is in
  `x.cloneSource` where applicable; source element id is `base`.
- **Type unfolded:** any `*.unfoldType` label; the unfolded type/profile SD is `x.typeSD`.
  Profile-template unfold: `processOneMatch.templateFromProfile` (`x.profileSD`).
- **contentReference expanded:** `replaceFromContentReference` (always), plus the
  inline-dump variant `processSimplePathDefault.contentReferenceInlineDump`.

## Branches deliberately NOT instrumented

These are real Java branches left un-traced, with the reason:

- **`updateFromDefinition` per-field merges** (PU:2694+, dozens of
  `derived.hasXElement()` field copies). Reason: field-level; would flood the
  trace and is downstream of the `updateFromDefinition.entry` record. Per-field
  behavior is gated by output parity, not decision parity. The single `.entry`
  record marks the decision (which base/diff pair merged, slice-root/doco flags).
- **`preprocess.merge.setField` per-field body** (SGPP:1006+). Same reasoning —
  one record per `merge` call, not per field.
- **Exception/precondition throws** (illegal slice, "not done yet",
  discriminator/order/rule mismatch, closed-slicing-extended, etc.). Reason: they
  abort generation; a Rust divergence there surfaces as a hard error or a missing
  record, not a silent decision difference. The consumed/unconsumed outcome is
  already captured by `generateSnapshot.diffNotConsumed`.
- **`checkToSeeIfSlicingExists` internal auto-slicing inserts** (PPP:973+).
  Reason: bookkeeping helper invoked from `processOneMatch.applySliceName`, which
  is already traced; its effect shows up as extra result rows.
- **`mergeElementsFromAdditionalBase`'s per-child `output.add(...)`** (SGPP:169,
  184, 193, …). Reason: the wholesale `replaceElementList` record captures the
  net effect (final `outputElements` count); per-child would be noisy and the
  additional-base path is rare.
- **`SliceInfo` bookkeeping** (SGPP inner class). Reason: mutates private helper
  lists, not the differential — not a snapshot decision.
- **`debug*` / logging branches** (`debugProcessPathsEntry`,
  `debugProcessPathsIteration`, `debugCheck`, the `log.debug` dumps). Reason:
  diagnostics, not decisions.

# Walk Decision Tree — Definitive Porting Spec for the Rust Snapshot Walk Engine

> **Status:** authoritative for the wave-1 rework (feeds REWORK-PLAN task #6, the walk
> implementation). Every behavioral claim carries a `file:line` citation against the
> **pinned oracle commit `5c4d5a0ff`** (`org.hl7.fhir.r5-6.9.10-SNAPSHOT`). Where the
> reverse-engineered `snapshot-fodder/SPECIFICATION.md` disagrees with the current source,
> **the source wins** and the disagreement is called out.
>
> **Anchor-drift caveat (coordinator note):** this spec was authored while the working
> tree carried the (then-uncommitted) `snap-trace` tracer insertions, so `PPP:`/`PU:`/`PRE:`
> line numbers may be shifted by a few lines relative to pristine `5c4d5a0ff` in tracer-
> instrumented regions. Treat anchors as "within ±10 lines"; the branch labels and method
> names are exact. Anchors are best resolved against the `snap-trace` branch tip.
>
> **Citation keys** (all under `org.hl7.fhir.r5/src/main/java/org/hl7/fhir/r5/`):
> - `PU:n` → `conformance/profile/ProfileUtilities.java:n`
> - `PPP:n` → `conformance/profile/ProfilePathProcessor.java:n`
> - `PRE:n` → `conformance/profile/SnapshotGenerationPreProcessor.java:n`
> - `MA:n` → `conformance/profile/MappingAssistant.java:n`
> - `UDN:n` → `utils/UserDataNames.java:n`
> - `PPS:n` → `conformance/profile/ProfilePathProcessorState.java:n`
> - `PSP:n` → `conformance/profile/PathSlicingParams.java:n`
> - `ER:n` → `conformance/ElementRedirection.java:n`
>
> **Oracle driver configuration** (`snapshot/oracle/SnapOracleR4.java`): the ONLY non-default
> knobs are `setForPublication(true)` (line 136) and `setNewSlicingProcessing(true)` (line 137).
> Everything else is Java default: `autoFixSliceNames=false` (PU:442), `debug=false` (PU:431),
> `mappingMergeMode=APPEND` (PU:449), `allowUnknownProfile=ALL_TYPES` (PU:448),
> `suppressIgnorableExceptions=false` (PU:154), `wantThrowExceptions=false` (messages list is
> non-null, PU:462/481), `xver` lazily created (PU:1198). The context FHIR version is **R4
> (4.0.1)** after `VersionConvertorFactory_40_50` conversion, so **`isR4Plus(version)` is TRUE**.
> `--sort` is *off* by default in the corpus path (sortDifferential only runs when passed);
> `setIds(derived, true)` is called before generation (SnapOracleR4:142). Branches that are dead
> under this exact configuration are marked **[DEAD under oracle config]**.

---

## 0. Table of contents

1. Pipeline orchestration (`generateSnapshot`)
2. Walk state (`ProfilePathProcessor` + `ProfilePathProcessorState`) → Rust `WalkContext`/`WalkFrame`
3. The decision tree (main loop + every `process*` method)
4. `updateFromDefinition` per-field merge → Rust leaf-function map
5. userData / side-channel inventory → `Annotations`
6. Message / error emission points
7. Proposed Rust skeleton (`src/walk/`)
8. Ambiguities & oracle experiments
9. Trace points (proving decision isomorphism)
10. Stale-anchor audit vs SPECIFICATION.md

---

# 1. PIPELINE ORCHESTRATION — `generateSnapshot` (PU:740–1112)

`generateSnapshot(base, derived, url, webUrl, profileName)` is the whole of Layer A. The walk
(`ProfilePathProcessor.processPaths`, PU:844) sits in the middle; there are pre-passes and
post-passes around it. Exact sequence:

### 1.1 Pre-walk (PU:741–842)

| # | Step | Lines | Notes |
|---|---|---|---|
| P1 | Null guards: base/derived non-null | PU:741–746 | throws `NO_BASE_PROFILE_PROVIDED` / `NO_DERIVED_STRUCTURE_PROVIDED` |
| P2 | `checkNotGenerating(base)`, `checkNotGenerating(derived)` | PU:747–748 | rejects using a mid-generation SD (guards half-built snapshots) |
| P3 | base/derived must `hasType`; derived must `hasDerivation` | PU:750–758 | |
| P4 | If `derivation==CONSTRAINT`: `base.type == derived.type` (else throw) | PU:759–760 | SPECIALIZATION may differ |
| P5 | **Base-snapshot backfill (recurse)**: if `!base.hasSnapshot()`, resolve `base.baseDefinition` → `sdb`, recurse `generateSnapshot(sdb, base, …)` | PU:762–768 | bottom-up along derivation chain. **[Usually DEAD under oracle config]** — the base is a core/dependency SD that already ships a snapshot. |
| P6 | `fixTypeOfResourceId(base)` — rewrites `Resource.id` type to `http://hl7.org/fhirpath/System.String` + `id` fhir-type extension (only under R4+; PU:1305–1322) | PU:769 | mutates the **base**. R4 config → runs. |
| P7 | If base has `EXT_TYPE_PARAMETER` → `checkTypeParameters` | PU:770–772 | **[DEAD]** unless base is a type-parameterized logical model |
| P8 | **Circular guard**: if `snapshotStack.contains(derived.url)` → throw `CIRCULAR_SNAPSHOT`; else `derived.setGeneratingSnapshot(true)`, `snapshotStack.add(derived.url)` | PU:774–778 | termination guarantee for re-entrant type/xver expansion |
| P9 | `oldCopyUserData = Base.isCopyUserData(); Base.setCopyUserData(true)` | PU:779–780 | **critical**: for the whole body, `ElementDefinition.copy()` **propagates userData**. The Rust port carries annotations through clones the same way. |
| P10 | Normalize `webUrl` (ensure trailing `/`); set `defWebRoot` if null | PU:783–787 | webUrl only affects markdown-relative rewriting; not decision-relevant to element structure |
| P11 | `derived.setSnapshot(new empty snapshot)` | PU:788 | |
| P12 | `checkDifferential(diff.element, derived.typeName, url)` | PU:791 | structural legality of every diff path (see §6); throws only |
| P13 | `checkDifferentialBaseType(derived)` | PU:792 | first diff element type check (throws Error if first element has a type and kind≠LOGICAL, unless `wantFixDifferentialFirstElementType`) — PU:1332. Oracle prepends a bare root element (SnapOracleR4:143–148) so the first element has no type → **passes trivially**. |
| P14 | `copyInheritedExtensions(base, derived, webUrl)` | PU:808 | copies SD-level extensions from base→derived per `EXT_SNAPSHOT_BEHAVIOR` action (default `defer` = copy if absent); PU:1243. SD-level, not element-level. |
| P15 | `findInheritedObligationProfiles(derived)` | PU:810 | populates the `obligationProfiles` field from `EXT_OBLIGATION_INHERITS_*`; PU:1220. **[Usually DEAD]** — empty in most corpus profiles. |
| P16 | **Clear** `SNAPSHOT_GENERATED_IN_SNAPSHOT` on every *original* differential element | PU:820–821 | resets the "was this consumed" flag |
| P17 | **`diff = cloneDiff(derived.getDifferential())`** | PU:824 | deep-copies the differential into a working list; each clone gets `SNAPSHOT_diff_source` → its original (PU:1503). **All walk mutation happens on this clone; provenance migrates back in P-post.** |
| P18 | trace: `generateSnapshot.begin` | PU:826–828 | (see §9) |
| P19 | **`new SnapshotGenerationPreProcessor(this).process(diff, derived)`** | PU:830 | slice-group trailing-property push-down (see §3.0). Mutates `diff` in place. |
| P20 | `baseSnapshot = base.getSnapshot()`; if `SPECIALIZATION`: `baseSnapshot = cloneSnapshot(baseSnapshot, base.typeName, derivedType)` (renames type prefix in id+path) | PU:832–837 | **[DEAD for CONSTRAINT profiles]** — the corpus is nearly all CONSTRAINT. |
| P21 | `mappingDetails = new MappingAssistant(mappingMergeMode, base, derived, version, suppressedMappings)` | PU:842 | builds the master mapping list + rename map (MA:45) |

### 1.2 The walk (PU:844)

```
ProfilePathProcessor.processPaths(this, base, derived, url, webUrl, diff, baseSnapshot, mappingDetails);
```

This is §3. It appends elements to `derived.getSnapshot().getElement()` in base order and records
messages. `diff` (the clone) is the differential list the walk consumes.

### 1.3 Post-walk (PU:846–1092)

| # | Step | Lines | Notes |
|---|---|---|---|
| Q0 | trace: `generateSnapshot.walkComplete` | PU:847–849 | |
| Q1 | `checkGroupConstraints(derived)` | PU:851 | choice-group `min=1` collapse: for each element (not sliced, max≠0), find choice groups; if exactly one member is mandatory, set the OTHER members' `max=0` and delete their subtrees. PU:1348. Throws `Error("huh?")` on dup child name, `Error(...two mandatory...)` on conflict. |
| Q2 | **SPECIALIZATION fill-in**: for each `diff` element lacking `SNAPSHOT_GENERATED_IN_SNAPSHOT` with `.` in path: if an element already exists at path → `updateFromDefinition` onto it; else copy+URL-fix+insert at child position, and if it walks into a single type → `addInheritedElementsForSpecialization` | PU:852–877 | **[DEAD for CONSTRAINT]** |
| Q3 | **Prune prohibited type-slices**: for each output element with >1 type, drop any type whose type-slice (found by `findTypeSlice`) is `prohibited()` (max=0) | PU:879–891 | live; affects `[x]` choice type narrowing |
| Q4 | **Root untyped assertion**: if `kind != LOGICAL` and `output[0].type` non-empty → throw `Error(TYPE_ON_FIRST_SNAPSHOT_ELEMENT…)` | PU:892–893 | PC-2 |
| Q5 | `mappingDetails.update()` | PU:894 | rewrites `derived.mapping` (SD-level) to only the used+inherited mappings (MA:150) |
| Q6 | **`setIds(derived, false)`** | PU:896 | assigns every `element.id` in BOTH differential and snapshot via the path+sliceName algorithm (PU:4285/4355). Emits `SAME_ID_ON_MULTIPLE_ELEMENTS` ERROR on collision. **This is what the oracle output ids reflect** — the id you see in golden output is generated here, not authored. |
| Q7 | **Provenance back-migration + PC-1 unconsumed-diff check** (see §6) | PU:918–963 | for each `diff` element: migrate `SNAPSHOT_DERIVATION_*` back to `SNAPSHOT_diff_source` original; if lacks `SNAPSHOT_GENERATED_IN_SNAPSHOT` and has id → ERROR "No match found…"; else cross-link snapshot→diff via `SNAPSHOT_DERIVATION_DIFF`. Aggregated → `handleError`. |
| Q8 | **Normalize mappings + constraint sources**: trim `mapping.map` whitespace; absolutize each `constraint.source` that is not an absolute URL to `http://hl7.org/fhir/StructureDefinition/<res>#<path>` | PU:965–983 | element-level, on final snapshot |
| Q9 | **SPECIALIZATION ensure `.base`**: for each output element without `.base`, set base from own path/min/max | PU:984–990 | **[DEAD for CONSTRAINT]** (constraint elements get `.base` in `updateFromBase`) |
| Q10 | **Slice cardinality + path discipline** (`ElementDefinitionCounter`): open a counter at each `hasSlicing()` element; on close (dedent) `checkMin`/`checkMax`/`checkMinMax`. Auto-correct min iff `SNAPSHOT_auto_added_slicing`, else emit slice-min mismatch (`forPublication ? ERROR : INFORMATION`, ignorable). Non-root path must start with `type+"."` (else Error). `sliceName` with no open group → ERROR (ignorable). Duplicate sliceName → ERROR (ignorable) | PU:996–1051 | PC-3, PC-4. `forPublication=true` → slice-min becomes ERROR. |
| Q11 | **Profile/targetProfile reference validation**: resolve each type profile (incl. xver); unresolved → WARNING; else type-compatibility check via `isCompatibleType` (→ `handleError` = ERROR) | PU:1053–1092 | resolves but does **not** pin (§Layer boundary) |
| Q12 (catch) | On any exception: `derived.setSnapshot(null); derived.setGeneratingSnapshot(false); rethrow` | PU:1093–1100 | failure atomicity |
| Q13 (finally) | restore `copyUserData`; `derived.setGeneratingSnapshot(false)`; `snapshotStack.remove(url)` | PU:1101–1105 | |
| Q14 | If `base.version != null` → stamp `EXT_VERSION_BASE` (base version) on the snapshot | PU:1106–1108 | the **only** version write Layer A performs |
| Q15 | `derived.setGeneratedSnapshot(true)`; `derived.setUserData(SNAPSHOT_GENERATED_MESSAGES, messages)` | PU:1109–1111 | |

> **Porting note.** For the CONSTRAINT-only corpus the live post-passes are Q1, Q3, Q4, Q5,
> Q6, Q7, Q8, Q10, Q11. Q2/Q9/Q20 (SPECIALIZATION) are dead. The Rust FINALIZE stage
> (REWORK-PLAN stage 5) implements exactly these. **Q6 setIds must run before Q7** because Q7's
> error messages reference `e.getId()`.

---

# 2. WALK STATE

The walk splits state into two objects; the Java realizes the per-frame object as an **immutable
builder** (`@With` lombok — every `.withX()` returns a *new* `ProfilePathProcessor` sharing the
`ProfileUtilities`), and the cursor object as **mutable**. Read this split literally: recursion
creates a new frame with narrowed limits and rewritten context, while cursors advance within a
frame and are sometimes shared across a recursion.

### 2.1 `ProfilePathProcessorState` (PPS:11–21) — the mutable cursor object

| Field | Meaning | Rust |
|---|---|---|
| `baseSource: StructureDefinition` (PPS:12) | the SD that owns `base` (may be a data type when unfolding) | `base_source_url: String` (index into loaded SDs) |
| `base: SnapshotComponent` (PPS:13) | the element list currently being walked (base snapshot, or a data type's snapshot) | `base: Rc<Vec<Value>>` (or index into a store) |
| `baseCursor: int` (PPS:15) | index of the base element in focus | `base_cursor: usize` |
| `diffCursor: int` (PPS:16) | forward-only low end of the diff match window | `diff_cursor: usize` |
| `contextName: String` (PPS:18) | diagnostic; also set to `dt.getUrl()` when unfolding a type (used only for messages) | `context_name: String` |
| `resultPathBase: String` (PPS:19) | the path prefix every emitted element must start with; set on the first emitted element, then asserted | `result_path_base: Option<String>` |

> `contextName`/`resultPathBase` are diagnostic-ish but `resultPathBase` is **load-bearing**: the
> "ADDING_WRONG_PATH" throws (PPP:364, 823, 1074, 1319, 1350, 1391, 1666, 1711) gate on it. It is
> set from the first emitted element's path (PPP:820, 1071) and never reset within a call chain
> that shares the cursor object.

### 2.2 `ProfilePathProcessor` immutable frame fields (PPP:61–122)

| Field | Meaning | Rebuilt per recursion? | Rust (`WalkFrame`) |
|---|---|---|---|
| `profileUtilities` (PPP:62) | back-reference to shared engine (context, config, messages) | shared | `ctx: &WalkContext` |
| `result` (PPP:70) | the output element list (`derived.snapshot`) — **shared, append-only** across all frames | shared | `&mut Vec<Value>` in WalkContext |
| `differential` (PPP:74) | the diff clone list being consumed — **shared** except the fake-diff sub-walk (PPP:1643) which swaps it | usually shared | `diff: Rc<RefCell<Vec<Value>>>` |
| `baseLimit` (PPP:78) | inclusive high end of the base window | **yes** | `base_limit: usize` |
| `diffLimit` (PPP:82) | inclusive high end of the diff window (−1 if no diff) | **yes** | `diff_limit: isize` |
| `url`, `webUrl`, `profileName` (PPP:86–94) | derived url, web root for markdown, running profile-name label | url/webUrl mostly stable; profileName grows via `pathTail` | `url`, `web_url`, `profile_name` |
| `contextPathSource` (PPP:98) | source-side path prefix for `fixedPathSource` rewriting (null at top) | **yes** | `context_path_source: Option<String>` |
| `contextPathTarget` (PPP:102) | dest-side path prefix for `fixedPathDest` | **yes** | `context_path_target: Option<String>` |
| `trimDifferential` (PPP:106) | passed to `updateFromDefinition`; when true, deletes base-equal props from the **diff clone** | **yes** (set true only inside `processPathWithSlicedBaseDefault` when base slicing is CLOSED, PPP:1335) | `trim_differential: bool` |
| `redirector: List<ElementRedirection>` (PPP:110) | contentReference redirection stack; consumed by `fixedPathSource/Dest` | **yes** (push via `redirectorStack`, reset to empty or null in type unfolds) | `redirector: Vec<ElementRedirection>` |
| `sourceStructureDefinition` (PPP:114) | the SD whose url is stamped into `SNAPSHOT_BASE_MODEL` and `updateFromBase` | **yes** (changes to the type SD / contentRef target SD when unfolding) | `source_sd_url: String` |
| `derived` (PPP:118) | the profile being generated | stable | `derived_url: String` |
| `slicing: PathSlicingParams` (PPP:122) | the slice context for `oneMatchingElementInDifferential` dispatch | **yes** (see §2.3) | `slicing: SlicingParams` |

`ElementRedirection` (ER:7–28): `{ path: String, element: ElementDefinition }`. `redirectorStack`
(PU:1867) is a pure functional push — copies the list and appends `new ElementRedirection(outcome,
path)`. Rust: `Vec<ElementRedirection>` cloned on push.

### 2.3 `PathSlicingParams` (PSP:15–37) — the slice context

| Field | Meaning |
|---|---|
| `done: bool` (PSP:17) | "we are already inside a slice / a slice group is active" — the key input to `oneMatchingElementInDifferential` (PPP:1799) |
| `elementDefinition: ElementDefinition` (PSP:19) | the slicing anchor element (may be null) — its `slicing.rules` is read at PPP:802/806 for slice-min defaulting |
| `path: String` (PSP:21) | the sliced path; when `currentBasePath.equals(slicing.path)` the loop treats an already-slicing base as unsliced (PPP:208) |
| `slices: List<ElementDefinition>` (PSP:23) | the sibling diff slices (`withDiffs` copies `diffMatches[1..]`, PSP:31) — size read at PPP:806 for shared-min logic |

Default (`new PathSlicingParams()`, PSP:25): `done=false, elementDefinition=null, path=null`.

> **Rust `WalkContext` (long-lived) vs `WalkFrame` (per-recursion).** Put the shared, mutable
> things in `WalkContext`: the output vec, the diff vec, messages, the loaded-SD store, config
> flags, the `snapshotStack`, and the `Annotations` sidecar (§5). Put the per-recursion window +
> context-path + redirector + slicing in `WalkFrame`, passed by value (cheap clone) into each
> recursion. The `ProfilePathProcessorState` cursor becomes a `&mut WalkCursor` **shared** across a
> recursion in exactly the cases the Java shares its `cursors` object vs. constructs a new
> `ProfilePathProcessorState` — track this precisely (§3), it changes cursor advancement.

---

# 3. THE DECISION TREE

Structure mirrors the code. **Clone timing is called out explicitly at every emission** — this is
the #1 thing the legacy engine got wrong. The rule: **the outcome is cloned from `currentBase`
(the base snapshot element) at the moment of emission, THEN `updateFromDefinition` overlays the diff
onto it.** The diff element is never the clone source for the emitted element (except contentRef /
type-unfold children, which clone from the *type/target snapshot*, and the reslice-template cases,
which clone from a previously-emitted result element).

## 3.0 Preprocessor — `SnapshotGenerationPreProcessor.process` (PRE:134)

Runs on the diff clone before the walk (P19). Two responsibilities; only the first is live for the
corpus:

**(a) `processSlices(diff, srcWrapper)` (PRE:688).** Slice-group trailing-property push-down:
1. First pass (PRE:690–711): walk diff building a `SliceInfo` tree. An element opens a slice group
   iff `hasSlicing()` and NOT `isExtensionSlicing` (PRE:1077: extension slicing = name in
   {extension, modiferExtension[sic]}, rules OPEN, ordered false/absent-treated-as, exactly one
   discriminator VALUE:url). Elements with a `sliceName` at the group path become slices
   (`newSlice`); other in-scope elements become `sliceStuff` (the trailing shared properties)
   **only while no slice has appeared yet** (PRE:113–120: `add` appends to `sliceStuff` only if
   `slices == null`). `getSlicing` (PRE:1088) scopes by path-prefix and auto-closes a group when a
   shorter path appears.
2. Complexity guard (PRE:713–723): if any `sliceStuff` element itself opens non-extension slicing →
   log warning `UNSUPPORTED_SLICING_COMPLEXITY` and **return early** (no push-down). 
3. Backward pass (PRE:726–736): for each slicing with non-empty `sliceStuff` and ≥1 slice, for each
   slice call `mergeElements(diff.element, sliceStuff, slice, slicer)` (PRE:743).
4. `mergeElements` (PRE:743–810): for each `sliceStuff` element, find it within
   `[startOfSlice, endOfSlice]` by `elementsMatch` (path-match + sliceName-match, PRE:812). If all
   present → `merge` (fill-missing-only, PRE:993) in place. If some missing → merge the present
   ones, then for each missing `sliceStuff` element compute an insertion point
   (`determineInsertionPoint`, PRE:847 — walks the id backwards to find peers and orders by child
   index) and **insert a copy** with `SNAPSHOT_PREPROCESS_INJECTED=true` and a rewritten id
   (slicer-id→slice-id). `endOfSlice++`.
5. `merge` (PRE:993): fill-missing-only for ~30 fields (label, code, short, definition, comment,
   requirements, alias, min, max, type, fixed, pattern, binding, mustSupport, isModifier, …).
   Emits trace `preprocess.merge.setField` (PRE:1000) and `preprocess.mergeElements.insert`
   (PRE:806).

**(b) additional-base merge (PRE:137–152).** Gated on `srcWrapper.hasExtension(EXT_ADDITIONAL_BASE)`.
**[DEAD under oracle config]** unless a profile carries `EXT_ADDITIONAL_BASE`. If you hit it, it is
a full second differential-merge engine (`mergeElementsFromAdditionalBase`, PRE:167) using
`DefinitionNavigator`; port lazily.

> **Porting note.** The injected-row id/order logic (4a) is subtle and order-dependent. Injected
> rows carry `SNAPSHOT_PREPROCESS_INJECTED` and are **exempt from PC-1 provenance back-migration**
> (Q7 skips rows without `SNAPSHOT_diff_source`, PU:923) but are **NOT exempt** from needing a
> `SNAPSHOT_GENERATED_IN_SNAPSHOT` (they still must be consumed by the walk).

## 3.1 Static entry — `processPaths` (PPP:155)

Builds the top frame (PPP:166–181): `result=derived.snapshot`, `differential=diff`,
`baseLimit=|baseSnapshot|−1`, `diffLimit=|diff|−1` (or −1 if no diff elements),
`contextPathSource=null`, `contextPathTarget=null`, `trimDifferential=false`,
`redirector=[]`, `sourceStructureDefinition=base`, `slicing=PathSlicingParams()` (done=false).
Cursor: `baseCursor=0, diffCursor=0, contextName=base.url, resultPathBase=null` (PPP:157–163).
Calls the instance `processPaths(cursors, mapHelper, slicerElement=null)`.

## 3.2 Main loop — `processPaths(cursors, mapHelper, slicerElement)` (PPP:191)

```
res := null;  typeList := [];  first := true
while cursors.baseCursor <= baseLimit  AND  cursors.baseCursor < |cursors.base.element|:     # PPP:198
    currentBase     := cursors.base.element[cursors.baseCursor]                              # PPP:200
    currentBasePath := fixedPathSource(contextPathSource, currentBase.path, redirector)      # PPP:201  (§3.9)
    diffMatches     := getDiffMatches(differential, currentBasePath, cursors.diffCursor, diffLimit)  # PPP:204 (§3.9)
    dc := cursors.diffCursor                                                                  # PPP:206
    if  NOT currentBase.hasSlicing()  OR  currentBasePath == slicing.path:                    # PPP:208
        currentRes := processSimplePath(currentBase, currentBasePath, diffMatches, typeList,
                                        cursors, mapHelper, first ? slicerElement : null)     # PPP:209  (§3.3)
        if res == null: res := currentRes
    else:
        processPathWithSlicedBase(currentBase, currentBasePath, diffMatches, typeList, cursors, mapHelper)  # PPP:214 (§3.6)
    if |diffMatches| > 0  AND  dc == cursors.diffCursor:            # PPP:225–228  (see R-WALK-1 caution)
        cursors.diffCursor += |diffMatches|
    first := false
checkAllElementsOK()   # PPP:233 — every emitted element must have non-null min, else Error(NULL_MIN)  (PPP:237–243)
return res
```

**R-WALK-1 (forward-only + blanket advance, PPP:216–229).** The `dc == cursors.diffCursor`
blanket-advance is an explicit implementer caution ("GDG 28-July 2025", PPP:216–224): some inner
branches advance the cursor themselves, others don't; this compensates. A port **MUST** reproduce
"advance by `|diffMatches|` iff no inner step moved the cursor," or inner content under slices is
mishandled. This is the single most fragile cursor rule.

**R-WALK-2 (window scoping).** `slicing.path` equality (PPP:208) makes an already-sliced base
element take the **simple** path when the loop has already descended to that slice's path — i.e.,
inside a slice sub-walk the anchor no longer looks "sliced." Get this wrong and re-slicing loops.

## 3.3 `processSimplePath` (PPP:296) — dispatch for an unsliced base element

Four-way branch on `diffMatches` shape (PPP:293–302):

```
if diffMatches.isEmpty():                                                → §3.3.1 processSimplePathWithEmptyDiffMatches  (PPP:1117)
elif oneMatchingElementInDifferential(slicing.done, path, diffMatches):  → §3.3.2 processSimplePathWithOneMatchingElementInDifferential  (PPP:730)
elif diffsConstrainTypes(diffMatches, path, typeList):                   → §3.3.3 processSimplePathWhereDiffsConstrainTypes  (PPP:549)
else:                                                                    → §3.3.4 processSimplePathDefault  (PPP:326)
```

`oneMatchingElementInDifferential(slicingDone, path, diffMatches)` (PPP:1797):
```
if |diffMatches| != 1: return false
if slicingDone: return true
if isImplicitSlicing(diffMatches[0], path): return false        # implicit [x] type slice → not "one match"
return NOT (diffMatches[0].hasSlicing()  OR  (isExtension(diffMatches[0]) AND diffMatches[0].hasSliceName()))
```
`isImplicitSlicing(ed, path)` (PU:1811): `path.endsWith("[x]") && ed.path.startsWith(stem(path)) && !path.equals(ed.path)`.

### 3.3.1 Empty diff — copy-through (`processSimplePathWithEmptyDiffMatches`, PPP:1117)

The differential says nothing about `currentBase`. **Clone-timing: `outcome = currentBase.copy()`
(via `updateURLs`), THEN re-home path.**
```
outcome := updateURLs(url, webUrl, currentBase.copy(), true)              # PPP:1118  (clone from BASE)
outcome.path := fixedPathDest(contextPathTarget, outcome.path, redirector, contextPathSource)  # PPP:1119
updateFromBase(outcome, currentBase, sourceSD.url)                        # PPP:1120 → stamps SNAPSHOT_BASE_MODEL/PATH, fills .base
updateConstraintSources(outcome, sourceSD.url)                           # PPP:1121  fill missing constraint.source
checkExtensions(outcome)                                                  # PPP:1122  strip NON_INHERITED_ED_URLS extensions
markExtensions(outcome, false, cursors.baseSource)                       # PPP:1123  stamp SNAPSHOT_EXTENSION_SOURCE
updateFromObligationProfiles(outcome)                                     # PPP:1124  (usually no-op)
updateURLs(url, webUrl, outcome, true); markDerived(outcome)             # PPP:1125–1126  mark constraints SNAPSHOT_IS_DERIVED
set/assert resultPathBase; addToResult(outcome)                          # PPP:1127–1131  append to output
if hasInnerDiffMatches(diff, currentBasePath, diffCursor, diffLimit, base.element, allowSlices=true):   # PPP:1133
    if baseHasChildren(cursors.base, currentBase):                       # PPP:1136  same-structure children present
        # recurse into children: new frame, baseCursor+1, narrowed baseLimit to child span; SHARES cursor advance-back
        newBaseLimit := span of currentBase's children                   # PPP:1137–1140
        processPaths(newFrame{baseLimit:newBaseLimit-1, slicing:PathSlicingParams()},
                     ncursors{baseCursor+1, diffCursor})                  # PPP:1142–1146
        cursors.baseCursor := newBaseLimit-1;  cursors.diffCursor := ncursors.diffCursor   # PPP:1147–1148
    else:                                                                # walk into a NEW type / contentReference
        if outcome.type empty AND no contentReference → Error(_HAS_NO_CHILDREN…)  # PPP:1149–1152
        (multi-type non-Reference non-extension diff child → Error _HAS_CHILDREN__AND_MULTIPLE_TYPES)  # PPP:1153–1173
        advance diffCursor over the child block                          # PPP:1165–1170
        if outcome.hasContentReference():                                # PPP:1174
            resolve via getElementById → replaceFromContentReference(outcome, tgt)   # PPP:1175–1178
            recurse with contextPathSource=tgt.path, contextPathTarget=outcome.path,
                   redirector=redirectorStack(...), sourceSD possibly = tgt.source    # PPP:1179–1212 (two branches: cross-SD vs same-SD)
        else:                                                            # data-type unfold
            dt := (outcome.type.size>1 ? fetchTypeDefinition("Element")  # PPP:1214
                                       : getProfileForDataType(outcome.type[0], webUrl, derived))
            recurse into dt.snapshot from index 1, contextPathSource=currentBasePath,
                   contextPathTarget=outcome.path (redirector empty or pushed)         # PPP:1219–1244
cursors.baseCursor++                                                     # PPP:1250
```

### 3.3.2 One match — the merge case (`processSimplePathWithOneMatchingElementInDifferential`, PPP:730)

The common path: exactly one diff element constrains this base element, not opening slicing.
Very dense (PPP:730–994); the decision-relevant skeleton:

1. **Illegal Reference type check** (PPP:735–739): if diff type[0] is Reference and not valid vs
   currentBase → throw `VALIDATION_VAL_ILLEGAL_TYPE_CONSTRAINT` (unless `suppressIgnorableExceptions`
   — **false** under oracle, so it throws).
2. **Template selection** — this determines the clone source:
   - **Reslice** (`lid` contains `/`, PPP:742–747): generate ids on the *result so far*, look up the
     already-emitted base slice `baseId` in `result` → `template = that result element`,
     `templateSD = sourceSD`. **Clone source = a previously emitted output element.**
   - **Profiled type** (diff type has a single non-Reference profile differing from base's profile,
     PPP:748–830): resolve the type profile SD (incl. xver, may re-enter `generateSnapshot`,
     PPP:766), then if `currentBase.typeSummary ∈ {Extension, Resource}`, build
     `template = merge(src, slicerElement).setPath(currentBase.path)` where `src = firstType SD's
     snapshot root (or a specific element via EXT_PROFILE_ELEMENT)`. **Clone source = the type
     profile's root element.** For non-Extension types, `template.min/max := currentBase.min/max`
     (PPP:825–827).
   - **Default** (`template == null`, PPP:831–843): `template = currentBase.copy()`,
     `templateSD = cursors.baseSource`. (`APPLY_PROPERTIES_FROM_SLICER=false` PPP:58 → the slicer
     branch is dead.) Else `template = fillOutFromBase(template, currentBase)` (PPP:842).
3. `outcome := updateURLs(url, webUrl, template, true)`; re-home path via `fixedPathDest`; **`res :=
   outcome`** (PPP:845–848).
4. `updateFromBase(outcome, currentBase, sourceSD.url)` (PPP:849) — base bookkeeping from
   currentBase (not template).
5. **If diff has a sliceName** (PPP:850–867): `template2 = merge(currentBase, slicer)` (a fresh
   currentBase copy), URL-fix, `checkToSeeIfSlicingExists(diff[0], template2)` — this may **emit a
   synthesized slicing anchor** into the result if none exists yet (PPP:955): extension → OPEN
   url-discriminated; `[x]` jumping into type slicing → CLOSED $this-type; else nothing. Then set
   `outcome.sliceName`; slice-min defaulting rules (PPP:857–866): if diff has no min and not a
   closed parent slicing and base not a slice → `outcome.min=0` (unless path ends
   `xtension.value[x]` — a hardcoded release-snapshot workaround); if CLOSED with >1 sibling
   slices → shared min → `outcome.min=0`.
6. `markExtensions(outcome, false, templateSD)` (PPP:868).
7. **`updateFromDefinition(outcome, diffMatches[0], …, fromSlicer)`** (PPP:869) — the merge (§4).
8. Slicer max clamp (PPP:872): if `outcome.maxAsInt > slicerElement.maxAsInt` → clamp to slicer max.
9. **`outcome.setSlicing(null)`** (PPP:875) — the merge case never carries slicing.
10. set/assert `resultPathBase`; `addToResult(outcome)` (PPP:876–881).
11. **Cursor advance** (PPP:882–883): `cursors.baseCursor++`; `cursors.diffCursor =
    indexOf(diffMatches[0]) + 1`.
12. **Descend into children/type/contentReference** (PPP:884–992) iff `diffLimit >= diffCursor`,
    path has `.`, and (`isDataType(type)` OR `isBaseResource(type)` OR `hasContentReference`), and
    the next diff element is a child of `diffMatches[0]` and base does not itself walk in
    (`!baseWalksInto`). Multi-type `[x]` renaming (PPP:887–909), then either contentReference
    resolution (PPP:914–952, cross-SD vs same-SD frames with redirector push) or data-type unfold
    (PPP:953–990, from `dt.snapshot` index 1 with `contextPathSource=diffMatches[0].path`,
    `contextPathTarget=outcome.path`, fresh empty redirector).

> **Clone-timing summary for the merge case:** the emitted element is a clone of **currentBase**
> (or of a type-profile root / a prior result element in the reslice/profiled subcases), never of
> the diff. The diff is overlaid by `updateFromDefinition` in step 7.

### 3.3.3 Diffs constrain types — implicit type slicing (`processSimplePathWhereDiffsConstrainTypes`, PPP:549)

Entered when `diffsConstrainTypes` (PU:1821) recognizes the diff rows as constraining individual
types of a `[x]` element and builds `typeList: List<TypeSlice>`. `diffsConstrainTypes`: requires
`diff[0].path` or `cPath` ends `[x]`; per diff row deduces the type from (a) single declared type,
(b) suffix after the stem (`valueQuantity`→`Quantity`, uncapitalized-primitive aware), or (c)
sliceName (PU:1837–1861).

1. `newBaseLimit = findEndOfElement(base, baseCursor)`; `newDiffCursor = indexOf(diff[0])`;
   `shortCut = typeList non-empty && typeList[0].type != null` (PPP:552–555).
2. **shortCut** (PPP:555–591): synthesize a slicing anchor element (`$this`/TYPE/CLOSED/unordered)
   and insert it at the front of `diffMatches` and into `differential` at `newDiffCursor`
   (`elementToRemove`). Under **R4+ with newSlicingProcessing (oracle config)** the R4 branch
   (PPP:571–585) is taken: the synthesized element specifies **no types** (= all base types
   allowed). **The pre-R4 branch (PPP:558–570) is [DEAD under oracle config].**
3. Non-shortcut (PPP:585–592): the last path segment of `cPath` and `diff[0].path` must match tails
   → else `ED_PATH_WRONG_TYPE_MATCH`.
4. Slicing legality on `diff[0].slicing` (PPP:596–611): ordered→true forbidden; discriminator, if
   present, must be exactly one, TYPE, `$this`.
5. **Slice-name/type coherence** (PPP:613–633): for each `TypeSlice` with a known type, compute
   `tn = rootName(cPath)+Capitalize(type)`; if the diff has no sliceName → set it; else if it
   mismatches → **`autoFixSliceNames` (false under oracle) → throw
   `ERROR_AT_PATH__SLICE_NAME_MUST_BE__BUT_IS_`** (the auto-fix branch is **[DEAD under oracle
   config]**). Type coherence: fill/verify single type.
6. **Process the root** (PPP:637–645): recurse over the base element from `baseCursor` with the
   synthesized slice active (`slicing=PathSlicingParams(done=true,…)`), get back `elementDefinition`;
   then **re-stamp slicing** onto it: `$this`/TYPE/**CLOSED**/unordered (PPP:648–652 — "type slicing
   is always closed; the differential might call it open but that only means it's not constraining
   unmentioned slices"). `slicerElement = elementDefinition.copy()`; if >1 type, `slicerElement.min=0`.
7. **Process each type slice sibling** (PPP:662–686): per diff row, min>0 with more rows → throw
   `INVALID_SLICING…MIN__1`; else `elementDefinition.min=1` and `fixedType = determineFixedType(...)`.
   Recurse with `slicing=PathSlicingParams(done=true, elementDefinition, …).withDiffs`,
   `slicerElement`. If `typeList.size > start+1`, force the slice `min=0`.
8. Cleanup (PPP:687–721): remove the synthesized `elementToRemove` from `differential`; if
   `fixedType != null`, prune non-matching types off `elementDefinition`; if not max=0, compute
   `allowedTypes` and if any remain either (extension.value shortCut) prune them, or **relax the
   anchor slicing to OPEN** (PPP:718). 
9. **Cursor advance**: `cursors.baseCursor = newBaseLimit+1`; `cursors.diffCursor = newDiffLimit+1`
   (PPP:724–725).

### 3.3.4 Default — the diff slices the base (`processSimplePathDefault`, PPP:326)

The remaining case: the diff **introduces slicing** on a previously-unsliced base element.

1. **Preconditions** (PPP:328–333):
   - If base is not `unbounded` AND not (`isSlicedToOneOnly(diff[0])` OR `isTypeSlicing(diff[0])`) →
     throw `ATTEMPT_TO_A_SLICE_AN_ELEMENT_THAT_DOES_NOT_REPEAT`.
   - If `!diff[0].hasSlicing()` AND base is not an extension → throw
     `DIFFERENTIAL_DOES_NOT_HAVE_A_SLICE`.
2. `newBaseLimit = findEndOfElement(base, baseCursor)`. Two shapes:
   - **Default-before-slices** (PPP:340–355): if `|diffMatches|>1 && diff[0].hasSlicing() &&
     (newBaseLimit>baseCursor || diff[1] not immediately after diff[0])` → there's a default set
     before the slices. Recurse over the default (slicing done), get `e`, set its slicing to
     `diff[0].slicing`, `slicerElement = e`, `start=1`.
   - **Accept differential slicing at face value** (PPP:356–443): `outcome = currentBase.copy()`
     (via updateURLs), re-home path, `updateFromBase`. Set slicing: if `!diff[0].hasSlicing()` →
     `outcome.slicing = makeExtensionSlicing()` (url/VALUE/OPEN/unordered) and
     **`SNAPSHOT_auto_added_slicing=true`** (PPP:362–364); else `outcome.slicing =
     diff[0].slicing.copy()` and check subsequent slice rows' slicing compatibility via
     `slicingMatches` (PPP:483 — a PPP-private method: ordered/rules/discriminator deep compare),
     emitting `ATTEMPT_TO_CHANGE_SLICING` ERROR or INFORMATION (PPP:367–378). Assert path prefix;
     `addToResult(outcome)`; `slicerElement = outcome`.
     - If `diff[0]` has **no** sliceName (the anchor row carries content, PPP:388–441):
       `updateFromDefinition(outcome, diff[0], …)`; if it walks into children (`hasInnerDiffMatches`)
       either recurse children (`baseHasChildren`) — with a **root-slicing-illegal Error at
       diffCursor==0** (PPP:395–397) and a hard "not handled" Error otherwise (PPP:398) — or unfold
       a type (`getTypeForElement`, PPP:401); else if base has a contentReference and no children,
       **dump the redirect target's children inline** (PPP:419–435: for each target-subtree
       element, copy, id=null, `fixForRedirect` path rewrite, `updateFromBase`, `markExtensions`,
       emit). `start++`.
     - Else (`diff[0]` has a sliceName, so it's a real slice, not a default): `checkExtensionDoco(outcome)` (PPP:440).
3. **Per-slice loop** (PPP:446–464): for `i` in `[start, |diffMatches|)`, set the diff window to
   `[indexOf(diff[i]), findEndOfElement(diff, that)]` and recurse with
   `slicing=PathSlicingParams(done=true, slicerElement, …).withDiffs(diffMatches)`,
   `slicerElement` — i.e. each slice body is processed against the same base subtree.
4. **Cursor advance**: `cursors.baseCursor = newBaseLimit+1`; `cursors.diffCursor = newDiffLimit+1`
   (PPP:465–466).

## 3.6 `processPathWithSlicedBase` (PPP:1252) — dispatch for an already-sliced base

Base element already carries slicing (profiling a profile / core-sliced element). Three-way
(PPP:1212–1222):
```
if diffMatches.isEmpty():                            → §3.6.1 processPathWithSlicedBaseAndEmptyDiffMatches  (PPP:1731)
elif diffsConstrainTypes(diffMatches, path, typeList): → §3.6.2 processPathWithSlicedBaseWhereDiffsConstrainTypes  (PPP:1568)
else:                                               → §3.6.3 processPathWithSlicedBaseDefault  (PPP:1281)
```

### 3.6.1 Empty diff on sliced base (`processPathWithSlicedBaseAndEmptyDiffMatches`, PPP:1731)

- If `hasInnerDiffMatches(diff, path, diffCursor, diffLimit, base.element, allowSlices=true)`
  (PPP:1732): emit `currentBase.copy()` (with `updateFromBase`, `markDerived`), then recurse into
  children (`baseHasChildren` → child span) or unfold a type (`getTypeForElement`), advancing the
  diff cursor over the child block; `cursors.baseCursor++` (PPP:1777).
- Else (PPP:1779–1795): the diff says nothing — **copy currentBase AND all its children and
  slices** (`while path startsWith`) verbatim, each stamped `SNAPSHOT_BASE_MODEL/PATH`; advance
  baseCursor across the whole block. **Clone source = base.**

### 3.6.2 Type-constraining on sliced base (`processPathWithSlicedBaseWhereDiffsConstrainTypes`, PPP:1568)

Mirror of §3.3.3 but slice-aware: `shortCut` also true when `diff[0].hasSliceName() &&
!diff[0].hasSlicing()` (PPP:1573). Same synthesized-anchor insertion (R4 branch live, pre-R4
**[DEAD]**), same `$this`/TYPE/CLOSED re-stamp (PPP:1585–1588). Additionally computes
`baseSlices = findBaseSlices(base, newBaseLimit)` (PU:1757) and for each diff type slice picks a
matching base slice via `chooseMatchingBaseSlice(baseSlices, type)` (PU:1747), processing the diff
against that base slice's `[start,end]` window (PPP:1606–1621). Base slices left unhandled get a
**fake empty differential** run against them (PPP:1635–1650) so they still emit. Note: here the
auto-fix-slice-name mismatch throws unconditionally (PPP:1557 — there is no `isAutoFixSliceNames`
guard in this variant, unlike §3.3.3). Cursor advance to `baseSlices.last.end+1` / `newDiffLimit+1`
(PPP:1652–1653).

### 3.6.3 Default on sliced base (`processPathWithSlicedBaseDefault`, PPP:1281)

The refine-existing-slicing path.
1. **Slicing compatibility** (PPP:1284–1296): if `diff[0].hasSlicing()`, check ordered
   (`orderMatches`), discriminator (`discriminatorMatches`), and (unless choice) rules
   (`ruleMatches`) against the base slicing — mismatches throw `SLICING_RULES_ON_DIFFERENTIAL…`.
   `closed = base.slicing.rules == CLOSED` (PPP:1282).
2. **Emit the anchor** (PPP:1296–1310): `outcome = currentBase.copy()`, re-home, `updateFromBase`.
   If `diff[0].hasSlicing() || !diff[0].hasSliceName()`: `updateFromSlicing(outcome.slicing,
   diff[0].slicing)` then `updateFromDefinition(outcome, diff[0], …, trimDifferential=closed)`. Note
   the branch `else if (!diff[0].hasSliceName())` (PPP:1302) is dead (subsumed) but sets
   `SNAPSHOT_GENERATED_IN_SNAPSHOT` when `updateFromDefinition` isn't called; the `else` stamps
   `SNAPSHOT_auto_added_slicing`. `markExtensions`; `addToResult`.
3. `diffpos` starts at 0; if `!diff[0].hasSliceName()`, `diffpos++` (the first row is the anchor,
   not a slice) (PPP:1315).
4. **Anchor children / BackboneElement copy** (PPP:1318–1366): if `hasInnerDiffMatches` unfold
   into the single fixed type or recurse children; else if base type is `BackboneElement`, copy the
   base's backbone children verbatim before slicing.
5. **Pair base slices with diff slices** (PPP:1369–1420): `baseMatches = getSiblings(base.element,
   currentBase)` (PU:2359). For each base slice: emit its `currentBase.copy()`-based outcome; if the
   next diff slice's sliceName matches → recurse to merge (with `trimDifferential=closed`, PPP:1394);
   else copy the base slice (and its children) through verbatim.
6. **New diff slices** (PPP:1422–1480): closed base + leftover diff → `THE_BASE_SNAPSHOT_MARKS_A_SLICING_AS_CLOSED…`
   unless `[x]`. Out-of-order → `NAMED_ITEMS_ARE_OUT_OF_ORDER_IN_THE_SLICE`. Reslice template via
   `getById`. Emit `template.copy()`-based outcome with `min=0`, `updateFromDefinition`, then
   pick up type-profile min/max constraints (PPP:1463–1478) and unfold children if the new slice
   walks into a type/backbone (PPP:1480–1531). `cursors.baseCursor++` at end (PPP:1537).

## 3.9 Path helpers used by the loop (exact logic)

- **`fixedPathSource(contextPath, p, redirector)`** (PU:2052):
  `contextPath==null → p`. Else if redirector non-empty: `ptail = (|contextPath|>=|p|) ?
  p.substring(p.indexOf(".")+1) : p.substring(|contextPath|+1)`; return
  `redirector.last.path + "." + ptail`. Else: return `contextPath + "." + p.substring(p.indexOf(".")+1)`.
- **`fixedPathDest(contextPath, p, redirector, redirectSource)`** (PU:2071): same shape but the
  redirector-branch tail uses `redirectSource.length()` instead of `contextPath.length()`, and the
  prefix used is `contextPath`.
- **`getDiffMatches(diff, path, start, end)`** (PU:2464): split both on `.`; a row matches iff
  **same segment count** and every segment is equal or `isSameBase` (§below). Returns rows in
  `[start,end]`. **Same-depth only** — never descendants. Multiplicity ⇒ slicing.
- **`isSameBase(p, sp)`** (PU:2507): `(p ends [x] && sp startsWith stem(p)) || (sp ends [x] && p
  startsWith stem(sp))`.
- **`hasInnerDiffMatches(diff, path, start, end, base, allowSlices)`** (PU:2440): true if some diff
  row in window is a strict descendant of `path` (or `[x]`-prefixed descendant); the `allowSlices`
  flag changes whether a same-path sliced row aborts (returns false) — used to decide whether to
  recurse into children.
- **`findEndOfElement(list, cursor)`** (PU:2511/2521): last index whose path `startsWith
  list[cursor].path+"."`. **`findEndOfElementNoSlices`** (PU:2529): same but stops at the first
  `hasSliceName()` row.
- **`pathStartsWith(p1,p2)`** (PU:2043): `p1.startsWith(p2) || (p2 ends "[x]." && p1 startsWith
  p2 without trailing "[x].")`.
- **`baseHasChildren(base, ed)`** (PPP:1099): next base element after `ed` is a child (`isChildOf`,
  which special-cases `[x]`, PPP:1108).
- **`baseWalksInto(elements, cursor)`** (PU:1897): `elements[cursor].path startsWith
  elements[cursor-1].path + "."`.

---

# 4. `updateFromDefinition` — PER-FIELD MERGE (PU:2605–3157)

Mental model (PU:2607): `base`/`dest` = the clone of the base snapshot element being built;
`derived`/`source` = the differential element. `Base.compareDeep` is a deep structural equality.
The universal per-field pattern is:
```
if derived.hasX():
    if !compareDeep(derived.X, base.X): base.X := derived.X.copy()     # OVERRIDE
    elif trimDifferential:              derived.X := null              # trim the diff clone
    else:                               mark derived.X SNAPSHOT_DERIVATION_EQUALS = true
```
Below, each Java behavior maps to the **existing legacy Rust leaf** in
`crates/snapshot_gen/src/lib.rs` (the file is currently monolithic — NOT yet split into modules
despite the task brief; all functions live in `lib.rs`). Gaps are flagged.

| # | Java behavior | Lines | Rust leaf (lib.rs) | Notes / gaps |
|---|---|---|---|---|
| 0 | set `source.SNAPSHOT_GENERATED_IN_SNAPSHOT = dest`; `derived.SNAPSHOT_DERIVATION_POINTER = base` | 2606–2611 | (annotations) | consumption + provenance stamps |
| 1 | `isExtension = checkExtensionDoco(base)`: if path is Extension/.extension/.modifierExtension → overwrite definition="An Extension", short="Extension", clear comment/requirements/alias/mapping | 2617 / PU:1968 | `check_extension_doco` (4171) | ✓ present |
| 2 | obligation-profile element extensions copied to dest | 2622–2643 | — | **gap** (obligationProfiles empty in corpus ⇒ [DEAD]); no leaf needed now |
| 3 | drop 2nd `EXT_TRANSLATABLE` extension (R5 hack) | 2631–2634 | — | rare; **gap** (low priority) |
| 4 | `updateExtensionsFromDefinition(dest, source, …)`: remove NON_INHERITED + default-inherited-if-source-has; copy source extensions with NON_OVERRIDING/OVERRIDING policy; stamp `SNAPSHOT_EXTENSION_SOURCE` | 2635 / 3228 | `merge_extensions_from_definition` (5238), `dedupe_extension_values*` (5261/5265) | ✓ approx; verify NON_OVERRIDING/OVERRIDING url lists |
| 5 | **Profile-on-type root override hack**: if source type→resource/Extension SD, overwrite base short/definition/comment/requirements/alias/mapping from that SD's root (processing relative urls); for non-resource profiles deliberately does NOT override (sets `msg=false`) | 2648–2717 | `apply_type_profile_root` (4655), `apply_extension_profile_root` (4217), `profile_root_element` (4862) | ✓ present (large legacy area) |
| 6 | sliceName copy | 2718–2721 | inside `merge_diff_into_element` (3699) | ✓ |
| 7 | short: override (deep-compare) | 2723–2730 | `merge_text_field`/`copy_if_present` | ✓ (plain override, no `...` for short) |
| 8 | definition: `mergeMarkdown` (the `...` append convention) | 2732–2739 | `merge_markdown` (5045), `append_derived_text_to_base` (5072) | ✓ |
| 9 | comment: `mergeMarkdown` | 2741–2748 | `merge_markdown` | ✓ |
| 10 | label: `mergeStrings` | 2750–2757 | `merge_string` (5055) | ✓ |
| 11 | requirements: `mergeMarkdown` | 2759–2766 | `merge_markdown` | ✓ |
| 12 | **sdf-9**: drop `requirements` on root (path has no `.`) in both derived and base | 2768–2771 | `trim_inherited_text_fields`? | **verify** this root-requirements drop exists |
| 13 | alias: additive union | 2773–2784 | `merge_unique_array_strings` (5080) | ✓ |
| 14 | min: override; **ERROR if derived.min < base.min and not a slice** | 2786–2795 | `merge_min_cardinality` (5392) | ✓ (+ message) |
| 15 | max: override; **ERROR if `isLargerMax`** | 2797–2806 | `merge_max_cardinality` (5404) | ✓ (+ message) |
| 16 | fixed: override (compareDeep with `true`) | 2808–2815 | `copy_if_present`? | **verify** fixed handling |
| 17 | pattern: override | 2817–2825 | `copy_if_present`? | **verify** |
| 18 | example: per-item add/suppress (`EXT_ED_SUPPRESS`, `$all` label) | 2827–2856 | — | **gap** (rare); low priority |
| 19 | maxLength, maxValue, minValue: override | 2858–2883 | `copy_if_present` | ✓ |
| 20 | **mustSupport**: merge with obligation-profile mustSupport; **ERROR if base MS true and derived MS false and !fromSlicer** | 2888–2909 | mustSupport logic in `merge_diff_into_element` | ✓ core; obligation merge is [DEAD] |
| 21 | mustHaveValue: like mustSupport (ERROR on weakening, !fromSlicer) | 2911–2921 | **verify** | |
| 22 | valueAlternatives: additive union | 2922–2933 | `merge_unique_values`? | **verify** |
| 23 | **isModifier/isModifierReason: ONLY if `isExtension`** (profiles cannot change; extensions can). Sets default modifierReason text | 2937–2956 | inside extension handling | **verify** the extension-only gate |
| 24 | **binding: only-narrow**: build `nb` from base binding, clear description, overlay derived strength/description/valueSet/additional; **ERROR on REQUIRED→weaker**; subset checks (tx expansion, too-costly) → WARNING/ERROR | 2958–3066 | `merge_binding` (5161), `merge_additional_binding` (5198) | ✓ structural; the tx subset validation is [SKIPPABLE] (messages only, needs a tx server) |
| 25 | **binding deleted if no bindable type after merge** | 3134–3137 | `has_bindable_type` (5301) | ✓ |
| 26 | isSummary: override; **Error** if base has isSummary and version≠1.4.0 | 3068–3077 | `copy_if_present` + guard | **verify** the summary-change Error |
| 27 | **type: replace wholesale** (`base.type.clear()` then copy all derived), with `checkTypeDerivation` per type; stamp extension sources | 3082–3109 | `merge_type_entries` (5321), `merge_type_extensions` (5347) | ✓ (wholesale replace) |
| 28 | `mappings.merge(derived, base)` — note reversed arg names (MA:173) | 3111 | — (SD-level mappings via MappingAssistant) | **gap**: MappingAssistant not ported; element `mapping` merge + rename map. See §MappingAssistant below. |
| 29 | **constraint: additive** (add only if key not present); stamp base constraints `SNAPSHOT_IS_DERIVED` + fill missing source | 3113–3127 | constraint merge in `merge_diff_into_element` | ✓ (accumulate, never replace) |
| 30 | **condition: additive** | 3128–3132 | **verify** | |
| 31 | fixed/pattern type-ok checks | 3150–3155 | — | messages only |

**Choice renaming note:** the `[x]` → concrete rename on the *dest path* happens in the walk
(PPP:887–909 / PPP:830–842), not in `updateFromDefinition`. `copy_choice_prefix` (5425) /
`remove_choice_prefix` (5437) / `canonicalize_choice_differentials` (593) handle it in the Rust
legacy layer.

**`updateFromBase(derived, base, baseProfileUrl)`** (PU:2024): stamps `SNAPSHOT_BASE_MODEL` and
`SNAPSHOT_BASE_PATH`; fills `derived.base` from `base.base` if present (origin propagation), else
from `base.path/min/max`. Rust: `normalize_inherited_element` (5477) / `updateFromBase`-equivalent —
**verify base.base origin-propagation preference.**

**`fillOutFromBase(profile, usage)`** (PU:1906): fill-missing-only from `usage` into a copy of
`profile` (never override). Used for slice templates.

**`MappingAssistant`** (MA, 279 lines) — **not yet ported; a real gap.** Constructor (MA:45) builds
a master mapping list (derived priority, base fills gaps, renames on identity collision, marks
inherited via `mappings_inherited`). `merge(base, derived)` (MA:173) merges element-level mappings
(base first, applying renames, dedup by `compareMaps`; R5-plus APPEND/DUPLICATE/IGNORE/OVERWRITE
policy at MA:233; **oracle uses APPEND**, but note context version is **R4** so `isR5Plus(version)`
is FALSE → `compareMaps` returns false for the non-exact case ⇒ effectively **no merge, just
dedup-by-exact-then-append-all** at MA:200–210). `update()` (MA:150) prunes SD-level mappings to
used+inherited. Trace: none. **Porting priority: medium** — element `.mapping` output differs
without it.

---

# 5. userData / SIDE-CHANNEL INVENTORY → `Annotations`

Keys used by the profile package (grep count in profile/*.java). String values from UDN.

| Key (UDN const) | UDN string | Writers | Readers | Lifecycle | `Annotations` field |
|---|---|---|---|---|---|
| `SNAPSHOT_GENERATED_IN_SNAPSHOT` (UDN:19) | `profileutilities.snapshot.processed` | `updateFromDefinition` (PU:2606), walk (PPP:1298), specialization (PU:858/863) | PU:855, 931, 944; PC-1 | cleared P16; set on consumption; absence@Q7 ⇒ unmatched | `generated_in_snapshot: Option<OutIdx>` (diff→output back-pointer) |
| `SNAPSHOT_diff_source` (UDN:28) | `diff-source` | `cloneDiff` (PU:1503) | PU:923,926,928,944 | diff-clone→original | `diff_source: Option<DiffIdx>` |
| `SNAPSHOT_DERIVATION_POINTER` (UDN:17) | `derived.pointer` | `updateFromDefinition` (PU:2611) | PU:928 (migrate) | diff→base cross-link | `derivation_pointer` |
| `SNAPSHOT_DERIVATION_EQUALS` (UDN:16) | `derivation.equals` | ~26 sites in `updateFromDefinition` + MA:203 | PU:926 (migrate) | marks diff prop == base (not trimmed) | `derivation_equals: bool` (per-field; may need per-property granularity — see ambiguity A7) |
| `SNAPSHOT_DERIVATION_DIFF` (UDN:20) | `profileutilities.snapshot.diffsource` | PU:945 | preprocessor trimSnapshot (PRE:1192) | snapshot→diff cross-link | `derivation_diff: Option<DiffIdx>` |
| `SNAPSHOT_BASE_MODEL` (UDN:14) | `base.model` | `updateFromBase` (PU:2025), PPP:1353/1716 | (rendering) | origin SD url | `base_model: String` |
| `SNAPSHOT_BASE_PATH` (UDN:15) | `base.path` | `updateFromBase` (PU:2026), PPP:1352/1717 | (rendering) | origin path | `base_path: String` |
| `SNAPSHOT_auto_added_slicing` (UDN:24) | `auto-added-slicing` | PPP:363, 1250; walk | PU:1013 (slice-min auto-correct vs error) | marks generator-synthesized slicing | `auto_added_slicing: bool` |
| `SNAPSHOT_IS_DERIVED` (UDN:18) | `derived.fact` | `markDerived` (PU:1994), PU:3115 | (rendering) | constraint provenance | `is_derived: bool` (on constraint) |
| `SNAPSHOT_SORT_ed_index` (UDN:27) | `ed.index` | `sortDifferential` (PU:3847) | PU:3909 (compareDiffs) | sort stability (only if `--sort`) | `sort_ed_index: Option<usize>` |
| `SNAPSHOT_slice_name` (UDN:26) | `slice-name` | (setIds slicing) | PU:4655 area | slice-name restore during id gen | `slice_name` |
| `SNAPSHOT_EXTENSION_SOURCE` (UDN:149) | `SNAPSHOT_EXTENSION_SOURCE` | `markExtensionSource` (PU:3217) | (rendering/obligation source) | which SD an extension came from | `extension_source` (per-extension) |
| `SNAPSHOT_PREPROCESS_INJECTED` (UDN:141) | `SNAPSHOT_PREPROCESS_INJECTED` | preprocessor (PRE:802) | Q7 exemption (PU:923 via diff_source absence) | marks preprocessor-injected diff row | `preprocess_injected: bool` |
| `SNAPSHOT_FROM_DIFF` (UDN:148) | `SNAPSHOT_FROM_DIFF` | `trimSnapshot` (PRE:1205/1211) | PRE:1187/1202/1219 | **[DEAD]** — trimSnapshot not called from generateSnapshot | (skip) |
| `SNAPSHOT_GENERATED_MESSAGES` (UDN:21) | `profileutils.snapshot.generated.messages` | PU:1111 | publisher | attaches messages to SD | (SD-level, not element) |
| `mappings_inherited` (UDN:59) | `private-marked-as-derived` | MA:59 | MA:166 | mapping pruning | (MappingAssistant internal) |
| `render_webroot` (UDN:65) | `webroot` | (loader) | PU:2684 | profile web root for relative urls | (read-only input) |
| `UD_DERIVATION_POINTER` | — | — | commented-out check (PPP:250) | dead debug | (skip) |

> **Rust decision:** one `Annotations` struct keyed by element index (separate maps for the diff
> list and the snapshot list). Because `Base.setCopyUserData(true)` (P9), annotations **propagate
> on clone** — the Rust `copy` helper must carry the annotation for the source index to the new
> index. `derivation_equals` is written **per-property** in Java (on the specific `MinElement`,
> `MaxElement`, `TypeRefComponent`, etc.), not on the whole element — if the Rust port needs to
> reproduce `trimDifferential` output exactly it must track equals at property granularity (A7).

---

# 6. MESSAGE / ERROR EMISSION POINTS

Two mechanisms: `throw` (fatal, aborts → snapshot nulled, Q12) and `addMessage`/`handleError`
(collected; `addMessage` PU:1236 additionally **throws** iff `msg.level==ERROR &&
wantThrowExceptions` — but `wantThrowExceptions=false` under oracle because the messages list is
non-null, so ERRORs are collected, not thrown). `handleError` (PU:1232) = addMessage of an ERROR.
`forPublication=true` promotes the slice-min finding to ERROR (PU:1018). `.setIgnorableError(true)`
marks a message ignorable but still recorded.

**Unconsumed-differential paths (PC-1) — the important ones for the port:**
- **Q7** (PU:918–963): for each diff clone row, if no `SNAPSHOT_GENERATED_IN_SNAPSHOT` and the row
  `hasId()` → per-row `addMessage(ERROR, "StructureDefinition.differential.element[i]", "No match
  found for <id> in the generated snapshot: check that the path and definitions are legal in the
  differential (including order)")` (PU:940–941), plus an aggregate `handleError(url, "The profile
  <url> has N elements in the differential (...) that don't have a matching element in the
  snapshot…")` (PU:949–962). Trace `generateSnapshot.diffNotConsumed` (PU:936). Injected rows
  (no `SNAPSHOT_diff_source`) skip provenance migration but still need consumption.
- The walk's own hard failures that indicate an illegal/misordered differential: `NOT_DONE_YET`
  (PPP:375/398), root-slicing-illegal (PPP:396), `NAMED_ITEMS_ARE_OUT_OF_ORDER_IN_THE_SLICE`
  (PPP:1375), `THE_BASE_SNAPSHOT_MARKS_A_SLICING_AS_CLOSED…` (PPP:1367),
  `ATTEMPT_TO_A_SLICE_AN_ELEMENT_THAT_DOES_NOT_REPEAT` (PPP:312),
  `DIFFERENTIAL_DOES_NOT_HAVE_A_SLICE` (PPP:314), `ADDING_WRONG_PATH*` (PPP:364/823/1074/…).

**Other collected messages** worth porting for message parity (all `addMessage`): min<base (PU:2789),
max>base (PU:2800), mustSupport weakening (PU:2902), mustHaveValue weakening (PU:2914), binding
REQUIRED→weaker (PU:2987), binding subset findings (PU:2993–3025), slice cardinality (PU:1016–1030),
launch-into-slicing (PU:1039), duplicate slice name (PU:1045), unknown/incompatible type profile
(PU:1065/1077/1085), `ATTEMPT_TO_CHANGE_SLICING` (PPP:351/355).

**Fatal Errors/Exceptions** (abort): `NULL_MIN` (PPP:242), `TYPE_ON_FIRST_SNAPSHOT_ELEMENT`
(PU:893), circular snapshot (PU:775), the many walk `throw`s above, `checkDifferential` throws
(PU:1428+), `checkGroupConstraints` `Error("huh?")`/two-mandatory (PU:1368/1377), `sortDifferential`
recursion-limit Error (PU:3815).

---

# 7. PROPOSED RUST SKELETON — `src/walk/`

The walk consumes the **R5-internal-model JSON** (REWORK-PLAN stage 2 output; elements are
`serde_json::Value` with preserve_order) and reuses the existing leaf-merge layer in
`crates/snapshot_gen/src/lib.rs` (§4 map). Module layout mirrors the decision tree 1:1.

```
src/walk/
  mod.rs            // pub fn generate_snapshot(...) -> WalkOutput   (§1 orchestration)
  context.rs        // WalkContext (shared) + Annotations (§5) + config
  frame.rs          // WalkFrame (per-recursion) + WalkCursor + SlicingParams + ElementRedirection
  preprocess.rs     // process_slices(...) (§3.0a); additional_base stub (§3.0b)
  loop_.rs          // process_paths(cursor, frame) main loop (§3.2)  + check_all_elements_ok
  simple.rs         // process_simple_path (§3.3) + the four sub-branches (§3.3.1–3.3.4)
  sliced.rs         // process_path_with_sliced_base (§3.6) + three sub-branches
  types.rs          // diffs_constrain_types, type-slice synthesis, find_base_slices (§3.3.3/3.6.2)
  contentref.rs     // content-reference resolution + redirector + replace_from_content_reference (§3.9)
  paths.rs          // fixed_path_source/dest, get_diff_matches, is_same_base, has_inner_diff_matches,
                    //   find_end_of_element[/no_slices], path_starts_with, base_has_children, base_walks_into
  slicing.rs        // slicing_matches, update_from_slicing, order/discriminator/rule_matches,
                    //   make_extension_slicing, is_sliced_to_one_only, is_type_slicing, one_matching_element
  finalize.rs       // §1.3 post-passes: check_group_constraints, prune_prohibited_type_slices,
                    //   root_untyped_assert, set_ids, provenance/PC-1, normalize, slice_cardinality,
                    //   profile_ref_checks, ext_version_base
  mapping.rs        // MappingAssistant port (§4) — new
  ids.rs            // set_ids / generate_ids (Q6)
```

Signatures (sketch; `Value` = `serde_json::Value` preserve_order):

```rust
pub fn generate_snapshot(base: &Value, derived: &mut Value, url: &str, web_url: &str,
                         profile_name: &str, cfg: &WalkConfig, store: &SdStore)
    -> Result<Vec<Message>, WalkError>;

// loop_.rs
fn process_paths(ctx: &mut WalkContext, cur: &mut WalkCursor, frame: &WalkFrame,
                 slicer: Option<&Value>) -> Option<usize /* res out-idx */>;

// simple.rs
fn process_simple_path(ctx, cur, frame, current_base: &Value, current_base_path: &str,
                       diff_matches: &[usize], type_list: &mut Vec<TypeSlice>, slicer) -> Option<usize>;
fn process_simple_path_empty(...);         // §3.3.1
fn process_simple_path_one_match(...) -> usize;   // §3.3.2  (returns emitted out-idx)
fn process_simple_path_type_constrain(...);       // §3.3.3
fn process_simple_path_default(...);              // §3.3.4

// sliced.rs — mirrors simple.rs
fn process_path_with_sliced_base(...);
fn process_path_with_sliced_base_empty(...);
fn process_path_with_sliced_base_type_constrain(...);
fn process_path_with_sliced_base_default(...);

// paths.rs
fn fixed_path_source(context_path: Option<&str>, p: &str, redirector: &[ElementRedirection]) -> String;
fn fixed_path_dest(context_path: Option<&str>, p: &str, redirector: &[ElementRedirection], redirect_source: Option<&str>) -> String;
fn get_diff_matches(diff: &[Value], path: &str, start: usize, end: isize) -> Vec<usize>;
fn has_inner_diff_matches(diff: &[Value], path: &str, start: usize, end: isize, base: &[Value], allow_slices: bool) -> bool;
```

**Cursor sharing rule (critical):** where Java constructs a *new* `ProfilePathProcessorState` and
passes it to the recursion (e.g. type unfold, PPP:399/932/1165), Rust passes a **fresh
`WalkCursor`**; where Java **reuses** `cursors` and then reads `ncursors.diffCursor` back into it
(e.g. PPP:1090–1092, PPP:1147–1148), Rust must copy the recursion's ending cursor back. Follow each
call site literally.

**Emission order:** `process_paths` appends to the single shared `ctx.output` in exactly the order
Java's `addToResult` does (PPP:1484). The output is base-ordered; the diff cursor only gates *which*
diff row overlays *which* base element.

---

# 8. AMBIGUITIES & ORACLE EXPERIMENTS

Places behavior cannot be nailed from reading alone; each with a minimal fixture experiment
(run through `snapshot/oracle/gen-snapshot.sh` / SnapOracleR4 and inspect output + `.trace.jsonl`).

| # | Ambiguity | Fixture / experiment |
|---|---|---|
| A1 | R-WALK-1 blanket cursor advance (PPP:225–228): which inner branches leave `dc==diffCursor`? The "sd-nested-ext" case is named but the full set isn't. | Build the `sd-nested-ext` text fixture (nested extension with inner content); diff two engines' `.trace.jsonl` `diffCursor` per step. |
| A2 | `slicing.path` equality (PPP:208) making a sliced base take the simple path — exact trigger during a slice sub-walk. | Profile-a-profile that re-refines an existing slice; watch whether the anchor is dispatched simple vs sliced. |
| A3 | `checkToSeeIfSlicingExists` (PPP:955): when does it inject a synthesized anchor vs. do nothing (extension vs `[x]` type-slice vs neither)? | Diff that names a slice without an anchor row, for (a) `.extension`, (b) `value[x]` type slice, (c) a plain repeating element. |
| A4 | Slice-min defaulting (PPP:857–866): the `xtension.value[x]` hardcoded skip and the CLOSED-shared-min rule. | Closed slicing with 2 slices, neither with an explicit min; check emitted mins. |
| A5 | contentReference `fixForRedirect` (PPP:455) is an unanchored `path.replace` — collision risk when the redirect fragment appears mid-path. | contentReference where the fragment string is a substring of a deeper path segment. |
| A6 | Type-slice OPEN-relaxation (PPP:718): when leftover allowed types force the anchor OPEN, and the `xtension.value && shortCut` pruning branch. | `[x]` element type-sliced on a subset of its base types; check whether anchor rules end OPEN or CLOSED. |
| A7 | `SNAPSHOT_DERIVATION_EQUALS` granularity — per-property vs per-element — matters only if `trimDifferential` output must match. Under oracle, `trimDifferential` enters the walk only as `closed` in `processPathWithSlicedBaseDefault` (PPP:1335). | Closed-slicing profile whose slice restates a base-equal property; check whether that property is trimmed from the emitted diff (note: oracle emits the *snapshot*, so this is only observable via message/id side effects). |
| A8 | MappingAssistant under R4 context: `isR5Plus(version)` false ⇒ `compareMaps` non-exact path returns false ⇒ effectively append-all-dedup-exact. Confirm no APPEND merging happens. | Profile whose element restates a base mapping with a different `.map` under the same identity; check whether maps merge or duplicate. |
| A9 | `checkGroupConstraints` choice-group collapse (Q1) interaction with slicing/`[x]`. | Resource with a choice group where the profile mandates one member; verify siblings get `max=0` + subtree removal. |
| A10 | `diffsConstrainTypes` vs `default` dispatch boundary for a single diff row on a `[x]` element with a sliceName but no slicing. | Single `valueQuantity`-style row with sliceName, no `.slicing`; check which branch (§3.3.3 vs §3.3.4). |
| A11 | Whether the SPECIALIZATION-only post-passes (Q2/Q9) ever fire for any corpus profile (all assumed CONSTRAINT). | Grep corpus for `derivation: specialization`; if any, add to fixture ladder. |
| A12 | `fixTypeOfResourceId` (P6) mutates the shared base snapshot — does it affect subsequent profiles reusing the same base object in one run? | Two profiles on the same base in one batch; confirm the base's `Resource.id` type mutation is idempotent/shared. |

---

# 9. TRACE POINTS (proving decision isomorphism)

**Current state of instrumentation in the pinned commit.** `SnapshotTracer.java` exists (241
lines) and is a working sink, but it is **only partially wired**. The trace call sites that
actually exist at `5c4d5a0ff` are:

| Existing trace `fn` / `branch` | Site | 
|---|---|
| `generateSnapshot` / `generateSnapshot.begin` | PU:826–828 |
| `generateSnapshot` / `generateSnapshot.walkComplete` | PU:847–849 |
| `generateSnapshot` / `generateSnapshot.diffNotConsumed` | PU:935–937 |
| `updateFromDefinition` / `updateFromDefinition.entry` | PU:2613–2615 |
| `updateFromDefinition` / `updateFromDefinition.checkExtensionDoco` | PU:2618–2620 |
| `replaceFromContentReference` / `replaceFromContentReference` | PU:1887–1889 |
| `SnapshotGenerationPreProcessor` / `preprocess.mergeElements.insert` | PRE:805–807 |
| `SnapshotGenerationPreProcessor` / `preprocess.merge.setField` | PRE:1000–1001 |

Record shape (SnapshotTracer:100): `{"seq":N,"d":depth?,"fn":"<method>","branch":"<label>",
"base":"<id|null>","diff":"<id|null>","x":{…}}`. `id(ed)` = `ed.getId()` or path (SnapshotTracer:147).
`enter()/exit()` bump a `depth` field for nested/recursive generation (not currently called).

**There is no `snapshot/specs/trace-schema.md` yet** (confirmed absent at time of writing). The
`SnapshotTracer` javadoc *names* intended-but-absent labels like `processSimplePath.overwriteSlicing`;
those branches are **not** instrumented in this commit. So the **delta** between "what trace-schema
should instrument" and "what the pinned Java currently emits" is: **the entire `ProfilePathProcessor`
dispatch is un-instrumented.** The `snap-trace` branch is expected to add these.

**Recommended trace points to prove decision isomorphism** (emit from BOTH engines; the Rust walk
emits the identical records). Minimum set = one record per dispatch decision + per emission +
per recursion boundary:

1. **Per main-loop iteration** (PPP:198): `processPaths.iterate` with x=`{baseCursor, diffCursor,
   baseLimit, diffLimit, currentBasePath, diffMatchCount, currentBaseHasSlicing, slicingDone,
   slicingPath}`. — proves cursor state + window at each step.
2. **Dispatch chosen** (PPP:293–302 / 1212–1222): `processSimplePath.dispatch` /
   `processPathWithSlicedBase.dispatch` with x=`{branch: empty|oneMatch|typeConstrain|default}`.
   — the single most important isomorphism signal.
3. **Every emission** (`addToResult`, PPP:1484): `emit` with x=`{outIdx, path, sliceName,
   cloneSource: base|type-root|result-reslice|contentref-target, sourceId}`. — proves clone-timing.
4. **`updateFromBase`** and **`updateFromDefinition.entry`** (already exists) — per-merge.
5. **Recursion boundaries**: `recurse.enter`/`recurse.exit` with x=`{kind: children|type-unfold|
   contentref|slice-body|type-slice|default-before-slices|backbone-copy|fake-diff, contextPathSource,
   contextPathTarget, redirectorDepth, baseLimit, diffLimit}`. Wrap with `enter()/exit()` for depth.
6. **Slicing synthesis**: `slicing.synthesize` with x=`{kind: extension|type|auto-added, rules,
   discriminator}` at PPP:344/510/595/975/1585 and `checkToSeeIfSlicingExists` injection (PPP:974/979).
7. **`SNAPSHOT_auto_added_slicing` set** and **slice-min defaulting** (PPP:363/857–866) — records the
   min override decision.
8. **Cursor advance** (R-WALK-1, PPP:225): `processPaths.blanketAdvance` with x=`{advancedBy,
   dcBefore, dcAfter}` — proves A1.
9. **PC-1 outcome** (Q7): already `generateSnapshot.diffNotConsumed` — keep.

**Delta vs a future `trace-schema.md`:** if the schema instruments only the 8 existing sites, it is
**insufficient** — output parity ≠ decision parity (REWORK-PLAN §4). The dispatch (item 2), emission
clone-source (item 3), and recursion-kind (item 5) records are mandatory additions; without them two
engines can agree on output while diverging on decisions. Coordinate: when `trace-schema.md`
materializes, reconcile branch label strings with the labels used here (items 1–9); if it omits any
of items 2/3/5/8, flag it as a gap.

---

# 10. STALE-ANCHOR AUDIT vs `snapshot-fodder/SPECIFICATION.md`

The SPECIFICATION.md `PU:`/`PPP:` anchors were written against an **earlier** revision; at
`5c4d5a0ff` **PU anchors are systematically ~15–29 lines low** (source has grown). Every PU anchor I
spot-checked is stale; PPP anchors happen to be closer but several are off. Corrected map (spec
anchor → **true line at `5c4d5a0ff`**):

| Symbol | SPEC anchor | TRUE line | Δ |
|---|---|---|---|
| `pathMatches(String,ED)` | PU:1141 | **PU:1156** | +15 |
| `pathMatches(String,String)` | PU:2027 | **PU:2047** | +20 |
| `fixedPathSource` | PU:2032 | **PU:2052** | +20 |
| `fixedPathDest` | PU:2051 | **PU:2071** | +20 |
| `getDiffMatches` | PU:2444 | **PU:2464** | +20 |
| `isSameBase` | PU:2487 | **PU:2507** | +20 |
| `findEndOfElement` | PU:2491 | **PU:2511** | +20 |
| `findEndOfElementNoSlices` | PU:2509 | **PU:2529** | +20 |
| `updateFromDefinition` | PU:2585 | **PU:2605** | +20 |
| `updateFromBase` | PU:2004 | **PU:2024** | +20 |
| `diffsConstrainTypes` | PU:1806 | **PU:1821** | +15 |
| `isSlicedToOneOnly` | PU:2397 | **PU:2417** | +20 |
| `isTypeSlicing` | PU:2401 | **PU:2421** | +20 |
| `makeExtensionSlicing` | PU:2408 | **PU:2428** | +20 |
| `checkExtensionDoco` | PU:1948 | **PU:1968** | +20 |
| `replaceFromContentReference` | PU:1870 | **PU:1885** | +15 |
| `sortDifferential` | PU:3815 | **PU:3844** | +29 |
| `getElementById` | PU:3524 | **PU:3553** | +29 |
| `EXT_VERSION_BASE stamp` | PU:1092 | **PU:1107** | +15 |
| `generateSnapshot` (entry) | PU:740 | **PU:740** | 0 (unchanged) |
| `ElementDefinitionCounter` | PU:157 | **PU:157** | 0 |
| `MAX_RECURSION_LIMIT` | PU:3786 | **PU:3787** (`int lc=0` guard at 3815) | ~+1 |
| PPP `processPaths` static | PPP:155 | **PPP:155** | 0 |
| PPP `processPaths` instance | PPP:191 | **PPP:191** | 0 |
| PPP `processSimplePath` | PPP:283 (implied) | **PPP:296** | +13 |
| PPP `processSimplePathDefault` | PPP:307 | **PPP:326** | +19 |
| PPP `oneMatchingElementInDifferential` | PPP:1723 | **PPP:1797** | +74 |
| PPP `processPathWithSlicedBase` | PPP:1196 | **PPP:1252** | +56 |

**Stale-anchor count: 24 of the ~28 cited symbols are off (the 4 exact matches are `generateSnapshot`
entry, `ElementDefinitionCounter`, and both `processPaths`).** PPP drift is larger than PU near the
tail (`oneMatchingElementInDifferential` +74, `processPathWithSlicedBase` +56). **All line
citations in THIS document are against `5c4d5a0ff` and supersede SPECIFICATION.md's anchors.**

Substantive content deltas found (source wins):
- SPECIFICATION.md §2.6 says `oneMatchingElementInDifferential` at PPP:1723 — it's PPP:1797, and its
  logic matches the spec's description.
- The `newSlicingProcessing` pre-R4 branch (SPEC §2.7 knob) is present but **dead** under the R4
  oracle config — the R4 branch (no-types synthesized anchor) is the live one (PPP:571/1517).
- SPECIFICATION.md implies a single `slicingMatches` gate; in the current source it is a
  PPP-**private** method (PPP:483) for the introduce-slicing case, while the refine-existing case
  uses the separate `orderMatches`/`discriminatorMatches`/`ruleMatches` triad (PU:2392/2396/2412).

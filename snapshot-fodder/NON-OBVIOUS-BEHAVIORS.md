# Non-obvious helpers & hidden behaviors in snapshot generation

Companion to `INDEX.md`. These are the utility functions / support classes whose
semantics are load-bearing but easy to miss when reading top-down. Paths are under
`org.hl7.fhir.r5/.../conformance/profile/`; `PU.java` = ProfileUtilities.java,
`PPP.java` = ProfilePathProcessor.java.

## 1. Path matching / comparison
- **Two overloaded `pathMatches`** — `pathMatches(String,ElementDefinition)` PU.java:1141 and
  `pathMatches(String,String)` PU.java:2027. Both implement the `[x]` rule: `value[x]`
  matches `valueString` but NOT `valueString.foo` (remainder must contain no `.`).
- **`pathStartsWith`** PU.java:2023 — special-cases a `value[x].` prefix so it matches `valueQuantity.unit`.
- **`getDiffMatches`** PU.java:2444 + **`isSameBase`** PU.java:2487 — the real "which diff
  elements belong to this base path" decision; segment-by-segment, requires EQUAL segment
  count (same-depth siblings only, never descendants), and the `[x]` may sit on either side.
- **`fixedPathSource`/`fixedPathDest`** PU.java:2032/2051 — rewrite paths when walking into
  datatypes or through contentReference redirects; consume the redirector stack's top frame.
- **`findElementIndex`** PU.java:627 — falls back from object-identity `indexOf` to id-string
  equality (the algorithm often holds copies, not originals).

## 2. Element merging (`updateFromDefinition`, PU.java:2585) — the real merge
Mental model (comment L2587): `dest` = clone of base snapshot element being built;
`source` = differential. Surprising precedence rules:
- **Types replace wholesale** (PU.java:3053): if types differ, `base.getType().clear()` then
  copy all of derived's — no per-type union.
- **Constraints accumulate, never replace** (PU.java:3084, only added if key not present).
  Conditions likewise additive.
- **Binding can only narrow**; weakening REQUIRED emits an error. If after type-merge the
  element has a binding but no bindable type, the binding is silently deleted (PU.java:3106).
- **`trimDifferential=true` MUTATES the differential in place** — deletes fields/types/bindings
  from the SOURCE that equal base. Caller-visible mutation.
- **Profile-on-type override hack** (PU.java:2619): if source type's profile points to a
  resource/Extension SD, short/definition/comment/alias/mapping are overwritten from that
  profile's root; for non-resource profiles it deliberately does NOT override.
- **Obligation extensions** silently copied into dest (PU.java:2608).
- **`updateFromBase`** PU.java:2004 — fills `ElementDefinition.base` (path/min/max), preferring
  the base element's own `.base` so original origin propagates; stamps SNAPSHOT_BASE_* userData.
- **`fillOutFromBase`** PU.java:1886 — slice templates: "fill missing only," never override.
- **`mergeMarkdown`/`mergeStrings`** PU.java:3134/3152 — the `"..."` convention: derived text
  starting with `...` is APPENDED to base text, not replaced.

## 3. Slicing
- **`isSlicedToOneOnly`** 2397, **`isTypeSlicing`** 2401 (one TYPE discriminator on `$this`).
- **Null-tolerant compatibility triad**: `orderMatches` 2372, `discriminatorMatches` 2376,
  `ruleMatches` 2392 — empty/null side always "matches"; base OPEN→anything, CLOSED→OPENATEND only.
- **`ElementDefinitionCounter`** PU.java:157 — accumulates min/max across slices to validate
  slice cardinality against the slicing element. Slice cardinality is enforced HERE.
- **`makeExtensionSlicing`** 2408 — default extension slicing is hard-coded (url/VALUE/open).
- **`getSliceList`** 604 (same exact path) vs **`getChildMap`** 510 (direct children).

## 4. Type / `[x]` expansion
- **`diffsConstrainTypes`** PU.java:1806 — the hairy one: detects implicit type slicing on a
  `[x]`, deducing each type from (a) single declared type, (b) the suffix after the stem
  (`valueQuantity`→`Quantity`), or (c) the sliceName. Builds the `TypeSlice` list.
- **`isDataType`/`isPrimitive`** 2117/3590/3617 gate whether the algorithm recurses into a
  type's own snapshot. `getTypeForElement` 1612 refuses multi-typed walk unless all Reference.

## 5. Content reference resolution
- **`getChildMap` contentReference branch** PU.java:523 — resolves `#id` / `url#id` and
  RECURSES, but only if not already walked inline (`!walksIntoElement`).
- **`getElementById`** PU.java:3524 — resolves a contentReference URI (incl. cross-SD `url#frag`).
- **`replaceFromContentReference`** PU.java:1870 — clears contentReference and copies target
  types in place so the element becomes typed.
- **`resolveContentReference`** PPP.java:459 — walks BACKWARDS, skipping slices (shared path).
  **`fixForRedirect`** PPP.java:455 is a blunt unanchored `path.replace(...)` — latent gotcha.
- **`ElementRedirection`** + `redirectorStack` (PU.java:1852) — the redirection frames consumed
  by fixedPathSource/Dest. Sort-time chasing has a `MAX_RECURSION_LIMIT` guard (3786).

## 6. External utilities (load-bearing)
- **`FHIRPathEngine fpe`** — compiles/validates constraint & discriminator expressions;
  `setAllowUknownFunctions(true)` toggled around parse (PU.java:4835). PU is its own IEvaluationContext.
- **`ExtensionUtilities`/`ExtensionDefinitions`** — pervasive: obligations, type-parameters,
  snapshot-behavior, version-resolution, "too costly" markers; `NON_INHERITED_ED_URLS` controls
  which extensions are stripped on binding inheritance.
- **`XVerExtensionManager`** PU.java:1182 — cross-version extension profiles are snapshot-generated
  ON DEMAND (a hidden re-entrant `generateSnapshot`).
- **`MappingAssistant.merge`** PU.java:3082 — mapping inheritance (note reversed arg names).
- **`Utilities.uncapitalize`** (type-slice detection), `appendDerivedTextToBase` (the `...` merge).

## 7. Caching / sorting / mutation gotchas
- **`childMapCache`** PU.java:447 — key (514) OMITS the `type` parameter that `getChildMap` accepts
  → potential stale result if called with different `type` for the same element.
- **`snapshotStack`** PU.java:436 — circular-reference guard; throws CIRCULAR_SNAPSHOT on re-entry.
  This is what makes re-entrant gen (xver, datatype profiles) safe.
- **`checkNotGenerating`/`isGeneratingSnapshot`** PU.java:1694 — prevents using a profile's
  snapshot before it's finished generating.
- **`sortDifferential`** PU.java:3815 — stamps `SNAPSHOT_SORT_ed_index` userData, builds a holder
  tree, sorts siblings by base order, then CLEARS and rewrites the differential list in place.
  The comparer lazily resolves & caches `baseIndex`, mutating `path`/`actual` while chasing
  contentReferences and `[x]`; `getComparer` swaps the base SD when sort runs into a datatype.
- **`checkExtensionDoco`** PU.java:1948 — silently overwrites definition/short and clears
  comment/requirements/alias/mapping for any `.extension`/`.modifierExtension` element
  (so extension elements never inherit base Element docs).
- **`getWorkingCode()` not `getCode()`** — used everywhere for type comparison (1156, 1623, 3934);
  resolves FHIRPath/`json-type` aliasing. A naive `getCode()` compare would silently mismatch.

## The userData side-channel (see utils/UserDataNames.java)
State is threaded between passes via `ElementDefinition.userData` keys
(`SNAPSHOT_BASE_PATH`, `SNAPSHOT_BASE_MODEL`, `SNAPSHOT_DERIVATION_POINTER`,
`SNAPSHOT_GENERATED_IN_SNAPSHOT`, `SNAPSHOT_SORT_ed_index`, …) — invisible in method
signatures and easy to miss, but downstream tooling and the sort depend on them.

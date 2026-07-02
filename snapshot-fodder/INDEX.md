# Snapshot-generation source fodder

Extracted from `/home/jmandel/work/ig-publisher-tx-hang/fhir-perf/repos/fhir-core` (FHIR core, R5 module) on the host.
Purpose: reverse-engineer a precise spec for StructureDefinition **snapshot generation**.

All paths below are relative to:
`org.hl7.fhir.r5/src/main/java/org/hl7/fhir/r5/`

## Deliverables in this directory
- **`SPECIFICATION.md`** — the full normative specification of the snapshot algorithm (Parts 0–XII,
  ~2,080 lines). Organized around the two-layer split: Layer A (the pure structural algorithm,
  specified pass-by-pass) vs. Layer B (policy passes like version-pinning). Start here.
- **`NON-OBVIOUS-BEHAVIORS.md`** — a map of the load-bearing helpers and hidden behaviors, with
  precise `file:line` references. The evidence layer behind the spec.
- **`INDEX.md`** (this file) — manifest of the source extract.

## Source extract (raw fodder)
Two forms are provided:
- `files/<relpath>` — each source file, original tree layout preserved.
- `00-ALL-IN-ONE.txt` — every file concatenated with banners (grep-friendly single dump).

## Reading order
1. ProfileUtilities.generateSnapshot → 2. ProfilePathProcessor.processPaths →
3. ProfilePathProcessorState + slice value objects → 4. SnapshotGenerationPreProcessor →
5. updateFromDefinition (the per-element merge) + MappingAssistant.

## Manifest

| Tier | File | Lines | Description |
|---|---|---:|---|
| 1-CORE | `conformance/profile/ProfileUtilities.java` | 5054 | Main engine. generateSnapshot() entry (~L740), updateFromDefinition() element merge (~L2585), updateFromBase(), getById(), pathMatches/pathStartsWith, fixedPathSource/Dest, getChildMap/getChildList, sortDifferential(). |
| 1-CORE | `conformance/profile/ProfilePathProcessor.java` | 1739 | Recursive base/differential walker. processPaths() dispatch; processSimplePath* and processPathWithSlicedBase* branches; slicingMatches, pathsMatch, indexOfFirstNonChild, merge(), checkToSeeIfSlicingExists. |
| 1-CORE | `conformance/profile/ProfilePathProcessorState.java` | 21 | Mutable recursion cursor (base/diff cursors, paths, limits, contextPathSrc/Dst, redirector list). |
| 1-CORE | `conformance/profile/SnapshotGenerationPreProcessor.java` | 1227 | Pre-pass that pushes trailing slice-group properties down into each slice (the multiple-inheritance edge case). |
| 1-CORE | `conformance/profile/PathSlicingParams.java` | 37 | Value object describing an in-progress slice context. |
| 1-CORE | `conformance/profile/BaseTypeSlice.java` | 45 | Base element + start/end index span for a type-slice group. |
| 1-CORE | `conformance/profile/TypeSlice.java` | 25 | Pairs an ElementDefinition with a type, for [x] / type slicing. |
| 1-CORE | `conformance/ElementRedirection.java` | 30 | contentReference redirection record (path rewriting on recursive structures). |
| 1-CORE | `conformance/profile/MappingAssistant.java` | 279 | Merges element.mapping entries from base+derived; mapping merge modes. |
| 1-CORE | `conformance/profile/ProfileKnowledgeProvider.java` | 24 | Caller-supplied SPI for link/binding resolution during generation. |
| 1-CORE | `conformance/profile/BindingResolution.java` | 17 | Tiny holder for a resolved binding (display + url). |
| 2-ENTRY | `context/ContextUtilities.java` | 559 | Convenience generateSnapshot(StructureDefinition) wrapper (~L262/L293): resolves base, sets up ProfileUtilities, lazy generation when a loaded profile has no snapshot. |
| 3-HELPER | `utils/UserDataNames.java` | 165 | String keys for ElementDefinition.userData side-channel used to thread base path/model and other state through generation. |
| 3-HELPER | `utils/ElementDefinitionUtilities.java` | 49 | Small ElementDefinition helpers used while merging. |
| 4-MODEL | `model/StructureDefinition.java` | 5407 | Resource shape: Differential/Snapshot components = the algorithm's input and output element lists. |
| 4-MODEL | `model/ElementDefinition.java` | 13188 | The element record that gets merged (path, slicing, type, cardinality, binding, constraints). Huge generated getters/setters. |

## Notes for the reverse-engineer
- Tier 4 model files are generated boilerplate (getters/setters); treat as the field
  dictionary, not logic. The semantics live entirely in Tier 1.
- Older, monolithic copies of this algorithm exist in the r4/r4b/dstu3 modules
  (`*/conformance/ProfileUtilities.java`) — useful for diffing intent across versions.
- The `userData` side-channel (see UserDataNames) carries state between passes that is
  not visible in method signatures — important and easy to miss.

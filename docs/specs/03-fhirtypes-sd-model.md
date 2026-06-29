# 03 — fhirtypes SD model (StructureDefinition / ElementDefinition in-memory model)

Scope: the in-memory element model used by SUSHI while applying rules: element tree, id/path
parsing, element lookup (`findElement` / `findElementByPath`), slicing structure, snapshot vs
differential, and `captureOriginal`/`calculateDiff`. Exporter wiring and JSON property order are
**out of scope** (other agents). All citations are to `sushi-ts/src` (READ ONLY, v3.20.0).

---

## 1. Purpose

`StructureDefinition` (SD) and `ElementDefinition` (ED) are SUSHI's mutable working model of a FHIR
profile/extension/logical/resource. SUSHI loads a parent SD's **snapshot** into a flat ordered
`elements` array, then applies FSH rules by looking up elements by FSH path, mutating them in place,
"unfolding" (grafting in a type's child elements on demand), and creating slices. Each ED remembers
an `_original` snapshot of itself so that at export time SUSHI can compute a minimal **differential**
by diffing current state against that original (`StructureDefinition.ts:449-451`, `605-642`). The
model is deliberately *flat + string-id based*: tree relationships are derived from element `id`
strings, not stored as a real tree, which makes id/path string parsing load-bearing for correctness.

---

## 2. TS entry points

- `class StructureDefinition` — `sushi-ts/src/fhirtypes/StructureDefinition.ts:47`
  - `constructor()` seeds a single root ED — `StructureDefinition.ts:118-128`
  - `get pathType()` — `StructureDefinition.ts:138-142`
  - `addElement(element)` / `addElementToTree` / `escapePath` — `StructureDefinition.ts:163-226`
  - `addElements(elements)` — `StructureDefinition.ts:233-235`
  - `findElement(id)` — `StructureDefinition.ts:242-247`
  - `findElementByPath(path, fisher)` — `StructureDefinition.ts:255-381`
  - `updatePathWithChoices(path)` — `StructureDefinition.ts:391-406`
  - `newElement(name)` — `StructureDefinition.ts:434-442`
  - `captureOriginalElements` / `clearOriginalElements` — `StructureDefinition.ts:449-459`
  - `toJSON(snapshot)` / `fromJSON(json, captureOriginalElements)` — `StructureDefinition.ts:498-586`
  - `sliceMatchingValueX` — `StructureDefinition.ts:819-868`
  - `findMatchingSlice` — `StructureDefinition.ts:878-929`
  - `findMatchingRefOrCanonical` / `getReferenceOrCanonicalName` — `StructureDefinition.ts:966-1011`
  - `type PathPart` — `StructureDefinition.ts:1029-1035`
  - `PROPS` / `PROPS_AND_UNDERPROPS` — `StructureDefinition.ts:1053-1092`
- `class ElementDefinition` — `sushi-ts/src/fhirtypes/ElementDefinition.ts:235`
  - `class ElementDefinitionType` (code getter/setter via extension) — `ElementDefinition.ts:137-233`
  - `get/set id` (derives `path`) — `ElementDefinition.ts:353-367`
  - `newChildElement(name)` — `ElementDefinition.ts:503-507`
  - `captureOriginal` / `clearOriginal` / `clearOriginalProperty` / `calculateClearPath` — `ElementDefinition.ts:514-573`
  - `hasDiff()` / `calculateDiff()` — `ElementDefinition.ts:579-642`
  - `getSlices()` — `ElementDefinition.ts:823-834`
  - `findConnectedElements` / `findConnectedSliceElement` / `findParentSlice` — `ElementDefinition.ts:1000-1049`
  - `parent()` / `getAllParents()` / `children(directOnly)` — `ElementDefinition.ts:2598-2651`
  - `slicedElement()` — `ElementDefinition.ts:2671-2677`
  - `unfold(fisher)` / `cloneChildren` — `ElementDefinition.ts:2687-2836`
  - `unfoldChoiceElementTypes(fisher)` — `ElementDefinition.ts:2910-2939`
  - `sliceIt(...)` / `addSlice(name, type)` — `ElementDefinition.ts:2969-3085`
  - `clone(clearOriginal)` — `ElementDefinition.ts:3101-3121`
  - `toJSON()` / `fromJSON(json, captureOriginal)` — `ElementDefinition.ts:3127-3178`
  - `PROPS` / `ADDITIVE_PROPS` / `REPLACEMENT_PROPS` — `ElementDefinition.ts:3285-3345`
- Path helpers: `parseFSHPath` / `assembleFSHPath` — `sushi-ts/src/utils/PathUtils.ts:12-69`;
  `splitOnPathPeriods` — `sushi-ts/src/fhirtypes/common.ts:87-89`.

---

## 3. Key data structures

### StructureDefinition
- `resourceType = 'StructureDefinition'` (const), plus the FHIR metadata props in `PROPS`
  (`id`, `url`, `version`, `name`, `type`, `baseDefinition`, `derivation`, `kind`, `mapping`, …) —
  `StructureDefinition.ts:48-81`, `1053-1087`. Each prop has an `_`-underscore twin
  (`PROPS_AND_UNDERPROPS`, `StructureDefinition.ts:1089-1092`) for FHIR primitive extensions.
- `elements: ElementDefinition[]` — the flat ordered snapshot; "snapshot vs differential" are NOT
  stored separately, both are derived from this array (`StructureDefinition.ts:42-43,87`).
- `derivation: 'specialization' | 'constraint'` — controls whether the root element is forced into
  the differential (`StructureDefinition.ts:81,523`).
- private `_sdStructureDefinition` (SD-of-SD cache, `:92,149-156`), `originalMapping`
  (`:93,465-467`), public `inProgress` flag for circular-dependency detection (`:113,542-544,582-584`).
- `name`/`id` are mixed in via `HasName`/`HasId` (`StructureDefinition.ts:1014-1015`).

### ElementDefinition
- `private _privateId` exposed via `get/set id`; **setting `id` recomputes `path`** by stripping all
  `:sliceName` segments (`ElementDefinition.ts:353-367`). `path` is therefore a derived field — never
  set it independently.
- Tree fields `treeParent: ElementDefinition` / `treeChildren: ElementDefinition[]`
  (`ElementDefinition.ts:338-339`) — lazily computed caches, NOT authoritative; the id strings are
  authoritative.
- `_original: ElementDefinition` — captured snapshot for diffing (`:340`).
- `_replacementProps: string[][]` — per-instance mutable clone of `REPLACEMENT_PROPS`, consumed once
  per replacement path (`:342,350,559-572`).
- Slicing: `sliceName`, `sliceIsConstraining`, `slicing: ElementDefinitionSlicing`
  (`{discriminator[], description?, ordered?, rules}`) — `:241-245`, `3181-3195`.
- `type: ElementDefinitionType[]`; `ElementDefinitionType` wraps `_actualCode` and exposes a `code`
  getter that prefers a `structuredefinition-fhir-type` extension's `valueUrl`/`valueUri`
  (`ElementDefinition.ts:137-173`). `toJSON` re-emits `code` from `getActualCode()`, dropping the
  private `_actualCode` (`:185-196`).
- The big block of `fixed*`/`pattern*`/`defaultValue*`/`minValue*`/`maxValue*` typed fields
  (`:263-322`) collapses to the choice props `fixed[x]`/`pattern[x]`/… in `PROPS`
  (`ElementDefinition.ts:3306-3313`).

### PathPart (`StructureDefinition.ts:1029-1035`, produced by `parseFSHPath`)
`{ base: string; brackets?: string[]; primitive?: boolean; slices?: string[]; prefix?: string }`.
`base` includes a trailing `[x]` if present; `brackets` are the contents of each `[...]` (slice
names, ref/profile names, numeric/`+`/`=` indices); `slices` accumulates non-index bracket values
seen so far (`PathUtils.ts:18-33`).

---

## 4. Algorithms & control flow (order matters)

### 4.1 id → path derivation
`set id` runs `id.replace(/(\.[^.:]+):[^.]+/g, '$1')` — for each path segment, drop a single
`:sliceName` suffix, keeping the element name. `Observation.component:abc.code` → `path`
`Observation.component.code` (`ElementDefinition.ts:362-367`). Note this only strips `:` introduced
*after a `.`* — the regex requires `\.` before the name. Reslices `:a/b` are stripped as one
`:[^.]+` unit because `/` is not `.` or `:`.

### 4.2 `addElement` insertion order (`StructureDefinition.ts:163-212`)
SUSHI keeps `elements` in a specific FHIR snapshot order. Algorithm:
1. set `element.structDef`, then `addElementToTree` sets `treeParent = element.parent()` and pushes
   onto the parent's `treeChildren` (`:214-222`).
2. If the element has a `sliceName`: find its `slicedElement()`, start `i` at its index, and walk
   forward while the current element's id is a descendant (`^id[.:/]`) of the running `lastMatchId`;
   break when the current id is no longer under `lastMatchId`, OR when inserting a non-slice child
   would otherwise land after sibling slices (the `[.]` vs `[:/]` test). Splice at `i`
   (`:167-191`). Ids are regex-escaped via `escapePath` (`.[]/​` → `\$&`, `:224-226`).
3. If NOT a slice: if it's the parent's only child, splice right after the parent; else find the
   next-older sibling (`treeChildren.slice(-2,-1)`), take that sibling's last (deep) child, and
   splice after it — falling back to right-after-sibling if the sibling has no children
   (`:195-209`). **Order rule: all plain children precede all slices at a given level; a slice goes
   after the sliced element and its existing reslices/children.**

### 4.3 `findElementByPath` (`StructureDefinition.ts:255-381`) — the core lookup
1. Fast path: build `fullPath = "{pathType}.{path}"` (or just `pathType` if path empty/`.`) and
   return the first element whose `path === fullPath` AND whose id contains no `:` (i.e. not a
   slice) (`:258-262`).
2. Else `parseFSHPath(path)` and iterate `pathPart`s, maintaining `fhirPathString` (accumulated FHIR
   path) and a shrinking `matchingElements` candidate set (starts = all elements). For each part:
   a. Append `.{pathPart.base}`; filter candidates to those whose `path` starts with
      `fhirPathString.`/`fhirPathString:` or equals it (`:275-281`).
   b. **Unfold on demand**: if no matches and exactly one candidate remained, call
      `candidate.unfold(fisher)` and re-filter; if still nothing and the candidate id ends with
      `[x]:{prev}` or `[x]`, call `unfoldChoiceElementTypes` (`:287-307`).
   c. If still nothing, try `sliceMatchingValueX(fhirPathString, matching+unfolded)` to resolve e.g.
      `valueString` → a type slice of `value[x]` (creating the slice if needed); on hit, push the
      slice + its children + its slices and set `fhirPathString = slice.path` (`:309-325`).
   d. If matches found, commit them; else `return` undefined (`:327-333`).
   e. If `pathPart.brackets`: resolve a slice via `findMatchingSlice`, else a ref/canonical via
      `findMatchingRefOrCanonical`; narrow `matchingElements` to `[match, ...match.children()]` or
      return undefined (`:336-355`).
   f. If NO brackets: drop slice candidates at the current depth unless the slice is the choice slice
      equivalent (`idEnd === "{pathEnd}:{base}"` and `pathEnd !== base`) (`:358-373`).
3. After the loop, filter to `path === fhirPathString` and return the element **only if exactly one
   remains**, else undefined (`:378-380`).

### 4.4 `unfold` (`ElementDefinition.ts:2687-2805`)
Proceeds only if single type with ≤1 profile (for `[x]`), or has `contentReference`
(`:2690-2693`). Three sources, in order:
1. `contentReference`: resolve the referenced element in the *same or parent* SD and clone its
   children, then drop `contentReference` and adopt the referenced type (`:2706-2735`).
2. `sliceName`: clone children from `slicedElement()` (`cloneChildren(..., false)` to avoid
   recapturing slice extensions) (`:2736-2757`).
3. Fallback: fish the type/profile JSON, build an SD via `fromJSON`, clone `elements.slice(1)`,
   rewrite each id replacing `def.pathType` with `this.id`, `removeUninheritedExtensions()`, and
   **`captureOriginal()` each clone** so later diffs only show post-unfold changes (`:2758-2798`).
Cloned elements are added via `structDef.addElements` (which re-runs §4.2 ordering) (`:2799-2802`).
`cloneChildren` rewrites ids `replace(targetElement.id, this.id)` and, when not recapturing, fixes
`_original.id` so id-only diffs don't fire (`:2814-2836`).

### 4.5 Slicing (`sliceIt` / `addSlice`)
- `sliceIt` warns if called on a slice, creates `slicing` with one discriminator (default
  `ordered=false`, `rules='open'`) or merges into existing slicing — **throws** if `ordered`
  true→false or `rules` closed→open/openAtEnd or openAtEnd→open; appends a new discriminator only if
  `(type,path)` not already present (`ElementDefinition.ts:2969-3025`).
- `addSlice`: requires the element to have `slicing` or be a slice itself, else throws
  `SlicingNotDefinedError` (`:3037-3039`). Clones self, deletes `slicing`, sets id
  `{id}:{name}` (or `{id}/{name}` for reslice), throws `DuplicateSliceError` if id exists, deletes
  `min/max/mustSupport` then `captureOriginal()` (so they always appear in diff), sets `sliceName`
  (`{parent}/{name}` for reslice). `min` becomes 0 **unless** slicing a choice already narrowed to a
  single type with `type/$this` discriminator (then keeps parent `min`); `max` copies parent;
  optional `type` overrides (`:3041-3084`).

### 4.6 captureOriginal / hasDiff / calculateDiff
- `captureOriginal`: `_original = this.clone()` with `structDef` nulled (`:514-517`). `clone` deep-
  clones but temporarily detaches `structDef`/`treeParent`/`treeChildren` so they aren't deep-cloned,
  reattaches `structDef` to the clone, and by default clears the clone's `_original`
  (`:3101-3121`).
- `hasDiff`: true if any `PROPS_AND_UNDERPROPS` value differs (`isEqual`) from `_original` (or a
  fresh blank ED if no original) — choice props `xxx[x]` are resolved to the concrete key present on
  `this` or `original` via regex; **plus** a slice/sliced element is dirty if any child `hasDiff`
  (`ElementDefinition.ts:579-598`).
- `calculateDiff`: new ED with same id; copies each changed prop; for `ADDITIVE_PROPS`
  (`mapping`,`constraint`) emits only `differenceWith(this, original)` and drops if empty; special
  case: a slice of a choice (`sliceName` && path ends `[x]`) always emits `type` even if equal; if
  original had a `sliceName`, force it onto the diff (`:605-642`, `3335`).

### 4.7 toJSON / fromJSON (model side only)
- SD `toJSON(snapshot=true)`: copy `PROPS_AND_UNDERPROPS`; build snapshot from
  `elements.map(e.toJSON())`; build differential by iterating elements and including any with
  `hasDiff()` OR (root element when `derivation==='specialization'`) → `calculateDiff().toJSON()`;
  if differential empty, push a stub `{id,path}` of the root; persist `inProgress` only if true
  (`StructureDefinition.ts:498-547`).
- SD `fromJSON`: copies props (deep-cloned), **clears the seeded root** (`elements.length = 0`),
  loads `snapshot.element` only (differential discarded), each via `ElementDefinition.fromJSON`,
  setting `structDef` and `addElementToTree`; throws `MissingSnapshotError` if no snapshot
  (`:557-586`).
- ED `fromJSON` clones json, copies props (`type` mapped through `ElementDefinitionType.fromJSON`),
  and `captureOriginal()` unless told not to (`:3155-3178`).

---

## 5. Edge cases & gotchas

1. **`path` is derived from `id`, recomputed on every `id` assignment** — never store/set `path`
   independently; in Rust make `path` a computed function or a field always updated through the id
   setter (`ElementDefinition.ts:362-367`).
2. **`code` is not `_actualCode`.** `ElementDefinitionType.code` getter returns the
   `structuredefinition-fhir-type` extension's `valueUrl`/`valueUri` when present (used for
   `Element.id`, `Extension.url`, primitive types in R4/R5). `getActualCode()` is the raw code.
   `findElementByPath` choice matching uses `t.code` (the getter), so this indirection affects path
   resolution (`ElementDefinition.ts:159-173`, `StructureDefinition.ts:823-824`).
3. **`treeParent`/`treeChildren` are lazy caches keyed off the elements array.** `parent()` computes
   from id substring before last `.` and caches; `children()` filters the whole `elements` array by
   id-prefix + depth and caches into `treeChildren` (`ElementDefinition.ts:2598-2608,2631-2651`).
   A naive port that treats these as a real owned tree will desync after `addElement`/`unfold`
   splices. SUSHI mitigates by setting them eagerly in `addElementToTree`, but `children()` still
   self-populates on first call from the flat array — replicate the *exact* filter
   (`e.id.startsWith(id+".")` AND `path` depth == parent depth +1).
4. **Insertion ordering is string/regex-driven, not numeric.** The slice-insertion loop uses regex
   tests on escaped ids with the `[.:/]` character classes to keep reslices and children ordered;
   reproduce the regex semantics exactly, including `escapePath` escaping `.[]/` (NOT `:` or `+`)
   (`StructureDefinition.ts:175-191,224-226`).
5. **Unfold mutates the SD as a side effect of a *lookup*.** `findElementByPath` calls `unfold`,
   which `addElements` new children and `captureOriginal`s them. A failed deeper lookup leaves those
   unfolded elements on the SD (acknowledged TODO, `StructureDefinition.ts:283-285`). Parity requires
   the same residue.
6. **`sliceMatchingValueX` may *create* a type slice during lookup** (calls `sliceIt`+`addSlice`),
   but returns the bare `[x]` element without slicing when it's the sole match already restricted to
   one type with no existing slices (`StructureDefinition.ts:834-868`).
7. **`addSlice` deletes `min/max/mustSupport` then captures original, then re-sets min/max** — so
   `min`/`max` are *always* in the slice's differential, and `min` defaults to 0 except the
   single-type choice `type/$this` case (`ElementDefinition.ts:3051-3079`).
8. **Diff additive props vs replacement props.** `mapping`/`constraint` diff additively
   (`differenceWith`); `type.aggregation` is a one-shot "replacement" path consumed via
   `_replacementProps` during `clearOriginalProperty` (`ElementDefinition.ts:3335,3345`,
   `559-572`). `_replacementProps` is per-instance mutable and spliced once.
9. **`hasDiff`/`calculateDiff` walk children for slices** — a slice or sliced element with no own
   change but a changed child is still emitted; required by IG Publisher
   (`ElementDefinition.ts:593-597`).
10. **Root element forced into differential for `specialization`** (logical models/resources) even
    when it has no diff (`StructureDefinition.ts:523`); and an empty differential is replaced by a
    `{id,path}` stub (`:530-535`).
11. **`fromJSON` discards the differential and requires a snapshot**, and resets the constructor's
    seeded root before loading (`StructureDefinition.ts:569-580`).
12. **`pathType` strips a URL** (`type` may be a full URL for logical models): take substring after
    last `/` only when it starts with `http` (`StructureDefinition.ts:138-142`). Used to build
    fhir paths everywhere — get this wrong and every lookup breaks.
13. **`parseFSHPath` bracket regex** matches *outermost* bracket pairs (one level of nesting only)
    and logs (not throws) on unmatched-bracket length mismatch; `splitOnPathPeriods` splits on
    periods *not inside* brackets (`PathUtils.ts:18-47`, `common.ts:87-89`).
14. **`findMatchingSlice` URL-encodes and retries**, clones connected reslices from the original
    sliced element across the SD, and falls back to fishing predefined extensions by url
    (`StructureDefinition.ts:888-923`). The bracket syntax is ambiguous: `foo[a][b]` is treated as
    reslice `a/b` here, but as ref `b` on slice `a` by `findMatchingRefOrCanonical` (`:924-928`).
15. **`getSlices` differs by self being a slice**: matches sibling ids with `/` suffix (reslices) vs
    `:` suffix (slices), filtered to same `path` (`ElementDefinition.ts:823-834`).

---

## 6. Recommended Rust mapping

**Crate: `fhir_model`.** This is the FHIR-side mutable model (distinct from `fsh_model`, which holds
parsed FSH AST). It depends on `package_store`/`Fishable` for `unfold`/fishing, and is consumed by
`compiler` (rule application) and `json_emit` (ordered serialization — out of scope here).

Suggested types/indexes:
- `struct StructureDefinition { meta props…, elements: Vec<ElementId>, arena: ElementArena, derivation, kind, type_, original_mapping, in_progress, … }`.
- **Use an arena/slotmap** (`slotmap::SlotMap<ElementKey, ElementDefinition>`) plus an ordered
  `Vec<ElementKey>` for `elements`. This replaces TS object-identity (`e !== this`,
  `indexOf(element)`) cleanly and avoids `Rc<RefCell>` cycles for `structDef`/`treeParent`.
- `struct ElementDefinition { id: ElementId, path: ElementPath /*derived*/, slice_name, slicing, type_: Vec<ElementDefinitionType>, fixed_pattern: BTreeMap-or-enum, original: Option<Box<ElementDefinition>>, replacement_props, … }`.
- Make `id` a setter-like method (`set_id`) that recomputes `path` via the same regex
  (`(\.[^.:]+):[^.]+` → `$1`). Keep `path` private, exposed read-only.
- **Maintain a `HashMap<String /*id*/, ElementKey>` index** on the SD to make `findElement` O(1)
  (TS uses linear `find`; behavior is identical as long as ids are unique). Rebuild/patch on every
  `add_element`/`splice`.
- `ElementDefinitionType.code` ⇒ a method that scans `extension` for the fhir-type url and returns
  `value_url`/`value_uri`; keep `actual_code` as the stored field.
- `parseFSHPath` ⇒ a `parse_fsh_path(&str) -> Vec<PathPart>` in a shared `paths` module (likely
  `fsh_model` or a small `paths` util shared by both); `splitOnPathPeriods` via the same negative-
  lookahead semantics (Rust `regex` lacks lookahead — implement a manual bracket-depth splitter).
- Slicing/insertion: port `add_element`'s regex walk verbatim using precompiled `regex` per call
  (or a hand-written id-prefix comparator that mimics the `[.:/]` boundary classes). Do not "improve"
  it — order is part of the contract.
- `clone`/`captureOriginal` ⇒ plain `#[derive(Clone)]` deep clone with `structDef`/tree refs being
  keys (cheap to copy) rather than owned pointers; null them in `_original` by storing
  `original: Option<Box<ElementDefinition>>` that simply omits arena keys.

Connections: `unfold`/`sliceMatchingValueX`/`fromJSON` call into `Fishable` (package_store). Rule
application (compiler) drives `findElementByPath` + mutation + `addSlice`/`sliceIt`. `json_emit`
calls `toJSON`/`calculateDiff` (covered by sibling specs for ordering).

---

## 7. Parity test ideas

Each fixture = a parent SD JSON + a small FSH profile; compare resulting SD `toJSON()` snapshot AND
differential element-by-element against stock SUSHI (`v3.20.0`).

1. **id↔path + slicing order**: profile that slices `Observation.component` into two slices each with
   a child rule; assert exact `elements` ordering (slices after plain children; reslices nested) and
   that slice `min/max` always appear in the differential (`addElement` §4.2, `addSlice` §4.5).
2. **Choice type slicing**: `* value[x] only string` vs `* valueString MS` vs
   `* value[x][valueString]`; assert when SUSHI returns the bare `[x]` element vs creates a type
   slice, and that the slice's `type` is always emitted in diff (`sliceMatchingValueX`,
   `calculateDiff:631-635`).
3. **Unfold-on-lookup residue**: a rule path `A.B.C` where `B` unfolds but `C` is invalid; assert the
   unfolded `B.*` children remain on the SD and were `captureOriginal`'d (no spurious diff) (§4.4,
   gotcha 5).
4. **contentReference unfold**: profile that constrains a `Questionnaire.item.item` style
   self-reference; assert `contentReference` removed and type adopted only when children clone
   (`unfold:2706-2735`).
5. **Additive diff**: add a `constraint`/`mapping` then assert differential contains only the new
   entries, and that an unchanged-but-child-changed slice still appears (`calculateDiff`,
   `hasDiff:593-597`).
6. **Reslice + ref bracket ambiguity**: `extension[a][b]` (reslice) vs `value[Reference][profileX]`;
   assert `findMatchingSlice` vs `findMatchingRefOrCanonical` dispatch and URL-encoded retry
   (`findMatchingSlice:888-923`).
7. **specialization root / empty differential stub**: a Logical model with only flag rules; assert
   root element present in differential and the `{id,path}` stub when nothing changed
   (`toJSON:520-535`).
8. **fhir-type extension code**: an element whose `type[0]` carries the
   `structuredefinition-fhir-type` extension; assert path resolution uses `valueUrl` and `toJSON`
   re-emits the raw `code` (`ElementDefinitionType:159-196`).
9. **pathType URL stripping**: a logical model whose `type` is a full URL; assert all derived FHIR
   paths use the last URL segment (`pathType:138-142`).

---

## 8. Open questions

1. **Shared `parseFSHPath` ownership**: does it live in `fsh_model` (FSH-facing) or a neutral util
   crate? Both `fhir_model` (lookup) and the importer use it. Recommend a small shared `paths` module
   to avoid a circular crate dep.
2. **Soft-index conversion** (`convertSoftIndices*` in `PathUtils.ts:77+`) is used by the importer,
   not directly by this SD model; confirm with the import-subsystem agent whether `PathPart.prefix`/
   `slices` need to round-trip through `fhir_model` at all (only `base`/`brackets` appear used here).
3. **`fishForFHIRBestVersion` / `Fishable` contract**: `unfold` and `sliceMatchingValueX` depend on
   fishing across `Type.{Resource,Type,Profile,Extension,Logical}` with version fallback
   (`ElementDefinition.ts:2762-2779`). The package_store spec must expose this exact multi-type,
   best-version lookup.
4. **Object identity vs id equality**: TS uses `e !== this` / `indexOf` (reference identity) in
   `children()`, `getSlices()`, `addElement`. With an arena, key equality is equivalent — but verify
   no path relies on two distinct EDs sharing the same id (ids are assumed unique;
   `addSlice`/`newElement` throw on duplicates — `:3046-3049`, `StructureDefinition.ts:436-438`).
5. **`removeUninheritedExtensions` / `UNINHERITED_ED_EXTENSIONS`** (`ElementDefinition.ts:88-133,
   2941-2953`) prunes specific extensions on unfold/clone. Confirm the exact list is ported (affects
   snapshot byte parity) — it is referenced here but its content belongs to a shared constants file.

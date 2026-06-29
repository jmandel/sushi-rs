# 05 — JSON Emission Order

> Compatibility port spec. Every behavioral claim cites `path:line` in `sushi-ts/` (submodule `sushi-ts@v3.20.0`, READ ONLY). Goal: **byte-identical** `fsh-generated/resources/*.json` output vs stock SUSHI.

## 1. Purpose

This subsystem converts SUSHI's in-memory FHIR resource objects (`StructureDefinition`, `ElementDefinition`, `InstanceDefinition`, `CodeSystem`, `ValueSet`, and the `ImplementationGuide`) into FHIR-conformant JSON, and writes those files to disk. It owns two distinct concerns: (a) the **order** of properties within each emitted object, and (b) the **textual formatting** of the JSON file (indentation, trailing newline, escaping). Property order is established three different ways depending on resource type: a fixed `PROPS` array (StructureDefinition / ElementDefinition), a fixed prefix followed by JS object-insertion order (InstanceDefinition), or pure JS object-insertion order (CodeSystem / ValueSet). Underscore "primitive sibling" keys (`_x`) are always reordered to sit immediately after their base key `x` by `orderedCloneDeep`. Getting either concern wrong produces non-byte-identical output even when the data is semantically correct.

## 2. TS entry points

- `sushi-ts/src/fhirtypes/common.ts:1571` — `orderedCloneDeep(input, keys?)`: the universal recursive reorderer. Used by every resource `toJSON`.
- `sushi-ts/src/fhirtypes/StructureDefinition.ts:498` — `StructureDefinition.toJSON(snapshot = true)`.
- `sushi-ts/src/fhirtypes/StructureDefinition.ts:1053` — `PROPS` array (canonical SD top-level order); `:1089` — `PROPS_AND_UNDERPROPS`.
- `sushi-ts/src/fhirtypes/ElementDefinition.ts:3127` — `ElementDefinition.toJSON()`.
- `sushi-ts/src/fhirtypes/ElementDefinition.ts:3285` — `PROPS` array (canonical element order); `:3325` — `PROPS_AND_UNDERPROPS`.
- `sushi-ts/src/fhirtypes/InstanceDefinition.ts:34` — `InstanceDefinition.toJSON()`.
- `sushi-ts/src/fhirtypes/CodeSystem.ts:74` — `CodeSystem.toJSON()`.
- `sushi-ts/src/fhirtypes/ValueSet.ts:74` — `ValueSet.toJSON()`.
- `sushi-ts/src/fhirtypes/common.ts:1192` — `cleanResource(resourceDef, skipFn)`: strips temp props and normalizes empty objects *before* `toJSON`.
- `sushi-ts/src/fhirtypes/common.ts:1161` — `replaceField(...)`: recursive walker used by `cleanResource`.
- `sushi-ts/src/fhirtypes/common.ts:631` — `setPropertyOnInstance(...)`: assigns nested props (establishes insertion order for instances/caret values, and creates `_x` siblings).
- `sushi-ts/src/fhirtypes/common.ts:540-595` — implied-property assignment ordering (`setImpliedPropertiesOnInstance` logic) inside `setImpliedPropertiesOnInstance`.
- `sushi-ts/src/utils/Processing.ts:649` — `writeFHIRResources(...)`: the disk writer; `:676` `fs.outputJSONSync(..., { spaces: 2 })`.
- `sushi-ts/src/utils/Processing.ts:595` — `checkNullValuesOnArray(...)`: pre-write validation (does not reorder).
- `sushi-ts/src/run/FshToFhir.ts:80-95` — in-memory API path: iterates artifact buckets in a fixed order and calls `toJSON(snapshot)`.
- `sushi-ts/src/ig/IGExporter.ts:1412` — writes `ImplementationGuide-*.json` via `outputJSONSync(igJsonPath, this.ig, { spaces: 2 })`.
- jsonfile (bundled under fs-extra) `utils.js` — `stringify(obj, { EOL='\n', finalEOL=true, spaces })`: formatting/EOL source of truth (see §4).

## 3. Key data structures

- `PROPS: string[]` (SD) — `sushi-ts/src/fhirtypes/StructureDefinition.ts:1053`. Exact order: `id, meta, implicitRules, language, text, contained, extension, modifierExtension, url, identifier, version, name, title, status, experimental, date, publisher, contact, description, useContext, jurisdiction, purpose, copyright, keyword, fhirVersion, mapping, kind, abstract, context, contextInvariant, type, baseDefinition, derivation`.
- `PROPS_AND_UNDERPROPS: string[]` (SD) — `:1089`. Built by interleaving each prop with its `_`-prefixed form: `[id, _id, meta, _meta, ...]`.
- `PROPS: string[]` (ElementDefinition) — `ElementDefinition.ts:3285`. Exact order: `id, extension, modifierExtension, path, representation, sliceName, sliceIsConstraining, label, code, slicing, short, definition, comment, requirements, alias, min, max, base, contentReference, type, defaultValue[x], meaningWhenMissing, orderMeaning, fixed[x], pattern[x], example, minValue[x], maxValue[x], maxLength, condition, constraint, mustSupport, isModifier, isModifierReason, isSummary, binding, mapping`. Note the `[x]` choice entries.
- `PROPS_AND_UNDERPROPS: string[]` (ElementDefinition) — `:3325`. Same interleave rule.
- `InstanceDefinition` — `InstanceDefinition.ts:15`. Has fixed declared fields `_instanceMeta`, `resourceType`, `meta`, plus an index signature `[key: string]: any` (`:20`) for arbitrary FHIR props. `_instanceMeta` (`:57`) is SUSHI bookkeeping, NOT a FHIR property, and is explicitly excluded from output.
- `CodeSystem` class fields — `CodeSystem.ts:16-51`. Declaration order matters only for the **initialized** fields (see §5). Field `resourceType` is a `readonly` initializer; `status='draft'` (`:31`) and `content='complete'` (`:46`) are initialized.
- `ValueSet` class fields — `ValueSet.ts:25-51`. Initialized: `resourceType` (`:26`), `status='draft'` (`:40`).
- `LooseStructDefJSON` — `StructureDefinition.ts:1040`; `LooseElementDefJSON` — `ElementDefinition.ts:3267`.

## 4. Algorithms & control flow

### 4.1 `orderedCloneDeep(input, keys?)` — `common.ts:1571`
Recursive deep clone that fixes property order:
1. If `input` is not object-like → `cloneDeep(input)` (`:1574`).
2. If `input` is an array → map `orderedCloneDeep` over elements; **array element order is preserved as-is, never reordered** (`:1576`).
3. Else (plain object):
   a. `keys = keys ?? Object.keys(input)` — default to JS insertion order (`:1579`).
   b. Pull out all keys starting with `_` into `underscoreKeys` (mutates `keys` via lodash `remove`) (`:1582`).
   c. Walk remaining (non-underscore) keys in order; for each `key`, push `key`, then if `_key` exists push it immediately after and remove it from `underscoreKeys` (`:1586-1592`).
   d. Append any leftover underscore keys (those with no matching base key) at the end, in their original order (`:1593-1595`).
   e. Build `result` by inserting keys in that computed order, recursing into each value (`:1597-1599`).
**Consequence:** the relative order of base keys equals the order of the supplied `keys` (or `Object.keys`); each `_x` is glued directly after `x`; orphan `_x` keys go last.

### 4.2 `StructureDefinition.toJSON(snapshot=true)` — `StructureDefinition.ts:498`
1. `j = { resourceType }` first (`:499`).
2. Iterate `PROPS_AND_UNDERPROPS` in array order; copy each defined `this[prop]` onto `j` (`:501-511`). This establishes top-level order = `PROPS` order (with `resourceType` forced first). `mapping` is special-cased to `buildMappingJSON` (`:504`, defined `:460`).
3. If `snapshot` true: `j.snapshot = { element: this.elements.map(e => e.toJSON()) }` (`:514-516`).
4. Always build `j.differential = { element: [...] }` by iterating elements in order; include an element if `e.hasDiff()` or (derivation==='specialization' && idx===0) (`:518-527`).
5. If differential empty, push a single `{ id, path }` stub from the root element (`:529-535`) — never emit an empty differential.
6. If `this.inProgress`, set `j.inProgress = true` (`:542-544`).
7. `return orderedCloneDeep(j)` (`:546`) — re-clones and applies underscore-sibling gluing. Key order entering this is already `PROPS`-driven; `snapshot`/`differential`/`inProgress` were appended after step 2, so they end up after `derivation`.

### 4.3 `ElementDefinition.toJSON()` — `ElementDefinition.ts:3127`
1. `j = {}` (`:3128`).
2. Iterate `PROPS_AND_UNDERPROPS` (`:3129`). For a `[x]` prop, resolve the actual key by regex `^<base>[A-Z].*$` against `Object.keys(this)` (`:3130-3133`) — picks whatever concrete choice key (e.g. `fixedString`) exists.
3. If prop resolved and `this[prop] !== undefined`, copy it; `type` is special-cased to `this.type.map(t => t.toJSON())` (`:3135-3142`).
4. Returns `j` directly — **no** `orderedCloneDeep` call here (unlike SD). Order = `PROPS` order; underscore gluing for elements only happens later when the whole SD is passed through `orderedCloneDeep` in 4.2 step 7.

### 4.4 `InstanceDefinition.toJSON()` — `InstanceDefinition.ts:34`
1. `orderedKeys = ['resourceType','_resourceType','id','_id','meta','_meta'].filter(k => this[k] != null)` (`:35-37`) — a fixed leading prefix.
2. `additionalKeys = difference(Object.keys(this), [...orderedKeys, '_instanceMeta'])` (`:39`) — everything else in JS insertion order, **excluding** `_instanceMeta`.
3. `return orderedCloneDeep(this, [...orderedKeys, ...additionalKeys])` (`:40`).
**Order:** `resourceType, id, meta` (with their `_` siblings) first, then all other FHIR props in the order they were assigned during export, with `_x` glued after `x` by `orderedCloneDeep`.

### 4.5 `CodeSystem.toJSON()` / `ValueSet.toJSON()` — `CodeSystem.ts:74`, `ValueSet.ts:74`
`return { ...orderedCloneDeep(this) }`. No `keys` arg → order = `Object.keys(this)` insertion order, with `_x` gluing. **There is no canonical PROPS reordering for these two types** — the property order is whatever order the exporter assigned fields plus the constructor-initialized fields (see §5.2).

### 4.6 Instance/caret property assignment order — `common.ts:631`, `:540-595`
The insertion order that 4.4/4.5 depend on is produced by `setPropertyOnInstance` (`:631`) walking `pathParts`, creating nested objects/arrays lazily and creating `_<base>` sibling arrays/objects when a primitive has children (`:645-646`, `:656-666`). For *implied* (min-cardinality) properties, paths are first reordered (`:547-589`): a tree is built so a path comes before its ancestors (depth-first postfix, `:602-628`), then unless `manualSliceOrdering` is set, a stable sort by `requirementRoot` / first-rule path-overlap is applied (`:551-590`). Rule-set caret values are otherwise applied in source order.

### 4.7 `cleanResource` — `common.ts:1192` (runs before `toJSON`, e.g. `InstanceExporter.ts:943`)
1. Delete every `_sliceName` temp prop (`:1197-1202`).
2. Replace every empty-object value `{}` with `null` (`:1204-1208`).
3. (continues past line 1210) converts `_primitive`-marked wrapper objects back to primitives via `replaceField`.
`replaceField` (`:1161`) iterates with `for..in` (insertion order); if an array becomes all-null it deletes the whole array key (`:1173-1176`). This mutates structure/order **before** serialization.

### 4.8 Disk write & formatting — `Processing.ts:649`, jsonfile `utils.js`
- `writeFHIRResources` iterates artifact buckets; for each non-predefined resource calls `checkNullValuesOnArray(resource)` (`:675`, validation only) then `fs.outputJSONSync(path, resource.toJSON(snapshot), { spaces: 2 })` (`:676`).
- `snapshot` originates from CLI flag `-s/--snapshot`, default **false** (`app.ts:76`, threaded to `writeFHIRResources` at `app.ts:316`); the in-memory API defaults it false too (`FshToFhir.ts:51`).
- `outputJSONSync` → jsonfile `stringify`: `JSON.stringify(obj, null, 2)` then `.replace(/\n/g, EOL)` with `EOL='\n'`, then append `finalEOL` = one trailing `'\n'`. So each file is 2-space-indented JSON **terminated by exactly one newline**.
- Output path: `<outDir>/fsh-generated/resources/<getFileName()>` (`:665`). File names: `StructureDefinition-<id>.json` style via each type's `getFileName()` (e.g. `CodeSystem.ts:66`, `InstanceDefinition.ts:26`), all passed through `sanitize-filename` with `replacement: '-'`.
- IG resource written separately: `outputJSONSync(igJsonPath, this.ig, { spaces: 2 })` (`IGExporter.ts:1412`); `this.ig` order is whatever the IGExporter built (out of scope here, but same formatting).
- The in-memory `FshToFhir` path (`FshToFhir.ts:80-95`) does NOT write files; it returns `fhir[]` in bucket order `profiles, extensions, instances, valueSets, codeSystems, logicals, resources`.

## 5. Edge cases & gotchas

1. **CodeSystem/ValueSet order is insertion-order, not a PROPS array** (`CodeSystem.ts:74`, `ValueSet.ts:74`). A naive port that emits CS/VS fields in FHIR canonical order will diverge. The Rust port must reproduce the *exact assignment order* the TS exporters use (e.g. for CS: name→id→title→description→version→status→url→…, per `CodeSystemExporter.ts:31-54`), plus the constructor-initialized fields, plus caret-set fields in source order.
2. **Class-field initialization order depends on `target: ES2018`** (`sushi-ts/tsconfig.json`). With `useDefineForClassFields=false` (the default below ES2022), TS class fields **without initializers are NOT own properties** until assigned. Only *initialized* fields exist at construction: CS → `resourceType, status, content`; VS → `resourceType, status`. Those occupy the head of `Object.keys` in declaration order; everything else appears in assignment order. A Rust port modeling these as an ordered map must seed exactly those initialized keys first, then append on assignment. (CS `status` `:31`, `content` `:46`; VS `status` `:40`.)
3. **`_instanceMeta` must never be emitted** (`InstanceDefinition.ts:39`) — it is filtered out of `additionalKeys`. It is SUSHI bookkeeping (`:57`).
4. **Underscore siblings are glued, not sorted** (`common.ts:1586-1595`). `_x` is moved to immediately follow `x`; an orphan `_x` (no base `x`) goes to the very end. Order among orphans is their original insertion order.
5. **Arrays are never reordered** (`common.ts:1576`). Slice/element order is fixed at assignment time (`setPropertyOnInstance`, and the implied-property sort `common.ts:551-589`). Port that sort faithfully — it is subtle (requirementRoot grouping, path-overlap tiebreak, `manualSliceOrdering` toggle).
6. **ElementDefinition `[x]` resolution via regex** (`ElementDefinition.ts:3130-3133`): the concrete choice key (`fixedCode`, `patternCoding`, …) is found by scanning `Object.keys(this)`; only ONE is expected per choice. The `[x]` placeholder's position in `PROPS` determines where the concrete key lands.
7. **`ElementDefinition.toJSON` does NOT call `orderedCloneDeep`** (`:3145`); underscore gluing for element props is deferred until the enclosing SD calls `orderedCloneDeep(j)` (`StructureDefinition.ts:546`). The in-memory API for SDs therefore relies on SD-level cloning to fix element underscore order.
8. **Empty object → null, then all-null arrays deleted** (`common.ts:1204-1208`, `:1173-1176`). This changes both presence and effective ordering of keys and must run before serialization (`cleanResource` at `InstanceExporter.ts:943`).
9. **`differential` is never empty** (`StructureDefinition.ts:529-535`): a `{id,path}` stub is injected. And the root element is force-included for `specialization` derivation (`:523`).
10. **`inProgress` persists only when true** (`StructureDefinition.ts:542-544`); never emit `inProgress:false`.
11. **Trailing newline + 2-space indent are load-bearing for byte parity** (jsonfile `stringify`, `Processing.ts:677`). `serde_json::to_string_pretty` uses 2 spaces but **no trailing newline** and **a space after `:` only** — verify exact whitespace: `JSON.stringify(x,null,2)` puts no space before `:`, one space after, newline+indent between items, and `[]`/`{}` with no inner space. Append a single `'\n'`.
12. **Non-ASCII / escaping:** `JSON.stringify` does NOT escape non-ASCII (emits raw UTF-8) and does NOT escape `/`. It escapes control chars and `"`,`\`. `serde_json` matches this (no `\u` for non-ASCII, no `/` escaping) — but confirm against fixtures with unicode in descriptions.
13. **Number formatting:** JS `JSON.stringify` renders numbers via the ECMAScript Number-to-string algorithm (e.g. `1`, `1.5`, `1e21`, no trailing `.0`). Rust must match: integers without decimals, no `+`, lowercase `e`. This is a known parity risk for `count`, `min`, `max`, decimals in instances.
14. **`snapshot` defaults to false** (`app.ts:76`, `FshToFhir.ts:51`); default output has no `snapshot` block. Don't emit snapshots unless the flag is set.

## 6. Recommended Rust mapping

**Crate: `json_emit`** (with the resource structs themselves living in `fhir_model`). `json_emit` owns the ordering + formatting; `fhir_model` owns the typed resources; `compiler` calls `json_emit` to write `fsh-generated/resources/`.

- **Ordered map type:** Use `indexmap::IndexMap<String, Value>` (or a custom `serde_json::Map` with `preserve_order` feature enabled) as the in-memory representation for InstanceDefinition / CodeSystem / ValueSet bodies, so insertion order is preserved exactly as assignments happen. Enable serde_json's `preserve_order` feature project-wide.
- **`ordered_clone_deep(value, keys: Option<&[String]>) -> Value`:** port of `common.ts:1571`. Operate on `serde_json::Value`. Implement the underscore-gluing exactly (base keys in `keys`/insertion order, glue `_k` after `k`, orphans last).
- **PROPS constants:** two `static`/`const` `&[&str]` arrays for SD and ElementDefinition matching `StructureDefinition.ts:1053` and `ElementDefinition.ts:3285`; generate `PROPS_AND_UNDERPROPS` at build time by interleaving.
- **`to_json` per type:** mirror each `toJSON`. SD: build via PROPS then `ordered_clone_deep`. ElementDefinition: build via PROPS, NO clone. Instance: fixed prefix + insertion-order remainder, exclude `_instance_meta`. CS/VS: insertion order + clone.
- **Writer:** a single `write_resource(path, value)` that serializes with a custom pretty formatter (2-space indent, `serde_json::ser::PrettyFormatter::with_indent(b"  ")`) and **appends `b"\n"`**. Centralize so newline/indent are consistent. Verify whitespace byte-for-byte against jsonfile output.
- **Connections:** consumes typed resources from `fhir_model`; `compiler` (exporters) must assign CS/VS/Instance fields in the **same order** as the TS exporters (this ordering responsibility leaks into `compiler` — see gotcha 1/2). The implied-property sort (`common.ts:540-595`) and `setPropertyOnInstance` belong in `compiler`/`fhir_model`, not `json_emit`, but they determine the insertion order `json_emit` serializes. `diagnostics` is uninvolved except `checkNullValuesOnArray` warnings (`Processing.ts:595`).

## 7. Parity test ideas

1. **Golden-file diff harness:** run stock SUSHI (v3.20.0) and the Rust port on a corpus of FSH projects; assert byte-identical `fsh-generated/resources/*.json` (including trailing newline). Use `diff`/`cmp -b`.
2. **CodeSystem field-order fixture:** a CS defined via FSH with `^version`, `^date`, `^property`, plus concepts — pin that emitted order matches the exporter assignment order (catches gotcha 1/2).
3. **Primitive sibling fixture:** an Instance with a primitive that has an extension (e.g. `status` + `status.extension`) forcing `status` + `_status`; assert `_status` immediately follows `status` and that a value-less primitive with only `_x` (orphan) lands last.
4. **Choice `[x]` fixture:** ElementDefinition with `fixedString`, `patternCodeableConcept`, `minValueInteger` — assert each concrete key appears at its `[x]` slot in PROPS order.
5. **Empty-differential & specialization fixture:** a Profile with no diffs (expect `{id,path}` stub) and a Logical model / resource (expect forced root element in differential).
6. **Slice ordering fixture:** sliced array assigned out of order, with and without `manualSliceOrdering`, to exercise `common.ts:551-589`.
7. **Unicode/number fixture:** descriptions with accented chars/emoji, decimals (`1.0`, `1.50`, `1e21`), large ints — verify no `\u` escaping, no `/` escaping, and JS-identical number rendering.
8. **`--snapshot` on/off:** assert snapshot block presence and that snapshot element order matches PROPS.
9. **InstanceDefinition `_instanceMeta` leak test:** confirm `_instanceMeta` never appears in output.

## 8. Open questions

1. **Number formatting parity:** Does the Rust port need a custom number serializer to match ECMAScript `Number.prototype.toString` exactly (e.g. `1e21`, `-0`, very small/large decimals)? `serde_json`'s default float formatting (Ryū / `f64`) may differ from V8 in edge cases. Need a decision on whether to round-trip decimals as strings to preserve source representation (does SUSHI preserve `1.0` vs `1`? — verify against `FshDecimal` handling, out of scope here).
2. **CS/VS exporter assignment order** is the de-facto canonical order but is defined by imperative code in `CodeSystemExporter.ts` / `ValueSetExporter.ts`, not a list. Should the port replicate the imperative sequence verbatim, or extract a derived PROPS-like list? Replicating verbatim is safest for parity but couples `compiler` to ordering.
3. **`Object.keys` insertion semantics for integer-like keys:** JS reorders integer-string keys numerically ahead of string keys. FHIR property names are never integer-like, but confirm no resource ever uses numeric-string keys (e.g. in `extension` value maps) that would trigger V8 key reordering that `IndexMap` would not replicate.
4. **IG resource (`ImplementationGuide-*.json`) ordering** is built in `IGExporter` (`this.ig`) and only formatted here. Is its property order in scope for this subsystem's parity, or covered by a separate IG-export spec? Currently assumed separate.
5. **`for..in` traversal in `replaceField`** (`common.ts:1167`) iterates prototype-chain-walked insertion order; confirm no enumerable inherited props exist on the mixin-augmented classes (HasId/HasName) that would be visited and reordered.

# Porting Spec 04 — Export Pipeline

Scope: `sushi-ts/src/export/` (FHIRExporter, StructureDefinitionExporter, InstanceExporter, ValueSetExporter, CodeSystemExporter, MappingExporter, Package, exportFHIR). All citations are `path:line` into the READ-ONLY submodule `sushi-ts/`.

## 1. Purpose

The export pipeline transforms the parsed FSH `FSHTank` (Profiles/Extensions/Logicals/Resources, Instances, ValueSets, CodeSystems, Mappings, Invariants) into concrete FHIR conformance artifacts (`StructureDefinition`, `InstanceDefinition`, `ValueSet`, `CodeSystem`) collected into a single `Package`. Each entity-type has a dedicated exporter that fishes up its parent/base definition, resets metadata, then applies the FSH rules in source order to mutate the FHIR JSON. Exporters are mutually recursive via fishing: exporting one entity can on-demand export a dependency (`StructureDefinitionExporter.fishForFHIR`, `InstanceExporter.fishForFHIR`). Almost every error/warning is emitted as a logged diagnostic (via `logger.error/warn/info`) rather than thrown — exceptions are caught per-entity and downgraded to a logged error, so a single bad entity never aborts the run. Order is load-bearing throughout: rule iteration order, exporter sequencing, and a deferred-rule phase all affect byte output.

## 2. TS entry points

- `exportFHIR(tank, FHIRDefs): Package` — top-level function; builds `Package`, `MasterFisher`, `FHIRExporter`, returns `exporter.export()`. `sushi-ts/src/export/exportFHIR.ts:13`.
- `FHIRExporter.export()` — fixed-order orchestration of all five exporters. `sushi-ts/src/export/FHIRExporter.ts:38`.
- `StructureDefinitionExporter` class — exports Profile/Extension/Logical/Resource. `sushi-ts/src/export/StructureDefinitionExporter.ts:103`. Key methods: `applyInsertRules` :1453, `export` :1612, `exportStructDef` :1475, `getStructureDefinition` :184, `setMetadata` :345, `setContext` :450, `preprocessStructureDefinition` :1327, `setRules` :705, `applyDeferredRules` :1068, `handleExtensionContainsRule` :1230, `fishForFHIR` :1408, `checkInvariants` :1572.
- `InstanceExporter` class. `sushi-ts/src/export/InstanceExporter.ts:53`. Methods: `applyInsertRules` :765, `export` :1012, `exportInstance` :772, `setAssignedValues` :93, `validateRequiredElements`/`validateRequiredChildElements` :609/:420, `shouldSetMetaProfile` :617, `shouldSetId` :644, `checkForNamelessSlices` :673, `fishForFHIR` :736.
- `ValueSetExporter` class. `sushi-ts/src/export/ValueSetExporter.ts:40`. Methods: `applyInsertRules` :556, `export` :563, `exportValueSet` :581, `setMetadata` :47, `setCompose` :73, `setCaretRules` :291, `setConceptCaretRules` :441, `addConceptComposeElement` :268.
- `CodeSystemExporter` class. `sushi-ts/src/export/CodeSystemExporter.ts:23`. Methods: `applyInsertRules` :339, `export` :384, `exportCodeSystem` :346, `setMetadata` :30, `setConcepts` :52, `setCaretPathRules` :108, `findConceptPath` :289, `updateCount` :316.
- `MappingExporter` class. `sushi-ts/src/export/MappingExporter.ts:11`. Methods: `applyInsertRules` :66, `export` :130, `exportMapping` :77, `setMetadata` :24, `setMappingRules` :44.
- `Package` class — output container + `Fishable`. `sushi-ts/src/export/Package.ts:10`.

## 3. Key data structures

- `Package` arrays, all `public readonly`: `profiles`, `extensions`, `logicals`, `resources` (all `StructureDefinition[]`), `instances` (`InstanceDefinition[]`), `valueSets`, `codeSystems`. `Package.ts:11-17`. Plus `fshMap: Map<filename, SourceInfo & {fshName, fshType}>` for traceability `Package.ts:19`. `config: Configuration` `Package.ts:22`.
- `Package.internalFish` — fishing dispatch over the seven types; for SD types it also falls back to `instances` whose `_instanceMeta.usage === 'Definition'` matching the right `resourceType`/`derivation`/`kind`/`type` (i.e. Instances of `StructureDefinition`/`ValueSet`/`CodeSystem` count as definitions). `Package.ts:38-188`. Version-aware matching by splitting on `|` `Package.ts:59-60`.
- `StructureDefinitionExporter.deferredCaretRules: Map<StructureDefinition, {rule: CaretValueRule; tryFish: boolean; originalErr?}[]>` — caret rules postponed until `applyDeferredRules`. `StructureDefinitionExporter.ts:104`.
- `StructureDefinitionExporter.knownBindingRules: Map<StructureDefinition, {rule: BindingRule; isInline: boolean; url?}[]>` — bindings re-checked after deferral so contained ValueSets become relative `#id` references. `StructureDefinitionExporter.ts:108`.
- `typeCharacteristicCodes` / `commaSeparatedCharacteristics` — computed once in ctor from the type-characteristics CodeSystem; `can-be-target` is always treated as supported even if absent. `StructureDefinitionExporter.ts:112-135`.
- `InstanceExporter.sdCache: Map<url, StructureDefinition>` — caches the parsed `instanceOf` SD across instances. `InstanceExporter.ts:54,822-827`.
- `InstanceDefinition._instanceMeta` — `name`, `title`, `description`, `usage` (`Example`/`Definition`/`Inline`), `sdType`, `sdKind`, `instanceOfUrl`. Set throughout `exportInstance` `InstanceExporter.ts:830-880,935`.

## 4. Algorithms & control flow

### 4.1 Top-level order (`FHIRExporter.export` `FHIRExporter.ts:38-53`)

Exact sequence — DO NOT reorder:
1. `applyInsertRules()` on **each** exporter, in order: SD, CodeSystem, ValueSet, Instance, Mapping (lines 39-43). This expands `insert` RuleSet references into concrete rules in the tank BEFORE any export begins.
2. `structureDefinitionExporter.export()` (45)
3. `codeSystemExporter.export()` (46)
4. `valueSetExporter.export()` (47)
5. `instanceExporter.export()` (48)
6. `structureDefinitionExporter.applyDeferredRules()` (49) — runs AFTER instances/VS/CS exist so deferred caret rules and inline-ValueSet bindings can resolve.
7. `mappingExporter.export()` (50) — last, because it mutates already-exported SDs.

Within each exporter, entities are processed in tank order: `getAllStructureDefinitions()` returns profiles, then extensions, then logicals, then resources (`FSHTank.ts:83-90`); others are doc-order `flatMap` over docs (`FSHTank.ts:74,96,104,128`). Because of on-demand fishing (4.6) actual export order can differ from this nominal order.

### 4.2 StructureDefinition export (`exportStructDef` `StructureDefinitionExporter.ts:1475`)

1. Dedup guard: if an SD with the same `name` already exists in any of the four pkg arrays, return early (1476-1483).
2. `getStructureDefinition` (1485 → 184): validate parent presence (`ParentNotProvidedError` 194), reject name/id==parent (`ParentDeclaredAsName/IdError` 215-232), fish parent JSON across Resource/Logical/Type/Profile/Extension (238); reject time-traveling R5 parents except `Base` for Logicals (254-265); type-check parent per child kind (Extension→Extension 267, Logical allowed kinds 274, Resource→Resource/DomainResource 289). Build `StructureDefinition.fromJSON(parent)`, set `baseDefinition = parent.url` (+canonical version) 312, set new `url` 317 and `type` 318, then `resetParentElements` 330.
3. Circular-dependency warning if parent still `inProgress` (1487-1494); set `structDef.inProgress = true` (1496).
4. `setMetadata` (1503 → 345): clears parent-inherited metadata in element order (meta/implicitRules/language/text/contained deleted 351-355, `removeMatchingExtensions(UNINHERITED_SD_EXTENSIONS)` 356), sets id/name/title/description/status; version deleted unless `FSHOnly` 385-389; `abstract=false` always 419; `derivation` = specialization for Logical/Resource else constraint 422; Extension auto-fixes `Extension.url.fixedUri` 427-432; deletes all `_`-prefixed top-level props 442-447. Extension title/description also written to root element short/definition unless `applyExtensionMetadataToRoot===false` 373-401.
5. **Push to pkg BEFORE applying rules** (1507-1515) — routing by `type==='Extension'` / `kind==='logical'&&specialization` / `kind==='resource'&&specialization` / else profiles. This early push is what lets circular references resolve against an incomplete def. Record `fshMap` 1516.
6. `preprocessStructureDefinition` (1522 → 1327): for extension paths, infer `value[x] 0..0` or `extension 0..0` cardinality; if both value and sub-extension are used, log error and apply neither inference (1356,1377). Appends inferred `CardRule`s to `fshDefinition.rules` 1403.
7. `setRules` (1524) — see 4.3.
8. Extensions: `setContext` LAST (1529 → 450) so self-referential contexts see all sub-extensions; default context is `{type:'element', expression:'Element'}` only if none inherited 539-547.
9. `cleanResource` ignoring recursive/internal props 1534; `inProgress=false` 1537; `structDef.validate()` → log each at its severity 1539; `checkForMultipleChoice` 1543; duplicate-id check across all four arrays → error 1552-1562.

### 4.3 `setRules` (`StructureDefinitionExporter.ts:705`)

1. `resolveSoftIndexing(fshDefinition.rules)` 709, then iterate over a **shallow copy** `rules = fshDefinition.rules.slice()` 712 (so obeys-injected rules don't leak into preprocessed output).
2. Pre-scan caret rules to collect `directResourcePaths` (path==='' && isInstance) and `inlineResourcePaths` (caretPath endsWith `.resourceType`) 722-738.
3. For each rule, in order:
   - `isAllowedRule` gate → error+continue for disallowed rule type 745-751.
   - `AddElementRule`: `structDef.newElement(path)` + `applyAddElementRule`; validate last id part against eld-19 (error) / eld-20 (warn) 753-787.
   - else `findElementByPath`; if no element → error "No element found at path" 1059-1063.
   - `CardRule` → `constrainCardinality` 793.
   - `AssignmentRule`: if `isInstance`, fish the Instance via a fresh `InstanceExporter` (819); Example-usage warning 813; `replaceReferences` then `assignValue`; on `MismatchedTypeError` with numeric/bool value, retry by fishing an Instance from `rawValue` 829-859.
   - `FlagRule` → `applyFlags(mustSupport, summary, modifier, trialUse, normative, draft)` 861.
   - `OnlyRule` → `constrainType` 870.
   - `BindingRule` 873: fish VS metadata; if inline ValueSet, record in `knownBindingRules` as `isInline` (deferred) 879; else resolve URL, reject CodeSystem-as-binding (`MismatchedBindingTypeError`) 887, `bindToVS`, record versionless url in `knownBindingRules` 897.
   - `ContainsRule` 907: if element is a single-typed unnamed Extension → `handleExtensionContainsRule` (1230); else require `isArrayOrChoice` (`InvalidElementForSlicingError`) and `addSlice` per item; error if `item.type` set on non-extension path 921.
   - `CaretValueRule` 936: `replaceReferences`. If `path!==''` set on element 939. If `path===''` (root caret): compute matching inline/direct resource paths; if `isInstance` defer with `tryFish:true` 967; else if it lands inside a directly-assigned instance defer with `tryFish:false` 975; else `structDef.setInstancePropertyByPath` with `inlineResourceTypes`, and on numeric `MismatchedTypeError` defer with `tryFish:true,originalErr` 994-1009.
   - `ObeysRule` 1016: fish Invariant from tank (error if missing 1019); `element.applyConstraint`; validate invariant name id (`InvalidFHIRIdError`); convert invariant's AssignmentRules into `constraint[idx].*` CaretValueRules, soft-index them, and **splice them into the live `rules` array** right after current index (`rules.splice(i+1, 0, …)`) 1032-1049 — they get processed in the same loop.
4. All per-rule exceptions are caught → `logger.error(e.message, rule.sourceInfo)` 1053.

### 4.4 `applyDeferredRules` (`StructureDefinitionExporter.ts:1068`)

Iterates `deferredCaretRules` per SD. For `tryFish` rules: fish an Instance (string value) or by rawValue (numeric); fall back to `fishForFHIR` wrapped in `InstanceDefinition.fromJSON` 1088-1092; on success `setInstancePropertyByPath` and record `successfulInstanceAssignments` with resolved resourceType (preferring `meta.profile[0]` → `instanceOfUrl` → `sdType` → `resourceType`) 1103-1119; Example warning 1097; on `MismatchedTypeError` prefer logging the saved `originalErr` 1122. For non-fish rules: rebuild `inlineResourceTypes` from `successfulInstanceAssignments`, mark SD for re-clean if any, `setInstancePropertyByPath` 1144-1168. After the loop, re-`cleanResource` every contained resource of touched SDs 1175-1179. Finally re-check `knownBindingRules`: if the bound ValueSet (inline `id` match, or real `url` match) is now contained, re-`bindToVS` to `#id` 1183-1210; inline binding with no contained VS → error 1211.

### 4.5 Instance export (`exportInstance` `InstanceExporter.ts:772`)

1. Name-dedup early return 773.
2. Fish `instanceOf` across Resource/Profile/Extension/Type/Logical → `InstanceOfNotDefinedError` if missing 778-793.
3. If parent kind is not resource/logical → force `usage='Inline'`, warn unless already Inline 795-805.
4. Walk `baseDefinition` chain to the specialization ancestor; if `abstract===true` → `AbstractInstanceOfError` 808-819.
5. Get/cache `instanceOfStructureDefinition` from `sdCache` 821-827.
6. New `InstanceDefinition`; set `_instanceMeta` name/title/description/usage 830-846; empty title/description warnings 831-837. For `usage==='Definition'`, also set `url`/`title`/`description` on the JSON when the SD has those elements 847-871. For resources set `resourceType` and (if `shouldSetId`) `id` 873-877. Set `_instanceMeta.sdType/sdKind` 879.
7. `setAssignedValues` (883 → 93) — see 4.5.1.
8. `shouldSetMetaProfile` 887 (config `setMetaProfile` gate 621-628; only profiles whose `meta` element is `Meta` 1..1) → unshift `instanceOf` url (with version) to `meta.profile` unless already present 892-932.
9. Set `_instanceMeta.instanceOfUrl` 935; `validateId` 936; `validateRequiredElements` 937; `checkForNamelessSlices` 942; `cleanResource` 943; `checkForMultipleChoice` 944.
10. Push to `pkg.instances` 945; non-Inline → `fshMap` 947. Dedupe `meta.profile` (merging complex `_profile` children) 956-979. Duplicate-(resourceType,id) check among non-Inline instances → error 985-1002.

#### 4.5.1 `setAssignedValues` ordering (`InstanceExporter.ts:93`)

The comment at 162-169 fixes the order and the port MUST match it:
1. `applyInsertRules` + `resolveSoftIndexing` (manualSliceOrdering from config 98); clone rules; strip optional `[0]` indices from paths 101-107; `replaceReferences` 108.
2. Convert `isInstance` AssignmentRules to fished InstanceDefinitions, dropping unresolved ones with error 112-136.
3. Build `inlineResourcePaths` from assigned instances and `.resourceType` rules 140-160.
4. For each rule: build `inlineResourceTypes`, `updatePathWithChoices` (warn on path change 240-246), `validateValueAtPath` → store in `ruleMap` keyed by final path; numeric/bool `MismatchedTypeError` retry via rawValue Instance 249-297. Choice-element-still-`[x]` → error "must use a specific type" 206. Track contained-resource rules for Canonical resolution 219-223.
5. Compute `paths` array (implied + rule paths) 300-305.
6. `createUsefulSlices` (manualSliceOrdering) or `determineKnownSlices` 312-323.
7. `setImpliedPropertiesOnInstance` — for core defs restricted to extension `secretPaths` only 325-350.
8. Clone, apply each `ruleMap` entry with `setPropertyOnInstance` on the clone 351-353; modifier-extension misuse checks 354-406; `instanceDef = merge(instanceDef, ruleInstance)` 408 (implied values win where rule overwrote them).

### 4.6 On-demand fishing / recursion

`StructureDefinitionExporter.fishForFHIR` (1408): if `fisher` returns null and a matching FSH def exists in the tank, it eagerly `exportStructDef` (or `exportInstance`) and re-fishes 1419-1438. `InstanceExporter.fishForFHIR` (736): checks pkg, else exports the tank Instance, else wraps fished FHIR in `InstanceDefinition.fromJSON` 746-749. This makes export order dependency-driven, not purely tank-order.

### 4.7 ValueSet / CodeSystem / Mapping

- VS `exportValueSet` (581): `setMetadata` (url `{canonical}/ValueSet/{id}`, version deleted unless FSHOnly 64-70) → partition caret rules into concept-level (`pathArray.length>0`) vs other 588 → `setCaretRules` (other) 594 → `setCompose` 595 → mark concept caret rules `isCodeCaretRule` 601 → `setConceptCaretRules` 602. Empty `compose.include` → `ValueSetComposeError` 603. `setCompose` (73) handles contained CodeSystem detection + `valueset-system` extension 118, inline-instance-not-contained error 127, self-reference removal 156-163, dedupe-merge of concepts across includes 216-261, drops empty `exclude` 262.
- CS `exportCodeSystem` (346): `setMetadata` (url `{canonical}/CodeSystem/{id}`) → `setConcepts` (hierarchical, duplicate-code error 70, missing-ancestor error 94) → `setCaretPathRules` (108; resolves concept paths via `findConceptPath` 289, only tracks extension paths for implied props 210). Duplicate-id error 365; `updateCount` only adjusts/validates `count` when `content==='complete'`, warning on mismatch 316-337.
- Mapping `exportMapping` (77): fish source SD from **pkg only** (Profile/Extension/Resource/Logical) 78; if a parent mapping shares the `identity`, require matching name/target else error+skip 94-110, else update inherited comment 111-119; otherwise `setMetadata` (push mapping, `InvalidFHIRIdError` on bad id) 122. `setMappingRules` applies `applyMapping` per element 44-64. Post-pass: duplicate mapping-id per source → error 143-151.

## 5. Edge cases & gotchas

- **Early push before rules** (`exportStructDef` 1507): the SD is in `pkg.*` while still `inProgress`. `Package.fishForMetadata` returns `undefined` for in-progress logicals lacking `can-be-target`/`can-bind` to force tank lookup `Package.ts:252-263`. A naive port that pushes after rule application breaks circular references and changes diagnostics.
- **Deferred caret rules** are a separate global phase between instance export and mapping export. Rules assigning inline instances (or values inside them) at root caret path `^...` do NOT execute during `setRules`; they queue and run in `applyDeferredRules` `FHIRExporter.ts:49`. Output order of contained resources depends on this.
- **Obeys-rule self-mutation**: invariant assignment rules are spliced into the live loop array mid-iteration `StructureDefinitionExporter.ts:1049` and processed as caret rules. Top-level (pathless) obeys maps to root element `.` 1037.
- **Numeric/bigint/boolean Instance ids**: an Instance whose id looks like `123`/`true` triggers a fallback re-fish using `rule.rawValue` in three places (SD assignment 829, SD caret 994, Instance validation 249, VS/CS caret). Error reporting deliberately keeps the *original* type error message, not the instance error 850-853,1122.
- **`merge(instanceDef, ruleInstance)`** at `InstanceExporter.ts:408` means implied properties survive even where a rule wrote over them — order of implied-vs-rule assignment is intentional and documented 306-311.
- **meta.profile**: unshifted (not pushed) so the profile URL is first 924; skipped entirely if a versioned form already present 892-899; deduped with complex `_profile` child merging via `mergeWith` 956-979.
- **Version handling**: `version` is *deleted* from VS/CS/SD unless `config.FSHOnly` (CS 43-47, VS 64-68, SD 385-389) to let the IG Publisher fill it.
- **Custom (non-conformant) resources**: warned, not blocked — SD resources outside `http://hl7.org/fhir/StructureDefinition/` 143-172; Instances of custom resources 65-90. They still appear in `pkg`.
- **`isAllowedRule` per SD type** silently skips+errors disallowed rule kinds 745.
- **ValueSet self-reference** in a component is removed with an error, not fatal 156-163.
- **CodeSystem `count`** is only auto-set/validated for `content==='complete'` 318; otherwise left alone.
- **Inline-ValueSet binding** must be re-resolved post-deferral; if not contained → hard error 1211-1215. A versioned real-URL binding is rewritten to `#id` only if the VS ends up contained 1190-1204.
- **Mapping fishes pkg, not fisher**, for its source 78 — so a Mapping can only target SDs already exported into the package (hence Mapping runs last).
- **`checkInvariants` runs once up front** in SD `export` 1613, separate from per-rule application, to avoid duplicate logging 1572.
- Exceptions thrown out of any `exportX` are caught at the `export()` loop and logged with the entity's `sourceInfo` (SD 1618, Instance 1017, VS 568, CS 389, Mapping 135) — never fatal.

## 6. Recommended Rust mapping

Belongs primarily in the **`compiler`** crate (the export pipeline is the compile step), producing values from **`fhir_model`** and emitting via **`diagnostics`**.

- `Package` → `compiler::Package` struct holding `Vec<StructureDefinition>` × 4, `Vec<InstanceDefinition>`, `Vec<ValueSet>`, `Vec<CodeSystem>` plus `fsh_map: IndexMap<String, FshMapEntry>`. Implement a `Fishable` trait mirroring `internalFish` (version-split match on id/name/url; SD-types fall through to definition-Instances). Keep insertion order (`Vec`, not `HashMap`) — output and dedup checks depend on it.
- The five exporters → structs in `compiler::export::{structdef,instance,valueset,codesystem,mapping}` each holding `&FSHTank`, `&mut Package`, and a `MasterFisher`. Because TS uses heavy mutual recursion + interior mutability (early-push, on-demand fishing), in Rust model `Package` behind a `RefCell`/arena or pass indices; prefer storing exported defs in an arena and fishing by index to avoid borrow conflicts during recursive export.
- `deferredCaretRules` / `knownBindingRules` → `Vec`-keyed-by-SD-index maps (e.g. `Vec<(SdIdx, DeferredCaret)>`) preserving insertion order.
- `sdCache` → `HashMap<String, StructureDefinition>` keyed by url.
- FHIR JSON mutation (`setInstancePropertyByPath`, `validateValueAtPath`, `findElementByPath`, `assignValue`) lives in `fhir_model` (the `fhirtypes/common` + `ElementDefinition`/`StructureDefinition` port) — the exporters call into it. Soft-index resolution (`resolveSoftIndexing`) is shared `fsh_model`/`compiler` util.
- Diagnostics: every `logger.error/warn/info` → `diagnostics::emit` carrying `SourceInfo`. Preserve exact message strings and severities for parity (§7). `logger.log(err.severity, …)` for `validate()` results means severity is data-driven.
- Final JSON serialization is **`json_emit`**'s job (not in this subsystem) — exporters stop at the in-memory `*Definition` objects (`toJSON` lives on the types).
- Neighbors: consumes `fsh_model` (Profile/Extension/Instance/FshValueSet/FshCodeSystem/Mapping/Invariant + rules), `package_store` (external FHIRDefinitions via MasterFisher), and `fsh_lexer_parser`/insert-rule expansion (`applyInsertRules`) which must run before export.

## 7. Parity test ideas

- **Pipeline ordering**: a tank where a Profile caret-assigns an Instance that is itself a Profile-Instance defined later — verify on-demand fishing produces identical output and identical order of `pkg.profiles`/`pkg.instances` vs stock SUSHI.
- **Deferred caret + contained**: Profile with `* ^contained[0] = SomeInlineObservation` and a follow-up `* ^contained[0].status = #final` — confirm contained resource content, re-clean, and ordering match byte-for-byte.
- **Obeys splice**: Invariant with extra assignment rules + a profile applying it at root and at a nested path; assert generated `constraint[n]` entries and ordering.
- **Inline ValueSet binding**: bind an element to an inline ValueSet contained in the SD; verify final binding becomes `#id` and the post-deferral error path when not contained.
- **meta.profile dedup/unshift**: Instance whose rules already set `meta.profile`; confirm unshift position and `_profile` merge.
- **Numeric Instance id fallback**: Instance with `Id: 123` assigned to a Reference/typed element; assert the original-type error message vs successful fallback assignment.
- **Version deletion**: same fixtures with `FSHOnly: true` vs false — confirm `version` present/absent on SD/VS/CS.
- **Duplicate id diagnostics**: two profiles / two value sets / two non-inline instances sharing an id — assert exact error strings and that all defs still emit.
- **CodeSystem count**: `#complete` with/without explicit `^count` mismatch → exact warning; non-complete content → no count touched.
- **Custom resource warnings**: a custom Resource + an Instance of it — assert the boxed warning text and that they remain in the package.
- **Extension inference**: extension with only `value[x]` rules → assert inferred `extension 0..0`; both value and sub-extension → assert error and no inference.

Each fixture should assert (a) the emitted JSON of every artifact and (b) the full ordered diagnostic stream (severity + message + sourceInfo).

## 8. Open questions

- Borrow model: TS relies on shared mutable `Package` + exporters re-instantiating `new InstanceExporter(...)` mid-rule (e.g. `setRules` 797, deferred 1086). Decide arena-of-indices vs `Rc<RefCell>` for the Rust `Package`/exporter graph before implementing.
- `MasterFisher` precedence (tank vs Package vs external FHIRDefinitions) is defined outside this subsystem (`utils/`) but drives every fish here — needs its own spec; confirm tie-break order matches.
- Exact behavior of `cleanResource`, `setImpliedPropertiesOnInstance`, `determineKnownSlices`/`createUsefulSlices`, `validateValueAtPath`, `replaceReferences` lives in `fhirtypes/common.ts` and `ElementDefinition`/`StructureDefinition` — out of scope here but required for byte parity; flag as dependencies.
- `manualSliceOrdering` (config `instanceOptions`) switches slice creation strategy 313-323 — confirm default (`false`) and that both branches are ported.
- Logger color/box-drawing output (chalk) for the non-conformant warnings 149-171 — decide whether parity includes ANSI formatting or only the logical message.
- Stable iteration order of `Map`s (`deferredCaretRules`, `ruleMap`, `successfulInstanceAssignments`) — JS preserves insertion order; the Rust port must use insertion-ordered maps (`IndexMap`) to match.

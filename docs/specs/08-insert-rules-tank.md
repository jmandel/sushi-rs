# 08 — Insert Rules + FSHTank

Porting spec for the global pre-export phase: insert-rule expansion (RuleSet / parameterized
RuleSet expansion, soft indexing, circular-insert detection) and FSHTank fishing (fish/fishAll,
alias resolution, version suffix, type search order). Citations are `path:line` relative to repo
root; all upstream paths are under `sushi-ts/src/`.

## 1. Purpose

`insert` rules let FSH authors splice a reusable `RuleSet` (optionally parameterized) into any
definition. This subsystem expands every `InsertRule` into the concrete rules of its referenced
RuleSet — recursively, with path prefixing, soft-index conversion (`[+]`→`[=]`), allowed-rule
filtering, and circular-dependency detection — so that downstream exporters see only concrete
rules. The `FSHTank` is the in-memory index of every parsed FSH entity (across all `.fsh` files /
documents); its `fish`/`fishAll` methods resolve a name/id/url (with optional `|version` suffix and
alias resolution) to the FSH entity, in a fixed type-search order, and `fishForMetadata` derives
url/version/parent/type metadata. This entire phase runs once, globally, before any export
(`FHIRExporter.export` calls all `applyInsertRules()` first; `FHIRExporter.ts:39-43`), so the port
must replicate it order-for-order to achieve byte/diagnostic parity.

## 2. TS entry points

- `applyInsertRules(fshDefinition, tank, seenRuleSets=[])` — core recursive expander.
  `fhirtypes/common.ts:1241-1364`.
- `FSHTank` class — the entity index implementing `Fishable`. `import/FSHTank.ts:32-726`.
  - `fish(item, ...types)` → `internalFish(item, types, true)[0]`. `FSHTank.ts:189-205`.
  - `fishAll(item, ...types)` → `internalFish(item, types, false)`. `FSHTank.ts:207-223`.
  - `internalFish(item, types, stopOnFirstMatch)` — the matcher engine. `FSHTank.ts:225-518`.
  - `fishForAppliedRuleSet(item)` — looks up parameterized (substituted) RuleSets by identifier.
    `FSHTank.ts:520-527`.
  - `fishForMetadata` / `fishForMetadatas` / `extractMetadataFromEntity`. `FSHTank.ts:529-638`.
  - `resolveAlias(name)`. `FSHTank.ts:137-143`.
  - `fishForFHIR` — always returns `undefined` for the tank. `FSHTank.ts:641-644`.
- Parse-time RuleSet handling in the importer:
  - `visitRuleSet` / `parseRuleSet` — un-parameterized RuleSets. `import/FSHImporter.ts:700-733`.
  - `visitParamRuleSet` — registers a `ParamRuleSet` (raw text + params). `FSHImporter.ts:735-759`.
  - `visitInsertRule` / `visitCodeInsertRule` — build `InsertRule`. `FSHImporter.ts:1841-1868`.
  - `applyRuleSetParams` — eager substitution + parse of parameterized RuleSets, populating
    `appliedRuleSets`. `FSHImporter.ts:1926-1990`.
  - `parseGeneratedRuleSet` — sub-parse of substituted FSH text in a temp document.
    `FSHImporter.ts:1992-2047`.
  - `applyRuleSetSubstitutions` / `MiniFSHImporter` — token-level parameter substitution.
    `import/MiniFSHImporter.ts:10-125`.
- `isAllowedRule(fshDefinition, rule)` + `allowedRulesMap`. `fshtypes/AllowedRules.ts:31-133`.
- `resolveSoftIndexing(rules, strict)` + `convertSoftIndices` / `convertSoftIndicesStrict`.
  `utils/PathUtils.ts:77-198, 204-276`.
- `Type` enum + `Metadata`/`Fishable` interfaces. `utils/Fishable.ts:3-36`.
- `MasterFisher` — composite fisher (package → tank → FHIR defs). `utils/MasterFisher.ts:21-...`.
- Per-exporter `applyInsertRules()` dispatchers: `StructureDefinitionExporter.ts:1453-1462`,
  `CodeSystemExporter.ts:339-342`, `ValueSetExporter.ts:556-559`, `InstanceExporter.ts:765-768`,
  `MappingExporter.ts:66-69`.

## 3. Key data structures

- `InsertRule` (extends `Rule`): `ruleSet: string`, `params: string[]`, `pathArray: string[]`,
  plus inherited `path: string`. `fshtypes/rules/InsertRule.ts:3-11`. `path` is the dotted path
  context the RuleSet is inserted at; `pathArray` is the code hierarchy for `* #a #b insert RS`
  (code-insert) form. `params` populated only for `RS(args)` form.
- `RuleSet` (extends `FshEntity`): `name`, `rules: Rule[]`, getter `id === name`,
  `constructorName === 'RuleSet'`. `fshtypes/RuleSet.ts`.
- `ParamRuleSet` (extends `FshEntity`): `name`, `parameters: string[]`, `contents: string` (raw
  unparsed FSH body). `getUnusedParameters()` regex-checks each declared param appears as `{param}`
  in `contents`. `fshtypes/ParamRuleSet.ts`.
- `FSHDocument`: per-file maps `profiles/extensions/logicals/resources/instances/valueSets/`
  `codeSystems/invariants/ruleSets/mappings/aliases`, and `appliedRuleSets: Map<string,RuleSet>`
  ("rulesets with substitutions applied"). `import/FSHDocument.ts:25,39`. `FSHImporter` also holds
  `paramRuleSets: Map<string,ParamRuleSet>` (document-spanning, on the importer). `FSHImporter.ts:153`.
- `FSHTank`: `docs: FSHDocument[]`, `config: Configuration`. `getAll*()` accessors `flatMap`
  over `docs` and `Array.from(map.values())`. `FSHTank.ts:32-130`.
- `Type` enum (string-valued): Profile, Extension, ValueSet, CodeSystem, Instance, Invariant,
  RuleSet, Mapping, Resource, Type, Logical. `utils/Fishable.ts:3-15`. NOTE: `Invariant`/`RuleSet`/
  `Mapping` only exist in tanks; `Type` only in FHIR defs.
- `Metadata`: `id, name?, sdType?, resourceType?, url?, parent?, imposeProfiles?, abstract?,`
  `version?, instanceUsage?, canBeTarget?, canBind?, resourcePath?`. `utils/Fishable.ts:17-30`.
- Rule fields consumed during expansion: `ConceptRule.hierarchy: string[]`
  (`fshtypes/rules/ConceptRule.ts:9`), `ConceptRule.system`, `CaretValueRule.pathArray: string[]`
  (`fshtypes/rules/CaretValueRule.ts:16`), `ValueSetConceptComponentRule.concepts: FshCode[]` and
  `.from.system` (`fshtypes/rules/ValueSetComponentRule.ts:21-22`).
- `sourceInfo` per rule: `{ file, location, appliedFile?, appliedLocation? }`. The
  `appliedFile`/`appliedLocation` fields are stamped on inserted rules to record the insert site.

## 4. Algorithms & control flow

### 4.1 Parse-time: registering RuleSets and pre-substituting params

1. Plain `RuleSet: Name` → `visitRuleSet` builds `RuleSet`, errors+skips on duplicate name in the
   current doc (`FSHImporter.ts:700-714`), then `parseRuleSet` parses child rules verbatim into
   `ruleSet.rules` (`FSHImporter.ts:715-733`). RuleSet rules are NOT path-context-resolved or
   soft-indexed at parse time — that happens during expansion / export.
2. `RuleSet: Name(p1, p2)` → `visitParamRuleSet` builds a `ParamRuleSet` storing the **raw text**
   body and the parameter list; warns on unused parameters; stores in `importer.paramRuleSets`
   (`FSHImporter.ts:735-759`).
3. When an `insert` rule has args (`* insert RS(a, b)`), `applyRuleSetParams` runs **eagerly at
   parse time** (`FSHImporter.ts:1926-1990`):
   - `ruleSetIdentifier = JSON.stringify([ruleSet.name, ...insertRule.params])`
     (`FSHImporter.ts:1945`).
   - If `paramRuleSet.parameters.length !== insertRule.params.length` → error "Incorrect number of
     parameters applied to RuleSet ..." and the InsertRule is dropped (returns `undefined`)
     (`FSHImporter.ts:1974-1980`).
   - If the param RuleSet is unknown → error "Could not find parameterized RuleSet named ..." and
     drop (`FSHImporter.ts:1981-1987`).
   - Otherwise, if `appliedRuleSets` doesn't already hold this identifier, substitute params into
     the raw text (`applyRuleSetSubstitutions`), parse the generated FSH (`parseGeneratedRuleSet`),
     fix up source info, and store the resulting `RuleSet` under the identifier in
     `currentDoc.appliedRuleSets` (`FSHImporter.ts:1948-1963`). Caching means identical
     `(name, args)` is substituted/parsed once per document.
   - The InsertRule retains `ruleSet = name` and `params = [...]`.
4. `parseGeneratedRuleSet` parses substituted text in a throwaway `FSHDocument`, suppressing the
   logger into a "secret" collector during top-level parse so errors are re-emitted as a single
   aggregated "Error(s) parsing insert rule with parameterized RuleSet <name>" message at the
   insert's sourceInfo (`FSHImporter.ts:1992-2047`). Child-rule line numbers are offset by
   `appliedRuleSet.sourceInfo.location.startLine - 1` (`FSHImporter.ts:1956-1962`).

### 4.2 Parameter substitution (MiniFSHImporter)

- `applyRuleSetSubstitutions(ruleSet, values)` re-lexes the raw body with a Mini grammar and emits
  `RuleSet: <name>\n` + transformed rules joined by `EOL` (`MiniFSHImporter.ts:31-41`).
- Per rule line, indentation is reconstructed from the star token column
  (`getStarContextStartColumn`, `MiniFSHImporter.ts:122-124`).
- Insert rules (`insert ...` or `path insert ...`) use **bracket-aware** substitution: `{param}`
  and `[[{param}]]` are replaced; values that land inside a `[[ ... ]]` bracket zone get `]],` and
  `]])` escaped (`MiniFSHImporter.ts:59-101`). Non-insert rules use plain `{param}` replacement
  (`doRegularSubstitution`, `MiniFSHImporter.ts:103-120`). Unmatched `{param}` → replaced with
  empty string.

### 4.3 Expansion: `applyInsertRules` (the heart)

Runs over `fshDefinition.rules`, building a fresh `expandedRules` array, then reassigns
`fshDefinition.rules = expandedRules` at the end (`fhirtypes/common.ts:1256-1363`).

For each `rule`:
1. Non-`InsertRule` → pushed through unchanged (`common.ts:1258-1261`).
2. `InsertRule`:
   - Compute `ruleSetIdentifier = JSON.stringify([rule.ruleSet, ...rule.params])`
     (`common.ts:1263`).
   - If `rule.params.length` → `ruleSet = tank.fishForAppliedRuleSet(ruleSetIdentifier)`
     (looks in each doc's `appliedRuleSets`, `FSHTank.ts:520-527`). Else
     `ruleSet = tank.fish(rule.ruleSet, Type.RuleSet)` (`common.ts:1265-1269`).
   - If not found → error "Unable to find definition for RuleSet <name>." and skip
     (`common.ts:1359-1361`).
   - **Circular detection**: if `seenRuleSets.includes(ruleSetIdentifier)` → error "Inserting
     <name> will cause a circular dependency, so the rule will be ignored" and skip
     (`common.ts:1272-1278`). The check uses the identifier string, so a recursion guard keyed on
     `JSON.stringify([name, ...params])`.
   - **Recurse first**: `applyInsertRules(ruleSet, tank, [...seenRuleSets, ruleSetIdentifier])` —
     a RuleSet may itself contain insert rules, so it is expanded in place before its rules are
     consumed (`common.ts:1279-1281`). NOTE: this **mutates the shared RuleSet object** (its
     `.rules` is reassigned); see §5.
   - Set `context = rule.path`; `firstRule = true`.
   - For each `ruleSetRule` of the expanded RuleSet, **in order**:
     a. Stamp `ruleSetRule.sourceInfo.appliedFile = rule.sourceInfo.file` and
        `.appliedLocation = rule.sourceInfo.location` (`common.ts:1285-1286`).
     b. **ConceptRule-with-system disambiguation** (`common.ts:1293-1309`): if `ruleSetRule` is a
        `ConceptRule` with a `.system`:
        - target is `FshValueSet` → convert into a `ValueSetConceptComponentRule` with
          `concepts = [new FshCode(code, system, display)]` and `from.system = system`.
        - target is `FshCodeSystem` → error "Do not include the system when listing concepts for a
          code system." (rule kept as-is).
     c. **Allowed-rule check** `isAllowedRule(fshDefinition, ruleSetRule)` (`common.ts:1310`); if
        not allowed → error "Rule of type <X> cannot be applied to entity of type <Y>" and skip
        this rule (`common.ts:1352-1356`).
     d. Deep-clone the rule (`cloneDeep`, `common.ts:1311`) — the original RuleSet rule is never
        mutated by path prefixing.
     e. **Path prefixing** (`common.ts:1312-1324`): if `context` (the insert's path) is non-empty:
        - if cloned rule path is the special `'.'` → error "The special '.' path is only allowed in
          top-level rules..." and keep path as `'.'`.
        - else if cloned rule has a path → `newPath = context + '.' + clonePath`.
        - else → `newPath = context`. Assign `ruleSetRuleClone.path = newPath`.
     f. **Code-hierarchy / caret-path prefixing** (`common.ts:1325-1332`): if
        `rule.pathArray.length > 0` (the `* #a #b insert` form): for a `ConceptRule`, unshift
        `rule.pathArray.map(code => code.slice(1))` (strip leading `#`) into `clone.hierarchy`; for
        a `CaretValueRule`, unshift `rule.pathArray` into `clone.pathArray`.
     g. **ConceptRule-with-context on CodeSystem** (`common.ts:1333-1344`): if clone is a
        `ConceptRule`, target is `FshCodeSystem`, and `context` is set → error "Do not insert a
        RuleSet at a path when the RuleSet adds a concept." (concept still added).
     h. Push clone into `expandedRules` (`common.ts:1345`).
     i. **Soft-index context handoff** (`common.ts:1346-1351`): after the **first** applied rule,
        replace all `[+]` in `context` with `[=]` (`context.replace(/\[\+\]/g, '[=]')`) and set
        `firstRule = false`. So only the first inserted rule advances a `[+]` index at the insert
        path; subsequent rules reuse the same index via `[=]`.

### 4.4 Soft indexing: `resolveSoftIndexing` (export time, after expansion)

Each exporter calls `applyInsertRules()` (global, §4.6) then later, per-definition, calls
`resolveSoftIndexing(rules[, strict])` on the now-concrete rule list (e.g.
`InstanceExporter.ts:101-102` does insert-expand then soft-resolve; `StructureDefinitionExporter.ts:709`;
`MappingExporter.ts:45`; `CodeSystemExporter.ts:133`; `ValueSetExporter.ts:296,446`).

`resolveSoftIndexing` (`PathUtils.ts:204-276`):
1. Parse each rule's `path` (and `caretPath` for `CaretValueRule`) into `PathPart[]`
   (`parseFSHPath`).
2. For each element, set `element.prefix` = assembled path of all prior elements, then call
   `convertSoftIndices(element, pathMap)` (non-strict) or `convertSoftIndicesStrict` (strict).
3. Reassemble `rule.path` (and `caretPath`) from the mutated parts.
4. Caret paths are resolved in a **per-`rule.path`** sub-map (`caretPathMap` keyed by the
   already-resolved `rule.path`), so caret indices restart for each distinct element path
   (`PathUtils.ts:243-254`).

`convertSoftIndices` (`PathUtils.ts:77-112`): map key
`mapName = "${prefix}.${base}|${slices.join('|')}"`. First time seen: if an explicit numeric
bracket exists, seed map to that number; else seed to 0, convert a `+` bracket → `0`, and a leading
`=` → error "The first index in a Soft Indexing sequence must be \"+\", an actual index of \"0\"
has been assumed" (still assumes 0). Subsequent: `+` → stored+1 (and stores it); `=` → stored;
numeric → resets stored to that number. `convertSoftIndicesStrict` (`PathUtils.ts:123-198`)
additionally tracks `maxPathMap` and propagates increments to less-sliced parent element keys —
used only with `manualSliceOrdering` for instances.

### 4.5 Parse-time path-context soft indexing (`prependPathContext`)

Orthogonal but related: indentation-based path context built during import
(`FSHImporter.ts:2284-2362`). Indented rules inherit a path context from the enclosing rule; the
`finally` block converts `[+]`→`[=]` across all stored contexts once a non-path rule (or any rule
on an Instance) consumes the context (`FSHImporter.ts:2356-2360`). A reset to column 0 while the
last context still holds an unused `[+]` warns and discards it (`FSHImporter.ts:2301-2320`). The
ported parser must reproduce this so InsertRule `.path`/`.pathArray` are already context-prefixed
before expansion.

### 4.6 Global ordering (must be byte-exact)

`FHIRExporter.export` (`FHIRExporter.ts:38-53`):
1. `structureDefinitionExporter.applyInsertRules()` — **invariants first**, then
   `getAllStructureDefinitions()` = profiles ++ extensions ++ logicals ++ resources
   (`StructureDefinitionExporter.ts:1453-1462`; tank order `FSHTank.ts:83-90`).
2. `codeSystemExporter.applyInsertRules()` (`CodeSystemExporter.ts:339-342`).
3. `valueSetExporter.applyInsertRules()` (`ValueSetExporter.ts:556-559`).
4. `instanceExporter.applyInsertRules()` (`InstanceExporter.ts:765-768`).
5. `mappingExporter.applyInsertRules()` (`MappingExporter.ts:66-69`).
6. Then exports run (SD, CS, VS, Instance, SD.applyDeferredRules, Mapping).

Entity iteration order within each `getAll*` is document order (`docs` array) then insertion order
of the per-doc `Map` (`flatMap`+`Array.from(map.values())`, `FSHTank.ts:42-130`). Diagnostics order
depends on this.

### 4.7 Fishing: `internalFish`

`internalFish(item, types, stopOnFirstMatch)` (`FSHTank.ts:225-518`):
1. `item = resolveAlias(item) ?? item` — alias resolution scans docs in order, first hit wins
   (`FSHTank.ts:255`, `137-143`).
2. Split version suffix: `[base, ...versionParts] = item.split('|')`;
   `version = versionParts.join('|') || null` (`FSHTank.ts:258-259`). So a base name may itself
   never contain `|`; everything after the first `|` is the version (rejoined with `|`).
3. Empty `types` → search ALL in this fixed order: Profile, Extension, Logical, Resource, ValueSet,
   CodeSystem, Instance, Invariant, RuleSet, Mapping (`FSHTank.ts:262-275`).
4. For each requested type **in the order given**, build an `entityMatcher` and (for SD/VS/CS
   types) an `instanceMatcher`:
   - Profile/Extension/Logical/Resource match on `name === base || id === base ||
     getUrlFromFshDefinition(...) === base`, AND `version == null || version ===
     getVersionFromFshDefinition(...)` (`FSHTank.ts:283-424`).
   - ValueSet/CodeSystem additionally match `getNonInstanceValueFromRules(x,'name',...) === base`
     (caret-assigned name) (`FSHTank.ts:425-472`).
   - Instance matches `name === base || id === base` + version (`FSHTank.ts:473-481`).
   - Invariant/RuleSet/Mapping match **name only** — no id, url, or version
     (`FSHTank.ts:482-493`).
   - `instanceMatcher` lets an `Instance` masquerade as a Profile/Extension/Logical/Resource/
     ValueSet/CodeSystem when `instanceOf` + `usage='Definition'` (or `'Inline'` for VS/CS) and the
     derivation/type/kind rules match (e.g. profile requires `derivation = #constraint` and NOT
     `type = Extension`; extension requires both) (`FSHTank.ts:293-471`).
5. If `stopOnFirstMatch`: within each type, try `entitiesToSearch.find(entityMatcher)` first; if
   none and an `instanceMatcher` exists, try `allInstances.find(instanceMatcher)`; return the first
   single hit (`FSHTank.ts:499-509`). Otherwise accumulate `filter` results across all types
   (entities before matching instances per type) (`FSHTank.ts:510-515`).

### 4.8 `fishForMetadata` / `extractMetadataFromEntity`

`fishForMetadata` = `fish` then `extractMetadataFromEntity` (`FSHTank.ts:529-532`).
`extractMetadataFromEntity` **calls `applyInsertRules(entity, this)` on the found entity first**
(`FSHTank.ts:553`) so caret-assigned url/version/name are visible, then derives `id`/`name` and,
per type: url (`getUrlFromFshDefinition`), version (`getVersionFromFshDefinition`), parent,
`resourceType`, imposeProfiles (from `structuredefinition-imposeProfile` extension rules),
Logical `sdType`/`canBeTarget`/`canBind` (via `findExtensionValues` /
`hasLogicalCharacteristic`, `FSHTank.ts:646-725`), Instance `instanceUsage`/url/parent/sdType
(`FSHTank.ts:539-638`).

## 5. Edge cases & gotchas

- **Two-phase parameterized RuleSets.** Parameter substitution happens at PARSE time
  (`applyRuleSetParams`, `FSHImporter.ts:1926`), producing entries in `FSHDocument.appliedRuleSets`
  keyed by `JSON.stringify([name, ...params])`. Expansion at export time only *looks up* that
  identifier via `fishForAppliedRuleSet` (`common.ts:1266`). A naive port that tries to substitute
  during expansion will mis-order diagnostics and lose the parse-time caching/aggregated-error
  behavior. Param count mismatch and unknown-param-RuleSet errors fire at parse time and **drop the
  InsertRule entirely** (`FSHImporter.ts:1974-1987`).
- **`fishForAppliedRuleSet` vs `fish(Type.RuleSet)` are different stores.** Parameterized inserts
  never consult the plain RuleSet index, and vice versa (`common.ts:1265-1269`). The identifier for
  a zero-arg insert is `JSON.stringify([name])` but that path uses `tank.fish`, not the applied map.
- **Circular detection keyed by identifier string, not RuleSet object.** `RS(1)` and `RS(2)` are
  distinct identifiers, so mutual/self recursion is only caught when the exact `[name, ...params]`
  repeats (`common.ts:1272`). Error wording is exact: "Inserting <ruleSet.name> will cause a
  circular dependency, so the rule will be ignored".
- **Shared-RuleSet mutation / idempotency.** `applyInsertRules(ruleSet, ...)` reassigns the
  RuleSet's own `.rules` in place (`common.ts:1363`). Because a RuleSet object is shared across all
  insert sites, the recursive expansion mutates it once; subsequent inserts of the same RuleSet see
  already-expanded rules (no InsertRules remain), so re-expansion is a no-op. Path prefixing uses a
  `cloneDeep` of each rule (`common.ts:1311`) so the source RuleSet rules keep their original paths.
  The port must mirror "expand the RuleSet in place once, clone per insertion when prefixing."
- **First-rule `[+]`→`[=]` collapse at the insert path.** Only the first applied rule keeps the
  insert path's `[+]`; all later rules in the same insert get `[=]` (`common.ts:1346-1351`). This is
  distinct from `resolveSoftIndexing`'s own per-element bookkeeping and from the parse-time
  `prependPathContext` collapse — there are THREE separate `[+]`→`[=]` mechanisms.
- **ConceptRule duality.** A bare `* SYS#code` inside a RuleSet is imported as a `ConceptRule` that
  retains `.system` (`FSHImporter.ts:1549-1556`). At expansion: inserted into a ValueSet it becomes
  a `ValueSetConceptComponentRule`; inserted into a CodeSystem with a system it errors
  (`common.ts:1293-1308`). A port that fixes the rule type at parse time will diverge.
- **`'.'` path special-casing twice.** Both `prependPathContext` (`FSHImporter.ts:2323-2332`) and
  `applyInsertRules` (`common.ts:1314-1319`) emit "The special '.' path is only allowed in
  top-level rules..." — once at parse for indented `.`, once at expansion when a RuleSet rule with
  `.` is inserted at a context.
- **Soft-index leading `=` throws but assumes 0.** Both strict and non-strict raise an Error that
  `resolveSoftIndexing` catches and logs at the rule's sourceInfo, then continues with index 0
  (`PathUtils.ts:90-95, 145-151, 234-239`). Don't abort.
- **Fishing match precedence.** Within a type, entity matches beat instance matches; and
  `stopOnFirstMatch` returns the first match in the **type order argument**, so `fish(x)` with no
  types prefers Profile over CodeSystem etc. (`FSHTank.ts:262-516`). `fishAll` returns entities
  before matching instances within each type, all types concatenated.
- **Version split is greedy on first `|`.** `version = versionParts.join('|')` re-joins, so a
  version containing `|` is preserved but the base can never contain one (`FSHTank.ts:258-259`).
  `version || null` means an empty version (`name|`) is treated as no version filter.
- **Invariant/RuleSet/Mapping fish by name only** — no id/url/version matching at all
  (`FSHTank.ts:482-493`). RuleSet `.id` getter returns `.name`, so id-based lookups still resolve.
- **`fishForMetadata` triggers expansion as a side effect** (`FSHTank.ts:553`). Calling it during
  another definition's processing can expand a not-yet-processed entity's insert rules early. The
  expansion is idempotent (per above) so this is safe but observable in diagnostic ordering.
- **Aggregated sub-parse diagnostics.** Errors/warnings from parsing a substituted param RuleSet
  are collected and re-emitted as a single multi-line message at the insert site, pluralized
  ("Error"/"Errors") and joined by OS `EOL` (`FSHImporter.ts:2018-2043`). Byte parity requires the
  same aggregation and the same `EOL`.
- **`fishForFHIR` on the tank is intentionally a no-op** (`FSHTank.ts:641-644`); the MasterFisher
  uses tank presence only to *suppress* falling through to external FHIR defs
  (`MasterFisher.ts:47-55`).

## 6. Recommended Rust mapping

- **Crate: `compiler`** owns the orchestration: a global `apply_insert_rules` pass over all
  entities in `FHIRExporter.export` order (§4.6), plus the per-exporter dispatch. The
  `resolveSoftIndexing` calls stay co-located with each exporter (also `compiler`).
- **Crate: `fsh_model`** owns the data: `InsertRule { rule_set: String, params: Vec<String>,
  path_array: Vec<String>, path: String }`, `RuleSet { name, rules: Vec<Rule> }`,
  `ParamRuleSet { name, parameters: Vec<String>, contents: String }`, the `Rule` enum, and
  `is_allowed_rule(def_kind, &rule)` as a static table keyed by an entity-kind enum (mirror
  `allowedRulesMap`, `AllowedRules.ts:31-108`). `FSHTank` and `FSHDocument` live here as the index
  type; `appliedRuleSets: HashMap<String, RuleSet>` per document, `param_rule_sets:
  HashMap<String, ParamRuleSet>` on the importer.
- **Crate: `fsh_lexer_parser`** owns parse-time substitution: `apply_ruleset_substitutions`
  (MiniFSH), `applyRuleSetParams`, `parse_generated_rule_set`, `prependPathContext`. The Mini
  substitution is string/token surgery — reproduce the exact regexes and bracket-zone escaping
  (`MiniFSHImporter.ts:59-120`).
- **Identifier keys**: replicate `JSON.stringify([name, ...params])` exactly (JSON array of
  strings, with JSON string escaping) since it is both the `appliedRuleSets` key and the
  circular-detection token (`common.ts:1263`, `FSHImporter.ts:1945`). Use `serde_json::to_string`
  on a `Vec<String>` to match byte-for-byte (comma+no-space separators, `"` escaping).
- **Recursion guard**: pass `seen_rule_sets: Vec<String>` (Vec, not Set — TS uses `Array.includes`)
  threaded by value (`common.ts:1254,1281`).
- **In-place expansion**: since RuleSets are shared and mutated once, store them in the tank behind
  interior mutability (`RefCell`/`&mut`) or expand into a cache keyed by RuleSet identity. Cloning
  per-insertion (for path prefixing) maps to Rust `.clone()` on the `Rule` (`common.ts:1311`).
- **Indexes**: `FSHTank` should keep insertion-ordered maps (`IndexMap`) per doc so `getAll*`
  iteration order matches `Array.from(map.values())`. `fish` returns by reference; model the
  `Type`-ordered search as an iterator over an explicit type list.
- **Diagnostics → `diagnostics` crate**: every `logger.error/warn` above must map to a diagnostic
  with the rule's `sourceInfo` (file + start/end line/col). Preserve exact message text and the
  parse-time vs export-time emission phase for ordering parity.
- **Neighbors**: this phase sits between `fsh_lexer_parser` (produces the tank) and the exporters
  (`json_emit` consumes concrete rules). `MasterFisher` (compiler) wraps the tank +
  `package_store`/`fhir_model` defs for the export phase, but the insert/tank phase itself uses only
  the `FSHTank`.

## 7. Parity test ideas

1. **Plain RuleSet insert** at a path: `* category insert RS` where RS has card + caret rules;
   assert prefixed paths and stamped `appliedLocation`. Compare emitted SD JSON byte-for-byte.
2. **Parameterized RuleSet**: `RuleSet: RS(x, y)` with `{x}`/`{y}` in plain and `[[{x}]]` bracketed
   positions; insert with values containing commas/parens/`]]` to exercise escaping
   (`MiniFSHImporter.ts:73-101`). Wrong arg count → exact "Incorrect number of parameters" error and
   dropped rule.
3. **Soft-index handoff**: RuleSet with ≥2 rules inserted at `component[+]`; assert first rule keeps
   `[+]`→resolved index N, rest collapse to `[=]`→N (`common.ts:1346-1351`). Cross-check final
   numeric indices after `resolveSoftIndexing`.
4. **Circular insert**: RS_A inserts RS_B inserts RS_A → exact "...will cause a circular dependency,
   so the rule will be ignored" once; outer rules still emit. Also `RS(1)` vs `RS(2)` NOT flagged
   circular.
5. **ConceptRule duality**: same RuleSet `* SYS#code "d"` inserted into a ValueSet (becomes VS
   component) vs a CodeSystem with system (error "Do not include the system...") vs CodeSystem at a
   path (error "Do not insert a RuleSet at a path when the RuleSet adds a concept.").
6. **Code-hierarchy insert**: `* #parent insert RS` where RS adds concepts; assert hierarchy unshift
   (`common.ts:1326-1328`).
7. **Allowed-rule rejection**: insert a RuleSet containing a `ContainsRule` into a Logical (no
   ContainsRule allowed, `AllowedRules.ts:82-93`) → "Rule of type ContainsRule cannot be applied to
   entity of type Logical".
8. **Fishing**: alias → name → id → url → caret-name resolution; `Name|1.2.3` version suffix
   including a version with embedded `|`; Instance-as-Profile matcher (`derivation=#constraint`,
   not `type=Extension`); `fish` type-order precedence (define a Profile and CodeSystem with same
   name, fish untyped → Profile wins).
9. **Aggregated sub-parse error**: parameterized RuleSet whose substituted body is syntactically
   invalid → single "Error parsing insert rule with parameterized RuleSet <name>" at insert site.
10. **Global ordering**: a project mixing invariants, profiles, codesystems, instances with errors
    in each; assert diagnostic order matches invariants→SDs→CS→VS→Instance→Mapping
    (`FHIRExporter.ts:39-43`).

## 8. Open questions

- Exact `JSON.stringify` byte compatibility for params containing control chars / non-ASCII /
  surrogate pairs — does `serde_json` match V8's escaping for the identifier key (`common.ts:1263`)?
  Needs a fuzz fixture; affects both `appliedRuleSets` keying and circular detection.
- `prependPathContext` indentation math relies on `DEFAULT_START_COLUMN`/`INDENT_WIDTH` and the raw
  ANTLR token columns (`FSHImporter.ts:2293-2294`). Confirm the ported lexer reports identical
  columns (tabs, multi-byte) so context indices line up.
- `getStarContextStartColumn` derives indent from a star token that may contain embedded newlines
  (`MiniFSHImporter.ts:122-124`) — verify the ported Mini lexer tokenizes leading whitespace into
  the star token identically.
- `manualSliceOrdering` (strict soft indexing) is only used for instances
  (`InstanceExporter.ts:102`); confirm its config plumbing and whether the strict less-sliced
  parent propagation (`PathUtils.ts:178-197`) is in scope for first parity milestone.
- The `extractMetadataFromEntity` side-effecting `applyInsertRules` (`FSHTank.ts:553`) means fishing
  order can change expansion order; decide whether the port pre-expands everything once up front
  (safe given idempotency) or replicates lazy expansion exactly for diagnostic-order parity.

# Porting Spec 02 — `fshtypes` subsystem

> Compatibility port of upstream TS SUSHI `sushi-ts/src/fshtypes/` (+ `import/FSHTank.ts`, `import/FSHDocument.ts`) at v3.20.0. Every behavioral claim cites `path:line` in `sushi-ts/`.

## 1. Purpose

`fshtypes` is the in-memory object model (AST) for FSH entities and rules. After the importer parses `.fsh` text, every declaration becomes one of ten entity classes (`Profile`, `Extension`, `Logical`, `Resource`, `Instance`, `FshValueSet`, `FshCodeSystem`, `Invariant`, `RuleSet`, `Mapping`), each holding an ordered list of typed `Rule` objects. The subsystem also defines value wrapper types (`FshCode`, `FshQuantity`, `FshRatio`, `FshReference`, `FshCanonical`), a per-file container (`FSHDocument`), and the cross-file index/lookup engine `FSHTank` (which implements the `Fishable` interface used everywhere downstream to resolve names → entities/metadata). Entities are largely passive data, but carry `toFSH()` round-trip serialization and derived getters (notably `id`) computed from their own rules. Order of rules is load-bearing throughout: derived values use "last matching rule wins" semantics.

## 2. TS entry points

- Abstract base + source tracking: `sushi-ts/src/fshtypes/FshEntity.ts:4` (`FshEntity`), `:46` (`SourceInfo`), `:53` (`TextLocation`).
- Structure-def base (Profile/Extension/Logical/Resource): `sushi-ts/src/fshtypes/FshStructure.ts:6`.
- Entity classes: `Profile.ts:5`, `Extension.ts:7`, `Logical.ts:5`, `Resource.ts:5`, `Instance.ts:7`, `FshValueSet.ts:10`, `FshCodeSystem.ts:10`, `Invariant.ts:14`, `RuleSet.ts:7`, `ParamRuleSet.ts:4`, `Mapping.ts:9`.
- Rule base + subtypes: `rules/Rule.ts:3`, and `rules/*.ts` (catalogued in §3).
- Allowed-rule table + check: `fshtypes/AllowedRules.ts:30` (map), `:118` (`isAllowedRule`).
- Shared helpers: `fshtypes/common.ts:14` (`typeString`), `:46` (`fshifyString`), `:55` (`findAssignmentByPath`), `:95` (`getValueFromRules`), `:121` (`getNonInstanceValueFromRules`).
- Per-file container: `sushi-ts/src/import/FSHDocument.ts:14`.
- Cross-file index / `Fishable`: `sushi-ts/src/import/FSHTank.ts:32`; `fish` `:189`, `fishAll` `:207`, `internalFish` `:225`, `fishForMetadata`/`extractMetadataFromEntity` `:529`/`:539`, `findExtensionValues` `:674`, `resolveAlias` `:137`, `checkDuplicateNameEntities` `:145`.
- `Fishable`/`Type`/`Metadata`: `sushi-ts/src/utils/Fishable.ts:3`/`:17`/`:33`.
- URL/version derivation used by fish: `sushi-ts/src/fhirtypes/common.ts:1410` (`getUrlFromFshDefinition`), `:1441` (`getVersionFromFshDefinition`).

## 3. Key data structures

### 3.1 `FshEntity` (base) — `FshEntity.ts:4`
- `sourceInfo: SourceInfo` (readonly field, init `{}`). Fields: `file?`, `location?: TextLocation`, `appliedFile?`, `appliedLocation?` (`:46`). `TextLocation = {startLine, startColumn, endLine, endColumn}` (`:53`).
- Builder methods `withLocation` / `withFile` / `withAppliedLocation` / `withAppliedFile` return `this`; `withLocation` accepts either a `TextLocation` or a 4-tuple `[startLine,startColumn,endLine,endColumn]` (`:7`,`:26`).
- Every class below has a `get constructorName()` returning a string literal — this string (NOT JS `instanceof`) is the discriminant used by `AllowedRules` (`AllowedRules.ts:132`).

### 3.2 `FshStructure` (abstract) — `FshStructure.ts:6`
Fields: private `_id`, `parent?`, `title?`, `description?`, `rules: Rule[]`, public `name` (ctor param). Constructor sets `id = name` then `rules = []` (`:13`). `get id()` is derived: returns `getNonInstanceValueFromRules(this,'id','','id')` if a string, else `_id` (`:23`). `metadataToFSH()` emits `<constructorName>: <name>`, optional `Parent:`, always `Id:`, optional `Title:`/`Description:` (`:35`).

Subclasses:
- `Profile` — `rules: SdRule[]`; `constructorName='Profile'` (`Profile.ts`).
- `Extension` — `rules: SdRule[]`; extra `contexts: ExtensionContext[]`; ctor sets `parent='Extension'` (`Extension.ts:15`). `ExtensionContext = {value, isQuoted, sourceInfo?}` (`Extension.ts:44`). `metadataToFSH` appends `Context:` line (`:23`).
- `Logical` — `rules: LrRule[]`; extra `characteristics: string[]`; ctor sets `parent='Base'` (`Logical.ts:13`). `metadataToFSH` appends `Characteristics: #a, #b` (`:21`).
- `Resource` — `rules: LrRule[]`; ctor sets `parent='DomainResource'` (`Resource.ts:12`).

### 3.3 Non-structure entities
- `Instance` (extends `FshEntity`) — `Instance.ts:7`. Private `_id`, `title?`, `instanceOf: string`, `description?`, `usage?: InstanceUsage`, `rules: (AssignmentRule|InsertRule|PathRule)[]`. Ctor: `id=name`, `rules=[]`, `usage='Example'` (`:15`). Same derived `get id()` as FshStructure (`:26`). `InstanceUsage = 'Example'|'Definition'|'Inline'`, guard `isInstanceUsage` (`InstanceUsage.ts:1`).
- `FshValueSet` (extends `FshEntity`) — `FshValueSet.ts:10`. `_id`, `title?`, `description?`, `rules: (ValueSetComponentRule|CaretValueRule|InsertRule)[]`. Derived `id` (`:26`).
- `FshCodeSystem` — `FshCodeSystem.ts:10`. `_id`, `title?`, `description?`, `rules: (ConceptRule|CaretValueRule|InsertRule)[]`. Derived `id`.
- `Invariant` — `Invariant.ts:14`. `description?`, `expression?`, `xpath?`, `severity?: FshCode`, `rules: (AssignmentRule|InsertRule)[]`. `get id()` returns `name` (read-only, `:34`). Maps to ElementDefinition.constraint: description→human, name→key (`:8`). `metadataToFSH` emits Invariant header + `* severity =`/`* expression =`/`* xpath =` lines (`:38`).
- `RuleSet` — `RuleSet.ts:7`. `rules: Rule[]`; `get id()` returns `name` (`:23`). No `toFSH`.
- `ParamRuleSet` — `ParamRuleSet.ts:4`. `parameters: string[]`, `contents: string` (raw unexpanded text). `getUnusedParameters()` returns params whose `{ param }` (whitespace-tolerant, regex-escaped) does NOT appear in `contents` (`:14`).
- `Mapping` — `Mapping.ts:9`. `id: string` (plain field, init `name` — NOT derived), `source?`, `target?`, `description?`, `title?`, `rules: (MappingRule|InsertRule)[]`.

### 3.4 Value wrapper types
- `FshCode(code, system?, display?)` — `FshCode.ts:5`. `toString()` quotes code if it starts with `"` or contains whitespace (`:16`). `toFHIRCoding()` splits `system` on first `|` into `system`+`version` (`:29`). `toFHIRCodeableConcept`, `toFHIRQuantity` (code→code, system→system, display→unit) (`:43`,`:49`).
- `FshQuantity(value: number, unit?: FshCode)` — `FshQuantity.ts:6`. `toString()`: if `unit.system=='http://unitsofmeasure.org'` print UCUM as `'code'` + optional display, else `unit.toString()` (`:14`). `toFHIRQuantity`, `equals` compare value+code+system+display (`:49`).
- `FshRatio(numerator, denominator)` — `FshRatio.ts:5`. `toString()` = `"<num> : <den>"` (`:13`). `toFHIRRatio`, `equals`.
- `FshReference(reference, display?)` plus mutable `sdType: string` — `FshReference.ts:4`. `toString()`=`Reference(<ref>)["disp"]`. `equals(other, ignoreOtherDisplay=false)` (`:27`).
- `FshCanonical(entityName)` plus `version: string` — `FshCanonical.ts:3`. `toString()`=`Canonical(<entityName>[|<version>])`.
- `ValueSetComponentFrom = {system?, valueSets?: string[]}`; `VsOperator` enum (`=`,`is-a`,`descendent-of`,`is-not-a`,`regex`,`in`,`not-in`,`generalizes`,`exists`); `ValueSetFilter = {property, operator, value}`; `ValueSetFilterValue = string|RegExp|boolean|FshCode` (`ValueSetComponentTypes.ts:3`).

### 3.5 Rule hierarchy (exhaustive)
Base `Rule(path: string)` extends FshEntity — `rules/Rule.ts:3`. Union aliases: `SdRule = CardRule|CaretValueRule|ContainsRule|AssignmentRule|FlagRule|ObeysRule|OnlyRule|BindingRule|InsertRule` (`SdRule.ts:13`); `LrRule = AddElementRule|SdRule` (`LrRule.ts:3`).

| Rule | constructorName | Fields (beyond `path`) | cite |
|---|---|---|---|
| `PathRule` | `PathRule` | (none) | `PathRule.ts:3` |
| `CardRule` | `CardRule` | `min: number`, `max: string` | `CardRule.ts:3` |
| `FlagCarryingRule` (abstract) | — | `mustSupport?`,`summary?`,`modifier?`,`trialUse?`,`normative?`,`draft?` (all `bool`); `get flags()` | `FlagCarryingRule.ts:6` |
| `FlagRule` | `FlagRule` | (flags via base) | `FlagRule.ts:3` |
| `BindingRule` | `BindingRule` | `valueSet: string`, `strength: string` | `BindingRule.ts:3` |
| `AssignmentRule` | `AssignmentRule` | `value: AssignmentValueType`, `rawValue?: string`, `exactly: boolean`, `isInstance: boolean` | `AssignmentRule.ts:22` |
| `OnlyRule` | `OnlyRule` | `types: OnlyRuleType[]` | `OnlyRule.ts:4` |
| `ContainsRule` | `ContainsRule` | `items: ContainsRuleItem[]` (`{name, type?}`) | `ContainsRule.ts:4`,`:38` |
| `CaretValueRule` | `CaretValueRule` | `caretPath: string`, `value`, `rawValue?`, `isInstance: bool`, `isCodeCaretRule=false`, `pathArray: string[]=[]` | `CaretValueRule.ts:10` |
| `ObeysRule` | `ObeysRule` | `invariant: string` | `ObeysRule.ts:3` |
| `InsertRule` | `InsertRule` | `ruleSet: string`, `params: string[]`, `pathArray: string[]=[]` | `InsertRule.ts:3` |
| `MappingRule` | `MappingRule` | `map: string`, `language?: FshCode`, `comment?: string` | `MappingRule.ts:5` |
| `ConceptRule` | `ConceptRule` | ctor `(code, display?, definition?)`; `system: string` (unused, conflict-resolution), `hierarchy: string[]`; path forced to `''` | `ConceptRule.ts:4` |
| `AddElementRule` | `AddElementRule` | extends FlagCarryingRule; `min:number`, `max:string`, `types: OnlyRuleType[]`, `contentReference?`, `short: string`, `definition?` | `AddElementRule.ts:5` |
| `ValueSetComponentRule` | `ValueSetComponentRule` | ctor `(inclusion: boolean)`; `from: ValueSetComponentFrom={}`; path `''` | `ValueSetComponentRule.ts:6` |
| `ValueSetConceptComponentRule` | `ValueSetConceptComponentRule` | + `concepts: FshCode[]` | `ValueSetComponentRule.ts:21` |
| `ValueSetFilterComponentRule` | `ValueSetFilterComponentRule` | + `filters: ValueSetFilter[]` | `ValueSetComponentRule.ts:60` |

`OnlyRuleType = {type: string, isReference?, isCanonical?, isCodeableReference?}` (`OnlyRule.ts:20`). `AssignmentValueType = boolean|number|bigint|string|FshCanonical|FshCode|FshQuantity|FshRatio|FshReference|InstanceDefinition` (`AssignmentRule.ts:10`).

### 3.6 `FSHDocument` (per-file) — `FSHDocument.ts:14`
`file` (readonly) + 12 `Map<string, T>`: `aliases` (string→string), `profiles`, `extensions`, `resources`, `logicals`, `instances`, `valueSets`, `codeSystems`, `invariants`, `ruleSets` (no params), `appliedRuleSets` (param-substituted), `mappings`. All keyed by entity `name`. Insertion-ordered Maps matter (see §4).

### 3.7 `FSHTank` — `FSHTank.ts:32`
`docs: FSHDocument[]`, `config: Configuration`. Implements `Fishable`. `Type` enum is the search dimension (`Fishable.ts:3`); `Metadata` is the lightweight lookup result (`Fishable.ts:17`).

### 3.8 `Configuration` — `Configuration.ts:30`
Large config record. Port-relevant fields used by `fshtypes`/`FSHTank`: `canonical: string` (required), `version?: string`, `fhirVersion: string[]`. Nested `ConfigurationInstanceOptions` (`:199`): `setMetaProfile`, `setId`, `manualSliceOrdering` (consumed downstream, not here). Most other fields are IG-export concerns.

## 4. Algorithms & control flow (ORDER-SENSITIVE)

### 4.1 Derived `id` (FshStructure/Instance/FshValueSet/FshCodeSystem)
`get id()` → `getNonInstanceValueFromRules(this,'id','','id')` (`FshStructure.ts:24`). That chains `getValueFromRules` → `findAssignmentByPath` (`common.ts:55`). Logic: if entity is `Instance` or `Invariant`, scan `rules` with lodash `findLast` for an `AssignmentRule` whose `path==='id'`; otherwise `findLast` for a `CaretValueRule` whose `path===''` AND `caretPath==='id'` (`common.ts:70`). `findLast` ⇒ **last matching rule wins**. `getNonInstanceValueFromRules` returns the value only if found AND `isInstance===false` (`common.ts:142`); otherwise the getter falls back to `_id` (init = `name`). So: `* ^id = "x"` (CaretValue) overrides the declared Id for SDs/VS/CS; `* id = "x"` (Assignment) for Instances. Mapping.id is a plain field, NOT derived (`Mapping.ts:11`). Invariant.id is read-only `name` (`Invariant.ts:34`). RuleSet.id is read-only `name` (`RuleSet.ts:23`).

### 4.2 URL / version derivation (`fhirtypes/common.ts`)
`getUrlFromFshDefinition` (`:1410`): `getValueFromRules(def,'url','','url')` (last CaretValue `^url`); if string and not instance, use it; else build `<canonical>/<fhirType>/<id>` where fhirType = `ValueSet`/`CodeSystem`/`StructureDefinition` by class (`:1419`). `getVersionFromFshDefinition` (`:1441`): last CaretValue `^version` string-not-instance, else config `version`. Both call the same `getValueFromRules`/`findLast` engine, so they are last-rule-wins and depend on rule ordering AND on `applyInsertRules` having already expanded inserts.

### 4.3 `FSHTank.internalFish` — name resolution (`FSHTank.ts:225`)
Order of operations (must replicate exactly):
1. `item = resolveAlias(item) ?? item` — alias lookup walks `docs` in order, first hit wins (`:137`,`:255`).
2. Split `item` on `|`: `base` + `version` = rest joined by `|`; empty version → `null` (`:258`).
3. If no `types` passed, default to all 10 tank types in this fixed order: Profile, Extension, Logical, Resource, ValueSet, CodeSystem, Instance, Invariant, RuleSet, Mapping (`:262`).
4. For each requested type in the given order: build `entityMatcher` and optional `instanceMatcher`, gather `entitiesToSearch` via `getAll*` (which `flatMap`s docs in order — `:42` etc.).
5. If `stopOnFirstMatch` (`fish`): `entitiesToSearch.find(entityMatcher)`; if hit return `[entity]` immediately; else if `instanceMatcher`, `allInstances.find(instanceMatcher)` (`:499`). **Entities are matched before instances within a type, and types are tried in order.**
6. Else (`fishAll`): push all entity matches then all instance matches, continue all types (`:510`).

Matcher details:
- Profile/Extension/Logical/Resource entityMatcher: `name===base || id===base || getUrlFromFshDefinition(...)===base`, AND version null or equals `getVersionFromFshDefinition` (`:285`...).
- Each of these ALSO has an `instanceMatcher` that lets an `Instance` of `StructureDefinition` with `usage==='Definition'` masquerade as the type, gated by AssignmentRules: derivation `#constraint` for Profile/Extension, `#specialization` for Logical/Resource; plus `type='Extension'` present (Extension) / absent (Profile) (`:293`,`:328`); `kind` `#logical`/`#resource` for Logical/Resource (`:363`,`:399`).
- ValueSet/CodeSystem entityMatcher additionally matches `getNonInstanceValueFromRules(vs,'name','','name')===base` (i.e. a `^name` caret rule), and instanceMatcher accepts `usage` `Definition` OR `Inline` (`:425`,`:449`).
- Instance entityMatcher: `name===base || id===base` + version (`:475`).
- Invariant/RuleSet/Mapping: `name===base` only — no id/url/version (`:482`).
- `Type.Type` and default: `continue` (tank holds no FHIR types) (`:494`).

### 4.4 `fishForMetadata` / `extractMetadataFromEntity` (`FSHTank.ts:529`)
1. `fish(...)` to get entity.
2. **Side effect:** `applyInsertRules(entity, this)` is called, mutating the entity by expanding InsertRules before reading metadata (`:553`). A naive port that treats fish as pure will diverge.
3. Build `Metadata` with `id`, `name`. For SDs add `url`, `version`, `parent`, `resourceType='StructureDefinition'`, `imposeProfiles` (via `findExtensionValues`), and for Logical also `sdType` (URL minus `http://hl7.org/fhir/StructureDefinition/` prefix if HL7), `canBeTarget`, `canBind` (`:558`). For VS/CS: url/version/resourceType. For Instance: `instanceUsage`, `url` from `^?`url assignment, version, and resourceType + parent/sdType derived from `baseDefinition`/`kind` assignments (`:596`).

### 4.5 `findExtensionValues` (`FSHTank.ts:674`)
Scans entity rules for CaretValueRules with `path===''` and a `caretPath`. Two regex forms: numeric `^extension(\[\d+\])?\.(url|value[A-Z]...)$` (default indexer `[0]`), and named `^extension(\[name\]\[idx\]?)\.value...$` where name is alias-resolved and matched against `[url,id,name]`. Collects indexers whose `.url` value equals the target extension URL, maps indexers→values, returns values in indexer-collection order, filtering nulls (`:722`). Used for `imposeProfiles`, `SDTypeCharacteristics`, `LogicalTarget`.

### 4.6 `checkDuplicateNameEntities` (`FSHTank.ts:145`)
Builds `allEntities` (note: includes `getAllExtensions()` again even though extensions are also in StructureDefinitions — a duplicate pass), and for each entity checks if any doc has a different object stored under the same `name` across nine map types. Emits a single `logger.warn` listing duplicate names (`:178`). RuleSet maps checked but NOT instances-vs-others cross detection beyond the listed maps.

### 4.7 `isAllowedRule` (`AllowedRules.ts:118`)
`allowedRulesMap.get(fshDefinition.constructorName)?.some(r => rule instanceof r)`. Keyed by `constructorName` string. Notable: Logical & Resource allow `AddElementRule` but explicitly **NOT** `ContainsRule` (`:86`,`:100`). RuleSet allows nearly everything. `InsertRule` is in NO list — it must be expanded, never exported (`:110` doc).

### 4.8 `toFSH()` serialization (round-trip)
Each entity: `metadataToFSH()` + EOL-joined `rules.map(r=>r.toFSH())`, with a leading EOL only if rules non-empty (e.g. `Profile.ts:16`). `EOL` is `os.EOL` (platform line ending). `fshifyString` escapes `\ " \n \r \t` in that order (`common.ts:46`). Multiline descriptions/expressions use `"""..."""` when they contain `\n` (raw, un-fshified) else `"..."` fshified (`FshStructure.ts:46`). `typeString` groups OnlyRuleTypes into normal / `Reference(...)` / `Canonical(...)` / `CodeableReference(...)`, joined by ` or ` (`common.ts:14`).

## 5. Edge cases & gotchas

1. **`constructorName` string, not `instanceof`, is the type discriminant** for `isAllowedRule` (`AllowedRules.ts:132`) and for `metadataToFSH` headers (`FshStructure.ts:37`). Port must carry an explicit tag, because subclassing (`FshStructure` get id reused) means JS class identity wouldn't distinguish Profile from a hypothetical subclass. Rust: an enum tag.
2. **`id` is a computed getter, recomputed on every access**, scanning rules with last-wins `findLast` (`FshStructure.ts:23`, `common.ts:71`). It changes as rules are added/expanded. Do NOT snapshot id at construction. Mapping.id and Invariant/RuleSet.id behave differently (plain field / `name`).
3. **`getNonInstanceValueFromRules` excludes instance-valued rules** (`common.ts:142`): an `^id = SomeInstance` does not override id. Easy to miss.
4. **Instances can masquerade as Profile/Extension/Logical/Resource/VS/CS in fish** via `usage:Definition`/`Inline` + specific AssignmentRule gates (`FSHTank.ts:293`,`:436`). Forgetting these makes name resolution miss "definitional instances."
5. **`fishForMetadata` mutates the entity** (`applyInsertRules`) before reading (`FSHTank.ts:553`). Lookup is not side-effect-free; insert expansion timing affects derived id/url/version.
6. **Default fish type order and entity-before-instance, type-by-type short-circuit** (`FSHTank.ts:262`,`:499`) determine which entity wins on name collisions. Must be byte-identical.
7. **Extension/Logical/Resource pre-seed `parent`** to `'Extension'`/`'Base'`/`'DomainResource'` in their constructors (`Extension.ts:15`,`Logical.ts:13`,`Resource.ts:12`). Instance pre-seeds `usage='Example'` (`Instance.ts:19`). These defaults are observable.
8. **`FshCode.toFHIRCoding` splits system on first `|` into system+version** but `toFHIRQuantity` does NOT (`FshCode.ts:29` vs `:49`). Asymmetric.
9. **`FshQuantity.toString` special-cases UCUM** (`http://unitsofmeasure.org`) to single-quote the code; other systems print full `system#code` (`FshQuantity.ts:20`).
10. **`FlagCarryingRule.flags` order is fixed**: MS, ?!, SU, then exactly one of D / TU / N (draft beats trialUse beats normative; mutually exclusive via else-if) (`FlagCarryingRule.ts:14`).
11. **`ContainsRule.toFSH` always appends ` 0..`** placeholder cardinality and multi-item layout differs (newline+indent for >1 item) (`ContainsRule.ts:26`,`:32`).
12. **`ValueSetConceptComponentRule.toFSH` mutates `concept.system`** from `from.system` when emitting (`ValueSetComponentRule.ts:49`) and has a 100-char line-wrap heuristic (`:39`,`:72`). RegExp is a valid filter value type (`ValueSetComponentTypes.ts:26`) — needs a Rust regex-or-string variant.
13. **`ConceptRule` forces `path=''` and stores codes in `hierarchy[]`** with the actual code appended last for serialization (`ConceptRule.ts:25`). `system` field exists only to disambiguate from ValueSetConceptComponentRule on RuleSets (`:5`).
14. **`InsertRule.fshifyParameters` has bespoke escaping** (`[[...]]` wrapping when param has surrounding whitespace or `,`/`)`, backslash doubling) (`InsertRule.ts:17`) — distinct from `fshifyString`.
15. **`CaretValueRule` "code caret" mode** (`isCodeCaretRule`, `pathArray`) emits `system#code` path prefixes instead of a normal element path (`CaretValueRule.ts:48`). Same dual mode in `InsertRule.pathArray` (`InsertRule.ts:33`).
16. **`getUnusedParameters` regex is whitespace-tolerant** `{\s*param\s*}` and uses lodash `escapeRegExp` on the param (`ParamRuleSet.ts:16`).
17. **`checkDuplicateNameEntities` double-counts extensions** in `allEntities` (`FSHTank.ts:147` & `:154`) — harmless for set dedupe but the iteration is as written.
18. **`AssignmentRule.toFSH` uses `rawValue` for numeric/bigint** to preserve original lexeme (e.g. trailing zeros) (`AssignmentRule.ts:43`); strings print bare when `isInstance` else quoted (`:45`).
19. **`getAll*` flat-map docs in `docs` array order, then Map insertion order** (`FSHTank.ts:42`). Determinism depends on preserving both orderings — use ordered maps in Rust.

## 6. Recommended Rust mapping

**Crate: `fsh_model`** owns all of `fshtypes/` (entities, rules, value wrappers, AllowedRules, common helpers) plus `FSHDocument` and `FSHTank` (the importer crate `fsh_lexer_parser` produces these; the lookup engine belongs with the model). `Configuration` may live in `fsh_model` or a small `config` module shared with `compiler`.

Suggested types/indexes:
- `enum FshRule` with one variant per rule subtype (Card, Flag, Binding, Assignment, Only, Contains, CaretValue, Obeys, Insert, Mapping, Concept, AddElement, Path, ValueSetConceptComponent, ValueSetFilterComponent). Carry a `SourceInfo` on each. Provide `fn constructor_name(&self) -> &'static str` and `fn to_fsh(&self) -> String`.
- `enum FshEntity { Profile, Extension, Logical, Resource, Instance, ValueSet, CodeSystem, Invariant, RuleSet, Mapping }` (or per-struct types + an enum wrapper). Each struct embeds `Vec<FshRule>` (ordered), `SourceInfo`, and the typed fields from §3.
- Derived `id`: implement as a method `fn id(&self) -> String` that runs the `find_last` scan — do NOT precompute. Mirror `getNonInstanceValueFromRules` exactly (last-wins, skip instance values).
- `AssignmentValueType`: Rust enum `Bool|Int(i64)|BigInt|Float/Decimal|Str|Canonical|Code|Quantity|Ratio|Reference|Instance(InstanceDefinition)`. Keep `rawValue: Option<String>` to preserve numeric lexemes. (Note bigint vs number — preserve via rawValue and a decimal type to avoid f64 drift; coordinate with `json_emit`/`fhir_model`.)
- `FSHDocument`: use insertion-ordered maps (`indexmap::IndexMap<String, T>`) for all 12 maps to preserve iteration order (§5.19).
- `FSHTank`: `Vec<FSHDocument>` + `Configuration`; `getAll*` = chained iterators over docs in order. Implement a `Fishable` trait mirroring `fishForFHIR/fishForMetadata/fishForMetadatas` and the `Type` enum + `Metadata` struct.
- `VsOperator`, `InstanceUsage` → Rust enums with explicit string mappings.

Connections: `fsh_lexer_parser` constructs these (consumes `withLocation`/`withFile`). `compiler` (StructureDefinition/Instance/ValueSet/CodeSystem exporters) consumes the model and `FSHTank.fish*`. `applyInsertRules` lives in `fhirtypes/common` upstream → in Rust belongs to `compiler` (rule expansion) but is invoked from `fishForMetadata`; keep the dependency edge `fsh_model → compiler` minimal (consider a trait/callback to avoid a cycle, since fish triggers insert expansion). `diagnostics` receives the `checkDuplicateNameEntities` warning and `FSHLogger` messages. `getUrlFromFshDefinition`/`getVersionFromFshDefinition` live in `fhirtypes/common` upstream (`fhir_model` crate) but operate on `fsh_model` types — port as free functions taking `&FshEntity` + canonical/version.

## 7. Parity test ideas

1. **Derived id, last-wins**: Profile with two `* ^id = "..."` caret rules → `entity.id()` returns the second. Instance with two `* id = "..."` assignment rules → second wins. Add an `^id = SomeInstance` (isInstance) and confirm it is ignored (falls back to name).
2. **fish type precedence**: define a Profile and an Instance(InstanceOf StructureDefinition, Usage Definition, derivation #constraint) with the same `name`; `fish(name)` must return the Profile (entity-before-instance, Profile type first). Then a name shared by Logical vs Resource definitional instances to exercise kind gates.
3. **Versioned fish**: `fish("Name|1.2.3")` vs entity with `^version = "1.2.3"` caret rule and config version mismatch.
4. **Alias resolution chains**: alias in doc 2 referenced from doc 1; confirm resolution and that `fishForAppliedRuleSet` ignores aliases.
5. **`toFSH` round-trip golden files** for every entity + every rule subtype, including: multiline description (`"""`), UCUM quantity, Reference/Canonical/CodeableReference OnlyRule grouping, ContainsRule multi-item layout, ValueSetConceptComponent 100-char wrap, code-caret CaretValueRule, InsertRule with whitespace/comma params (`[[...]]`), flag ordering (set all six flags, expect `MS ?! SU D`).
6. **`fishForMetadata` insert side-effect**: entity whose id/url depends on a rule contributed by an InsertRule; metadata must reflect post-expansion id.
7. **`findExtensionValues`**: Logical with `^extension[url].valueCode = #can-be-target` (named + numeric indexer forms, alias-resolved) → `canBeTarget==true`; also via `characteristics` list and LogicalTarget boolean extension.
8. **Duplicate-name warning**: two entities (e.g. Profile + ValueSet) same name across docs → exactly one warn listing the name; same-type same-name objects.
9. **`getUnusedParameters`**: ParamRuleSet with `{ a }` used, `b` unused → returns `["b"]`; whitespace inside braces still counts as used.
10. **`fshifyString` vs `fshifyParameters`** escaping divergence on backslash/quote/comma inputs.

## 8. Open questions

1. **Number/bigint fidelity**: TS uses JS `number` + `bigint` + `rawValue` to round-trip numeric assignments without precision loss (`AssignmentRule.ts:43`). Rust needs a decision: store `rawValue` always and a parsed decimal (`rust_decimal`/`bigdecimal`) vs `i64`/`f64`. Affects JSON emission parity downstream.
2. **`RegExp` as `ValueSetFilterValue`** (`ValueSetComponentTypes.ts:26`) — need a Rust representation (store original pattern string + flags) and confirm how/where it is constructed/serialized (importer dependent).
3. **Cycle risk**: `fishForMetadata` → `applyInsertRules` (rule expansion, currently in `fhirtypes/common`) → may re-enter fish. Decide crate boundary: does `fsh_model` depend on `compiler`, or is insert-expansion injected via a trait? Resolve before laying out crates.
4. **`InstanceDefinition` in `AssignmentValueType`** (`AssignmentRule.ts:6`) couples `fsh_model` rules to `fhir_model`'s `InstanceDefinition`. Confirm whether the AST holds a fully-built InstanceDefinition or a lighter handle at parse time.
5. **`os.EOL` platform line endings** in `toFSH` (every entity). For deterministic byte-parity tests, decide whether to hardcode `\n` or replicate platform behavior. Stock SUSHI output varies by OS.
6. **Does the port need `toFSH`/`metadataToFSH` at all for v1?** These are used by FSH-export/round-trip tooling, not core compile. Confirm scope; if needed, they are a large surface to match byte-for-byte.

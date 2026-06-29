# AST Shape Reference (fsh_model target)

Authoritative field shapes for the import AST, from `sushi-ts/src/fshtypes/**`
(class declarations) cross-checked against the `parse-oracle.cjs` JSON keys. This
is the target for `fsh_model` and the Rust AST dumper (which must serialize to the
same JSON shape the oracle emits, so `parse-oracle` goldens diff cleanly).

`__type` = JS constructor name (the oracle tag). Every entity & rule also carries
`sourceInfo { file, location{startLine,startColumn,endLine,endColumn}, appliedFile?,
appliedLocation? }` (1-based, inclusive). Maps in `FSHDocument` are insertion-ordered.

## Entities
- **FshStructure** (base of Profile/Extension/Logical/Resource): `name`, `id`
  (getter→`_id` in oracle), `parent?`, `title?`, `description?`, `rules`.
- **Profile**: rules = SdRule[]. **Extension**: + `contexts: ExtensionContext[]`
  (oracle: none in IPS). **Logical**: + `characteristics: string[]`, rules = LrRule[].
  **Resource**: rules = LrRule[].
- **Instance**: `name`, `_id`, `instanceOf`, `title?`, `description?`,
  `usage?: 'Example'|'Definition'|'Inline'`, rules = (Assignment|Insert|Path)[].
- **FshValueSet**: `name`, `_id`, `title?`, `description?`,
  rules = (ValueSetComponentRule|CaretValue|Insert)[].
- **FshCodeSystem**: `name`, `_id`, `title?`, `description?`,
  rules = (Concept|CaretValue|Insert)[].
- **Invariant**: `name`, `description?`, `expression?`, `xpath?`, `severity?: FshCode`,
  rules = (Assignment|Insert)[]. (id = name)
- **RuleSet**: `name`, `rules: Rule[]`. (parameterized → ParamRuleSet, expanded at parse time)
- **Mapping**: `name`, `id`, `source?`, `target?`, `title?`, `description?`,
  rules = (MappingRule|Insert)[].

## Rules (base `Rule { path }`)
- **CardRule**: `min: number`, `max: string`.
- **FlagRule** (FlagCarryingRule): `mustSupport?`, `summary?`, `modifier?`,
  `trialUse?`, `normative?`, `draft?` (all bool, only-set-when-true).
- **BindingRule**: `valueSet`, `strength`.
- **AssignmentRule**: `value: AssignmentValueType`, `rawValue?`, `exactly: bool`,
  `isInstance: bool`.
- **OnlyRule**: `types: OnlyRuleType[]`.
- **ContainsRule**: `items: ContainsRuleItem[]`.
- **CaretValueRule**: `caretPath`, `value`, `rawValue?`, `isInstance`,
  `pathArray: string[]`, `isCodeCaretRule` (oracle shows it).
- **ObeysRule**: `invariant`.
- **InsertRule**: `ruleSet`, `params: string[]`, `pathArray: string[]`.
- **PathRule**: just `path` (side-effect only; sets context, returns []).
- **AddElementRule** (FlagCarrying): `min: number`, `max: string`,
  `types: OnlyRuleType[]`, `contentReference?`, `short`, `definition?`.
- **ConceptRule**: `code`, `system`, `display?`, `definition?`, `hierarchy: string[]`.
- **MappingRule**: `map`, `language?: FshCode`, `comment?`.
- **ValueSetConceptComponentRule**: `inclusion: bool`, `from {system?,valueSets?}`,
  `concepts: FshCode[]`.
- **ValueSetFilterComponentRule**: `inclusion: bool`, `from`, `filters[]`.

## Value types (AssignmentValueType = boolean | number | bigint | string |
FshCanonical | FshCode | FshQuantity | FshRatio | FshReference | InstanceDefinition)
- **FshCode**: `code`, `system?`, `display?`.
- **FshQuantity**: `value: number`, `unit?: FshCode`.
- **FshRatio**: `numerator: FshQuantity`, `denominator: FshQuantity`.
- **FshReference**: `reference`, `display?`.
- **FshCanonical**: `entityName`, `version?`.
- numbers: assignment/caret integers → bigint (oracle `{__bigint}`), else number;
  quantities use float. Keep `rawValue` for NUMBER/bool.

## Subtypes
- **OnlyRuleType**: `{ type, isReference?, isCanonical?, isCodeableReference? }`.
- **ContainsRuleItem**: `{ name, type? }`.
- **ExtensionContext**: `{ value, isQuoted }` (see fshtypes/ExtensionContext.ts).

## Rust mapping note
Represent rules as an enum `Rule` with a struct per variant; entity as enum or
structs keyed in `IndexMap`. Serialize via a dedicated dumper matching the oracle
(Map→`{__map}`, bigint→`{__bigint}`, `__type` tags, `_id` for the id getter).
Validate with `parse-oracle.cjs` goldens under `tests/goldens/ast/`.

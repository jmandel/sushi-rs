# Pipeline Stage 2: R4 StructureDefinition JSON → R5-internal-model JSON

> **Ground-truth spec for `fn r4_sd_to_r5(json) -> json`.** This is the pure
> conversion the IG Publisher performs *before* snapshot generation
> (`VersionConvertorFactory_40_50.convertResource` for a `StructureDefinition`,
> then serialize with the R5 `JsonParser`). It is **context-free**: no package
> context, no base SD, no `generateSnapshot`. R5 inputs pass through unchanged
> (stage-2 no-op).
>
> **Oracle pin:** fhir-core commit `5c4d5a0ff`, jar
> `org.hl7.fhir.r5-6.9.10-SNAPSHOT`. Every claim cites `file:line` at that
> commit. Paths are relative to `/home/jmandel/hobby/fhir-perf/repos/fhir-core`.
> `CONV/` = `org.hl7.fhir.convertors/src/main/java/org/hl7/fhir/convertors/`,
> `R4/` = `org.hl7.fhir.r4/src/main/java/org/hl7/fhir/r4/`,
> `R5/` = `org.hl7.fhir.r5/src/main/java/org/hl7/fhir/r5/`.
>
> **Oracle driver for this stage:** `SnapOracleR4 --dump-converted`
> (`snapshot/oracle/SnapOracleR4.java`), wrapped by
> `bash snapshot/oracle/gen-snapshot.sh --r4 --dump-converted ...`. Goldens live
> in `snapshot/converted-goldens/<ig>/<name>.converted.json`.

## Table of contents

1. [Entry point, dispatch, and the default advisor](#1-entry-point-dispatch-and-the-default-advisor)
2. [The universal copy mechanics (Element / BackboneElement / DomainResource)](#2-the-universal-copy-mechanics)
3. [Primitive-value rules (the empty-string drop, decimals, sidecars)](#3-primitive-value-rules)
4. [StructureDefinition top-level field map](#4-structuredefinition-top-level-field-map)
5. [ElementDefinition field map](#5-elementdefinition-field-map)
6. [ElementDefinition sub-structures](#6-elementdefinition-sub-structures)
7. [Field↔extension transforms (the exhaustive list)](#7-fieldextension-transforms)
8. [string→markdown promotions](#8-stringmarkdown-promotions)
9. [Enum value sets](#9-enum-value-sets)
10. [R5 JSON serialization: key order + byte format](#10-r5-json-serialization-key-order--byte-format)
11. [Surprises vs the AGENTS.md Java Parity Notes](#11-surprises-vs-agentsmd)
12. [Open questions for the Rust implementer](#12-open-questions-for-the-rust-implementer)

---

## 1. Entry point, dispatch, and the default advisor

`SnapOracleR4` calls `VersionConvertorFactory_40_50.convertResource(derivedR4)`
with **no advisor** (`SnapOracleR4.java:163`).

- The no-advisor overload (`CONV/factory/VersionConvertorFactory_40_50.java:9-11`)
  delegates to `convertResource(src, new BaseAdvisor_40_50())`. The default
  advisor is a plain `BaseAdvisor_40_50` with `failFast=true`
  (`CONV/.../BaseAdvisor.java:5`) and `produceIllegalParameters=false`.
- Conversion runs `new VersionConvertor_40_50(advisor).convertResource(src)`
  (`VersionConvertorFactory_40_50.java:15`), initializing `ConversionContext40_50`
  with `path = "StructureDefinition"` (`VersionConvertor_40_50.java:74-80`), then
  dispatches by `instanceof` to
  `StructureDefinition40_50.convertStructureDefinition`
  (`CONV/conv40_50/Resource40_50.java:250-251`).

**Default-advisor behavior (what a Rust port must replicate):**

- `useAdvisorForExtension(...)` → `false` (`CONV/.../BaseAdvisor50.java:67-70`),
  so **every** extension is converted structurally (url + value, nested
  extensions preserved) and **nothing is handed to an advisor**.
- `BaseAdvisor_40_50.ignoreExtension(path,url)` returns true **only** for
  `path` ending in `TestScript` AND url
  `http://hl7.org/fhir/5.0/StructureDefinition/extension-TestScript.scope`
  (`BaseAdvisor_40_50.java:30-42`). For a StructureDefinition (`path` never ends
  in `TestScript`) this **never fires**.
- `ignoreType(...)` → `false`.

**Net: for a StructureDefinition, no extension is dropped or specially handled
by the advisor. A Rust port copies all `extension`/`modifierExtension` entries
verbatim (recursing into each extension's own value/sub-extensions), except the
specific URLs promoted to fields listed in §7.**

---

## 2. The universal copy mechanics

Every converter routine begins by copying the "envelope" (id + extensions), then
sets the typed fields in source order. Reproducing this order is what makes the
serialized `extension[]` arrays byte-stable.

### `Element40_50.copyElement(src, tgt, ...ignoreUrls)` — `CONV/conv40_50/datatypes40_50/Element40_50.java:21-37`
1. `if (src.hasId()) tgt.setId(src.getId())` (line 25) — id copied verbatim.
2. Iterate `src.getExtension()` **in order**; skip any URL in `ignoreUrls`; for
   each survivor call `Extension40_50.convertExtension(e)` and `tgt.addExtension`
   (lines 26-36). Order preserved.

### `BackboneElement40_50.copyBackboneElement(src, tgt, ...ignoreUrls)` — `CONV/conv40_50/datatypes40_50/BackboneElement40_50.java:10-17`
Calls `copyElement` (with the same ignore list), then copies `modifierExtension[]`
(same skip rule).

### `Resource40_50.copyDomainResource(src, tgt)` — `CONV/conv40_50/Resource40_50.java:542-561`
In this order: `copyResource` (`id`, `meta`, `implicitRules`, `language`;
lines 24-29) → `text` → `contained[]` (each recursively `convertResource`) →
`extension[]` (each converted, order preserved) → `modifierExtension[]`.

**Ordering consequence (critical):** in the produced R5 object, `id` and the
`extension[]` array (built from the source's pre-existing, non-ignored
extensions) come **first**. Any extension the converter *inserts afterward*
(e.g. the constraint-xpath extension, §7) is **appended** — it lands *after* the
verbatim-copied extensions in the array. Verified in the golden
`us-core-observation-lab.converted.json`: constraint `us-core-4` has
`extension[0]` = the pre-existing `elementdefinition-bestpractice`,
`extension[1]` = the inserted xpath extension.

---

## 3. Primitive-value rules

### 3.1 The empty-string / blank-primitive drop (CRITICAL)

**Rule:** an R4 primitive whose value is blank (`""` or whitespace-only) is
**dropped** — the field is absent in R5 output.

- `PrimitiveType.hasValue()` = `!StringUtils.isBlank(getValueAsString())`
  (`R4/model/PrimitiveType.java:125-127`, identical in
  `R5/model/PrimitiveType.java:127-129`). `isBlank("")` is true → `hasValue()`
  is **false** for an empty string.
- Every primitive converter is guarded by `src.hasValue()`. E.g.
  `String40_50.convertString` (`CONV/conv40_50/datatypes40_50/primitive40_50/String40_50.java:7-11`):
  `StringType tgt = src.hasValue() ? new StringType(src.getValueAsString()) : new StringType();`
  For `""` the target is created with **no value**, then `copyElement` still
  copies id/extensions. Same `hasValue()?…:new X()` pattern in every primitive
  converter (Boolean40_50:8, Code40_50:8, Canonical40_50:8, Uri40_50:8,
  MarkDown40_50:8, Decimal40_50:8, …).
- Serialization would also suppress it: `composeStringCore` only emits the value
  when `value.hasValue()` (`R5/formats/JsonParser.java:36427-36433`).

**Stage precision:** the value is *retained* at R4 parse time (R4 `JsonParser`
stores `new StringType("")` — `R4/formats/JsonParser.java:1281-1282, 123-126`);
it is **dropped at the converter** and would be dropped again at compose. The
AGENTS.md phrasing "R4→R5 parse drops empty-string primitives" is imprecise: it
is dropped at *conversion/serialization*, not at parse. For a Rust port
operating on `serde_json::Value`, the net rule is: **treat any string primitive
whose trimmed value is empty as absent** (drop the field; but keep a `_field`
sidecar object if the primitive still has an `id`/`extension` — see 3.3).

Verified: `mcode-cancer-stage` `Observation.value[x].comment:""` → converted
output has no `comment` key (the *only* diff between input and output).
`ndh-HealthcareService` `HealthcareService.category.slicing.description:""` →
dropped, rest of slicing kept.

> **Blank ≠ empty.** `isBlank` also treats `" "` / `"\t"` as no-value. A single
> space is dropped just like `""`. Match `StringUtils.isBlank` semantics
> (Java: null-or-length-0-or-all-`Character.isWhitespace`).

### 3.2 Decimal / integer precision

- `DecimalType.setValueAsString(s)` stores `new BigDecimal(s)` (preserving scale
  / trailing zeros) plus a forced lexical representation
  (`R5/model/DecimalType.java:169-177, 200-203`; R4 identical).
  `Decimal40_50.convertDecimal` passes `src.getValueAsString()` verbatim
  (`CONV/.../primitive40_50/Decimal40_50.java:6-11`) — value-preserving through
  conversion.
- **Serialization caveat:** `composeDecimalCore` emits `value.getValue()` (the
  `BigDecimal`), not the representation string
  (`R5/formats/JsonParser.java:36715-36721`), via
  `JsonCreatorDirect.value(BigDecimal)` → `BigDecimal.toString()`
  (`R5/formats/JsonCreatorDirect.java:180-187`). Because the `BigDecimal` was
  built from the original literal, scale (trailing zeros) survives, but
  **`BigDecimal.toString()` can emit scientific notation** (e.g. `1E+2`) for
  some magnitudes. A byte-exact Rust port must match `BigDecimal.toString()`,
  not the raw input lexeme. PRETTY explicitly uses `JsonCreatorDirect`
  "because this preserves decimal formatting" (`R5/formats/JsonParserBase.java:210`).
  (StructureDefinitions rarely carry decimals; relevant only for
  `minValue[x]`/`maxValue[x]`/`example.value[x]` of decimal type.)
- Integers: copied value-preserving; emitted by `JsonCreatorDirect` as the
  integer.

### 3.3 value + extension sidecar (`_field`)

Serialization splits every primitive into two independent helpers, always called
Core-then-Extras (`R5/formats/JsonParser.java:36427-36443`, callers e.g. 68199):
- **Core** (`"name": value`) emitted only if `value.hasValue()`.
- **Extras** (`"_name": { id?, extension[]? }`) emitted only if the primitive
  has an id, extensions, or fhir_comments — independent of whether a value
  exists.

So a primitive with an extension but no value emits **only** `"_name": {...}`.
For primitive arrays: the value array (`"name"`) is emitted first with `null`
placeholders for value-less entries, then a parallel `"_name"` array holds the
sidecars (e.g. `contextInvariant`/`_contextInvariant`
`R5/formats/JsonParser.java:68293-68306`; `alias`/`_alias` 37525-37538). Value
field/array always precedes its `_field` sidecar.

### 3.4 id / extension on primitives across conversion

`copyElement` (§2) copies the primitive's `id` and its `extension[]` onto the R5
primitive even when the value was dropped. So `_field` sidecars survive
conversion; only the blank *value* is removed.

---

## 4. StructureDefinition top-level field map

`StructureDefinition40_50.convertStructureDefinition` —
`CONV/conv40_50/resources40_50/StructureDefinition40_50.java:55-110`. First
`copyDomainResource` (line 56, §2), then each field is a **direct same-name copy
in source order**. **No top-level SD field is renamed, moved, dropped, or turned
into an extension; no extension becomes a field at SD level.**

> **"Same-name copy" ≠ verbatim JSON passthrough for complex-typed metadata.**
> `identifier[]` (Identifier), `contact[]` (ContactDetail), `useContext[]`
> (UsageContext), `jurisdiction[]` (CodeableConcept), and `keyword[]` (Coding) are
> routed through their own datatype converters, which **reconstruct the object in
> canonical R5 field order** and recurse (blank-string drops, nested Coding
> `system→version→code→display→userSelected`, etc.). A Rust port must NOT copy
> these arrays byte-for-byte — verified against the IPS goldens, where R4
> `jurisdiction[0].coding[0]` is authored `{code, system}` but the golden emits
> `{system, code}` (Coding40_50 field order). Only genuine primitives
> (`url`/`version`/`name`/… ) and value-less scalars are literal copies.
> UsageContext.value[x] is a `convertType` choice (same datatype dispatch as
> `fixed[x]`/`pattern[x]`), so an exotic `useContext.value` datatype hits the same
> fail-loud converter as ED value[x].

| # | R4 field | R5 field | Converter (line) | Notes |
|---|----------|----------|-----|-------|
| — | id/meta/implicitRules/language/text/contained/extension/modifierExtension | same | copyDomainResource (56) | §2 |
| 1 | `url` | `url` | Uri (57-58) | |
| 2 | `identifier[]` | `identifier[]` | Identifier (59-60) | |
| 3 | `version` | `version` | String (61-62) | |
| 4 | `name` | `name` | String (63-64) | |
| 5 | `title` | `title` | String (65-66) | |
| 6 | `status` | `status` | PublicationStatus enum (67-68) | |
| 7 | `experimental` | `experimental` | Boolean (69-70) | |
| 8 | `date` | `date` | DateTime (71-72) | |
| 9 | `publisher` | `publisher` | String (73-74) | |
| 10 | `contact[]` | `contact[]` | ContactDetail (75-76) | |
| 11 | `description` | `description` | Markdown (77-78) | already markdown in R4 |
| 12 | `useContext[]` | `useContext[]` | UsageContext (79-80) | |
| 13 | `jurisdiction[]` | `jurisdiction[]` | CodeableConcept (81-82) | |
| 14 | `purpose` | `purpose` | Markdown (83-84) | |
| 15 | `copyright` | `copyright` | Markdown (85-86) | |
| 16 | `keyword[]` | `keyword[]` | Coding (87) | copied straight through though R5 deprecated it |
| 17 | `fhirVersion` | `fhirVersion` | FHIRVersion enum (88-89) | e.g. keeps `"4.0.1"` |
| 18 | `mapping[]` | `mapping[]` | backbone (90-91) | §4a |
| 19 | `kind` | `kind` | StructureDefinitionKind enum (92-93) | §9 |
| 20 | `abstract` | `abstract` | Boolean (94-95) | |
| 21 | `context[]` | `context[]` | backbone (96-97) | §4a |
| 22 | `contextInvariant[]` | `contextInvariant[]` | String (98-99) | |
| 23 | `type` | `type` | Uri (100-101) | |
| 24 | `baseDefinition` | `baseDefinition` | Canonical (102-103) | |
| 25 | `derivation` | `derivation` | TypeDerivationRule enum (104-105) | §9 |
| 26 | `snapshot` | `snapshot` | backbone → `ElementDefinition40_50` per element (106-107) | §4b |
| 27 | `differential` | `differential` | backbone → `ElementDefinition40_50` per element (108-109) | §4b |

Every field is guarded by `hasXxx()`; absent input → absent output.

**R5-only top-level fields NOT sourced from R4** (`versionAlgorithm[x]`,
`copyrightLabel`): the converter never sets them, so they are absent in output.
A Rust port must not synthesize them.

### 4a. SD backbone sub-components
- **mapping** (`StructureDefinition40_50.java:278-292`): copyBackboneElement,
  then `identity` (Id), `uri` (Uri), `name` (String), `comment` (String).
- **context** (310-320): copyBackboneElement, then `type`
  (ExtensionContextType enum, §9), `expression` (String). R4 already uses the
  structured type+expression shape (no legacy `contextType` handling).

### 4b. snapshot / differential
Both `StructureDefinition*Component`s hold `element[]`. `snapshot` (386-394) and
`differential` (406-414) each: copyBackboneElement, then for each
`ElementDefinition` → `ElementDefinition40_50.convertElementDefinition`. Element
order preserved. **All element field logic is delegated to `ElementDefinition40_50`
(§5); the SD converter touches no ED fields.**

---

## 5. ElementDefinition field map

`ElementDefinition40_50.convertElementDefinition` —
`CONV/conv40_50/datatypes40_50/special40_50/ElementDefinition40_50.java:33-95`
(forward = R4→R5).

Begins with `copyBackboneElement(src, tgt, EXT_MUST_VALUE, EXT_VALUE_ALT)`
(lines 36-38) — copies id + non-ignored extensions + modifierExtensions, while
**stripping the two "extension→field" URLs** (see §7) so they don't double up.
Then, in source order:

| R4 field | R5 field | Conversion | Line |
|---|---|---|---|
| (id + extensions) | id + extension[] | copyBackboneElement (ignoring EXT_MUST_VALUE, EXT_VALUE_ALT) | 36-38 |
| path | path | String | 39 |
| representation[] | representation[] | enum (XMLATTR/XMLTEXT/TYPEATTR/CDATEXT/XHTML) | 40 |
| sliceName | sliceName | String | 41 |
| sliceIsConstraining | sliceIsConstraining | Boolean | 42-43 |
| label | label | String | 44 |
| code[] | code[] | Coding | 45 |
| slicing | slicing | §6 | 46 |
| short | short | String | 47 |
| definition | definition | Markdown | 48 |
| comment | comment | Markdown | 49 |
| requirements | requirements | Markdown | 50 |
| alias[] | alias[] | String | 51 |
| min | min | unsignedInt | 52 |
| max | max | String | 53 |
| base | base | §6 | 54 |
| contentReference | contentReference | uri | 55-56 |
| type[] | type[] | §6 | 57-58 |
| defaultValue[x] | defaultValue[x] | convertType | 59-60 |
| meaningWhenMissing | meaningWhenMissing | Markdown | 61-62 |
| orderMeaning | orderMeaning | String | 63 |
| fixed[x] | fixed[x] | convertType | 64-65 |
| pattern[x] | pattern[x] | convertType | 66-67 |
| example[] | example[] | §6 | 68-69 |
| minValue[x] | minValue[x] | convertType | 70-71 |
| maxValue[x] | maxValue[x] | convertType | 72-73 |
| maxLength | maxLength | integer | 74 |
| condition[] | condition[] | id | 75 |
| constraint[] | constraint[] | §6 (xpath→ext) | 76-77 |
| mustSupport | mustSupport | Boolean | 78 |
| isModifier | isModifier | Boolean | 79 |
| isModifierReason | isModifierReason | String | 80-81 |
| isSummary | isSummary | Boolean | 82 |
| binding | binding | §6 | 83 |
| mapping[] | mapping[] | §6 | 84-85 |
| — extension `EXT_MUST_VALUE` | **field** `mustHaveValue` (boolean) | promote ext→field | 87-89 |
| — extension `EXT_VALUE_ALT`[] | **field** `valueAlternatives[]` (canonical) | promote ext→field | 90-92 |

**No R4 ED field is dropped** (constraint.xpath is re-expressed, not lost — §7).

---

## 6. ElementDefinition sub-structures

All in `ElementDefinition40_50.java`. Each starts with copyElement (id +
verbatim non-ignored extensions), then sets typed fields in the order shown.

- **slicing** (220-230): discriminator[] → `description` (String) → `ordered`
  (Boolean) → `rules` (CLOSED/OPEN/OPENATEND). **discriminator** (294-301):
  `type` (VALUE/EXISTS/PATTERN/TYPE/PROFILE) → `path` (String).
- **base** (374-382): `path` (String) → `min` (unsignedInt) → `max` (String).
- **type** (`TypeRefComponent`, 394-406): `code` (Uri) → `profile[]` (Canonical,
  value-preserving) → `targetProfile[]` (Canonical) → `aggregation[]`
  (CONTAINED/REFERENCED/BUNDLED) → `versioning` (EITHER/INDEPENDENT/SPECIFIC).
  **Note:** R4 `profile`/`targetProfile` are already `canonical[]` — no
  uri→canonical transform, just value-preserving copy.
- **example** (522-530): `label` (String) → `value[x]` (convertType).
- **constraint** (542-556): `key` (id) → `requirements`
  (**string→markdown**, §8) → `severity` (ERROR/WARNING) → `human` (String) →
  `expression` (String) → **xpath → extension** (551-553, §7) → `source`
  (Canonical). No `xpath` field survives.
- **binding** (618-634): copyElement with ignore-list
  {`EXT_ADDITIONAL_BINDING`, `EXT_BINDING_ADDITIONAL`} (621-622) → `strength`
  (BindingStrength enum) → `description` (**string→markdown**, §8) → `valueSet`
  (Canonical, value-preserving; R4 binding.valueSet is already canonical) →
  `additional[]` built from the two ignored extension families (627-632, §7).
- **binding.additional** (`convertAdditional`, 636-659): copyElement ignoring
  {`valueSet`,`purpose`,`documentation`,`shortDoco`,`usage`,`any`} (639) →
  `purpose` (from child ext `purpose`) → `valueSet` (canonical) →
  `documentation` (markdown) → `shortDoco` (string) → `usage[]` (UsageContext,
  repeating) → `any` (boolean). (Any other child extension is copied verbatim.)
- **mapping** (700-709): `identity` (id) → `language` (code) → `map` (String) →
  `comment` (**string→markdown**, §8).

---

## 7. Field↔extension transforms

The **complete** set for stage 2 (everything else copies verbatim):

| Direction | R4 form | R5 form | Extension URL | Cite |
|---|---|---|---|---|
| field→ext | `constraint.xpath` (string) | `constraint.extension[…]` valueString, **appended last** | `http://hl7.org/fhir/4.0/StructureDefinition/extension-ElementDefinition.constraint.xpath` | ED:551-553; `ExtensionDefinitions.java:158` |
| ext→field | `binding.extension[additional-binding]` (tooling) | `binding.additional[]` | `http://hl7.org/fhir/tools/StructureDefinition/additional-binding` | ED:621-632; consts below |
| ext→field | `binding.extension[…binding.additional]` (5.0 backport) | `binding.additional[]` | `http://hl7.org/fhir/5.0/StructureDefinition/extension-ElementDefinition.binding.additional` | ED:621-632 |
| ext→field | ED `extension[mustHaveValue]` | `ElementDefinition.mustHaveValue` (boolean) | `http://hl7.org/fhir/5.0/StructureDefinition/extension-ElementDefinition.mustHaveValue` | ED:36-38, 87-89 |
| ext→field | ED `extension[valueAlternatives]`[] | `ElementDefinition.valueAlternatives[]` (canonical) | `http://hl7.org/fhir/5.0/StructureDefinition/extension-ElementDefinition.valueAlternatives` | ED:36-38, 90-92 |

URL constants: `EXT_BINDING_ADDITIONAL` = `.../tools/.../additional-binding`
(`R5/.../ExtensionDefinitions.java:130`); `EXT_ADDITIONAL_BINDING`,
`EXT_MUST_VALUE`, `EXT_VALUE_ALT` = `.../5.0/.../extension-ElementDefinition.*`
(`CONV/.../VersionConvertorConstants.java:48,62,70`).

### 7.1 constraint.xpath ordering (byte-critical)
`tgt.addExtension(new Extension(EXT_XPATH_CONSTRAINT, new StringType(xpath)))`
is called at ED:552 **after** copyElement already populated `constraint.extension[]`
from the source's own extensions. So the xpath extension is **always the last
entry** of `constraint.extension[]`. If the R4 constraint had no other
extensions, `extension = [{url: <xpath-url>, valueString: <xpath>}]`.
Serialized constraint key order: `id?`, `extension?` (verbatim + xpath appended),
`key`, `requirements?`, `severity`, `human`, `expression?`, `source?`.
Verified in `sdc-library.converted.json` (no pre-existing ext → xpath at
`extension[0]`) and `us-core-observation-lab.converted.json` (bestpractice at
`[0]`, xpath at `[1]`).

### 7.2 binding.additional structure
R5 `binding.additional[]` element (serialized order): `id?`, `extension?`
(verbatim non-consumed), `purpose`, `valueSet`, `documentation?`, `shortDoco?`,
`usage[]?`, `any?` (ED:636-659). Built by reading the R4 complex-extension's
child extensions.

### 7.3 What is NOT transformed (copied verbatim as extensions)
`elementdefinition-bindingName`, `elementdefinition-isCommonBinding`,
`elementdefinition-minValueSet`, `elementdefinition-maxValueSet`,
`elementdefinition-bestpractice`, best-practice-explanation, and **all other**
ED/binding/constraint extensions have **no code path** in `ElementDefinition40_50`
— they are copied verbatim by copyElement onto the R5 object's `extension[]`.
Verified: `us-core-genderIdentity` keeps `elementdefinition-bindingName`
verbatim on `binding.extension[0]`; `us-core-medicationrequest` keeps
`elementdefinition-maxValueSet` verbatim on `binding.extension[0]` (NOT in
`additional[]`).

> This directly contradicts the AGENTS.md framing that
> "additionalBinding/bindingName projection, isCommonBinding" are conversion
> transforms. **Only the two `additional-binding` tooling URLs project into
> `binding.additional[]`.** `bindingName`/`isCommonBinding`/`minValueSet`/
> `maxValueSet` pass through as plain extensions. See §11.

---

## 8. string→markdown promotions

R4 `string` fields whose R5 type is `markdown`. This is a **type-tag change
only** — the JSON value is identical, so it is invisible in a value diff but the
Rust model layer must treat them as markdown (matters for later markdown-URL
rewriting in stage 5, not stage 2). Forward direction:

- `constraint.requirements` — ED:547 (`convertStringToMarkdown`)
- `binding.description` — ED:624
- `mapping.comment` (ElementDefinition.mapping) — ED:707

(`ElementDefinition.definition/comment/requirements/meaningWhenMissing` and
`StructureDefinition.description/purpose/copyright` are already markdown in R4;
no promotion.)

---

## 9. Enum value sets

Each enum converter returns `null` when `src == null || src.isEmpty()` (empty
enum → field omitted) and maps by Java enum name, out-of-range → NULL:

- **StructureDefinition.kind** (SD:174-201): PRIMITIVETYPE, COMPLEXTYPE,
  RESOURCE, LOGICAL.
- **StructureDefinition.derivation** (SD:232-253): SPECIALIZATION, CONSTRAINT.
- **StructureDefinition.context.type** (SD:334-358): FHIRPATH, ELEMENT,
  EXTENSION.
- **status** (PublicationStatus) / **fhirVersion** (FHIRVersion): delegated to
  `Enumerations40_50` — value-preserving code map (e.g. fhirVersion keeps
  `"4.0.1"`).
- ElementDefinition enums: `representation`, `slicing.rules`,
  `discriminator.type`, `type.aggregation`, `type.versioning`,
  `constraint.severity`, `binding.strength` — all name-mapped, out-of-range →
  NULL (see §5/§6 for allowed values).

---

## 10. R5 JSON serialization: key order + byte format

Serialized by `R5/formats/JsonParser.java` (PRETTY). **No sorting anywhere** —
emission order == source-code call order; base-class properties emit before own
fields; extension arrays emit in **model list order**
(`composeElementProperties` 36757-36767, no comparator).

### 10.1 Byte format
`setOutputStyle(PRETTY)` → `JsonCreatorDirect(osw, true, false)`
(`R5/formats/JsonParserBase.java:208`). Observed in every golden:
- Indent: **2 spaces**.
- Colon separator: **`" : "`** (space-colon-space), e.g. `"id" : "x"`.
- Line endings: **CRLF (`\r\n`)**.
- Trailing newline at EOF.
- Arrays of objects put the opening `{` on the same line as `[` /the key
  (HAPI's compact-object style), e.g. `"contact" : [{`.

A byte-exact Rust golden comparison must reproduce `" : "`, 2-space indent,
CRLF, and HAPI's `[{` array-object style. (If the gate is *semantic* JSON diff
rather than byte diff, only key order within objects and array order matter —
still reproduce those.)

### 10.2 Top-of-resource order
`resourceType` (always first) → `id`, `meta`, `implicitRules`, `language`
(`composeResourceProperties` 38888) → `text`, `contained[]`, `extension[]`,
`modifierExtension[]` (`composeDomainResourceProperties` 38856) → resource
fields.

### 10.3 StructureDefinition (`composeStructureDefinition` 68179, props 68186)
resourceType → [id, meta, implicitRules, language, text, contained, extension,
modifierExtension] → url → identifier[] → version → **versionAlgorithm[x]**
(68203, R5-only, absent from converted R4) → name → title → status →
experimental → date → publisher → contact[] → description → useContext[] →
jurisdiction[] → purpose → copyright → **copyrightLabel** (68260, R5-only) →
keyword[] → fhirVersion → mapping[] → kind → abstract → context[] →
contextInvariant[] (+ `_contextInvariant[]`) → type → baseDefinition →
derivation → snapshot → differential.

SD sub-components: **mapping** (68335): identity, uri, name, comment.
**context** (68363): type, expression. **snapshot**/**differential**: element[].

### 10.4 ElementDefinition (`composeElementDefinition` 37462, props 37470)
[id, extension[], modifierExtension[]] → path → representation[]
(+`_representation`) → sliceName → sliceIsConstraining → label → code[] →
slicing → short → definition → comment → requirements → alias[] (+`_alias`) →
min → max → base → contentReference → type[] → defaultValue[x] →
meaningWhenMissing → orderMeaning → fixed[x] → pattern[x] → example[] →
minValue[x] → maxValue[x] → maxLength → condition[] (+`_condition`) →
constraint[] → **mustHaveValue** (37614) → **valueAlternatives[]**
(+`_valueAlternatives`, 37619) → mustSupport → isModifier → isModifierReason →
isSummary → binding → mapping[].

### 10.5 ED sub-structures (each: id, extension[] first, then)
- **slicing** (37666): discriminator[] → description → ordered → rules.
  **discriminator** (37696): type → path.
- **base** (37716): path → min → max.
- **type** (`composeTypeRefComponent` 37732): code → profile[] (+`_profile`) →
  targetProfile[] (+`_targetProfile`) → aggregation[] (+`_aggregation`) →
  versioning.
- **constraint** (37819): key → requirements → severity → **suppress** (37834,
  R5-only) → human → expression → source. **No `xpath` field** (it only lives in
  `extension[]`).
- **binding** (37859): strength → description → valueSet → additional[].
- **binding.additional** (37889): purpose → valueSet → documentation → shortDoco
  → usage[] → any.
- **mapping** (37927): identity → language → map → comment.
- **example** (37800): label → value[x].

### 10.6 Extension (`composeExtension`, props 38023)
[id, extension[]] → **url** → **value[x]** (`url` before `value[x]`). Extensions
never sorted — model order preserved.

---

## 11. Surprises vs AGENTS.md

The AGENTS.md "Java Parity Notes" mix generator behaviors with conversion
behaviors. Corrections/clarifications discovered here:

1. **bindingName / isCommonBinding / minValueSet / maxValueSet are NOT
   conversion transforms.** AGENTS.md lists "additionalBinding/bindingName
   projection, isCommonBinding" as conversion facts. In truth
   `ElementDefinition40_50` has **no** handling for `elementdefinition-bindingName`,
   `elementdefinition-isCommonBinding`, `elementdefinition-minValueSet`, or
   `elementdefinition-maxValueSet` — they are copied verbatim as extensions.
   **Only** the two `additional-binding` tooling URLs
   (`.../tools/.../additional-binding` and `.../5.0/...binding.additional`)
   project into `binding.additional[]`. The AGENTS.md line "R4
   `elementdefinition-bindingName`/`additional` extensions project into R5
   `binding.additional[]`" is only correct for `additional`, not `bindingName`.
   Any `binding.additional[]` / `isCommonBinding` *stripping* seen in real
   goldens happens later (generator's `NON_INHERITED_ED_URLS`, R5-only, at
   snapshot/projection time — stages 4/5), NOT in stage-2 conversion. Verified
   in goldens (§7.3).

2. **The empty-string drop happens at conversion, not parse.** AGENTS.md:
   "R4->R5 parse drops empty-string primitives". Precisely: the value survives
   R4 parse and is dropped by the converter's `src.hasValue()` guard (and again
   at compose). Net effect is the same (blank string → absent field), but a Rust
   port must implement it in the *conversion* function, and the trigger is
   `StringUtils.isBlank` (whitespace-only also drops), not literal `== ""`.

3. **xpath extension is appended LAST** within `constraint.extension[]` (after
   any pre-existing constraint extensions), with URL
   `http://hl7.org/fhir/4.0/StructureDefinition/extension-ElementDefinition.constraint.xpath`
   (the `/4.0/` path, not `/5.0/`). AGENTS.md's "native R5 conversion of R4
   constraint `xpath` extensions inserts `constraint.extension` first in key
   order" refers to *key order within the constraint object* (extension key
   comes before key/severity/etc.) — that is TRUE and consistent with §10.5.
   It does **not** mean the xpath extension is first *within the extension
   array*; it is last there.

4. **fhirVersion stays `"4.0.1"`** through conversion (§4 #17) — matches
   AGENTS.md "Native R5 internal output for R4 resources keeps fhirVersion
   4.0.1".

5. **`constraint.xpath` absence in native-R5 output** (AGENTS.md) is because
   the field is *converted into the extension* — the field is genuinely gone,
   the data is preserved in the extension. Both facts hold simultaneously.

6. **string→markdown promotions are invisible to a JSON value diff** (§8) but
   real in the model — flagged so the Rust implementer models these three fields
   as markdown for downstream stage-5 URL rewriting.

---

## 12. Open questions for the Rust implementer

1. **Byte vs semantic golden comparison.** The goldens are Java-PRETTY:
   `" : "` separators, 2-space indent, **CRLF**, HAPI `[{` array-object style
   (§10.1). Decide whether the stage-2 gate compares bytes (then the Rust
   serializer must reproduce all of this exactly, including CRLF) or normalized
   JSON (then only object-key order + array order matter). Recommend semantic
   with an explicit key-order check, since CRLF/`[{` are serializer cosmetics
   unrelated to conversion correctness.

2. **Decimal serialization = `BigDecimal.toString()`**, which can emit
   scientific notation (`1E+2`) and preserves scale/trailing zeros (§3.2). If a
   stage-2 fixture ever carries a decimal `minValue[x]`/`example`, the Rust
   serializer must match `java.math.BigDecimal.toString()` output, not the raw
   input lexeme. None of the current 39 goldens exercise this — add a fixture if
   the walk stage will.

3. **Blank = `StringUtils.isBlank`**, not `== ""` (§3.1). Confirm the Rust drop
   uses trimmed-empty semantics (whitespace-only also drops).

4. **`convertType` for `fixed[x]`/`pattern[x]`/`defaultValue[x]`/`example.value[x]`/
   `minValue[x]`/`maxValue[x]`** recurses into arbitrary datatypes (Coding,
   CodeableConcept, Quantity, Reference, Identifier, etc.). This spec covers the
   SD/ED envelope; the datatype converters those `value[x]` fields dispatch to
   (`CONV/conv40_50/datatypes40_50/*`) are structurally same-name copies but
   have not each been line-audited here. For StructureDefinition fixtures the
   common cases are CodeableConcept/Coding/Quantity/string/uri/boolean/code —
   audit any exotic datatype a fixture actually uses before trusting the port.

   **RESOLVED (W2a, Rust port).** The Rust `convert.rs` implements a fail-loud
   `convert_datatype`: every primitive (verbatim value copy + blank-drop) plus the
   complex datatypes that surface across the 39 goldens AND the full
   `{ips,us-core,qicore,sdc,pas,ecr}` fixture sweep (338 fixtures). Audited field
   orders (all begin with copyElement = id+extension, then):
   - **Coding** (`general40_50/Coding40_50.java:14-19`): system, version, code,
     display, userSelected.
   - **CodeableConcept** (`CodeableConcept40_50.java:12-14`): coding[], text.
   - **Identifier** (`Identifier40_50.java:14-20`): use, type (CodeableConcept),
     system, value, period, assigner (Reference).
   - **Reference** (`Reference40_50.java`): reference, type, identifier, display.
   - **Period** (`Period40_50.java:11-13`): start, end.
   - **Quantity** (+Duration/Age/Count/Distance/MoneyQuantity/SimpleQuantity, which
     delegate to `Quantity40_50.copyQuantity`; `Quantity40_50.java:16-21`): value,
     comparator, unit, system, code.
   - **ContactPoint** (`ContactPoint40_50.java:14-18`): system, value, use, rank,
     period.
   - **ContactDetail** (`ContactDetail40_50.java:13-15`): name, telecom[].
   - **UsageContext** (`UsageContext40_50.java:12-14`): code (Coding), value[x]
     (convertType — same fail-loud dispatch).
   Any datatype NOT in this set returns `Err("unimplemented value[x] datatype
   converter: <Name>")` — no silent passthrough. None of the 338 smoke fixtures
   needed anything beyond the above.

5. **Nested/contained resources.** `copyDomainResource` recurses `contained[]`
   through `convertResource` with `failFast=true` (throws on unknown types). SD
   fixtures rarely contain resources; if one does, the contained resource is
   fully converted by its own R4→R5 converter (out of scope here).

6. **Modifier extensions with the promoted URLs.** The ED ignore-list
   (`EXT_MUST_VALUE`, `EXT_VALUE_ALT`) is applied by `copyBackboneElement` to
   *both* `extension` and `modifierExtension` (§2). Confirm the Rust port strips
   these URLs from both lists (unlikely to appear as modifierExtensions, but the
   Java code would).

7. **`--dump-converted-sorted` vs `--dump-converted`.** These goldens are the
   **unsorted** raw converted form (pure stage 2). `sortDifferential`+`setIds`
   (the stage-3 prep `generateSnapshot` sees) is a *separate* stage; the
   sorted-dump mode exists in the driver for stage-3 oracling but should not be
   conflated with stage-2 output.

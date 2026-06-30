# Snapshot Generator Notes

This directory is for the Rust pure snapshot generator work on branch
`snapshot-gen`.

## Scope

- Target: Layer-A `ProfileUtilities.generateSnapshot` behavior using the same
  model path the IG Publisher uses.
- Important R4 fact: current Publisher parses/loads R4 IG resources into the R5
  model and runs `org.hl7.fhir.r5.conformance.profile.ProfileUtilities`, then
  downconverts to R4 only when writing R4 artifacts. Do not use old
  `org.hl7.fhir.r4.conformance.ProfileUtilities` as the R4 oracle target.
- Default implementation path sorts/normalizes the input differential before
  generation. Direct raw Java behavior is still available with `--direct`.
- Transitional CLI flags (`--native-r5`, `--output-r5`, `--output-r4`) exist to
  keep old fixture checks useful while we move the target. Do not let this turn
  into two production paths: the steady-state Rust target is the Publisher
  native internal model path, with R4 artifact downconversion kept only if we
  intentionally build that as a separate output step.
- Cleanup requirement: once the migration fixtures no longer need them, remove
  transitional output flags from the Rust CLI/harness or move downconversion
  behind a clearly separate artifact-projection command. The main snapshot path
  should be one Publisher-native Layer-A path.
- Out of scope for now: Publisher canonical version pinning, narrative work,
  validation orchestration, and broader IG Publisher Layer-B policy.

## Oracle Commands

```sh
# Java oracle, default sort/normalize then generateSnapshot (R5 default).
# For --r4, the default output is Publisher's native R5 internal model with
# fhirVersion=4.0.1. Use --output-r4 for the separate Publisher-style downconvert.
bash snapshot/oracle/gen-snapshot.sh [--r4|--r5] [--native-r5|--output-r4] <input.json> <out.json> [pkg#ver ...]

# Load local IG canonical resources into the oracle context, matching Publisher:
bash snapshot/oracle/gen-snapshot.sh --r4 --native-r5 --local-dir /home/jmandel/periodicity/temp/ips-ig/fsh-generated/resources \
  snapshot/harvested/r4/ips/fixtures/AllergyIntolerance-uv-ips.json temp/allergy.snapshot.json \
  hl7.fhir.r4.core#4.0.1 hl7.fhir.uv.ipa#1.1.0 hl7.fhir.uv.extensions.r4#5.3.0

# Direct raw generateSnapshot without sortDifferential:
bash snapshot/oracle/gen-snapshot.sh --direct <input.json> <out.json> [pkg#ver ...]

# Regenerate fixture goldens:
bash snapshot/gen-goldens.sh

# Harvest real R4 SUSHI-generated profiles and generate Publisher-path goldens:
bash snapshot/harvest-r4-sushi.sh ips /home/jmandel/periodicity/temp/ips-ig/fsh-generated/resources \
  hl7.fhir.r4.core#4.0.1 hl7.fhir.uv.ipa#1.1.0 hl7.fhir.uv.extensions.r4#5.3.0

# Harvested R4 goldens default to native R5 internal output. Set ORACLE_OUTPUT=r4
# to generate the explicit downconverted R4 artifact shape instead.
ORACLE_OUTPUT=r4 bash snapshot/gen-harvested-r4-goldens.sh snapshot/harvested/r4/ips \
  hl7.fhir.r4.core#4.0.1 hl7.fhir.uv.ipa#1.1.0 hl7.fhir.uv.extensions.r4#5.3.0

# Compare snapshot.element semantically:
node snapshot/diff-snapshot.cjs <expected.json> <actual.json>
```

Defaults:
- R5 package list: `hl7.fhir.r5.core#5.0.0`
- R4 package list: `hl7.fhir.r4.core#4.0.1`
- cache: `temp/fhir-home/.fhir/packages`
- local Java dependency source: `FHIR_CORE_REPO`, defaulting to
  `/home/jmandel/hobby/fhir-perf/repos/fhir-core`

The oracle wrapper seeds `temp/fhir-home/.fhir/packages` from real
`~/.fhir/packages` using hardlinks and uses the root harness guard. Never point
`FHIR_HOME` at the real home.

GOTCHA: `SimpleWorkerContextBuilder` must be created with
`withAllowLoadingDuplicates(true)` before `fromPackage(...)`; setting
`ctx.setAllowLoadingDuplicates(true)` after `fromPackage` is too late because R5
core loads a duplicate `spdx-license` CodeSystem during the builder load.

GOTCHA: R4 IPS/mCODE profiles with open type slicing on choice elements are real
Publisher inputs. Old R4 `ProfileUtilities.processPaths` rejects them with
`Type slicing with slicing.rules != closed`; current Publisher's R5
`ProfilePathProcessor` treats type slicing as closed for snapshot generation
even when the differential says open. This is oracle behavior, not an IPS bug.

The R4 wrapper (`SnapOracleR4.java`) therefore parses R4 JSON, converts to the R5
model, runs R5 `ProfileUtilities` with `setNewSlicingProcessing(true)`, and
emits native R5 internal JSON by default. `--output-r4` performs the separate
R5->R4 downconversion.

## Rust Commands

```sh
cargo run -p snapshot_gen -- --cache temp/fhir-home/.fhir/packages \
  --package hl7.fhir.r5.core#5.0.0 snapshot/fixtures/r5-patient-card-ms.json

cargo test -p snapshot_gen

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/ips \
  hl7.fhir.r4.core#4.0.1 hl7.fhir.uv.ipa#1.1.0 hl7.fhir.uv.extensions.r4#5.3.0

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/mcode \
  hl7.fhir.r4.core#4.0.1 hl7.fhir.us.core#6.1.0 \
  hl7.fhir.uv.genomics-reporting#2.0.0 hl7.fhir.uv.extensions.r4#5.3.0

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/crd \
  hl7.fhir.r4.core#4.0.1 hl7.fhir.uv.xver-r5.r4#0.1.0 \
  hl7.fhir.us.core#7.0.0 hl7.fhir.us.core.v610#6.1.0 \
  hl7.fhir.us.core.v311#3.1.1 hl7.fhir.uv.sdc#4.0.0 \
  hl7.fhir.us.davinci-hrex#1.2.0 us.nlm.vsac#0.19.0 \
  hl7.fhir.uv.tools.r4#1.1.2 hl7.fhir.uv.cds-hooks#3.0.0-ballot \
  hl7.fhir.uv.cds-hooks-library#1.0.1

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/sdc \
  hl7.fhir.r4.core#4.0.1 hl7.fhir.uv.xver-r5.r4#0.1.0 \
  hl7.fhir.r4.examples#4.0.1 hl7.fhir.uv.extensions.r4#5.3.0
```

The Rust generator now gates strict semantic `snapshot.element[]` parity against
the current fixture ladder:

- Patient: min profile, cardinality/MS, unsorted cardinality/MS, binding overlay,
  fixed/pattern, additive merge fields, choice type narrowing, nested child
  unfold, simple slice, slice child, slash reslice, datatype unfold.
- R4 Patient: cardinality/MS against `hl7.fhir.r4.core#4.0.1`.
- Observation: `Reference.targetProfile` narrowing.
- Extension: basic Extension root doco normalization.
- Questionnaire: recursive `contentReference` unfold.
- Real R5 core: stripped `MoneyQuantity` profile over `Quantity`.

Keep broadening oracle-first; do not replace fixture parity with looser field
assertions.

## Sorting Contract

FHIR-core's snapshot walker is forward-only. The usable differential order is
tree/preorder: root first, then elements in base snapshot order, with descendants
inside their parent window. The Java oracle can run `ProfileUtilities.sortDifferential`
before `generateSnapshot`; the Rust implementation has an explicit normalizer for
the same role. Keep the core merge/walk logic separate from this prep step.

GOTCHA: Java `sortDifferential` can reorder children when the root is already
first, but it does not recover a root that appears after child paths; that case
reports a count mismatch and drops the later block.

Current strict full-snapshot tests compare stable JSON for the complete
`snapshot.element[]` array, plus normalized differential order.

## Java Parity Notes

- Real R4 IPS coverage (2026-06-30): harvested 29 constraint
  StructureDefinitions from `/home/jmandel/periodicity/temp/ips-ig/fsh-generated/resources`.
  Publisher-path native R5 Java oracle generated **29/29** goldens, and Rust
  now matches **29/29**. The 14 profiles that failed under old R4 Java all
  succeed now, including `AllergyIntolerance-uv-ips`, the Observation
  choice/type-slicing profiles, and `Procedure-uv-ips`.
- Real R4 mCODE coverage (2026-06-30): harvested 46 constraint
  StructureDefinitions from `/home/jmandel/periodicity/temp/mcode-ig/fsh-generated/resources`
  after skipping 7 local-base derived profiles. Publisher-path native R5 Java
  oracle generated **46/46** goldens, and Rust now matches **46/46**. The two
  profiles that failed under old R4 Java, `mcode-genomic-region-studied` and
  `mcode-genomic-variant`, now generate successfully.
- Real R4 CRD coverage (2026-06-30): harvested 22 constraint
  StructureDefinitions from `/home/jmandel/periodicity/temp/crd-ig/fsh-generated/resources`
  after skipping 4 local-base derived profiles plus 1 specialization. Package
  context must include the CRD dependency set listed in the command above. The
  Publisher-path native R5 Java oracle generated **22/22** goldens, and Rust now
  matches **22/22**.
- Real R4 SDC coverage (2026-06-30): harvested 73 constraint
  StructureDefinitions from `/home/jmandel/periodicity/temp/sdc-ig/fsh-generated/resources`
  after skipping 18 local-base derived profiles. Package context must include
  R4 core, xver-r5.r4, R4 examples, and extensions.r4 as listed above. The
  Publisher-path native R5 Java oracle generated **73/73** goldens, and Rust now
  matches **73/73**.
- Native R5 internal output for R4 resources keeps `fhirVersion: "4.0.1"` but
  follows R5 model conversion behavior, e.g. R4 constraint `xpath` fields are
  absent.
- `NON_INHERITED_ED_URLS` in Rust intentionally mirrors
  `org.hl7.fhir.r5.conformance.profile.ProfileUtilities.NON_INHERITED_ED_URLS`
  and is R5-only. R4 Java does not strip the same inherited binding extensions;
  the R4 Patient fixture keeps `elementdefinition-isCommonBinding`.
  Keep odd-looking URLs like `elementdefinition-isCommonBinding`,
  `obligation-profile`, `structuredefinition-standards-status-reason`, and
  `structuredefinition-summary`; Java strips these from inherited
  ElementDefinitions/bindings in R5.
- Java keeps duplicate inherited extensions on unconstrained elements, but a
  constrained element merge can collapse exact duplicate extension values. The
  Questionnaire fixture exercises this with duplicate
  `elementdefinition-translatable` extensions.
- Recursive `contentReference` expansion follows Java
  `replaceFromContentReference`: copy only the referenced element's `type` to
  the referring element, remove `contentReference`, then clone referenced
  descendants under the recursive id/path. Relative child contentReferences are
  canonicalized to the source StructureDefinition URL, e.g.
  `http://hl7.org/fhir/StructureDefinition/Questionnaire#Questionnaire.item`.
- Inherited relative `contentReference` values are canonicalized even when Rust
  does not walk into that element. Observation exercises this on
  `Observation.component.referenceRange`.
- Markdown link rewriting must preserve UTF-8. Do not byte-cast non-link text;
  Observation has curly apostrophes that Java preserves.
- R4 markdown URL bases differ by path: old standalone R4 output prepends the
  unversioned `webUrl` argument (`http://hl7.org/fhir/`), while Publisher-path
  native output for R4 uses `http://hl7.org/fhir/R4/` and native R5 uses
  `http://hl7.org/fhir/R5/`. Publisher has observed quirks:
  `device-mappings.html#udi` remains unversioned absolute, and inherited
  `null.html` from BodyStructure becomes
  `http://hl7.org/fhir/extension-bodysite.html`. CRD also showed unversioned
  `general-requirements.html#required-bindings-when-slicing-by-valuesets` and
  `servicerequest-example-di.html` links in inherited R4-native text. SDC added
  `event.html` as unversioned absolute, while several extension-root doco links
  remain relative even under R4-native output:
  `StructureDefinition-rendering-markdown.html`,
  `StructureDefinition-rendering-xhtml.html`,
  `codesystem-concept-properties.html#concept-properties-itemWeight`, and
  `workflow-extensions.html#instantiation`.
- Binding overlay fixtures should include `strength` when setting
  `binding.description`; Java can throw a null dereference for a
  description-only binding differential on a required base binding.
- Extension root and extension child doco are normalized by Java
  `checkExtensionDoco`: short/definition become generic extension text and
  comment/requirements/alias/mapping are removed before derived rules apply.
- `ProfileUtilities.updateURLs` rewrites relative markdown `.html` links to the
  context spec URL (`http://hl7.org/fhir/R5/` for the current oracle).
- Differential sorting must apply unsliced constraints before sliced
  descendants. A base-order-only sort can move entries such as
  `Bundle.entry:composition.resource` before the unsliced `Bundle.entry`
  children and lose inherited constraints.
- Slice descendant unfolding must clone the current constrained unsliced
  children, not the original base children. Keep Java-retained extension anchors
  such as `Composition.section:sectionProblems.extension`; removing them breaks
  real profile parity.
- When a differential narrows an element to a profiled type, unfold children from
  the profiled StructureDefinition snapshot, not the raw type. If the profiled
  SD is local, convert its own R4 XPath constraints before native projection.
  For profiled resource types on `Parameters.parameter.resource`, Java overlays
  profile root short/definition but preserves the base element comment when the
  profile root has no comment.
- Extension profile root application is path-sensitive in Publisher native R5
  output. Local/generated-on-demand extension profiles may have no snapshot in
  SUSHI output; Rust recursively generates their snapshots before using them for
  child unfolding or root overlay. Local extension slices project root
  constraints when the differential owns the slicing context; adding a slice
  under inherited slicing copies root doco but keeps only base
  `ele-1`/`ext-1` constraints. Local extension root `condition` is kept for
  resource-level and BackboneElement extension slices, but omitted under
  datatype-valued extension sites. Existing local snapshots with no `ext-1`
  source get the host/base profile source (mCODE behavior); generated-on-demand
  local profiles that already have `source=Extension` keep it (CRD behavior).
  SDC shows owned nested extension slicing under BackboneElement
  (`QuestionnaireResponse.item.extension:itemMedia`) also projects local root
  XPath constraints and uses the host profile as the missing `ext-1.source`.
  Non-local extension slices under inherited slicing copy root doco but keep
  inherited/base constraints and omit root `condition`.
  Observed exceptions still need root-cause cleanup:
  `mcode-histology-morphology-behavior`, core `condition-related`,
  `alternate-reference` condition omission, core `codeOptions`, and versioned
  `artifact-versionAlgorithm|5.2.0` generic Extension doco differ from the
  general overlay rule.
- Mapping arrays are differential-first in Publisher snapshots: derived
  `mapping[]` entries precede inherited mappings while exact duplicates are
  suppressed. SDC `CodeSystem`, `Questionnaire`, and `ValueSet` root/element
  mappings exercise this.
- When inserting slices under a sliced `contentReference` anchor, Java can
  materialize referenced children while keeping `contentReference` on the anchor
  itself. SDC `Parameters.parameter:context.part` exercises this.
- Native R5 conversion of R4 constraint `xpath` extensions inserts
  `constraint.extension` first in key order. SDC root QuestionnaireResponse
  constraints exercise this.
- SDC `ServiceRequest.code.binding.description` shows a one-off Publisher text
  normalization: the inherited R4 text drops the final sentence pointing at the
  LOINC Order codes ValueSet.
- R4 `elementdefinition-bindingName`/`additional` extensions project into R5
  `binding.additional[]`; inherited fhir-type extensions only become the native
  `fhir-type` shape on the actual resource root `.id`.
- Type-slicing `rules` are not globally normalized. Publisher keeps most R4
  open `$this` choice slicing open (IPS timing/effective/value[x], CRD
  Timing/NutritionOrder), but CRD's Reference-vs-CodeableConcept choice slices
  (`DeviceRequest.code[x]`, `MedicationRequest.medication[x]`) come out closed.
- When a non-Backbone datatype element is sliced, Java removes inherited
  unsliced datatype children under that anchor before inserting the slices
  (CRD `Practitioner.identifier`). Backbone slices keep their unsliced children.
- Current native R5 projection preserves missing `constraint.source` for the US
  Core identifier check constraints `us-core-16` through `us-core-19`; Java does
  not stamp those with the containing profile URL in CRD.

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

# Install and harvest a published R4 package into snapshot/harvested/r4/<key>.
# The helper writes only under temp/fhir-home/.fhir/packages and uses the package
# itself as local canonical context, so local-base chains in the package are fishable.
bash snapshot/harvest-r4-package.sh pacio-toc hl7.fhir.us.pacio-toc#1.0.0

# Recheck a harvested package with the R4-compatible dependency closure from cache.
# Rust harvested checks run one package-loaded batch by default (`RUST_BATCH=1`);
# set `RUST_BATCH=0` only when debugging one-resource process state.
bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/pacio-toc \
  $(node snapshot/package-deps.cjs --cache temp/fhir-home/.fhir/packages hl7.fhir.us.pacio-toc#1.0.0)

# Harvested R4 goldens default to native R5 internal output. Set ORACLE_OUTPUT=r4
# to generate the explicit downconverted R4 artifact shape instead. Java oracle
# generation is batched by default (`ORACLE_BATCH=1`); set `ORACLE_BATCH=0` only
# for single-fixture debugging.
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

Published-package dependency closures are filtered to R4-compatible packages.
Some IG package manifests include future/R5 dependencies; loading those into the
R4 Publisher-path oracle creates ambiguous duplicate R4/R5 types (for example
`Identifier`). `snapshot/package-deps.cjs` skips packages whose `fhirVersions`
has no `4.*` entry and canonicalizes `hl7.fhir.r4.core#4.0.0` to `4.0.1`.

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

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/carinbb \
  hl7.fhir.r4.core#4.0.1 hl7.fhir.us.core#7.0.0 \
  hl7.fhir.uv.extensions.r4#5.3.0 hl7.fhir.us.carin-bb#dev

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/dtr \
  hl7.fhir.r4.core#4.0.1 hl7.fhir.uv.xver-r5.r4#0.1.0 \
  hl7.fhir.us.core#7.0.0 hl7.fhir.us.core.v610#6.1.0 \
  hl7.fhir.us.core.v311#3.1.1 hl7.fhir.uv.sdc#4.0.0 \
  hl7.fhir.us.davinci-hrex#1.2.0 \
  hl7.fhir.uv.extensions.r4#5.3.0-ballot-tc1 \
  hl7.fhir.us.davinci-crd#2.2.1 hl7.fhir.us.davinci-pas#2.2.1 \
  hl7.fhir.uv.tools.r4#1.1.2

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/ecr \
  hl7.fhir.r4.core#4.0.1 hl7.fhir.uv.xver-r5.r4#0.1.0 \
  hl7.fhir.us.core#6.1.0 us.nlm.vsac#0.23.0 \
  us.cdc.phinvads#0.12.0 hl7.fhir.us.bfdr#2.0.0 \
  hl7.fhir.us.odh#1.3.0 hl7.fhir.us.ph-library#2.0.0-snapshot \
  hl7.fhir.uv.cql#2.0.0 hl7.fhir.uv.crmi#1.0.0

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/ndh \
  hl7.fhir.r4.core#4.0.1 hl7.fhir.uv.xver-r5.r4#0.1.0 \
  hl7.fhir.us.core#6.1.0 hl7.fhir.uv.extensions.r4#5.3.0 \
  hl7.fhir.uv.subscriptions-backport.r4#1.1.0

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/pas \
  hl7.fhir.r4.core#4.0.1 hl7.fhir.uv.xver-r5.r4#0.1.0 \
  hl7.fhir.us.core#7.0.0 hl7.fhir.us.core.v610#6.1.0 \
  hl7.fhir.us.core.v311#3.1.1 hl7.fhir.uv.sdc#4.0.0 \
  hl7.fhir.us.davinci-hrex#1.2.0 hl7.fhir.us.davinci-crd#2.2.1 \
  hl7.fhir.us.davinci-cdex#2.1.0 hl7.fhir.uv.subscriptions-backport#0.1.0 \
  hl7.fhir.uv.tools.r4#1.1.2 hl7.fhir.uv.extensions.r4#5.3.0-ballot-tc1

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/mhd \
  $(node snapshot/package-deps.cjs --cache temp/fhir-home/.fhir/packages ihe.iti.mhd#4.2.5-comment)

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/eu-eps \
  $(node snapshot/package-deps.cjs --cache temp/fhir-home/.fhir/packages hl7.fhir.eu.eps#1.0.0-ballot)

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/eu-mpd \
  $(node snapshot/package-deps.cjs --cache temp/fhir-home/.fhir/packages hl7.fhir.eu.mpd#1.0.0)

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/au-ps \
  $(node snapshot/package-deps.cjs --cache temp/fhir-home/.fhir/packages hl7.fhir.au.ps#1.0.0-preview)

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/pacio-toc \
  $(node snapshot/package-deps.cjs --cache temp/fhir-home/.fhir/packages hl7.fhir.us.pacio-toc#1.0.0)

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/dapl \
  $(node snapshot/package-deps.cjs --cache temp/fhir-home/.fhir/packages hl7.fhir.us.dapl#1.0.0-ballot)

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/us-core \
  $(node snapshot/package-deps.cjs --cache temp/fhir-home/.fhir/packages hl7.fhir.us.core#9.0.0)

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/ipa \
  $(node snapshot/package-deps.cjs --cache temp/fhir-home/.fhir/packages hl7.fhir.uv.ipa#1.1.0)

bash snapshot/check-harvested-r4.sh snapshot/harvested/r4/qicore \
  $(node snapshot/package-deps.cjs --cache temp/fhir-home/.fhir/packages hl7.fhir.us.qicore#6.0.0)
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

- Holdout-IG corpus expansion (2026-06-30): harvested six previously-unseen IGs
  from the stock-SUSHI outputs under
  `/home/jmandel/hobby/sushi-rs/temp/holdout/<ig>-stock/...` and gated Rust against
  the Java native-R5 oracle. Results: **CARIN BB 6/6**, **Genomics Reporting 33/33**,
  **DTR 21/21**, **eCR 28/28**, **NDH 50/50**, **PAS 73/73**. Missing dependency
  packages (bfdr/odh/ph-library/cql/crmi for eCR, davinci-pas/subscriptions-backport
  for PAS) were hardlinked into the isolated cache from the main repo's cache,
  never `~/.fhir`. Generator behaviors discovered/fixed while closing these:
  - Package loaders cannot trust `.index.json` to be populated. PAS's
    `hl7.fhir.uv.subscriptions-backport.r4#1.1.0` package has an empty index despite
    top-level StructureDefinition JSON files, so both the Java R4 oracle and
    Rust `PackageContext` fall back to scanning top-level package JSON when no
    StructureDefinitions are indexed.
  - The older SUSHI-harvested PAS goldens match
    `hl7.fhir.uv.subscriptions-backport#0.1.0` for `backport-subscription`;
    official published-package PAS uses
    `hl7.fhir.uv.subscriptions-backport.r4#1.1.0` and is covered separately
    below.
  - `constraint.source` is stamped with the base/source-SD URL during the
    per-element merge (`updateFromDefinition` ~PU:3085) for base constraints that
    lack a source — but only for merged (differential-touched) elements. CARIN BB
    `Organization.identifier:NPI` stamps `us-core-organization`; CRD leaves the same
    `us-core-16..19` sourceless because it only touches the unsliced parent. The old
    `preserves_missing_constraint_source` hack is gone from the merge path (kept on
    the projection path).
  - Inherited unsliced datatype children under a sliced anchor are dropped only when
    the base was already sliced AND the differential constrains none of them. Base
    unsliced (CARIN BB `Patient.identifier`, introduces slicing) keeps them; base
    sliced + no unsliced-child diff (CRD `Practitioner.identifier`) prunes; base
    sliced + unsliced-child diff (NDH `Organization.identifier.assigner`) keeps.
    The same rule applies when merging a profiled datatype onto an existing sliced
    anchor (PAS `Practitioner.identifier` prunes inherited unsliced Identifier
    children before the NPI slice).
  - An anchor-only slicing/cardinality row is not an unsliced-child constraint.
    TWPAS `Patient.identifier` / `Practitioner.identifier` prune inherited
    unsliced Identifier children even though the differential touches the anchor.
    Newly introduced `Coding` slicing is the counterexample: AU Core
    `Medication.code.coding` and AU Core medication choice `*.coding` keep the
    unsliced `coding.*` branch alongside `coding:pbs/amt.*`.
  - The profiled-resource root overlay copies short/definition/comment/requirements/
    alias/mapping always, but condition/isModifier/isModifierReason/isSummary only
    when the element narrows to a **single** profiled type. Multi-typed
    `Parameters.parameter.resource` (DTR order.resource, 9 CRD profiles) keeps its
    inherited inv-1/isSummary.
  - Local-base chains without stored snapshots are generated on demand
    (`structure_with_r4_snapshot`, plus a `profile_root_element` full-snapshot
    fallback) so a profiled-type root like DTR `dtr-questionnaireresponse-adapt`
    resolves. PAS bundle resource slots also need this so local
    Claim/ClaimResponse derivatives over local base profiles overlay the final
    profiled root doco/alias/mappings.
  - Extension-root cardinality overlay constrains but does not widen inherited
    cardinality. PAS `Subscription.channel.payload.extension:content` keeps its
    inherited `1..1` from `backport-subscription` even though the extension profile
    root is generic `0..*`.
  - Extension-slice `isSummary` is never stripped by the root overlay (it keeps the
    inherited value: none for stored us-core birthsex, false for a fresh clone).
  - `checkExtensionDoco` normalizes an extension profile's untouched root
    (short=Extension, definition="An Extension", clearing comment/req/alias/mapping)
    — but only for the top-level profile, never a dependency extension consumed as a
    slice/overlay source. Gated by `SnapshotOptions.apply_extension_root_doco`
    (true only at the public entry point).
  - Markdown known-relative links (workflow-extensions.html#instantiation,
    questionnaire.html, rendering-markdown/xhtml, itemWeight) stay relative only in
    freshly-applied extension-root doco; inherited copies are rewritten to the spec
    URL. Empty-string `slicing.description` is dropped (R4->R5 parse drops empty
    primitives).
  - Mapping merge suppresses duplicates by semantic `identity + map`, not by full
    object equality; NDH has duplicate map rows that differ only by comment.
  - Current Publisher-path oracle behavior keeps normal dependency extension root
    doco and root `condition` for NDH `artifact-description` /
    `artifact-effectivePeriod`; do not preserve the older `{{title}}` placeholder
    or generic `"An Extension"` fixture behavior.
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
- Real R4 CARIN BB coverage (2026-06-30): harvested 6 constraint
  StructureDefinitions. Package context must include R4 core, US Core 7.0.0,
  extensions.r4 5.3.0, and the local CARIN BB dev package. The Publisher-path
  native R5 Java oracle generated **6/6** goldens, and Rust now matches **6/6**.
- Real R4 DTR coverage (2026-06-30): harvested 21 constraint
  StructureDefinitions. Package context must match the DTR command above,
  including `hl7.fhir.uv.extensions.r4#5.3.0-ballot-tc1` and
  `hl7.fhir.us.davinci-pas#2.2.1`; using plain extensions `5.3.0` or the
  cached DTR package `2.1.0` dependency closure is a false context and causes
  diffs such as the qpackage input parameter profiles. The Publisher-path native R5 Java oracle generated
  **21/21** goldens, and Rust now matches **21/21**.
- Real R4 eCR coverage (2026-06-30): harvested 28 constraint
  StructureDefinitions. Package context must match the eCR command above,
  including US Core 6.1.0, VSAC 0.23.0, PHIN VADS 0.12.0, BFDR 2.0.0,
  ODH 1.3.0, PH Library 2.0.0-snapshot, CQL 2.0.0, CRMI 1.0.0,
  and xver-r5.r4 0.1.0.
  The Publisher-path native R5 Java oracle generated **28/28** goldens, and
  Rust now matches **28/28**.
- Real R4 NDH coverage (2026-06-30): harvested 50 constraint
  StructureDefinitions. Package context must match the NDH command above. The
  Publisher-path native R5 Java oracle generated **50/50** goldens, and Rust
  now matches **50/50**.
- Real R4 PAS coverage (2026-06-30): harvested 73 constraint
  StructureDefinitions after skipping 8 local-base/specialization profiles.
  Package context must match the PAS command above; the current goldens use
  `hl7.fhir.uv.subscriptions-backport#0.1.0` rather than
  `hl7.fhir.uv.subscriptions-backport.r4#1.1.0`. The Publisher-path native R5
  Java oracle generated **73/73** goldens, and Rust now matches **73/73**.
- Published-package R4 coverage (2026-07-02): package harvesting now gates
  **PDDI 1/1**, **MHD 42/42**, **DeID 1/1**, **EU EPS 23/23**, **EU MPD 4/4**,
  **AU Core 2.0.0 26/26**, **AU PS 17/17**,
  **gematik ePA medication 49/49 usable profiles**,
  **Belgium Vaccination 7/7**, **PACIO TOC 4/4**, **DARTS 1/1**, and
  **DAPL 26/26**. Additional package harvests: **Radiation Dose Summary 4/4**,
  **Taiwan PAS 43/43** after skipping 3 specialization profiles, **US Core
  9.0.0 70/70**, **IPA 1.1.0 12/12**, and **QI-Core 6.0.0 63/63 usable
  profiles**, plus **SMART App Launch 2.2.0 6/6** and **Da Vinci CDex 2.1.0
  8/8**, **Da Vinci PDex 2.1.0 37/37**, **Da Vinci Plan Net 1.2.0 22/22**,
  **Da Vinci Drug Formulary 2.1.0 19/19 usable profiles**,
  **Da Vinci PAS 2.2.1 80/80 usable profiles**, and
  **Subscriptions Backport R4 1.1.0 9/9 usable profiles**.
  QI-Core has 65 harvested profile fixtures, but the Java oracle fails on
  `qicore-devicerequest` and `qicore-devicenotrequested` because the package
  references the invalid versioned R5-backport extension URL
  `http://hl7.org/fhir/5.0/StructureDefinition/extension-DeviceRequest.doNotPerform`.
  Drug Formulary has 20 harvested fixtures, but Java oracle generation fails
  on `insurance-plan-coverage` because the package references invalid
  `http://hl7.org/fhir/5.0/StructureDefinition/extension-Coverage.insurancePlan`.
  Subscriptions Backport R4 has 11 harvested fixtures, but Java oracle
  generation fails on `backport-subscription-notification` because it constrains
  `Bundle.entry.resource` to R5 `SubscriptionStatus`, and on
  `backport-subscription` because it changes
  `Subscription.criteria.extension.isSummary` from false to true.
  **CDS Hooks 3.0.0-ballot 0/0** because the package contains no R4
  StructureDefinition profile JSONs.
  **SAFR 0/0**, **Bulk Data 2.0.0 0/0**, **Application Feature 0/0**,
  **IHE VHL 0/0**, **C-CDA R5.0 0/0**, **C-CDA on FHIR 0/0**,
  **PH Query 0/0**, and **Order Catalog 0/0** because those packages have no
  R4 constraint StructureDefinitions reachable by this harvest path. gematik had a larger
  scanned set, but Java oracle generation only
  produced the 49 usable profiles because several package resources reference
  invalid R5-backport URLs. Belgium Vaccination harvested 7/9 after skipping
  non-constraint/specialization resources. PACIO and AU PS require the R4-compatible
  dependency filter because their dependency closures otherwise pull R5 packages
  into the R4 oracle and make core datatypes ambiguous.
- Final docs-list audit (2026-07-02): every IG in main's
  `docs/igs-to-test-with*.json` has a `snapshot/harvested/r4/<key>/manifest.json`.
  All harvested profiles either have Java native-R5 goldens that Rust matches, or
  are one of the documented Java-oracle failures above. The zero-profile entries
  were checked against package contents: Application Feature, C-CDA R5.0, and
  Order Catalog are R5-only; Bulk Data, C-CDA on FHIR, CDS Hooks, PH Query, SAFR,
  and VHL contain no R4 `StructureDefinition` constraints in their published
  package material.
- Published-package dependency closures are not always enough to reproduce older
  SUSHI-harvested goldens. IPS needs the Publisher high-auto extensions package
  for core extension roots such as `translation` and `allergyintolerance-abatement`.
  DTR goldens match the explicit command above with SDC 4.0.0 and ballot-tc1
  extensions; blindly using package-declared SDC 3.0.0 causes false diffs in
  DTR Bundle/resource overlays and SDC-inherited children.
- Native R5 internal output for R4 resources keeps `fhirVersion: "4.0.1"` but
  follows R5 model conversion behavior, e.g. R4 constraint `xpath` fields are
  absent.
- Java local-dir converted resources and package-loaded resources are not
  identical sources of truth for every annotation. Local R4 resources converted
  through the wrapper can preserve `xpath` as native R5 constraint extensions;
  package-loaded core/dependency snapshots often arrive without those converted
  extensions. Treat fixture source (local-dir vs package context) as part of the
  oracle input.
- Local extension profile roots can differ between standalone snapshot output
  and consuming slice overlay. Standalone TWPAS `extension-claim-encounter`
  omits R4 `constraint.xpath` in native-R5 output, but a Claim
  `extension:encounter` slice overlays the local extension root and preserves
  the R4 root `ele-1`/`ext-1` xpaths as native-R5
  `ElementDefinition.constraint.xpath` extensions.
- `NON_INHERITED_ED_URLS` in Rust intentionally mirrors
  `org.hl7.fhir.r5.conformance.profile.ProfileUtilities.NON_INHERITED_ED_URLS`
  and is R5-only. R4 Java does not strip the same inherited binding extensions;
  the R4 Patient fixture keeps `elementdefinition-isCommonBinding`.
  Keep odd-looking URLs like `elementdefinition-isCommonBinding`,
  `obligation-profile`, `structuredefinition-standards-status-reason`, and
  `structuredefinition-summary`; Java strips these from inherited
  ElementDefinitions/bindings in R5.
- Publisher-native R4 projection strips most inherited non-inherited metadata
  extensions even though the resource is internally R5. Two observed exceptions
  are tied to semantic element annotations: `elementdefinition-isCommonBinding`
  can survive on a binding when the ElementDefinition carries semantic
  extensions and a fixed/pattern value (QI-Core
  `Coverage.identifier:memberid.type` with USCDI), and
  `structuredefinition-explicit-type-name` can survive alongside semantic
  extensions on BackboneElement rows (QI-Core Coverage class slices). Untouched
  core rows such as `*.language` still strip `isCommonBinding`. Display-only
  annotations such as `structuredefinition-display-hint` do not count as
  semantic for this exception; PDex `ExplanationOfBenefit.addItem` keeps the
  display hint but strips inherited `structuredefinition-explicit-type-name`.
- Java keeps duplicate inherited extensions on unconstrained elements, but a
  constrained element merge can collapse exact duplicate extension values. The
  Questionnaire fixture exercises this with duplicate
  `elementdefinition-translatable` extensions.
- Differential-owned extensions are appended after inherited duplicates are
  collapsed, but exact duplicate merge behavior is URL-sensitive. Java preserves
  duplicate US Core `uscdi-requirement` extensions when both inherited and
  differential-owned (QI-Core RelatedPerson telecom/address), while exact
  duplicates of `elementdefinition-translatable`, `structuredefinition-display-hint`,
  and `qicore-keyelement` collapse.
- Inherited `ElementDefinition.type.extension` values with
  `structuredefinition-hierarchy` are not propagated into derived type entries
  (QI-Core Encounter/Location/Organization/Patient self-reference targets).
  Obligation/type MS extensions still merge separately.
- Existing direct datatype slices may need child materialization when their slice
  group is constrained by a derived profile, not only when the slice is newly
  inserted. QI-Core Practitioner materializes inherited `identifier:NPI.*`
  children when adding/constraining identifier slices. Coding slices are
  context-sensitive: CDex `Task.meta.tag:work-queue` materializes children when
  a bound Coding slice is paired with unsliced `system`/`code` constraints; AU
  Core medication `coding:pbs/amt` materializes children when a repeatable
  bound Coding slice carries semantic obligation/MS extensions; capped
  `max=1` Coding slices such as AU Core immunization and CRD/PAS `$this`
  value-sliced codings stay childless.
- Direct-slice child `mustSupport` propagation depends on current-differential
  ownership of the unsliced source child as MS/obligation semantics, not merely
  on any diff row. US Core's own `identifier.system MS` propagates to
  `identifier:NPI.system`; AU Core obligation rows preserve inherited MS on
  `Observation.category:VSCat.coding.*`; QI-Core inherits base MS but only adds
  `qicore-keyelement`, so materialized `identifier:NPI.system/value` drop MS.
  A must-support direct slice root can also preserve/add MS on constrained
  descendants when the unsliced base ancestry was already MS: TWPAS
  `Composition.section:subjective.code`, nested `code.coding.system/code`, and
  `entry` inherit MS from the Twcore `Composition.section`/`code`/`entry`
  context. The same rule must not apply to `Identifier.use` under TWPAS Claim
  identifier slices, because core `Claim.identifier` lacks inherited MS context.
  Fixed coding terminology children such as AU Core vital sign
  `Observation.code.coding:snomed*.system/code` do not inherit MS just because
  the ancestor `Observation.code` is MS; pattern-valued children can inherit.
  Comment-only slice descendants can keep inherited MS (CARIN BB
  `Coverage.class:group/plan.value`), but pattern `identifier.type` rows do
  not absorb MS from the slice root unless the differential says so (CARIN BB
  Patient identifier slices); their materialized `identifier.system/value`
  children do preserve MS when the identifier slice root is differential-MS.
  The same rule applies under recursive/contentReference slices: IPS
  Composition section children keep MS when `Composition.section.code MS` is in
  the current differential. Existing inherited slice descendants that are
  explicitly present in the differential keep their inherited MS even when the
  diff row is MS-silent (PDex `Provenance.agent:ProvenanceAuthor.type`), but
  newly materialized slice children still use the stricter current-differential
  source-child rule.
- Recursive `contentReference` expansion follows Java
  `replaceFromContentReference`: copy only the referenced element's `type` to
  the referring element, remove `contentReference`, then clone referenced
  descendants under the recursive id/path. Relative child contentReferences are
  canonicalized to the source StructureDefinition URL, e.g.
  `http://hl7.org/fhir/StructureDefinition/Questionnaire#Questionnaire.item`.
  Expansion is additive: if some descendants already exist under the referring
  element, Rust must still clone the missing referenced descendants without
  duplicating existing rows (PDex `ExplanationOfBenefit.addItem`).
- If a differential supplies `type[]` for an inherited `contentReference` row,
  the final row drops `contentReference`. CDex `Parameters.parameter.part` uses
  this when constraining a content-reference child to `Attachment`. New slices
  cloned from an original contentReference anchor must also inherit the current
  anchor's resolved state: when the same differential has converted the anchor
  to a real `BackboneElement`, inserted slices drop `contentReference` and copy
  the resolved `type` (PDex `ExplanationOfBenefit.adjudication` slices).
  Existing contentReference rows need the same pre-merge unfold: if the row
  already exists and the differential supplies `type[]`, clone any missing
  referenced descendants before dropping `contentReference`. Drug Formulary
  `GraphDefinition.link:formulary.target.link.target.link` exercises the
  recursive case.
- When inserting a slice under a contentReference anchor, Java keeps
  `contentReference` on the anchor but the new slice root itself drops the
  copied `contentReference` and gets the referenced target's `type`.
  DARTS `Parameters.parameter:data.part:resourceType` exercises this.
- Relative inherited contentReferences from HL7 core profiles may canonicalize
  to the fragment's root type rather than the immediate base profile URL. PACIO
  `TOC-Composition` over `clinicaldocument` keeps
  `http://hl7.org/fhir/StructureDefinition/Composition#Composition.section`, not
  `.../clinicaldocument#Composition.section`.
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
  `workflow-extensions.html#instantiation`. DTR adds
  `OperationDefinition-Questionnaire-assemble.html`,
  `StructureDefinition-sdc-questionnaire-subQuestionnaire.html`,
  `operational.html#guidelines-for-estimated-time-to-complete-a-dtr-questionnaire`,
  and `extraction.html` as relative links. eCR/PH-library adds a quirk where
  `StructureDefinition-us-ph-composition.html` rewrites to the unversioned
  absolute `http://hl7.org/fhir/StructureDefinition-us-ph-composition.html`.
  Current Publisher-path output keeps the SDC variable comment's `2025Jan` link
  in both eCR PlanDefinition and DTR Questionnaire contexts.
- Binding overlay fixtures should include `strength` when setting
  `binding.description`; Java can throw a null dereference for a
  description-only binding differential on a required base binding.
- Inherited placeholder `comment: "-"` is stripped during native R5 projection
  unless the current differential explicitly owns that placeholder. CDex
  `QuestionnaireResponse.subject` drops the inherited placeholder; US Core and
  QI-Core rows that declare `comment: "-"` keep it.
- Extension root and extension child doco are normalized by Java
  `checkExtensionDoco`: short/definition become generic extension text and
  comment/requirements/alias/mapping are removed before derived rules apply.
- For an extension/modifierExtension slice whose profile URL is not resolvable
  in the R4 context, Publisher still applies generic extension slice doco
  cleanup: definition becomes `"An Extension"` and comment/requirements/alias/
  mapping are removed, while differential-owned short text is kept. SMART App
  Launch `organization-brand`, `organization-portal`, and `endpoint-fhir-version`
  slices exercise this missing-profile path.
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
- Direct-slice child unfolding does not copy same-differential generated child
  slices sideways for ordinary non-recursive anchors: official Da Vinci PAS
  `Claim.supportingInfo.extension:infoChanged` does not become
  `Claim.supportingInfo:AdditionalInformation.extension:infoChanged`. Recursive
  anchors are the counterexample: AU PS `Composition.section` copies
  same-differential `Composition.section.extension:section-note` into each
  section slice because the section subtree content-references back to
  `Composition.section`.
- Extension/modifierExtension child anchors under non-recursive direct slices
  clone the original/base extension doco rather than same-differential generic
  slicing-anchor doco (official PAS `Claim.supportingInfo:* .modifierExtension`).
  Recursive anchors keep the current generic extension anchor doco (AU PS
  `Composition.section:sectionProblems.extension`), while the concrete generated
  extension slice keeps its profiled doco (`...extension:section-note`).
- Slice-descendant diffs layer over matching unsliced differential rows from
  the same sliced context. A row such as
  `InsurancePlan.plan:drug-plan...cost.qualifiers` sets min/max/MS, then
  `...cost:copay.qualifiers` adds a binding; Java applies both to the slice
  child. Match this by treating earlier rows with lower-level slice markers
  removed as generalizations, while preserving higher-level slice context
  (`plan:drug-plan`). This overlay is for descendants below a slice, not the
  direct slice root itself; AU PS medication choice slices must not absorb the
  same-differential `medication[x]` comment. Drug Formulary
  `usdf-PayerInsurancePlan` exercises the descendant overlay.
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
  inherited/base constraints and omit root `condition`. Current Publisher-path
  output keeps root `condition` for `workflow-supportingInfo`; PH-library
  `us-ph-named-eventtype-extension` still omits it.
  Local-dir context matters for root constraint XPath projection: package-loaded
  local extension profiles that already carry snapshots can project base
  `constraint.xpath` into native-R5 constraint extensions on consuming slices
  (published PDex `extension-levelOfServiceCode`), while SUSHI-generated local
  profiles with only differentials are generated in the native-R5 helper path
  and do not add XPath extensions to copied base constraints (CRD/PAS).
  Observed exceptions still need root-cause cleanup:
  `mcode-histology-morphology-behavior`, core `condition-related`,
  `alternate-reference` condition omission, core `codeOptions`, and versioned
  `artifact-versionAlgorithm|5.2.0` generic Extension doco differ from the
  general overlay rule.
- eCR `cqf-fhirQueryPattern` inserted under PlanDefinition materializes
  extension-profile children even without explicit descendant differentials.
  In the source-harvested eCR context the canonical is not loaded in the package
  set, but native Publisher output still overlays the current UV extensions root
  doco and profile children: root short/definition/comment come from the
  5.2+/5.3 `cqf-fhirQueryPattern` extension, `url` is `type=uri` with
  `fixedUri=http://hl7.org/fhir/StructureDefinition/cqf-fhirQueryPattern` and
  `mustSupport=true`, and `value[x]` is required `string`.
- eCR PlanDefinition recursive action handling has observed Publisher quirks:
  first-level `PlanDefinition.action(.slice)?.relatedAction.offset[x]` inferred
  type-slice anchors are closed and copy the `offsetDuration` definition, while
  nested recursive action slices stay open. Nested `checkSuspectedDisorder` and
  `checkReportable` action rows get MS/binding/max/min stamps, and nested
  DataRequirement filter constraint sources reset to the core PlanDefinition
  URL for `drq-1`/`drq-2`. Unsliced nested `*.action.trigger` gets `id` and
  `extension` children, but nested direct action slices do not clone
  `trigger.id` / `trigger.extension`.
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
- Native R5 projection strips inherited non-inherited ElementDefinition
  extensions, but differential-owned occurrences must survive. Additional
  binding conversion is likewise limited to differential-owned binding
  extensions; inherited additional-binding fodder should not be projected into
  the derived binding. For R4 concrete choice aliases, the differential's
  `Observation.valueQuantity.code` additional binding maps to the native
  `Observation.value[x]:valueQuantity.code` row (AU Core vitals). Nested
  additional-binding child extensions `usage` and `any` project to native
  `binding.additional[].usage[]` and `binding.additional[].any`; CDex
  `Task.input:AttachmentsNeeded` exercises both.
- A differential binding on a non-bindable final element type is dropped, even
  when the differential explicitly carries the binding. CDex puts an additional
  binding on `Task.input:AttachmentsNeeded` (`BackboneElement`); Publisher omits
  it from the slice root rather than preserving a binding on a non-bindable row.
- Publisher-native R4 projection of `Extension.value[x]` should remove R5-only
  value datatypes, but the R4 allow-list includes `Contributor`. US Core root
  extension profiles over R4 core keep `Contributor`; AU Core
  `au-core-rsg-sexassignedab` stays at 49 types because its
  `individual-recordedSexOrGender` base snapshot already lacks `Contributor`.
- Obligation `snapshot-source` augmentation is recursive. Publisher adds
  `snapshot-source` not only to element-level obligation extensions but also to
  obligation extensions nested under `ElementDefinition.type.extension`. AU PS
  onset/occurrence/performed choice elements exercise this.
- When a differential replaces/constrains `type[]`, Java merges extensions on
  matching inherited type codes. Derived type extensions come first, then
  inherited non-obligation type metadata such as
  `elementdefinition-type-must-support`, then inherited obligations. Do not
  replace `type[]` wholesale or the AU Core obligations on AU PS choice elements
  disappear.
- Type-slicing `rules` are not globally normalized. Publisher keeps most R4
  open `$this` choice slicing open (IPS timing/effective/value[x], CRD
  Timing/NutritionOrder), but CRD's Reference-vs-CodeableConcept choice slices
  (`DeviceRequest.code[x]`, `MedicationRequest.medication[x]`) come out closed.
  PAS date/Period choice slices with descendant date rules also close, while
  SDC `UsageContext.value[x]` CodeableConcept/Range descendant slicing stays
  open. Exact `[x]` rows that are not descendant-unfolded can remain open.
  Nested choice anchors below an already-sliced parent also keep explicit open
  slicing even when constrained to Reference+CodeableConcept; Plan Net
  `Extension.extension:whereValid.value[x]` exercises this.
- Type-slice group ordering follows current differential order for direct
  slices owned by the derived profile; inherited type-slice groups come after
  current-differential groups. Do not sort by the anchor's raw type order:
  EU EPS needs the differential's `effectiveDateTime` before inherited
  `effectivePeriod`, while IPS lab value slicing must keep the order Java emits.
- Extension `value[x]` type slicing is a narrow exception to the open-choice
  rule: when a direct nonzero slice narrows `Extension.value[x]`, Publisher
  prunes the anchor type list to the live slice type(s) and closes the slicing
  even if the differential says open. PACIO `point-of-contact-extension`
  exercises this. Do not generalize this to Observation/Timing choice slicing.
- When a non-Backbone datatype element is sliced, Java removes inherited
  unsliced datatype children under that anchor before inserting the slices
  (CRD `Practitioner.identifier`). Backbone slices keep their unsliced children.
- Existing sliced `Coding` anchors under `CodeableConcept.coding` also prune the
  unsliced `coding.*` branch when a derived profile touches the inherited
  slices but does not constrain unsliced descendants. AU PS medication profiles
  over AU Core keep `coding:pbs.*` and `coding:amt.*` but drop unsliced
  `coding.id/extension/system/version/code/display/userSelected`.
- Unsliced differential rows must not fall back to sliced descendants just
  because `path` matches and `sliceName` is absent. Taiwan PAS
  `Encounter.serviceType.coding.code` must unfold/merge the unsliced
  `coding.*` branch even when inherited `coding:<slice>.code` rows already
  exist. The fallback matcher therefore excludes candidate ids containing slice
  markers when the differential id itself is unsliced.
- Profiled resource-root overlay for sliced `.resource` entries depends on the
  target profile source. Stored package snapshots (Taiwan PAS, AU PS) overlay
  the profiled resource root. Local differential-only profiles with an explicit
  root differential can require the base resource root plus profile-only
  mappings (DTR `dtr-base-questionnaire`), while local differential-only profiles
  without a root differential need the generated local profile root. Root
  overlay must not overwrite keys explicitly supplied on the current differential
  slice; CDex contained `Task.contained` resource slices keep their diff-owned
  short/definition.
- Final native R5 projection must preserve constraints that are explicitly
  sourceless in the R4 differential. Java stamps inherited/base constraints
  during merge when the element is touched, but DAPL date/Period constraints
  (`dapl-*`) and IPS-family constraints that originate sourceless in the
  differential remain sourceless in the final snapshot. Track this by
  `(element id/path, constraint key)`, not by a growing list of IG-specific keys.
- R4 choice-specific differential paths are canonicalized to Publisher internal
  `[x]` paths before merging. A direct choice row such as
  `Observation.effectiveDateTime` becomes the type slice
  `Observation.effective[x]:effectiveDateTime`; a child row such as
  `Observation.valueRange.extension:dataAbsentReason` maps to
  `Observation.value[x].extension:dataAbsentReason` rather than introducing a
  separate `valueRange` anchor. DAPL income observation exercises both cases.
- When a differential introduces slicing on an extension/modifierExtension
  anchor, Java also applies generic extension doco cleanup to the anchor
  (short/definition generic, comment/requirements/alias/mapping removed).
  Extension anchors under primitive/scalar parents use the `ordered:false`
  slicing shape, while complex parents generally keep the descriptive slicing
  text. DAPL postalCode/canonical extensions exercise the scalar path; many IPS
  and AU PS complex/reference anchors exercise the descriptive path.
- Extension profile root overlay includes root cardinality. Derived `max="*"`
  must not widen an inherited/profile `max="1"` after the overlay. Publisher
  does not omit root `condition` merely because an extension slice is prohibited
  with `max=0` (NDH Practitioner keeps `ele-1`).
- Slice min propagation from direct slice children back to the sliced anchor is
  applied when a derived profile adds/constrains slices under an inherited
  slicing context, but not when the current differential owns the slicing anchor
  itself. PDDI needs `Extension.extension.min=4`, and AU Core blood pressure
  needs `Observation.component:SystolicBP.code.coding.min=2`; the R5 Patient
  identifier fixture keeps `Patient.identifier.min=0` because its differential
  defines the `Patient.identifier` slicing entry.
- Extension root overlay trims trailing whitespace on copied text fields
  (`short`, `definition`, `comment`, `requirements`) when applying root text to
  an extension slice. Belgium Vaccination has a slice definition with a trailing
  space on the extension profile root; the slice overlay is trimmed while the
  extension profile root itself keeps the original text.
- Profiled-resource root overlay copies root `mustSupport` only for a single
  profiled `.resource` type. PACIO Bundle entry resource slices copy
  `mustSupport=false` from single US Core/ADI profile roots, but a multi-type
  resource slot such as procedures (Procedure or ServiceRequest) does not get a
  synthetic `mustSupport=false`. Merge-time slice-descendant MS cleanup must not
  strip an explicit `mustSupport=false` projected from a profiled resource root;
  PDex multi-member-match Parameters keeps false on Patient/Coverage resource
  slots while the unrelated Consent slot remains absent.
- Unfolded type constraints use the containing/base StructureDefinition URL as
  `constraint.source`, not the raw datatype URL, for non-`ele-1`/`ext-1`
  constraints. This applies for non-core base profiles (DTR Narrative
  descendants with `txt-1`/`txt-2`) and core bases (EU MPD
  `Dosage.timing.repeat` rehomes `Timing` `tim-*` constraints to core
  `Dosage`).
- A direct slice row with no differential extension keeps inherited/profiled
  obligation extensions. Only explicit slice descendants without their own
  extension drop inherited obligations. EU MPD
  `MedicationRequest.dosageInstruction.doseAndRate.dose[x]:doseQuantity`
  inherits the profiled Dosage obligation extension from `Dosage-eu-mpd`.
- A direct slice row with its own obligation extensions drops unprovenanced
  obligations copied from the current differential's unsliced anchor, but keeps
  source-marked inherited/profile obligations. EU EPS obligation profiles
  exercise the drop on direct `[x]` type slices such as
  `AllergyIntolerance.onset[x]:onsetDateTime`,
  `Condition.onset[x]:onsetDateTime`, and
  `Immunization.occurrence[x]:occurrenceDateTime`; AU PS direct slices keep
  inherited AU Core obligations plus AU PS obligations.
- R4 choice-specific paths need context-sensitive canonicalization. A direct
  choice row such as `Observation.valueQuantity` becomes a type slice
  `Observation.value[x]:valueQuantity`, and descendant rows under that direct
  choice root rehome under the same slice id
  (`Observation.value[x]:valueQuantity.value`). But a choice row under an
  already-sliced parent, e.g.
  `Observation.component:industry.valueCodeableConcept`, narrows
  `Observation.component:industry.value[x]` directly rather than creating a
  nested `:valueCodeableConcept` type slice. US Core social/pregnancy and vital
  profiles exercise both shapes.
- Direct datatype slices may need eager child cloning even with no
  slice-specific descendant differential rows, but not merely because a direct
  slice exists. Java materializes Identifier slice children when a nonzero
  fixed/pattern Identifier slice is paired with unsliced `identifier.system` or
  `identifier.value` constraints in the current differential (QI-Core
  `Practitioner.identifier:NPI`). It does not materialize root-only/max=0
  inherited slices (NDH `Organization.identifier:NPI|CLIA`) or pattern slices
  lacking those system/value constraints (NDH `identifier:TID`). Do not eagerly
  clone BackboneElement/contentReference slice children; Bundle/Composition
  section slices must unfold lazily when the relevant child differential is
  merged, after anchor children such as `Composition.section.extension:section-note`
  have been introduced. If an inherited direct datatype slice row is touched
  only by `id/path/sliceName/mustSupport`, Java does not eagerly clone its
  datatype children; PDex `Organization.identifier:CLIA|NAIC` stays childless
  while QI-Core semantic extension/constraint cases still materialize children.
- A new direct slice with no explicit `min` starts at `0` even when the unsliced
  anchor is required (`Provenance.agent:ProvenanceAuthor` over
  `Provenance.agent 1..*`). Explicit slice mins still win.
- A new direct slice starts with the original/base condition set for its slice
  or anchor; conditions added to the slicing parent by the same differential do
  not leak into the new slice. CDex `Task.reasonCode.coding:use` drops the
  same-differential `cdex-1`, while AU Core `Organization.identifier:hpio`
  keeps inherited `org-1`, AU PS
  `Composition.section:sectionProblems.entry:problem` falls back to the fully
  unsliced `Composition.section.entry` inherited `cmp-2`, and AU PS
  `Observation.value[x]:valueCodeableConcept` keeps inherited `obs-*`/AU Core
  conditions. If a nested anchor was unfolded from a contentReference or complex
  type and has no original/base slice row, a new direct slice may inherit the
  current anchor condition unless that anchor's condition is owned by the same
  differential. MHD
  `Parameters.parameter:operation.part:path.value[x]:valueString` keeps `inv-1`.
- Type extensions cloned into sliced-parent choice children are base-owned only:
  if the original base child carried `ElementDefinition.type.extension`
  (`vitalsigns` component value), the slice child keeps it; if the extension was
  introduced by the current differential on the unsliced anchor (US Core average
  blood pressure), Java does not propagate it into the slice child.
- Copied slicing blocks get Java's implicit fill-ins selectively: choice/type
  slicing on `[x]` rows gets `ordered:false` when omitted, but resource
  containment type slicing such as CDex `Task.contained` leaves `ordered` absent.
  Top-level `Extension.extension` URL slicing gets the standard extension
  slicing description, but explicit
  `DomainResource.extension` slicing such as US Core FamilyMemberHistory does
  not get that description.

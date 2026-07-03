# Layer B Audit — Canonical Version Pinning & the R4-Artifact Projection

> Status: AUDIT (task #17, 2026-07-03). READ-ONLY spec task; no code changed.
> Oracle version: **IG Publisher 2.2.10-SNAPSHOT** (fixture jar
> `periodicity-impl/cycle/input-cache/publisher.jar`, built 2026-06-25; source
> checkout `ig-publisher-tx-hang/fhir-perf/repos/ig-publisher` at git
> `2.2.9-1-gad36e748` "Updating version to: 2.2.10-SNAPSHOT", pom
> `<version>2.2.10-SNAPSHOT</version>`).
> **fhir-core is version-matched separately**: publisher 2.2.10 pins
> `<core_version>6.9.10</core_version>`. All fhir-core citations below are from
> the **6.9.10-SNAPSHOT** checkout
> `/home/jmandel/hobby/fhir-perf/repos/fhir-core`
> (`target/org.hl7.fhir.r5-6.9.10-SNAPSHOT.jar` present). The other checkout
> (`ig-publisher-tx-hang/.../fhir-core`) is 8.4.0 and is NOT used for line
> numbers (mechanism is stable across both, but numbers differ).
>
> **Headline finding (reframes the whole task):** for a plain R4 IG with
> default settings (cycle), the canonical `|4.0.1` decorations we attributed to
> a "Layer-B pinning post-pass" are **NOT** produced by the publisher's
> policy-driven pin pass at all. They are produced by an **unconditional
> core-load pass in fhir-core** (`CoreVersionPinner`) that version-stamps the
> *base/core* definitions before snapshot generation; profile snapshots then
> *inherit* the already-pinned canonicals via ordinary `generateSnapshot`
> element copying. The policy-driven publisher pin pass
> (`pin-canonicals`) is a *separate, orthogonal* mechanism that is **off by
> default** and did not run for cycle. This distinction is the crux of §1 and
> §5.

---

## 0. Empirical ground truth (measured, not inferred)

Measured directly against the freshly-regenerated cycle package.db
(`periodicity-impl/cycle/temp/pages/package.db`, Publisher 2.2.10, R4 IG
`fhirVersion: 4.0.1`, **no `pin-canonicals` parameter** in
`sushi-config.yaml`):

- Profile `menstrual-bleeding` `Resources.Json` snapshot carries:
  - 30 × `snapshot.element[].type[].targetProfile[]` ending in `|4.0.1`
  - 13 × `snapshot.element[].binding.valueSet` ending in `|4.0.1`
  - 2 × `snapshot.element[].type[].profile[]` ending in `|4.0.1`
  - 59 × `constraint.xpath` present
  - **`differential` has ZERO `|4.0.1` and ZERO `xpath`.**
- `period-tracking-fact` (base profile, `baseDefinition` = core
  `http://hl7.org/fhir/StructureDefinition/Observation`, unversioned): 0 pins
  in differential, **45** pins in snapshot.
- The pinned URLs are exactly the *core* canonicals resolvable in-context
  (Patient, Observation, SimpleQuantity, Resource, and core ValueSets incl.
  `.../ValueSet/languages` which is **unversioned in the R4 core source**).
- The R4 core `Observation` **source** (`hl7.fhir.r4.core#4.0.1`) has
  unversioned `binding.valueSet` and unversioned `targetProfile`; only its two
  required-binding `observation-status` valueSets carry `|4.0.1`.

Therefore something *resolves and stamps* canonicals during load — it is not
verbatim inheritance from source, and (see §1) it is not the `pin-canonicals`
pass, which is dormant.

---

## 1. WHERE pinning lives, and the exact rule set

There are **two independent pinning mechanisms**. For the default R4-IG path
(cycle) only mechanism **(A)** fires.

### (A) `CoreVersionPinner` — unconditional, fhir-core, load-time (THE ONE THAT FIRES)

**File:** `fhir-core/org.hl7.fhir.r5/src/main/java/org/hl7/fhir/r5/context/CoreVersionPinner.java` (6.9.10-SNAPSHOT).

**Trigger (unconditional, independent of `pinningPolicy`):**
`BaseWorkerContext.finishLoading(boolean genSnapshots)` —
`BaseWorkerContext.java:3285`:
```java
new CoreVersionPinner(this).pinCoreVersions(listCodeSystems(), listValueSets(), listStructures());
if (genSnapshots) { for (sd : listStructures()) ... generateSnapshot(sd) ... }   // 3286-3298
```
Pinning runs **before** the base-snapshot generation at line 3290, so the
pinned core snapshots are what later profile `generateSnapshot` copies from.
`finishLoading` is invoked from `SimpleWorkerContext.fromPackage(...)` at
`SimpleWorkerContext.java:329`. Runs once per loaded context, over **all**
loaded structures/valuesets/codesystems.

**Traversal** (`pinCoreVersions(List,List,List)`, `CoreVersionPinner.java:17-40`):
walks every CodeSystem (`valueSet`, `supplements`), every ValueSet
(`compose.include/exclude`), and every StructureDefinition —
`sd.baseDefinition` (line 32) **plus every element of BOTH
`differential` AND `snapshot`** (lines 33-38). It is a whole-resource visitor,
but only appends a version when the target canonical *resolves to a versioned
resource in-context*, which is why authored profile differentials (whose
local/unresolved canonicals don't resolve to a versioned core resource, or
already contain `|`) come out clean while inherited core canonicals in
snapshots get stamped.

**Exact per-field rules** (`pinCoreVersions(ElementDefinition)`, lines 42-61):

| Element path | Pinner | Resolves against | Line |
|---|---|---|---|
| `type[].profile[]` | `pinCoreVersionSD` | StructureDefinition | 45, 97-105 |
| `type[].targetProfile[]` | `pinCoreVersionSD` | StructureDefinition | 48, 97-105 |
| `valueAlternatives[]` (R5-only, unused in core) | `pinCoreVersionSD` | StructureDefinition | 53 |
| `binding.valueSet` | `pinCoreVersionVS` | ValueSet | 56, 87-95 |
| `binding.additional[].valueSet` | `pinCoreVersionVS` | ValueSet | 58, 87-95 |
| `StructureDefinition.baseDefinition` | `pinCoreVersionSD` | StructureDefinition | 32 |
| ValueSet `compose.include/exclude[].valueSet` | `pinCoreVersionVS` | ValueSet | 66 |
| ValueSet `compose.include/exclude[].system` (+ set `version`) | inline | CodeSystem | 68-73 |
| CodeSystem `.valueSet`, `.supplements` | `pinCoreVersionVS`/`CS` | VS / CS | 19, 21 |

**The append rule** (identical shape in all three `pinCoreVersion{SD,VS,CS}`,
lines 77-105):
```java
if (ct.hasValue() && !ct.getValue().contains("|") /* && !terminology.hl7.org for VS/CS */) {
  X x = context.fetchResource(X.class, ct.getValue(), getVersionResolutionRules(ct));
  if (x != null && x.hasVersion()) {
    ct.setValue(ct.getValue() + "|" + x.getVersion());          // the pin
    ct.setUserData(VERSION_PINNED_ON_LOAD, true);
  }
}
```

**Which version is chosen:** the `version` of the *resolved target resource*
(`x.getVersion()`) — i.e. the package version the canonical resolves to in the
loaded context. For an R4 IG the core package is `hl7.fhir.r4.core#4.0.1`, so
core canonicals resolve to `|4.0.1`.

**Suppression / guards:**
- already versioned (`getValue().contains("|")`) → skip.
- target does not resolve, or resolves but `!hasVersion()` → skip.
- `pinCoreVersionVS` / `pinCoreVersionCS` additionally skip URLs containing
  `terminology.hl7.org` (THO — versions there are "completely wrong for a
  start", mirroring the publisher-side note). `pinCoreVersionSD` has **no**
  THO guard.
- `contentReference` is FHIR type `uri` in R5 (ElementDefinition.java:6177,
  `/*contentReference*/ return new String[] {"uri"}`), so it is **never**
  visited by any canonical pinner → **not pinned**.
- Extension `url` is `uri` → **not pinned**. (Extension *values* of type
  `canonical`, e.g. `valueCanonical`, would be pinned by mechanism (B) but
  not by `CoreVersionPinner`, which only walks the ED fields above.)

### (B) `CanonicalVisitor` — policy-driven, publisher, post-snapshot (DORMANT for cycle)

**File:** `ig-publisher/org.hl7.fhir.publisher.core/.../publisher/PublisherBase.java`.

**Entry:** `checkCanonicalsForVersions(FetchedFile, CanonicalResource, boolean snapshotMode)` — `PublisherBase.java:766`. Called for SDs at
`PublisherBase.java:913` (immediately after `generateSnapshot`, `snapshotMode=false`)
and for other canonical resources at `PublisherIGLoader.java:4228,4277`.

**Policy gate** (`PublisherBase.java:775`): if
`pinningPolicy == NO_ACTION`, it runs only the `CanonicalVisitorLatest` pass
(which merely *strips* `|*` suffixes and marks `latest`; never appends a
version — `PublisherBase.java:1418-1453`) and returns. **The version-appending
`CanonicalVisitor` at line 780 does not run.**

**Policy source** (`PublisherIGLoader.java:739-753`, IG/sushi-config
`parameters: pin-canonicals: <value>`):
- `pin-none` → `NO_ACTION` (default, `PublisherFields.java:299`)
- `pin-all` → `FIX`
- `pin-multiples` → `WHEN_MULTIPLE_CHOICES`
- `pin-manifest: <Parameters-id>` → write pins into a manifest instead of inline (`PublisherIGLoader.java:754-756`, `pinInManifest` at `PublisherBase.java:1455`).

**`CanonicalVisitor.visit`** (`PublisherBase.java:1352-1414`): walks *all*
`CanonicalType` nodes via `DataTypeVisitor` (fhir-core
`r5/utils/DataTypeVisitor.java`, a generic recursive typed visitor). Per node:
skip if `null`, already contains `|` (1359), has `CANONICAL_RESOLUTION_METHOD`
extension (1362), THO CodeSystem NOTPRESENT (1366-1371), unresolved (1373),
target `!hasVersion()` (1376), or path starts with
`ImplementationGuide.dependsOn` (1379 — **IG dependsOn is explicitly NOT
pinned**). Then:
- `FIX`: append `url|tgt.getVersion()` inline (1392) or route to manifest.
- `WHEN_MULTIPLE_CHOICES`: only pin if
  `validationFetcher.fetchCanonicalResourceVersionMap(url).size() >= 2`
  (1396-1397) — i.e. pin only when the environment actually knows ≥2 versions.

**ValueSet compose** has a parallel policy-driven path
(`checkValueSetVersions`, `PublisherBase.java:788-849`) that sets
`inc.setVersion(...)` rather than mutating a URL.

**Net for cycle:** mechanism (B) contributed nothing (policy = NO_ACTION). The
observed pins are 100% mechanism (A). A Rust Layer B that only reproduces (A)
matches the default corpus; (B) is needed only for IGs that set
`pin-canonicals`.

---

## 2. The R4-artifact projection path (stage-6 PROJECT)

The bytes stored in `package.db.Resources.Json` are **not** a raw R5 compose
and **not** a separate downconversion step inside the DB writer. They are the
element-model serialization of `r.getElement()`, and `r.getElement()` is
produced by a **round-trip through the IG's declared FHIR version** at
snapshot time.

**Writer sink (stores whatever bytes it's handed):**
`DBBuilder.saveResource(FetchedFile, FetchedResource, byte[] json)` —
`renderers/DBBuilder.java:325`, `psql.setBytes(25, json)` at line 380
(CanonicalResource branch) / `setBytes(13, json)` at 347 (other). The scalar
columns (`base`, `standardStatus`, `derivation`, `kind`, `sdType`, …) are
projected from the R5 `CanonicalResource` object (lines 360-379), not from the
json blob.

**Bytes producer:**
`PublisherGenerator.saveNativeResourceOutputs(f, r)` —
`PublisherGenerator.java:5730`. Returns
`jp.compose(element = r.getElement(), …)` (line 5735, R5 elementmodel
JsonParser). Caller `generateNativeOutputs` →
`db.saveResource(f, r, json)` at `PublisherGenerator.java:687-689`.

**Where the R4 shape (incl. `xpath`) is baked in:**
`PublisherBase.convertToElement(r, res)` — `PublisherBase.java:396-429`,
called from the snapshot path at `PublisherBase.java:756`
(`r.setElement(convertToElement(r, sd))`). For an R4 IG
(`parseVersion` = 4.0.1, from resource config / `pf.version`), it:
1. `VersionConvertorFactory_40_50.convertResource(res)` — R5 SD → **R4** SD
   (`PublisherBase.java:411`);
2. composes with the **R4** JsonParser;
3. re-parses that R4 JSON back into the R5 element model
   (`elementmodel.JsonParser(...).parseSingle(...)`, `PublisherBase.java:427`).

The element model faithfully preserves the R4-shaped tree, so what lands in
package.db is the **R4 artifact projection**: R4 field layout, and R4-only
fields restored by the downconvertor.

**`constraint.xpath` survival — the exact mechanism:**
R5 `ElementDefinition.constraint` has **no** `xpath` child (verified: no
`xpath` in `r5/model/ElementDefinition.java`). R4 does
(`r4/model/ElementDefinition.java:3509`). During snapshot generation fhir-core
carries the R4 xpath as the extension
`ExtensionDefinitions.EXT_XPATH_CONSTRAINT`; the R5→R4 downconvert restores it:
`convertors/.../conv40_50/.../special40_50/ElementDefinition40_50.java:568`
`tgt.setXpath(readStringExtension(src, EXT_XPATH_CONSTRAINT))` (and the reverse
at line 551-552). Hence the 59 `constraint.xpath` in cycle's stored snapshot —
they are a **downconversion artifact**, present only because the IG is R4.

**Implication for our stage-6:** our PROJECT stage must replicate
"convert native-R5 snapshot → IG's fhirVersion artifact shape" — for R4 that
means (a) R4 field layout and (b) re-emitting `constraint.xpath` from the
carried extension. For an R5 IG, `convertToElement` takes the else-branch
(`PublisherBase.java:421-424`, plain R5 compose) and there is no xpath and no
downconversion — so the projection is version-conditional.

**Multi-version outputs are a different, opt-in path** (`generateVersions` /
`generateOtherVersions`, `PublisherProcessor.java:259` and
`PublisherGenerator.java:5746-5761`, `convVersion`) — only when the IG requests
extra R4B/R5 copies. **Not** the default package.db path; out of scope for the
first Rust Layer B.

---

## 3. Other Layer-B post-passes inventory (bounding, not specifying)

Pipeline order: **load phase** runs snapshots + `CoreVersionPinner` + the
policy pin pass (`PublisherIGLoader.java:4043` `generateSnapshots()`);
**process phase** (`PublisherProcessor.java:80-112`) then runs the passes
below. Only the ones marked **MUTATES SD** change resource content that reaches
package.db.

| Pass | Location | Scope note | Mutates SD? |
|---|---|---|---|
| `CoreVersionPinner` | `BaseWorkerContext.java:3285` | §1(A) — canonical `\|ver` on core-resolvable refs, at load | yes (base + inherited) |
| policy pin (`CanonicalVisitor`) | `PublisherBase.java:766` | §1(B) — off unless `pin-canonicals` set | conditional |
| `convertToElement` R4 projection | `PublisherBase.java:396` | §2 — R4 shape + `constraint.xpath` restore | yes (shape) |
| `generateNarratives` | `PublisherProcessor.java:1209` | fills/strips `text` on DomainResources; `no-narrative` filter drops `text` (cycle strips Bundle/Observation/Patient/Device/Procedure). SDs get narrative too. | yes (`text`) |
| `checkConformanceResources` | `PublisherProcessor.java:1417` | **validation only** (`pvalidator.validate`, jurisdiction, realm rules) — no content mutation | no |
| `generateOtherVersions` | `PublisherProcessor.java:259` | only if `generateVersions` set; extra R4B/R5 package copies | opt-in only |
| `generateAdditionalExamples` | `PublisherProcessor.java:1544` | synth examples; not SDs | n/a to SD |
| `propagateStatus` | `PublisherProcessor.java:91` | if `isPropagateStatus`; standards-status propagation | conditional |
| `scanForUsageStats` | `PublisherProcessor.java:1680` | bookkeeping/indexing; no content change | no |
| `setIds` / `sortDifferential` | `PublisherBase.java:716,724` | pre-snapshot normalization inside `generateSnapshot` (already Layer-A-adjacent) | pre-snap |
| apply-* (`apply-version`, `apply-jurisdiction`, `apply-contact`) | `PublisherIGLoader.java:508…` | stamps IG-level metadata onto resources per config; cycle sets `apply-version:true`, `apply-jurisdiction:false` | metadata |

For package.db content parity, the load-bearing post-passes are:
**CoreVersionPinner (A)**, **R4 projection (§2)**, and **narrative/`text`
strip** (which the cycle Phase-1 spike already handles via `no-narrative`).
Everything else is validation/bookkeeping or opt-in.

---

## 4. Oracle strategy

- **Golden #1 — the cycle package.db, regenerated.** The freshly regenerated
  Java DB is at `periodicity-impl/cycle/temp/pages/package.db` (Publisher
  2.2.10; committed fixture `site-gen/fixtures/package.db` was noted STALE in
  cycle plan §4b and must be refreshed from HEAD — the regen at
  `temp/pages/package.db` IS that refresh and should be promoted). It already
  exhibits mechanism (A) end to end (45 pins on `period-tracking-fact`, xpath
  restored). Gate a Rust `layer_b` PROJECT+PIN by comparing our stage-6
  `Resources.Json` for each SD to this DB's blob, canonical-JSON-normalized.
- **Second corpus (minimal, targeted):** a tiny **R5** IG (proves the
  projection is version-conditional: no xpath, plain R5 compose, and
  `CoreVersionPinner` pins only against R5-core versions) **plus** a tiny R4 IG
  that **sets `pin-canonicals: pin-all`** (exercises mechanism (B), the
  DataTypeVisitor path, `dependsOn`-exclusion, and `already-|`-guard).
  cycle's own test fixture `site-gen/publisher/fixtures/r5-minimal/`
  (`pin-canonicals: pin-all`) is a ready seed for the (B) case.
- **Headless driveability:** the Publisher is **whole-IG only** in practice —
  there is a per-SD `regenerate(uri)` entry (`PublisherGenerator.java:656`) but
  it presupposes a fully loaded context (it re-runs `generateNativeOutputs`).
  We already know the whole-IG `_genonce` path works for cycle (the fixture was
  regenerated that way, task #15). So the oracle is: run `_genonce` on a fixture
  IG, read the resulting `output/package.db`. Do **not** attempt per-SD headless
  invocation as the oracle; drive whole-IG and diff the DB.
- **Quirk registry from day one** (per REWORK-PLAN §7 item 4): every classified
  DB diff gets an entry {SD, path, rule-A/rule-B/projection/narrative,
  principled-vs-caselaw}.

---

## 5. Quirk-risk assessment (case law vs principled rule)

**Principled (safe to implement as rules):**
- (A) "append the resolved target's version to unversioned, in-context
  canonicals on `type.profile`/`targetProfile`/`binding.valueSet`/
  `binding.additional.valueSet`/`baseDefinition`" — clean, deterministic,
  keyed on resolution. This is the bulk of the cycle diff.
- Version chosen = resolved resource's `version` (package version). Principled.
- R4 projection = mechanical R5↔R4 downconvert; `constraint.xpath` is a
  deterministic function of the carried extension. Principled but
  version-conditional.
- `already-contains-|` and `!hasVersion()` guards. Principled.
- IG `dependsOn` never pinned (mechanism B). Principled/documented.

**Case law (registry candidates — implement only against the oracle, flag):**
- **THO carve-out asymmetry:** `pinCoreVersionVS`/`CS` skip
  `terminology.hl7.org`, but `pinCoreVersionSD` does **not**
  (`CoreVersionPinner.java:87 vs 97`). A real behavioral asymmetry, not an
  obvious rule.
- **THO NOTPRESENT CodeSystem** ignored in the ValueSet-compose paths
  ("their version is completely wrong for a start",
  `PublisherBase.java:810`, `CoreVersionPinner.java` compose path 68-73). Pure
  case law.
- **Whole-resource traversal but selective effect:** (A) walks differential
  too, yet differentials come out clean *because of resolution*, not because
  differential is skipped. A naive "pin only snapshot" reimplementation would
  usually match but could diverge for a profile whose differential names a
  resolvable, versioned, unversioned-in-source core canonical. Must mirror
  "walk everything, gate on resolution."
- **`WHEN_MULTIPLE_CHOICES` (B)** depends on
  `validationFetcher.fetchCanonicalResourceVersionMap(...).size() >= 2` — an
  *environment/registry* query, not a pure function of IG content. Hard to
  reproduce deterministically; treat as opt-in + oracle-gated only.
- **`CANONICAL_RESOLUTION_METHOD` / `|*` "latest" handling** (both visitors)
  — niche, extension-driven; registry it.
- **`valueAlternatives`** pinned "for thoroughness" though R5-only/unused in
  core (`CoreVersionPinner.java:53`). Low-value, note it.

---

## 6. Proposed phasing (small, opt-in crate/stage)

Target from REWORK-PLAN §7 item 4: Layer B as a **separate OPT-IN composable
stage**, own pin, own goldens, quirk registry from day one. Layer A stays the
pure `generateSnapshot` (untouched). Layer B consumes a finished native-R5
snapshot + the resolution context.

- **Phase B0 — projection only (stage-6 PROJECT), R4.** Implement
  native-R5 → R4-artifact projection: R4 field layout + `constraint.xpath`
  re-emit from the carried extension. Gate: `Resources.Json` structural diff
  vs cycle `temp/pages/package.db` for the SD blobs, **pins excluded**
  (mask `|<ver>` before diffing). This isolates the projection from pinning.
  *This is the piece cycle Phase-2 actually needs for byte-parity and the one
  §4b explicitly deferred to #17.*
- **Phase B1 — CoreVersionPinner (mechanism A).** Add the load-time-style pin
  as a Layer-B pass over the projected (or pre-projected) snapshot, driven by
  the same resolution context Layer A already builds (base/type/extension
  closure — REWORK-PLAN §2c records this dep set). Reproduce the field set,
  the resolve-and-stamp rule, the `already-|`/`!hasVersion` guards, and the
  THO asymmetry. Gate: full `Resources.Json` parity vs cycle DB, **pins
  included**; every remaining diff classified into the quirk registry.
- **Phase B2 — policy pin (mechanism B), opt-in.** Implement `pin-canonicals`
  = `pin-none|pin-all|pin-multiples` + `pin-manifest`, the generic
  canonical-node walk, and the `dependsOn` exclusion. Gate against the
  `r5-minimal`/`pin-all` fixture. `WHEN_MULTIPLE_CHOICES` gated behind a
  pluggable "known-versions" provider (defaults to "single version → no-op"),
  flagged as environment-dependent.
- **Phase B3 — narrative strip only** (not full narrative gen): honor
  `no-narrative` filters and `text` stripping to match package.db `text`
  handling (cycle already relies on this). Full narrative *generation* stays
  out of scope (render-time, per cycle plan).

**Gates (all):** whole-IG oracle via `_genonce`, DB-blob diff, no silent
normalization, quirk registry entry per classified diff — the same discipline
as the Layer-A rework. Composability: Layer B is `layer_a_snapshot →
[pin(A)] → [pin(B)?] → project(fhirVersion) → [narrative-strip]`, each stage
individually toggleable and individually goldened.

---

## Appendix — every load-bearing citation

Pinning (A): `fhir-core@6.9.10 org.hl7.fhir.r5/.../context/CoreVersionPinner.java`
lines 17-40 (traversal), 42-61 (per-ED fields), 77-105 (append+guards);
trigger `BaseWorkerContext.java:3285-3298`, `SimpleWorkerContext.java:329`.
Pinning (B): `ig-publisher@2.2.10 .../publisher/PublisherBase.java`
766-786 (entry+policy gate), 801-849 (VS compose), 1336-1416
(`CanonicalVisitor`), 1418-1453 (`CanonicalVisitorLatest`), 1455-1498
(`pinInManifest`), 913 (SD callsite); policy parse
`PublisherIGLoader.java:739-756`; default `PublisherFields.java:299`;
`DataTypeVisitor` `fhir-core .../r5/utils/DataTypeVisitor.java`.
Projection (§2): `PublisherBase.java:396-429` (`convertToElement`, R4 at 409-411,
re-parse 427), 756 (callsite); bytes
`PublisherGenerator.java:5730-5804` (compose at 5735), 687-689 (saveResource
call); writer `DBBuilder.java:325-398`; xpath restore
`convertors/.../conv40_50/.../ElementDefinition40_50.java:551-552,568`;
R5 ED has no xpath / `contentReference`=uri
`fhir-core r5/model/ElementDefinition.java:6177,11996`; R4 ED has xpath
`fhir-core r4/model/ElementDefinition.java:3509`.
Post-passes (§3): `PublisherProcessor.java:80-112` (order), 259, 1209, 1417,
1544, 1680; `PublisherIGLoader.java:4043` (generateSnapshots), 508 (apply-*).
Config (§0): `periodicity-impl/cycle/sushi-config.yaml` (`fhirVersion: 4.0.1`,
no `pin-canonicals`); measured DB `periodicity-impl/cycle/temp/pages/package.db`.
Methodology boundary: `sushi-rs-snapshot/snapshot/METHODOLOGY.md:15,43-45`
(oracle is Layer-A-only, bypasses publisher).

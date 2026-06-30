# Next-20 IG Validation Findings (2026-06-30)

Stock SUSHI **v3.20.0** vs our port across the **next 20 FHIR IGs** in
`docs/igs-to-test-with-next-20.json` (international/diverse: IHE, HL7 Europe/
Belgium/AU/Taiwan, gematik, CDA-on-FHIR). Isolated FHIR home at
`temp/next20/fhir-home` (hardlink-seeded from `temp/fhir-home`; guard
`assert_real_fhir_untouched` confirmed the real `~/.fhir` was untouched before/
after every stock + rust build). Clones/outputs in
`temp/next20/<slug>{,-stock,-rust}` (gitignored).

**13 of 20 are FSH/SUSHI projects** (have `sushi-config.yaml` + `input/fsh/*.fsh`).
All 13 built on **both** sides with **no rust crash** and **no over-production**
(EXTRA=0 everywhere). 5 reached full byte parity. 7 are **N/A** (not FSH compile
targets). FHIR-version coverage: 11×R4, 2×R5 (application-feature, ccda-cda); no
R4B this batch.

## Scorecard (buildable FSH projects)

| IG | FHIR | STOCK | PASS | FAIL | MISS | parity | top root-causes |
|----|----|----|----|----|----|----|----|
| **vhl** (IHE) | R4 | 37 | 37 | 0 | 0 | **37/37** | — |
| **deid** (IHE) | R4 | 21 | 21 | 0 | 0 | **21/21** | — |
| **ph-query** (HL7) | R4 | 3 | 3 | 0 | 0 | **3/3** | — |
| **radiation-dose** | R4 | 28 | 28 | 0 | 0 | **28/28** | — |
| **pacio-toc** | R4 | 83 | 83 | 0 | 0 | **83/83** | — |
| **eu-mpd** (HL7-EU) | R4 | 41 | 40 | 1 | 0 | 40/41 | B3 (IG groupingId from `groups:`) |
| **safr** (CDC/HL7) | R4 | 25 | 24 | 1 | 0 | 24/25 | **G9** (inline Bundle entry truncation) |
| **be-vaccination** | R4 | 72 | 69 | 3 | 0 | 69/72 | B5 (VS compose concept `^designation`) |
| **mhd** (IHE) | R4 | 120 | 117 | 3 | 0 | 117/120 | **B4** (`Parent:`-by-Id drops `Mapping`) |
| **eu-eps** (HL7-EU) | R4 | 51 | 31 | 20 | 0 | 31/51 | **B2** (obligation ext implied-`url` key-order) |
| **ccda-cda** (CDA) | R5 | 229 | 197 | 32 | 0 | 197/229 | **N6** (surrogate `\uXXXX`) |
| **tw-pas** (TW) | R4 | 187 | 144 | 43 | 0 | 144/187 | **B1** (xhtml narrative html-minifier ws) |
| **application-feature** | R5 | 13 | 6 | 5 | 2 | 6/13 | **B6** (predefined logical-model cluster) |
| **TOTAL** | | **910** | **800** | 108 | 2 | **800/910 (87.9%)** | |

## N/A — not FSH compile targets on the default branch (7)

| IG | FHIR | Why N/A |
|----|----|----|
| **pddi** (PDDI-CDS) | R4 | **0 `.fsh`**; CDS/CQL IG — resources hand-authored in `input/resources`, CQL in `input/cql`. IG-Publisher target, not SUSHI. |
| **ccda-fhir** (C-CDA on FHIR) | R4 | **0 `.fsh`**; Composition profiles authored as JSON/XML in `input/resources`. No `sushi-config.yaml`. |
| **epamed** (gematik ePA Medication) | R4 | Default branch `ePA-3.1.3` ships **0 `.fsh`** (only docs/tools/`README`). FHIR artifacts published out-of-band. |
| **order-catalog** | R4 | **Has** `input/fsh/` (16 `.fsh`) **but NO `sushi-config.yaml`** — SUSHI cannot build standalone (IG-Publisher invokes its own). To exercise it, a synthesized config would be needed. |
| **au-ps** (AU PS) | R4 | **0 `.fsh`**; `input/resources` + `_resources` XML/JSON authored. No `sushi-config.yaml`. |
| **darts** (US DARTS) | R4 | **0 `.fsh`**; CQL + `input/resources` authored. No `sushi-config.yaml`. |
| **dapl** (US DAPL) | R4 | **0 `.fsh`**; CQL + `input/resources` authored. No `sushi-config.yaml`. |

> The "CDA" suspicion held only partially: **ccda-cda** (`hl7.cda.us.ccda`) *is* a
> FSH project (233 `.fsh`, FHIR R5, CDA **logical models** via `hl7.cda.uv.core`) and
> built fine. **ccda-fhir** (`hl7.fhir.us.ccda`, Composition-on-FHIR) is the JSON/XML
> one. C-CDA-R5-as-CDA-templates was the wrong guess for which repo.

---

## NEW root causes (not in holdout G1–G14, mining N1–N7, or top20 X1–X6)

Ordered by impact. Labeled **B1–B6** (this batch).

### B1 — xhtml narrative whitespace ≠ html-minifier-terser  *(HIGH — 42 files)*
Stock runs every assigned `xhtml` value (`text.div`, and the same on
`fixedXhtml`/`patternXhtml`) through `html-minifier-terser`
`minify(value, {collapseWhitespace:true, html5:false, keepClosingSlash:true})`
— **`sushi-ts/src/fhirtypes/ElementDefinition.ts:2385-2393`**. Our port has a
hand-rolled `minify_xhtml` (`crates/compiler/src/instance_export.rs:292`) that is
close but **diverges on the inner boundary of inline elements**: html-minifier
**trims** whitespace at the *start* of an element's content (right after its own
opening tag) and at the *end* (right before its own closing tag), even for inline
elements; it only *preserves* (collapses to one space) whitespace **between**
sibling inline elements/text. Our `trim_left`/`trim_right`
(`instance_export.rs:346-362`) keys solely on whether the adjacent tag is inline,
**without distinguishing opening vs closing tags** — so for
`<span ...>\n   （…）\n</span>` we emit `<span> （…） </span>` where stock emits
`<span>（…）</span>`.

- **Repro (FSH):** `* text.div = "<span ...>\n    (text)\n</span>"` → stock `<span>(text)</span>`, ours `<span> (text) </span>`.
- **Hits:** **tw-pas** — ~**42** of 43 fails are this single cause (Patient/
  Observation/Organization/Substance/Procedure/DiagnosticReport/Encounter/Claim/
  MedicationRequest/DocumentReference/Bundle/CapabilityStatement … every instance
  with an authored multi-line `text.div`). General: any IG that authors indented
  narrative directly in FSH.
- **Fix direction:** model html-minifier's text-node trimming precisely — a text
  token immediately after an *opening* tag trims leading ws; immediately before a
  *closing* tag trims trailing ws; inter-element (after a closing tag / before an
  opening tag) collapses to a single space iff the adjacent element is inline.

### B2 — Caret extension with implied `url` (URL-valued slice token): `url` ordered wrong  *(HIGH — 20 files, pure key-order)*
When an SD element's extension is built by caret rules keyed on the extension's
**URL** (here via alias `$obligation` = `http://hl7.org/fhir/StructureDefinition/obligation`)
with **no explicit `.url` rule** —
`* element ^extension[$obligation][+].extension[code].valueCode = …` /
`… ^extension[$obligation][=].extension[actor].valueCanonical = …` — stock emits
the extension object as `{ extension:[…sub-exts…], url:"…obligation" }` (the
**implied `url` last**), while we emit `{ url:"…obligation", extension:[…] }`
(canonical url-first). Every diff is purely this transposition; the resources are
**JSON-equal** (verified).

- **Stock algorithm:** `setImpliedPropertiesOnInstance`/`setPropertyOnInstance`
  (`sushi-ts/src/fhirtypes/common.ts:150-255`): a sliced extension element is first
  pushed as a placeholder `{ _sliceName: sliceName }` and the **sub-extensions are
  filled in as the path descends**; the slice's `url` is materialized only later
  when `_sliceName` is resolved/cleaned — so `url` lands in insertion order **after**
  the assigned children. (SD caret rules reach the same machinery via
  `setPropertyOnDefinitionInstance`.)
- **Our divergence:** our SD caret-extension export writes `url` in canonical
  first position rather than appending it after the assigned sub-extensions.
- **Hits:** **eu-eps** — all **20** obligation profiles (`*-obl-eu-eps`,
  `patient-eu-eps`, `procedure-eu-eps`, …); the obligation RuleSets in
  `input/fsh/rulesSet/rulesSet-common.fsh:101-107`. This is the X6/G4 "implied-url /
  source-order" family but a distinct *ordering* manifestation (we now emit the url,
  unlike X6, just in the wrong slot).
- **Fix direction:** emit the slice-token-implied `url` at the END of the extension
  object (after children written by caret rules), matching stock's
  `_sliceName`→`url` deferral.

### B3 — IG `definition.resource` entry metadata/order from `groups:` not applied  *(MED — 2 IGs)*
Three sub-symptoms on the ImplementationGuide resource list:
1. **`groupingId` dropped** when group membership is declared via the top-level
   `groups:` block listing resources by **profile NAME**. Our match
   (`crates/compiler/src/ig_export.rs:1203`) is
   `entries.find(|e| e.reference_key == rref)` where `reference_key` is
   `Type/id` (e.g. `StructureDefinition/MedicationRequest-eu-mpd`) but `rref` is the
   group's name token (`MedicationRequestEuMpd`) → no match → `groupingId` never
   stamped. We already build an `id_to_ref` map (`ig_export.rs:1172-1173`) but the
   group loop doesn't do **name→reference** resolution the way stock's
   `addConfiguredGroups` (`sushi-ts/src/ig/IGExporter.ts`, groups handling) does.
   - *Hit:* **eu-mpd** `ImplementationGuide-hl7.fhir.eu.mpd.json` (4 SD entries lose
     `groupingId: FHIRProfiles`/`DataTypes`).
2. **`name`/`description` dropped** on resource entries (app-feature IG — entries for
   the predefined logical-model instances lose `name`/`description`).
3. **Resource-entry ordering swap** — **tw-pas** IG: stock orders `Claim/cla-3`
   before `Bundle/bun-3`; we swap them (a `sort_resources` divergence on
   auto-collected examples).

### B4 — `Parent:` referenced by **Id** re-exports the ancestor → post-export `Mapping` lost  *(MED — 3 files; broader latent bug)*
A FSH `Mapping:` entity attaches SD-level + element-level `mapping` to its `Source`
profile, applied in a post-export pass
(`MappingExporter` ≈ `crates/compiler/src/sd_export.rs:350-470`). It works in
isolation, **but the whole `mapping` (SD-level and element-level) vanishes when the
source profile has a descendant that references it as `Parent:` by its *Id***.

- **Root cause (minimal repro confirmed):** `already_exported(name)` and
  `export_sd(name)` key **only on `e.name`** (`sd_export.rs:345-346`), but a child's
  `Parent:` may be the parent's **Id**. `get_structure_definition`
  (`sd_export.rs:579-582`) does `if !already_exported(parent) && tank_index(parent) { export_sd(parent) }`
  — with `parent` = the Id, `already_exported` (name-keyed) returns **false**, so the
  already-exported ancestor is **re-exported as a duplicate `exported` entry**. Then
  `export_mapping` applies the mapping to the *first* matching entry (matched by id at
  `sd_export.rs:360-363`), but the duplicate fresh re-export (no mapping) is written
  to the same `StructureDefinition-<id>.json` and wins → mapping lost.
  - 2-level chain with `Parent:` by **name** → mapping kept; by **Id** → mapping
    dropped (both verified).
- **Hits:** **mhd** — `IHE.MHD.Minimal.{DocumentReference,Folder,SubmissionSet}`
  (each is an ancestor in an Id-referenced 3-level chain and carries a
  `DocumentEntry/Folder/SubmissionSet-Mapping`). Stock emits SD-level `mapping` +
  ~34 element `mapping`s; we emit none.
- **Severity note:** the duplicate re-export is latent and broader than mappings —
  IHE/EU IGs reference `Parent:` by dotted Id heavily; today it only *visibly* breaks
  because mappings are the main post-`export_all` mutation, but it's a correctness
  smell. **Fix = make `already_exported`/`export_sd`/`tank_index` resolve a parent
  token by name OR id consistently** so an Id-referenced ancestor is recognized as
  already exported.

### B5 — ValueSet compose `include.concept` caret (`^designation`) dropped  *(MED — 3 files)*
`* $sct#code "display"` followed by `* $sct#code ^designation[0].language = #fr-BE`
/ `^designation[=].value = "…"` adds the concept to `compose.include[].concept[]`
**and** a `designation` array on it. We emit the concept (`code`+`display`) but
**drop the `^designation`** caret. CodeSystem-concept designations authored the same
way **do** work (be-vaccination's CodeSystems pass) — so this is specifically
**caret application onto a VS `compose.include.concept` element**, not concept carets
in general.

- **Hits:** **be-vaccination** `ValueSet-be-vs-{vaccine-code,vaccination-bodysite,
  vaccination-reason-code}.json` (sources `input/fsh/valuesets/BeVS*.fsh`).
- Sibling of **G8** (CS concept `property` dropped) but on the ValueSet side; likely
  the VS exporter's concept-caret path doesn't reach the same set-caret-on-concept
  logic the CS exporter uses.

### B6 — Predefined **logical-model** resource (`input/resources` XML) cluster  *(R5; application-feature)*
`application-feature` (R5) ships a hand-authored logical model
`input/resources/StructureDefinition-FeatureDefinition.xml` (kind=logical, a custom
"resource"). Stock loads predefined resources, makes them fishable, and this drives
several behaviors we miss — one cluster, multiple sub-causes:
- **(a) Instances of the logical model dropped — 2 MISS.** `Instance: FavoriteColor`
  / `FeatureSupport` (`InstanceOf: FeatureDefinition`) are emitted by stock as
  `Binary-FavoriteColor.json` / `Binary-FeatureSupport.json` (resourceType = the
  FeatureDefinition canonical). We emit nothing → 2 missing resources.
- **(b) `only Canonical(<local logical model>)` type dropped.** `* …value[x] only
  Canonical(FeatureDefinition)` — stock constrains to
  `type:[{code:canonical, targetProfile:[…/FeatureDefinition]}]`; we keep `min:1` but
  **drop `type`** (can't fish the predefined logical SD). Hits
  `StructureDefinition-{feature,FeatureQueryInputParameters,FeatureQueryOutputParameters}.json`.
- **(c) IG resource-format extension.** Stock references these instances as
  `Binary/<id>` with `…/implementationguide-resource-format` `valueCode
  application/fhir+json` + `isExample:true`; we emit `…/implementationguide-resource-logical`
  `valueCanonical <FeatureDefinition url>`.
- **(d) XML char-entity not decoded.** The predefined SD's `description` has `&#xA;`
  (XML numeric entity for newline); stock decodes it to a real newline, we keep
  `&#xA;` (predefined-resource XML→JSON entity handling).
- **(e) OperationDefinition `url` last segment.** stock `…/OperationDefinition/feature-query`
  (by id) vs ours `…/FeatureQuery` (by name).
- **Severity:** niche (predefined logical models are rare) but it's the only place a
  rust build *under-produced* (MISS≠0) this batch — worth a focused look at
  predefined-resource loading + logical-model fishing.

---

## KNOWN causes hit (would already-on-main clear them?)

- **N6 (mining)** — surrogate-pair `\uXXXX` (e.g. `𝗨` → 𝗨) left as
  **literal `\\ud835\\udde8`** instead of the decoded character. **ccda-cda — 31 of
  32 fails.** Mining catalogued this as an "edge"; here it's the dominant cause of a
  whole R5 IG. The decode lives in stock's `unescapeQuotedString`/`unescapeUnicode`
  (`sushi-ts/src/import/FSHImporter.ts:2407` via the `\\(u[0-9a-f]{4})` replace).
  If our string-unescape already handles BMP `\uXXXX` but not **surrogate pairs**
  (two `\uXXXX` combining into one astral codepoint), that's the gap. Not known to be
  fixed on main.
- **G9 (holdout)** — inline/contained resource assigned to a Bundle entry truncated.
  **safr** `Bundle-USSAFRMeasureBundleExample.json`: `entry.resource = <inline
  Measure with contained Library>` → stock 4677 lines, we ~1. Still open per
  holdout-findings "Root Cause C / G9".
- **(minor)** ccda-cda `StructureDefinition-HealthConcernAct.json` (the 1 non-N6
  fail): we emit one **extra** `targetProfile` (`…/ProblemObservation`) stock omits —
  a reslice/targetProfile over-production (G10-ish), 1 line.

---

## Priority for the next alignment wave
1. **B1** (xhtml html-minifier inner-inline trim) — 42 files in one IG, general to
   any authored narrative; bounded fix in `minify_xhtml`
   (`instance_export.rs:346-362`).
2. **N6** (surrogate-pair unescape) — 31 files / a whole R5 IG; small, localized
   string-unescape fix.
3. **B2** (implied-url ordering) — 20 files, pure key-order, JSON-equal; defer the
   slice-token `url` to after caret-written children.
4. **B4** (`Parent:`-by-Id duplicate re-export) — 3 visible files but a real
   correctness smell; fix name-or-id parent resolution in
   `already_exported`/`export_sd`/`tank_index`.
5. **B3 / B5 / B6 / G9** — the tail (IG group/order metadata; VS concept designations;
   predefined logical-model cluster; Bundle contained truncation).

## Honesty on coverage
- 13/20 were genuinely FSH-buildable; **all 13 built on both sides with no rust crash
  and no over-production**. 5 are byte-perfect (vhl, deid, ph-query, radiation-dose,
  pacio-toc).
- 7/20 are not FSH compile targets (listed above) and were correctly skipped.
  **order-catalog** is the notable case: it *has* `input/fsh` but **ships no
  `sushi-config.yaml`**, so neither stock nor we can build it standalone — to stress
  it you'd synthesize a config.
- No dependency-fetch failures: stock downloaded all needed deps into the isolated
  home; our builds reused that populated cache (`FHIR_CACHE`).
- ccda-cda's stock build exits non-zero (53) with terminology/validation errors but
  still emits 229 resources — we match the 229 set (197 byte-identical), so its errors
  did not block the parity comparison.

# Top-20 IG Validation Findings (2026-06-30)

Stock SUSHI **v3.20.0** vs our port across the **top-20 FHIR IGs** in
`docs/igs-to-test-with.json`. Focus was the *new* IGs not already covered by the
4-IG tuning corpus or the 8-IG holdout set. Isolated FHIR home reused at
`temp/fhir-home` (guard `assert_real_fhir_untouched` confirmed real `~/.fhir`
untouched before/after every stock + rust build). Clones/outputs in
`temp/top20/<slug>{,-stock,-rust}` (gitignored).

## Coverage / classification of the 20

**Already covered (not re-tested — see `holdout-findings.md`):** IPS, mCODE, CRD,
PAS, DTR, SDC, Genomics, case-reporting(eCR). Status there: all build both sides,
documented diffs (G1–G14 / N1–N7).

**N/A — not a FSH/SUSHI compile target on default branch** (no `input/fsh` with
`.fsh`, or no `sushi-config.yaml`): these author conformance resources directly in
JSON/XML under `input/resources` and run the IG Publisher, not SUSHI:

| IG | FHIR ver | Why N/A |
|----|----|----|
| **US Core** | R4 | `sushi-config.yaml` present but **0 `.fsh`**; 213 hand-authored JSON in `input/resources`. `fsh-generated/` holds only the auto-built IG resource. |
| **CDex** | R4 | sushi-config present, **0 `.fsh`**; JSON in `input/resources`. |
| **IPA** | R4 | sushi-config present, **0 `.fsh`**; JSON in `input/resources`. |
| **QI-Core** | R4 | **No `sushi-config.yaml`**; authored as `input/resources` + `input/profiles` XML. |
| **AU Core** | R4 | **No `sushi-config.yaml` on `master`**; FSH migration lives on an unmerged `ft-fsh-conversion` branch. Master is XML/JSON authored. |
| **SMART App Launch** | R4 | OAuth framework; **no `sushi-config.yaml`**, no resources. |
| **Bulk Data** | R4 | *Is* a FSH project (9 `.fsh`) — built below. |
| **CDS Hooks** | R4 | *Is* a FSH project (19 `.fsh`) — built below. |

> NB: US Core / CDex / IPA / AU Core could still be exercised as **IG-generation-only**
> targets (stock would scan `input/resources` and emit the ImplementationGuide +
> `fsh-generated/includes`); not done here. If the next wave wants SD-exporter stress
> on national base profiles, point at a **release tag/branch that still ships FSH**
> (older US Core versions did) or AU Core's `ft-fsh-conversion` branch.

## Buildable FSH projects — parity scorecard

Both sides built successfully (no rust build crashes). STOCK = stock resource count.

| IG | FHIR ver | STOCK | PASS | FAIL | MISS | parity | top root-causes |
|----|----|----|----|----|----|----|----|
| **Bulk Data** | R4 | 13 | 13 | 0 | 0 | **13/13 ✅** | — |
| **PDex** | R4 | 179 | 176 | 3 | 0 | 176/179 | X2 (unfold sub-ext), X4 (named+soft index) |
| **Plan Net** | R4 | 110 | 107 | 3 | 0 | 107/110 | X2 (qualification complex ext) |
| **Drug Formulary** | R4 | 86 | 84 | 2 | 0 | 84/86 | X1 (xver pkg substitution) |
| **CDS Hooks** | R4 | 8 | 7 | 1 | 0 | 7/8 | X5 (translateR5PropertiesToR4 / copyrightLabel) |
| **Subscriptions R5 Backport** | R4/R4B | 34 | 28 | 6 | 0 | 28/34 | X3 (unresolvable xver ext over-produced), X6, G13 |

Stock itself reports **43 errors** on Subscriptions (xver extensions genuinely
unresolvable, see X3) yet still emits 34 resources — those 43 errors are the
*intended* behavior we must reproduce.

---

## NEW root causes (not in holdout G1–G14 or mining N1–N7)

Ordered by impact. All are about **cross-version extensions** and **extension-slice
resolution** — areas the prior corpora barely touched.

### X2 — Referenced complex extension's sub-extensions not unfolded  *(HIGH)*
When a profile does `* extension contains <ComplexExt> named y` and then dives into
`extension[y].extension[<subslice>]`, stock fishes the referenced extension's
StructureDefinition and **unfolds its sub-extension children** so the subslice path
resolves. We create the `y` slice but never unfold its children.

- **Stock algorithm:** `ElementDefinition.unfold()`
  (`sushi-ts/src/fhirtypes/ElementDefinition.ts:2687-2805`): for a sliced extension
  whose `type[0].profile` points at an extension, it
  `fishForFHIRBestVersion(..., Type.Extension)`, `StructureDefinition.fromJSON(json)`,
  clones `def.elements.slice(1)` re-id'd to the slice path, and **`captureOriginal()`**
  on each so the profile *differential* shows only post-unfold changes.
- **Our divergence:** no fish/unfold of the external complex extension. Two symptoms:
  - *SD differential:* we emit `…extension:y.value[x]` `max:0` (the generic
    "complex ext ⇒ forbid value" constraint, which stock keeps in the *snapshot* not
    the diff) and **drop** the real `…extension:y.extension:<subslice>.value[x]`
    constraint (e.g. the binding).
  - *Instances:* assignments into `extension[y].extension[<subslice>]` are **dropped**
    entirely (the whole complex extension vanishes from the instance).
- **Hits:**
  - Plan Net `qualification` ext (def `Extensions.fsh:175 Id: qualification`, sub-exts
    code/status/issuer/…): `StructureDefinition-plannet-PractitionerRole.json`,
    `StructureDefinition-plannet-OrganizationAffiliation.json`,
    `PractitionerRole-HansSoloRole1.json` (whole qualification ext dropped).
  - PDex `reviewAction` ext (sub-exts number/reviewActionCode):
    `ExplanationOfBenefit-PDexPriorAuth1.json` (whole `item.adjudication.extension` dropped).
- **Count:** ≥5 files across 2 IGs; structural — likely affects every Da Vinci IG that
  slices a local complex extension and constrains/sets a sub-extension.

### X3 — `extension contains <URL> named y`: referenced extension existence not validated  *(HIGH, mirror of X2)*
When the contained extension's definition **cannot be found**, stock errors
(`Cannot create <y> extension; unable to locate extension definition for: <url>`) and
**skips the ContainsRule** — the slice is never created, and all downstream Card/Flag/
CaretValue/instance rules on `extension[y]` also error+skip. We create the slice
anyway from the bare `type.profile` URL without validating existence ⇒ **over-produce**.

- **Stock algorithm:** `FHIRDefinitions.fishForFHIR/fishForMetadata`
  (`sushi-ts/src/fhirdefs/FHIRDefinitions.ts:130-205`) — direct URL lookup, then for
  `XVER_EXTENSION_REGEX` URLs tries `fixXverURL`, else
  `logXverExtensionDependencyError`. The xver package `hl7.fhir.uv.xver-r5.r4#0.1.0`
  does **not** contain `extension-Subscription.content/heartbeatPeriod/timeout/maxCount`
  (verified: no file/url in cache), so stock fails to create those slices.
- **Our divergence:** ContainsRule on `extension` doesn't fish/validate the referenced
  extension SD; we synthesize the slice (and the instance value) regardless.
- **Hits (Subscriptions):** `StructureDefinition-backport-subscription.json` (we add
  extension:content/heartbeatPeriod/timeout/maxCount slices stock omits),
  `Subscription-subscription-admission.json`, `-multi-resource.json`, `-zulip.json`
  (we add `valueCode/valueUnsignedInt/valuePositiveInt` entries stock drops).
- **Count:** 1 SD + 3 instances (+ contributes to CapabilityStatement). This is why
  stock's Subscriptions build has 43 errors and ours has 0 — **we are too lenient**.

### X1 — Old-style cross-version extensions package not substituted  *(MED-HIGH)*
A dependency `hl7.fhir.extensions.r5` (or `.r4b`, etc.) is a legacy alias. Stock
rewrites it to the official xver package `hl7.fhir.uv.xver-r5.r4` **both** at load time
(so its extensions become fishable) **and** in the emitted `IG.dependsOn`.

- **Stock algorithm:** `fixCrossVersionDependencies`
  (`sushi-ts/src/utils/Processing.ts:540-570`): regex `/^hl7\.fhir\.extensions\.r\d+b?$/`
  ⇒ `packageId = hl7.fhir.uv.xver-{source}.{target}`, `version = "latest"`,
  `uri = http://hl7.org/fhir/uv/xver/ImplementationGuide/<id>`. Called again in
  `IGExporter.ts:230` for the dependsOn entry. (Stock log: *"Found old-style
  cross-version extensions package … SUSHI will use the official xver package instead"*.)
- **Our divergence:** we take the sushi-config key/version literally → look up a
  non-existent `hl7.fhir.extensions.r5#4.0.1`, never load the xver package, and emit a
  literal/derived dependsOn entry.
- **Hits (Drug Formulary, config dep `hl7.fhir.extensions.r5: 4.0.1`):**
  - `ImplementationGuide-…drug-formulary.json`: dependsOn packageId/version/uri/id wrong
    (`hl7.fhir.extensions.r5`/`4.0.1`/`fhir.org/packages/…` vs stock
    `hl7.fhir.uv.xver-r5.r4`/`0.1.0`/`hl7.org/fhir/uv/xver/…`/`hl7_fhir_uv_xver_r5_r4`).
  - `Coverage-InsurancePlanCoverageExample.json`: drops the R5 xver extension
    `http://hl7.org/fhir/5.0/StructureDefinition/extension-Coverage.insurancePlan`
    (defined via `Formulary.fsh:323`) — downstream of not loading the substituted pkg.
- **Count:** 2 files (1 IG dependsOn + 1 example) here; will recur in any R4 IG using
  the legacy `hl7.fhir.extensions.rN` alias.

### X5 — `translateR5PropertiesToR4()` for the IG resource not implemented  *(MED)*
When an **R4** IG's sushi-config sets an R5-only IG property (`copyrightLabel`,
`versionAlgorithmString/Coding`, page `source[x]`, parameter `code` system, resource
`profile`), stock represents it on the R4 IG as an
`http://hl7.org/fhir/5.0/StructureDefinition/extension-ImplementationGuide.*`
extension. We omit these.

- **Stock algorithm:** `IGExporter.translateR5PropertiesToR4()`
  (`sushi-ts/src/ig/IGExporter.ts:1662-1735`) — note this constructs the extension
  inline (no xver package needed).
- **Our divergence:** `copyrightLabel` (and the other R5 props) not emitted as the R4
  back-compat extension.
- **Hits (CDS Hooks, `copyrightLabel: "HL7 & Boston Children's Hospital"`):**
  `ImplementationGuide-hl7.fhir.uv.cds-hooks.json` drops the
  `extension-ImplementationGuide.copyrightLabel` extension.
- **Count:** 1 file here; affects any R4 IG using `copyrightLabel`/`versionAlgorithm`/
  R5 page-source/parameter-code-system.

### X4 — Named soft-index tokens and `[+]/[=]` share one index sequence on an **unsliced** array  *(MED)*
On an array element with **no formal slicing** (R4 `Coverage.class`), FSH may mix
pseudo-slice tokens `class[group]`, `class[plan]` with `class[+]`/`class[=]`. Stock
keeps a **single** name→index map per element path: `group→0, plan→1, [+]→2, [+]→3`
(4 entries). We keep the `[+]/[=]` counter **separate** from named tokens, so `[+]`
restarts at 0 and collides into the named entries.

- **Symptom:** stock 4 `class` entries (group/plan/subplan/class); we emit **2**, with
  codings merged (`[group,subplan]` and `[plan,class]`) and earlier `value`s overwritten.
- **Hits (PDex, FSH `ExampleMultiMemberMatchParameters.fsh:189-196` /
  `:44-51`):** `Coverage-coverage-2.json`,
  `Parameters-payer-multi-member-match-in.json`.
- **Count:** 2 files; any instance mixing named + `[+]/[=]` indices on the same
  unsliced repeating element.

### X6 — Extension slice named by an absolute URL token (implicit url) dropped  *(LOW)*
`* rest[=].mode.extension[http://hl7.org/fhir/StructureDefinition/capabilitystatement-expectation].valueCode = #SHALL`
— the bracket token is a full URL and there is **no explicit `.url` rule**; stock infers
the extension's `url` from the slice token and emits `{url, valueCode}`. We drop it
(the sibling `_mode` is absent), even though sibling forms using `extension[0].url = $exp`
+ `extension[0].valueCode` (numeric index, explicit url) **do** work for us in the same
instance.

- **Hits (Subscriptions, `Capabilities.fsh:85`):**
  `CapabilityStatement-backport-subscription-server-r4.json` (missing `_mode.extension`).
  *This file also shows instance key-ordering drift (≈ known G13, ordered by snapshot)
  and X3 effects — it is a multi-cause file.*
- **Count:** 1 file confirmed; construct (absolute-URL extension slice token) is general
  and worth a minimal repro in the next wave.

---

## Priority for the next alignment wave
1. **X2 + X3** (same surface — *fish & unfold the extension referenced by an
   `extension contains` slice*): fixing the fish/unfold path resolves both the
   under-production (X2: PDex/PlanNet, plus likely most Da Vinci complex-extension IGs)
   and the over-production (X3: Subscriptions). Highest structural ROI.
2. **X1** (`fixCrossVersionDependencies`) — small, well-bounded port of
   `Processing.ts:540` + call sites; unlocks every legacy-`extensions.rN` R4 IG.
3. **X5** (`translateR5PropertiesToR4`) — bounded IGExporter port; `copyrightLabel`
   et al. is common in modern R4 IGs.
4. **X4** (unify named + soft index on unsliced arrays) — localized indexing fix.
5. **X6** (absolute-URL extension slice token) — low impact; confirm with a repro.

## Honesty on coverage
- 6 of the 12 *new* IGs were genuinely FSH-buildable (bulk/pdex/plannet/formulary/
  subscriptions/cdshooks); all 6 built on both sides with **no rust crash** and
  **no missing files** — only byte diffs. Bulk Data is byte-perfect (13/13).
- 6 of the 12 (US Core, CDex, IPA, QI-Core, AU Core, SMART) are **not FSH projects on
  their default branch** and were correctly skipped; to stress the SD exporter on
  national base profiles, target an FSH-bearing tag/branch (noted above).
- The known-covered IGs (IPS/mCODE/CRD/PAS/DTR/SDC/Genomics/eCR) were not rebuilt here;
  their status is tracked in `holdout-findings.md`.

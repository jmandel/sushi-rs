# How the Java IG Publisher decides WHAT xhtml fragments to produce

Research date: 2026-07-03. Publisher source pinned to **HL7/fhir-ig-publisher tag `2.2.10`** (commit
`37a39a2c`, shallow clone in scratchpad). Templates from **HL7/ig-template-base** and
**HL7/ig-template-fhir** (default branch). Ground truth from a real publisher run under
`/home/jmandel/hobby/periodicity-impl/cycle/temp/pages/_includes` plus two real
`fragment-usage-analysis.csv` files found on disk.

Paths below are relative to the publisher clone root
`…/scratchpad/ig-publisher/org.hl7.fhir.publisher.core/src/main/java/org/hl7/fhir/igtools/`.

---

## (a) The generation-decision mechanism — who decides, driven by what config

### The core writer: `fragment()` unconditionally writes one `_includes/<name>.xhtml` per call
`publisher/PublisherGenerator.java:2463` — `fragment(...)`:
- Computes content, records a `FragmentUseRecord` keyed by `context+"."+code` into `pf.fragmentUses`
  (`:2466-2471`), then calls `checkMakeFile(... Utilities.path(pf.tempDir, "_includes", name+…+".xhtml"))`
  (`:2473`). **Every call that reaches here writes a physical fragment file.** There is no consultation
  of "did a page ask for this" at write time.
- The publisher's OWN Java code is the thing that enumerates fragments — not the template. E.g.
  `generateOutputsStructureDefinition()` (`PublisherGenerator.java:1808`) contains **113** fragment
  calls, one per fragment kind, for **every** StructureDefinition (`summary`, `summary-table`,
  `class-table`, `header`, `uses`, `ctxts`, `diff`, `snapshot`, `snapshot-by-key`,
  `snapshot-by-mustsupport`, `*-bindings`, `*-obligations`, `expansion`, `grid`, `tx*`, `inv*`,
  `dict*`, `maps`, `sd-xref`, `search-params`, `span`, `spanall`, `example-list/table`,
  `testplan/testscript-list/table`, …). Analogous `generateOutputsCodeSystem`/`…ValueSet`/etc.
  exist at `:1478`, `:1528`, …

### The single gate: `wantGen(r, code)` — a three-part AND
`PublisherGenerator.java:2146`:
```java
private boolean wantGen(FetchedResource r, String code) {
  if (pf.wantGenParams.containsKey(code) && !genParam) return false;   // (1) global "generate" params
  return pf.igpkp.wantGen(r, code)                                     // (2) per-resource IGKnowledgeProvider
      && pf.template.wantGenerateFragment(r.fhirType(), code);         // (3) the template
}
```
1. **`wantGenParams`** — global on/off switches from `ig.ini`/IG `parameters` (`generate` params).
   Default: absent ⇒ allowed.
2. **`igpkp.wantGen`** — per-resource policy from the template's `config.json` `"defaults"` block
   (`ig-template-base/config.json:85`, `"Anything not mentioned defaults to true"`). This block maps
   *resource types to page LAYOUTS* (`template-base`, `template-defns`, `template-mappings`, …), not to
   individual fragments; it governs which PAGES exist, and defaults to true.
3. **`template.wantGenerateFragment(type, code)`** — the only place the template can suppress an
   individual FRAGMENT. See below. **In default builds it always returns `true`.**

### `template.wantGenerateFragment` — the "rapido" on-demand path (OPT-IN, non-default)
`templates/Template.java:624`:
```java
public boolean wantGenerateFragment(String s, String code) {
  if (usedFragmentTypes == null) return true;   // no scan → produce everything
  if (!rapido)               return true;   // <<< DEFAULT: produce everything
  ... consult usedFragmentTypes (scanned from template includes) ...
  // returns false for any fragment the template never {% include %}s
}
```
- `rapido` is set **only** when the CLI is invoked with `-rapido` or `-cascais`
  (`publisher/Publisher.java:1461` → `settings.setRapidoMode(true)`); `PublisherSettings.rapidoMode`
  defaults `false` (`PublisherSettings.java:24`). Normal HL7 builds do **not** pass it.
- When `rapido` IS on, `usedFragmentTypes` comes from `TemplateFragmentTypeLoader.process(templateDir)`
  (`Template.java:189`). That loader (`templates/TemplateFragmentTypeLoader.java`) walks the template's
  `.html` files, regex-matches `{% include %}` statements
  (`INCLUDE_PATTERN = \{%\s*include\s+…`, `:16`), and extracts the
  `<prefix>-{{[id]}}-<suffix>.xhtml` pattern (`:97`) to build a map of
  `resourceType → {referenced suffixes}`. In rapido mode a fragment is produced only if its
  `type`/`{{[type]}}` prefix + suffix appears in that scanned set (plus a hardcoded always-on list:
  `xml,json,ttl,csv,xlsx,sch,xml-html,json-html,ttl-html,jekyll-data`, `Template.java:643`).
- `Template.rapidoSummary()` (`:651`) prints e.g. "don't produce N of M fragments" — confirming rapido
  is a *reduction* off the full eager menu.

**Verdict on decision mechanism:** By default the **publisher's own Java code decides**, and it emits
the *complete menu* for every resource. The template only ever *subtracts*, and only in the opt-in
`-rapido` mode by scanning its `{% include %}` references. The stock HL7 template chain merely
**CONSUMES** whatever the publisher wrote (its layouts do `{% include {{[type]}}-{{[id]}}-history.xhtml %}`
etc., e.g. `ig-template-base/layouts/layout-profile-history.html:17`).

---

## (b) Complete-menu vs on-demand — verdict + evidence

**Verdict: default = COMPLETE MENU, eagerly generated, mostly unused.** On-demand generation exists
only as the opt-in `-rapido`/`-cascais` mode.

### Direct evidence from the cycle build output (`…/cycle/temp/pages/_includes`)
The cycle IG (`me.fhir.period-tracking-mvp`) has **11 conformance resources** in
`fsh-generated/resources/` (7 StructureDefinition, 1 CodeSystem, 2 ValueSet, 1 ImplementationGuide) +
example instances (6 example JSONs in `input/`, a longitudinal Bundle, etc.) — ~17 artifacts total once
examples/instances are counted, matching the "17-resource IG" framing.

Per-resource fragment inventory (base `.xhtml`, i.e. excluding the duplicated `-en` language copies):
- **One StructureDefinition (`menstrual-flow`) produces 79 base fragments.** Full list:
  `adl adl-all class-table contained-index crumbs ctxts dict dict-active dict-diff dict-key dict-ms
  diff diff-all diff-bindings diff-bindings-all diff-obligations diff-obligations-all eview eview-all
  expansion experimental-warning grid header history html inv inv-diff inv-key ip-statements json-html
  json-schema maps maturity obligations obligations-all other-versions pseudo-json pseudo-ttl pseudo-xml
  sd-changes sd-use-context sd-xref search-params shex snapshot snapshot-all snapshot-bindings
  snapshot-bindings-all snapshot-by-key snapshot-by-key-all snapshot-by-key-bindings
  snapshot-by-key-bindings-all snapshot-by-key-obligations snapshot-by-key-obligations-all
  snapshot-by-mustsupport snapshot-by-mustsupport-all snapshot-by-mustsupport-bindings
  snapshot-by-mustsupport-bindings-all snapshot-by-mustsupport-obligations
  snapshot-by-mustsupport-obligations-all snapshot-obligations snapshot-obligations-all span spanall
  status summary summary-all summary-table ttl-html tx tx-diff tx-diff-must-support tx-key
  tx-must-support typename uses validate validation xml-html`
  (158 files once `-en` copies are included).
- **Whole-IG totals:** `_includes` holds **10,924** `.xhtml` files (= **5,462** base + 5,462 `-en`
  duplicates), for ~17 artifacts.

### How many of the 79 are actually consumed by this IG's template chain?
Grepping every `{% include {{[id]}}-<suffix>.xhtml %}` reference across the full template chain
(`ig-template-base` + `ig-template-fhir` + cycle's own `template/`):
- The chain references only **~36 distinct fragment suffixes total** across ALL resource types.
- For a StructureDefinition, **only 27 of the 79 produced base fragments are referenced anywhere** in
  the chain. **52 of 79 are produced but never `{% include %}`d** — e.g. every `*-obligations`,
  `*-bindings`, `*-by-mustsupport*`, `dict-active`, `dict-ms`, `adl/adl-all`, `grid`, `eview/eview-all`,
  `span/spanall`, `experimental-warning`, `other-versions`, `search-params`, `json-schema`, `shex`,
  `status`, `maturity`, `validate/validation`. This is the "complete menu the template could ever use,
  mostly unused" you described — quantified: **~66% of per-SD fragments are dead weight for this IG.**

(The stock chain has separate profile layouts — `snapshot`, `snapshot-by-key`, `snapshot-by-mustsupport`
tabs — so a *maximal* profile template consumes more than cycle's does; but even the full stock base
never references the obligations/bindings permutations, ADL, shex, json-schema, etc. that the publisher
always writes.)

---

## (c) Role of `fragment-usage-analysis.csv` + incremental skip logic

### What the CSV is
`generateFragmentUsage()` at `PublisherGenerator.java:6386` writes `<outputDir>/fragment-usage-analysis.csv`
(`:6417`). Columns: `Fragment,Count,Time (ms),Size (bytes)` — with a 5th `Used?` column **only if
`settings.isTrackFragments()`** (`:6396`). Each row is one fragment *kind* (`context.code`, e.g.
`StructureDefinition.snapshot`), and `Count` is how many times `fragment()` was invoked for that kind
(`FragmentUseRecord.record()` at `PublisherBase.java:1299`).

**Crucially, the CSV is a PRODUCTION log, not a consumption/needs list.** It lists what the publisher
*wrote*, sorted `used`-first then `unused` (`:6401` vs `:6409`). Observed real files confirm this:
- `…/period-fhir/kit/period-tracking-mvp-ig/output/fragment-usage-analysis.csv`: 4,239 rows,
  **0 with Count==0, no `Used?` column** (4,157 of them are `Cross…` cross-version rows). Every listed
  fragment has Count≥1 because a row only exists once `fragment()` ran.
- `…/sushi-rs/temp/top20/pdex/output/fragment-usage-analysis.csv`: 4,246 rows, same shape, no `Used?`.

### The `Used?` column and true "unused" detection (opt-in only)
"Used" ≠ "produced". A fragment is only marked used when its rendered marker is later found in a final
HTML page:
- With `-trackFragments`, `fragment()` appends `<!-- fragment:context.code -->` to the content
  (`PublisherGenerator.java:2464`).
- `HTMLInspector` scans generated HTML for `"<!-- fragment:"` (`HTMLInspector.java:484`) and calls
  `fragmentUses.get(v).setUsed()` (`:488`); wired only when tracking is on
  (`PublisherIGLoader.java:1026` passes `pf.fragmentUses` only `if isTrackFragments()`).
- `trackFragments` defaults `false` (`PublisherSettings.java:27`); enabled by `-trackFragments`
  (`ui/IGPublisherUI.java:38`, `Publisher.java:1482`). So in normal builds the "Used?" data doesn't
  even exist — the publisher does not know or care which of its 79 SD fragments a page consumed.

### Incremental skip logic
- `checkMakeFile` (used by `fragment()` at `:2473`) is a content-hash/`allOutputs` guard: it avoids
  rewriting a fragment whose bytes are unchanged, and tracks output names. This is byte-level dedup, NOT
  a "skip because unused last build" mechanism.
- There is **no cache that skips regenerating fragments that went unused in a prior build.** The only
  "produce less" path is rapido's up-front template scan (§a), which is a *this-run* decision, not a
  cross-build usage cache. (Regeneration granularity is per-changed-`FetchedFile` via the `regen` flag,
  not per-fragment-usage.)

---

## (d) Implications for OUR approach (cycle site-gen live from Resources.Json; sushi-rs site.db)

**Yes — the Publisher's model is "everything, eagerly"; ours is "on demand."** The publisher's default
is the eager complete menu (79 fragments/SD, ~5,462 base fragment files for ~17 artifacts, ~66% never
included). Our renderers instead compute a view when a page/route needs it:

- **cycle site-gen** (`…/cycle/site-gen/`) renders per-artifact React pages on demand —
  `ProfilePage.tsx` builds Key/Differential/Snapshot element tables via `ElementTable`/`elementViews`
  (`ProfilePage.tsx:131-135`), an Examples section (`:75-93`), and machine formats
  (`MachineFormats.tsx`). `fhir/fragments.ts` is a live directive-driven renderer (json/xml payloads,
  `elide`/`except`) — it materializes the *data-payload* fragments, not the publisher's derived-analysis
  menu. This is structurally the rapido idea done natively: produce a view only where consumed.
- **sushi-rs site.db pipeline**: same philosophy — derive HTML views from the resource DB per route
  rather than pre-baking a fixed fragment set.

### Fragment kinds in the Publisher's menu that our renderer has NO equivalent for yet
Cross-referencing the 79-kind SD menu (and CS/VS menus) against what cycle/site.db renders. cycle
already covers: snapshot/differential/key element tables, examples list, machine formats
(json/xml/ttl code blocks), dependency/globals/ip-statements includes
(`project/includes.ts:53-73`), CodeSystem content, ValueSet expansion pages. **Missing equivalents:**

1. **cross-version-analysis** — explicitly stubbed/omitted; "not derivable from package.db"
   (`site-gen/project/includes.ts:76-78`). The publisher's biggest fragment category by row count.
2. **xref / "used-by" / reverse references** (`sd-xref`, CodeSystem/ValueSet `xref`, `uses`,
   `sd-use-context`) — who references this profile/CS/VS across the IG.
3. **Mappings** (`maps`) — the profile "Mappings" tab (mapping to other specs).
4. **Terminology bindings / obligations permutations** — `*-bindings`, `*-obligations`,
   `*-by-mustsupport*`, `tx`, `tx-diff`, `tx-must-support`, `tx-key` (must-support & obligation views).
5. **Invariants/constraints tables** (`inv`, `inv-diff`, `inv-key`) as standalone fragments (cycle shows
   `formal-constraints` inline but not the full publisher invariant grid).
6. **Data dictionary** (`dict`, `dict-diff`, `dict-ms`, `dict-key`, `dict-active`) — the long-form
   element dictionary page.
7. **Downloadable/alt renderings**: CSV / XLSX (`all-profiles.csv`, `.xlsx`), Turtle (`.ttl`),
   ShEx (`shex`), JSON-Schema (`json-schema`), UML (`uml`), ADL (`adl`) — cycle covers json/xml/ttl code
   views but not csv/xlsx/shex/json-schema/uml/adl.
8. **Expansion as a fragment for profiles** (`StructureDefinition.expansion`), `grid`, `span`/`spanall`
   (aggregated cross-profile spanning tables), `experimental-warning`, `maturity`/`status`,
   `other-versions`.

For OUR pipeline the practical takeaway: we don't need to match the eager menu 1:1 — most of it is
never included even by the stock template. The high-value gaps to prioritize are the ones a normal IG's
pages actually reference: **xref/used-by, mappings, invariant tables, terminology binding/tx views, the
data dictionary, and downloadable csv/xlsx/ttl/shex/json-schema**. cross-version-analysis is genuinely
outside a single-package DB and is reasonable to keep stubbed.

---

## One-line source map (most load-bearing lines)
- Eager per-SD emitter (113 fragment calls): `PublisherGenerator.java:1808`
- Physical write per fragment: `PublisherGenerator.java:2463` (write at `:2473`)
- The gate: `wantGen` `PublisherGenerator.java:2146`
- Template subtract-only, default-true; rapido scan: `Template.java:624` (`!rapido → true` at `:628`)
- Template `{% include %}` scanner: `TemplateFragmentTypeLoader.java:16,97`
- rapido opt-in flag: `Publisher.java:1461`; default false `PublisherSettings.java:24`
- CSV writer (production log): `PublisherGenerator.java:6386` / file at `:6417`
- Usage record: `PublisherBase.java:1292` (`record` `:1299`, `setUsed` `:1305`)
- `Used?` only with tracking: `HTMLInspector.java:484-488`; default false `PublisherSettings.java:27`
- Template config `defaults` (page policy, not fragments): `ig-template-base/config.json:85`
- Templates CONSUME publisher fragments: `ig-template-base/layouts/layout-profile-history.html:17`
- Our on-demand renderers: `cycle/site-gen/fhir/ProfilePage.tsx:131`, `cycle/site-gen/fhir/fragments.ts`,
  `cycle/site-gen/project/includes.ts:76`

---

## Part 2 — follow-up (Josh's challenges)

Same pins as Part 1 (publisher `2.2.10`; `ig-template-base`=`fhir.base.template`,
`ig-template-fhir`=`hl7.fhir.template`). New evidence: 15 real IG repos read passively from
`scratchpad/ig-survey/`; `davinci-pdex-plan-net` + `US-Core` cloned fresh to `scratchpad/ig-survey-mine/`.

### Q1 — Representative waste for stock-template IGs

**(a) Cycle's template is atypical only in NAME, not in behaviour.** Cycle declares
`template = fhir2.base.template` (`cycle/ig-gh-actions.ini:3`), NOT stock `hl7.fhir.template`. But the
materialized `cycle/template/` layer is a near-clone of stock `fhir.base.template`: `diff` of
`cycle/template/layouts` vs `ig-template-base/layouts` = exactly ONE addition (`layout-questionnaire.html`)
plus two extra includes (`fragment-feedback_form.html`, `fragment-language.html`). It **restricts
nothing** and its config.json `defaults` block is the same page-layout map. So cycle's per-SD numbers are
effectively the pure-stock numbers.

**(b) Reference set from the PURE stock chain.** Generated N = distinct base `.xhtml` (excluding `-en`
dupes) actually produced in `cycle/temp/pages/_includes` (SD `menstrual-flow` = 79, ground-truthed).
M = suffixes the stock `ig-template-base` layouts actually `{% include %}` (union over the layouts each
type uses per `config.json` `defaults`). The generated menu per type was enumerated from the
`wantGen(r,"code")` guards in `PublisherGenerator` (CodeSystem `:1478`, ValueSet `:1528`,
StructureDefinition `:1808`, plus the shared per-resource path `:950-1347` that adds
`html/json-html/xml-html/ttl-html/status/maturity/validate/validation/ip-statements/history`).

| Type | N (produced) | M (ref by stock) | Dead K | Dead % |
|---|---|---|---|---|
| StructureDefinition | 79 | 25 | 54 | 68% |
| ValueSet | 18 | 5 | 13 | 72% |
| CodeSystem | 17 | 4 | 13 | 76% |
| Example instance | 10 | 3 | 7 | 70% |
| ImplementationGuide | 11 | 0 | 11 | 100% |

SD M(25) = contained-index, dict, dict-diff, dict-key, diff, diff-all, history, inv, inv-diff, inv-key,
maps, pseudo-json/ttl/xml, sd-use-context, sd-xref, snapshot, snapshot-all, snapshot-by-key,
snapshot-by-key-all, summary, summary-all, tx, tx-diff, tx-key
(`ig-template-base/layouts/layout-profile.html`, `layout-ext.html`, `layout-profile-definitions.html`,
`layout-profile-mappings.html`, `layout-profile-history.html`).
SD dead(54) = every `*-obligations`, `*-bindings`, `*-by-mustsupport*`, `adl/adl-all`, `class-table`,
`crumbs`, `ctxts`, `dict-active`, `dict-ms`, `eview/eview-all`, `expansion`, `experimental-warning`,
`grid`, `header`, `html`, `json-schema`, `maturity`, `other-versions`, `sd-changes`, `search-params`,
`shex`, `span/spanall`, `status`, `summary-table`, `tx-must-support`, `tx-diff-must-support`, `typename`,
`uses`, `validate/validation`, `xml-html/json-html/ttl-html`, `ip-statements`. (Cycle's own tabbed layer
consumes 27 vs the pure-stock 25 — the extra 2 are `snapshot`/tab permutations; the correction
27→25 is the "cycle's own template layer" contamination Josh flagged.)

This corrects Part 1's "27/79". The pure-stock number is **25/79 referenced, 54 dead (68%)**.

**(c) Sanity-check + the authored-pagecontent channel.** Ground-truthed against the real cycle run
(`cycle/temp/pages/_includes`, 5,462 base `.xhtml`). Authored `pagecontent` CAN `{% include X.xhtml %}`,
but across **15 real IGs** (`scratchpad/ig-survey/*`), authored pagecontent contains **ZERO** per-resource
`Type-id-suffix.xhtml` includes. It only pulls IG-level aggregates — union across all 15:
`ip-statements`, `dependency-table(-short)`, `globals-table`, `cross-version-analysis`, `expansion-params`,
`table-{profiles,valuesets,codesystems,extensions,conceptmaps,capabilitystatements,searchparameters,
operationdefinitions}`, `list-{simple-operationdefinitions,requirements,capabilitystatements}`,
`summary-observations` (~20 kinds). **How much does this move M for per-resource types? Zero.** The authored
channel is a separate, small, bounded IG-level set; it does not touch the per-SD/VS/CS menu. (Whole-IG the
biggest categories are `list-*` = 2730 files and `table-*` = 1400 files — all IG-level aggregates, not
per-resource, confirming the authored channel is aggregate-only.)

### Q2 — Rapido's regex brittleness

`TemplateFragmentTypeLoader` (`:16` INCLUDE_PATTERN, `:97` processIncludes). Two stages:
`INCLUDE_PATTERN` captures the first whitespace-free token after `include`;
`processIncludes` keeps only tokens containing literal `{{[id]}}` matching
`^(.+?)-\{\{\[id\]\}\}-(.+?)(\{\{\[langsuffix\]\}\})?\.xhtml$`, then splits prefix/suffix (expanding
`{{format}}`→json/xml/ttl). Resolution at `Template.java:624 wantGenerateFragment`: check
`usedFragmentTypes.get(type)` then fall back to `usedFragmentTypes.get("{{[type]}}")` (`:637`) then a
hardcoded always-on list (`:643`).

**Tested against the real stock chain: 55 unique include tokens → 35 parse, both the literal-prefix
(`StructureDefinition-{{[id]}}-snapshot.xhtml`) and generic (`{{[type]}}-{{[id]}}-history.xhtml`, caught by
the `{{[type]}}` fallback) forms.** It IGNORES control flow — includes wrapped in `{% if %}`/tab blocks
(`layout-profile.html:84-274`) are treated as unconditionally referenced. That is the SAFE direction:
superset → over-generate, never under-generate. **Only genuine miss:** `{{[type]}}-{{[name]}}.xhtml`
(`layout-instance-format.html:30`, uses `{{[name]}}` not `{{[id]}}` → silently skipped), and even that is
backstopped by the always-on `xml-html/json-html/ttl-html` list. It would break on `assign`/`capture`-built
names, includes-from-data, or `{% include {{var}} %}` — legal Liquid, absent from the stock chain, so
fragment need is undecidable statically in the general case.

**Verdict:** sound-but-underengineered (safe, because it errs toward over-generation), not fundamentally
wrong for stock templates — but fragile for arbitrary templates. **Recommended robust mechanism for the
Rust reimpl: don't statically scan. Resolve includes at render time and generate-on-first-include-miss**
(lazy materialization keyed by the include actually requested). Nothing in the stock model breaks this:
no template probes a fragment's *existence* (no `{% if fragmentexists %}`); every include is an
unconditional consume. First-miss generation is therefore complete AND eliminates both the dead-weight
(§Q1) and the scanner.

### Q3 — Can templates define/shape fragments?

The fragment-KIND universe IS closed in Java (`wantGen`-gated `fragment()` calls, write at
`PublisherGenerator.java:2473`), but "hardcoded Java menu, template composes only" undersells three real
template-side channels:

1. **`liquid/` dir = a template-defined PER-RESOURCE rendering channel.**
   `IGPublisherLiquidTemplateServices.load()` (`:41-53`) keys `<ResourceType>.liquid` by
   `fhirType().toLowerCase()` and serves it via `findTemplate(ctx, resource)` (`:57-61`). Stock base ships
   `Measure/Library/ActivityDefinition/PlanDefinition.liquid`. These SHAPE the narrative/`-html` CONTENT the
   publisher emits for those types (they don't add fragment kinds).
2. **ant `onGenerate.processIncludes` writes into the consumed `_includes` namespace.**
   `ig-template-base/scripts/ant.xml:120-135` `<copy>`s `template/includes/*` into `${temp}/_includes` and
   generates `artifacts.xml` (XSLT `createArtifactSummary.xslt`) + plantUML SVGs there, via the
   `onGenerate.extend` extension-point (`:139`). A template-injected include channel distinct from Java.
3. **`extraTemplates` (config.json) registers new page LAYOUTS/tabs** (mappings, testing, examples, format,
   profile-history), iterated in `PublisherGenerator.java:1019,1039`
   (`for (templateName : extraTemplates.keySet())`). New pages/tabs; fragment kinds inside still Java.

Plus **parameterization that reshapes fragment content:** `tabbed-snapshots` (`PublisherIGLoader.java:652`
→ `tabbedSnapshots` threaded into every `sdr.snapshot/diff/byKey/tx` call), and `no-narrative`/`generate`
params (`PublisherIGLoader.java:336,342`).

**Verdict:** the kind-set is closed (Java), but CONTENT and PAGE-composition are template-open. A Rust
reimpl's compatibility surface must reproduce four things, not one: (a) the closed Java kind menu,
(b) the per-type `liquid` narrative renderer channel, (c) the template-injected `_includes` aggregate files
(ant), (d) the `tabbed-snapshots`/`no-narrative`/`generate` shaping. Only (a) is fixed; (b)-(d) are the
template-defined surface.

### Source map (Part 2)
- Cycle template decl: `cycle/ig-gh-actions.ini:3`; near-identical to stock (diff = +`layout-questionnaire`)
- Stock layout→fragment map: `ig-template-base/config.json:85-` (`defaults`), `layouts/layout-profile.html`,
  `layout-ext.html`, `layout-profile-definitions.html`, `layout-valueset.html`, `layout-codesystem.html`,
  `layout-instance-base.html`
- Generated menu enumerated via `wantGen` guards: `PublisherGenerator.java:1478,1528,1808,950-1347`
- Authored pagecontent survey: `scratchpad/ig-survey/*/input/pagecontent` (15 IGs, 0 per-resource includes)
- Rapido loader: `TemplateFragmentTypeLoader.java:16,97`; resolution `Template.java:624,637,643`
- Liquid per-resource channel: `IGPublisherLiquidTemplateServices.java:41-61`; `ig-template-base/liquid/*.liquid`
- ant include injection: `ig-template-base/scripts/ant.xml:120-139`
- extraTemplates iteration: `PublisherGenerator.java:1019,1039`; `Template.java:524-535`
- Params: `PublisherIGLoader.java:336,342,652`

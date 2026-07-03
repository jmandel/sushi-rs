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

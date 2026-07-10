# Rust Fragment Generator + Minimal-Jekyll: Feasibility Study

> **HISTORICAL FEASIBILITY STUDY — not a current handoff design.** Its renderer
> inventory and oracle analysis remain useful, but its `site.db`-centered
> proposals predate renderer-neutral `SiteBuild`, the typed native
> `ArtifactResolver`, and Cycle's closed v2 contract. Use the repository
> [`README.md`](../README.md), [`hosting.md`](hosting.md), and
> [`crates/site_build/README.md`](../crates/site_build/README.md) for current
> APIs.
>
> Status: FEASIBILITY STUDY (task #23, 2026-07-03). READ-ONLY analysis, no code.
> Question: can a Rust fragment generator + a minimal-Jekyll layer, built around
> the `site.db` abstraction, produce **fully equivalent builds** for existing IGs
> using the **stock base/HL7 templates**?
>
> **This document makes no decisions.** It maps the territory: scope, architecture,
> the renderer-sizing table (the new fourth-pillar analysis), oracle/gating, phases,
> effort, and ranked risk. Where a design choice exists it is *proposed with
> trade-offs*, not decided.
>
> Input pillars (all in `docs/`): `publisher-fragments-notes.md` (Parts 1+2),
> `ig-jekyll-surface-survey.md` (+ scope decision), `cycle-package-db-plan.md`,
> `snapshot/REWORK-PLAN.md` §9, `layer-b-audit.md`. Fourth pillar (renderer sizing)
> is produced here from the pinned `fhir-core@6.9.10-SNAPSHOT`
> (`/home/jmandel/hobby/fhir-perf/repos/fhir-core`) and IG-Publisher `2.2.10`
> (`/home/jmandel/hobby/fhir-perf/repos/ig-publisher`).

---

## 1. Executive summary

**Feasible? Yes — with a sharply asymmetric effort profile.** The *page layer*
(minimal-Jekyll T1+T2 + a kramdown subset) and the *fragment plumbing* (which
fragments to produce, when, and how they hang off `site.db`) are already
90%-designed and low-risk: the surface has been measured, the mechanism
(render-time first-include-miss generation) is chosen, and the incremental
substrate (two-ledger design, `site.db` contract) exists. The *fragment
*content** — the HTML the publisher's Java renderers emit — is where the real
mass is, and it concentrates almost entirely in **one shared engine**.

**The 3 numbers that matter:**

1. **25 / 5 / 4 / 3** — the fragment kinds a pure-stock build actually
   `{% include %}`s per resource type (SD/VS/CS/instance), out of the
   79/18/17/10 the publisher eagerly emits. First-include-miss generation
   collapses the target from "the eager menu" to "the used set." This is the
   number that makes the project tractable.
2. **~1 shared XL cluster** carries the bulk of the SD fragment value. The
   `generateTable` element-table engine (fhir-core SDR lines ~613–3550,
   **~2,900 LOC**) plus the `HierarchicalTableGenerator` xhtml table builder
   (**1,503 LOC**) is the single dependency behind `snapshot`, `snapshot-all`,
   `diff`, `diff-all`, `byKey`, `byMustSupport`, `obligations`, `bindings`,
   `grid`, and the extension table. Build this one engine to parity and ~15 of
   the 25 stock SD fragments fall out of it.
3. **955/955 snapshot parity across 34 IGs, ~2 days wall for the walk engine.**
   That is the calibration datum (REWORK-PLAN §9). The renderer work is *wider*
   than the walk (more distinct output surfaces, weaker per-fragment oracle
   isolation) but rides the *same oracle discipline* and the *same already-built
   inputs* (walk snapshots, expansion rows, `site.db` projections).

**Shape of the effort.** Not one big push — a **spine + leaves** shape. The spine
is the `generateTable` engine (XL) and the minimal-Jekyll engine (M, mostly
designed). Once the spine is at HTML-golden parity, the leaves (summary tables,
dict, tx views, VS expansion, CS content, instance narrative, the ~20 IG-level
aggregates) are independently sized S–L items harvested and gated one fragment
kind at a time against real publisher goldens from our existing 34-IG cycle-regen
corpus. **Rough envelope: 6–10 engineer-weeks** to "stock-template IGs build
byte-equivalently for the used fragment set + T1/T2 pages," dominated by the two
XL items (element-table engine; kramdown-fidelity) and the long tail of
per-kind golden-chasing. This is bigger than the walk rework and should be
scoped as a multi-wave effort, not a sprint.

---

## 2. Scope — what "fully equivalent builds" means

"Fully equivalent" is defined operationally, not aspirationally. A build is
equivalent iff, over the corpus, **every page the stock template chain produces
is byte-identical (post minimal normalization, §5) to the Java Publisher's page**
— which decomposes into two provably-separable halves plus a compatibility
contract.

### 2.1 The fragment set = used set + first-include-miss for the rest

We do **not** reproduce the publisher's eager complete menu (79 SD / 18 VS /
17 CS / 10 instance / ~20 IG-level aggregates). Per `publisher-fragments-notes.md`
Part 2, the stock chain `{% include %}`s only:

| Type | eager menu (N) | stock-referenced (M) | dead (never included) |
|---|---:|---:|---:|
| StructureDefinition | 79 | **25** | 54 (68%) |
| ValueSet | 18 | **5** | 13 (72%) |
| CodeSystem | 17 | **4** | 13 (76%) |
| Example instance | 10 | **3** | 7 (70%) |
| ImplementationGuide | 11 | 0 (+~20 IG-level aggregates authored-channel) | — |

**Mechanism (chosen, from Part 2 §Q2):** *render-time first-include-miss
generation.* When the minimal-Jekyll engine resolves `{% include Type-id-suffix.xhtml %}`
and the fragment is absent, it invokes the Rust renderer for that
(type, id, suffix) triple, materializes the fragment, and continues. Nothing in
the stock model probes fragment *existence* (`publisher-fragments-notes.md` Part 2:
no `{% if fragmentexists %}`; every include is an unconditional consume), so
first-miss generation is provably **complete** for stock templates and
simultaneously eliminates both the 68% dead weight and the rapido static-scan.
The *scope* of renderers we must build is therefore exactly the **union of stock
`{% include %}` suffixes** — the M column above, ~34 distinct suffixes total —
not the eager menu.

### 2.2 The page pass = T1 + T2 Liquid + kramdown subset

Per `ig-jekyll-surface-survey.md` (+ Josh's 2026-07-03 scope decision):

- **T0** (48.6% of 939 pages): plain markdown — GFM tables + raw-HTML passthrough
  + kramdown IAL.
- **T1** (→92.2% cumulative): `include`, `{{site.data.fhir.path}}` /
  `{{site.data.fhir.ver.*}}` / `page.*`, `assign`/`capture`/`comment`/`raw`,
  the string filter set, kramdown IAL / `{:toc}` / `no_toc`.
- **T2 (IN SCOPE)**: `{% for %}` (`forloop.*`, `offset:`, `limit:`),
  `if`/`elsif`/`unless` with `contains`/`.size`, array filters
  `split|where|sort|uniq|map|join`, parameterized includes
  (`{% include x.md k=v %}` + `include.param`, incl. US-Core's `where:` data-joins).
  67 of 73 T2 pages are US-Core; T2's gate is US-Core's own subset rendered
  byte-comparable to Jekyll.

### 2.3 The 4-part compatibility surface (from Part 2 §Q3)

The fragment *kind*-set is closed in Java, but content & page composition are
template-open. A faithful reimpl must reproduce **four** things:

- **(a) the closed Java kind menu** — fixed; this is what §2.1 + §4 size.
- **(b) the per-type `<ResourceType>.liquid` narrative channel** —
  `IGPublisherLiquidTemplateServices` keys `Measure/Library/ActivityDefinition/PlanDefinition.liquid`
  by fhirType; these *shape the `-html` narrative content*, not add kinds.
- **(c) template-injected `_includes` aggregates via ant** — `ant.xml`
  copies `template/includes/*` into `_includes` and generates `artifacts.xml`
  (+ plantUML SVGs). A template-side include channel distinct from Java.
- **(d) content-shaping parameters** — `tabbed-snapshots`, `no-narrative`,
  `generate` — thread into every `snapshot/diff/byKey/tx` call.

### 2.4 Explicitly OUT

- **rapido / cascais** — obviated by first-include-miss (§2.1).
- **The 54 dead SD / 13 dead VS/CS fragments** — not produced (never included).
- **`-langsuffix` i18n beyond copy** — the publisher duplicates every fragment
  as `-en` (10,924 files = 5,462 × 2). *Proposal (decide later):* emit the base
  fragment and, for stock single-language builds, treat `-en` as a byte-copy of
  base with the suffix substituted — matching the observed `-en` = base duplication.
  Full i18n translation (`lang-fragment` alt-language content) is a separate,
  larger effort; **out** for v1, flagged in §7.
- **shex / json-schema / uml / adl / csv / xlsx** — all in the 54 dead SD set;
  not included by stock; **out**.
- **cross-version-analysis** — genuinely outside a single-package DB
  (`cycle-package-db-plan.md`: "not derivable from package.db"); **stubbed**.
- **plantUML** — SVGs pre-rendered/committed under `input/images/`, producer
  fails loud on missing (per `cycle-package-db-plan.md` §2b); **out**.
- **QA / `qa.json` / HTMLInspector** — Java-only; dropped (walk-engine messages
  channel is the future QA source).
- **`{% sql %}`** — IG-Guidance-only extension; keep cycle-specific, not core.
- **Decide-and-state:** front-matter (`---`) in authored pages (1 page total,
  effectively absent); layout inheritance / `{% layout %}`; collections /
  arbitrary `site.data.*` sprawl; `case`/`highlight`/`tablerow`/`cycle` (measured
  zero) — all OUT, per `ig-jekyll-surface-survey.md` §(d).

---

## 3. Architecture — around `site.db`

The design reuses the `site.db` contract and the pipeline stages already
specified in `cycle-package-db-plan.md` (S1–S7) and inserts fragment generation
and the Jekyll layer as **derived, content-hash-keyed artifacts** driven off it.
Everything below is a *proposal*; the load-bearing decisions (fragments in-DB vs
beside-DB; liquid engine in Rust vs kept in TS; ant channel honored vs replaced)
are called out as such.

### 3.1 Fragments as derived artifacts keyed by content hash

Fragments are pure functions of `site.db` rows (a resource's `Resources.Json` +
its snapshot + expansion rows + IG metadata) plus a small render context
(corePath, canonical, `tabbed-snapshots`/`no-narrative` params, language).
Therefore each fragment is addressable by
`hash(kind, resource-content-hash, render-context-hash)` and is a **derived
artifact** in exactly the sense the two-ledger design already handles.

*Proposal A (fragments beside `site.db`):* materialize fragments into a
`Fragments(name, lang, content_hash, bytes)` table **inside** `site.db`, so the
existing renderer-side ledger (Ledger 2) observes fragment reads through the
single `core/db.ts` choke point for free, and incremental rebuilds skip
unchanged fragments by content hash. *Proposal B (sidecar `_includes` dir):*
write physical `_includes/*.xhtml` to match the publisher's on-disk shape
exactly (maximizes template-chain compatibility; costs a second dependency
surface the ledger must track). **Trade-off:** A keeps the "single artifact"
discipline of `cycle-package-db-plan.md` §2b and the wasm path open; B is a
drop-in for any tool that expects real files. Not decided here.

### 3.2 First-include-miss generation as the trigger

The minimal-Jekyll engine owns the `{% include %}` resolver. On a miss for a
`Type-id-suffix.xhtml` pattern it dispatches to the Rust fragment renderer
(§4 map: suffix → renderer entry point), which reads only `site.db` rows for
`(Type, id)` and the render context, produces bytes, records the fragment (§3.1),
and returns them inline. This is the natural home for the two content-shaping
params (§2.3d): they enter the render context, not the resolver. **No static
template scan; no eager menu; generation scope = actual include set.**

### 3.3 The two-ledger incrementality (already designed)

`cycle-package-db-plan.md` §2c: Ledger 1 (Rust producer, `BuildState`
`node_key→input_hash→output_hash`) already tracks source→row lineage. Ledger 2
(renderer, `RenderDeps` per-page read-set replay) already handles
"re-render iff replayed queries change." **Fragments slot in as Ledger-2
observed reads** (each fragment's inputs are `site.db` SELECTs through the same
proxy), so a resource edit invalidates exactly its fragments and exactly the
pages that included them — no new incrementality mechanism is required. This is
the single biggest reason the plumbing is low-risk: the substrate exists and is
day-1 scope already.

### 3.4 Where the Liquid engine sits

Two options, both viable:
- *Proposal C (Rust-native minimal-Jekyll):* port the T1+T2 dialect into the
  Rust producer, emit finished HTML pages into `site.db`. Keeps one language,
  one binary, wasm-friendly; costs a Liquid+kramdown reimplementation the
  cycle TS side (`core/liquid.ts` + `markdown.ts`) has already partly done.
- *Proposal D (keep the TS renderer):* the Rust producer emits `site.db` +
  fragments; the existing locked-down LiquidJS (`core/liquid.ts`) renders pages,
  extended to T2 (`for`, richer `if`, `split/where/sort/uniq`, param includes —
  the gaps enumerated in `ig-jekyll-surface-survey.md` §(e)). This is the
  *smaller* immediate step (T2 is an add-on to a proven T1 engine) and matches
  cycle's current shape. **Trade-off:** C is the end-state for a fully-Rust
  wasm build; D reaches "equivalent builds" fastest by reusing proven markdown
  handling (kramdown IAL, `markdown="1"` re-entry — the §7 fidelity risk). The
  doc does not decide; the phasing (§6) is written so either can be chosen at
  the Phase-2 boundary.

### 3.5 Honoring vs replacing the template ant/extraTemplates channels

- **ant `onGenerate.processIncludes` (§2.3c):** copies `template/includes/*` →
  `_includes` and builds `artifacts.xml` + plantUML. *Proposal:* honor the
  static `<copy>` of `template/includes/*` (a plain file copy the producer's S6
  include-scan already models); **replace** the generated `artifacts.xml` with a
  Rust producer of the same aggregate (it is derivable from `site.db` resource
  rows via `createArtifactSummary.xslt`'s logic); **skip** plantUML (SVGs
  pre-committed, §2.4).
- **`extraTemplates` (config.json):** registers new page LAYOUTS/tabs
  (mappings, testing, examples, format, profile-history). These are page-layer,
  not fragment-layer: they add pages whose bodies `{% include %}` fragments we
  already generate. *Proposal:* the minimal-Jekyll layer reads the same
  `config.json` layout map so `wantGen`-equivalent page policy is honored;
  fragment kinds inside are unchanged. Not decided; sized as page-layer work.

---

## 4. The renderer sizing table (the fourth-pillar analysis)

**Method.** Each stock-referenced fragment suffix (§2.1 M-columns + the IG-level
aggregates) was traced from its `PublisherGenerator.fragment(...)` call to the
`sdr.`/`vsr.`/`csr.` method it invokes (publisher-side renderer wrappers at
`ig-publisher .../renderers/{StructureDefinition,ValueSet,CodeSystem}Renderer.java`),
then into the fhir-core renderer it delegates to
(`fhir-core .../r5/renderers/*`). LOC is the **approximate transitive render
logic** for that cluster (measured by `wc -l` of the owning method blocks and
their private helpers); it is deliberately conservative (shared helpers are
counted once, in the cluster row). Difficulty S/M/L/XL folds in LOC × branch
density × oracle-isolation.

### 4.1 The shared clusters (build these once)

| # | Cluster | Home (file:line) | ~LOC | Feeds which fragments | Difficulty |
|---|---|---|---:|---|---|
| **C1** | **Element-table engine** `generateTable`/`generateTableInner`/`genElement`/`genElementCells`/`genTypes`/`generateDescription`/`genFixedValue` | fhir-core `StructureDefinitionRenderer.java:613–3550` | **~2,900** | snapshot(-all), diff(-all), byKey, byMustSupport, obligations, *-bindings, grid, extension-table | **XL** |
| **C2** | **Hierarchical xhtml table builder** (indent images, hierarchy lines, gen anchors) | fhir-core `utilities/.../xhtml/HierarchicalTableGenerator.java` (1,503) | **~1,500** | the render target of **all of C1**; every element table bottoms out here | **XL** |
| **C3** | **xhtml model + composer** (`XhtmlNode` 1,506, `XhtmlComposer` 516) | fhir-core `utilities/.../xhtml/` | **~2,000** | *everything* — every fragment is an `XhtmlNode` tree composed to string | **L** (shared, but mechanical; whitespace/escape parity is the risk, §5) |
| **C4** | **Code resolution + links** (`CodeResolver`, `DataRenderer` code/link/display paths, `TerminologyRenderer`) | fhir-core `DataRenderer.java` (2,405) + `TerminologyRenderer.java` (343) + `CodeResolver.java` (50) | **~2,000** (subset used) | tx views, VS expansion/composition, CS content, bindings columns, instance narrative code cells | **L** |
| **C5** | **Obligations + additional-bindings** renderers | fhir-core `ObligationsRenderer.java` (618) + `AdditionalBindingsRenderer.java` (546) | **~1,160** | obligations, diff-obligations, snapshot-*-obligations, *-bindings columns | **M** |

**Honest verdict on the XL items:** C1 + C2 together (**~4,400 LOC**) are the
project's center of gravity. The publisher's `snapshot`, `diff`, `byKey`,
`byMustSupport`, `obligations`, and `*-bindings` methods are *all thin wrappers*
that call the **same** `sdr.generateTable(...)` with different flags
(verified: `StructureDefinitionRenderer.java:510/523/532/547` each call
`sdr.generateTable(...)` → `HierarchicalTableGenerator`). So the good news is
leverage — one engine, ~15 SD fragments; the bad news is you cannot ship *any*
SD table fragment to parity without most of C1+C2. This is the "XL, say so"
item: it is the walk-engine-of-the-renderer, and it should be scoped and gated
like the walk was.

### 4.2 StructureDefinition fragments (M = 25 stock-referenced)

| Fragment suffix | Publisher method (file:line) | fhir-core cluster | Diff | What we already have that feeds it |
|---|---|---|---|---|
| `snapshot`, `snapshot-all` | `SDR.snapshot():510` → `generateTable` | **C1+C2** | XL | walk snapshot.element (955/955 parity) |
| `diff`, `diff-all` | `SDR.diff():487` → `generateTable(diff=true)` | **C1+C2** | XL | differential rows in `Resources.Json` |
| `snapshot-by-key`, `-all` | `SDR.byKey():532` → `generateTable` on key-element subset | C1+C2 (+ key-element selection) | L | snapshot + key-element rule |
| `dict`, `dict-diff`, `dict-key` | `SDR.dict():1308` → `renderDict:3968`/`generateElementInner:4361` | dict cluster (`3968–4760`, ~790) | **L** | snapshot.element (per-element long form) |
| `inv`, `inv-diff`, `inv-key` | `SDR.invOldMode():1203` | invariant table (subset of C1 constraint logic) | M | `constraint[]` in snapshot (Layer-B xpath) |
| `tx`, `tx-diff`, `tx-key` | `SDR.tx():851` | **C4** (code resolution) | L | binding.valueSet + expansion rows (S4) |
| `maps` | `SDR.mappings():1323` | mappings block | M | `mapping[]` in SD |
| `summary`, `summary-all` | `SDR.summary():154` | small (summary assembly) | S | scalar cols + snapshot metadata |
| `summary-table` | `SDR.summaryTable()` | small | S | scalar cols |
| `uses` | `SDR.uses():1529` | **xref** (reverse-reference scan over `site.db`) | M | all Resources rows (query, not per-SD) |
| `sd-xref` / `xref` | `SDR.references():2254` | xref | M | all Resources rows |
| `sd-use-context` | `SDR.useContext()` | small | S | `useContext[]` |
| `contained-index` | shared per-resource path | small | S | contained resources |
| `pseudo-json`/`-xml`/`-ttl`, `template-*` | `SDR.pseudoJson():` + elementmodel composers | machine-format serializers | **M** | `Resources.Json` (json/xml/ttl composers) |
| `history` | shared per-resource path | small | S | resource metadata |
| `crumbs` | `SDR.crumbTrail()` | trivial | S | IG page tree |

Notes: `-en` duplicates of every SD fragment double the file count but are
byte-copies for single-language stock builds (§2.4). The dict cluster and the
inv table share slivers of C1 (per-element cell rendering) but have their own
top-level layout code (`renderDict`/`generateElementInner`, ~790 LOC).

### 4.3 ValueSet fragments (M = 5)

| Suffix | Publisher method | fhir-core (file:line) | Diff | We have |
|---|---|---|---|---|
| `expansion` | `vsr.cld` / expansion HTML | `ValueSetRenderer.generateExpansion:244` (~980 LOC to `1224`) | **L** | expansion rows (S4 tx client) |
| `cld` (compose/definition) | — | `generateComposition:1224` (~end) | L | VS compose + CS concepts |
| `summary` | `vsr.summaryTable:257(pub wrapper)` | small | S | scalar cols |
| `xref` | `vsr.xref` | xref | M | Resources rows |
| `changeSummary` | `vsr.changeSummary` | small/diff | S | prior-version (usually empty) |

VS expansion + composition share **C4** (code resolution / display / links) with
tx and CS. That is why C4 is a shared cluster worth building once.

### 4.4 CodeSystem fragments (M = 4)

| Suffix | Publisher method | fhir-core | Diff | We have |
|---|---|---|---|---|
| `content` | `csr.content` | `CodeSystemRenderer.java` (830) — concept hierarchy table | **L** | `Concepts` rows (flattened, ParentKey) |
| `summary` | `csr.summaryTable` | small | S | scalar cols |
| `xref` | `csr.xref` | xref | M | Resources rows |
| `nsInfo` / `changeSummary` | `csr.nsInfo`/`changeSummary` | small | S | CS metadata |

CS `content` is the concept hierarchy table; it reuses **C4** for code/display
and a simpler tree layout than C1/C2.

### 4.5 Example-instance fragments (M = 3)

| Suffix | Publisher method | fhir-core | Diff | We have |
|---|---|---|---|---|
| `html` (narrative) | resource renderer → `ProfileDrivenRenderer.buildNarrative:48` (677) | **C4** + ProfileDrivenRenderer (677) + type renderers | **L** | `Resources.Json` + snapshot for the profile |
| `json-html`/`xml-html`/`ttl-html` | elementmodel composers + syntax highlight | machine-format + highlighter | M | `Resources.Json` |
| `header` / status blocks | shared per-resource path | small | S | resource metadata |

Instance narrative (`ProfileDrivenRenderer` + the `DataRenderer` type family) is
its own L cluster: it walks a live resource against its profile and renders each
datatype. This is the least-shared-with-SD-tables cluster and the one most likely
to need per-datatype golden-chasing.

### 4.6 IG-level aggregates (~20 kinds, authored-channel)

These are the includes authored `pagecontent` actually pulls
(`publisher-fragments-notes.md` Part 2 §Q1c). All are **queries over
`site.db`**, not per-resource:

| Aggregate | Difficulty | We have |
|---|---|---|
| `dependency-table(-short)` | S | IG `dependsOn` |
| `globals-table` | S | IG `global` |
| `ip-statements` | S | package metadata |
| `expansion-params` | S | IG parameters |
| `table-{profiles,valuesets,codesystems,extensions,conceptmaps,capabilitystatements,searchparameters,operationdefinitions}` | **M** (8 kinds, one templated query each) | Resources rows |
| `list-{simple-operationdefinitions,requirements,capabilitystatements}` | S | Resources rows |
| `summary-observations` | S | Observation profiles |
| `cross-version-analysis` | — | **stubbed (§2.4)** |

Whole-IG the biggest *file* categories are `list-*` (2,730 files) and `table-*`
(1,400) — but those are cheap per-file: they are the same handful of query
templates instantiated per resource. cycle already models several of these as
generators (`project/includes.ts:53-73`).

### 4.7 i18n and narrative-vs-fragment overlap (honest notes)

- **`-en` duplication:** every base fragment has an `-en` twin (5,462 → 10,924).
  For stock single-language builds these are byte-identical modulo the suffix
  (§2.4); the cost is file-plumbing, not new render logic. True multi-language
  (`lang-fragment`, 59 uses / 7 IGs) is a separate effort, **out** for v1.
- **Narrative ↔ fragment overlap:** the instance `html` fragment (§4.5) and the
  per-type `.liquid` channel (§2.3b) both drive the *same* `ProfileDrivenRenderer`
  narrative machinery that also fills `Resources.text`. Layer-B's narrative-strip
  work (`layer-b-audit.md` §3, `no-narrative`) already touches this boundary.
  Building C4 + ProfileDrivenRenderer once serves narrative generation, the
  instance `html` fragment, and the `.liquid` channel — real leverage, but it
  means the instance-narrative cluster carries the widest datatype surface and
  should be sequenced with that overlap in mind.

**Cluster inventory summary (build-once mass):** C1 (~2,900) + C2 (~1,500) +
C3 (~2,000) + C4 (~2,000 used) + C5 (~1,160) ≈ **~9,500 LOC of shared engine**,
of which C1+C2 (~4,400) are the XL critical path. Everything in §4.2–4.6 is a
thin driver on top.

---

## 5. Oracle & gating

**Oracle: per-fragment HTML goldens harvested from real Publisher runs.** We
already run the Publisher across the corpus (cycle regen; `layer-b-audit.md` §4
`_genonce`; the 34-IG scorecard in REWORK-PLAN §9 came from real publisher
outputs). The publisher **eagerly writes every fragment to
`temp/pages/_includes/*.xhtml`** — so a single whole-IG `_genonce` run *is* a
complete per-fragment golden dump, for free. Harvest = copy `_includes/*.xhtml`
per IG; each file is a golden keyed by `(IG, Type, id, suffix)`.

**Gating ladder (mirror the walk-engine discipline, REWORK-PLAN §5):**

1. **Per-fragment HTML diff** — our first-miss-generated fragment vs the
   harvested `_includes/<name>.xhtml`, per (IG, resource, suffix). Classify every
   diff; empty quirk registry is the target (the walk hit that — §9).
2. **Per-page diff (T0/T1/T2)** — rendered authored page vs the publisher's page.
   US-Core is the T2 gate corpus (67 of 73 T2 pages).
3. **Whole-site diff (end-game)** — every output HTML file byte-compared across
   the 34-IG corpus. This is the definition of "fully equivalent builds" (§2).

**Normalization policy — minimal + explicit (house rule).** Default is
**byte-exact, zero normalization**; each normalization is a *named, justified*
carve-out with an oracle citation, exactly like Layer-B's "no silent
normalization" rule. Candidates to decide explicitly (not pre-approved here):
- **Whitespace:** the publisher composes via `XhtmlComposer` (C3) with a fixed
  pretty-print discipline; matching it byte-for-byte is a *C3 parity requirement*,
  not a normalization. Prefer reproducing the composer's whitespace over
  stripping it. (The stock `template-page-md.html` even warns "white space is
  critical inside of capture" — whitespace is load-bearing, §7.)
- **Generated ids / anchors:** `HierarchicalTableGenerator` and
  `gen.withUniqueLocalPrefix(...)` produce deterministic anchor prefixes
  (`s`/`k`/`m`/`o` + mode). These are a *function of inputs*, so reproduce them,
  don't mask them. Mask only if a genuinely non-deterministic id is found — flag
  it as a quirk if so.

**Kramdown differential-testing strategy.** Kramdown IAL / `markdown="1"`
re-entry is the named fidelity risk (`ig-jekyll-surface-survey.md` §(f)#5). Strategy:
extract the corpus's kramdown constructs (242 `{: .class}` / 10 IGs;
`markdown="1"` blocks; footnotes; `{:toc}`) into a **differential corpus** and
diff our markdown engine's HTML against the publisher's page output *for those
constructs specifically*, before whole-page gating — so a kramdown divergence is
caught as a small isolated diff, not buried in a 200-line page diff. The
`{% raw %}`-evaluates-`{% fragment %}` quirk (§(f)#4) is a *seed quirk-registry
entry*, not a normalization.

---

## 6. Phases, gates, effort

Effort ranges are calibrated against **this project's actuals** (REWORK-PLAN §9:
the walk engine reached 955/955 across 34 IGs in ~2 days wall *with* full oracle
discipline). The renderer is **wider** than the walk — more distinct output
surfaces, weaker per-fragment isolation, whitespace-exactness burden — so
per-cluster estimates are scaled up honestly from that datum.

### Phase 0 — Oracle harvest + normalization spec (0.5–1 wk)
Harvest `_includes/*.xhtml` goldens across the 34-IG corpus via `_genonce`;
build the per-fragment diff harness (keyed by IG/type/id/suffix); write the
explicit normalization spec (§5). **Gate:** every corpus IG has a complete
fragment-golden set and a green "identity" diff (golden vs itself). Low risk;
reuses existing publisher-run tooling.

### Phase 1 — Minimal-Jekyll T1+T2 + kramdown (1.5–2.5 wk)
Bring the page layer to parity **on pages with no per-resource fragments**
(T0/T1 first, then T2 driven by US-Core). Either extend cycle's `core/liquid.ts`
to T2 (Proposal D, smaller) or port to Rust (Proposal C). Includes the kramdown
differential corpus (§5). **Gates:** (i) T0/T1 pages byte-parity across corpus;
(ii) US-Core T2 subset byte-parity; (iii) kramdown differential corpus green.
**Risk:** kramdown fidelity (XL-ish risk, M-ish LOC) — see §7.

### Phase 2 — C1+C2 element-table engine (2.5–4 wk) — THE XL CRITICAL PATH
Build the `generateTable`/`HierarchicalTableGenerator` engine to HTML-golden
parity, gated one flag-combination at a time: `snapshot` → `diff` → `byKey` →
`byMustSupport` → `obligations`/`*-bindings` (C5) → `grid`/extension-table.
Fed by the walk snapshots we already have. **Gate:** per-fragment HTML parity for
the ~15 SD table fragments across all 34 IGs, quirk registry classified.
**This phase dominates the schedule and the risk (§7).** Decide Proposal A/B
(fragment storage) and C/D (liquid engine location) at the Phase-1→2 boundary.

### Phase 3 — the leaves (2–3 wk, parallelizable)
Independent per-cluster items on top of the shared engine, each its own golden
gate: dict cluster; inv tables; tx/VS-expansion/CS-content (C4 shared);
xref/uses/mappings; machine formats (pseudo-json/xml/ttl); instance narrative
(C4 + ProfileDrivenRenderer); the ~20 IG-level aggregates. **Gate:** per-kind
HTML parity. Fan out across agents (like Wave 3's per-IG batches).

### Phase 4 — whole-site equivalence (1–1.5 wk)
Wire first-include-miss end-to-end; run whole-site byte-diff across the 34-IG
corpus; classify residuals; drive the quirk registry to empty (or documented).
**Gate:** whole-site diff clean-or-explained per IG. This is the §2 definition of
done.

**Total envelope: ~7.5–12 weeks**, midpoint **~9 weeks**, dominated by Phase 2
(C1+C2) and Phase 1 (kramdown). Phases 0/1 are low-risk and can start
immediately; Phase 3 parallelizes once Phase 2's engine lands.

---

## 7. Risks (ranked) + quirk-registry seeds

**R1 — C1+C2 element-table parity (XL, likely-blocking).** ~4,400 LOC of
branch-dense table logic with weak per-fragment oracle isolation (one engine, 15
fragments) and byte-exact whitespace/anchor requirements. *Mitigation:* gate one
flag-combination at a time against harvested goldens (Phase 2 sub-gates);
reuse walk snapshots as fixed input; treat it as "the walk engine of the
renderer" and apply the identical decision-isomorphic discipline. This is the
single item most likely to blow the estimate.

**R2 — kramdown / `markdown="1"` fidelity (M LOC, XL fidelity risk).** IAL
`{: .class}` (242× / 10 IGs), `markdown="1"` re-entry into raw HTML, footnotes,
`{:toc}` — GFM engines don't do these; ignoring them "visibly breaks styling."
*Mitigation:* the kramdown differential corpus (§5) as a Phase-1 gate; consider
reusing cycle's `markdown.ts` (Proposal D) which already targets this. *Seed
quirk:* `markdown="1"` block re-entry; trailing-IAL attachment to headings/links.

**R3 — XhtmlComposer whitespace/escape exactness (C3, L).** Every fragment is an
`XhtmlNode` tree composed to a string; byte-parity requires reproducing the
composer's exact indentation, self-closing, and entity-escaping rules. A single
off-by-one space cascades into every fragment diff. *Mitigation:* build C3
first, gate it against a golden-fragment corpus before building C1/C2 on top;
prefer *reproducing* the composer over post-hoc normalization (§5). *Seed
quirks:* `&amp;` vs `&`; `<br/>` vs `<br>`; whitespace inside `{% capture %}`
("white space is critical" — stock `template-page-md.html`); the
`{% raw %}`-still-evaluates-`{% fragment %}` publisher wart.

Secondary (tracked, not blocking): **i18n `-en` scope creep** (keep to
byte-copy for v1, §2.4); **C4 code-resolution display drift** (depends on tx
expansion identity — pin like Layer-B, cache with `expansion.parameter`);
**instance-narrative datatype long tail** (per-datatype golden-chasing in
Phase 3); **ant `artifacts.xml` reimplementation** (derivable but a distinct
XSLT-logic port).

---

## Appendix — load-bearing citations (fourth pillar)

Publisher fragment→renderer wiring: `ig-publisher@2.2.10
.../publisher/PublisherGenerator.java` (SD fragment calls `:1808`+, `snapshot`
`:1870`, `diff` `:1865`, `dict` `:2011`, `inv` `:2007`, `maps` `:2035`, `grid`
`:1934`, `span` `:2070`; VS `:1523/1550`, CS `:1473`). Publisher-side wrappers:
`.../renderers/StructureDefinitionRenderer.java` (3,204 LOC; `snapshot():510`,
`diff():487`, `byKey():532`, `byMustSupport():547`, `dict():1308`, `tx():851`,
`invOldMode():1203`, `mappings():1323`, `uses():1529`, `references():2254`,
`span():1718`, `expansion():2699` — all table methods call
`sdr.generateTable(...)`), `ValueSetRenderer.java` (257), `CodeSystemRenderer.java`
(268). fhir-core@6.9.10 engines:
`.../r5/renderers/StructureDefinitionRenderer.java` (6,104; `generateTable:578`,
`generateTableInner:613`, `genElement:920`, `genElementCells:1351`, `genTypes:2320`,
`generateDescription:1536`, `genFixedValue:2760`, `renderDict:3968`,
`generateElementInner:4361`, `generateSpanningTable:3713`, `generateExtensionTable:3818`),
`utilities/.../xhtml/HierarchicalTableGenerator.java` (1,503),
`utilities/.../xhtml/XhtmlNode.java` (1,506) + `XhtmlComposer.java` (516),
`DataRenderer.java` (2,405), `ResourceRenderer.java` (1,645),
`ProfileDrivenRenderer.java` (677), `ValueSetRenderer.java` (1,842;
`generateExpansion:244`, `generateComposition:1224`), `CodeSystemRenderer.java`
(830), `ObligationsRenderer.java` (618), `AdditionalBindingsRenderer.java` (546),
`TerminologyRenderer.java` (343). Fragment-menu evidence: `publisher-fragments-notes.md`
Part 2 (N/M table, first-include-miss). Page surface: `ig-jekyll-surface-survey.md`
(T0/T1/T2, kramdown §(c)/(f)). Substrate: `cycle-package-db-plan.md`
(§2b S1–S7, §2c two-ledger). Calibration: `snapshot/REWORK-PLAN.md` §9
(955/955 / 34 IGs / ~2 days).

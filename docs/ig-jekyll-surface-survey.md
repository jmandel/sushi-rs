# FHIR IG Jekyll/Liquid Surface Survey — Empirical, Real-World Corpus

**Purpose.** Feasibility input for a Rust "just enough Jekyll" layer. This is an *honest usage
distribution* measured over real IG source repos, not the theoretical Jekyll/Liquid feature set.
Every number below is counted from the corpus; commands are reproducible from
`scratchpad/ig-survey/`.

> **Historical survey, not a current capability contract.** Current behavior
> and executable evidence live in the root architecture document and
> `crates/render_liquid/README.md`. SQL is a Publisher pre-Liquid close-step
> capability; Cycle deliberately rejects SQL.

**Method.** 16 IG source repos cloned shallow into
`.../scratchpad/ig-survey/`, plus the local `periodicity-impl/cycle` IG (17 total). Authored,
Jekyll-processed content = `input/pagecontent/**`, `input/pages/**`, `input/intro-notes/**`
(`.md`/`.xml`), with `input/includes/**` inventoried separately as the transitive include layer.
Constructs extracted with ripgrep/grep; each reported with **total count** and **IG breadth**
(how many of 17 IGs use it — a construct used by 12 IGs matters more than one used by 1).

**Headline numbers.** 939 authored pages across the corpus. **456 (48.6%) are plain markdown
with zero Liquid.** Another 410 (43.6%) use only *simple* Liquid (include + `{{output}}` +
assign/capture). Only **73 pages (7.8%) use control flow** (`for`/`if`/`where`), and **67 of those
73 are US-Core alone.**

---

## (a) Corpus — template choice per IG

`template =` from each `ig.ini`. "Stock" = a published HL7/IHE/base template package pulled from
the registry; "repo-local" = a `template/` dir carried in the repo (`#name` form). None of the 16
carried a `template/` override *directory* except the two that declare `#local` template packages.

| IG | repo | `template =` | class | authored pages | includes files |
|---|---|---|---|---:|---:|
| fhir-ips | HL7/fhir-ips | `hl7.fhir.template#current` | stock (base/fhir) | 26 | 1 |
| mCODE | HL7/fhir-mCODE-ig | `#custom-template` | **repo-local** (`custom-template/`) | 88 | 1 |
| davinci-crd | HL7/davinci-crd | `hl7.davinci.template#current` | stock (davinci) | 48 | 1 |
| davinci-pas | HL7/davinci-pas | `hl7.davinci.template#current` | stock (davinci) | 29 | 2 |
| davinci-dtr | HL7/davinci-dtr | `hl7.davinci.template#current` | stock (davinci) | 18 | 0 |
| sdc | HL7/sdc | `hl7.fhir.template#current` | stock (base/fhir) | 111 | 1 |
| ecr (case-reporting) | HL7/case-reporting | `hl7.fhir.template#current` | stock (base/fhir) | 39 | 0 |
| ndh | HL7/fhir-us-ndh | `hl7.fast.template#current` | stock (fast) | 47 | 1 |
| US-Core | HL7/US-Core | `hl7.fhir.template#1.0.0` | stock (base/fhir, pinned) | 173 | 66 |
| qi-core | HL7/fhir-qi-core | `hl7.fhir.template#current` | stock (base/fhir) | 89 | 2 |
| carin-bb | HL7/carin-bb | `hl7.fhir.template#current` | stock (base/fhir) | 58 | 0 |
| ig-guidance | FHIR/ig-guidance | `fhir.base.template#current` | stock (base) | 32 | 1 |
| au-fhir-core | hl7au/au-fhir-core | `hl7.au.sparked.template#current` | stock (AU) | 89 | 20 |
| IHE MHD | IHE/ITI.MHD | `ihe.fhir.template` | stock (IHE) | 41 | 0 |
| eu-x-ehealth | hl7-eu/x-ehealth | `#ig-template` | **repo-local** (`ig-template/`) | 8 | 0 |
| eu-laboratory | hl7-eu/laboratory | `#ig-template` | **repo-local** (`ig-template/`) | 37 | 1 |
| cycle | (local) | fhir base + custom `template/` | stock+local | 6 | 0 |

Template reality: **13/17 use a stock published template**, 3 carry a repo-local template package,
1 (cycle) rides the base template with a thin local `template/` for config. US-Core is the outlier
on authored volume (173 pages, 66 include files). qi-core keeps prose in `input/pages/` +
`input/intro-notes/` rather than `input/pagecontent/`.

---

## (b) Construct frequency + IG breadth

### Tags (`{% … %}`) across authored pages

| Tag | total | IG breadth | notes |
|---|---:|---:|---|
| `include` | 738 | **14/17** | near-universal; see include split below |
| `assign` | 377 | 5 | but 315 are `{{site.data.fhir.ver…}}` link-prefix assigns |
| `fragment` (Publisher) | 143 | 3 | davinci-crd, ig-guidance, sdc |
| `if` | 63 | 3 | eu-x-ehealth, fhir-ips, **us-core** |
| `lang-fragment` | 59 | **7** | localization fragment tag (carin, davinci×3, ecr, fhir-ips, qi-core) |
| `for` | 56 | 4 | eu-x-ehealth, fhir-ips, mcode, **us-core** |
| `elsif` | 23 | 1 | us-core only |
| `raw` | 22 | 3 | davinci-crd, ig-guidance, us-core |
| `unless` | 20 | 2 | au-fhir-core, us-core |
| `else` | 14 | 1 | us-core only |
| `capture` | 12 | 2 | mcode, us-core |
| `comment` | 6 | 1 | us-core |
| `sql` | 2 | 1 | ig-guidance (documents its own feature) |
| `case`/`when` | 0 | 0 | **not used** |
| `highlight` | 0 | 0 | **not used** |
| `tablerow`/`cycle`/`increment` | 0 | 0 | **not used** |

### Output `{{ … }}` — variable roots

`site` dominates: 943 of ~1050 output expressions. The rest are loop/local vars (`item`, `sd`,
`p`, `forloop`, `resource_scope`, `granular_scope`) that only appear inside the control-flow IGs.

**`site.data.*` second level** (which data files):
`site.data.fhir` = 892 · `structuredefinitions` = 57 · `StructureDefinition` = 31 · `ig` = 23 ·
`profile_metadata` 8 · `search_requirements`/`uscdi`/`assessments`/… (single-IG, us-core).

**`site.data.fhir.*` third level** (the only widely-shared data surface):
`site.data.fhir.path` = 560 · `site.data.fhir.ver.<alias>` = 315 · `version`/`ig`/`igVer` small.
→ Two paths (`site.data.fhir.path`, `site.data.fhir.ver.*`) account for **~98% of all data reads**,
and both are pure package metadata (the core spec base URL and dependency canonical bases).

**Filters** (pipe count): `split` 84 · `replace` 17 · `trim` 15 · `prepend` 12 · `first` 12 ·
`remove` 8 · `append` 7 · `strip` 3 · `join` 3 · `default` 3 · `where` 20 · `sort` 26 · `uniq` 18 ·
`markdownify` 1 · `capitalize` 1 · `downcase` 1. The string filters (`split/replace/prepend/…`) are
broad; the *collection* filters (`where`/`sort`/`uniq`, breadth 3–7) travel only with `for`.

### `include` split — template-artifact vs repo-local

Include targets by extension: **`.md` 527 · `.svg` 72 · `.html` 70 · `.xhtml` 69.**

- **Template-generated artifact includes (`.xhtml`)** — resolved by the Publisher from IG metadata,
  *not* present in the repo. Broad and boring:
  `dependency-table.xhtml` (8 IGs) · `ip-statements.xhtml` (7) · `globals-table.xhtml` (7) ·
  `cross-version-analysis.xhtml` (7) · `expansion-params.xhtml` (4) ·
  `table-{profiles,valuesets,codesystems}.xhtml` (2) · `dependency-table-short.xhtml` (2).
  These are exactly the includes cycle already models as generators.
- **Repo-local content includes (`.md`/`.html`/`.svg`)** — checked into `input/includes/`, mostly
  **single-IG** authoring: `link-list.md` (149×, us-core) · `markdown-link-references.md` (61×, 2
  IGs) · `quickstart-*.md`, `*-intro.md`, `img.html` (44×, 3 IGs), sequence-diagram `.svg` includes.
  These resolve to a checked-in file and (crucially) may themselves contain control-flow Liquid.

---

## (c) Kramdown / Markdown-dialect reality

| Feature | total | IG breadth | verdict |
|---|---:|---:|---|
| Pipe tables (`\| … \|`) | 2805 | **13/17** | must support GFM tables |
| Raw HTML `<div …>` | 930 | **17/17** | universal — HTML passthrough is mandatory |
| Raw HTML `<br>` | 971 | 13 | universal-ish |
| HTML comments `<!-- -->` | 445 | 13 | common |
| Raw HTML `<table>` | 252 | **15/17** | authors hand-write tables too |
| IAL attribute lists `{: .class}` | 242 | **10/17** | kramdown inline-attribute-list; broad |
| ` {: #id}` anchors | 22 | 2 | qi-core, us-core |
| Footnotes `[^ref]` | 27 | 4 | ecr, mcode, qi-core, us-core |
| `{:toc}` / markdown-toc | 9 | 1 | qi-core (also produced by the stock page template) |
| `no_toc` | 4 | 2 | fhir-ips, us-core |
| Definition lists (`: term`) | 0 | 0 | not used |
| **Front matter (`---`)** | **1 page** | **1** | **effectively absent in authored pages** |

Takeaways: (1) **Raw-HTML-in-markdown is universal** — the renderer must pass HTML through
untouched. (2) **GFM pipe tables are mandatory.** (3) **Kramdown IAL `{: .class}` / `{: #id}` is a
real broad feature (10 IGs)** — not full kramdown, but the attribute-list needs handling (often to
attach CSS classes/`markdown="1"` blocks). (4) **Front matter is a non-issue** in authored pages —
Jekyll front-matter (title etc.) is carried by the template's `site.data.pages[page.path]` machinery,
not by `---` blocks in the `.md` files. (5) Footnotes appear but are rare (4 IGs).

---

## (d) "Just enough Jekyll" cutline proposal

Tiers are by **share of the 939 authored pages rendered verbatim**.

### T0 — plain markdown, no Liquid. **456 pages, 48.6%.**
Needs: GFM tables + raw-HTML passthrough + kramdown IAL. Nothing Liquid. Half the corpus is here.

### T1 — simple Liquid. **+410 pages → 866 pages, 92.2% cumulative.**
The bounded dialect. Exactly:
- `{% include NAME %}` where NAME is either a **template-artifact** (`.xhtml`: dependency-table,
  ip-statements, globals-table, cross-version-analysis, expansion-params, table-*) resolved from IG
  metadata, or a **checked-in `input/includes/*` file** inlined and re-rendered.
- `{{ site.data.fhir.path }}` and `{{ site.data.fhir.ver.<alias> }}` (98% of data reads) + `page.*`.
- `{% assign %}`, `{% capture %}`, `{% comment %}`, `{% raw %}`.
- String filters: `split, replace, replace_first, remove, remove_first, append, prepend, downcase,
  upcase, capitalize, strip, trim, default, join, first, last, escape/escape_once, markdownify, date`.
- Kramdown IAL `{: .class}` / `{: #id}`, footnotes, `{:toc}`/`no_toc`.
**This tier covers 92% of the corpus and every IG except the control-flow-heavy ones below.**

### T2 — escape hatches / control flow. **+73 pages → 939, 100%.**
`{% for %}` (+ `forloop.first/last`, `offset:`), `{% if/unless/elsif/else %}` with `==`/`!=`/
`contains`/`.size`, collection filters `where:`/`sort`/`uniq`, and `{% include X.md k=v %}` +
`include.<param>` (parameterized includes). **67 of 73 T2 pages are US-Core**; the remaining 6 are
one page each in au-fhir-core, eu-x-ehealth, fhir-ips, ig-guidance(2), mcode, sdc(2). Transitively,
control-flow lives mostly in generator **include files**: us-core 24, au-fhir-core 4, davinci-pas 2,
fhir-ips 1, eu-laboratory 1.

### Explicitly OUT (measured zero or single-doc-self-reference)
`case`/`when`, `highlight`, `tablerow`, `cycle`, `increment`/`decrement`, layout inheritance /
`{% layout %}`, collections / arbitrary `site.data.*` sprawl beyond the curated surface, Liquid
inside data files, runtime filesystem `_includes` resolution, definition lists, whitespace-control
*parity* nuance, and Jekyll `---` front-matter in authored pages (1 page total). `{% sql %}` is an
IG-Guidance-documented Publisher extension used only by IG-Guidance (2×). It is not Liquid core:
the current Publisher path executes it in SiteEngine before Rust Liquid, while Cycle intentionally
has no database or SQL tag.

**Recommended build target: implement T0+T1 fully (92% verbatim); make T2 a bounded add-on driven by
US-Core.** An IG that hits an unimplemented T2 construct should fail loud, not silently mis-render.

---

## (e) Historical delta vs the former Cycle subset

This section records the 2026-07-03 implementation that the survey originally
measured. It is retained as provenance for the cutline, not as a current support
claim. The present Cycle renderer uses LiquidJS with strict filters and closed
include/fragment adapters, and deliberately rejects `sql`/`sqlToData`.

cycle's `core/liquid.ts` (LiquidJS locked down) + `liquid-subset.md` already covers the T1 core and
several T2/extension items. Cross-checked against the corpus:

**Covered by the surveyed Cycle implementation:** `include` → registry/asset, `assign`, `capture`, `comment`,
`raw`, `{{path|filter}}`, `lang-fragment`, `fragment` (Publisher), the string
filters (`replace/remove/append/prepend/downcase/upcase/strip/default/date/markdownify`), and the
`site.data.fhir.ig` surface. cycle also nails the two dominant data paths conceptually (it exposes
`site.data.fhir.*`).

**Present in corpus but MISSING / weak in cycle's subset:**
1. **`{% for %}`** — cycle marks it "❌ phase 2". Corpus: 56 uses / 4 IGs, and it is the spine of the
   us-core generator includes. Needed for T2, with `forloop.first/last` and `offset:`.
2. **`{% if/unless/elsif/else %}`** — cycle is "⚠ minimal, `==`/`!=` only, no and/or". Corpus needs
   `contains`, `.size` comparisons, and chained `elsif` (us-core `search-requirement-handler.md`).
3. **Collection filters `where:` / `sort` / `uniq`** — not in cycle's filter list. Corpus breadth
   7 / 3 / 3 IGs; `where:"code",x | where:"base",y | first` is the us-core data-join idiom.
4. **`split`** — the single most-used *string* filter in the corpus (84×) and heavily used in the
   generators; cycle lists `replace/remove/...` but not `split`/`join`/`first`/`last`/`trim`.
5. **Parameterized includes `{% include X.md k=v %}` + `include.<param>`** — cycle parses include
   args into `params` and exposes `include.*` to asset re-render (good), but the subset doc frames
   includes as a *shortcode registry*; the corpus reality is that repo-local `.md` includes are
   full Liquid **templates with parameters and their own for/if** (us-core: 120 param-passing sites,
   64 `include.*` reads). cycle's asset-include path does re-render recursively, so this mostly needs
   the missing for/if/filters, not new plumbing.
6. **Kramdown IAL `{: .class}`** (242×, 10 IGs) and **`{:toc}`** — these are *markdown*-layer, not
   Liquid; cycle's `markdown.ts` must handle them (out of scope for `liquid.ts` but on the cutline).
7. **The stock page template itself** — `template-page-md.html` wraps every authored page and uses
   `{% include {{path}}.md %}` (dynamic include), `site.data.pages[page.path].title`, a
   `capture`+`markdownify`+`contains` TOC-detection dance, and `escape_once`. cycle bypasses this by
   supplying its own React Layout, which is the right call — but any tool aiming to *reuse* stock
   templates must handle dynamic-name includes and `escape_once`.

**Net:** cycle's subset is a faithful T1 engine and already handles the 92%-tier semantics. To cover
the corpus it needs **`for`, richer `if` (`contains`/`.size`/`elsif`), and the `split`/`where`/`sort`/
`uniq` filters** — i.e., the US-Core-shaped T2 layer. Everything cycle explicitly cut (layouts,
collections, tablerow, case, front-matter) is confirmed *absent* from the corpus.

---

## (f) The 5 nastiest things found (cited)

1. **US-Core `search-requirement-handler.md`** — `input/includes/search-requirement-handler.md`
   (HL7/US-Core). A parameterized include invoked as
   `{% include search-requirement-handler.md conf_verb="SHOULD" search=search %}` that reads
   `include.*` params, does data joins
   `site.data.search_requirements | where:"code",x | where:"base",y | first`, nested `for`,
   `forloop.first/last`, chained `elsif` on `search_type`, and `.size` guards — all emitting
   markdown that is *then* rendered. 142 tags in one include. This single file is the hardest thing
   in the corpus and the reason T2 exists.

2. **US-Core `sd-list-generator.md`** — `input/includes/sd-list-generator.md` (HL7/US-Core, mirrored
   in au-fhir-core). Iterates `site.data.structuredefinitions` (a hash), builds a comma-string of
   types via `append`, then `split | sort | uniq`, then `for i in my_types offset:1` to render a
   grouped/sorted/linked profile table joined against a `profile-metadata.csv`. Classic
   "Liquid-as-ETL" — depends on build-generated `temp/pages/_data/*.json`.

3. **Stock template `template-page-md.html`** — `ig-template-base/includes/template-page-md.html`.
   The wrapper the Publisher applies to EVERY authored page: `{% assign path = page.path | split:
   '.html' %}` then **`{% include {{path}}.md %}`** (a *dynamic* include name), captured twice into
   `toc-content`/`no-toc-content`, `| markdownify | remove:'###### ' | … | replace:"<h3","### "`,
   then `{% if teststring contains h3headers %}` to decide whether to show a TOC. Comment in-file:
   "white space is critical inside of capture." Dynamic-name include + markdownify-then-regex-strip
   is the gnarliest stock-template pattern; cycle sidesteps it with a React layout.

4. **`{% raw %}` protecting handlebars/fragments** — `davinci-crd/input/pagecontent/index.md` opens
   with a bare `{% raw %}{% endraw %}` pair, and `ig-guidance/related-igs.md` /
   `diagrams-mermaid.md` wrap literal `{% raw %}```mermaid{% endraw %}` inside `<code>`. cycle's own
   note (liquid.ts lines 213-220) documents that the Java Publisher *still* evaluates `{% fragment %}`
   inside `{% raw %}`, so raw is not a clean escape — a real semantic wart to replicate or reject.

5. **Kramdown IAL + `markdown="1"` HTML blocks** — 242 `{: .class}` occurrences across 10 IGs, e.g.
   `<blockquote class="note-to-balloters" markdown="1">` (davinci-crd/index.md) and stray-attribute
   lists on headings/links. These force the markdown engine to (a) honor `markdown="1"` re-entry into
   raw HTML blocks and (b) attach classes/ids from trailing `{: … }` — a kramdown-ism GFM engines
   don't do out of the box. Broad enough (10 IGs) that ignoring it visibly breaks styling.

---

## Reproduction

Corpus + scripts under `scratchpad/ig-survey/` (`repos.txt` lists repo→dir). Stock templates at
`scratchpad/ig-template-base/` and `scratchpad/ig-template-fhir/`. cycle subset at
`periodicity-impl/cycle/site-gen/core/liquid.ts` + `site-gen/liquid-subset.md`. Counts produced with
`grep -oE` over authored `input/{pagecontent,pages,intro-notes}` files, includes inventoried
separately.


---

## Scope decision (Josh, 2026-07-03)

**T2 (the US-Core-shaped layer) is IN scope for the Rust engine, if feasible**
— not an optional tier. Concretely that adds, on top of T1:
`{% for %}` (incl. `forloop.*`, `offset:`, `limit:`), full `if`/`elsif`/
`unless` with `contains`/`.size` operands, the array filters
`split | where | sort | uniq | map | join`, and parameterized includes
(`{% include x.md param="v" %}` + `include.param`, incl. US Core's
`where:`-style data-join includes). This is standard Liquid semantics —
bounded, deterministic — so the feasibility question is not "can it be
implemented" but "can output parity be proven": the gate is US Core's own
939-page… corpus subset rendered byte-comparable vs the Jekyll output
(kramdown interaction + whitespace being the risk concentration). The
synthesis doc must size T2 explicitly and give it its own gate.

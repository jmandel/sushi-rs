# Stock-Template Renderer — COMMITTED PLAN

> Status: COMMITTED (Josh, 2026-07-03). Goal: **US Core editing in the browser,
> the standard FHIR template selectable, pages rendered at Publisher parity,
> near-instant.** This operationalizes docs/rust-fragment-generator-feasibility.md
> (read it for evidence); this doc is decisions + phases + gates only.

## 1. What ships

In fhir-ig-editor: open **US Core** (or cycle, or any loaded IG) → pick a
**template** from a selector — `hl7.fhir.template` / `fhir.base.template`
(stock, NEW Rust renderer) or `cycle site-gen` (existing TS path) → edit FSH
or pagecontent → the rendered IG page (profile tabs, snapshot/diff tables,
dicts, ValueSet pages, authored markdown pages) updates at **Publisher
parity**, re-rendering only what the edit dirtied.

## 2. Architecture decisions (final)

1. **We interpret the real template, we don't reimplement it.** The stock
   template package (layouts/includes/config.json/liquid dir) is a DATA input
   — bundled like FHIR packages, read at render time. Template upgrades flow
   through; custom templates work to the extent they use supported features.
   The four-part compatibility surface (fragments doc Part 2 Q3): Java kind
   menu = our fragment generators; per-type `.liquid` channel = interpreted;
   ant-injected includes = reimplemented as built-ins for the stock template's
   known set (artifacts.xml, includes copy) with a registry for others;
   `tabbed-snapshots`/`no-narrative`/`generate` params honored.
2. **Fragments are derived artifacts, generated on first-include-miss**,
   content-hash-keyed beside site.db (BuildState ledger nodes). No static
   template scanning (rapido's mistake); no eager menu (the Publisher's 68%
   waste). Proven complete: no template probes fragment existence.
3. **One Rust engine, native + wasm** (pure compute, no new wasm blockers):
   - `xhtml` — C3 XhtmlComposer-exact serializer (byte-parity substrate).
   - `tables` — C2 HierarchicalTableGenerator port.
   - `sd_render` — C1 StructureDefinitionRenderer `generateTable` + leaves.
   - `md` — kramdown-subset engine (IAL, footnotes, toc, tables,
     `markdown="1"` re-entry).
   - `liquid` — T1+T2 engine (survey cutline; US-Core layer IN scope).
   - `page` — layouts + `site.data.*` (backed by site.db queries) + menu.
4. **Editor render paths**: template selector chooses (a) stock-template Rust
   renderer (wasm) or (b) cycle TS site-gen over site.db (existing M2 path).
   Both consume the same site.db + fragment store.
5. **Perf model for "almost immediately"**: cold page = liquid render + the
   handful of fragments that page includes, generated on miss (ms-scale each
   in Rust); edits invalidate by content hash (Ledger 1) and re-render only
   read-set-dirty pages (Ledger 2). US Core's 70 profiles never render
   eagerly — only visited pages materialize.
6. **Oracle & goldens**: per-fragment goldens = the `_includes/*.xhtml` dump
   of real Publisher runs (pinned 2.2.10). Golden corpora: cycle (have),
   **US Core**, + 2 mid-size IGs (sdc, plan-net) — one-time Publisher runs,
   isolated HOME, committed like all goldens. Page-level goldens = the same
   runs' final HTML. Never normalize beyond the explicit documented set.
   Quirk registry from day one ({% raw %} evaluation, kramdown edges).

## 3. Phases (dependencies →; gates per phase)

| Ph | Deliverable | Gate |
|---|---|---|
| **F0** | Golden harvest: Publisher runs for us-core/sdc/plan-net (+have cycle); fragment+page golden corpus committed; harvest+diff scripts | corpora on disk, documented pins |
| **F1a** | `xhtml` C3 composer | byte-exact re-serialization of harvested fragments' parsed xhtml (round-trip gate) |
| **F1b** | `md` kramdown subset | differential corpus: all 939 survey pages, rendered vs kramdown, classified diffs → 0 unexplained on used-feature set |
| **F1c** | `liquid` T1+T2 | survey corpus pages render byte-equal vs Jekyll (staging inputs); US Core's 73 T2 pages included |
| **F2** | `tables` C2 engine | table-shaped fragments' structural skeleton matches goldens (pre-content) |
| **F3** | `sd_render` C1 generateTable + the 15 table fragments | SD table fragments byte-match goldens across 4-IG corpus |
| **F4** | SD leaf fragments (dict/inv/tx/maps/pseudo-*/summary/…) + VS/CS/instance fragments (expansion via tier-1 evaluator + cached tx) + ~20 IG aggregates | full used-fragment set byte-parity, 4 IGs |
| **F5** | `page` pass: layouts, site.data via site.db, menu, first-include-miss wiring, ledger integration | whole-page HTML parity (classified) for cycle + plan-net; then US Core |
| **F6** | Editor integration: template bundling, selector UI, wasm exports, per-page preview swap, Ledger-2 replay-skip | live demo: open US Core, edit, page updates <1s warm; E2E + live-URL gates |

F1a/F1b/F1c are independent (parallel). F2→F3→F4 serial on the engine. F5
needs F1*+F4. F6 needs F5 (+ M2's editor plumbing, already in flight).

## 4. Effort + calibration

Feasibility envelope ~7.5–12 wk human-scale; this project's demonstrated
agent-parallel compression (walk engine: 955/955 in ~2 days) applies best to
oracle-gated porting (F2–F4) and worst to fidelity grinds (F1b kramdown).
Sequencing above front-loads the three independent fidelity substrates so the
XL porting work lands on proven foundations.

## 5. Standing rules

All house rules apply: oracle wins, goldens never edited to pass, every
behavior cited (fhir-core/publisher file:line at pinned versions), quirk
registry for case law, no hardcoded IG-specific behavior, isolated caches,
coordinator verifies + commits. Native gates (existing 955/955 corpus, sushi
harvest, wasm parity) must stay green through every phase.

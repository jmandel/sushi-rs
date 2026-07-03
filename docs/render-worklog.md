# Render worklog — F2 (C2 tables) + F3 (C1 generateTable / SD table fragments)

Branch `snapshot-gen`. Ports fhir-core **6.9.10-SNAPSHOT** (the renderer version
that produced the golden corpus — checked out as a detached worktree at commit
`6c04914e4` under the scratchpad; the repo's default fhir-core checkout is 8.4.0
and must NOT be cited for line numbers). Publisher wrappers read from the
scratchpad `fhir-ig-publisher` clone.

## Crates added (own `[workspace]`, path deps only; root Cargo.toml untouched)

- `crates/render_tables` (C2) — HierarchicalTableGenerator port.
  - `hashorder.rs` — Java `HashMap<String,String>` iteration-order emulation
    (the load-bearing attribute-order decision, below). ~90 LOC.
  - `build.rs` — `Elem` builder over `render_xhtml::XhtmlNode` that buffers
    attributes and flushes them in HashMap order. ~140 LOC.
  - `model.rs` — Piece / Cell / Title / Row / TableModel / Counter. ~430 LOC.
  - `generate.rs` — `generate` / `renderRow` / `renderCell` / `init*Table` /
    `checkModel` / `srcFor` / `checkExists` / `pathURL` / anchors. ~650 LOC.
  - `lib.rs` ~35 LOC.  **Total ~1,350 LOC.**  `cargo test`: 3 green.
- `crates/render_sd` (C1) — StructureDefinitionRenderer element-table port.
  - `sdmodel.rs` — typed JSON views over an SD (Ed/TypeRef/Constraint/Sd). ~230.
  - `links.rs` — `getLinkFor` for R4 core types (override table + datatypes/
    resource page rules). ~110.
  - `markdown.rs` — `Cell.addMarkdown` plain-prose path (text + `br;br`). ~75.
  - `grid.rs` — `generateGrid` / `genGridElement` / `genCardinality` /
    `genTypes` / `generateGridDescription`. ~430.
  - `lib.rs` ~55; `bin/render_frag.rs` ~45; `bin/corpus.rs` ~150.
  - **Total ~1,090 LOC.**  `cargo test`: 4 green (grid parity pins).

## The load-bearing decision: attribute ordering

fhir-core's `XhtmlNode.attributes` is a `HashMap<String,String>` and
`XhtmlComposer.attributes` iterates `keySet()` (XhtmlComposer.java:308). So the
publisher's bytes carry attributes in **Java HashMap iteration order**, NOT the
order the renderer set them. `render_xhtml`'s OrderMap is insertion-ordered
(correct for the C3 round-trip substrate). `render_tables::hashorder` reproduces
HashMap order — cap starts 16, doubles when `size > cap*0.75`; bucket =
`(cap-1) & (h ^ h>>>16)`; stable within bucket. Verified against the golden
`<img src style class alt>` → emits `src alt style class`, `<table border
cellspacing cellpadding style>` → `border cellpadding cellspacing style`. Every
`Elem::build()` reorders its buffered attributes through this before composing.

## Composer fix in render_xhtml (F1a) — `breakBlocksWithLines` recursion

`XhtmlComposer.java:92-102` captures `node = list.get(i)` BEFORE the sibling
`\r\n` insert and always recurses into that captured node (line 101). The Rust
port re-read `list[i]` AFTER the insert, so recursion diverted onto the newly
inserted text node and never descended into nested block rows — producing
`</td><td>` instead of the golden's `</td>\r\n<td>` inside data rows (header
`<th>`s stay inline because the header `<tr>` is child index 0, which the
`i > 0` loop skips). Fixed by recursing into `list[i]` before the insert
(recursion touches only the block's children, unaffected by a sibling insert).
**F1a-gate-neutral**: the corpus round-trip gate reports identical
`parity=12165` and the identical pre-existing 5-fragment failure set with and
without this change (the goldens already contain their separators, so
breakBlocks inserts nothing during a round-trip; the bug only manifests on
freshly-built trees — exactly the F2/F3 use case). The 5 pre-existing failures
(`*-expansion`, `deprecated-list`, `expansion-params`, `summary-extensions`) are
tiny raw-string leaf fragments unrelated to this work and owned by F1a.

## The 15 SD table fragments → flags map (publisher SDR wrappers)

Every table-shaped SD fragment routes through ONE
`sdr.generateTable(status, defnFile, sd, DIFF, destDir, false, id, SNAPSHOT,
corePath="", imagePath="", isLogical, ALLINV, tracker, MUSTSUPPORT, gen', anchorPfx,
resE, idSfx)` — or `generateGrid` for grid. `mc(mode)` prefixes the
uniqueLocalPrefix: BINDINGS→"b", DATA_DICT→"d", OBLIGATIONS→"o", SUMMARY→"".
All wrappers compose with `new XhtmlComposer(XhtmlComposer.HTML)` = HTML,
non-pretty. Citations = scratchpad `fhir-ig-publisher .../renderers/
StructureDefinitionRenderer.java`.

| Fragment suffix | Wrapper (line) | diff | snapshot | allInv | mustSupport | mode / prefix | idSfx |
|---|---|:--:|:--:|:--:|:--:|---|---|
| `-grid` | grid():791→generateGrid | — | — | — | — (MS children only) | prefix "g" | — |
| `-snapshot` | snapshot():510 | F | T | T | F | SUMMARY / "s" | S |
| `-snapshot-all` | snapshot():510 (all) | F | T | T | F | SUMMARY / "sa" | SA |
| `-diff` | diff():487 | T | F | F | F | SUMMARY / "" | D |
| `-diff-all` | diff():487 (all) | T | F | F | F | SUMMARY / "a" | DA |
| `-snapshot-by-key` | byKey():532 (mode SUMMARY) | F | T | T | F | "k" | K |
| `-snapshot-by-key-all` | byKey():532 | F | T | T | F | "ka" | KA |
| `-snapshot-by-mustsupport` | byMustSupport():547 | F | T | F | T | "m" | M |
| `-snapshot-by-mustsupport-all` | byMustSupport():547 | F | T | F | T | "ma" | MA |
| `-snapshot-obligations` | obligations():523 (mode OBLIGATIONS) | F | T | T | F | OBLIG / "o" | O |
| `-snapshot-obligations-all` | obligations():523 | F | T | T | F | OBLIG / "oa" | OA |
| `-snapshot-bindings` | snapshot()+mode BINDINGS | F | T | T | F | BIND / "bs" | S |
| `-snapshot-bindings-all` | snapshot()+BINDINGS (all) | F | T | T | F | BIND / "bsa" | SA |
| `-diff-bindings` / `-diff-obligations` | diff()/obligations()+mode | T/F | F/T | … | … | b*/o* | D/O |
| `-span` / `-spanall` | span():1718→generateSpanningTable | — | — | — | onlyConstraints | ANCHOR_PREFIX | — |

The `-by-key-*` / `-by-mustsupport-*` / `-bindings` / `-obligations` combos are
the same `generateTable` call with (a) a pre-filtered element list
(`getKeyElements` / `getMustSupportElements`) and (b) a `StructureMode`
(SUMMARY/BINDINGS/OBLIGATIONS/DATA_DICT) that toggles the Flags/extra columns.
`-grid` and `-span*` are the two that use dedicated entry points
(`generateGrid`, `generateSpanningTable`) rather than `generateTable`.

## Parity (kind × IG → byte-identical / total-with-golden)

Inputs: us-core + plan-net from the F0 build `output/` SDs (publisher's actual
snapshot-complete inputs — eliminates snapshot-source variance). cycle from
`periodicity-impl/cycle/fsh-generated` (SUSHI snapshots; no cycle F0 build — a
documented snapshot-source variance for cycle).

| kind | cycle | plan-net | us-core |
|---|---|---|---|
| **grid** | 0 / 7 | 7 / 22 | 10 / 70 |

(Other 14 kinds: engine ready, drivers not yet written — see "remaining".)

All grid failures are cleanly classified (below); the passers are the
structurally-complete profiles (single-root extensions with core-type values and
no external-resolution dependency).

## Divergences classified (with citations)

1. **Binding resolution → ValueSet webPath** (e.g. us-core-allergyintolerance @
   5733). Ours emits `<a href="{vs.url}">{last-segment}</a>`; golden emits
   `<a href="{vs.webPath = …/R4/valueset-…​.html}" title="{vs.url}">{name}</a>`.
   The real path is `context.getPkp().resolveBinding(...)` → a `BindingResolution`
   whose `.url` is the VS's resolved **webPath** and `.display` its name
   (SDR:3139-3141). This is C4 (code/terminology resolution) — deferred; the
   grid `render_binding` is a stub.
2. **Reference/profiled-type link resolution** (e.g. plannet-Practitioner @
   2534; cycle basal-body-temperature @ 2512). A `Reference(target)` or a root
   whose base is an in-IG **profile** resolves to the profile's webPath + display
   name (`getLinkForProfile` / the root-base branch SDR:2344-2347), not the core
   type. Needs the IG's profile→webPath map (SpecMapManager) — a context
   dependency, not a formula. Deferred with C4/context wiring.
3. **cycle snapshot-source variance** — cycle inputs are SUSHI snapshots, not
   publisher-regenerated; some divergence may be input, not renderer. Flagged;
   revisit with a cycle F0 build.

No divergence required a golden edit. No quirk-registry entries needed yet (the
one candidate — the `addStyledText` Java precedence bug at HTG:521 producing
`background-color: null` — is ported verbatim in `model.rs`, so it is faithful,
not a quirk).

## Quirk registry

Empty. Faithful ports of Java warts (not quirks, because reproduced exactly):
- `addStyledText` background-color precedence bug (HTG:521) → `model.rs`.
- Grid tables leave `mode` unset (null) so grid `<a>`s never get
  `no-external`/`data-no-external` (guard `mode == XHTML`, HTG:1160) —
  `Gen.mode: Option`, `None` for grid.
- `context.prefixAnchor` (RenderingContext) is null-prefix for grid, so the
  "g-" anchor prefix is applied exactly once (by the HTG, in renderCell).

## Remaining

- Grid: binding resolution (C4) + profile/reference link resolution (context) to
  green the remaining ~100 grid fragments; cycle F0 build for clean inputs.
- The 14 non-grid table kinds: the `generateTable` entry point (`genElement` /
  `genElementCells` / the Flags column / slicing rows / obligations+bindings
  columns / by-key + by-mustsupport element filters). The C2 engine +
  `init_normal_table` + genTypes/genCardinality/description are already in place
  and shared; the remaining work is `generateTable`'s row walk and the
  Flags/mode-specific columns. The publisher flags map above is the driver spec.

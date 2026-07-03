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

## Increment 2 (2026-07-03, cont.): generateTable SUMMARY — snapshot GREEN

**Renderer pin correction**: the golden jar (publisher 2.2.10, built
2026-06-25) embeds fhir-core **6.9.11** (orgfhir.buildnumber
6a8b9c0c679411132054d835dbc68d545fa51c8a in the jar's fhir-build.properties),
NOT 6.9.10. A worktree at tag v6.9.11 is the citation source
(scratchpad/fhir-core-6911). The only behavioral 6.9.10→6.9.11 delta in the
table path: element-row anchors use element ID with path fallback (SDR:933) —
whitespace-insensitive diff of the two SDRs verified everything else is
comments/suppressions.

**Input provenance fixed for cycle**: renders now read the publisher's own
post-snapshot SDs from `periodicity-impl/cycle/temp/pages` (same provenance as
the goldens), packages from the user's global cache (the cycle build ran
without an isolated HOME — PIN.md). The `.id`-type variance disappeared.

## Parity (kind × IG → byte-identical / total-with-golden)

Inputs: us-core + plan-net from the F0 build `output/` SDs (publisher's actual
snapshot-complete inputs — eliminates snapshot-source variance). cycle from
`periodicity-impl/cycle/fsh-generated` (SUSHI snapshots; no cycle F0 build — a
documented snapshot-source variance for cycle).

| kind | cycle | plan-net | us-core |
|---|---|---|---|
| **snapshot** | 6 / 7 † | 20 / 20 ‡ | **70 / 70** |
| **snapshot-all** | 6 / 7 † | 20 / 20 ‡ | **70 / 70** |
| **snapshot-by-mustsupport** | 6 / 7 † | **22 / 22** | **70 / 70** |
| **snapshot-by-mustsupport-all** | 6 / 7 † | **22 / 22** | **70 / 70** |
| **snapshot-by-key** | 6 / 7 † | **22 / 22** | **70 / 70** |
| **snapshot-by-key-all** | 6 / 7 † | **22 / 22** | **70 / 70** |
| **grid** | **7 / 7** | **22 / 22** | **70 / 70** |
| **diff** | 6 / 7 † | **22 / 22** | **70 / 70** |
| **diff-all** | 6 / 7 † | **22 / 22** | **70 / 70** |
| **snapshot-bindings** | **7 / 7** | 21 / 21 ‡ | **70 / 70** |
| **snapshot-bindings-all** | **7 / 7** | **22 / 22** | **70 / 70** |
| **snapshot-obligations** | **7 / 7** | **22 / 22** | **70 / 70** |
| **snapshot-obligations-all** | **7 / 7** | **22 / 22** | **70 / 70** |
| **diff-bindings** | **7 / 7** | **22 / 22** | **70 / 70** |
| **diff-bindings-all** | **7 / 7** | **22 / 22** | **70 / 70** |
| **diff-obligations** | **7 / 7** | **22 / 22** | **70 / 70** |
| **diff-obligations-all** | **7 / 7** | **22 / 22** | **70 / 70** |

**15/15 kinds GREEN corpus-wide** (2026-07-03, session 4): + bindings/obligations
modes and their diff variants. The bindings/obligations kinds pass cleanly on
cycle (7/7) because the mode is the LOAD-BEARING difference, not the snapshot
input — the cycle snapshot-source variance (quirk †) only bites the SUMMARY
element rows, which the by-Name custom tables mostly don't restate.

† cycle's one failure (period-tracking-fact) is byte-equal except the
  abstract-profile child-list ORDER — a genuinely non-deterministic publisher
  behavior (fetchResourcesByType → CanonicalResourceManager.getList() iterates
  an identity-hash HashSet; CanonicalResourceManager.java getList/allResources).
  Classified unstable-oracle; our order is deterministic (sorted).
‡ plan-net snapshot total is 20 (excludes 2 publisher error-artifact goldens —
  `I/O error writing PNG file!` spans, quirk #2). The by-key/by-mustsupport
  totals are 22/22 because those goldens are NOT error artifacts (the failure was
  snapshot-specific), so all 22 count.
§ (resolved 2026-07-03) grid went GREEN corpus-wide once (a) the commonmark
  cell engine landed (below) and (b) two cited grid fixes: the STRUC_DEF_SEE
  dead-i18n-arg contentReference text (quirk #8) and the
  `generateGridDescription` used-gate (SDR:3104 — empty description cell for
  prohibited max=0 elements, plannet-Network).

## The markdown cell engine (2026-07-03) — commonmark, NOT kramdown

**MarkDownProcessor finding**: the SD table description cells do NOT go
through Jekyll/kramdown OR even fhir-core's `MarkDownProcessor` COMMON_MARK
path. `Cell.addMarkdown` (HTG:340-353) runs **vanilla commonmark-java**
(`Parser.builder().build()` — no TablesExtension, no `preProcess` raw-html
escaping) with `HtmlRenderer.escapeHtml(true)`, then re-parses the HTML string
via XhtmlParser into Pieces (`htmlToParagraphPieces`, HTG:392-425 — two
`Piece("br")` before every non-first top-level child; `<p>` inlined via
`addNode` HTG:439-472; other elements become tag-Pieces carrying XhtmlNode
children). render_md (kramdown, F1b) is the WRONG engine for these cells and
was not wired in. Instead `render_sd::commonmark` (~530 LOC) implements the
commonmark-java subset the corpus exercises (measured over all 7,410
definition/comment strings): paragraphs, tight ul/ol, hard/soft breaks, code
spans, links, delimiter-stack emphasis with flanking + intraword-`_` +
rule-of-3. 11 unit tests pin exact HTML shapes; out-of-scope constructs fail
loud. `markdown.rs` is the faithful htmlToParagraphPieces/addNode port over
`render_xhtml`'s parser (+ a styled variant for the diff view's dimmed
binding descriptions, HTG:372/414/441-466).

## diff / diff-all (2026-07-03) — pointer RECONSTRUCTION, snapshot_gen untouched

The prior analysis ("needs the 23 ProfileUtilities DERIVATION_EQUALS set-sites
ported into snapshot generation") was WRONG in a useful way. Key discovery:
most diff-view dimming is derived at RENDER time from
`SNAPSHOT_DERIVATION_POINTER` (the diff element -> base element link), not from
the snapshot-gen property stamps:
- genCardinality (SDR:1431-1447) copies missing min/max from the POINTER and
  stamps EQUALS itself; genTypes (SDR:2357-2364) likewise for types;
  generateDescription's short-fallback (SDR:1594-1602) dims the pointer's
  short unconditionally; makeUnifiedBinding (SDR:2726-2758) merges the
  pointer's binding parts in with EQUALS stamps.
- The pointer itself is reconstructable from JSON: pointer(diffElem) = the
  profile's OWN snapshot element with the same id (PU:2591 sets it to the base
  clone that becomes that snapshot element; for properties the diff didn't
  restate, snapshot value == base value byte-for-byte). Choice renames need two
  id fallbacks: sliced (`value[x]:valueQuantity` -> `valueQuantity`) and
  unsliced (`valueQuantity` -> `value[x]`, camelCase rewrite) — the walk's
  isSameBase matches (PU:2507 / PPP:887-909).
- Element list = `supplementMissingDiffElements` (SGPP:1102-1181; pure function
  of the differential — sets NO userData; ported as `render_sd::diff`).
  Synthetic (sparse-fill/root) elements get NO pointer, faithfully.
- genFixedValue: empty pattern/fixed properties render only when
  `values.size() > 0 || snapshot` (SDR:2786) — the diff view skips them.
- checkForNoChange piece map: genTypes wraps separators/profiled/plain/
  genTargetLink pieces (SDR:2379-2500, 2534-2565) but NOT the
  Reference-link/parens/aggregation pieces; `getOpacity()` = `opacity: 0.5`
  (RenderingContext.java:76); reference pieces double the style
  (`opacity: 0.5; opacity: 0.5`) via the HTG:1171 double-addStyle Java wart
  already ported in render_tables.
- The snapshot-gen-only property EQUALS (a diff RESTATING a value equal to
  base — e.g. short/definition/fixed identical to base) is NOT modeled; corpus
  evidence: zero fragments need it (all restated corpus values differ from
  base). If a future IG hits it, the fix is a base-profile compare at render
  time, still not a snapshot_gen change. **snapshot_gen was not touched** —
  snapshot output byte-neutrality holds by construction (no gate run needed).

### by-mustsupport / by-key (2026-07-03) — the filtered-view kinds

Both route the SUMMARY `generateTable` engine over a filtered `sdCopy` element
list (no new render engine). All GREEN corpus-wide.
- **by-mustsupport** (SDR:552): list = getMustSupportElements (MS elements +
  ancestors, example cleared, binding/constraint cleared on non-MS copies, MS
  flag zeroed); non-MS-below-root rows dimmed via render_opaque→opacity 0.5.
  mustSupportMode threads through gen_types/make_choice_rows: type/target/profile
  filters (`!all&&!any` allTypesMustSupport / allProfilesMustSupport) + S-flag
  suppression. Load-bearing fix: pattern genFixedValue `skipnoValue =
  mustSupportOnly` (SDR:2085) suppresses empty pattern properties in the MS view.
- **by-key** (SDR:532): list = getKeyElements — non-logical constraint profiles
  filter to the "key" set (scanForKeyElements oldMS||newMS predicate vs the
  base-type element); else all elements. allInvariants=T (NOT F — the publisher
  arg order: allInv is position 12 = true, mustSupport position 14 = false).

`resolve_binding` + `BindingRes` + `strip_version` now live in `context.rs`
(shared by table + grid). The dead `links.rs` (grid's old hardcoded table) was
removed once grid moved onto IgContext.

## Resolution engine (IgContext) — the publisher-parity link/binding oracle

`context.rs` reproduces the publisher's canonical→webPath/name resolution from
the same inputs the publisher had:
- own IG resources (relative `{Type}-{id}.html` pages),
- the dependsOn closure of packages (package.json `url` base via
  PackageHacker.fixPackageUrl; spec.internals `paths`; getOverride table;
  `.examples` packages excluded; `hl7.fhir.us.core.vNNN` facades mapped to the
  real us-core packages — SimpleWorkerContext.java:695),
- fhirVersion-matched core package only (an R4 IG never resolves R5 core),
- **masterDefinitions rule** (CanonicalResourceManager.java:394-400 + get()):
  core-package CodeSystem/ValueSet/specializing-SD copies win for non-THO urls;
  terminology.hl7.org urls are excluded from master so THO packages win,
- resource-version pins (`url|ver` matches the RESOURCE version from
  .index.json), highest-(resource-version, package-version) otherwise,
- meta.source webPath fallback for spec.internals-less special packages
  (us.cdc.phinvads ViewValueSet URLs — PhinVadsImporter.java:67 +
  publisher SpecMapManager.getPath def param),
- the tx cache (`input-cache/txcache/vs-externals.json`) as last resort with
  `external.png` flagging (BaseWorkerContext.java:3499-3511).

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

1. **Per-run HTG uuid** (HTG:128 `uuid = UUIDUtilities.makeUuidLC()`): a random
   per-JVM constant emitted in every active-table's filter script. Supplied as
   run context; the corpus harness harvests each IG's uuid from its goldens.
2. **Publisher error-artifact goldens**: plannet-Network / plannet-Practitioner
   `-snapshot` goldens are `I/O error writing PNG file!` spans (publisher-side
   failure). Invalid oracles; excluded with a note.
3. **Abstract-profile child order**: non-deterministic in the publisher
   (identity-hash HashSet iteration, CanonicalResourceManager.getList). Ours is
   deterministic; one cycle fragment diverges on order only.
4. **Fixed-value links are dead**: `getLinkForUrl` gates on
   `hasResource(CanonicalResource.class, url)` which never matches (abstract
   class fetch) — all 193 fixed values in the us-core goldens are unlinked.
   Reproduced by never linking.
5. **`active-tables`** is per-IG template config (PublisherIGLoader.java:443):
   us-core false, plan-net/cycle true. Read from the template's
   onGenerate-ig-working.json.
6. **Grid name-cell bold is dead** (SDR:2625 `genGridElement`): the bold branch
   tests `element.getType().get(0).isPrimitive()`, but `isPrimitive()` is
   `Base.isPrimitive()` (Base.java:266) — hard-coded `return false` and never
   overridden on `TypeRefComponent`. So the grid name piece is NEVER bold.
   Reproduced by never bolding. (Same shape as quirk #4: a dead Java branch.)
7. **byKey additionalBindings comparison is a no-op** (publisher
   scanForKeyElements, SDR:747-749): `getAdditional(binding.getAdditional())`
   is compared to `getAdditional(binding.getAdditional())` — the SAME value on
   both sides (a copy-paste bug; the base binding is never consulted). So the
   additional-bindings signal never flips `bindingChanged`. Reproduced by omission.
8. **STRUC_DEF_SEE dead i18n args** (grid contentReference,
   fhir-core SDR:3111/3113): the rendering phrase `STRUC_DEF_SEE = See`
   (rendering-phrases.properties) has NO `{0}` placeholder, so the element-path
   / typeName arguments passed to formatPhrase are silently dropped. Golden
   text is `See` (same-source) / `See.` + path (other-source, e.g.
   `See.QuestionnaireResponse.item` in us-core-questionnaireresponse-grid).
   NOTE the SUMMARY-table genTypes contentReference (SDR:2329-2333) is a
   DIFFERENT code path that appends " " and real link text — not affected.

Faithful ports of Java warts (not quirks — reproduced exactly): (previously listed)
- `addStyledText` background-color precedence bug (HTG:521) → emits the literal
  `; null` style suffix (`color: black; null`), byte-verified.
- Grid tables leave `mode` unset (null) so grid `<a>`s never get
  `no-external`/`data-no-external` (guard `mode == XHTML`, HTG:1160) —
  `Gen.mode: Option`, `None` for grid. **Same for BINDINGS/OBLIGATIONS**:
  initCustomTable (SDR:885) never sets `this.mode` (only initNormalTable does,
  HTG:858), so those tables also emit zero no-external — `Gen::new` (mode None).
- **initCustomTable help16 src is not makeSecureRef'd** (SDR:892
  `pathURL(prefix,…)` vs initNormalTable HTG:866 `pathURL(makeSecureRef(prefix),…)`)
  — BINDINGS/OBLIGATIONS help16 stays `http://`, SUMMARY is `https://`. Ported
  in `init_custom_table` (render_tables).
- `context.prefixAnchor` (RenderingContext) is null-prefix for grid, so the
  "g-" anchor prefix is applied exactly once (by the HTG, in renderCell).
- Pattern genFixedValue `skipnoValue = mustSupportOnly` (SDR:2085), fixed
  genFixedValue `skipnoValue = false` (SDR:2069): in the by-mustsupport view
  empty pattern properties are suppressed; empty fixed properties are not.

## bindings / obligations modes (2026-07-03, session 4) — StructureMode

DONE: **snapshot-bindings/-all, snapshot-obligations/-all + diff-bindings/-all,
diff-obligations/-all** ALL GREEN corpus-wide. `StructureMode` enum threaded
through TableConfig; the mode selects `initCustomTable` (Name + scanned columns)
vs `initNormalTable` (SDR:627-648) and the per-element cell builder in
genElement (SDR:1022-1035). Findings:

- **`snapshot-obligations` uses the `snapshot()` wrapper, not `obligations()`**
  (PublisherGenerator:1920 `sdr.snapshot(..., OBLIGATIONS, all)`). So idSfx =
  S/SA (NOT O/OA) and uniqueLocalPrefix = `mc(OBLIGATIONS)+"s"` = os/osa. The
  `obligations()` wrapper (prefix oo/ooa, idSfx O/OA) makes the DISTINCT
  `-obligations`/`-obligations-all` fragments (not in the F3 set). us-core's
  active-tables=false hid this (no id emitted); plan-net (active-tables=true)
  revealed it — the golden id is `…S`. snapshot-bindings likewise: bs/bsa, S/SA.
- **initCustomTable leaves HTG.mode = null** (SDR:885 never sets `this.mode`;
  only initNormalTable does, HTG:858). So BINDINGS/OBLIGATIONS tables carry ZERO
  `no-external`/`data-no-external` link attrs (HTG:972/1153 gate on
  `mode == XHTML`) — same as grid. Reproduced with `Gen::new` (mode None) for
  these modes. (NEW faithful port, below.)
- **initCustomTable help16 src stays `http://`** — SDR:892 `pathURL(prefix,…)`
  WITHOUT makeSecureRef, where initNormalTable (HTG:866) upgrades to `https://`.
  Golden-confirmed: -bindings/-obligations = http://, -snapshot = https://.
  (NEW faithful port.)
- **OBLIGATIONS skip gate** (SDR:930): an element with no obligation on it or any
  descendant is skipped ENTIRELY (no row, no anchor bump, no recursion). Since NO
  IG in this corpus uses obligation extensions, every obligations table is
  Name-only header + footer, ZERO data rows. The ObligationsRenderer body
  (renderCodes/CodeResolver) is NOT ported — it fires a loud gap if columns ever
  populate (zero corpus hits). scan_obligations actor-column titles likewise gap.
- **BINDINGS choice-row guard** (SDR:1173): makeChoiceRows is `mode == SUMMARY`
  only; the per-type `[x]` choice rows do NOT appear in BINDINGS/OBLIGATIONS.
  This was the one structural bug (5 extra scaffold rows on `onset[x]` → wrong
  tree-line `tbl_bckNN` on every following row); fixed with the mode guard.
- **genElementBindings** (SDR:1259) uses the ABR:437 `render(children, list, sd)`
  path — one binding → inline `<a href title>display</a>`, many → `<ul><li>` —
  NOT the SUMMARY additional-bindings TABLE. `collect_bindings(element, col.id)`
  gathers: strength binding (as synthetic ab when strength==col), max/minValueSet
  exts, native binding.additional, ext-additional — filtered by purpose.
  Reused the existing `resolve_binding` (the C4 oracle already in context.rs).
- Holder rows (:All Slices / :All Types / Slices-for) push `columns.len()` empty
  cells in BINDINGS/OBLIGATIONS (SDR:1078/1107/1143) vs SUMMARY's 4-cell pattern
  — unified in `push_scaffold_tail`.

## Remaining

Prior cycles: grid→IgContext migration, by-mustsupport/-all, by-key/-all
(session 2); grid + diff/-all GREEN (session 3, commonmark + pointer recon).

- **span/spanall**: `generateSpanningTable` (SDR:3713) — separate entry point.
  (Only remaining kind NOT ported. Goldens exist: 7/22/70.)
- **Simplification candidates (logged)**: (a) grid.rs `gen_types`/
  `gen_target_link` are branch-for-branch duplicates of table.rs's (both port
  the SAME Java `genTypes`/`genTargetLink`); table.rs's now additionally
  carries the `dim` param — unifying would let grid drop its copy (grid never
  dims: non-diff). Deferred to a consolidation pass (touches two green paths,
  gate carefully). (b) table.rs `gen_types_erased` (the lifetime-erased
  ext-value duplicate) could fold into gen_types with a lifetime refactor.
- Residual gap markers in table.rs (each fires loudly): choice groups
  (readChoices/processConstraint), aggregation modes, standards-status flag,
  cross-structure contained targets, complex merged-pattern partner rows,
  usage cells in additional bindings, narrative language/source-control exts.
- Diff residual risk (documented, zero corpus hits): a diff RESTATING a
  property byte-equal to base would dim in the publisher (snapshot-gen EQUALS)
  but render bright here; fix would be a render-time base-profile compare.

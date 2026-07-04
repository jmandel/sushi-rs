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
| **span** | **7 / 7** | **22 / 22** | **70 / 70** |
| **spanall** | **7 / 7** | **22 / 22** | **70 / 70** |

**ALL kinds GREEN corpus-wide** (2026-07-03, session 4): bindings/obligations
modes + their diff variants + span/spanall. All the non-SUMMARY kinds pass
cleanly on cycle (7/7): the mode/entry-point is the LOAD-BEARING difference, not
the snapshot input — the cycle snapshot-source variance (quirk †) only bites the
SUMMARY element rows (which the by-Name custom tables mostly don't restate, and
which span doesn't render at all).

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
9. **`*-ref(-all)-list` References-column order is unstable** (CrossViewRenderer
   renderVSList:1494-1505 / renderCSList:1759-1770): the References cell iterates
   `Set<Resource> rl = (HashSet) vs.getUserData(pub_xref_used)` in Java
   identity-hash order (nondeterministic per JVM run) when `rl.size() < 10`;
   `>= 10` collapses to the deterministic `"{N} references"`. Same unstable-oracle
   class as #1 (HTG uuid) and #3 (abstract-profile child order). We emit a
   DETERMINISTIC order (first-seen over the sorted own-resource scan). RIGOROUSLY
   CONFIRMED a valid member of the oracle order-set: `corpus classify-reflist <ig>`
   parses ours+golden per row and proves the (title,link) reference MULTISET is
   identical per cell (no duplicate pairs corpus-wide → multiset==set); the only
   byte difference is intra-cell order. 87 order-only cells across the corpus
   (cycle 9, plan-net 24, us-core 54). These fragments therefore CANNOT be
   byte-green by construction; they are correct-modulo-quirk.

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

## span / spanall (2026-07-03, session 4) — generateSpanningTable

DONE GREEN corpus-wide. `render_sd::span` (~330 LOC): a separate entry point
(SDR:3713), NOT generateTable. A constraint profile's focus row + one hop into
each typed, non-max-0 element's first Reference-targetProfile that is itself an
in-IG constraint (`onlyConstraints=true`, `constraintPrefix=igpkp.getCanonical()`).
Findings:
- **span vs spanall differ ONLY in the HTG anchor prefix** ("sp" vs "spall",
  PublisherGenerator:2080/2084) — both pass onlyConstraints=true + the same
  canonical. Golden anchors `sp-…` / `spall-…`.
- **genSpanEntry does NOT call makeAnchorUnique** (SDR:3690-3694): the SAME
  child profile under two references gets the SAME anchor (golden: two
  `sp-us-core-patient`, no `.2`). This was the one bug — I had ported the
  makeAnchorUnique dedup from generateTable; removing it took us-core 53→70.
- **initSpanningTable** (SDR:3674): active=true (isActive), docoRef=
  `formats.html#table` (NOT readingIgs), docoImg=`pathURL("","help16.png")`=
  `help16.png` (no makeSecureRef, no https), 4 titles Property/Card./Content/
  Description. Mode null → no no-external (Gen::new).
- `constraintPrefix` = the IG canonical, newly captured in IgContext
  (`own_canonical_prefix`, the ImplementationGuide url minus /ImplementationGuide/id).
- getCardinality (SDR:3751) walks parents tightening min/max; the content cell
  links the child profile's webPath with the resType text; description = the
  child profile.getName(). Observation.code key-property fixed summary
  (isKeyProperty, SDR:3669) ported but rarely hit.

# F4 — remaining USED fragment kinds (leaves + VS/CS/instance + IG aggregates)

## F4 architecture finding (session 5, 2026-07-03)

The F3 SD **table** kinds route through fhir-core's
`org.hl7.fhir.r5.renderers.StructureDefinitionRenderer.generateTable`. The F4
**leaf** kinds are produced by a DIFFERENT class: the **publisher's**
`org.hl7.fhir.igtools.renderers.StructureDefinitionRenderer` (3204 LOC, a
`CanonicalRenderer` subclass) whose methods `summary/invOldMode/tx/txDiff/dict/
mappings/references/useContext/pseudoJson/uses/contexts` each emit one leaf.
Citations for F4 = `psdr` = that publisher class (path in
scratchpad/pg_path.txt / psdr_path.txt / sdr_r5_path.txt). VS/CS leaves come
from the publisher's `ValueSetRenderer`/`CodeSystemRenderer` in the same dir.
Producing-call map: PublisherGenerator generateOutputsStructureDefinition
(fragment calls SD leaves @1831-2074), ...ValueSet (@1533-1580), ...CodeSystem
(@1483-1503), shared per-resource (contained-index @894, history @1150).

## F4 TARGET LIST (denominators enumerated from goldens — SDs/VSs/CSs producing each kind)

Per-IG counts (cycle / plan-net / us-core). All leaves wrap in `{% raw %}..{% endraw %}` (wrap_raw).

**SD leaves** (all: 7 / 22 / 70 producers):
dict, dict-diff, dict-key, dict-ms, dict-active, inv, inv-diff, inv-key,
tx, tx-diff, tx-key, tx-must-support, tx-diff-must-support, maps,
sd-use-context, sd-xref, summary, summary-all, pseudo-json, pseudo-ttl,
pseudo-xml, contained-index, history.

**ValueSet leaves** (cld 2/24/21, expansion 2/22/20, xref 2/24/21, history, contained-index).
**CodeSystem leaves** (content 1/14/4, xref, history, contained-index).
**Instance** (per example resource): html (narrative), history, contained-index.
**IG aggregates** (1 each unless noted): dependency-table(-short/-nontech),
globals-table, ip-statements, expansion-params, cross-version-analysis(-inline),
deprecated-list, deleted-extensions, new-extensions, canonical-index,
obligation-summary, summary-observations, summary-extensions,
related-igs-list/-table, codesystem-list, codesystem-ref-list,
codesystem-ref-all-list, valueset-list, valueset-ref-list, valueset-ref-all-list,
list-* (per-type; -json/-xml pseudo-payloads), table-* (per-type; -json/-xml),
maps-<CoreType> (one per FHIR core type, IG-invariant).

### Classification by cost (from golden byte sizes + producing-method reading)
- **CONSTANT** (1 distinct value corpus-wide): contained-index (empty),
  history (empty), pseudo-ttl, pseudo-xml, tx-diff/inv-diff when no diff
  constraints. Trivial fixed strings via fragmentError/genContainedIndex/
  HistoryGenerator (all empty in this corpus).
- **SELF-CONTAINED small engines**: summary/-all (i18n phrase counts),
  invOldMode (inv/-diff/-key; XhtmlComposer(false,TRUE)=pretty table),
  tx/txDiff (binding table), useContext (extension context list).
- **SELF-CONTAINED XL**: pseudo-json (46KB; JsonXhtmlRenderer of the SD JSON),
  dict (240KB; fhir-core sdr.renderDict — big).
- **WHOLE-IG cross-resource scans** (need full FetchedFile resource set +
  examples + capstmts + r5 sdmap.details): references (sd-xref), uses, maps
  (mappings), and most IG aggregates (dependency-table, list-*, table-*,
  codesystem/valueset-*-list, summary-observations).
- **TERMINOLOGY** (need vs-externals.json / tx-cache expansion source, per
  F4 brief): VS cld/expansion, CS content.
- **NARRATIVE (instance html)**: DataRenderer family — sizing TBD (flagged XL
  candidate in brief).

## F4 scoreboard (kind × IG → byte-identical/total; ✅=corpus-wide green)

| kind | cycle | plan-net | us-core | notes |
|---|---|---|---|---|
| contained-index | 7/7 | 22/22 | 70/70 | ✅ constant empty (genContainedIndex, no contained) |
| history | 7/7 | 22/22 | 70/70 | ✅ constant empty (HistoryGenerator, no history) |
| pseudo-ttl | 7/7 | 22/22 | 70/70 | ✅ constant fragmentError "Turtle template" |
| pseudo-xml | 7/7 | 22/22 | 70/70 | ✅ constant fragmentError "Xml template" |
| inv | 7/7 | 22/22 | 70/70 | ✅ invOldMode GEN_MODE_SNAP |
| inv-key | 7/7 | 22/22 | 70/70 | ✅ invOldMode GEN_MODE_KEY (reuses key_elements) |
| inv-diff | 7/7 | 22/22 | 70/70 | ✅ invOldMode GEN_MODE_DIFF (reuses supplement_missing_diff) |
| sd-use-context | 7/7 | 22/22 | 70/70 | ✅ (session 6) deprecated markdown via publisher_markdown |
| summary | 7/7 | 22/22 | 69/70 | (session 6) Extension SDs GREEN via extensionSummary+md; 1 fail=practitioner nested-split (cited residual) |
| summary-all | 7/7 | 22/22 | 69/70 | same as summary, anchor `a-` |
| contained-index (ALL types) | 17/17 | 166/166 | 443/443 | ✅ empty across SD/VS/CS/instances (626 files/IG) |
| history (ALL types) | 17/17 | 166/166 | 443/443 | ✅ empty across ALL resource types |

**contained-index + history are byte-identical (empty) for EVERY resource type**
(SD/VS/CS/ImplementationGuide/Bundle/all instances) corpus-wide — one
`empty_body()` covers the SD/VS/CS/instance targets for these two kinds at once.
Harness gained a `contained-index-all`/`history-all` mode that scans every
`*-{kind}.xhtml` golden (not just SDs).

### F4 sizing verdicts (required findings)

- **Expansion source (VS `expansion`, CS `content`, VS `cld`):** the golden VS
  expansions say "Expansion from tx.fhir.org based on Loinc v2.82" — sourced
  from the build's `input-cache/txcache/` (external VS defs `vs-*.json` +
  cached tx-server `$expand` results, `cs-*.json`/`*.cache`), NOT live tx and
  NOT local enumeration. 75 vs-*.json in us-core's cache. Rendering these needs
  reading the cached expansion + the concepts-table renderer (+ code-display
  terminology). MEDIUM-XL; deferred (terminology category).
- **Instance `html` (narrative): XL on its own — a full DataRenderer port.**
  The fragment is the publisher-GENERATED narrative (`text.status:generated`),
  NOT the authored div: a fresh property-by-property render of each example
  resource (Profile banner + status/category/code/subject/… with CodeableConcept
  title tooltips, Reference resolution+links, Quantity/dateTime formatting). This
  is fhir-core's DataRenderer + ResourceRenderer + every datatype renderer
  (thousands of LOC) PLUS terminology display lookups + reference resolution —
  comparable in size to the entire F3 table effort. us-core has 695 instance
  html fragments (139 distinct Observations + others). **Recommend its own
  phase, not folded into F4.** history + contained-index for instances ARE done
  (empty, above).

### F4 remaining (per-kind status)
- **DONE green:** contained-index, history (all types), pseudo-ttl, pseudo-xml,
  inv/-key/-diff, sd-use-context (−3 md), summary/-all (−15 md, −4 nested-split).
- **Blocked on publisher-markdown engine** (preProcessMarkdown + MarkDownProcessor;
  real corpus hits): summary Extension-type SDs, sd-use-context deprecated, tx
  binding descriptions, dict, sd-xref present().
- **Self-contained XL (portable, no md/no whole-IG):** pseudo-json
  (JsonXhtmlRenderer, ~400 LOC recursive JSON-shape walk). Next-best target.
- **Whole-IG cross-resource scans:** sd-xref/references, uses, maps, CS/VS xref,
  + most IG aggregates (dependency-table, list-*, table-*, codesystem/valueset-
  *-list, summary-observations). Need the full FetchedFile resource set.
- **Terminology:** VS cld/expansion, CS content (tx-cache source, above).
- **dict family (240KB):** fhir-core sdr.renderDict + markdown — XL + md-blocked.

summary findings (session 5, checkpoint 3):
- Non-extension profiles are markdown-FREE (describeProfile/summariseExtension
  use present()+webPath, no markdown). Extension-type SDs route the description
  through extensionSummary→processMarkdown → LOUD GAP (15 us-core, 13 plan-net).
- **corePath = getSpecUrl(igVersion)+"/"** (checkAppendSlash(specPath)) — R4 =
  `http://hl7.org/fhir/R4/`. Threaded as `core_path_v` (the slices link).
  Differs from the F3 table path's corePath="".
- **igp.isDatatype(name) resolves the CORE type by NAME and requires
  derivation==specialization** (IGKnowledgeProvider:551). An extension whose URL
  matches the core SD prefix is kind=complex-type but derivation=constraint →
  isDatatype false → it DOES appear in the Extensions section. Using a
  resolve-the-profile is_data_type (kind only) wrongly suppressed it. Fixed with
  igp_is_datatype (kind primitive/complex AND derivation==specialization).
- FMM maturity ext value is `valueInteger` (readStringExtension reads any
  primitive).
- **RESIDUAL (4 us-core, documented):** parentChainHasOptional outright-vs-nested
  SPLIT. The exact predicate walks the SNAPSHOT_DERIVATION_POINTER (intermediate
  base-profile min), not own-snapshot min nor base.min. Total mandatory count is
  always correct; only the "(N nested)" sub-split diverges on practitioner +
  3 observation profiles. Needs the reconstructed-pointer chain (diff.rs
  machinery). Low value vs the markdown blocker; left as a silent approximation.

sd-use-context findings (session 5, checkpoint 2):
- **Composer inline no-pretty fix (load-bearing).** `el()` built nodes via
  `XhtmlNode::new` which does NOT set `notPretty` for the inline element set
  (b/code/a/span/…). fhir-core's `div.code()`/`li.b()` go through `makeTag`
  (XhtmlNode.java:218) which sets notPretty. Symptom: `<code><a>Location</a>`
  got indented/newlined instead of the golden's inline `<code>\n<a…>x</a>    </code>`.
  Fixed by adding `render_xhtml::XhtmlNode::new_tag` (makeTag as free ctor) and
  routing `el()` through it. This also hardened inv (still green).
- Non-extension SDs + Element-ID / Extension / fhirpath contexts + context
  invariants ported (composer html_pretty). Element-ID links the core type
  webPath via `ctx.resolve_type` (R4 core page).
- **3 us-core gaps = the deprecated standards-status markdown block**
  (psdr:2879 → `ddiv.markdown`). This needs the PUBLISHER markdown engine
  (BaseRenderer.processMarkdown = preProcessMarkdown `[[[link]]]`/`||`/relative-
  url rewrite + fhir-core MarkDownProcessor). That engine is a shared F4/F1b
  dependency (see the blocker note below). Fired as LOUD GAP; harness now
  catch_unwinds per-SD and reports `(N gaps)`.

**F4 MARKDOWN BLOCKER (session 5).** Beyond the trivial leaves, most remaining
kinds route description/definition strings through the publisher's
`processMarkdown` (BaseRenderer:184): preProcessMarkdown (FHIR `[[[ ]]]` link
syntax, `||`→para, `processRelativeUrls`, corePath prefixing) THEN
`markdownEngine.process` (fhir-core MarkDownProcessor, dialect DARING_FIREBALL
by default / COMMON_MARK per param — the F1b engine). Corpus HITS (not zero):
summary simple-extension descriptions (11 us-core), useContext deprecated (3),
tx binding descriptions, dict, sd-xref `present()`. This is a genuine shared
substrate, larger than any single leaf. Recommend a dedicated
`publisher_markdown` port (preProcessMarkdown + a MarkDownProcessor subset)
as its own increment before summary/tx/dict can go fully green.

`render_sd::leaf` (~700 LOC). Findings from checkpoint 1 (session 5):
- **inv composer = HTML pretty** (`new XhtmlComposer(false,true)` = xml=false,
  pretty=true → Config::html_pretty + compose_nodes overload, NO breakBlocks).
  Table is `class="list presentation" data-fhir="generated-heirarchy"`.
- **allInvariants defaults true; NO IG in corpus sets `show-inherited-invariants`**
  (PublisherIGLoader:479). So the invOldMode source filter (psdr:1241) never
  excludes — threaded as `all_invariants` param, corpus value = true always.
- **best-practice extension URL is lowercase** `elementdefinition-bestpractice`
  (ExtensionDefinitions.EXT_BEST_PRACTICE, r5:59) — NOT `-bestPractice`. Grade
  column shows "best practice" for dom-6. This was the only inv bug.
- inv-key reuses `table::key_elements_pub` (getKeyElements); inv-diff reuses
  `diff::supplement_missing_diff_elements`. Both exported as pub wrappers.
- F3 regression floor RE-CONFIRMED byte-identical (snapshot/diff/grid/by-key/
  bindings/span all match the F3 scoreboard incl. the known cycle † failure).

## Session 6 (2026-07-03): publisher_markdown + summary/sd-use-context GREEN

### MARKDOWN-ENGINE DETERMINATION (required finding — resolves the blocker)

The SDR's `markdownEngine` is **COMMON_MARK** for every corpus IG.
`PublisherIGLoader.java:908-910`: `version 1.0/1.4/1.6/3.0 → DARING_FIREBALL,
else → COMMON_MARK`. The corpus is R4/R4B/R5 (4.0/4.3/5.0) → COMMON_MARK.
`XhtmlFluent.markdown` (fhir-core:305) also hard-codes COMMON_MARK.

COMMON_MARK is `MarkDownProcessor.processCommonMark` (fhir-core
MarkDownProcessor.java:239-247), which is **NOT** the vanilla `Cell.addMarkdown`
engine (`render_sd::commonmark`). Deltas:
  (a) `preProcess(source)` (MDP:222-237) — a regex that backslash-escapes raw
      HTML tags (`<tag …>`, `</tag>`, `<!`/`<?`) so they render as literal `<`;
  (b) `TablesExtension` enabled in parser + renderer;
  (c) `html.replace("<table>", "<table class=\"grid\">")`.
Same `escapeHtml(true)`.

**Corpus measurement (1229 markdown-bearing strings over all 3 IGs — the actual
strings that flow through the blocked kinds):** zero `[[[`, zero `||`, zero raw
HTML tags, zero tables/fences. Live features = links, code spans, `*em*`, tight
bullet lists, soft/hard breaks — ALL already covered by `commonmark.rs`. So (a)/
(b)/(c) are inert on the corpus; they are ported faithfully in
`publisher_markdown` and (a) is regex-exact, (b) fires a LOUD GAP on a GFM table.
Verified against golden bytes BEFORE building: us-core-birthsex-summary (simple
ext, plain desc → stripPara) and us-core-ethnicity-summary (complex ext, link +
`(CDCREC)` nested parens + bullet list → stripAllPara) both match `commonmark.rs`
output exactly (commonmark emits LF; the publisher StringBuilder scaffolding uses
CRLF — the golden's mixed `\n`/`\r\n` confirms the split).

`preProcessMarkdown` (BaseRenderer:78): `||`→para, `[[[link]]]` via IgContext,
`ProfileUtilities.processRelativeUrls`. corePath = `http://hl7.org/fhir/R4/`, so
`isLikelySourceURLReference` (PU:2320) takes the `baseUrl.startsWith("http://
hl7.org/fhir/R")` fast path: a relative `](x)` link is corePath-prefixed only if
basename `x` ∈ `BASE_FILENAMES` (the 208 FHIR core spec pages, ported) or starts
`extension-`. Corpus-verified: the 8 relative links (all IG-local pages) are left
unchanged. So preProcessMarkdown is effectively inert on the summary/useContext
Extension descriptions.

`publisher_markdown.rs` (~530 LOC): pre_process, md_process, pre_process_markdown,
process_markdown (String path), markdown_children_from_html (XhtmlFluent.markdown:
parse `<div>`+html+`</div>` and add the parsed `<div>` — the nested-div the
goldens show, because `XhtmlParser.parse(..)` returns an XhtmlDocument whose
single child IS that `<div>`), strip_para/strip_all_para. 4 unit pins.

### tx is NOT markdown-blocked (correction to session-5 brief)

`txItem` is called with `hasDesc=false` HARD-CODED (psdr:889 `txItem(txmap, tbl,
path, sd.getUrl(), false)`), so the `if (hasDesc) td.markdown(...)` block
(psdr:1042-1044) NEVER fires. The binding description is only a `title=`
attribute (psdr:933). tx's real dependency is TERMINOLOGY resolution:
`context.findTxResource(ValueSet)` (tx-cache/package VS), the ValueSet
title/name/webPath, source-package attribution (`THO v7.2` = getSourcePackageName
+ presentVersion), the copy-button, external.png, `insertBreakingSpaces` on path,
and `showVersion` → `ResourceRenderer.renderVersionReference` (the `📦2.0.0`
version cell with the "fixed to …, found through package references" title).
Composer = `new XhtmlComposer(false, true)` = html_pretty (same as inv). NOT a
markdown port. Reclassify tx under TERMINOLOGY, not markdown.

### summary Extension SDs (extensionSummary psdr:285)

Simple ext = `<p>` + `SDR_EXTENSION_SUMMARY` = "Simple Extension with the type
{typeSummary}: {stripPara(processMarkdown(description))}" (+`_MODIFIER` variant).
Complex ext = `<p>Complex Extension: {stripAllPara(...)}</p>` + a `<ul data-fhir=
"generated-heirarchy">` of the value-slice sub-elements. EXT_SUMMARY (psdr:157)
short-circuits the whole method. The Extension branch (psdr:222) REPLACES the
mandatory/refs/ext/slices block (only the FMM tail also runs). Result: summary/
-all us-core 51/55+15gap → 69/70 (0 gaps); plan-net 9/9+13gap → 22/22 GREEN.

### summary nested-split — silent approximation ELIMINATED (3 of 4)

`parentChainHasOptional` (psdr:318) now uses the faithful
SNAPSHOT_DERIVATION_POINTER walk via `diff::reconstruct_diff_pointers` (factored
out of the diff table path; exact/sliced-choice/unsliced-camelCase id aliases +
`table::dechoice_candidates_pub`). The old own-id `position` walk returned true on
any id not literally in the snapshot, which mis-split the choice-renamed value[x]
mandatories. observation-occupation / -pregnancyintent / -pregnancystatus now
byte-parity. RESIDUAL: us-core-practitioner's 2/1 sub-split among {identifier.
system, identifier.value, name.family} is IRRECOVERABLE from finished JSON — those
are datatype expansions absent from the immediate base (core Practitioner expands
neither Identifier nor HumanName), so the publisher pointer is a base clone read
mid-`updateFromDefinition` (PU:2586) whose transient `.min` (datatype default 0)
is later overwritten to 1 in the same object; base.path/base.min are symmetric
across all three → no JSON discriminator. Total mandatory count stays correct (5).
Cited, not a silent shrug (fires no gap; the doc explains the irrecoverability).

### sd-use-context deprecated (psdr:2879) GREEN

Red div + SDR_EXT_DEPR + the nested `structuredefinition-standards-status-reason`
markdown via `preProcessMarkdown → md_process → XhtmlParser re-parse`. us-core
67/70+3gap → 70/70 GREEN corpus-wide.

### pseudo-json GREEN corpus-wide (session 6, forked port)

us-core **70/70**, plan-net **22/22**, cycle **7/7**. `pseudojson.rs` (~700 LOC,
index-based snapshot walk over psdr pseudoJson:1722-2240) + `Ed::constraint_values`.
getSrcFile/getLinkForProfile both resolve through ctx.resolve(_type).web_path
(own SD → local page; core types → spec.internals `datatypes.html#code` joined
with the R4 base); `suffix(link,code)` keeps an existing `#anchor` else appends
`#code`. Quirks (cited in-module):
1. getInvariants allInvariants=FALSE (no invOldMode-style genMode escape) —
   inherited ele-1 dropped → empty `C?` titles (834 corpus-verified).
2. Binding link is ALWAYS `{corePath}null.html` — vs `render_filename` userdata
   null at pseudoJson time; 834/834 golden links are `.../R4/null.html`.
3. Java `List.toString()` leak: non-core targetProfile/profile branch emits
   `[CanonicalType[url]]` verbatim.
4. Version-suffixed target type (`CarePlan|4.0.1`): hasType/hasResource miss →
   raw text, no link.
5. contentReference leaf (empty type array, no children): finished JSON drops the
   in-memory null-code type → `<n/a>` branch; widened to types.len()<=1.
6. describeSlicing leading space; the Java ternary is literal (ordered==false →
   SDR_SORTED, ordered==true → SDR_ANY_ORDER).
7. Complex-extension no-value → `SDR_NOT_HANDLED_EXT` double-nested
   "Not handled yet: complex extension {url}" string.

### tx family GREEN corpus-wide (session 6, forked port — commit 4acae6d9)

All 5 kinds (tx, tx-must-support, tx-key, tx-diff, tx-diff-must-support):
us-core **70/70**, plan-net **22/22**, cycle **7/7** each (empty-binding SDs
render "" faithfully). `tx.rs` (~450 LOC); `context.rs` Resolved gains
`pkg: Option<PkgMeta>` + `tx_server` (PkgEntry reads package.json
name/canonical/title; resolve behavior untouched — full floor re-verified).

- **renderVersionReference branches hit** (RR:1597): STATED 📍 (1320 — core VS
  `|4.0.1` pins; astral glyph NCR-escaped), BY_PACKAGE 📦+actual (280 — THO/
  US Core/phinvads), THIS_PACKAGE 📦 (260 — own-IG VS, keyed off relative
  webPath), FOUND ⏿ + `td opacity: 0.5` (242 — tx-cache externals, no
  sourcePackage), NOTHING → the literally-truncated `Not State` phrase
  (VS_VERSION_NOTHING_TEXT, 7 rows with no valueSet). LATEST/WILDCARD/NONE:
  zero corpus hits, loud gaps.
- **Source cell**: getSourcePackageName = canonical switch (THO/US Core/VSAC/
  DICOM) else package.json `title` (phinvads has none → falls to id
  `us.cdc.phinvads`), + " v" + majMin; link = package.json `url`. Core package →
  "FHIR Std." linked to https spec base. Own IG → "This IG" unlinked. External →
  URL host linked to server.
- **Quirks**: Usage anchor = Java Enumeration-toString leak
  (`terminologies.html#Enumeration[extensible]`); Status column hardcoded
  "Base"; txItemHeadings hasFixed param dead (always false — ValueSet/Code
  heading + render_tx_value unreachable); insertBreakingSpaces = ZWSP after '.'
  at ≥20 chars.
- **Dimming rule**: SNAPSHOT_DERIVATION_POINTER exists only on DIFFERENTIAL
  elements → snapshot walks (tx/-key/-ms) never dim; diff walks dim the VS link
  for every pointer-bearing element, the strength link only when the diff does
  not restate strength.

### F4 remaining after session 6
- **DONE green corpus-wide:** contained-index, history, pseudo-ttl, pseudo-xml,
  inv/-key/-diff, sd-use-context, pseudo-json, tx/-must-support/-key/-diff/
  -diff-must-support, summary/-all (−1 practitioner cited residual).
- **Terminology:** VS cld/expansion, CS content (tx-cache expansion source;
  TxCacheSource abstraction still to design).
- **Whole-IG + markdown:** dict (renderDict + processMarkdown — md now
  unblocked), sd-xref/uses/maps (mappings md at psdr:1482 — unblocked), VS/CS
  xref, IG aggregates (need full FetchedFile resource set from F0 outputs).

## Session 7 (2026-07-03): uses / sd-xref / maps GREEN — whole-IG scan group

The three whole-IG cross-resource SD scans, all GREEN corpus-wide (cycle/plan-net/
us-core). New module `xref.rs` (uses + references + maps wrapper); table.rs gains
the MAPPINGS StructureMode; context.rs gains the whole-IG denominator.

### The whole-IG FetchedFile denominator (required finding)
The publisher's `files` (FetchedResources) / `scanAllResources` set = the IG's
own resources = `ImplementationGuide.definition.resource` (442 for us-core: 70 SD,
139 Observation, 2 CapabilityStatement, 21 VS, …), each `output/{Type}-{id}.json`.
IgContext now loads `own_files` (EVERY own output/*.json incl. url-less example
instances — the old `own` map keyed by url dropped them) + `own_resources()`
enumerator + `own_package_id`. `scanAllResources(SD)` collapses to own SDs here
(deps never reference the IG; `refersToThisSD` == url modulo `|version`).

### uses (psdr:1529) GREEN — findDerived + findUses over own SDs.

### sd-xref / references (psdr:2254) GREEN — findings:
- Examples use FetchedResource.getTitle() = the resource's OWN `name` element
  (PublisherIGLoader:3031), else `Type/id` — NOT present()/title. An SD in an
  examples list shows its NAME (USCoreAllergyIntolerance); an instance `Type/id`.
- EXAMPLE_UPPER_LIMIT = 50 (psdr:85) — examples cap at 50 (PRAPARE/TAPS overflow
  dropped); ordering coincides sorted==IG-declaration-order for this corpus.
- FetchedResource id falls back to the `{Type}-{id}.json` filename when the body
  omits `id` (us-core DocumentReference-discharge-summary stub).
- refList "Show N more" collapse past 5 (psdr:2630); "not used" gate EXCLUDES
  capStmts (a capStmt-only usage shows both lines). Zero-hit branches omitted with
  citation: Original Source, Draw in/Impose/Comply, SearchParameters, R5 sdmap.

### maps (psdr:1323) GREEN — MAPPINGS-mode generateTable ×3 (bounded table.rs ext)
- `StructureMode::Mappings` + `MapStructureMode`; scan_mappings (columns =
  profile.mapping order, hint "??"); genElementMappings (identity match keeps
  LAST, comma-split → plain-text). render_maps_table returns None on no columns.
- ANCHOR_PREFIX_MAP="" (empty anchor prefix); "M"=idSfx. OTHER holds everything
  (all mapping URIs → dest null → IN_LIST/NOT_IN_LIST empty → "No Mappings Found").
- genElementMappings cell = ONE Piece (`p=addText(""); p.addHtml(div)`).
- Java `map.split("\\,")` drops trailing empty strings (trailing `,` → no empty <li>).
- **QUIRK: MAPPINGS render-order icon** — empty-type elements (Extension roots +
  contentReferences) render `icon_resource.png` in maps but `icon_element.gif` in
  snapshot: the publisher's earlier snapshot/dict passes mutate the shared SD's
  types before maps runs (SDR:994 fallthrough). Reproduced for the MAPPINGS view.

### F4 scoreboard delta (session 7)
| kind | cycle | plan-net | us-core |
|---|---|---|---|
| uses | 7/7 ✅ | 22/22 ✅ | 70/70 ✅ |
| sd-xref | 7/7 ✅ | 22/22 ✅ | 70/70 ✅ |
| maps | 7/7 ✅ | 22/22 ✅ | 70/70 ✅ |

Full F3 (15 kinds) + F4 floor re-verified byte-identical after the table.rs
MAPPINGS extension (only the known cycle † and practitioner-summary residuals).

## Session 7 (cont.): IG-level aggregates — 38/45 cells GREEN (aggregates.rs)

New module `aggregates.rs` (~870 LOC, forked port merged 0ce733e4) + a
`run_singleton` corpus harness path. Producers all JAVA (PublisherGenerator /
CrossViewRenderer / DependencyRenderer / DeprecationRenderer / R4ToR4BAnalyser)
— **NONE required XSLT/ant/artifacts.xml** (required classification: confirmed).

| kind | cycle | plan-net | us-core |
|---|---|---|---|
| new-extensions, related-igs-table/-list, globals-table, obligation-summary, deleted-extensions, cross-version-analysis(+-inline), codesystem-list, canonical-index | ✅ | ✅ | ✅ |
| valueset-list | ✅ | ❌ 1-row | ✅ |
| summary-extensions | ✅ | GAP | GAP |
| summary-observations | GAP | ✅ | GAP |
| deprecated-list | ✅ | ✅ | GAP |
| expansion-params | ✅ | ✅ | GAP |

Findings (cited in aggregates.rs):
- Per-IG build facts NOT derivable from output/*.json, fed as golden-matched
  harness inputs: deleted-extensions `(none)`/`(n/a)` (PreviousVersionComparator
  lastVersion from network package-list.json, dpr:267); cross-version-analysis
  `newFormat` (`../package` vs `package`, r44b:316); expansion-params
  interesting-params flag. trackedFragment `<!--$$N$$-->` markers are fragment
  bytes (globals $$4$$, cross-version $$2$$).
- codesystem-list Version column flag = needVersionReferences over the USED-ALL
  ValueSet list (pg:2799 passes the leftover vslist, not the CS list) — the
  used-VS whole-IG scan was ported just for that boolean.
- canonical-index needs oids.ini (authoritative OID registry; IG row OID =
  sushi-config auto-oid-root) + the R5-in-R4 `Basic` re-projection (us-core
  Requirements as Basic with extension-Requirements.{url,version}).
- renderStatus is a no-op (Renderer:84, changeVersion null corpus-wide).

STOP classifications (cited, not approximated):
- **valueset-list plan-net (1 row)**: nucc provider-taxonomy Source cell —
  publisher fetchCodeSystem finds NO CS (dropped in THO 7.2.0; only in
  transitively-loaded 6.1.0/5.5.0) → `Other`; our shared resolve finds the older
  copy. Fixing needs a context.rs resolution-rule change (do-not-modify during
  fork); revisit with care.
- **codesystem/valueset-ref(-all)-list (12 cells)**: References column iterates
  a Java HashSet<Resource> UNSORTED (cvr:1494/1759 — identity-hash order,
  golden-verified unsorted). Same unstable-oracle class as the HTG uuid quirk.
- **ip-statements (singleton)**: deterministic but XL (whole-IG listAllCodeSystems
  element-walk + per-system copyright catalog). Follow-up.
- **dependency-table/-short/-nontech**: XL (full NpmPackage transitive dep graph
  from the package cache). Follow-up.
- Grid branches of summary-extensions/observations/deprecated-list/
  expansion-params: loud panic! gaps.

## Session 7 (cont.): dict family MERGED — 487/495 (98.4%), zero loud gaps

`dict.rs` (~2,700 LOC, forked port, merge 26c1de16-content). Scoreboard:

| kind | cycle | plan-net | us-core |
|---|---|---|---|
| dict | **7/7** | **22/22** | 69/70 |
| dict-active | **7/7** | **22/22** | 69/70 |
| dict-ms | **7/7** | **22/22** | 69/70 |
| dict-key | **7/7** | **22/22** | 67/70 |
| dict-diff | **7/7** | **22/22** | 69/70 |

Full floor re-verified post-merge (dict extended the SHARED commonmark.rs +
publisher_markdown.rs engines — commonmark backslash escapes §2.4; publisher_
markdown isLikelySourceURLReference resourceNames branch (BaseRenderer passes
webUrl="" so it was always live). Only the known residuals remain (cycle † 6/7,
us-core summary 69/70) — the shared-engine changes are floor-neutral-or-better.

dict findings/quirks (cited in dict.rs):
- **incProfiledOut**: ALL dict kinds pass true EXCEPT dict-active (false) —
  PG:2019-2038. (The session-5 assumption that -key/-ms/-diff drop prohibited
  elements was WRONG; only dict-active drops them.)
- **hashmap_order_surviving** (NEW hashorder variant): describeTypes' leftover
  compare-type map capacity reflects the PEAK put count (Java HashMap removes
  don't shrink capacity) — affects attribute order.
- **Lazy-getter compare semantics** (dict-key/diff): Java compare.getBinding()/
  getCommentElement() never null when the compare ELEMENT exists — an absent
  field compares as empty-but-present, selecting compareMarkdown's not-equal
  branch WITHOUT fixFontSizes + renderBinding's removed "For codes, see" conf.
- **Base-element rewriting** (SDR getElementById:4065 updateURLs): markdown
  relative-URL rewrite + core canonical |version pinning on the BASE element
  drives areEqual/DarkGray + versioned-strikethrough bytes.
- **additional-binding purpose rows are NOT rendered in dict** (only max/min
  ValueSet: 73 Max / 2 Min corpus-wide, zero additional-binding) — collection
  skips the extension, golden-proven.
- **generateSlicing Java bug preserved** (SDR:4810): builds a `<ul>` but appends
  `<li>`s to the PARENT → `<ul></ul><li>…`.
- dict us-core golden count is 76 but 6 are Questionnaire-* resources (not
  SDs) — the SD denominator is 70.

dict residuals (8 fragments, cited in-code):
1. us-core-questionnaireresponse (all 5 kinds): comment `"-"` — commonmark-java
   evidently yields noString for the bare dash (we render the empty
   `<ul><li></li></ul>`); not verifiable from static source.
2. us-core-device / us-core-servicerequest (dict-key): base-element links
   `device-mappings.html`/`event.html` corePath-prefixed via a live-publisher
   runtime spec-page set that is provably neither BASE_FILENAMES nor
   getResourceNames() — needs the live context's name sets.

## F5 handoff assessment — what the page pass needs from the fragment layer

**Ownership note:** Group 4 (VS cld/expansion, CS content, TxCacheSource) was
reassigned mid-session to a separate worker (worktree `sushi-rs-snapshot-txfrag`,
branch `agent-txfrag-ae811bd`). The TxCacheSource seam doc will land with that
branch; the requirement (a storage-agnostic trait over the build txcache that the
editor's OPFS cache can back) was handed over in full.

**Group-4 results pointer (NOT merged here — owner's call):** a completed
implementation exists on branch `worktree-agent-ae811bd549ee68d98` (HEAD
`f3131c0c`, 4 commits atop 0fc79919): CS content **19/19 GREEN**, VS cld
**46/47** (1 cited cross-fragment-anchor residual, same unstable-oracle class
as HTG-uuid), VS expansion **35/35 rendered GREEN** + 9 loud-gapped multi-
include cache-miss cases. TxCacheSource seam: `src/txcache.rs` (trait, owned-
data signatures, OPFS-implementable) + `src/fstxcache.rs` (the only std::fs
implementor; parses .cache request/response blocks + cs/vs-externals.json +
internal-expansion synthesis). Additive `IgContext::resolve_cs_external`;
`resolve()` untouched. Key cited quirks in that branch: composer split
(content/cld=HTML-compact vs expansion=XML `<p/>`+literal NBSP);
`Utilities.nmtokenize` (other chars → `.{decimal-codepoint}`); TWO different
processRelativeUrls (DataRenderer.java:83 unconditional dir-prefix for
expansion definitions vs ProfileUtilities' BASE_FILENAMES-gated one); cld
filter `{prop}  {op}` double-space (vsr:1517); version-note emoji set (rr:1597).

### The entry-point shape: promote corpus.rs's dispatcher into the library

Everything F5 needs already EXISTS in `bin/corpus.rs` but as harness code. The
seam is a two-step promotion into `render_sd`:

```rust
pub struct FragmentEngine {
    ctx: IgContext,          // build_ctx: output/ + packages + txcache
    run_uuid: String,        // quirk #1 (per-run HTG uuid; editor: mint one per build)
    active_tables: bool,     // per-IG template param (PublisherIGLoader:443)
}
impl FragmentEngine {
    /// `ref_` = "StructureDefinition-us-core-patient" ("" for IG singletons);
    /// `kind` = the fragment suffix ("snapshot", "dict", "uses", "canonical-index"…).
    /// Returns the FULL fragment file body (wrap_raw applied) or a typed error.
    pub fn render_fragment(&self, ref_: &str, kind: &str) -> Result<String, FragError>;
}
pub enum FragError {
    UnknownKind(String),          // kind not in the registry at all
    Gap { kind: String, msg: String }, // documented loud gap (catch_unwind boundary)
    NoSuchResource(String),
}
```

- The kind registry = the two `match` arms in corpus.rs today (`render()` for
  per-resource kinds @75, `render_singleton()` @405). Everything else the
  dispatcher derives internally: `def_file` = `{ref_}-definitions.html`,
  `core_path` from the SD's fhirVersion (corpus.rs:47), element-list selection
  per kind. F5 should NOT need to know any of that.
- The engine's per-IG constructor consumes exactly corpus.rs's `build_ctx` +
  `harvest_uuid` + `ig_active_tables` triple. In the editor, run_uuid is minted
  (uuid v4 lowercase) once per build — parity-testing harvests it from goldens.
- `catch_unwind` moves INSIDE `render_fragment` so loud gaps surface as
  `FragError::Gap` values, not process panics (the corpus harness already
  proves this boundary works per-SD).

### First-include-miss integration points (the page pass)

The publisher pre-generates every fragment file, then Jekyll `{% include %}`s
them into pages. For the editor's lazy model:
1. **Include resolution in render_liquid**: on `{% include {name}.xhtml %}` miss
   → parse `{name}` into (ref_, kind) — the split is LAST-suffix-wins against
   the kind registry (ids contain hyphens; kinds are a closed set, so match the
   longest registered kind suffix) → `engine.render_fragment(ref_, kind)` →
   cache the result under the include name.
2. **Cache invalidation** keys on the resource content hash + the IG-level
   inputs (the whole-IG scan kinds — uses/sd-xref/maps/aggregates — depend on
   ALL resources, so their key is the IG manifest hash, not the single resource).
   This split (per-resource vs whole-IG kinds) is already explicit in the
   registry: per-resource kinds take `ref_`, whole-IG kinds consult
   `ctx.own_resources()`.
3. **Unknown/gapped kinds**: pages that include a not-yet-ported fragment get
   the FragError surfaced as a visible placeholder (NOT silent empty) — same
   loud-gap discipline at the page level.

### Remaining fragment inventory for the registry (post-session-7 truth)

- GREEN per-resource: the F3 15 table kinds + grid/span/spanall + 20+ F4 leaves
  (inv*, tx*, summary*, pseudo-*, sd-use-context, uses, sd-xref, maps,
  contained-index, history) + dict family (pending fork merge).
- GREEN singletons: 10 aggregate kinds (see scoreboard above).
- DOCUMENTED-DEFERRED: dependency-table*, ip-statements, *-ref(-all)-list
  (unstable oracle), instance `html` (F4b narrative), VS/CS group (txfrag
  worker), and the newly-enumerated phase-2 SD leaves: adl/adl-all,
  class-table, crumbs, ctxts, eview/-all, experimental-warning, header,
  json-schema, maturity, obligations/-all (the DISTINCT oo/ooa wrapper),
  other-versions, sd-changes, search-params, shex, status, summary-table,
  typename, validate, validation, and the by-key/by-mustsupport × bindings/
  obligations combo tables (engine components all exist; wiring is bounded).

## Remaining

Prior cycles: grid→IgContext migration, by-mustsupport/-all, by-key/-all
(session 2); grid + diff/-all GREEN (session 3, commonmark + pointer recon).

- **No SD table kinds remain.** All snapshot/diff/grid/by-*/bindings/obligations/
  span kinds are byte-parity corpus-wide across us-core/plan-net/cycle.
- **Simplification DONE (F3 close)**: the genTypes dedup landed. grid.rs's
  duplicate `gen_types`/`gen_target_link` AND table.rs's `gen_types_erased`/
  `gen_types_inner_for_ext` collapsed into `gentypes::TypesHost` (trait default
  methods, generic over the element lifetime `'e`; host supplies ctx/core_path/
  sd_root/gap/pointer/must_support_mode). grid = the non-dim/non-pointer/non-MS
  specialization; the ext-value cell calls the trait directly (now honors
  SDR:1402's ambient mustSupport). ~510 dup lines → ~331 shared. Gate: all 19
  kind×IG combos byte-identical. (Ledger updated.)
- Residual gap markers in table.rs (each fires loudly): choice groups
  (readChoices/processConstraint), aggregation modes, standards-status flag,
  cross-structure contained targets, complex merged-pattern partner rows,
  usage cells in additional bindings, narrative language/source-control exts.
- Diff residual risk (documented, zero corpus hits): a diff RESTATING a
  property byte-equal to base would dim in the publisher (snapshot-gen EQUALS)
  but render bright here; fix would be a render-time base-profile compare.

## Merge: VS/CS terminology fragments (peer worktree branch f3131c0c)

Merged by coordinator; both corpus.rs harnesses kept (aggregate singleton +
vs/cs iterators). Independently re-verified post-merge from the main
checkout: **cs-content 19/19 GREEN** (1+14+4); **cld 46/47** — the 1 residual
is us-core-documentreference-category, a cross-fragment anchor-ordering
divergence (first-divergence @137/503); **vs-expansion 35/35 rendered GREEN
+ 9 loud gaps** (plan-net 2, us-core 7): multi-include cache-assembly cases
where the golden expansion stitches multiple cached $expand results — gap
markers fire loudly, deferred. New seam: `txcache.rs::TxCacheSource` trait +
`fstxcache.rs::FsTxCache` over the F0 builds' input-cache/txcache — designed
for the editor's OPFS tx cache to back the same trait (editor spec §6).
Floor spots re-confirmed post-merge: snapshot us-core 70/70, dict cycle 7/7,
uses plan-net 22/22.

# Session 8 (2026-07-03) — F4 CLOSEOUT (the STOPPED list)

Coordinator + 2 forks. Every commit re-ran the FULL floor (F3 15 kinds + all F4
greens incl. cs-content/cld/vs-expansion); only the known cited residuals persist
(cycle † 6/7; us-core dict 69/70 & dict-key 67/70; summary 69/70; cld 20/21;
vs-expansion gaps). **Zero regressions** from any session-8 change.

## Target 2 — valueset-list plan-net residual: GREEN (context.rs, careful)
Root cause CITED: `CanonicalResourceManager.see()` exit-condition 1
(CRM:349-353) drops any resource from an `hl7.terminology*` package whose url is
in `INVALID_TERMINOLOGY_URLS` (CRM:24-28 = {snomed sct, dicom DCM, nucc
provider-taxonomy}) — "erroneous code systems found in THO". Active THO 7.2.0
omits nucc; the transitively-loaded THO 5.5.0/6.1.0 copies carry it but are
rejected → `fetchCodeSystem(nucc)` = null → `describeSource` → "Other". FIX:
`load_package` drops those urls from the file index when the package id starts
`hl7.terminology` (faithful see() port). Core-package DCM/SCT copies survive
(match publisher: THO copies dropped, core kept). **valueset-list now 1/1 all 3
IGs.** Floor-neutral (cld/cs-content/codesystem-list/tx/snapshot-bindings all
re-verified byte-identical).

## Target 3 — 4 deferred grid branches (fork B): 3 GREEN, 1 reclassified STOP
| kind | cycle | plan-net | us-core |
|---|---|---|---|
| summary-extensions | 1/1 ✅ | 1/1 ✅ **grid** | 1/1 ✅ **grid** |
| summary-observations | 1/1 ✅ **grid** | 1/1 ✅ | 1/1 ✅ **grid** |
| deprecated-list | 1/1 ✅ | 1/1 ✅ | 1/1 ✅ **grid** |
| expansion-params | 1/1 ✅ | 1/1 ✅ | **STOP** (cited) |

Findings (cited in aggregates.rs): summary-extensions value-type cells link
`fetchTypeDefinition(name).getWebPath()`; definitions render escapeXml with NO
markdown (cvr:470); nested component `defn`=slice-header's definition (els[i-1],
cvr:403), `code`=fixed url (cvr:423). summary-observations = dynamic column set
(cvr:494-517), renderCodingCell links via `fetchCodeSystem(system).getWebPath()#`
+ validateCode display (through the existing `TxCacheSource::lookup_display`);
QUIRK (faithful): processObservationComponent appends component code into
`obs.method` (cvr:346/363, dead copy-paste bug); STOP sub-branch = category-
collapse (cvr:518, zero corpus hits, loud panic). deprecated-list composer =
`XhtmlComposer(true,true)`=XML-pretty (new `Config::xml_pretty()`); empty Change
cell = `td().tx("")` → `<td></td>` (not self-closed); reason/description via the
ported publisher_markdown. **expansion-params us-core reclassified STOP**:
`getExpansionParameters()` (pg:1688) is a runtime tx-state artifact — the ~80
`default-canonical-version` rows are injected in build-execution order as the tx
layer pins each resolved canonical; NOT in any input/input-cache file (verified;
manifest holds only `system-version`). Feeding the full list = replaying the
golden, not rendering it. Deferred to the terminology phase. Loud panic with the
precise message. (The empty branches remain GREEN — not regressed.)

## Target 1 — dependency-table family + ip-statements (fork A)
| kind | cycle | plan-net | us-core |
|---|---|---|---|
| dependency-table-nontech | 1/1 ✅ | 1/1 ✅ | 1/1 ✅ |
| ip-statements | 1/1 ✅ | 1/1 ✅ | 0/1 (1-cell residual) |
| dependency-table | GAP | GAP | GAP |
| dependency-table-short | GAP | GAP | GAP |

New `ipstmt.rs` (~290) + `deptable.rs` (~330) + `md_fragment_strip_paras_autolinks`
in publisher_markdown. Findings (cited): the IG resource is itself a FetchedResource
(getPath=index.html); the denominator is `definition.resource` (not every output/
*.json — excludes expansions.json); R5-in-R4 `Basic` projections need
`extension-{Type}.{field}` markers; `fetchCodeSystem` consults tx-cache externals
(nucc/AMA copyright); nontech title-normalization collapses the us.core facades
into one rowspan; `PackageHacker.fixPackageUrl` applied on-load; the loaded-package
set is a per-IG build fact (harvested from the tech golden's simplifier links).
- **ip-statements us-core residual (byte 31859) = ONE cell**: `v3-ucum`
  (`http://unitsofmeasure.org`) resolves to THO 6.5.0 in ours vs
  `hl7.fhir.uv.xver-r5.r4` 0.1.0 in the golden (both v3.0.1; webPath only). SAME
  cross-package precedence family as the ref-list residuals below (see Target 4).
- **dependency-table/-short GAP (cited)**: `DeprecationRenderer.render()` (depr:298)
  uses the HTG `inlineGraphics=true` path; the `render_tables` port is
  `inlineGraphics=false` only. Needs a captured PNG tree-line oracle (Java ImageIO
  `genImage` is not byte-reproducible in Rust) + row-tree + version-comment column.
  Loud panic (deptable.rs:311). dependency-table-nontech (no inline graphics) is GREEN.

## Target 4 — *-ref(-all)-list (coordinator): ported + rigorously classified
New `xreflist.rs` (~360) — the whole-IG reference SCAN (buildUsed{VS,CS}List,
CVR:1239/1568); the table reuses `aggregates::{render_vs_list,render_cs_list}`
via extracted `{vs,cs}_row_cells` + a `used` References column. All 12 kind×IG
render (no crashes). Scan bugs found & fixed during classification:
- `findValueSets(vs)` (CVR:1291) `list.add(vs)` adds the VS as a row with NO
  self-reference; only its IMPORTS record a ref whose SOURCE is the VS (CVR:1297),
  not the outer SD. (Fixed a spurious self-reference.)
- `findCodeSystems(vs)` (CVR:1650) `resolveCS(system, **vs**)`: a CS reference's
  SOURCE is the resolved VALUESET, not the SD that bound it. (Fixed.)
- `ed.getBinding().getAdditional()` reads the R4-output `additional-binding`
  tools-extension's nested `valueSet` (valueCanonical), not just R5 `binding.additional`
  (us-core DocumentReference.type → clinical-note-type). (Fixed — cleared the
  Category-A empty-ref cells.)
- `resolveCS`/`fetchResource(CodeSystem)` also finds tx-cache externals
  (cs-externals.json) that CanonicalResourceManager.get() misses (nucc/formatcode,
  dropped from THO by INVALID_TERMINOLOGY_URLS yet present as tx externals);
  webPath = `{server}/ValueSet/{cs.id}`. (Fixed — cleared the missing-row cells.)
  Distinct from `fetchCodeSystem` (describeSource, target 2) which does NOT — hence
  nucc is "Other" in valueset-list yet a real row here.

**Classification (corpus `classify-reflist <ig>` — the in-repo PROOF):**
- **87 order-only cells** (cycle 9, plan-net 24, us-core 54): References-cell
  (title,link) multiset identical, only intra-cell order differs → unstable-oracle
  QUIRK #9 (registered above), our deterministic order a valid member. Proven by
  set-equality per cell; no duplicate pairs corpus-wide.
- **14 resolution residuals** (cycle 1, plan-net 1, us-core 12) on ~6 canonicals
  (languages, ucum, ietf3066/bcp:47, PHOccupationalDataForHealthODH, cpt,
  THO/Languages): OUR `resolve()` picks a THO copy; the golden picks
  `hl7.fhir.uv.xver-r5.r4` (or a different THO version) — a cross-package
  version/webPath PRECEDENCE tiebreak (`MetadataResourceVersionComparator` +
  `compareByLoadCount`, CRM:483, resolved by package LOAD ORDER). Same family as
  ip-statements us-core. NOT the ordering quirk; a deterministic resolution-oracle
  gap. **Deliberately NOT fixed**: the fix is a `resolve()` load-order tiebreak
  touching every resolved canonical — high floor risk for cells that are
  byte-unstable anyway (quirk #9). Cited residual; shared with Target 1.

Because of quirk #9, the ref-list kinds are byte-unstable BY CONSTRUCTION: 4/12
fragments are pure-unstable (would be green but for HashSet order); the other 8
add the resolution residual. Row sets + reference multisets are otherwise correct.

## Target 5 — vs-expansion multi-include gaps: CHARACTERIZED, deferred (not cheap)
The 9 loud-gapped vs-expansion cases (plan-net 2, us-core 7) are MULTI-INCLUDE VSs.
FINDING (cache-file structure): the publisher issues a SEPARATE single-include
`$expand` per include, each cached in its PER-SYSTEM `.cache` file (icd-10-cm.cache,
snomed.cache, …) keyed by that ONE include's compose + `hierarchical` param
(url=None); the golden expansion STITCHES them in include-order with a merged
multi-source header. `all-systems.cache` holds ONLY `v:` (validate) blocks — no
multi-include `e:` block exists. Our `FsTxCache.cache_expand` fingerprints the WHOLE
multi-include compose → never matches. Fix = per-include expand + assemble (bounded
but NON-trivial: ~1000-row tables, merged source header, count=1000 truncation box).
Stays loud-gapped with this precise spec.

## F4-CLOSE scoreboard delta (session 8)
| kind | cycle | plan-net | us-core |
|---|---|---|---|
| valueset-list | 1/1 ✅ | 1/1 ✅ (was ❌) | 1/1 ✅ |
| summary-extensions | 1/1 ✅ | 1/1 ✅ | 1/1 ✅ |
| summary-observations | 1/1 ✅ | 1/1 ✅ | 1/1 ✅ |
| deprecated-list | 1/1 ✅ | 1/1 ✅ | 1/1 ✅ |
| expansion-params | 1/1 ✅ | 1/1 ✅ | STOP (tx-state) |
| dependency-table-nontech | 1/1 ✅ | 1/1 ✅ | 1/1 ✅ |
| ip-statements | 1/1 ✅ | 1/1 ✅ | 0/1 (xver-precedence) |
| dependency-table / -short | GAP (inline-graphics PNG oracle) | | |
| valueset-ref-list / -all | unstable-oracle (#9) + resolution residual | | |
| codesystem-ref-list / -all | unstable-oracle (#9) + resolution residual | | |

## F5 handoff delta (session 8)
The fragment-kind registry now additionally covers: valueset-list (all 3 green),
the 3 grid-branch aggregates, dependency-table-nontech, ip-statements (−1 cell),
and the 4 *-ref(-all)-list kinds (correct-modulo-quirk-#9). New modules:
`xreflist`, `deptable`, `ipstmt`. New context seam: INVALID_TERMINOLOGY_URLS
(load-time THO filter) + `resolve_cs_external` reused for ref-list CS rows.
`corpus classify-reflist <ig>` is the reusable unstable-oracle proof harness.
STILL DOCUMENTED-DEFERRED for F5: dependency-table/-short (inline-graphics PNG
oracle), expansion-params us-core (tx op-log), vs-expansion multi-include
(per-include cache assembly), the xver-r5.r4-vs-THO resolve() load-order tiebreak
(affects ip-statements us-core + 14 ref-list resolution cells), instance `html`
narrative (F4b), and the phase-2 SD leaves enumerated in session 7.

# F5 — the page pass (convergence): FragmentEngine + Jekyll-equivalent page render

Branch `snapshot-gen`. New crate `crates/render_page` (own `[workspace]`, path
deps on render_liquid + render_md + render_sd, like the siblings). Goal: whole
PAGES at Publisher parity.

## LOAD-BEARING CORPUS FINDINGS (established before any code — cited from the F0 builds)

These reshape what the F5 gate actually verifies. All verified against the
committed F0 builds (`../sushi-rs-snapshot-f0-builds/<ig>/`) and the cycle build
(`periodicity-impl/cycle/`).

1. **The Jekyll source tree is the build's `temp/pages/` dir.** It carries
   `_config.yml` (kramdown settings only), `_data/*.json` (= `site.data.*`),
   `_includes/*` (17,569 for plan-net — EVERY fragment PRE-generated by the
   publisher before Jekyll), and the per-page `.html`/`.md` inputs under
   `temp/pages/en/` (the localized copies Jekyll actually renders).

2. **There is NO Jekyll `layout:` step.** The rich page inputs (`temp/pages/en/
   *.html`) have EMPTY front-matter (`---\n---`, no `layout:` key). The layout is
   applied by INCLUDING a template fragment: the page body is
   `{% include template-page.html lang='en' %}` (or the profile pages inline the
   layout-profile chrome directly, with a `<!-- layouts\layout-profile -->`
   trace-comment the publisher's pre-Jekyll transclusion left behind). So the
   whole page pass is: **strip front-matter → Liquid render**. `template-page.html`
   → `fragment-pagebegin.html` (the `<?xml?><!DOCTYPE><html><head>…` chrome) →
   content + `{% include {{path}}.xml %}` (the payload dump) + fragment includes →
   `fragment-pageend.html`. No layout inheritance engine needed — render_liquid's
   existing `{% include %}` mechanism covers it end to end.

3. **The golden `output/en/*.html` is the DIRECT Liquid output — NO post-Jekyll
   tidy/reindent pass.** The `<?xml version="1.0"?>` prolog and the leading blank
   lines come literally from `fragment-pagebegin.html` (its `{% assign %}`/`{% if %}`
   preamble emits blank lines, byte-matching the golden's blanks). Verified on
   `output/en/toc.html`. So page byte-parity is achievable with the Liquid engine
   + FragmentEngine + `markdownify`; no HTML re-serializer at the page level.

4. **ROUGE DECISION: rouge is NOT active for this template — no port needed.**
   The template `_config.yml` has ONLY `kramdown:` settings; NO `highlighter:`/
   `syntax_highlighter: rouge` key. Evidence over the real corpus
   (`output/en/*.html`): `class="highlight"` appears **0** times and rouge token
   spans (`<span class="nt">` etc.) appear **0** times. The only code blocks are
   the `.xml.html`/`.json.html`/`.ttl.html` PAYLOAD-DUMP pages carrying kramdown's
   OWN `<pre><code class="language-{xml,json,turtle}">` form (139 each) — NOT
   rouge markup (no inner token spans). render_md already emits exactly that
   kramdown form (F1b documented boundary). **Page byte-parity does NOT require a
   rouge lexer for the stock hl7.fhir/base template.** (If a future custom
   template sets `syntax_highlighter: rouge`, that is a separate, bounded port —
   assessed and deferred with this evidence.)

5. **PAGE-GOLDEN PROVENANCE CORRECTION (important).** The committed
   `render-goldens/plan-net/pages/` set (nominally "1,173 page goldens") is the
   publisher's `output/` TOP-LEVEL files: **1,172 byte-identical 295-byte
   language-redirect stubs** (only the `<title>`… no, verified: fully identical —
   they differ in NOTHING; all share one uuid) **+ 1 `searchform.html` (5,347 B,
   a static template copy still carrying `{{site.data.info.assets}}` UNRENDERED
   because the publisher copies top-level files to `output/` WITHOUT Jekyll)**.
   The harvest script's `find output -maxdepth 1` captured only these; the RICH
   content pages live under `output/en/` (1,173 of them, e.g. `en/artifacts.html`
   = 91 KB with full chrome + fragment includes) and were NOT committed. So the
   committed plan-net "page gate" is trivial (one redirect stub + one static
   searchform); the REAL page-parity gate is `output/en/` (F0 builds) and cycle's
   `temp/pages/en/` — exactly the staged gate the F5 brief names ("cycle's
   temp/pages jekyll output").

## site.data inventory (the DataProvider surface)
`_data/*.json` in `temp/pages/` ARE `site.data`. Keys the page/fragment includes
read (measured over template-page.html + fragment-*.html + the en page inputs):
`site.data.fhir` (path/canonical/igVer/version/ver.*/basespec/tx-server — from
`fhir.json`), `site.data.info` (assets/releaselabel/… from `info.json`),
`site.data.pages[slug]` (title/titlelang/breadcrumb/next/prev/label — `pages.json`),
`site.data.resources[Type/id]` (path/url/version/name/experimental/history —
`resources.json`), `site.data.structuredefinitions[id]` (abstract/kind/type/base/
path — `structuredefinitions.json`), `site.data.stringsBase[lang][KEY]` (i18n —
`stringsBase.json`), `site.data.languages` (defLang/langs — `languages.json`),
plus artifacts/canonicals/questionnaires/related/structuredefinitions/
stringsArtifacts. For the gate these are read straight from the build's own
`_data/*.json` (faithful oracle input, same pattern as reading `output/` SDs for
fragments). The site_db → _data mapping (reproducing these from site_db rows +
IgContext) is documented separately for the editor path; the gate uses the
publisher's own _data as the site.data oracle.

## F5 BUILD — FragmentEngine + page pass AS-BUILT (session 9)

New crate `crates/render_page` (own `[workspace]`; path deps render_liquid +
render_md + render_sd + render_xhtml). New module `crates/render_sd/src/engine.rs`
(the FragmentEngine promotion). Substrate changes (all neutrality-proven, below).

### FragmentEngine API (as-built) — `render_sd::engine`
```rust
pub struct FragmentEngine { /* ctx + run_uuid + active_tables + IgFacts */ }
impl FragmentEngine {
  pub fn new(ctx: IgContext, run_uuid: String, active_tables: bool, facts: IgFacts) -> Self;
  pub fn render_fragment(&self, ref_: &str, kind: &str) -> Result<String, FragError>;
  pub fn split_include(name: &str) -> Option<(String, String)>; // longest-kind-suffix
  pub fn is_whole_ig_kind(kind: &str) -> bool;                   // cache-key split
}
pub enum FragError { UnknownKind(String), NoSuchResource(String), Gap{kind,refname,msg} }
```
- `render_fragment` promotes corpus.rs's `render()` (per-resource, ~45 kinds) +
  `render_singleton()` (23 IG-level kinds) into the library, VERBATIM dispatch
  (same TableConfig/leaf/tx/dict/aggregates calls). `catch_unwind` moved INSIDE
  so a documented loud gap surfaces as `FragError::Gap`, not a process panic.
- `split_include`: `StructureDefinition-us-core-patient-snapshot` → (`…patient`,
  `snapshot`) by matching the LONGEST registered kind suffix (`PER_RESOURCE_KINDS`
  ordered longest-first; `SINGLETON_KINDS` = whole-stem). `IgContext::load_own_file`
  (new, additive) loads the single resource JSON by `{Type}-{id}` ref.
- Cache-key split exposed via `is_whole_ig_kind`: per-resource kinds key on the
  resource hash; whole-IG kinds (uses/sd-xref/maps + the 23 singletons) key on
  the IG manifest (they consult `ctx.own_resources()`).
- `IgFacts` carries the singleton build-facts corpus.rs harvests (oids.ini,
  loaded_set, ig_json, new_format, etc.) — the editor computes them; the harness
  harvests them (same oracle-input pattern as run_uuid).

### The page pass (`render_page::render_page`)
`strip_front_matter → render_liquid::render_with(markdownify=render_md rouge_on)`.
Jekyll semantics ported: (a) files WITHOUT a leading `---` line are STATIC —
copied verbatim, not Liquid-rendered (`has_front_matter` gate; verified on
searchform.html whose golden keeps `{{title}}` literal); (b) `page.path` global
(`en/<name>` multi-lang / `<name>` flat); (c) includes resolve via `PageProvider`
= `_includes/<name>` first, then FragmentEngine on MISS (first-include-miss);
`--engine-first` flips the order to PROVE the engine materializes byte-identical
fragments (16 SD fragments on us-core-patient, all byte-exact modulo the page's
own rouge markdown). site.data = `_data/*.{json,yml}` (SiteData; YAML added for
us-core's `ig.yml`). Post-Jekyll ReleaseHeader substitution modeled as an
optional post-pass (us-core's output/ is post-substitution; plan-net's is the
pre-substitution Jekyll stage — harness auto-detects via the placeholder).

### PER-IG PAGE-PARITY TABLE (content pages; payload-dumps classified out, below)
| IG | byte-identical | notes |
|---|---|---|
| **plan-net** | **678 / 678 (100%)** | all content pages GREEN |
| **us-core** | **1258 / 1334 (94.3%)** | 76 fails: CONF-links 50, rouge-lexer 11, misc 15 (classified below) |
| **cycle** | **n/a (no golden)** | cycle's committed build never ran Jekyll to output (missing `_includes/sample-viewer-links.md` aborts the publisher's jekyll step); inputs+data+includes all present and RENDER, but there is no rendered oracle to byte-compare. Documented gap; not a renderer defect. |

(Committed `render-goldens/plan-net/pages/` = the 1,172 identical redirect stubs
+ searchform — the trivial `output/` top-level set; the REAL gate is `output/en/`,
finding #5 above. plan-net 678 = the `en/` content pages with a committed golden.)

### ROUGE DECISION (evidence-based, resolved)
`_config.yml` has NO `highlighter:` key, but Jekyll 4.x defaults
`syntax_highlighter: rouge`, and Jekyll's markdownify (NOT bare kramdown) applies
rouge wrappers. Split by cost (live-Jekyll-oracle-verified):
- **Inline code spans + plaintext fenced blocks (DETERMINISTIC, no lexer)** —
  `<code class="language-plaintext highlighter-rouge">` / the plaintext div-wrap.
  PORTED behind `render_md::Options::rouge_wrappers` (default FALSE = F1b gate
  untouched; page pass sets TRUE). This closed plan-net (677→678) and ~114 us-core
  pages. Oracle: `ruby -e 'require "jekyll"; …conv.convert("`x`")'`.
- **Real-lexer fenced blocks (json/js/http)** — need a rouge TOKENIZER (token
  spans). Corpus set: ~10 blocks over 11 us-core content pages (json 6, js 3,
  http 1). DEFERRED with this sizing (a bounded 3-lexer port; STOP-and-report).

### CLASSIFIED / DEFERRED (the 76 us-core residuals + cycle)
1. **CONF-NNNN conformance-link injection (50 pages)**: the publisher injects
   `[CONF-NNNN]: requirements.html#CONF-NNNN "CONF-NNNN"` link-reference
   definitions into markdownify (555 CONF anchors in requirements.html). Absent
   from ALL build inputs — derived from the US-Core Requirements/FSH conformance
   registry. US-Core-specific publisher-markdown feature. Deferred (needs the
   conformance-anchor registry; bounded — a global link-def set appended pre-kramdown).
2. **Rouge real-lexer fenced blocks (11)**: above.
3. **Misc (15)**: relative-link auto-id anchors (`us-core-roadmap.html` →
   `…#us-core-roadmap`, publisher processRelativeUrls/kramdown auto-id — 3 pages);
   UTF-8 BOM preservation on one page; toc-entry link-stripping (kramdown drops
   `<a>` from toc text — 1); CSV `_data` tables (uscdi-table.csv not loaded → empty
   panel — `SiteData` loads json/yml not csv; no page reads a csv key EXCEPT these
   table renders — bounded csv-to-array-of-hashes add).
4. **Payload-dump pages (`.json/.xml/.ttl.html`)**: CLASSIFIED OUT (harness
   default; `--dumps` to include). Same class the harvest excludes from fragments
   (serialized-resource echoes). They route through a post-Jekyll XHTML
   normalization (one collapsed leading newline) the analysis pages don't;
   render_liquid was PROVEN byte-identical to Ruby Liquid on the pre-normalization
   output (`ruby -e 'require "liquid"…'`), so this is a post-pass, not an engine bug.
5. **cycle**: no rendered oracle (incomplete build), above.

### SUBSTRATE CHANGES — neutrality proofs
- **render_liquid** `Options.markdownify: Option<fn(&str)->String>` (default None =
  the F1c `MD…/MD` marker stub unchanged) + a real `date` filter (Time.parse +
  strftime for the corpus's genDate/ISO shapes + `%Y%m%d%H%M%S%z%b%y`). The F1c
  differential corpus uses NEITHER (`markdownify` marker; zero `date:` calls) →
  gate-neutral. `cargo test`: 25/25 green.
- **render_md** `Options.rouge_wrappers` (default false) + the `{:toc}`-in-div
  indentation fix + last-child trailing-blank fix. F1b differential: 459/459 →
  459/459 (0 regressions, fork-verified against the kramdown oracle). `cargo test`
  11/11.
- **render_sd** `engine.rs` (new; wraps existing dispatchers) + `load_own_file`
  (additive). Fragment floor re-verified: snapshot us-core 70/70, dict/tx/uses/
  maps cycle 7/7, grid/diff cycle 6/7 (known † quirk #3), valueset-list plan-net
  1/1 — all at documented parity, zero regressions.

### Ledger integration
Coarse per plan §2c: `PageProvider` is the read-set boundary — a page's
materialized-fragment set (the include names hitting the engine, tracked in
`frag_cache` + `miss_count`) is the page node's fragment dependency set; per-
resource fragments key on the resource hash, whole-IG kinds on the IG manifest
(`is_whole_ig_kind`). Wiring these into site_db's BuildLedger nodes is the F6
editor-integration step (the ledger API lives in site_db, a different workspace);
the hash-real boundary (content-hash-keyed fragment cache) is in place here.

### Remaining (for F6 / follow-up)
CONF-link injection; rouge json/js/http lexers (~10 blocks); the misc-15 classes
(auto-id relative links, BOM, toc link-strip, csv _data); cycle oracle (re-run
the publisher's jekyll step with the FragmentEngine backing the missing includes);
full IgFacts population in the page harness (engine-first singleton aggregates —
ip-statements' "Show N more" needs the full ig_json/loaded_set the harness omits).

# F5 — session 10 (2026-07-03): page-pass convergence to 1322/678/72

The bulk of the "76 us-core residuals" collapsed to ONE root discovery: the
US-Core template drives its CONF-NNNN links, requirements/uscdi/vsacname tables,
provenance bullets and search-requirement narratives ENTIRELY from `_data/*.csv`
+ Liquid — there is NO publisher-side CONF injection (the session-9 brief's
"CONF-NNNN conformance-link registry" is the template's own
`requirements-link-list.md` reading `us_core_reqs.csv`; confirmed a publisher
source-tree search finds no Java injector). CSV `_data` support unblocked all of
it. Every fix below is gate-neutral against its differential floor.

## PER-IG PAGE-PARITY TABLE (final, this session)
| IG | byte-identical | delta | notes |
|---|---|---|---|
| **plan-net** | **678 / 678 (100%)** | — | held across every substrate change |
| **us-core** | **1322 / 1334 (99.1%)** | +64 (1258→1322) | 12 residuals classified below |
| **cycle** | **72 / 72 (100%)** | new oracle | Jekyll re-run over temp/pages (target 4) |

## Target outcomes
1. **CONF-NNNN links (target 1) — DONE via CSV `_data`.** The real mechanism is
   NOT publisher injection: `SiteData` now loads `_data/*.csv` as Jekyll does
   (`CSV.read headers:true` → array-of-hashes; RFC-4180 quoting). LOAD-BEARING:
   empty cells become **nil** (not `""`) — Liquid nil is falsy, `""` truthy — so
   `{% if row.col %}` skips them (suppresses spurious `- Including` bullets AND
   keeps the search list loose). Two render_md link-ref fixes complete it:
   LAST-definition-wins (kramdown) + a `{: #id}` IAL following a link-def stamps
   the `<a>` (the `id="CONF-NNNN"`). `page.name` (Jekyll basename) added — the
   provenance/search CSV loops key `item.Path == page.name`.
2. **Rouge real-lexer fences (target 2) — DONE (json+js).** `render_md::rouge`
   (behind `rouge_wrappers`, F1b-neutral): Rouge-4.7.0-faithful json + js
   tokenizers with the token→shortname table and the formatter's same-type-run
   coalescing. **`http` never appeared** once json/js landed (the earlier "http 1"
   count was a mis-classified js block) — target-2 sizing resolved: 2 lexers,
   not 3.
3. **misc — mostly DONE.** toc link-strip (drop nested `<a>` from toc entries),
   csv `_data` (above), BOM/auto-id/table-column all resolved or reclassified:
   the "auto-id relative links" were the last-wins link-ref bug (fixed); the CSV
   tables were the same csv-data unblock; a GFM separator-row-wider column bug
   and an invalid-tag-name escaping bug (`<patient|user|system>`) were found and
   fixed. **No 'misc' bucket left** — the 12 residuals are named classes below.
4. **cycle page oracle (target 4) — DONE, 72/72.** The publisher's jekyll aborted
   on the missing `_includes/sample-viewer-links.md` (the sitegen wrapper
   `build-sitegen-site.ts:writeSampleViewerInclude` generates it). Supplied that
   include with the wrapper's real content shape, ran the SAME Jekyll 4.4.1 over
   the existing `temp/pages` (jekyll only, isolated HOME) → a rendered oracle;
   harness gates it via `CYCLE_GOLDEN_DIR`. **cycle 72/72 byte-identical.**
5. **Full IgFacts / engine-first (target 5) — seam validated.** `--engine-first`
   on us-core-patient materializes 16 SD fragments BYTE-EXACT (the seam works).
   The only engine-first divergences on content pages are 4 CodeSystem `-html`
   (instance-narrative) fragments — the DOCUMENTED-DEFERRED F4b kind leaking
   through, wrapped in `{% raw %}` the publisher's `-html` doesn't. The
   xver-vs-THO tiebreak did NOT surface: no `en/` content page includes
   ip-statements, so the tiebreak stays a fragment-layer follow-up (untouched;
   the full-fragment-floor regression net was not needed).
6. **Ledger (target 6) — design note.** The `PageProvider` read-set boundary is
   in place: `frag_cache` (materialized-on-miss include store) + `miss_count`;
   `FragmentEngine::is_whole_ig_kind` splits the cache key (per-resource →
   resource hash; whole-IG/singleton → IG manifest hash). Wiring these into
   site_db's BuildLedger nodes is F6 (that API lives in a different workspace);
   the hash-real, fs-free (wasm-safe) boundary is the deliverable here.

## SUBSTRATE CHANGES — neutrality proofs (all green)
- **render_liquid**: parenthesized boolean grouping in `{% if %}` (`a and (b or
  c)` = `a && (b||c)`, Ruby-Liquid-4.x). F1c fixtures 34/34; +1 semantics test.
  This alone recovered ~38 SD pages (the `{gt|lt|ge|le}` search-URL condition).
- **render_md** (F1b differential 459/459 UNCHANGED, fixtures 11→14): last-wins +
  IAL link-refs; span-level IAL on emphasis/code; rouge json/js (opt-in); js
  string-escape rule; GFM separator-row column rule; toc `<a>` strip; invalid
  inline-tag-name escaping.
- **render_page**: CSV `_data` (nil empty cells); `page.name` global; cycle
  `CYCLE_GOLDEN_DIR`.

## 12 us-core residuals (named, none 'misc')
- **nested `markdown="1"` in a raw HTML block (6)**: basic-provenance,
  clinical-notes, medication-list, screening-and-assessments, documentreference,
  practitionerrole. kramdown descends into a `markdown="1"` div nested inside a
  non-md `<div class="collapse">`; render_md honors `markdown="1"` only on the
  OUTER element (verified byte-exact in isolation). A bounded render_md feature
  (raw-block child re-processing) — DEFERRED as F1b-risky vs 6 pages.
- **post-Jekyll XHTML re-serialization (2)**: changes-between-versions,
  relationship-with-other-igs — a LATER pipeline stage reorders `<meta>` attrs
  and adds a BOM (same class as the payload-dump normalization, finding #4).
  Not a Liquid/kramdown defect.
- **rouge code-block-in-list-item indent (2)**: general-guidance,
  general-requirements — a rouge fenced block nested in a loose `<li>` closes
  with `    </div>` vs our `</div>` (list-continuation indent of the rouge
  wrapper's final line).
- **toc heading-in-markdown="1"-block not captured (1)**: scopes — a heading
  inside a `markdown="1"` div is missing from the generated toc (same nested-md
  root as the 6 above).
- **`_id` search token/link (1)**: specimen — a `_id` search-parameter cell
  links differently.

FINAL: page-pass at 1322/678/72; all substrate changes differential-neutral;
cycle now has a rendered oracle; the FragmentEngine seam is byte-proven for the
ported kinds. Remaining is one render_md feature (nested markdown-in-raw-block,
covers 7 of 12) + 2 pipeline-stage artifacts + 3 narrow edges.

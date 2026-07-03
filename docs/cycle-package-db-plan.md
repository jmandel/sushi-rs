# Cycle IG: Rust Pipeline as the package.db Producer — Gap Analysis & Plan

> Status: ANALYSIS (2026-07-02). Source: exhaustive read of
> `jmandel/cycle@aa10e71` `site-gen/` (inventory by Explore agent; file:line
> citations live in that repo). Implementation is post-rework roadmap work
> (after task #12 perf/clarity pass).

## 1. The finding: the contract is one SQLite file

The cycle TS site generator (`site-gen/ingest.ts` + `build.tsx`) reads **only**
`output/package.db` (plus repo-authored inputs it ingests itself: pagecontent
markdown, sushi-config menu, images, includes). It reads **no other publisher
outputs** — no expansions files, no rendered narrative, no fragments files, no
qa.json, no .index.json. "Publisher fragments" are computed live from
`Resources.Json` at render time. The wrapper script copies `output/qa*` into
the site cosmetically, but that's not a data input.

Even better: the cycle repo contains
- **`site-gen/publisher/contract.ts`** — the hard, ingest-asserted table/column
  contract;
- **`site-gen/publisher/{schema,writer,rows}.ts`** — an experimental TS
  reimplementation of the Java DB producer = a readable spec for every column's
  derivation;
- **`site-gen/fixtures/package.db`** — a committed golden of the Java
  Publisher's actual DB for this IG, plus `publisher/compare.ts` to diff
  producers against it.

That is an oracle + golden + comparator, ready-made for our methodology.

## 2. What the DB must contain vs what our pipeline has

| DB content | Origin | Rust pipeline status |
|---|---|---|
| `Resources` rows: Type/Id/Url/Version/Status/Name/Title/Description/kind/sdType/derivation/content/supplements/base | SUSHI outputs | ✅ **Have** (byte-parity resources; scalar columns are simple projections; `base` needs the version-pinning rule from `rows.ts:199-210`) |
| `Resources.Json` — FULL resource, and for SDs **snapshot-complete** (`snapshot.element` is load-bearing for ProfilePage) | snapshot generation | ✅ **Have post-rework** (walk engine; decision-parity with the same Java the Publisher runs) |
| `Resources.Json` for the IG (definition.page/resource, dependsOn, global) | SUSHI IG export | ✅ **Have** (byte-parity IG resource) |
| `Concepts` (flattened CodeSystem concept[] w/ ParentKey hierarchy) | SUSHI CS output | ✅ Trivial projection (`rows.ts:344-367` is the spec) |
| `ValueSet_Codes` (expansion: System/Code/Display per ValueSetUri) | **terminology $expand** | ❌ **The one real gap.** Not in our pipeline. Options in §3 |
| `Metadata` (~10 consumed keys: path/canonical/packageId/igId/igName/igVer/version/releaseLabel/genDate/genDay) | publisher bookkeeping | ✅ Trivial (config + clock) |
| `Resources.standardStatus` | derived bookkeeping | ✅ Port `rows.ts:167-197` |
| `ValueSetList*` / `CodeSystemList*` tables | publisher indexed lists | ⚠️ Contract requires tables to EXIST; renderer never reads them. Create empty for v1; populate later only for `compare.ts` parity or future `{% sql %}` pages |
| Narrative/fragments/includes | render-time synthesis | ✅ Nothing to produce |

**Bottom line:** with the snapshot rework done, our pipeline already produces
the hard 90%. The genuinely missing piece is **ValueSet expansion**; the rest
is a SQLite-assembly step (projections + bookkeeping).

## 2b. Target boundary (decided 2026-07-02): Rust emits **site.db**; TS only renders

`ingest.ts` today copies `package.db` to `site.db` and ADDS four tables
(`Pages`, `Menu`, `SiteConfig`, `Assets`); `build.tsx` renders only from
`site.db`. Decision: **the Rust pipeline absorbs the ingest step** and emits
`site.db` as the single artifact the TS code consumes. `build.tsx`/`core/db.ts`
already read only `site.db`, so the TS change is just deleting/bypassing
ingest and asserting the site.db contract (package.db contract + the four
augmentation tables) at render start.

**PlantUML: OUT OF SCOPE.** Precondition instead: every SVG referenced by a
page must already exist under `input/images/` (pre-rendered/committed). The
producer fails loud with the list of missing images. No Java, no Maven fetch.

### Processing model (explicit stages, each with declared inputs → outputs)

```
S1 parse      input/fsh/**/*.fsh ────────────────→ tank entities        (per file)
S2 compile    tank + package cache ──────────────→ resource JSONs + IG  (rust_sushi, parity)
S3 snapshot   SD JSONs + base/type SD closure ───→ snapshot-complete SDs (walk engine)
S4 expand     VS + CS closure + tx pin ──────────→ expansion rows        (tx client, cached)
S5 rows       S2-S4 + config + build meta ───────→ Resources/Concepts/
                                                    ValueSet_Codes/Metadata rows
S6 augment    definition.page tree (from OUR IG resource)
              + input/pagecontent/<slug>.{md,xml}  → Pages rows (body verbatim;
                first `# H1` overrides title; fail loud on missing page file)
              + sushi-config.yaml RAW ────────────→ SiteConfig (verbatim yaml→json)
              + sushi-config.yaml `menu:` ────────→ Menu rows (label/href/kind/
                                                    parent/ord/depth)
              + input/images/** ─────────────────→ Assets (mime by extension)
              + include scan: {% include %} / {% lang-fragment %} refs in page
                bodies, nested text includes followed → Assets (referenced only;
                fail loud on missing; generated includes must exist BEFORE this
                stage — the wrapper's viewer-build slot is a declared S6 input)
S7 emit       all rows ──────────────────────────→ site.db (SQLite)
```

Rules: sushi-config.yaml is consumed twice (S2 compile semantics; S6 verbatim
— never normalize it). Every stage declares its input set explicitly; nothing
reads the filesystem outside its declared inputs (this is what makes §2c
possible and keeps the wasm path open — S7 is a thin sink, swappable).

## 2c. Incremental builds / dependency tracking (design requirement, not a bolt-on)

Requirement: an edit to one IG source file must not compel a full-world
rebuild — this serves both CI and the future editor loop.

- **Build-state ledger**: a `BuildState` table inside site.db (or sidecar):
  `node_key → input_hash → output_hash`, where node = {fsh file, md page,
  asset, resource, snapshot(url), expansion(vs url), db row group}. Recompute
  a node only when its input hash changed; propagate via output hashes.
- **Honest granularity per stage** (respect global semantics, don't fake it):
  - S1/S2: FSH semantics are tank-global (first-wins aliases, RuleSets,
    fishing order). V1 policy: recompile ALL FSH on any .fsh/config change —
    it's sub-second native for real IGs and milliseconds for cycle. Finer
    per-entity invalidation is a later optimization, not v1. But S2's OUTPUT
    is diffed per-resource (hash), so unchanged resources don't dirty S3+.
  - S3: per-SD node, deps = own JSON + base/type/extension SD closure
    (the walk already knows this closure; record it as the dep set).
  - S4: per-VS node, deps = VS def + referenced CS defs + tx-server identity.
    Expansions are the expensive/networked stage — cache keyed by content
    hash means the tx server is hit ONLY when terminology actually changed.
  - S6: per-file nodes (one md edit → one Pages row + its referenced assets).
  - S7: UPDATE site.db in place (delete/insert dirty rows), not rewrite.
- **Renderer-side incrementality — the two-ledger split.** Nobody can know a
  page's transitive data inputs statically (liquid fragments, baseDefinition
  walks, arbitrary `{% sql %}`). So: Ledger 1 (Rust, above) tracks
  source→row lineage; Ledger 2 (TS, later) OBSERVES each page's read set at
  render time by wrapping the single data choke point (`core/db.ts` +
  the `sql()` tag) in a recording proxy → `RenderDeps` table of
  (page → query fingerprint → result hash). Next build: re-render a page iff
  replaying its recorded queries yields a changed result hash (replay is
  cheap SQLite SELECTs; handles `{% sql %}` and fragment fishing for free —
  instrument the data boundary, never analyze templates). Global inputs
  (Menu/Metadata/layout/generator version) roll into one "chrome hash" that
  is a dep of every page. Prerequisites owned by the Rust producer NOW:
  stable row identities + content hashes in site.db; render-time
  determinism. Ledger 2 itself is a TS follow-up, enabled — not required —
  by this design (cycle-sized sites render fully in ~no time).
- **Determinism prerequisite**: `genDate`/`genDay` metadata must come from an
  injected build timestamp (env/git commit time), never wall clock, or every
  build is dirty by construction.

Wrapper-level site assets that are NOT site.db inputs but ARE final-site
inputs (copied after render): viewer bundles, sample SHL files, `skill.zip`,
`CNAME`, `404.html`, `site-gen/project/package-list.json`, and the
`output/qa*` cosmetic copies (Java-QA only; drop or stub in a Rust-only
build).

## 3. The expansion gap — options

1. **Pluggable tx client** (recommended): the producer calls `$expand` on a
   configured terminology server with an on-disk cache keyed by
   (valueset, version, tx-version). Point it at **terminus** (Josh's Bun/TS
   FHIR terminology server) locally/CI, or tx.fhir.org as fallback — exactly
   what cycle's experimental `publisher/terminology.ts` does today.
2. Reuse cycle's existing TS terminology step and only replace the
   resource/snapshot inputs (see Option A below) — zero new code.
3. Port expansion into Rust — **rejected for now** (a whole subsystem;
   terminus already exists and cycle's VS needs are small).

## 4. Implementation options

- **Option A — hybrid proof (days):** run cycle's existing experimental TS
  producer (`site-gen/publisher/build.ts`) but feed it OUR outputs: point its
  resource ingestion at `rust_sushi` output and REPLACE its `snapshots.ts`
  step with walk-engine snapshots (or pre-embed snapshots in the resource
  JSONs so its snapshot step becomes a no-op). Its own terminology.ts still
  does expansions; the existing TS ingest still builds site.db. Gate with
  `publisher/compare.ts` vs the committed Java fixture DB. Proves the §2
  data path with near-zero new code.
- **Option B — Rust-native `site_db` producer (the deliverable):** new crate
  in this workspace implementing S1–S7 of §2b: `rust_sushi build` → walk
  snapshots → expansions via tx client (§3) → S6 augmentation (faithful port
  of ingest.ts semantics, PlantUML excluded) → emit `site.db` (rusqlite),
  with the §2c BuildState ledger from day one (coarse S1/S2 granularity is
  fine; the ledger schema is not). TS side: bypass ingest, render from our
  site.db, assert the site.db contract at startup.
  Gates: (i) package.db-contract assertion; (ii) row parity vs the TS
  ingest's site.db built over the SAME inputs (the TS ingest is the
  augmentation oracle — same methodology, new oracle); (iii) `compare.ts`
  vs `site-gen/fixtures/package.db` for the §2 tables, diffs classified;
  (iv) the real gate — rendered site diffs clean (or explained) vs the
  Java-produced site; (v) incrementality gate — touch one md / one fsh /
  one VS: only the declared dep cone recomputes (assert via BuildState
  hashes), and a no-op rebuild is a no-op.
- **Option C — full Publisher-shaped DB:** also populate ValueSetList/
  CodeSystemList/Properties/Designations per `schema.ts` for future
  `{% sql %}` pages. Only on demand.

Recommendation: A as a spike to validate the §2 contract understanding, then
B. C deferred.

## 5. Notes / risks

- **SQLite in Rust**: rusqlite (bundled C sqlite) is fine natively; it does
  NOT fit the wasm editor path. Keep the DB writer as a thin sink behind the
  same data model so a wasm build can emit rows as JSON for a JS-side writer
  (or wa-sqlite) later. Do not let sqlite types leak into the pipeline.
- **`{% sql %}` open surface**: pages may query arbitrary tables. Today cycle
  uses none. Option B satisfies the asserted contract; anything beyond is
  Option C territory — document that boundary in the producer README.
- **`base` version pinning + `standardStatus` derivation** are the two
  bookkeeping behaviors with real logic; port from `rows.ts` with citations,
  same discipline as the snapshot work.
- Expansion determinism: cache expansions in-repo (like goldens) so site
  builds are reproducible offline and tx-server drift is a deliberate refresh.
- The committed `site-gen/fixtures/package.db` + `compare.ts` make this
  project oracle-gated end to end — same methodology, much smaller scope than
  the snapshot rework.
- **Residual Java touchpoints, resolved by decision (2026-07-02):** PlantUML
  is OUT OF SCOPE — SVGs must be pre-rendered/committed, producer fails loud
  on missing images (§2b). `output/qa*` copies: no Java run → no QA report;
  drop or stub in a Rust-only build (open, cosmetic-only).

## 6. Sequencing

After the post-rework roadmap's perf/clarity pass (task #12). Independent of
the WASM demo (#13) — can run in parallel or after, user's choice. Rough
effort: Option A spike ~1-2 days; Option B ~1 week including gates.

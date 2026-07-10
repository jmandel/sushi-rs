# Cycle IG: Rust Pipeline as the package.db Producer — Gap Analysis & Plan

> **HISTORICAL ANALYSIS — handoff design superseded.** Preferred
> `cycle-site/v2` exposes typed resource, terminology, navigation, config, and
> raw asset artifacts in a verified `ClosedSiteBuild`. The one-object
> `compat.site_db` projection remains only for v1 migration; `site.db` is not
> the universal renderer contract. Current model:
> [`crates/site_build/README.md`](../crates/site_build/README.md) and
> [`hosting.md`](hosting.md).

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
| `ValueSetList*` / `CodeSystemList*` tables | publisher indexed lists | ✅ Created empty-but-present (contract requires existence; renderer never reads them — see §4 "not doing") |
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
  is a dep of every page. Prerequisites owned by the Rust producer:
  stable row identities + content hashes in site.db; render-time
  determinism.
  **Scope decision (2026-07-02): Ledger 2 is DAY-1 scope**, not a follow-up —
  the TS renderer must handle SDC/IPS-scale IGs (hundreds of pages,
  per-resource fragment rendering), where full re-render per edit doesn't
  hold and the editor loop makes it worse. Deliverable order inside Phase 2:
  (1) producer ships hashes/determinism; (2) TS recording proxy + RenderDeps
  + replay-skip lands with it; (3) benchmark checkpoint: full cold render of
  SDC and IPS (generalizing site-gen beyond cycle is a cycle-repo concern,
  but OUR gate includes those two corpora), then warm single-edit renders
  must touch only the affected pages. Cold-render cost is measured, not
  assumed — if it's already trivial the ledger still pays for the editor
  loop.
- **Determinism prerequisite**: `genDate`/`genDay` metadata must come from an
  injected build timestamp (env/git commit time), never wall clock, or every
  build is dirty by construction.

Wrapper-level site assets that are NOT site.db inputs but ARE final-site
inputs (copied after render): viewer bundles, sample SHL files, `skill.zip`,
`CNAME`, `404.html`, `site-gen/project/package-list.json`, and the
`output/qa*` copies — DROPPED in a Rust-only build (Java-QA only; the walk
engine's messages channel is the future QA source; revisit on demand).

## 3. The expansion gap — decision

The producer calls `$expand` through a **pluggable tx client** with an
on-disk cache keyed by content hash of (valueset, referenced code systems,
tx-server identity). Default endpoint: **tx.fhir.org** — the same server the
Java IG Publisher uses, which also makes it the right parity choice — the
same shape as cycle's experimental `publisher/terminology.ts`. Expansions are
committed like goldens so builds are reproducible offline (the tx server is
touched only on deliberate refresh); the pluggable interface means a
different/local server can slot in later, but NO plan depends on one
existing.

Rejected: porting a terminology SERVICE into Rust (external-system
subsumption/filters). Carve-out (2026-07-02, for the editor):
a small `expand_enumerable()` evaluator in the engine for composes that are
pure functions of IG content (local CS + enumerated external codes) — CI
gates it against the cached tx expansions on the shared domain (editor §6).
Authoritative site builds still use the tx client below for everything. The Phase-1 spike sidesteps the question entirely by
reusing cycle's TS terminology step — that's part of what makes it a spike.

**Tx surface beyond $expand (decided):** `$expand` is the ONLY terminology
operation the current contract needs — member displays come back in the
expansion; profile-referenced code displays render from authored JSON; local
CS concepts need no tx; `$validate-code`/display-checking is QA scope
(dropped). BUT the tx client interface is **operation-generic from day one**
(`expand` | `lookup` | `validate-code`, one content-hash cache discipline) so
publisher-grade display resolution or validation later is a new cache entry,
not a redesign. Reproducibility: cache each `$expand` WITH the response's
`expansion.parameter` (the code-system versions the server actually used) —
the tx-side equivalent of our oracle pin; a changed server-side CS version is
a visible cache-key event, not silent drift. Precondition to state: filter/
`is-a` composes require the configured tx server to have the referenced
external code systems loaded — fail loud when it doesn't, never emit a
partial expansion.

## 4. Plan

**Phase 1 — spike (days):** run cycle's existing experimental TS producer
(`site-gen/publisher/build.ts`) fed by OUR outputs: point its resource
ingestion at `rust_sushi` output with walk-engine snapshots pre-embedded (its
snapshot step becomes a no-op). Its terminology.ts still does expansions; the
existing TS ingest still builds site.db. Gate: `publisher/compare.ts` vs the
committed Java fixture DB. Purpose: validate the §2 contract understanding
with near-zero new code before building anything.

**Phase 2 — the deliverable:** a Rust-native `site_db` crate in this
workspace implementing S1–S7 of §2b: `rust_sushi build` → walk snapshots →
expansions via the tx client (§3) → S6 augmentation (faithful port of
ingest.ts semantics, PlantUML excluded) → emit `site.db` (rusqlite), with the
§2c BuildState producer ledger from day one (coarse S1/S2 granularity is
fine; the ledger schema is not) AND the day-1 TS renderer ledger (§2c).
TS side: delete/bypass ingest, render from our site.db, assert the site.db
contract at startup.

Gates for Phase 2: (i) package.db-contract assertion; (ii) row parity vs the
TS ingest's site.db built over the SAME inputs (the TS ingest is the
augmentation oracle — same methodology, new oracle); (iii) `compare.ts` vs
`site-gen/fixtures/package.db` for the §2 tables, diffs classified; (iv) the
real gate — rendered site diffs clean (or explained) vs the Java-produced
site; (v) producer incrementality — touch one md / one fsh / one VS: only the
declared dep cone recomputes (asserted via BuildState hashes), and a no-op
rebuild is a no-op; (vi) renderer incrementality — after a single-file edit,
only pages whose replayed read sets changed re-render; verified at SDC/IPS
scale with cold-render benchmarks recorded.

**Explicitly not doing:** populating the full Publisher-shaped extra tables
(ValueSetList/CodeSystemList/Properties/Designations per `schema.ts`). They
are created empty to satisfy the contract; the producer README documents that
boundary. Revisit only when an actual page needs `{% sql %}` over them.

## 4b. Phase 1 spike — RESULTS (2026-07-03, task #15)

Ran end-to-end with ZERO engine changes and zero cycle-repo code edits (env
wiring only, `rust-feed-spike` branch). Verdicts:

- **Row-parity gate: 0 differences.** Rust-fed (rust_sushi resources + walk
  snapshots embedded) through the TS producer = byte-identical package.db
  row set vs the TS producer's own-SUSHI run over identical sources — all 16
  tables, 17 resources, Metadata/Concepts/IndexedLists exact.
- **Render sanity: PASS.** ingest contract assertion passed; 39 HTML pages
  rendered from OUR walk-engine snapshot.element; link check clean.
- **Committed fixture is STALE**: fixtures/package.db was built from 0.1.0
  sources matching no commit. Phase-2 gates (iii)/(iv) REQUIRE regenerating
  it from HEAD with the Java Publisher — first cycle-repo task of Phase 2.
- **Cycle needs zero expansions** (ValueSet_Codes empty even in the Java
  fixture) → **S4 is deferred behind S5/S6/S7**; promoted only when a corpus
  (SDC/IPS) actually has expandable non-local ValueSets.
- **Resources.Json byte-parity vs Java needs two engine-level behaviors we
  deliberately do not have in Layer A**: (1) `constraint.xpath` present in
  the R4-artifact shape the Publisher stores in package.db (our native-R5
  internal drops it — this is stage-6 PROJECT territory), and (2)
  version-pinning of inherited canonicals (`|4.0.1` on binding.valueSet /
  type.profile/targetProfile) — which is LAYER B (task #17) arriving early.
  DECISION: Phase 2 gates on (a) row parity vs the TS-own oracle and (b)
  rendered-site parity; full Resources.Json byte-parity vs Java folds into
  #17 / stage-6 work and is NOT a Phase-2 gate. (Amusing datum: on
  type.extension valueUrl the walk engine matches Java and SUSHI is the
  outlier — our snapshots are closer to Java than SUSHI's are.)
- **rust_sushi gap found**: IG export emits `exampleBoolean:false` where
  SUSHI/Java emit `exampleCanonical:<profile-url>` for profile-associated
  examples (association from sushi-config). Drives ProfilePage "Examples"
  lists. Isolated engine fix, schedule deliberately.

**Phase-2 sequencing (revised by spike evidence):** (1) S7 emit + S5 rows
first, gated on the TS-own oracle; (2) the snapshot deltas needed for
RENDERED parity only; (3) rust_sushi exampleCanonical fix; (4) S6 augment
(ingest.ts port — semantics already pinned by the spike); (5) S4 + tx client
LAST and not on cycle's gate; BuildState ledger schema from day one.
Fixture regen from HEAD precedes gates (iii)/(iv).

## 5. Notes / risks

- **SQLite in Rust**: rusqlite (bundled C sqlite) is fine natively; it does
  NOT fit the wasm editor path. Keep the DB writer as a thin sink behind the
  same data model so a wasm build can emit rows as JSON for a JS-side writer
  (or wa-sqlite) later. Do not let sqlite types leak into the pipeline.
- **`{% sql %}` open surface**: pages may query arbitrary tables. Today cycle
  uses none. Phase 2 satisfies the asserted contract; the extra tables stay
  empty per §4 "not doing", with that boundary documented in the producer
  README.
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
  DROPPED (cosmetic-only; see §2c wrapper-assets note).

## 6. Sequencing

After the post-rework roadmap's perf/clarity pass (task #12). Independent of
the WASM P0 (#13) and a PREREQUISITE of the editor demo repo (#16, decided
2026-07-02 — the demo's site preview depends on this producer). Rough
effort: Phase 1 spike ~1-2 days; Phase 2 ~1.5 weeks including gates and the
renderer ledger.

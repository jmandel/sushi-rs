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

## 2b. The ingest augmentation (package.db → site.db) — its inputs count too

`ingest.ts` copies `package.db` to `temp/site-gen/site.db` and ADDS four
tables (`Pages`, `Menu`, `SiteConfig`, `Assets`); `build.tsx` renders only
from `site.db`. So the full replacement contract = package.db (§2) **plus the
augmentation inputs**. We keep ingest as-is (it's part of the TS downstream),
but a self-contained pipeline must ensure every one of these exists at
ingest time:

| Ingest input | → table | Produced by | Notes for our pipeline |
|---|---|---|---|
| `input/pagecontent/<slug>.md` (or .xml); first `# H1` overrides title | `Pages` | repo-authored | Page SET comes from the IG resource's `definition.page` tree (which WE generate) — a page listed there but missing on disk breaks ingest; keep the same fail-loud behavior |
| `sushi-config.yaml` (whole file → `SiteConfig`; `menu:` → `Menu`) | `Menu`, `SiteConfig` | repo-authored | Same file rust_sushi compiles from; no new work, but note ingest reads the RAW yaml (incl. fields SUSHI ignores) — don't "clean" it |
| `input/images/**` (recursive, mime by extension) | `Assets` | repo-authored | pass-through |
| `input/includes/<name>` — ONLY includes actually referenced by page markdown; nested text includes followed | `Assets` | repo-authored **+ build-generated**: the wrapper writes e.g. `sample-viewer-links.md` into includes BEFORE ingest | ⚠️ ordering dependency: any generated includes must be materialized before ingest runs. In cycle, that's the wrapper's job (viewer build); our pipeline orchestration must preserve the slot |
| `input/images-source/<name>.plantuml` → rendered SVG when a referenced `.svg` is missing | `Assets` | repo-authored source + **PlantUML jar (Maven fetch) + `java -jar`** | ⚠️ the one ingest path that reintroduces Java + network. Mitigations: pre-render SVGs into `input/images/` in CI (jar never triggers), commit rendered SVGs, or swap ingest to a non-Java plantuml renderer later. Decide per-repo; for cycle a pre-render step is trivial |
| env knobs: `PKG_DB`, `CONFIG`, `SITE_LIQUID_ASSET_DIRS`, `PLANTUML_JAR` | — | wrapper | our orchestration sets `PKG_DB` to the Rust-produced DB; the rest keep defaults |

Wrapper-level site assets that are NOT ingest inputs but ARE final-site
inputs (copied after render): viewer bundles, sample SHL files, `skill.zip`,
`CNAME`, `404.html`, `site-gen/project/package-list.json`, and the
`output/qa*` cosmetic copies (Java-QA only; drop or stub when no Java run
exists — decide what replaces the QA page in a Rust-only build).

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
  does expansions. Gate with `publisher/compare.ts` vs the committed Java
  fixture DB. Proves end-to-end with minimal new code.
- **Option B — Rust-native producer (the deliverable):** new crate
  `package_db` in this workspace: `rust_sushi build` → walk snapshots for all
  SDs → expansions via tx client (§3) → emit `package.db` with rusqlite,
  satisfying `contract.ts` (vendored copy + a Rust-side contract test).
  Gates: (i) contract assertion passes; (ii) `compare.ts` (or a port of it)
  vs `site-gen/fixtures/package.db` with classified diffs; (iii) the real
  gate — `bun run build:sitegen` against our DB renders a site that diffs
  clean (or explained) vs the Java-produced site.
- **Option C — full Publisher-shaped DB:** also populate ValueSetList/
  CodeSystemList/Properties/Designations per `schema.ts` for future
  `{% sql %}` pages. Only on demand.

Recommendation: A as a spike to validate the contract understanding, then B.
C deferred.

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
- **A Rust-only build has two residual Java touchpoints to plan around** (both
  ingest/wrapper-side, not renderer-side): PlantUML rendering (pre-render SVGs
  to avoid) and the `output/qa*` copies (no Java publisher run → no QA report;
  decide replacement or drop). Neither blocks Option A/B — both have trivial
  mitigations — but a "zero-Java CI" claim isn't true until they're handled.

## 6. Sequencing

After the post-rework roadmap's perf/clarity pass (task #12). Independent of
the WASM demo (#13) — can run in parallel or after, user's choice. Rough
effort: Option A spike ~1-2 days; Option B ~1 week including gates.

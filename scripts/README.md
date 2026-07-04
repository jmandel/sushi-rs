# Scripts index

One-line inventory of every script under `snapshot/`, `scripts/`, `harness/`,
and `demo/*`, with the project phase that owns it. This is the map; the scripts
themselves carry the detailed header comments. (Vendored third-party code under
`demo/wasm-p0/vendor/` is excluded.)

Owner-phase legend: **P0** snapshot walk engine · **#32** one Rust resolver ·
**F1c** liquid · **F1b** md/kramdown · **F0/F1–F5** stock-template renderer ·
**P/M** wasm editor demos · **cross** shared gate/oracle infra.

## `snapshot/` — snapshot walk engine + published-package harvest (P0, #32)

| Script | One-liner | Owner |
|---|---|---|
| `oracle/gen-snapshot.sh` | Run the fhir-core oracle to emit a golden snapshot (`--r4`/`--r5`, `--sort`; `TRACE=1` for the decision tracer). | P0 |
| `gen-goldens.sh` | Regenerate every `snapshot/goldens/*.snapshot.json` from `snapshot/fixtures/*.json` via the oracle. | P0 |
| `install-fhir-package.cjs` | Fetch + unpack a FHIR package (and its deps) into the isolated cache. | cross |
| `package-deps.cjs` | Print an IG's transitive R4 context closure — a pure shim over `rust_sushi resolve` (no Node fallback; #3). | #32 |
| `package-deps-gate.sh` | 8-IG regression gate: `package-deps.cjs` stdout == direct `rust_sushi resolve` (shim-wiring parity). | #32 |
| `harvest-r4-package.cjs` | Harvest an R4 published-package IG's SDs into a fixtures+goldens corpus dir. | P0 |
| `harvest-r4-package.sh` | Driver for `harvest-r4-package.cjs` (resolves the closure via the shim, then harvests). | P0 |
| `harvest-r4-sushi.cjs` | Harvest self-built (SUSHI `fsh-generated`) R4 SD fixtures + goldens. | P0 |
| `harvest-r4-sushi.sh` | Driver for `harvest-r4-sushi.cjs` (optionally runs stock SUSHI first). | P0 |
| `gen-harvested-r4-goldens.sh` | (Re)generate goldens for a harvested-R4 corpus dir from the oracle. | P0 |
| `check-harvested-r4.sh` | Gate: build each harvested SD with `snapshot_gen`, byte-diff vs its golden. | P0 |
| `diff-snapshot.cjs` | Byte-diff two snapshot JSONs (`<expected> <actual>`) with element-path context. | P0 |
| `diff-trace.cjs` | Compare two snapshot decision-trace JSONL files (oracle vs walk engine). | P0 |
| `arbitrary-ig-e2e.sh` | End-to-end (#32 gate iv): an un-prepinned IG's closure loads + snapshots. | #32 |

## `scripts/` — stock-template renderer substrates (F1b/F1c/F0–F5)

| Script | One-liner | Owner |
|---|---|---|
| `harvest-render-goldens.sh` | Build the render golden corpus from real IG Publisher runs (fragments + pages). | F0 |
| `diff-fragment.cjs` | Normalization-free byte diff between two fragment/page files (line/col context). | F0–F5 |
| `liquid-oracle.rb` | Jekyll/Shopify-Liquid oracle: render a template through the real Liquid gem. | F1c |
| `liquid-build-context.rb` | Build the `site`/`page` context object the liquid oracle renders against. | F1c |
| `strip-liquid.py` | Strip `{% … %}` (incl. whitespace-control) — differential-corpus prep. | F1c |
| `liquid-gate-setup.sh` | Materialize the F1c differential-gate corpus (staging inputs). | F1c |
| `liquid-gate.sh` | F1c differential gate: render every liquid-bearing corpus page vs the oracle. | F1c |
| `liquid-diff.sh` | Ad-hoc single-template render + diff for `render_liquid`. | F1c |
| `kramdown-oracle.rb` | kramdown oracle: render markdown through the real kramdown gem. | F1b |
| `md-diff.py` | Render the survey corpus through `render_md`, classify diffs vs kramdown. | F1b |
| `md-diff-cluster.py` | Cluster unexplained md diffs by changed-line signature (find the dominant bug). | F1b |
| `refresh-terminology-goldens.py` | Refresh the tx/expansion goldens used by the renderer/terminology tests. | F4 |
| `pack-site-tree.cjs` | Pack a staged site dir into the `Session.mountSite` files JSON (text/b64 map). | F6 |
| `wasm-parity.sh` | Build `wasm_api` (wasm32 + wasm-bindgen) and run the fixture+corpus gates against the WASM build. | P/M |
| `wasm-parity-driver.mjs` | Node driver `wasm-parity.sh` invokes (init → set_local → generate_snapshot vs goldens). | P/M |

## `harness/` — SUSHI-compiler parity gates, oracles, dashboards (cross)

| Script | One-liner | Owner |
|---|---|---|
| `_guard.sh` | Defensive guard sourced by gates: NEVER let a run touch the real `~/.fhir`. | cross |
| `run-stock.sh` | Phase-0 oracle: run stock TS SUSHI on an IG, capture `fsh-generated`. | cross |
| `harvest-gate.sh` | Permanent regression gate for the SUSHI-harvest corpus (`tests/sushi-harvest/`). | cross |
| `harvest-oracle.sh` | Regenerate the stock-SUSHI-CLI oracle (`expected/*.json`) for the harvest corpus. | cross |
| `harvest-extract.cjs` | Harvest self-contained FSH snippets from stock SUSHI's own unit tests. | cross |
| `harvest-materialize.cjs` | Materialize harvested snippets into the permanent corpus layout. | cross |
| `gate1.sh` | Concurrency-safe single-purpose parity gate (private out dir; parallel-worktree safe). | cross |
| `full-dashboard.sh` | Parity scorecard across the 4 tuning IGs + 8 holdout IGs (12 total). | cross |
| `parity-dashboard.sh` | Phase-8 scorecard: build each IG, report per-resource-type parity. | cross |
| `dashboard-top20.sh` | Parity scorecard for the 6 FSH-buildable top-20 IGs outside the 12-IG corpus. | cross |
| `perf31.sh` | Two-phase performance harness over the 31-IG corpus. | cross |
| `acquisition-dashboard.sh` | Compare a stock-cache Rust build vs an acquisition-materialized Rust build. | cross |
| `acquisition-pkg-fish.sh` | Package-fishing parity against acquisition-materialized caches. | cross |
| `diff-resources.sh` | Phase-0 byte-parity gate: diff generated resources of two SUSHI runs. | cross |
| `diff-resources-glob.sh` | Per-resource byte-parity for a filename-glob subset. | cross |
| `diff-instances.sh` | Instance byte-parity: diff all generated resources EXCEPT SD/VS/CS. | cross |
| `lex-oracle.cjs` | ANTLR lexer oracle: emit token goldens for a FSH fixture. | cross |
| `gen-lex-goldens.sh` | Regenerate lexer goldens from the ANTLR oracle for every fixture. | cross |
| `parse-oracle.cjs` | SUSHI import-AST oracle: emit the parsed-AST JSON golden for a fixture. | cross |
| `gen-ast-goldens.sh` | Regenerate AST goldens from the import oracle for every lex fixture. | cross |
| `cmp-ast.cjs` | Semantic AST comparator: compare two import-AST JSON dumps for equality. | cross |
| `expand-oracle.cjs` | SUSHI ValueSet-expansion oracle for a FSH fixture. | cross |
| `gen-expand-goldens.sh` | Regenerate expansion goldens (`compiler/tests/goldens/expand`) via the oracle. | cross |
| `package-oracle.cjs` | package_store oracle: resolve+load the FHIR package closure for a project. | cross |
| `gen-pkg-queries.cjs` | Generate deterministic package-fishing queries from a materialized cache. | cross |
| `diff-pkg.cjs` | Compare package-store fishing results (oracle vs rust), ignoring volatile fields. | cross |
| `diag.cjs` | Parse a compiler console log into ordered JSON records + diff two logs. | cross |

## `demo/` — in-browser wasm editor demos (P/M phases)

| Script | One-liner | Owner |
|---|---|---|
| `wasm-p0/prepare.sh` | Prepare the P0 demo's data (package bundle + fixtures). | P0-demo |
| `wasm-p0/check-native.sh` | P0 byte-match gate — native side (compare demo output vs native). | P0-demo |
| `wasm-p0/app.js` | P0 browser demo driver (WASI shim path). | P0-demo |
| `wasm-p2/prepare.sh` | Prepare the P2 demo's data (bundles + IG working set). | M2-demo |
| `wasm-p2/worker.js` | P2 engine Web Worker (calls the wasm_api surface). | M2-demo |
| `wasm-p2/drive-node.mjs` | Headless P2 driver: same init → compile → snapshot flow under Node. | M2-demo |

## Removed

- `harness/t1-dashboard.sh` — deleted 2026-07-03 (Consolidation Pass 1). A
  private-dir 4-IG snapshot dashboard superseded by `gate1.sh` (concurrency-safe)
  + `full-dashboard.sh`; referenced by nothing, last touched 2026-06-30, and it
  hardcoded a stale external `/home/jmandel/periodicity/temp/` path.

# fhir-ig-editor — Spec for the Demo Repo

> Status: SPEC (2026-07-02). Target: new GitHub repo `jmandel/fhir-ig-editor`.
> This is the concrete home of the "WASM demo" roadmap item (task #13 → #16);
> docs/wasm-editor-plan.md remains the engine-side plan (P0-P2 land in
> sushi-rs), this repo is the product shell (P3). Sequencing: after the
> snapshot-rework cutover + main merge + perf/clarity pass, per REWORK-PLAN §7.

## 1. What it is

A fully static, GitHub-Pages-hosted web editor for FHIR IGs: list/edit FSH
and IG files in the browser, compile with rust_sushi + generate snapshots
with the walk engine (both as WASM in a Web Worker), see diagnostics and
rendered results live. Loads an existing IG — **default: the cycle IG** —
and works offline after first load. No server component, ever: hosting is
`git push` → Actions → Pages.

## 2. Repo shape

```
fhir-ig-editor/
  .gitmodules
  vendor/sushi-rs/        # submodule: jmandel/sushi-rs (engine workspace)
  vendor/cycle/           # submodule: jmandel/cycle (default IG + site-gen)
  app/                    # the editor SPA (Vite + React + Monaco)
    src/
      editor/             # Monaco setup, FSH grammar, file tree, tabs
      worker/             # engine worker: wasm loading, build protocol
      views/              # diagnostics panel, resource JSON, snapshot tree,
                          #   differential table, build status
      vfs/                # OPFS-backed project store + loaders (see §5)
  data/                   # build-time generated static data (NOT committed):
      packages/           #   package bundles {tgz + prebuilt index}
      expansions/         #   precomputed tx cache for the default IG
  scripts/                # CI glue: build-wasm.sh, bundle-packages.ts,
                          #   precompute-expansions.ts (runs terminus)
  .github/workflows/pages.yml
  SPEC.md                 # this document, moved over
```

**Version pinning = submodule SHAs.** The engine the demo runs is exactly the
pinned sushi-rs commit; bumping the submodule is the upgrade event (CI runs
the engine's own test suite at that SHA before deploying). Same for cycle.

## 3. Decided technology choices

- **Editor: Monaco** (the VS Code editor component), not full VS Code Web.
  Monaco gives the VS Code editing feel, multi-file model, markers API for
  diagnostics, and works as a plain static bundle. Full vscode.dev/code-server
  is rejected for v1: its FS provider + extension host model is heavy and
  fights static hosting; if we later want it, the worker protocol (§4) is the
  reusable seam. FSH syntax: reuse the TextMate grammar from SUSHI's vscode
  extension (license-check in CI).
- **Engine build: `wasm32-unknown-unknown` + wasm-bindgen** via the sushi-rs
  `wasm_api` crate (wasm-editor-plan P2). The editor repo does NOT patch the
  engine; anything the editor needs from the engine is a sushi-rs PR.
- **Storage: OPFS** for the working project + package cache, IndexedDB
  fallback. The `PackageSource` browser impl lives in sushi-rs (P1); the
  editor only mounts it.
- **UI stack: Vite + React + TypeScript** — matches cycle's site-gen/viewer
  idioms; no SSR, no framework server.

## 4. Dataflow / worker protocol

```
UI (Monaco, file tree)
  │ file edits (debounced ~300ms)
  ▼
Engine Worker (one, owns wasm instance + OPFS handles)
  compile(changedPaths) ──→ { resources[], diagnostics[], buildMs }
  snapshot(profileUrl) ───→ { snapshot, messages }      (on-demand, memoized)
  fileOps(list/read/write/rename/delete)
  ▼
Views: diagnostics → Monaco markers + problems panel
       resources  → JSON view, differential table, snapshot tree
       status     → per-stage timings (parse/compile/snapshot)
```

- Diagnostics carry SUSHI-exact wording + spans (the port guarantees this) —
  they map 1:1 to Monaco markers.
- Incrementality v1 = the engine's own speed (full compile per debounce;
  sub-second for cycle-scale) + per-profile snapshot memoization. The
  BuildState/ledger machinery from the cycle plan slots in here later
  unchanged — same engine, same hashes.

## 5. Loading an IG (three modes, all → OPFS project)

1. **Baked default (demo path):** the cycle IG's `input/**`, `sushi-config.yaml`
   (+ fsh sources) are exported at CI time from the submodule into static
   JSON manifests; "Open demo IG" hydrates OPFS from them. One click, works
   offline thereafter.
2. **From GitHub:** user pastes `owner/repo[@ref]`; loader pulls the tree via
   the GitHub API / raw.githubusercontent (CORS-friendly), filtered to IG
   files. Read-only origin; edits live in OPFS; "download zip" to export
   (no push integration in v1).
3. **Local folder:** File System Access API directory handle (Chromium),
   drag-drop zip fallback elsewhere.

## 6. Terminology: in-editor re-expansion (revised 2026-07-02)

Editing a ValueSet is a first-class editor action — its expansion must
recompute cleanly in the browser. Three tiers, decided:

1. **Enumerable composes expand IN the engine (wasm), no tx.** Includes over
   IG-local CodeSystems (with local filters/hierarchy) and explicitly
   enumerated external codes (authored code+display lists) are pure functions
   of IG content — a small `expand_enumerable()` evaluator in the engine
   handles them per keystroke. This covers ALL of the cycle IG's ValueSets
   and most authoring reality. NOT a tx server port: no external-CS
   subsumption, ever (that boundary stays per cycle-plan §3).
2. **Filter-based composes over external systems (SNOMED is-a etc.):**
   cannot be computed without the external CS. The affected views show a
   precise "needs terminology server" state naming the un-expandable
   include, and — if the user configures a tx endpoint (their terminus, CORS
   enabled) — the editor calls `$expand` live and caches the result in OPFS
   (same content-hash cache keys as the CI cache). No configured tx =
   visible, scoped degradation; never a silently partial expansion.
3. **CI-precomputed cache (terminus)** still ships for the default IG as the
   warm-start + the AUTHORITY: a CI gate asserts `expand_enumerable()`
   output matches the terminus expansion for every enumerable VS in the
   default IG — two expanders are tolerable only while provably agreeing on
   the shared domain; on mismatch, terminus wins and the evaluator is fixed.

Cascade correctness: an expansion is an S4 node in the same dependency
ledger — VS edit → re-expand → ValueSet_Codes rows dirty → dependent pages
re-render via read-set replay. The demo should show exactly this: edit a
compose, watch the VS page's expansion table update.

## 7. What "viewing results" means, by milestone

- **M1 (the demo bar):** per-resource views — compiled JSON, differential
  table, **snapshot element tree** (the walk engine's output), diagnostics
  panel, build timings. This is already more than any in-browser FSH tool
  shows today.
- **M2 (in the demo bar — decided 2026-07-02):** site preview — run the
  cycle site-gen renderer against an in-browser site.db (the Rust site.db
  producer, task #15, is a PREREQUISITE of this repo; compiled into the wasm
  build with a wa-sqlite or JS-side row store for `core/db.ts`). The demo
  shows the actual rendered IG pages updating on edit, not just resource
  views.

## 8. CI / deploy (`pages.yml`)

1. Checkout with submodules.
2. Run the pinned sushi-rs test suite (fast subset: ladder + IPS gate) — the
   demo never deploys an engine that fails its own gates.
3. `cargo build --target wasm32-unknown-unknown -p wasm_api` + wasm-bindgen
   + wasm-opt.
4. Bundle packages (r4.core + cycle deps) into `data/packages/`.
5. Precompute expansions via terminus into `data/expansions/`.
6. Export the default-IG manifest from `vendor/cycle`.
7. Vite build → deploy to Pages.

## 9. Milestones & gates

| M | Deliverable | Gate |
|---|---|---|
| M0 | Repo scaffold, submodules pinned, CI deploys a hello-world page that instantiates the wasm engine and compiles one FSH string | Pages URL up; engine version + build time shown |
| M1 | Full demo: open cycle IG, edit FSH, live diagnostics + JSON + snapshot tree; offline after first load | Edit→feedback < 1s for cycle; wasm outputs byte-match native for the whole cycle IG (CI assert); works in Chrome+Firefox |
| M2 | Site preview (in demo bar) | rendered pages match the real site build; page updates on edit touch only read-set-dirty pages (Ledger 2) |

## 10. Dependencies & sequencing

- Needs from sushi-rs first (wasm-editor-plan): P1 `PackageSource` trait,
  P2 `wasm_api` + wasm parity harness. The editor repo consumes releases of
  these via submodule bump — it never forks engine code.
- Order per REWORK-PLAN §7 roadmap: cutover → main merge → perf/clarity →
  wasm P0-P2 (sushi-rs) + **cycle site.db producer (task #15, PREREQUISITE)**
  → this repo M0-M2 (task #16).

## 11. Risks

- **Monaco + FSH grammar fidelity**: TextMate grammar in Monaco needs
  monaco-textmate shim; budgeted in M1, fallback = basic tokenizer.
- **Package bundle size** on Pages (~tens of MB): lazy-load per dependency,
  cache in OPFS; measure first load in M1 gate.
- **Safari OPFS/FS-Access gaps**: Chrome+Firefox are the M1 bar; Safari
  degrade-gracefully (in-memory project, no persistence) — stated, not fixed.
- **wasm/native drift**: prevented structurally — CI byte-compares wasm
  output vs native output for the whole default IG on every deploy.

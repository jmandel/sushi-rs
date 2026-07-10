# Simplification Ledger

> **HISTORICAL LEDGER through its dated entries.** It explains consolidation
> decisions already made, but its open list is not the current roadmap. Current
> engine seams and remaining cleanup live in the repository
> [`README.md`](../README.md) and [`hosting.md`](hosting.md).

> Standing directive (Josh, 2026-07-03): complexity-by-accretion is tracked
> and collapsed with the SAME priority as performance work. Rules: candidates
> land here as they're spotted (agents flag them in reports; coordinator
> aggregates); a consolidation pass runs at phase boundaries (like the #12
> clarity pass): one coherent change-set per gate cycle, full gates green,
> correctness never traded. "Simpler but different output" is not simpler.

## Open candidates (spotted → collapse when owning agents quiesce)

9. **cycle rust-feed-spike branch** — carries spike wiring + fixture regen;
   fold what's permanent into a clean PR to cycle main, drop the rest.
   (cycle-repo owned.)

## Done

- (2026-07-04, Consolidation Pass 2 — the `fig` unified CLI) **Three user
  binaries → one, and ONE envelope for two skins.**
  - **Envelope dedup**: the apiVersion result/error envelope (`envelope` /
    `envelope_ser` / `API_VERSION`) is now the single `api_envelope` crate;
    `wasm_api` deleted its 42-line private copy and imports it, and `fig --json`
    uses the same. One implementation for the Session AND the CLI (schema pinned
    by `crates/fig/tests/json_envelope.rs`). Gate: session_equiv permanent gate
    green (envelopes byte-identical), workspace tests 0 failures.
  - **snapshot_gen + site_db binaries folded into fig**: both original `main.rs`
    bins deleted (snapshot_gen 3-line wrapper; site_db's 149-line CLI promoted
    verbatim into `site_db::run_cli`, so the bin AND `fig sitedb` compose ONE
    implementation). `fig` provides deprecated alias shims (migration note to
    stderr) for one release. `rust_sushi` kept one release (dev-oracle
    subcommands = gate infra, plan §2); its user build/resolve/bundle are
    `fig build`/`fig resolve`/`fig packages bundle` (byte-identical resolve
    proven; package-deps gate 8/8 through `fig resolve`).
  - **Harness migration**: package-deps.cjs + gate, arbitrary-ig-e2e,
    check-harvested-r4 now drive `fig`/the aliases. No gate regressed.
  - Net: 2 fewer user binaries, one canonical envelope, one canonical site_db
    CLI. The new `fig` crate (~1580 lines incl. the runner .mjs) is composition
    over the existing engine — no engine logic duplicated (iron rule).

- (2026-07-04, coordinator adjudication) **markdown-it stays in cycle's
  generator as PRESENTATION** — the ContentApi sunset targets duplicate
  PUBLISHER semantics (liquid/kramdown/publisher-markdown), not a
  generator's own design-layer markdown (table-scroll wrappers, heading
  permalinks, task checkboxes, custom slugify are cycle chrome by design).
  The byte-identity gate correctly protected against the over-broad
  directive; principle recorded: one implementation of publisher semantics;
  hosts own their own presentation.

- (2026-07-04, F6 scope 2) **#2 Editor worker protocol — DONE** (editor
  `2d1c654`): ONE typed op table (`EngineOps`) types every operation's args +
  result; worker = a handler per op, client = `call(op, ...args)`; Session
  envelopes unwrapped in ONE place (apiVersion check, `ok:false` → throw).
  Gate: tsc clean, vite build, verify-e2e PASS.
- (2026-07-04, F6 scope 2) **#8 Editor M2 shims — RESOLVED with a documented
  remainder** (editor `6e0c866`, `25a0f3b`): DELETED — `@cycle/core/liquid`
  import + the `liquidjs` vite alias (TS liquid sunset; cycle narrative Liquid
  runs in the engine ContentApi, byte-gate 17/17); the M2-special worker
  surface is now the generic op table behind the SiteGeneratorAdapter
  contract. RETAINED lawfully — the vite `core/db` resolveId redirect,
  `@cycle/*` aliases, and `process.env.SITE_*` defines: they exist to run the
  READ-ONLY cycle submodule's React chrome in a browser worker, which the
  adapter contract keeps as cycle's presentation layer by design.
  markdown-it also stays: cycle's markdown config is DESIGN (table-scroll
  wrappers, heading-anchor permalinks, task-list checkboxes, code-col
  detection, custom slugify) — a kramdown swap cannot hold the byte gate by
  construction; conflict reported to the coordinator with the feature list.

- (2026-07-03, Consolidation Pass 1 — item #3) **Standalone-crate workspaces
  folded into root**: render_{xhtml,liquid,md,tables,sd,page} each carried their
  own `[workspace]` (parallel-agent isolation). Folded ALL six into the root
  workspace in one commit (`7c6d3cda`): root `members` + `workspace.dependencies`
  path entries; per-crate tables dropped; version/edition/license/rust-version
  inherited from `workspace.package`; serde_json (preserve_order)/serde_yaml
  unified via `workspace = true`; the 6 per-crate `Cargo.lock`s deleted (ONE root
  lock) and stray `target/` dirs removed. Gate: `cargo test --workspace` 176
  passed / 0 failed (now includes the render crates); fragment spot snapshot
  us-core 70/70, dict cycle 7/7, cld plan-net 24/24 byte-identical; pagecorpus
  plan-net 678/678; sushi harvest 326/326 (256/256 byte-identical). Cargo-only
  change — snapshot_gen bytes untouched, so the harvested-r4 spot floors are
  unchanged.
- (2026-07-03, Consolidation Pass 1 — item #1) **wasm_api collapsed into a
  `Session` surface** (`bc211a85`): the 8 accreted flat exports become one
  wasm-bindgen `Session` handle with grouped methods over the SINGLE process
  engine, ONE apiVersion-stamped result envelope + ONE error envelope (domain
  errors are `ok:false`, never thrown). All operation logic moved into inherent
  `Engine` methods returning `Result<_, String>`; the two facades only marshal.
  The old flat exports REMAIN as `#[deprecated]` thin wrappers preserving their
  exact historical output bytes (M2 editor + wasm-parity-driver depend on them;
  F6 migrates then deletes). New `tests/session_api.rs` proves envelope shape +
  that `result` == the legacy raw payload (migration is pure re-wrapping). Gate:
  `cargo test -p wasm_api` (expand_api 5/5, session_api 5/5, site_db_snapshot
  1/1); compiler compile_equiv 1/1; site_db inmem_vs_disk 1/1; workspace green.
  DEFERRED: `scripts/wasm-parity.sh` — the wasm32 toolchain + wasm-bindgen are
  NOT installed in this env (no scratch rustup set); native gates cover the
  shared Engine logic and the wasm path is a mechanical bindgen wrapper. Re-run
  wasm-parity on a box with the toolchain (recipe in demo/wasm-p0/README.md)
  before F6 freezes the editor against `Session`.
- (2026-07-03, Consolidation Pass 1 — item #4) **package-deps.cjs Node fallback
  retired** (`8d95509a`): the #32 Rust-vs-Node resolver parity gate soaked green
  for the full 8-IG set, so the ~112-line retained Node reimplementation is
  deleted. `snapshot/package-deps.cjs` is now a pure shim over
  `rust_sushi resolve` (missing binary = clean FATAL, no fallback). The 8-IG
  `package-deps-gate.sh` is repurposed from "Node algo == Rust" to "shim stdout
  == direct `rust_sushi resolve` stdout" (shim-wiring regression). Gate: 8/8
  pass; harvest-r4-package.sh's shim call still emits the correct closure.
- (2026-07-03, Consolidation Pass 1 — item #5, CLOSED as documented) **Two
  file-abstraction traits — KEEP BOTH, justified.** `package_store::PackageSource`
  and `site_db::augment::FileSource` are NOT unified. They serve different layers
  with genuinely different surfaces: (a) error model — PackageSource returns
  `io::Result` (distinguishes IO errors on the package-cache read path);
  FileSource returns `Option` (missing == None, errors swallowed, right for
  best-effort site-content reads). (b) directory listing — PackageSource's
  `read_dir` is SINGLE-LEVEL with per-entry `is_file` (the version-resolvers +
  deep-scan need entry types); FileSource's `list_recursive` is RECURSIVE and
  returns flattened sorted POSIX rel-path strings (the S6 include-tree walk).
  (c) PackageSource additionally carries `write_new` (write-once derived-index
  sidecar) and a `Debug` supertrait (to live in `#[derive(Debug)]`
  PackageContext) — neither of which FileSource wants. Decisively: **site_db does
  not depend on package_store at all**; unifying would force a new cross-crate
  dependency edge purely to share a trait, then either widen one trait with
  methods the other's impls must stub or interpose an adapter — net MORE
  complexity for byte-identical behavior. Two small, purpose-fit traits at
  different layers is the simpler state. Item closed, no code change.
- (2026-07-03, Consolidation Pass 1 — item #6/#7 combined) **scripts/ index +
  docs current-state map** (`a8e39756` + this pass): `scripts/README.md`
  inventories every non-vendor script (snapshot/, scripts/, harness/, demo/*)
  with a one-liner + owner phase (verified complete); `harness/t1-dashboard.sh`
  deleted (private-dir 4-IG dashboard superseded by gate1.sh + full-dashboard.sh,
  referenced nowhere, stale hardcoded path, last touched 2026-06-30);
  md-diff-cluster.py KEPT (active F1b diagnostic touched today). `docs/README.md`
  maps every plan/spec/findings doc to its state; `wasm-editor-plan.md` gets a
  SUPERSEDED banner + inline P3/P4 markers pointing at stock-template-renderer-
  plan.md (F5/F6) + fhir-ig-editor-spec.md (P0/P1/P2 kept as DONE evidence).
- (2026-07-03, F3 close) **render_sd genTypes dedup**: grid.rs's
  `gen_types`/`gen_target_link` (branch-for-branch duplicates of table.rs's) and
  table.rs's `gen_types_erased`/`gen_types_inner_for_ext` (the lifetime-erased
  ext-value copy) BOTH collapsed into a shared `gentypes::TypesHost` trait
  (default methods, generic over the element lifetime `'e`; host supplies
  ctx/core_path/sd_root/gap/pointer/must_support_mode). grid = the
  dim=false/pointer=None/must_support_mode=false specialization; the ext-value
  path calls the trait directly (now MORE faithful — honors SDR:1402's ambient
  mustSupport). ~510 lines of duplicate removed for ~331 in gentypes.rs. Gate:
  all 19 kind×IG combos byte-identical (only the pre-existing cycle
  period-tracking-fact unstable-oracle differs, as before). NET simpler + one
  source of truth for the type cell.
- (2026-07-03) #12 clarity pass: CAS derived-index deleted three probing
  code paths; fetch Rc; dead-code sweep — the template for these passes.
- (2026-07-03) F0 interim byte-scans (perf pass) deleted same-day by the
  CAS index — accretion lived exactly one commit.
- (2026-07-04, task #39 template loader) **Reuse over reinvention**: the loader is
  ~470 LOC on top of the EXISTING `package_store` substrate — no registry client
  (acquisition delegates, as `TemplateManager` does to `pcm`), no new mount
  primitive (`PageProvider::with_template_includes` is one fallback branch;
  `Session.mountTemplate` reuses `site_files` merge). The scary ant/XSLT/plantuml
  surface was correctly NOT built: §3's 98%-copy finding held, so the whole
  compute layer collapsed to two merge rules + a JSON pretty-printer. The firm
  `AntHookError` line means we never grow an ant runner. F0 template snapshots
  demoted from runtime source → test fixture/oracle (kept: they're the free gate).

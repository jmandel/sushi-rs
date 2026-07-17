# Engine working agreement

Read `/home/jmandel/hobby/fhir-ig-editor/AGENTS.md` first. It is the normative
project decision and landing ledger. This file adds only engine-specific rules.

## Scope and state

- This checkout is the editor's vendored engine at
  `/home/jmandel/hobby/fhir-ig-editor/vendor/sushi-rs`.
- The clean remote base is `c3c9f881` on `snapshot-gen`/`main`.
- The deletion-first dirty tree is the authorized landing candidate, subject to
  exact rebuilt WASM/browser/Pages/live gates. Do not reset or broaden scope.
- The live deployed editor still lacks verified TOC/Artifacts outputs. The
  structural fix is reconstructed as an isolated producer slice and must pass
  the exact combined browser gate before this engine is pushed.

## Permanent architecture

The only public site flow is:

```text
prepare(ProjectRevision, GeneratorSpec) -> Build
Build.outputs()                        -> OutputCatalog
Build.render(path)                     -> ContentRef
Build.finalize()                       -> SiteOutput
```

The domain chain is `PreparedGuide -> SiteBuild -> SiteOutput`. Content-addressed
bytes live in `ContentStore`; assets are ordinary outputs. Publisher keeps
resolution/rendering internal. Cycle is a callback-free external builder over a
closed SiteBuild. Do not add staged public lifecycles, asset APIs, ambient
renderer state, mutable adapters, or another serialized build representation.

One successful engine transition is now:

```text
capture/resolve
  -> owned CompilationCandidate
  -> owned TargetCandidate
  -> close and verify all fallible content
  -> one infallible commit_success
```

Retain exactly three bounded current/previous histories: semantic compilation,
prepared target derivations, and installed runtimes. A miss does not refresh
recency. Failed candidates promote nothing.

## Accepted reconstruction slices

### Structural Publisher pages

`site_producer` owns one pure projection from PreparedGuide/template inputs to
one generic collision-checked produced-file catalog plus resource-page
subjects. TOC and Artifacts are ordinary generated pages/includes/data derived
from one shared structural model. SiteEngine must not know their names or paths.

### Package authority

Use one immutable `PackageEnvironment`: an ordered set of mounted labels,
authenticated PreparedPackage carriers, dependencies, and one BundleSource.
Derive views, carrier identities, lookup maps, and PackageLock from it. Mounting
constructs and validates an off-side successor. Do not mirror Rust-active
resolution/template state in the browser.

The package source may route non-root paths directly to their owning layer and
expose a non-following regular-file predicate. Preserve root union ordering,
missing/symlink/directory failure behavior, and live/restore identity parity.

### Snapshot derivation

Keep one opaque per-StructureDefinition `SnapshotDerivation`. Its structural
input identity, current envelope, exact positive/negative package reads,
authenticated immutable member proofs, generated snapshot, bounds, and
validation remain inseparable and private. Fresh and retained paths share the
same SD loader and snapshot installer. Unproven/mutable inputs read and parse
canonically. History is current/previous; overflow installs a tombstone; failed
builds do not promote.

Remove validation micrometrics and public fact/manifest/test accessor surfaces.
The overlapping whole snapshot-completed cache is deleted. Do not restore it.

### Publisher SQL

SQL is required Publisher behavior and uses actual bounded read-only SQLite in
native and WASM builds. It is not a Liquid implementation. Build one database
from the complete closed Publisher resource/dependency snapshot, expand SQL
directives deterministically during model closure, and hand ordinary rewritten
page/include/data files to rendering. Direct results are raw-isolated;
`sqlToData` is global and collision-checked. Unsupported/unpopulated
`package.db` tables fail explicitly. Require a pinned Java schema/query/result/
error oracle before claiming compatibility. Keep SQL separately reversible and
report its permanent WASM-size cost.

## Closed deletion/rewrite decisions

- Delete the compiler instance evaluator from the minimal stack. Preserve its
  experiment/oracle as reference; reconsider only as a later isolated compiler
  slice. Do not land its facts, manifests, clocks, or generation API now.
- Delete disabled production dependency-observation/page-replay plumbing.
  `INCREMENTAL_EXECUTION_ENABLED=false` and all-page Unknown evidence are not a
  product capability.
- Publisher preparation now owns one `TargetCandidate`; the canonical path
  closes and verifies before the one infallible `commit_success`.
- Delete the lazy `ValidatedPublisherOutputPlan`/`OnceCell` materialized catalog
  shape. Build one eager collision-checked path/descriptor/ready inventory;
  individual page bodies may remain lazy.
- Delete exact PreparedGuide and `closed_cycle` caches unless independently
  qualified. Runtime history owns exact target reuse.
- Move authenticated in-memory objects behind the content-store boundary rather
  than keeping a SiteEngine mini-store.
- Replace `PrepareMeasurements`, compiler/export microtimers, Publisher mounted-
  tree/artifact timing structs, and metric-key plumbing with a small generic set
  of stable spans and reuse counters.
- Delete test-only alternate prepare/compile routes. Tests exercise canonical
  candidates, close/verify, commit, restore, and failure recovery.
- Split the remaining SiteEngine facade by ownership, not by lifecycle layer:
  package environment, compilation, preparation generation, Publisher executor,
  external builder, runtime history, and restore codec. Moving tests alone is
  not an architectural split.

## Required evidence

Preserve focused unit tests and the frozen four-guide A -> B -> A differential
oracle for Tiny, IPS, US Core, and mCODE. Preserve failure atomicity, bounded
history/tombstone, live-vs-restore, reverse render order, every ContentRef/body,
final SiteOutput, real mCODE, US Core one-shell/assets, Service Worker restart,
scroll, and mobile gates. Move large tests out of production modules where
possible.

The current post-cleanup focused gates pass:

- SiteEngine: 51 passed, 1 fixture-dependent ignored;
- site_producer: 14 unit and 7 integration tests passed;
- publisher_sql: 13 passed; package_store: 56 + 2; snapshot_gen: 17 + 15;
- editor: 155/155 plus TypeScript; Cycle: 240/240 plus renderer typecheck;
- `cargo fmt --all` and `git diff --check` passed.

The exact current frozen four-guide differential passes at
`target/incremental-differential/landing-deletion-four-guide-20260717/aggregate.json`:
603/1,012/2,155/1,639 complete Tiny/IPS/US Core/mCODE outputs match fresh and
retained A -> B -> A in both render orders.

No rebuilt WASM, complete browser gate, or performance matrix is exact-current
after cleanup. Frozen earlier receipts are comparison evidence only.

Use Rust 1.96 with `wasm32-unknown-unknown`, wasm-bindgen 0.2.126, and Binaryen
117 as documented in the parent `AGENTS.md`. Prefer `cargo test -p <crate>
--release` for focused gates. Never infer deployment success from local tests;
Pages and fresh-profile live verification remain separate required boundaries.

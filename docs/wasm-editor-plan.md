# WASM / In-Browser FSH Editor — Plan

> Status: PLANNED (post-rework). Written 2026-07-02, while the snapshot-generator
> rework (snapshot/REWORK-PLAN.md) is in wave 3. Nothing here starts before that
> rework's wave-4 cutover completes — the walk engine is the snapshot generator
> this plan ships.

## 1. Goal

Run the full Rust toolchain — **rust_sushi (FSH → FHIR resources) + the walk
snapshot generator** — as WebAssembly in the browser, powering a fast FSH
editor: compile-on-keystroke, inline SUSHI-exact diagnostics, and live
**validation-grade snapshots**. Offline-capable once packages are cached.

Why this is worth doing:
- Native perf today: full IPS build 0.6–0.8s (vs stock SUSHI ~39s); snapshot
  generation ~10ms/profile (29 IPS profiles in 0.37s incl. process startup).
  At WASM's typical 1.5–3× penalty: **full-IG rebuild 1–2s, single-profile
  updates in low milliseconds** — a real-time editing loop.
- The incumbent (FSH Online) runs TS SUSHI (~40s-class builds) and **cannot
  produce snapshots at all** — snapshots require the Java publisher, which will
  never run in a browser. Both our engines are parity ports, so what the editor
  shows is byte-for-byte what CI will produce. "No surprises later" is the
  product.

## 2. Feasibility audit (done 2026-07-02)

The compute core is already WASM-clean:

| Concern | Status |
|---|---|
| Native/C deps in hot path | **None.** serde/serde_json/serde_yaml/indexmap/smallvec/rustc-hash/flate2/sha1/sha2/hex — all pure Rust, wasm-compatible. |
| Threads | None (single-threaded throughout; no rayon/tokio). |
| Allocator | mimalloc is CLI-binary-only already; wasm uses default. |
| Network | Only `package_acquisition` (registry downloads) — replaced by `fetch` in browser; not needed at compile time. |
| Filesystem | **The one real blocker.** `package_store`, snapshot `PackageContext`, and the CLI read the extracted package cache via `std::fs`. See §4.1. |
| Clock | A few `Instant::now` timing calls (`rust_sushi`, `package_acquisition`) — `wasm32-unknown-unknown` panics on these; feature-gate or shim. |
| JVM | Not needed at runtime anywhere. The Java oracle is development-time only. |
| Memory | R4 core + a US Core-sized closure is a few hundred MB parsed. Fine for desktop wasm32 (4 GiB ceiling); `PackageContext` already parses lazily per-fetch with memoization. Mobile needs further laziness — out of scope for v1. |

## 3. Target architecture

```
┌ Browser ────────────────────────────────────────────────────────┐
│  Editor UI (Monaco or CodeMirror, FSH grammar)                   │
│    │ debounced change events / hover / go-to-def                 │
│  Web Worker                                                      │
│    wasm module (rust_sushi + snapshot_gen behind wasm-bindgen)   │
│      compile(files, sushi-config) -> {resources[], diagnostics[]}│
│      generate_snapshot(sd_url | inline sd) -> snapshot + messages │
│    PackageSource (trait impl) ── OPFS/IndexedDB package cache    │
│         ▲ cold miss                                              │
└─────────┼────────────────────────────────────────────────────────┘
          │ fetch (one blob per package)
   CDN: prebuilt package bundles  {tarball + prebuilt .index/CAS index}
```

- All compilation in a Worker; the UI thread never blocks.
- Snapshots computed on demand per visible profile (memoized per URL, already
  implemented), full-IG rebuild debounced.
- Diagnostics use the port's SUSHI-exact wording + spans → straight to editor
  squiggles.

## 4. Work items

### 4.1 Storage abstraction (the blocker)
Two routes; do (a) first, keep (b) as the production shape:
1. **(a) Prototype via WASI**: build `wasm32-wasip1`, run under a browser WASI
   shim (e.g. `browser_wasi_shim`) with a virtual FS mounted from a package
   bundle. Near-zero code change; proves the experience end-to-end in days.
2. **(b) `PackageSource` trait**: minimal surface (`read(path) -> bytes`,
   `list(dir)`, `exists`) behind `package_store::PackageStore` and
   `snapshot_gen`'s `PackageContext`. Native impl = disk (unchanged behavior,
   re-run full corpus gates to prove it); browser impl = OPFS with IndexedDB
   fallback. This also cleans up the multiple ad-hoc `std::fs` sites.

### 4.2 Package delivery
- `packages.fhir.org` has no CORS → do NOT depend on it directly.
- Ship **prebuilt package bundles** on a CDN: `{package.tgz + prebuilt index}`
  as one blob. Main already has CAS-cached generated package indexes
  (`package_acquisition`) — reuse that format so cold-start = one fetch +
  inflate (flate2/tar already in-tree and wasm-fine).
- Bundle manifest pins exact versions (the editor's "lockfile"); resolution UI
  can come later. Optional small CORS proxy for arbitrary registry packages.

### 4.3 Crate/API packaging
- New crate `wasm_api` (wasm-bindgen): owns the JS surface, keeps bindgen out
  of the core crates. API sketch:
  - `init(package_source)`
  - `compile(files: Map<path, text>, config: text) -> { resources, diagnostics, timings }`
  - `generate_snapshot(profile_url) -> { snapshot, messages }`
  - `invalidate(paths[])` (hook for later incrementality)
- Feature-gate `Instant::now` sites (`web-time` or cfg shims); confirm
  `mimalloc` stays out of wasm builds.
- Binary-size pass at the end (`wasm-opt`, `panic=abort`, strip serde_yaml if
  config parsing moves JS-side). Expect 2–5 MB gz; not a v1 concern.

### 4.4 Editor shell (v1 scope)
- Monaco/CodeMirror + FSH syntax; worker protocol; diagnostics panel + inline
  squiggles with SUSHI wording; tabbed view per profile: **differential |
  snapshot | rendered tree**; status line with build timings.
- Golden demo: load IPS, edit a profile, watch differential + snapshot update
  live, offline after first load.

### 4.5 Incrementality (v2, explicitly deferred)
Debounced full rebuild (1–2s) is acceptable for v1. The architecture already
invites incrementality later: per-file parser, per-profile snapshot generation
memoized per URL, deterministic pipeline. V2 = dependency-aware invalidation
(file → entities → dependent profiles → dependent snapshots). Do not build
this speculatively.

### 4.6 Parity in the browser (do not skip)
The browser build must pass the same gates as native:
- A headless harness (node + wasm, or wasmtime for the WASI build) running the
  fixture ladder + at least IPS/mcode/sdc corpus gates **against the wasm
  binary**. Byte parity native↔wasm is expected (same code, serde_json is
  deterministic) — prove it once in CI rather than assume it.

## 5. Phases

| Phase | Deliverable | Exit gate | Rough effort |
|---|---|---|---|
| P0 | WASI prototype: compile IPS + generate one snapshot in a browser page | IPS build < 3s in-browser; output byte-matches native | days |
| P1 | `PackageSource` trait + OPFS impl + CDN bundle format | full native corpus gates green over the trait; browser cold-start < 5s for IPS closure | ~1 wk |
| P2 | `wasm_api` crate + worker protocol + wasm parity harness in CI | ladder + 3-IG gates green on wasm build | ~1 wk |
| P3 | Editor shell v1 (Monaco, diagnostics, diff/snapshot views, IPS demo) | golden demo usable end-to-end, offline-capable | 1–2 wk |
| P4 | Polish: bundle size, more packages on CDN, error UX, share links | public demo | open-ended |

## 6. Risks / open questions

- **Corpus gates over the storage trait** (P1) is the riskiest refactor — it
  touches the same load path the snapshot rework just proved correct
  (lenient-R5-parse vs convert split in `resolve.rs`). Mitigation: land it
  AFTER wave-4 cutover, gate with the full ~40-IG scorecard.
- **Package licensing/hosting**: confirm redistribution of hl7.* packages via
  our CDN is acceptable (they're open, but verify terms; FSH Online precedent
  exists).
- **Memory on big closures** (QI-Core-scale): measure in P0; if needed, evict
  parsed-SD memos LRU-style — mechanical.
- **serde_yaml** is unmaintained upstream; fine for now, note for later
  (config parsing could move to the JS side or `serde_yml`).
- Editor product questions (multi-file projects, persistence, share links,
  GoFSH integration for round-tripping) — deliberately out of scope here.

## 7. Sequencing vs the snapshot rework

Hard dependency: **wave-4 cutover first** (walk engine default, legacy
deleted). P0 may start any time after, using whatever engine is default — but
P1's storage-trait refactor must be gated by the post-cutover full-corpus
scorecard, so do not interleave P1 with wave 3/4.

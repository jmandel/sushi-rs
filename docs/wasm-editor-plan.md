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
| P3 | Editor shell v1 — now specced as its own repo `jmandel/fhir-ig-editor` (see docs/fhir-ig-editor-spec.md; default IG = cycle, submodule-pinned engine, GitHub Pages) | that spec's M1 gate | 1–2 wk |
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

Hard dependency chain (user-confirmed 2026-07-02, tracked as tasks #11–#13):
**wave-4 cutover → merge latest origin/main into snapshot-gen → perf+clarity
review pass (sushi + snapshot generator) → THEN P0 here.** P1's storage-trait
refactor must additionally be gated by the post-cutover full-corpus
scorecard. Do not interleave any of this with wave 3/4.

## 9. P0 status — DONE (2026-07-03)

**Verdict: P0 gate PASS.** Both binaries compile to `wasm32-wasip1` and run in a
real browser (Chromium 148, headless), compiling the cycle IG and generating
snapshots whose output is **byte-identical to native**. Spike lives in
`demo/wasm-p0/` (page + vendored `@bjorn3/browser_wasi_shim` 0.4.2 + a
`prepare.sh` data script + a `check-native.sh` byte-match gate; heavy data
gitignored, prepared by script).

**Code changes (the only ones — both native-inert):**
- `crates/rust_sushi`: **mimalloc cfg-gated to `not(target_family="wasm")`**
  (Cargo.toml target-dep + `#[global_allocator]`). mimalloc is a C dep with no
  wasm libc (`wchar.h`); wasm uses the default allocator. Native keeps mimalloc
  (verified: 343 `mi_*` symbols still in the native binary).
- `snapshot_gen`: **zero changes.** No clock/`Instant` shim needed — wasip1 has
  a WASI wall-clock, and the only `std::time` sites are `#[cfg(test)]` or in
  `package_acquisition` (not linked into the WASI runtime path).

**Native gates re-run green with the change in place:** workspace `cargo test`
(all suites ok); SUSHI-harvest gate **326/326 cases, 256/256 byte-identical**;
native `rust_sushi build` of IPS = **118 resources**.

**Binary sizes** (release, no wasm-opt/strip pass yet):

| binary | native | wasm32-wasip1 |
|---|---|---|
| rust_sushi | 5.72 MB | 4.36 MB |
| snapshot_gen | 1.22 MB | 0.86 MB |

**Timings — cycle IG (7 profiles), single-threaded:**

| step | native (per-process) | wasmtime 46 | Chromium 148 (in-tab) |
|---|---|---|---|
| full IG build | ~45 ms | ~99 ms | 129 ms |
| snapshot / profile | ~18 ms | ~32 ms | ~21 ms |
| **compute total (build + 7 snaps)** | ~0.17 s | ~0.32 s | **0.27 s** |

Browser wasm ≈ 1.5–2× native — matches the plan's estimate. **0.27 s ≪ 3 s
gate.** (Browser wall-clock adds ~80 ms to fetch the 9 MB virtual FS + ~27 ms to
compile both modules — one-time, cacheable.)

**Byte-match gate:** browser exports `wasm-hashes.json` (SHA-256 of all 18
outputs = 11 build resources + 7 snapshots); `check-native.sh` regenerates the
native manifest and diffs → **18/18 identical, 0 mismatches**. Cross-checked
under wasmtime 46 and Node `node:wasi` (also byte-identical) so the parity is in
the code, not one shim.

**Minimal package cache:** the store reads `.index.json`/`.derived-index.json`
eagerly and resource bodies lazily; `prepare.sh` straces a native run and ships
only the fished set — **~8 MB** vs the ~230 MB full closure.

**One real gotcha (documented in the demo README):** package dir names contain
`#` (`hl7.fhir.r4.core#4.0.1`); a raw `#` in a `fetch` URL is a fragment
delimiter, so the browser 404'd on every package file until `app.js`
`encodeURIComponent`-ed path segments. Invisible under wasmtime/node (direct
disk reads).

**Toolchain note (Arch):** the system rustc can't build wasip1 std and Arch's
target-std is ABI-incompatible with upstream prebuilt std (same commit, patched
metadata). Use a rustup-managed **upstream** toolchain matching the rustc
version + `wasm32-wasip1`. See `demo/wasm-p0/README.md`.

**Shortest path for P1/P2 (what this proved is safe to build next):**
- **P1 — `PackageSource` trait (the blocker):** replace the ~5 `std::fs` sites in
  `package_store` (`.index.json` read, `derived_index::load`, the two
  `resolve_latest`/`resolve_minor_wildcard` readdir version-resolvers, the
  deep-scan `read_dir` fallback) + `snapshot_gen`'s `PackageContext` with a
  `read(path)/list(dir)/exists` trait. Native impl = today's disk code
  (re-run full corpus gates over it). Browser impl = OPFS/IndexedDB. Note the
  lazy-read pattern already makes cold-start cheap; keep it.
- **P1 — CDN bundle format:** ship `{tarball + .index.json + .derived-index.json}`
  per package (reuse the CAS index already in `package_acquisition`); pin exact
  versions in a manifest lockfile. Cold-start = one fetch + inflate.
- **P2 — `wasm_api` bindgen crate + worker protocol:** move argv/FS marshalling
  (currently the throwaway `app.js`) into a typed `compile()/generate_snapshot()`
  surface behind wasm-bindgen; run in a Web Worker. Then port the byte-match gate
  (this demo's `check-native.sh` diff) into CI against the wasm build.
- **Deferred, non-blocking:** binary-size pass (`wasm-opt`, strip) — 4.4 MB is
  already fine for a spike; the WASI shim can be dropped entirely once the
  `PackageSource` route (b) replaces the WASI route (a).

## 9b. P1 status — storage trait + bundles DONE (2026-07-03)

**Verdict: P1 gate PASS.** The WASI-shim route (P0's route (a)) is replaced by the
production storage shape (route (b)): a `PackageSource` trait threaded through the
entire package read path, with a `DiskSource` (native, byte-for-byte unchanged)
and a read-only in-memory `BundleSource` (the browser's mount). No browser wiring
built — that's P2.

**Trait surface** (`crates/package_store/src/source.rs`, the lowest shared crate):
`PackageSource: Debug` with `read(&Path)->io::Result<Vec<u8>>`,
`read_dir(&Path)->io::Result<Vec<DirEntry{file_name,is_file}>>`, `exists`,
`is_dir`, and `write_new(&Path,&[u8])` (write-once; default = read-only Err, so
read-only sources fail-soft to an in-memory derive per the derived-index design).
`DiskSource` is the only `std::fs` site; it reproduces the old atomic write-once
sidecar semantics exactly.

**Call-site inventory (before → after):**
- `package_store::lib.rs`: `.index.json` read, deep-scan `read_dir`+`fs::read`
  fallback, the two version-resolvers (`resolve_latest`,
  `resolve_minor_wildcard`), the cache `is_dir` guard, and the lazy `read_value`
  resource read — all `std::fs::*` → `source.*`. `PackageStore` now holds a
  `Box<dyn PackageSource>` for the lazy read.
- `package_store::derived_index.rs`: `resource_filenames` readdir, `build`
  per-file reads, `load` sidecar read + write-once — all → the source; `build`/
  `load` now take `&dyn PackageSource`.
- `snapshot_gen::package.rs` (`PackageContext`): the `.index.json` SD-probe, the
  derived-index `load`, the package-dir `is_dir` guard, and the lazy `fetch`
  read — all → `self.source`. (`load_local_dir` stays `std::fs`: it reads the
  native IG project, not the mounted cache.)
- `package_acquisition`: `derived_index::build(dir)` → `build(&DiskSource, dir)`.
- **Native constructors unchanged:** `PackageStore::for_project(ig, cache)` and
  `PackageContext::new(cache, pkgs)` keep their signatures and construct a
  `DiskSource` internally (zero behavior change). New source-taking variants:
  `for_project_with`, `new_with`, `package_resource_entries_with`.
  Residual non-source `std::fs` in the read path: only `parse_config`'s read of
  the IG `sushi-config.yaml` (native project file, not the package cache — the
  browser feeds config JS-side per §4.3). → P2.

**Bundle format + builder** (doc: `docs/package-derived-index.md` §"PackageSource
trait + browser bundles"): per-package gzipped-tar of the `package/` top-level
files (resource JSONs + `.index.json` + guaranteed `.derived-index.json`
sidecar), plus a `bundle-manifest.json` lockfile
(`package_store::BundleManifest`). Builder = `package_acquisition::build_bundle` /
`read_bundle` / `build_bundle_set` + `rust_sushi bundle --cache … --out … <id#ver>…`.
`package_store::BundleSource` is the read-only in-memory `PackageSource` that
mounts them (synthetic cache root; `flate2`/`tar` are wasm-clean).

**Gate results (verbatim):**
- **Full snapshot corpus over the trait path — 955/955, all 34 IGs at §9
  scorecard counts, failed=0:** ips 29/29, mcode 46/46, genomics 33/33, crd
  22/22, sdc 73/73, carinbb 6/6, dtr 21/21, ecr 28/28, ndh 50/50, pas 73/73, mhd
  42/42, eu-eps 23/23, eu-mpd 4/4, au-ps 17/17, pacio-toc 4/4, dapl 26/26,
  us-core 70/70, ipa 12/12, qicore 63/63, pddi 1/1, deid 1/1, darts 1/1,
  radiation-dose-summary 4/4, be-vaccination 7/7, smart-app-launch 6/6, cdex 8/8,
  plan-net 22/22, pdex 37/37, drug-formulary 19/19, subscriptions-backport 9/9,
  twpas 43/43, davinci-pas 80/80, gematik-epa-medication 49/49, au-core 26/26.
- **`cargo test --workspace` — 68 passed, 0 failed** (excludes the pre-existing,
  unrelated `site_db` WIP crate, which has no `lib.rs`/`main.rs` and cannot
  compile independent of this work).
- **Sushi native IPS build — 118 resources, byte-identical before/after** the
  trait refactor (aggregate SHA-256 `8c4de17a…` unchanged; the compiler links
  `package_store`).
- **BundleSource fixture ladder — green** (`snapshot_gen/tests/bundle_ladder.rs`):
  builds r4/r5 core bundles from the isolated cache, round-trips
  `build_bundle`→`read_bundle`→`mount_package`, runs all 17 ladder rungs through a
  `BundleSource`-backed `PackageContext` to the same goldens as disk.

**Open items for P2:** `wasm_api` bindgen crate (`init(source)` /
`compile` / `generate_snapshot`) owning the JS surface; a wasm `PackageSource`
impl over OPFS/IndexedDB (the `BundleSource` map can back it directly); moving
`sushi-config.yaml` parsing JS-side (or feeding config bytes) so the last
read-path `std::fs` leaves the wasm build; the Web Worker protocol; and porting
the byte-match parity gate (P0's `check-native.sh`) into a CI harness that runs
the fixture ladder + a few corpus IGs against the wasm binary.

## 9c. P2 status — wasm_api + worker protocol + parity harness DONE (2026-07-03)

**Verdict: P2 gate PASS.** The production JS surface (`wasm_api`, wasm-bindgen)
runs the compiler + walk snapshot engine on `wasm32-unknown-unknown` — the WASI
route (P0) is fully retired. The 17-rung ladder + ips/mcode/sdc corpus gates pass
**against the wasm build** to the same goldens the native gates use.

**`wasm_api` surface** (`crates/wasm_api/src/lib.rs`, cdylib+rlib; bindgen stays
out of the core crates):
- `init(bundles_json) -> u32` — mount prebuilt package bundles into an in-memory
  `BundleSource` (JSON `[{label, files:{name:base64}}]`; the browser fetches each
  `.tgz`, inflates it, base64s the bytes). Returns packages mounted.
- `compile(files_json, config, predefined_json) -> {resources, diagnostics,
  timings}` — runs the compiler **in-memory** over a `{path: text}` FSH map + the
  `sushi-config.yaml` TEXT + a `{path: json}` predefined-resource map. `resources`
  carry the byte-identical SUSHI output + `{resourceType,id,url}` for the editor's
  views. Stashes the outputs as local SDs for snapshot base resolution.
- `set_local_resources(json) -> u32` — the in-memory `--local-dir` (the parity
  harness / an editor loads a corpus's sibling SD set this way).
- `generate_snapshot(input) -> {snapshot, messages}` — `input` is an inline SD
  JSON or a canonical URL/id/name resolved against the last compile + the mounted
  packages; runs the walk engine (R5-internal output).
- `version() -> {version, commit, engine}` (commit from `WASM_API_GIT_COMMIT`).
- `console_error_panic_hook` on wasm. No clock/`Instant` needed (the walk +
  compiler have no runtime time calls); timings are measured JS-side at the call
  boundary. `mimalloc` never enters this crate (it depends on the libs, not the
  `rust_sushi` bin).

**Compiler-crate additions (in-memory entry point) — same code path, proven
equivalent:**
- `compiler::compile_conformance(cfg_text, fsh_refs, &store, &predefined)` — the
  compute core (import → global insert-rule expansion → export VS/CS, SD,
  Instances in `FHIRExporter` order, serialized byte-identically) with **zero
  `std::fs`** and no IG-project/cache path assumptions; every package read flows
  through the `PackageStore`'s `PackageSource`. The disk `build_project_inner`
  was refactored to gather its inputs then call this SAME function (not a fork).
- `compiler::build_project_in_memory(cfg_text, fsh_files, predefined, source,
  cache_dir)` — the wasm entry point: builds the store via a new
  `PackageStore::for_project_with_config` (resolves deps from config TEXT, not a
  disk read — the last read-path `std::fs`, per §4.3) + `PredefinedPackage::
  load_from` (in-memory predefined), then `compile_conformance`. Returns the
  conformance resources; the **ImplementationGuide resource is disk-only** and
  excluded (it scans `input/pagecontent` + the cache dir for depends-on via
  `std::fs` beyond the package-cache `PackageSource` boundary — not needed by the
  editor's M1 views; abstracting those scans is deferred).
- `snapshot_gen::PackageContext::load_local_resources(entries)` — in-memory
  `load_local_dir` (parsed `(path, body)` SDs; bodies stashed so `fetch` serves
  them without a source read). `package_store::parse_config_text`.
- **Equivalence proof:** `crates/compiler/tests/compile_equiv.rs` runs the cycle
  IG + 6 harvest mini-IGs through BOTH `build_project_with_cache` (disk) and
  `build_project_in_memory` and asserts every non-IG resource is byte-for-byte
  identical. Green.

**Bundle sizes** (wasm-bindgen `nodejs`/`web` target, release; no `wasm-opt`
available on this box — a `-Oz` pass is wired in both scripts and would cut this
further; gzip on the wire is ~⅓):

| artifact | size |
|---|---|
| raw `wasm_api.wasm` (cargo) | 2.6 MB |
| wasm-bindgen `_bg.wasm` | 2.5 MB |

**Worker demo** (`demo/wasm-p2/`, data gitignored, `prepare.sh` assembles it):
a plain page + Web Worker (no framework) mounting the cycle IG's 5-package
closure, compiling in-memory, and snapshotting every profile. Timings (headless
Node driver, nodejs target — the browser uses the identical wasm + surface):

| step | cycle IG |
|---|---|
| init (fetch + inflate + mount 5 bundles, cold) | ~4.0 s (16 MB full bundles, base64 in JS — one-time) |
| compile (4 FSH → 10 resources) | ~89 ms |
| snapshot / profile | ~38–50 ms |
| **compute total (compile + 7 snapshots)** | **~0.37 s** |

(Matches P0's ~0.27–0.32 s. The init cost is bundle-inflation of the *full*
package closure, not the P0 strace-minimized subset; OPFS caching + lazy
per-package fetch removes it after first load — that's the editor repo's job.)

**Parity harness — `bash scripts/wasm-parity.sh`** (§4.6, byte parity
native↔wasm proven): builds the wasm module, runs `wasm-bindgen`, builds the
package bundles via `rust_sushi bundle`, then a Node driver
(`scripts/wasm-parity-driver.mjs`) runs the ladder + 3 corpus gates against the
wasm module, comparing `snapshot.element` to the native goldens. **Verbatim:**

```
engine: {"version":"0.1.0","commit":"6957654","engine":"rust_sushi + snapshot_gen (walk)"}
PASS  ladder: 17/17 (expected 17)
PASS  ips: 29/29 (expected 29)
PASS  mcode: 46/46 (expected 46)
PASS  sdc: 73/73 (expected 73)

WASM PARITY GATE: PASS
```

(The driver mirrors `check-harvested-r4.sh` exactly: separate r4-core/r5-core
contexts per ladder group; the corpus `--local-dir` = the manifest's
`resourcesDir` full `fsh-generated/resources` set, else `fixtures/`.)

**Native gates re-run green with all P2 changes in place:**
- `cargo test --workspace` — **75 passed, 0 failed** (incl. new `compile_equiv`;
  `wasm_api` links + tests native).
- SUSHI-harvest gate — **326/326 cases, 256/256 byte-identical, 0 diffs**.
- Sushi IPS build — **118 resources**, aggregate SHA-256 `8c4de17a…`
  (unchanged from §9b — the disk build is byte-identical after the refactor).
- Native snapshot corpus — ips 29/29, mcode 46/46, sdc 73/73, plus the P1
  BundleSource fixture ladder.

**Toolchain (Arch):** `wasm32-unknown-unknown` std ships prebuilt (simpler than
wasip1 — no custom sysroot). Use a rustup-managed upstream toolchain matching the
repo rustc + `rustup target add wasm32-unknown-unknown`, and
`cargo install wasm-bindgen-cli --version <crate ver>` into a scratch root. The
scripts honor `WASM_RUSTUP_HOME` / `WASM_CARGO_HOME` / `WASM_BINDGEN` overrides.

**What remains for the editor repo (#16, `fhir-ig-editor`) to consume this:**
- A wasm `PackageSource` over **OPFS/IndexedDB** for lazy per-package cold-start
  (the `BundleSource` map backs it directly; kills the ~4 s full-inflate init).
- **Monaco** + FSH grammar, file tree, the worker protocol wiring
  (`init`/`compile`/`set_local_resources`/`generate_snapshot` are the seam — the
  demo worker is the reference implementation), and the M1 views (JSON /
  differential / **snapshot tree** / diagnostics / timings).
- `compile` currently returns `diagnostics: []` — SUSHI-exact diagnostic
  wording+spans exist in the compiler but are not yet threaded through
  `compile_conformance`'s return; wiring them to the JS surface (→ Monaco markers)
  is a small follow-up the editor will want.
- The **ImplementationGuide resource** + site-preview (M2) need the IG-project/
  cache-dir FS scans abstracted (deferred here); the site.db producer (#15) is
  the other half of M2.

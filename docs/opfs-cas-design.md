# Unified content-addressed package store (CAS) — OPFS + native, derived-index persistence (design)

> Design investigation for task #41. READ-ONLY: this doc proposes work for a
> follow-up agent to implement after the current editor cache/resolver branch
> merges. It touches TWO repos — `fhir-ig-editor` (host/OPFS, the bulk) and
> `sushi-rs-snapshot` (the wasm seam + the native `CasSource` — gated commits).
>
> Goal (Josh): make the browser's OPFS package cache a content-addressed store
> (CAS) "just like local rust" — mirror the native engine's **CAS + derived-index**
> model so warm opens stop re-deriving the per-package metadata index. **Folded in
> (Josh, 2026-07-05): the native/`fig` side is the same store.** fig should work
> DIRECTLY from CAS — no `id#version/package/` folder materialization required —
> so there is ONE `CasSource` concept, disk-blob-backed natively and OPFS-blob-
> backed in wasm; the only difference between the two is the blob backend. See §9.

---

## 1. The native model (what we are mirroring)

### 1.1 What the derived index IS

A **derived-columns index**: one row per resource file in a package, carrying the
metadata columns the read path needs but the stock `.index.json` does not —
notably `name` and `baseDefinition`
(`docs/package-derived-index.md` lines 28-66; `crates/package_store/src/derived_index.rs:34-58`).
Ten columns are lifted verbatim from each resource's root object:
`filename, resourceType, id, url, version, kind, type, derivation, baseDefinition, name`
(`derived_index.rs:94-108`). The row set is the FPL `getPotentialResourcePaths`
set — every top-level `^[^.].*\.json$` except `package.json`, sorted — so it also
covers packages whose stock `.index.json` is empty/SD-less
(`derived_index.rs:68-92`).

**What it costs to compute:** exactly one `serde_json::from_slice` of every
resource file in the package, once, lifting ten root strings
(`derived_index.rs:114-133`). The design note calls this "the only full parse of
the corpus that survives, and it happens once per content hash, ever"
(`package-derived-index.md:63-66`). This is the whole point: the parse is
immutable-content-derived, so it should happen **once per package content, not
once per process** (`package-derived-index.md:24-27`).

### 1.2 How it is keyed

By **package content sha256 + `DERIVED_INDEX_FORMAT_VERSION`**
(`package-derived-index.md:30-40`). The format version is a plain integer constant
(`derived_index.rs:23`, currently `1`), deliberately distinct from the stock
`.index.json` `index-version`. Bumping it changes the artifact **filename**
(`cas_artifact_name()`, `derived_index.rs:29-32` → `derived-index-v1.json`), so a
stale artifact is simply never read again — entries are never mutated in place; a
bump invalidates by key (`derived_index.rs:19-23`). The on-disk JSON self-declares
its version (`derived-index-version`) and `parse()` rejects a mismatch as "absent"
so the caller rebuilds (`derived_index.rs:60-66, 144-150`).

Note the version is **content + format** only — NOT engine commit. The derived
index is pure content extraction; any engine build produces byte-identical rows
from the same content. (This matters for the OPFS key — see §4.3.)

### 1.3 WHEN computed vs consumed

- **Computed once at ingest / CAS population** (`ingest_artifact_bytes`),
  alongside the generated `.index.json` — never at process start, never per
  materialize (`package-derived-index.md:28-33, 69-73`). Stored at
  `<cas>/packages/<sha256>/derived/derived-index-v<FORMAT>.json`.
- **Consumed at materialize / load**: materialization only **hardlinks it out**
  as the sidecar `<cache>/<name>#<ver>/package/.derived-index.json`
  (`package-derived-index.md:74-82`). The consumer
  (`snapshot_gen::PackageContext::load_package`) reads the sidecar instead of
  parsing `.index.json` + `probe_name` per SD + `scan_package_structure_definitions`
  (`crates/snapshot_gen/src/package.rs:133-141`; `package-derived-index.md:110-121`).

### 1.4 The sidecar + write-once fallback (the seam that matters for wasm)

Reads no longer touch `std::fs` directly — everything goes through the
`PackageSource` trait: `read`/`read_dir`/`exists`/`is_dir`/**`write_new`**
(`crates/package_store/src/source.rs:48-75`). `write_new` is the write-once
sidecar seam: writable sources (`DiskSource`) create it atomically; **read-only
sources return `Err` and the caller fails soft** (`source.rs:66-74, 137-150`).

`derived_index::load` encodes the whole ladder (`derived_index.rs:152-176`):
1. sidecar present + current → read it;
2. else build from content, `write_new` it once, return rows;
3. if `write_new` fails (read-only dir) → still return the freshly-built rows
   (correct data, pay the one-build cost). **Fail-loud-safe: never wrong, never
   errors.**

For **plain / non-CAS caches** (extracted dirs, the isolated test cache with no
reachable CAS handle) this write-once-on-first-need path is how the sidecar gets
created (`package-derived-index.md:89-101`). One shared builder
(`derived_index::build`) produces byte-identical output for the CAS-hardlinked and
the write-once sidecar.

**Native win (measured):** the CAS derived-index removed the residual load-path
cost — davinci-pas 1.77 s → 0.80-0.95 s (~−48%), us-core −31%, qicore −44%
(`docs/perf-snapshot-gen.md:296-304`). That is native disk IO + parse; in wasm the
IO is memory but the **parse to build the index is the same CPU work** (see §5).

---

## 2. Does the wasm path expose the derived index?

**Partly — and there is NO host-readable seam today.**

### 2.1 The bundle IS the wasm-side source

The browser mounts a **`BundleSource`** — a read-only in-memory `PackageSource`
(`crates/package_store/src/bundle.rs:92-222`). A *package bundle* is the byte
content of one materialized `package/` dir: every resource JSON **plus
`.index.json` and the `.derived-index.json` sidecar** (`bundle.rs:9-24`).

- **Baked bundles carry the sidecar.** `package_acquisition::build_bundle`
  guarantees it — if the materialized dir lacks it, it derives it with the shared
  `derived_index::build` before packing (`crates/package_acquisition/src/lib.rs:1534-1552`;
  `package-derived-index.md:149-159`). So for baked bundles the sidecar is in the
  tgz → in the mounted `BundleSource` → `derived_index::load` reads it (cheap parse
  of the ~1 MB sidecar, not the ~38 MB corpus).

- **Registry tarballs do NOT carry it.** A raw npm registry `.tgz` roots files
  under `package/` with **no `.derived-index.json`**; the editor's inflate strips
  the prefix and the engine "derives the `.derived-index.json` sidecar in-memory"
  (`fhir-ig-editor/app/src/worker/packageResolver.ts:288-296`;
  `package_acquisition/src/lib.rs:2056-2088`). That in-memory derive is
  `derived_index::load` → `build()` (parse every file) on **every**
  `PackageContext::new_with`, and…

- **`BundleSource` is read-only**, so `write_new` returns the default `Err`
  (`bundle.rs:219-221`; `wasm_api/src/lib.rs:86`). The freshly-built rows are used
  but **never cached** — not even within the session. A new `PackageContext` is
  built on every `snapshot()` (`wasm_api/src/lib.rs:290-296, 301`), every
  `build_site_db`, and every render-state rebuild
  (`snapshot_complete_own`, `lib.rs:1074-1092`), each dropped on any state change.
  So for a registry-sourced closure the full-corpus parse repeats **per compile /
  per snapshot / per render**, not just per open.

### 2.2 Can `PackageSource::write_new` persist a sidecar in the bundle world?

Not as written — `BundleSource` takes the read-only default (`bundle.rs:219-221`).
It *could*: give `BundleSource` an interior-mutable `write_new` (e.g.
`RefCell<BTreeMap>` or `Rc<RefCell<…>>`) that inserts into its `files` map. Then
the first `PackageContext` build writes the sidecar into the mounted source and
every later build in the same session reads it — a within-session win with zero
host involvement. It does **not** by itself survive a reload (the `BundleSource`
is rebuilt from the OPFS bundle each cold start).

### 2.3 What Session surface would let the HOST read + re-supply it?

**There is none today.** No `Session` method returns the derived index bytes; the
sidecar only ever lives inside the transient `BundleSource`. `Session.version()`
returns `{version, commit, engine, apiVersion}` (`lib.rs:882-890`) — it does not
even expose `DERIVED_INDEX_FORMAT_VERSION`.

The DRY seam to add (mirrors the package-fetch seam: **Rust computes/decides, host
persists/transports** — `packageResolver.ts:1-18`): a call that, for the mounted
packages, hands the host the engine-computed sidecar bytes so the host can persist
them to OPFS and feed them back on warm start. Concrete options in §4.4.

---

## 3. Current OPFS caching in the editor

| store | file | key | contents | derived index? |
|---|---|---|---|---|
| inflated bundles | `worker/bundleCache.ts` | `v1__<label>` (label only, `CACHE_VERSION='v1'`, `bundleCache.ts:14-21`) | `{label, files:{name→base64}}` — the whole inflated package (`bundleCache.ts:34-60`) | **only if present in the tgz** — baked yes, registry no |
| materialized template trees | `worker/templateTreeCache.ts` | `v1__<engineCommit>__<coord>__<treeHash>` (`templateTreeCache.ts:43-46`) | the `mountSite`-shaped tree | n/a — this is the **derived-artifact-persistence precedent** (`templateTreeCache.ts:1-15`) |
| project VFS | `vfs/store.ts` | flattened path key in one OPFS dir (`store.ts:32-37, 71-120`) | working IG files | n/a |

The acquisition ladder (`packageResolver.ts:197-243`, `obtainPackage`) is
OPFS-warm → local drop → baked → registry; every branch `writeCachedBundle`s the
result (`packageResolver.ts:206-235`). `client.ts:113-142` (`loadBundle`) is the
same pattern for baked bundles.

**What is cached:** the inflated bundle (label-keyed, whole copy).
**What is NOT cached:** the derived index for registry-sourced packages. When a
registry bundle is stored (`{label, files}` with no sidecar,
`packageResolver.ts:293-296`), warm start re-mounts it **still without the
sidecar** → engine re-derives every time, forever.

**OPFS mechanics to reuse:** `navigator.storage.getDirectory()` +
`getDirectoryHandle({create})` + per-file `getFileHandle`/`createWritable`,
degrade-to-null on unavailable (`bundleCache.ts:24-31, 49-60`); filesystem-safe
label encoding `'/'→'∕' '#'→'＃'` (`bundleCache.ts:19-21`,
`templateTreeCache.ts:44`); Web-Crypto `sha256Hex` + canonical-JSON hashing
(`templateTreeCache.ts:24-36`); manifest-file indirection to point a stable key at
a content-hashed entry (`templateTreeCache.ts:57-104`).

---

## 4. The OPFS-CAS design

Two independent pieces. The **derived-index store is the prize**; the
**content-addressed blob store is a marginal add-on** (§6 quantifies this).

### 4.1 Layout

```
<opfs>/fhir-ig-editor-cas/
  blobs/<sha256>                     # inflated package content, stored ONCE (§4.2)
  labels/<encoded-label>.json        # { sha256, bundleFormatVersion } — label → content (§4.2)
  derived/<sha256>/idx-v<FORMAT>.json # the engine-computed derived index, keyed by
                                      # content hash + DERIVED_INDEX_FORMAT_VERSION (§4.3)
```

Mirrors native `<cas>/packages/<sha256>/derived/derived-index-v<FORMAT>.json`
(`package-derived-index.md:33-35`) — same content-hash + format-version key,
same "bump the filename to invalidate" rule.

### 4.2 Content-addressed blob store (label → hash → bytes)

- **Blob key = sha256 of the package's canonical content.** Cheapest correct
  source: the `.tgz` bytes' sha256 — the `BundleManifest` already carries
  `sha256` per baked bundle (`bundle.rs:39-51`); for registry/local tarballs the
  host computes it with Web Crypto (`crypto.subtle.digest`, already used in
  `templateTreeCache.ts:24-28`). Same tgz ⇒ same inflated files ⇒ same derived
  index, so the tgz sha is a sound content key.
- **`labels/<label> → {sha256}`** is the indirection that yields dedup: two labels
  (e.g. an alias and its target, or a re-pin) resolving to identical bytes share
  one blob and one derived index.
- Store the inflated `{name→base64}` map (today's `BundleSpec.files`) as the blob
  body, so mounting is unchanged.

### 4.3 Derived-index store (the prize)

- **Key: `(content sha256, DERIVED_INDEX_FORMAT_VERSION)`** — NOT engine commit.
  The derived index is pure content extraction (§1.2), so it is reusable across
  engine bumps as long as the format version is unchanged. This is *stronger* than
  `templateTreeCache`'s engine-commit key (which is right there because rendered
  HTML depends on engine code — the derived index does not).
- **Value: the exact `.derived-index.json` bytes** the engine would put in the
  sidecar (`derived_index::to_bytes`, `derived_index.rs:135-139`).
- **On mount:** the host injects the cached sidecar bytes into the bundle's
  `files` map under key `.derived-index.json` before handing it to `Session.mount`.
  `BundleSource` then serves it and `derived_index::load` reads it (the baked-bundle
  fast path — no rebuild). This makes registry packages behave exactly like baked
  ones.
- **On miss:** mount without the sidecar (engine derives in-memory as today), then
  ask the engine for the bytes (§4.4) and persist them for next time.

### 4.4 The engine seam (a separate, gated snapshot-gen/wasm commit)

Pick ONE. Both keep the DRY rule (Rust computes the columns, host persists bytes).

**Option A — export/import methods (recommended; smallest, most explicit).**
```
Session.exportDerivedIndex() -> { ok, result: { indexes: { "<label>": "<b64 sidecar bytes>" } } }
```
For each mounted package, return `derived_index::build(source, pkg_dir)` →
`to_bytes` → base64 (or read-through if the mounted source already has the
sidecar). Pure read; no state change. Host writes each into
`derived/<sha256>/idx-v<FORMAT>.json` and injects on the next mount. No `import`
method is needed — "import" is just the host putting `.derived-index.json` back
into the `BundleSpec.files` it passes to `mount` (works with **zero engine
change** on the read side, since baked bundles already prove that path).

**Option B — fold into `mount`.** Have `mount`/`init` build the sidecar for each
newly-mounted package (making `BundleSource.write_new` interior-mutable per §2.2
so it is cached in-session too) and **return** the sidecar bytes in the mount
envelope for the host to persist. One round-trip instead of two, at the cost of
touching the hot mount path.

**Also expose the format version.** Add `derivedIndexFormatVersion:
DERIVED_INDEX_FORMAT_VERSION` to `Session.version()`'s JSON (`lib.rs:882-890`) so
the host keys/invalidates correctly without hard-coding the constant. This is the
only strictly-required engine change; a bump then re-keys every derived entry
(old files are orphaned, exactly like the native filename bump).

**Cheapest correct MVP:** Option A + the `version()` field. Everything else
(dedup blob store, in-session `write_new`) is optional and additive.

### 4.5 Cache versioning / invalidation

- **Derived store:** `DERIVED_INDEX_FORMAT_VERSION` in the filename (from
  `version()`). Bump ⇒ new filename ⇒ old orphaned. No engine-commit key.
- **Blob store:** `BUNDLE_FORMAT_VERSION` (`bundle.rs:32-34`) in the label record,
  so a bundle-container change re-keys.
- **`bundleCache.ts` `CACHE_VERSION`** bumps `v1`→`v2` at the migration cutover
  (§5) so the old label-keyed store is ignored, not mis-read.

---

## 5. Migration from the current label-keyed `bundleCache`

Keep the app working throughout; no flag day.

1. **Additive first.** Land the derived store beside today's `bundleCache`
   (unchanged). On mount, host does: read `derived/<sha>/idx-v<FMT>` → if hit,
   inject `.derived-index.json` into `BundleSpec.files`; if miss, mount as today,
   then `exportDerivedIndex()` → persist. Baked bundles that already ship the
   sidecar simply seed the store on first mount (host can lift the sidecar straight
   out of `files` — no engine call needed for those). This alone delivers the
   registry-package win with `bundleCache` untouched.
2. **Then CAS the blobs (optional).** Introduce `blobs/` + `labels/`, write
   through on fetch, read-through on mount. Bump `bundleCache` `CACHE_VERSION`
   `v1→v2` (`bundleCache.ts:17`) or replace `bundleCache` with the CAS reader
   behind the same `readCachedBundle`/`writeCachedBundle` signatures
   (`packageResolver.ts:26`, `client.ts:114,141`) so callers are unchanged.
3. **One-time cleanup:** on first `v2` run, best-effort delete the
   `fhir-ig-editor-bundles` dir (degrade-silent, per `bundleCache.ts:24-31`).

---

## 6. The win and the cost (sizing)

**Re-derivation cost per interaction (registry closure, measured inputs).**
`hl7.fhir.r4.core#4.0.1` = **4 579 resource JSON files, 38.7 MB**; its derived
index is **1.09 MB** (88 KB gzipped). Building it parses all 38.7 MB with serde
and lifts ten strings per file (`derived_index.rs:114-133`) — and today that
happens on **every** `PackageContext` build for a sidecar-less package (§2.1). A
real closure adds terminology.r4 (derived idx 857 KB), extensions.r4 (311 KB),
tools.r4 (39 KB) — so the per-build parse is the whole multi-MB closure, repeated
per compile/snapshot/render. Native numbers bound the payoff: eliminating this
parse cut load time ~30-48% (`perf-snapshot-gen.md:296-304`); in the browser it is
pure main-thread-adjacent wasm CPU on every edit, so the felt win is larger.

**OPFS storage.** Tiny. Derived indexes for a full R4 closure ≈
1.09 + 0.86 + 0.31 + 0.04 ≈ **2.3 MB** raw (≈ 250-400 KB if gz'd before store).
Negligible beside the inflated bundles already in OPFS (r4.core alone is ~38 MB
inflated). The derived store is essentially free.

**Dedup benefit (content-addressed blobs): marginal.** Labels are near-unique
(`id#exact-version`); the same bytes under two labels is the rare case
(alias/re-pin). The blob CAS buys correctness-of-model and a little space, not a
warm-open speedup.

**Derived-index-persistence benefit: the real prize.** It converts "parse the
whole closure on every interaction" into "parse a ~1 MB sidecar once, then read a
cached artifact forever," for the exact packages (arbitrary/registry IGs, task
#32) that don't ship a sidecar. Baked-bundle IGs already get this; the redesign
extends it to everything.

---

## 7. Sized build plan

Each size is shippable on its own. Gates are cumulative.

### S — Derived-index persistence, host-only where possible (highest value)
- **Engine (separate gated commit):** add `Session.exportDerivedIndex()`
  (Option A, §4.4) + `derivedIndexFormatVersion` in `Session.version()`. Pure
  read; native unit test that exported bytes byte-equal `derived_index::to_bytes`
  and re-import via `BundleSpec.files` yields byte-identical snapshots.
- **Host:** `derivedIndexCache.ts` (mirror `templateTreeCache.ts`), keyed
  `(tgz-sha256, formatVersion)`. On mount: read-through inject `.derived-index.json`;
  on miss: mount, `exportDerivedIndex`, persist. Seed from sidecars already in
  baked `files` with no engine call.
- **Gate:** warm-open re-render of a **registry-sourced** IG measurably skips
  re-derivation (instrument the `build()` path / time the second compile);
  byte-identical render + snapshot output vs no-cache; existing E2E/byte/parity
  green. Engine commit is its own gate: full snapshot corpus + `cargo test
  --workspace` + sushi byte-parity (the standing derived-index gates,
  `package-derived-index.md:124-127`).

### M — Content-addressed blob store + dedup
- **Host only** (no engine change): `blobs/<sha>` + `labels/<label>` behind the
  existing `readCachedBundle`/`writeCachedBundle` surface; `CACHE_VERSION` `v1→v2`
  + one-time old-dir cleanup (§5).
- **Gate:** cold + warm loads byte-identical to S; two labels with identical bytes
  share one blob (dedup unit test); OPFS quota under budget.

### L — In-session `write_new` + mount-fold (Option B) + full parity sweep
- **Engine (separate gated commit):** interior-mutable `BundleSource.write_new`
  (§2.2) so the sidecar is cached in-session even before OPFS; optionally fold
  export into the `mount` envelope. Touches the hot mount path.
- **Gate:** all S/M gates + the full snapshot corpus + `bundle_ladder`
  (`crates/snapshot_gen/tests/bundle_ladder.rs`) + sushi byte-parity, proving the
  writable source changed no output.

### Engine-repo changes called out explicitly (each its own snapshot-gen commit + gates)
1. **Required (S):** `Session.exportDerivedIndex()` + `version()` format-version
   field. Small, pure-read.
2. **Optional (L):** interior-mutable `BundleSource::write_new`; mount-fold return
   of sidecar bytes. Hot-path — gate hard against the full corpus.

The host-side derived store and blob CAS need **no** engine change beyond #1 (the
read side already works — baked bundles prove `.derived-index.json`-in-`files`).

---

## 8. Recommendation

**Do the derived-index-on-OPFS first; content-addressing second (and optional).**

- The derived-index persistence is where the warm-open cost actually lives for the
  packages that don't ship a sidecar (registry/arbitrary IGs), it is ~free in
  storage, and it needs only a tiny pure-read engine seam (`exportDerivedIndex` +
  a version field). Everything else on the read side already works — baked bundles
  demonstrate the `.derived-index.json`-in-`files` path daily.
- Content-addressed blobs/dedup are a correctness-of-model nicety with marginal
  payoff (labels are near-unique), so they are the **M** follow-up, not a
  prerequisite. Note the derived store is *already* content-addressed (keyed by the
  tgz sha256), so you get the "just like local rust" content-keying where it
  matters without waiting on the blob CAS.

**Sequence: S (derived store + engine seam) → M (blob CAS) → L (in-session
writable source), each independently shippable and gated.** Don't do "both
together" — the derived store delivers essentially the whole felt win by itself,
and coupling it to the blob-CAS migration only enlarges the first gate for little
extra benefit.

---

## 9. The native / `fig` side is the same store (folded in, Josh 2026-07-05)

**Question (Josh): with the fig CLI do we need to materialize `id#version/package/`
folders, or can it work directly from CAS?** Answer: **no materialization needed —
fig needs a `PackageSource`, and CAS can be one.** This makes the native store and
the OPFS store ONE design (`CasSource`), differing only in blob backend.

### 9.1 The engine already accesses packages through an abstraction, not the FS

- `IgContext::load` delegates to `load_with_tree(tree::fs_tree(), …)`
  (`crates/render_sd/src/context.rs:167,177`): it is generic over a `TreeSource`,
  not bound to `std::fs`.
- Packages are addressed by a **virtual path convention**, not physical folders:
  `packages_dir.join(format!("{id}#{ver}")).join("package")` then read through
  `tree.read_dir(packages_dir)` (`context.rs:304,321`). The `id#version/package/`
  shape is a path *convention inside the tree*, not a directory requirement.
- **Proof it is already decoupled:** wasm runs completely folder-free today —
  `MemTree` over `BundleSource` serves packages by that same virtual path with no
  directories on disk (§2.1). Native `fig` uses `fs_tree()` over real
  `.fhir/packages` folders purely because that store is what exists, not because
  anything requires the layout.

So fig's folder use is incidental. A CAS-backed tree that presents the same virtual
`id#version/package/<file>` paths, backed by content-hashed blobs + a per-package
file→hash manifest, satisfies the identical interface. fig then reads DIRECTLY from
CAS: no untar-to-folders, dedup across versions, no tgz+inflated double-storage.

### 9.2 What this adds to the build (native `CasSource`)

- A `package_store::CasSource` (a `TreeSource`/`PackageSource` impl) that resolves
  the virtual package path → blob-by-content-hash via a manifest. Disk-blob-backed
  natively; the OPFS store (§4) is the SAME logical store, blob-backed by OPFS.
  Native and wasm differ ONLY in the blob backend.
- `IgContext`'s one remaining literal-path parameter (`packages_dir: &Path`) is
  already routed through `TreeSource`, so pointing it at a CAS-backed tree is small
  — no change to the render/snapshot logic above it.
- The derived index (§1, §4.3) lives in the SAME CAS keyed by content hash, so a
  package populated by `fig` and one populated by the browser share the identical
  derived-index artifact — the DRY endpoint.

### 9.3 Interop: materialization becomes an optional EXPORT, not the working form

- `fig packages export --to ~/.fhir/packages` materializes folders **on demand**,
  for coexistence with the Java IG Publisher / SUSHI / any tool that reads the
  standard layout. Not needed for fig's own operation.
- Keep the existing `DiskSource` as an **input** option too, so fig drops into a
  machine SUSHI/Publisher already populated (`~/.fhir/packages`). The store thus
  reads three ways — CAS (native default), an existing folder cache (DiskSource),
  or mounted bundles (wasm) — and materializes folders only when asked.

### 9.4 Sequencing this piece

Native `CasSource` is **M/L-adjacent**: it depends on the content-addressed blob
store (M) existing, and is the native consumer of it. Recommended order becomes
**S (derived store + engine seam) → M (blob CAS, now spec'd as shared native+OPFS)
→ native `CasSource` + `fig` CAS-default + `export` (the native half of M) → L**.
The derived-index win (S) still lands first and independently; the folder-free fig
operation rides on the same blob CAS the browser needs, built once.

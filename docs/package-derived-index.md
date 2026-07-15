# Package derived-columns index (CAS artifact)

> Design note for the CAS derived-index landed in the CLARITY pass (task #12b).
> Companion to `docs/perf-snapshot-gen.md` "Future levers → 1" (the strategic
> load-path fix). Parity is sacred: the full snapshot corpus stays byte-identical
> and the sushi compiler is untouched.

## Problem

Three package-reading layers each re-derived the same per-resource metadata from
package *content* on every process, purely because two columns are not in the
stock `.index.json`:

- **`name`** — a FHIR conformance resource's `name` is never written to
  `.index.json`. `package_store` (eager, every run) and `snapshot_gen`'s
  `PackageContext` (`probe_name`, every run) each read every SD file to recover
  it. The perf pass byte-scanned it, but still touches every file every process.
- **`baseDefinition`** — likewise absent from `.index.json`; `package_store`
  full-parses the resolved SD to expose `parent` in `fishForMetadata`.
- **SD-less / empty-index packages** (`us.nlm.vsac`, `phinvads`,
  `subscriptions-backport.r4`): `PackageContext` falls into
  `scan_package_structure_definitions`, which directory-scans + byte-checks +
  parses every top-level JSON just to discover the SDs the `.index.json` omitted.

All of this is derived from immutable package content, so it should be computed
**once per package content**, not once per process.

## The artifact

A **derived-columns index** is a CAS artifact keyed by
`(package content sha256 + DERIVED_INDEX_FORMAT_VERSION)`. It is computed **once,
at ingest** (same trigger/lifecycle as the existing generated `.index.json`
artifact — never at process start, never per materialize), and stored at:

    <cas>/packages/<sha256>/derived/derived-index-v<FORMAT>.json

The format version is a plain integer constant
(`package_store::derived_index::DERIVED_INDEX_FORMAT_VERSION`). Bumping it changes
the artifact filename, so an old artifact is simply never read again — entries are
never mutated in place; a bump invalidates by key.

### Contents (derived from package CONTENT, not from stock `.index.json`)

One row per resource file in the package (every `^[^.].*\.json$` except
`package.json`, sorted — the exact FPL `getPotentialResourcePaths` set, so it
also covers packages whose stock `.index.json` is empty/SD-less):

| column | source |
|---|---|
| `filename` | directory entry |
| `resourceType` | top-level `resourceType` |
| `id` | top-level `id` |
| `url` | top-level `url` |
| `version` | top-level `version` |
| `kind` | top-level `kind` |
| `type` | top-level `type` (SD `type`, FPL `sdType`) |
| `derivation` | top-level `derivation` |
| `baseDefinition` | top-level `baseDefinition` |
| `name` | top-level `name` |

`resourceType`/`id`/…/`derivation` match the columns the stock generated
`.index.json` already carries; `baseDefinition` and `name` are the two new
columns that eliminate the per-process reads. The file is parsed **once** with a
single `serde_json::from_slice` and the ten columns are lifted from the root
object — this is the only full parse of the corpus that survives, and it happens
once per content hash, ever.

## Lifecycle and placement

The artifact is **computed once at ingest** (`ingest_artifact_bytes`), alongside
the existing generated `.index.json` derived artifact. **Materialization only
hardlinks it out** — it is never derived at materialize or process start.

It is installed into the materialized cache as a **sidecar** next to
`.index.json`:

    <cache>/<name>#<version>/package/.derived-index.json

- **Hardlink-tree materialize** (the isolated test cache, and any cache on a
  filesystem/CAS layout where the whole package dir is not symlinked): the CAS
  `derived/derived-index-v<FORMAT>.json` artifact is hardlinked to
  `package/.derived-index.json`. Zero copy, zero recompute.
- **Symlink-whole-dir materialize** (when the stock `.index.json` is trustworthy
  and the platform supports directory symlinks): `package/` is a symlink into the
  read-only CAS content, so a sidecar cannot be written there. Consumers fall
  through to the on-first-need path below. (This path is not exercised by the
  snapshot corpus gate, whose cache is hardlink-materialized.)

### Non-CAS caches (plain extracted dirs, already-materialized test cache)

The isolated snapshot-corpus cache `temp/fhir-home/.fhir/packages` is a plain,
already-materialized hardlink cache with **no CAS handle reachable from
`snapshot_gen`** (it receives only a `--cache <dir>` path). For any package dir
whose `.derived-index.json` sidecar is absent, consumers **write it once, on
first need**, by deriving from package content with the same shared builder, then
read it. This is fail-loud-safe: if the sidecar cannot be written (read-only dir,
e.g. a CAS symlink), the consumer silently falls back to its in-process
probe/scan path and still produces byte-identical results. The write-once sidecar
and the CAS-hardlinked sidecar are **byte-identical** — one shared builder
(`package_store::derived_index::build`) produces both.

## Shared implementation

The format, builder, and reader live in **`package_store::derived_index`** — the
lowest crate both the write side (`package_acquisition`) and the read sides
(`package_store`, `snapshot_gen`) can share. `snapshot_gen` gains a
`package_store` dependency (it is, after all, the FHIR package read layer).

## Consumers

- **`snapshot_gen::PackageContext::load_package`**: reads the derived-index
  sidecar (or writes-once-then-reads for the isolated cache) instead of parsing
  `.index.json` + `probe_name` + `scan_package_structure_definitions`. The
  derived index already folds in the scan-fallback (it lists content, not the
  possibly-empty stock index), so the empty-`.index.json` full-conversion path is
  preserved by keying `local` on "package had no SD rows in its stock index" the
  same way the old `loaded == 0` scan trigger did. Fail-loud fallback: a cache
  with neither a sidecar nor a writable dir uses the old code path unchanged.
- **`package_store`**: takes `name` (and, where used, `baseDefinition`) from the
  derived index instead of the eager `probe_name_from_path` per SD, same fallback
  rules.

## Parity & gates

See `docs/perf-snapshot-gen.md` "CAS derived-index" section for the before/after
load-time numbers and the gate results (full corpus at scorecard counts,
`cargo test --workspace`, sushi IPS byte-parity).

## `PackageSource` trait + browser bundles (WASM P1)

The read path no longer calls `std::fs` directly: every access goes through the
`package_store::source::PackageSource` trait
(`read`/`read_dir`/`exists`/`is_dir`/`write_new`). The native impl `DiskSource`
forwards each call to `std::fs`, so behavior is byte-for-byte unchanged (the full
34-IG corpus + `cargo test --workspace` + sushi IPS byte-parity gate this). This
is what keeps `std::fs` out of the read path so a wasm build stays plausible.

Native callers are unchanged: `PackageStore::for_project(ig, cache)` and
`PackageContext::new(cache, packages)` keep their old signatures and construct a
`DiskSource` internally. Explicit-source variants (`for_project_with`,
`new_with`, `package_resource_entries_with`, and `derived_index::{build,load}`,
which now take a `&dyn PackageSource`) let a browser/test caller mount a different
backing store.

### Bundle format (v1)

The browser mounts a **`BundleSource`** (`package_store::bundle`) — a read-only,
in-memory `PackageSource`. It is fed **package bundles** produced by the builder
in `package_acquisition`:

- **One bundle per package**: `package_acquisition::build_bundle(package_dir)`
  emits a **gzipped tar** of the materialized `package/` directory's top-level
  files (every resource JSON + the stock `.index.json` + the
  `.derived-index.json` sidecar). Tar entries are named by the package-relative
  filename (no `package/` prefix). The sidecar is **guaranteed present** — if the
  materialized dir lacks it, `build_bundle` derives it from content with the same
  shared `derived_index::build`, so the bundle is fully self-describing and the
  `BundleSource` never needs to write (write-once fails soft on read-only
  sources). Determinism: files sorted, gzip `mtime=0`, tar `mode=0644`/`mtime=0`.
- **Manifest lockfile**: `build_bundle_set(cache, labels, out_dir)` writes one
  `<id>#<ver>.tgz` per package plus a `bundle-manifest.json`
  (`package_store::BundleManifest`: `{bundle-format-version, packages:[{id,
  version, bundle, sha256}]}`) — the editor's pin of the exact package set.
- **Mounting**: `read_bundle(blob)` inflates a bundle to its `filename -> bytes`
  entries. Hosts first call `normalize_package_material(label, entries)`, the
  shared native/WASM trust boundary: it verifies `package.json` identity and
  dependency shape, validates and retains safe nested template transport,
  regenerates `.derived-index.json`, and returns the complete canonical files
  from which the deterministic prepared carrier is built.
  `BundleSource::mount_package(label, material.files)` places
  them under a synthetic cache root at `<root>/<id>#<ver>/package/...`. Pass
  `source.cache_root()` as the `cache_dir` to `new_with`/`for_project_with`. Cold
  start = one fetch + one inflate + map lookups thereafter; no `std::fs`
  (flate2/tar are wasm-clean).
- **CLI**: `rust_sushi bundle --cache <cache> --out <dir> <id#ver> [<id#ver>...]`
  drives `build_bundle_set` and prints the manifest.

**Gate**: `crates/snapshot_gen/tests/bundle_ladder.rs` builds the r4/r5 core
bundles from the isolated cache, round-trips them through
`build_bundle`→`read_normalized_bundle`→`mount_package`, and runs the full fixture ladder
through a `BundleSource`-backed `PackageContext` — proving the bundle path
end-to-end, natively, to the same goldens as the disk cache.

### PreparedPackage format (v3)

`package_store::PreparedPackage` is the warm-start successor to both the
JSON/base64 envelope and the expanded v1 artifact. Its deterministic compact
container carries a canonical member directory plus independently compressed
1 MiB raw-DEFLATE chunks. Every member and chunk has an exact raw length and
SHA-256; the source key is a domain-separated digest of canonical member
metadata. `PackageLock` roots the SHA-256 and byte length of this exact carrier,
which is also the object execution mounts. There is no parallel inflated
package-lock payload.

Decode checks the format tuple, host-selected key, dependency and member order,
safe paths, metadata digest, exact chunk partition, bounds, and required
`package.json`/current-sidecar members without inflating a chunk. It computes
the exact carrier SHA-256 once and retains it for the enclosing `ContentRef`;
the content-addressed carrier has no redundant internal checksum footer.
`PackageSource::read` later bounded-inflates one chunk and verifies its chunk and
member digests before exposing bytes. An 8 MiB per-artifact LRU is the only
optional expanded retention; oversized chunks are never cached.

SiteEngine retains one typed prepared mount containing the exact carrier and
its validated directory. Closing a build verifies that already-admitted carrier
reference without expanding members; fresh restoration reads the same rooted
carrier and mounts it lazily. Chunk and member digests are checked before any
body is exposed.

Native production uses the same API exposed to WASM:

```sh
fig packages prepare --cache <cache> --out <dir> <id#ver>...
# equivalent lower-level compatibility command:
rust_sushi packages prepare --cache <cache> --out <dir> <id#ver>...
```

Both commands emit `<id>#<ver>.fpp` files and
`prepared-package-manifest.json`. Hosts verify the manifest's artifact SHA-256,
then pass each artifact through the indexed `beginPreparedMount` /
`stagePreparedMount` / `commitPreparedMount` transaction. The engine
independently verifies the prepared-package key, expected label, resolver slot,
and canonical directory metadata, and derives the exact carrier digest retained
by SiteBuild.

For a warm multi-package start, call `Session.beginPreparedMount(count)`, then
read/authenticate and `stagePreparedMount(resolverIndex, bytes, cacheKey,
expectedLabel)` as artifacts arrive. Arrival order is irrelevant: commit
reconstructs the exact resolver order. The expected label is checked before a
slot is occupied, so a rejected carrier leaves that slot retryable.
`commitPreparedMount()` installs the complete set only after every artifact and
conflict validates; `abortPreparedMount()` drops staged state on failure.
This bounds JavaScript peak memory by the largest artifact instead of allocating
a closure-sized concatenation. Results report `decodeValidateMs`, `mountMs`,
artifact bytes, requested packages, and newly added/total package counts,
compressed retained/declared raw bytes, lazy-inflate counters, `indexedMembers`,
and `memberBodyCopies` (zero at mount).

For a cold registry package, `Session.prepareTgzArtifact(label, bytes)` performs
bounded TGZ parsing, normalization, derived-index construction, and binary
encoding without mounting. The host calls `takePrepared(label)` once to receive
the direct `.fpp` `Uint8Array` for OPFS and the same indexed transaction stages
it immediately. `prepareArtifacts(rawBundlesJson)` is retained only for explicit
raw local-bundle compatibility. PreparedPackage v1/v2 pointers are rejected as
misses and rebuilt from the authenticated source; the browser never expands an
old pointer merely to migrate it.

`BundleSource` stores each mounted package in an immutable `Rc` layer keyed by
package label. Transactional mounts shallow-clone that small label map; they do
not clone previously mounted file bodies. Each compact layer stores compressed
backing bytes plus validated chunk/member ranges and
shares that allocation. `PackageSource::read` copies only the specific body a
compiler/renderer requests. Lookups select the package layer by the first cache-
path component, avoiding a linear scan across packages.

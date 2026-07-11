# ContentStore and prepared packages: native + browser

This document describes the implemented storage and package-loading design. The
old inflated/base64 OPFS proposal is obsolete.

## The layers

```text
package registry / baked tgz / local drop
                    |
                    v
          untrusted package entries
                    |
       normalize + validate exactly once
                    |
                    v
             PreparedPackage (.fpp)
                    |
          immutable ContentStore bytes
                    |
        +-----------+-----------+
        |                       |
 FileContentStore           OPFS ContentStore
 native CLI/Fig             browser Worker/WASM
```

`ContentStore` owns immutable bytes. Typed manifests explain what those bytes
mean. A path, package coordinate, or project id is never a content identity.

The shared Rust `content_store` crate defines:

- `Sha256Digest` and `ContentRef { sha256, byteLength, mediaType }`;
- verified reads and writes through the `ContentStore` trait; and
- `FileContentStore`, with digest/length/media validation, symlink and race
  defenses, and atomic no-clobber publication.

The editor implements the same value contract over OPFS in
`app/src/storage/contentStore.ts`. Reads recheck length and SHA-256. Prepared
package pointers are published only after their referenced content object is
durable, so a torn write produces a cache miss, never authorized partial bytes.

## PreparedPackage

`package_store::PreparedPackage` is the stable execution representation for one
exact FHIR package. Its binary artifact contains:

- canonical metadata for every safe member, including length and SHA-256;
- validated package identity, declared dependencies, and current derived index;
- deterministic independently raw-DEFLATE-compressed 1 MiB chunks; and
- a trailing checksum.

Its cache key binds all interpretation-relevant versions:

```text
source SHA-256
+ PreparedPackage binary format version
+ normalization algorithm version
+ derived-index format version
+ package engine ABI version
```

The canonical string is
`pp<format>-sha256-<digest>-n<normalization>-d<index>-a<abi>`.
Changing any component causes a clean cache miss.

The decoder verifies the host-selected key, compact artifact checksum, canonical
member/dependency order and paths, member-digest source root, chunk/member
partition, lengths, and required metadata members without inflating bodies.
Every first member read bounded-inflates its chunk and verifies both chunk and
member SHA-256. Warm loading never rebuilds the derived index or parses every
FHIR resource.

Prepared layers retain compact bytes plus validated member/chunk indexes.
`PackageSource::read` materializes only a requested body; an 8 MiB per-artifact
LRU bounds reusable raw chunks and does not retain oversized chunks. BundleSource
itself remains an immutable label-to-layer map, so transactional append
shallow-copies labels/Rcs rather than package bodies.

## Browser flow

Engine boot loads only the package catalog. The Rust resolver selects the active
project's closure; manifest `loadPhase` values distinguish resolver-selected
`compile`, `snapshot`, and `on-demand` packages.

Cold package flow:

1. The host authenticates/fetches a baked tgz, local drop, or registry package.
2. `Session.prepareAndMount` validates, normalizes, builds the derived index,
   mounts immutable layers, and reports artifact metadata.
3. `Session.takePrepared(label)` transfers the `.fpp` as `Uint8Array`.
4. The Worker writes it to the OPFS ContentStore and publishes a small pointer.

Warm package flow:

1. The main thread reads only small pointers.
2. The Worker reads and authenticates `.fpp` bytes directly from OPFS.
3. It calls `beginPreparedMount`, then reads and stages one compact artifact at
   a time; it never allocates a second closure-sized batch.
4. `commitPreparedMount` installs layers only after every artifact validates;
   any failure calls `abortPreparedMount` and reacquires original transport.

No warm package body becomes a `{ filename: base64 }` JS object or expanded
package blob. PreparedPackage v1 pointers are clean misses rather than migration
inputs, avoiding a one-time 932 MB read. The legacy v3 inflated JSON reader
exists only to migrate older profiles; new acquisitions do not create one.

A persistent resolution lock may pre-acquire an exact closure in one batch, but
it is only an optimization hint. The lock key binds exact config bytes, resolver
schema, the emitted JS/WASM engine recipe digest, baked authority, registries,
and proxy. Mutable requests
(`latest`, ranges, `current`, or `dev`) are freshness-checked against their
current authority. Rust re-resolves after mounting and the exact compile/context
closures must match; otherwise the normal resolve/fetch/mount fixpoint runs.

## Build and output reuse

The browser does not persist a second serialized build representation. The
Worker retains bounded immutable build handles created by `prepare`; their
catalog entries carry `ContentRef`s into the same OPFS `ContentStore`. A lazy
page render writes and verifies its content first, then updates only the private
handle runtime. `finalize` can create a `SiteOutput` only after the complete
declared catalog is ready.

The preview Service Worker persists one small per-IG publication pointer:
`{ handle, buildId, catalog }`. It accepts only validated output paths and reads
ready bytes by digest/length from OPFS; build-id response caches and transferred
ArrayBuffers are performance fallbacks, not authority. There is no derived-
artifact manifest, asset-byte API, or base64 generation cache.

Within WASM, PreparedGuide and target preparation have exact in-session reuse.
Their identity binds ProjectRevision, PackageLock, actual compiled semantics,
preparation inputs, and recipe/API versions. There is intentionally no browser
cache of the UI `CompileResult`: it cannot restore compiler semantics into a
new Session and would be a false cache hit.

Native complete output caching uses `FileSiteOutputCache`. It indexes canonical
`SiteOutput` manifests by the pre-render `sok1` key, verifies the requested
closed input and every `ContentStore` object on a hit, publishes atomically with
no clobber, and reports same-key/different-`so1` output as renderer
nondeterminism.

## Native and CLI flow

Both supported CLIs call the same `package_acquisition` implementation:

```sh
fig packages prepare --cache <cache> --out <dir> <id#version>...
rust_sushi packages prepare --cache <cache> --out <dir> <id#version>...
```

They emit one `<id>#<version>.fpp` plus
`prepared-package-manifest.json`. Their output is byte-identical for identical
inputs. Native Fig publishes and rereads closed-build content through
`FileContentStore`; browser artifacts use the OPFS implementation of the same
contract.

Materialized `id#version/package/` trees remain compatibility inputs/exports,
not the semantic handoff and not the required warm representation.

## Project and build objects

Catalog source archives carry a small SHA-256 descriptor. `ProjectStore`
persists text, binary assets, and a commit marker written last. Reopening the
same source digest skips archive fetch/unpack/rewrite; any edit clears the marker.

Compilation and rendering use separate immutable identities:

- `ProjectRevision`: exact authored bytes;
- `PackageLock`: exact content-addressed package closure;
- `PreparedGuide`: renderer-neutral prepared semantics;
- `SiteBuild` / `ClosedSiteBuild`: artifact graph and ready-closure proof; and
- renderer output identity: closed BuildId plus renderer recipe/output schema.

`site.db` was an obsolete v1/SQLite projection and has been deleted. It is not a CAS
and not the compile-to-render handoff.

## Measurement and gates

The repeatable persistent-profile benchmark is:

```sh
BASE_PATH=/fhir-ig-editor/ \
  bash scripts/run-uscore-benchmark.sh app/dist > uscore-benchmark.json
```

It measures cold start, warm hard reload, and same-worker reopen, including
network bytes, progress-stage durations, Worker operations, engine timings, and
long tasks. The browser correctness gate remains:

```sh
BASE_PATH=/fhir-ig-editor/ bash scripts/run-browser-gates.sh app/dist
```

Rust gates include `content_store`, `package_store`, `package_acquisition`, both
CLI envelope suites, `site_build`, `fig`, and `wasm_api`, plus an actual
`wasm32-unknown-unknown` release build.

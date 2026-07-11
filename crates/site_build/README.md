# SiteBuild v1

`site_build` is the renderer-neutral handoff between compilation and site
production. It is a value contract, not a build coordinator and not another
name for `site.db`.

A `SiteBuild` records:

- an exact project revision and content-addressed source manifest;
- an exact, content-addressed FHIR package closure;
- the renderer, template, mode, and parameters of the render target;
- typed semantic, fragment, page, asset, and data artifact keys; fragments carry
  a whole-IG or resource scope, while assets carry an authored, template,
  Publisher-runtime, generated, or extension namespace;
- an explicit `ready`, `deferred`, `unsupported`, or `failed` state for every
  cataloged artifact;
- the producer, recipe, and actual read dependencies of each artifact; and
- stable build diagnostics.

The value is immutable once constructed. Its `sb1-sha256:...` build id covers
every other field using recursively key-sorted canonical JSON. Deserialization
recomputes that id and rejects tampering or accidental partial rewrites.

Artifact content is addressed but not embedded. A host is responsible for
putting referenced bytes in a CAS and verifying their digest and length. A
demand-driven renderer records a typed `Need<ArtifactKey>` and answers it with
an atomic `ResolutionBatch`; `SiteBuild::successor_batch` applies that batch to
an explicit predecessor. The lower-level `SiteBuild::successor` transition
remains available for producers that already have a complete set of
`ArtifactResolution` values. Ready
resolutions carry their exact bytes; the result contains the re-hashed successor
and a digest-keyed set of newly introduced `ContentObject`s for CAS publication.
Non-ready resolutions carry no stale object. Batch order cannot change the
successor id or object set, identical bytes are stored once even when records
use different media types, and the predecessor is never mutated. This is an
additive Rust API: the `site-build/v1` JSON shape and hashing semantics did not
change.

A `RenderPlan` declares the roots a renderer needs. Converting to
`ClosedSiteBuild` follows their artifact read dependencies transitively and
fails with typed blockers unless the whole closure is `ready`. A callback-free
external builder should accept this proof-bearing wrapper, not an open
`SiteBuild`.

## Native stock-template revisions

`render_page::collect_stock_revision` is the stock renderer's thin collector
over the transition API. Its plan roots are every advertised final page and
every file in the `stock.assembled` asset namespace. Actual page-source,
`site.data`, staged-include, template-include, and successful fragment reads are
artifact dependencies, so sealing checks them transitively. A failed fragment
attempt is retained as `deferred`, `unsupported`, or `failed`, but is not added
to a successfully rendered page's reads when an ordinary staged/template file
provided the fallback.

`render_page::ClosedBuildArtifactResolver` replays generated includes from an
explicit CAS with no fragment callback. It serves only artifacts reachable from
the sealed plan (unrelated ready catalog entries are outside the closure proof)
and rechecks digest, length, and UTF-8 before returning content.

Fig's publication entry point is
`render_site_for_revision(predecessor, root, options)`. It recursively captures
the public staged tree, renders from the captured bytes, repeats the capture to
detect mutation, and returns an opaque value bound to the predecessor and
root/options, with a canonical seal over HTML, complete read sets, fragment
observations, asset bytes, counters, and inventory.
`collect_site_build_revision` accepts only that bound value. Plain `render_site` is intentionally a direct-write API,
not an alternative revision handoff. The strict path rejects unreadable or
non-regular public-tree entries, symlinks, unsupported Markdown page sources,
and malformed or unreadable `_data` rather than silently publishing an
incomplete inventory.

The initial predecessor/root association is a trusted native-producer
assertion, not a proof derived from the ambient F0 filesystem. The seal prevents
changing or relabeling an outcome after capture. A future native closed-input
adapter should reconstruct the own-resource/package/tx-cache trees from the
predecessor and CAS, eliminating this remaining trust edge.

## Cycle typed projection

`PreparedGuide` is the renderer-neutral semantic preparation result: guide
identity, FHIR resources and publication metadata, terminology expansions,
navigation, parsed config, and authored assets with their source reads. It has
no database row keys and no Cycle artifact names or schemas.

`cycle_semantic::close_prepared` consumes that value directly; this API is
available without `site_db` or the `site-db-projections` feature. It emits a
closed `cycle-site/v2` plan rooted in:

- `cycle.semantic/v1/resources.json` — prepared FHIR objects and only the
  publication facets not safely recoverable from them;
- `terminology.json` — actual ValueSet expansion products;
- `navigation.json` — recursive page and menu trees;
- `config.json` — parsed `sushi-config`; and
- one raw `AssetNamespace::Authored` artifact per logical asset path.

There are no row surrogate keys, PascalCase database columns, flattened tree
ids, JSON strings inside JSON, or base64 asset bodies on this wire. Embedded
FHIR/config object order is intentionally retained: artifact identity hashes the
exact serialized bytes through `ContentRef`, while only the SiteBuild manifest
uses recursively key-sorted canonical JSON.

Navigation roots are semantic roots. The transitional flat `Pages` projection
may begin at a positive depth after a synthetic structural page such as
`toc.html` was omitted; the projector rebases that uniform source offset to
semantic depth zero while preserving every relative parent/child edge. The
offset is compatibility bookkeeping and is not part of v2 identity.

The optional `site-db-projections` feature adds the one migration adapter,
`cycle_semantic::prepare_from_site_db`. `close_projection` remains a convenience
wrapper around that adapter plus `close_prepared`. Native Fig and WASM spell the
direct path explicitly, so their renderer-facing seam is
`PreparedGuide -> ClosedSiteBuild`; neither constructs SiteDb rows for v2.
Equality tests prove that shared preparation and the compatibility adapter
produce the same semantic objects for common inputs.

The `prepared_guide` crate is upstream of both `site_build` and `site_db`.
Shared disk and in-memory preparation constructs it directly from compiled
resources, navigation/config inputs, and authored bytes. Authored assets retain
the exact winning source path after de-duplication. `site_db::project_prepared`
is the single one-way compatibility projection used only by v1/SQLite callers;
the semantic preparation path never constructs rows in parallel.
`SiteDb` retains an in-memory, non-legacy-row primary ImplementationGuide key
selected by the compiler before examples are merged and rows sorted. Snapshot
completion uses the complete exact resolved dependency closure, with core only
as a validated distinguished member.
The optional legacy row projection carries the same choice without a new wire
field: exactly one ImplementationGuide row has `Web = index.html`; additional
guides keep their resource ids, canonicals, and ordinary pages.
The reverse `prepare_from_site_db` adapter is retained only for migration gates
and legacy callers; it can be removed once those callers move to shared
preparation.

## V1 `site.db` compatibility

The optional `site-db-compat` feature provides a deliberately narrow adapter. It
canonicalizes the current Cycle-oriented `SiteDb` rows as one legacy data
artifact and returns both the bytes and their ready artifact record. The core
crate does not depend on SQLite, and individual legacy rows are not presented as
the universal semantic model.

`site_db_compat::close_projection` is the one shared Rust assembly for this
external-builder target. It accepts a row model plus an already-derived exact
`ProjectRevision`, `PackageLock`, render target, and diagnostics; attaches the
complete source/package reads; creates the one-root plan; and returns a
`ClosedSiteBuild` plus the addressed bytes. It cannot derive or fabricate
project/package identity from `site.db`. Both WASM and native
`fig prepare --target cycle-site/v1` use this function only for migration. The
editor and preferred native flow select `cycle-site/v2`. Fig supplies the exact
inputs by content-addressing every authored input file, resolving and hashing
the explicit-cache package closure with
`package_store::normalize_package_material` (the same identity/dependency/
derived-index/canonical-byte boundary used by WASM), reconstructing both inputs
in a private staged filesystem, and
deriving identity from the same native `site_db::build` whose rows are
projected. No post-capture live-tree read can influence semantic inputs,
execution, or identity; later live comparisons are mutation diagnostics only.

## Exact rendered-output caching

`SiteOutput` is the renderer-neutral, browser-serializable receipt for a
complete materialized site. Its two identities have different jobs:

- `OutputCacheKey` (`sok1-sha256:`) is computable before rendering. It binds the
  closed `SiteBuild` id to renderer id/version, an exact renderer recipe digest,
  output schema, and normalized options. Hosts may use this only as a cache
  lookup key.
- `SiteOutputId` (`so1-sha256:`) additionally binds the canonical sorted output
  inventory: each safe relative path, content digest/length/media type,
  producer, source recipe, and owner.

`SiteOutput::verify_for` rejects a receipt from another closed build, while
`verify_store` reads and verifies every addressed object through the shared
`ContentStore`. Paths and mutable project names are never cache keys. A hit is
usable only after both manifest identities and every referenced byte verify.
`FileSiteOutputCache` provides the native implementation: canonical manifests
are atomically published under `OutputCacheKey`, hits re-read every object, and
different outputs under the same derivation key are rejected as renderer
nondeterminism rather than overwritten. Browser hosts implement the same
`SiteOutputCache` contract over OPFS.

```sh
cargo test -p site_build
cargo test -p site_build --features site-db-projections
cargo test -p site_build --features site-db-compat
```

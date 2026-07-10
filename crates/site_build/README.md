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
demand-driven renderer uses the pure `SiteBuild::successor` transition with an
explicit predecessor and a batch of `ArtifactResolution` values. Ready
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

The optional `site-db-projections` feature currently exposes the transitional
`cycle_semantic::close_projection` producer for `cycle-site/v2`. It emits a
closed plan rooted in:

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

This producer accepts `SiteDb` only because that is today's prepared-site model.
`SiteDb` retains an in-memory, non-legacy-row primary ImplementationGuide key
selected by the compiler before examples are merged and rows sorted. Snapshot
completion uses the complete exact resolved dependency closure, with core only
as a validated distinguished member.
That dependency is scaffolding, not an architectural claim that the database is
the handoff. The intended next refactor extracts a renderer-neutral
`PreparedSite`; typed Cycle artifacts and optional SQLite output will both be
projections of that value.

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

```sh
cargo test -p site_build
cargo test -p site_build --features site-db-projections
cargo test -p site_build --features site-db-compat
```

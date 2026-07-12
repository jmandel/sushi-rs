# SiteBuild contract

`site_build` is the renderer-neutral handoff between compilation and site
production. It is a value contract, not a build coordinator.

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

## Cycle typed projection

`PreparedGuide` is the renderer-neutral semantic preparation result: guide
identity, FHIR resources and publication metadata, terminology expansions,
navigation, parsed config, and authored assets with their source reads. It has
no database row keys and no Cycle artifact names or schemas.

`cycle_semantic::close_prepared` consumes that value directly and emits a closed
`cycle-site/v2` plan rooted in:

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

The `prepared_guide` crate owns both native and in-memory preparation. Fig and
WASM pass its `PreparedGuide` result directly to
`cycle_semantic::close_prepared`; relational rows and reverse adapters are not
part of the contract.

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
```

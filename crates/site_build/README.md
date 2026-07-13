# SiteBuild contract

`site_build` is the renderer-neutral handoff between compilation and site
production. It is a value contract, not a build coordinator.

A `SiteBuild` records:

- an exact project revision and content-addressed source manifest;
- an exact, content-addressed FHIR package closure whose `content` value is the
  deterministic prepared-package carrier consumed by execution;
- the renderer, template, mode, and parameters of the render target;
- typed semantic, fragment, page, asset, and data artifact keys; fragments carry
  a whole-IG or resource scope, while assets carry an authored, template,
  Publisher-runtime, generated, or extension namespace;
- an explicit `ready`, `deferred`, `unsupported`, or `failed` state for every
  cataloged artifact;
- the producer, recipe, and actual read dependencies of each artifact; and
- stable build diagnostics.

The only supported wire value is `site-build/v2`. Older tags are rejected by
deserialization; there is no compatibility adapter or upgrade path.

The value is immutable once constructed. Its `sb1-sha256:...` build id covers
every other field using recursively key-sorted canonical JSON. Deserialization
recomputes that id and rejects tampering or accidental partial rewrites.

Artifact content is addressed but not embedded. A host is responsible for
putting referenced bytes in a CAS and verifying their digest and length.
Preparation constructs one complete immutable build atomically. Path rendering
may memoize output objects inside its bounded handle, but it never promotes
artifacts into a successor build. The removed `Need`, `ResolutionBatch`,
`ArtifactResolution`, and `SiteBuildSuccessor` API was an unused parallel
handoff architecture.

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

`site_engine` owns native and in-memory preparation. Fig and WASM both call
`SiteEngine::prepare_project`, which constructs the `PreparedGuide` and projects
the same Cycle closure; relational rows and reverse adapters are not part of
the contract.

## SiteOutput

`SiteOutput` is the renderer-neutral, browser-serializable receipt for a
complete materialized site. `SiteOutputId` (`so1-sha256:`) binds the exact
closed `SiteBuild`, renderer id/version/recipe, output schema/options, and the
canonical sorted output inventory: each safe relative path, content
digest/length/media type, producer, source recipe, and owner.

`SiteOutput::verify_for` rejects a receipt from another closed build, while
`verify_store` reads and verifies every addressed object through the shared
`ContentStore`. The contract deliberately defines no cache key, cache trait, or
filesystem cache. A host may privately index an exact derivation, but a hit is
usable only after parsing an ordinary canonical `SiteOutput`, checking its
input identity, and verifying every referenced byte. That optimization cannot
become another serialized build value or functional operation.

```sh
cargo test -p site_build
```

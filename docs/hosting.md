# Hosting the engine

The editor's [`ARCHITECTURE.md`](../../../ARCHITECTURE.md) is the normative
cross-repository contract. This document explains how the Rust, WASM, and
native Fig hosts implement it; it does not define another build flow.

## Domain values and byte storage

There are three site-generation domain values:

```text
PreparedGuide -> SiteBuild -> SiteOutput
                      |
                 ContentStore
```

- `PreparedGuide` is the complete renderer-neutral guide: prepared FHIR
  resources, terminology products, navigation, parsed configuration, and
  authored-content references.
- `SiteBuild` is one immutable target-specific renderer input. A
  `ClosedSiteBuild` proves that every required artifact is ready.
- `SiteOutput` is the authenticated complete mapping from safe output paths to
  content, media type, producer, ownership, and exact renderer/input identity.

`ContentStore` contains immutable bytes addressed by digest and length. It is
storage plumbing, not a fourth domain value, site database, or serialized
renderer model. Project revisions, package locks, caches, template trees,
fragment observations, output catalogs, and handles are inputs or scoped
execution details.

For Publisher, the closed object set includes the exact project sources and
package lock plus prepared semantic documents, every authored role, the
materialized template tree, assembled runtime inputs, and the package evidence
needed by rendering. Closure verification authenticates those bytes before a
runtime is installed.

## The only host API

```text
prepare(project, generatorSpec) -> BuildHandle
outputs(handle)                 -> OutputCatalog
render(handle, path)            -> Output
finalize(handle)                -> SiteOutput
```

`BuildHandle`, `OutputCatalog`, and `Output` are scoped views, not stored domain
values. A handle names one immutable `SiteBuild`. Rendering a path can memoize
its addressed bytes, but it neither mutates build identity nor creates a
successor handle. Rendering A/B/A must produce the same bytes as B/A/B.

The host captures one complete project revision and supplies one exact,
resolver-scoped `PackageEnvironment`. A private preflight may send only config
and template identity to Rust so the host can acquire missing exact package
bytes; dependency and template decisions remain Rust-owned. The complete FSH,
predefined-resource, and authored-site payload crosses once in
`SiteEngine::prepare_project`, which owns the single semantic compile and target
preparation transaction. A failure installs neither a partial project nor a
partial runtime. `wasm_api` parses and serializes transport; it does not
assemble a second site model.

The browser worker exposes these four operations. Lower-level binary reads and
the external-renderer branch of finalization are private transport plumbing:

- `readContent(handle, digest)` returns verified bytes for a `ContentRef`;
- `finalize(handle, externalInput?)` uses the optional typed external input only
  for an external-builder handle; it admits a complete catalog and verified
  file references so Rust can construct the canonical `SiteOutput`.

Neither is another semantic handoff. Every ordinary WASM Session owns an
independent engine; there is no process-global Session.

## Generator specifications

Generator specifications are closed tagged objects. Unknown fields fail.

```json
{ "generator": "cycle", "buildEpochSecs": 0 }
```

```json
{
  "generator": "publisher",
  "templateCoordinate": "hl7.fhir.template#1.0.0",
  "buildEpochSecs": 0,
  "activeTables": false
}
```

Cycle preparation creates a callback-free closed `cycle-site/v2` build and
roots every semantic and authored object needed by the external renderer.
Publisher preparation resolves and materializes the exact template base chain,
assembles page shells, `_data`, runtime/template/authored files, captures an
immutable Rust render state, and declares its complete output catalog before
any page is rendered.

## Fresh-process restoration

`SiteEngine::restore(closedBuild, contentStore)` reconstructs an executor from
an authenticated closed handoff. It is a lifecycle constructor, not a fifth
host operation and not a new domain value. Once restored, callers use only the
same handle-scoped `outputs`, `render`, and `finalize` operations.

Publisher restoration verifies the full object closure and recipe identities,
reconstructs the `PreparedGuide`, materialized template/runtime trees,
renderer-visible package view, Publisher model, render state, and output
catalog, then installs an ordinary immutable handle. It does not require the
original authored directory, package cache, process, or an opaque serialized
Rust runtime.

Cycle restoration verifies and installs the same closed external-builder
handle and addressed objects. Cycle remains callback-free; its LiquidJS host
owns catalog construction and rendering, then submits the complete result for
Rust finalization.

Restoration correctness requires identical catalogs, output `ContentRef`s and
bytes regardless of render order, and identical canonical `SiteOutput` bytes
after the original preparing engine has been dropped.

## Content references and finalization

`ContentRef` contains a SHA-256 digest, byte length, and optional media type.
Transport JSON carries references and metadata, never base64 site bodies.
Aliases may share one stored object.

Publisher `finalize` succeeds only when every declared output is ready and
verified. A host must publish all referenced objects before atomically
publishing the `SiteOutput` receipt. A `SiteOutputCache` may index a complete
verified output by the exact closed build, renderer implementation and recipe,
output schema, and options. A cache hit reconstructs that same `SiteOutput`; it
does not authorize a parallel cached representation.

For an external renderer, Rust verifies exact catalog equality, safe paths,
media types, and content references before constructing the receipt. Native Fig
also re-reads and authenticates the complete private staging tree. In the
browser, the host has already put and verified each referenced body in its
ContentStore before calling Rust. Renderer code cannot seal a second
authoritative receipt.

## Template acquisition

Rust owns template semantics. `resolveTemplate(coordinate)` walks
`package.json.base` and its exact dependency versions. The host may acquire a
reported missing coordinate as an ordinary package and retry. Malformed bases,
missing versions, and cycles fail loudly.

There is no pre-materialized template-directory host escape hatch. The selected
coordinate and complete base chain belong to the authenticated
`PackageEnvironment` and participate in build identity.

## Why there are two Liquid implementations

The host contract is shared; the renderer implementations intentionally differ:

| Architecture | Liquid engine | Resolution behavior |
| --- | --- | --- |
| Publisher-compatible native template | Rust `render_liquid` | registered generated fragments resolve synchronously inside the captured immutable `ArtifactResolver` |
| Cycle external builder | Cycle LiquidJS | all semantic and authored requirements are closed before execution; no Rust callback |

Publisher templates discover some generated includes only while evaluating
Liquid. Registered names map to typed artifact keys; the immutable resolver
returns a ready value or typed terminal observation and records the read. This
is private renderer behavior, not a public fragment API, file-miss callback, or
affine successor-handle protocol.

Cycle needs no callback because `cycle-site/v2` is eagerly closed. Its browser
and native hosts use the same LiquidJS renderer. Both paths end in the same
Rust-validated `SiteOutput` contract.

## Native Fig hosting

Fig is a transport over the same engine, not a staged-tree renderer:

```sh
# Publisher
fig prepare <ig-dir> \
  --target publisher-site/v1 \
  --template hl7.fhir.template#1.0.0 \
  --cache <package-cache> \
  --out <closed-bundle> \
  --build-date <epoch-or-RFC3339>
fig outputs <closed-bundle>
fig render <closed-bundle> en/index.html -o index.html
fig finalize <closed-bundle> -o <new-site-directory>

# Cycle: an external LiquidJS renderer fills a private staging tree and plan.
fig prepare <ig-dir> \
  --target cycle-site/v2 \
  --cache <package-cache> \
  --out <closed-bundle> \
  --build-date <epoch-or-RFC3339>
fig finalize <closed-bundle> \
  --site <private-staging> \
  --external-plan <plan.json> \
  --cache <optional-site-output-cache>
```

The bundle is exactly `site-build.json` plus `objects/sha256/<digest>`. Native
`outputs`, `render`, and `finalize` may each open it in a new process and invoke
`SiteEngine::restore`; no mutable `temp/pages`, generated include directory,
ambient package home, or prior Fig process is required.

Package acquisition remains separate from generation. Coordinates are exact,
and a compiled revision retains the resolver closure used for compilation.
Later package mounts affect only a later preparation.

## Removed surfaces

The following are not compatibility APIs and must not be restored:

- `site.db`, `cycle-site/v1`, row projections, and base64 object batches;
- public `mountSite -> mountTemplate -> produceStockSite -> openStockBuild`
  choreography;
- ambient `renderPage`, `renderFragment`, `listPages`, mutable renderer globals,
  and successor handles;
- staged Fig `fragment`, `fragments`, `produce`, build-root `render`, and `watch`;
- Fig's mutable `temp/pages` handoff, standalone template materializer,
  `--template-dir`, and page-only watch benchmark;
- host-authored Publisher fragments, runtime assets, or `SiteOutput` receipts.

The replacement is one captured project, one preparation, a complete catalog,
independent path rendering, verified content reads, and one canonical output
receipt.

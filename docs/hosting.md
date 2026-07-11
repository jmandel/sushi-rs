# Hosting the engine

The supported site-generation model has three domain values:

```text
PreparedGuide -> SiteBuild -> SiteOutput
                      |
                 ContentStore
```

`ContentStore` is digest-addressed byte plumbing, not a fourth model. Compiler
revisions, template trees, package locks, fragment observations, and opaque
handles are private implementation details or inputs to those values.

The editor's [`ARCHITECTURE.md`](../../../ARCHITECTURE.md) is authoritative for
the cross-repository boundary. This document describes the Rust/WASM side.

## The one site host facade

The public worker API is deliberately four operations:

```ts
prepare(project, generatorSpec): Promise<BuildHandle>
outputs(handle): Promise<OutputDescriptor[]>
render(handle, path): Promise<{ path: string; mediaType: string; content: ContentRef }>
finalize(handle): Promise<SiteOutput>
```

The worker serializes `prepare`, mounts the exact resolved package closure,
calls `compileProject` once, and immediately calls Rust `prepare`. Project bytes
cross the worker boundary once. A handle is opaque and immutable: rendering a
path does not create a successor handle, and A/B/A render order must produce the
same bytes as B/A/B.

The Rust `Session` binding exposes the same generation verbs as `prepare`,
`outputs`, `render`, and `finalize`. At this lower boundary compilation has
already installed the exact `ProjectRevision`, so Rust `prepare` accepts only a
generator specification. It rejects config, FSH, predefined resources, and
site-file bodies rather than allowing the host to resend or override a project.

Every ordinary `Session::new()` owns an independent `Engine`; there is no
process-global Session. Session calls use the shared result envelope:

```json
{ "apiVersion": 1, "ok": true, "op": "prepare", "result": {} }
```

Hosts must reject `ok:false`; `Session.version()` is the static exception.

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
retains its addressed objects. Cycle's LiquidJS renderer owns its output
catalog and path rendering. Publisher preparation materializes the exact base
template chain, assembles runtime/template/authored files, produces page shells
and `_data`, captures an immutable Rust render state, and declares its complete
output catalog before any page is rendered.

## ContentStore and finalization

`ContentRef` contains a SHA-256 digest, byte length, and optional media type.
JSON carries references and metadata, never file bodies. Two private worker
plumbing calls are intentional:

- `readContent(handle, digest)` returns a direct `Uint8Array` after digest
  verification.
- `finalizeExternal(handle, metadata)` lets the Cycle host submit only its
  complete catalog, file metadata, and verified `ContentRef`s. Rust checks exact
  catalog equality and constructs the canonical `SiteOutput`.

Publisher `finalize` succeeds only when every declared page is rendered.
Already-prepared assets are content-addressed during `prepare`; aliases share a
single stored body. A host publishes all referenced objects before publishing
the `SiteOutput` receipt.

## Template acquisition

Rust, not JavaScript, interprets template manifests. Private
`resolveTemplate(coordinate)` walks `package.json.base` and exact parent
dependencies. If it returns one missing exact coordinate, the host acquires and
mounts that ordinary package and retries. Malformed bases, missing dependency
versions, and cycles fail loudly.

This keeps package acquisition in the host while preserving one authoritative
template-chain algorithm in `package_store::template_loader`.

## Why the two renderer paths differ

There are two Liquid implementations, intentionally:

| Architecture | Liquid engine | Resolution behavior |
| --- | --- | --- |
| Publisher-compatible native template | Rust `render_liquid` | registered generated fragments resolve synchronously through the captured typed `ArtifactResolver` |
| Cycle external builder | Cycle LiquidJS | all semantic and authored requirements are closed before execution; no Rust callback |

Publisher templates can name a generated include only while evaluating Liquid.
The Rust facade keeps that discovery internal: registered names map to typed
artifact keys, the immutable resolver returns a ready value or a typed terminal
failure, and the page records its reads. There is no host callback, ambient
Session lookup, or affine "next handle". Cycle needs none of this because its
v2 contract is closed before LiquidJS runs.

## Native hosting

Rust callers should prefer typed crate APIs. `prepared_guide` owns source
preparation; `site_build` owns `SiteBuild`, closure proofs, `ContentRef`, and
`SiteOutput`; `site_producer` owns Publisher page/data/runtime assembly. `fig
prepare --target cycle-site/v2` publishes a native closed bundle for external
builders. `fig render` is the direct Publisher-template path.

Package acquisition and warm mounting remain separate from generation. Package
coordinates are exact, a compiled revision retains the resolution closure used
for compilation, and later mounts affect only a later compile.

## Removed surfaces

The following are not compatibility APIs and must not be restored:

- `site.db`, `cycle-site/v1`, row-shaped Cycle projections, and base64 object
  batches;
- `buildSiteBuildFromCompile`, `mountSite`, `mountTemplate`,
  `produceStockSite`, `openStockBuild`, and `renderStockPage`;
- ambient `renderPage`, `renderFragment`, `listPages`, `renderLiquid`,
  `renderMarkdown`, and `Session.global`.

The facade replaces that choreography with one compile, one preparation, a
complete catalog, independent path reads, and one canonical output receipt.

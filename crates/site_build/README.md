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
demand-driven renderer may return a new `SiteBuild` with a deferred artifact
resolved. A `RenderPlan` declares the roots a renderer needs. Converting to
`ClosedSiteBuild` follows their artifact read dependencies transitively and
fails with typed blockers unless the whole closure is `ready`. A callback-free
external builder should accept this proof-bearing wrapper, not an open
`SiteBuild`.

## `site.db` compatibility

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
`fig prepare --target cycle-site/v1` use this function. Fig supplies the exact
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
cargo test -p site_build --features site-db-compat
```

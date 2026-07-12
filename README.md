# sushi-rs

This repository is the Rust semantic and native-template engine for the FHIR IG
browser toolchain. It compiles FHIR Shorthand, resolves packages, generates
snapshots, closes callback-free Cycle renderer inputs, and assembles and renders
Publisher templates without Java or Jekyll. Generated Publisher fragments stay
inside the immutable renderer runtime rather than becoming a host API.

It is a compatibility stack: when stock SUSHI or the Java IG Publisher and the
spec disagree, the pinned tool output is the oracle unless an intentional break
is recorded. The browser editor is at
<https://joshuamandel.com/fhir-ig-editor/>.

## Architecture at a glance

The supported site-generation model contains three domain values:

```text
PreparedGuide -> SiteBuild -> SiteOutput
                      |
                 ContentStore
```

`ContentStore` carries verified digest-addressed bytes; it is not another
domain layer. Compiler revisions, exact package locks, template trees, fragment
observations, caches, and opaque handles are inputs or private implementation
details.

The browser host has one four-operation facade:

```text
prepare(project, generatorSpec) -> immutable handle
outputs(handle)                 -> complete path catalog
render(handle, path)            -> path + media type + ContentRef
finalize(handle)                -> canonical SiteOutput
```

Before the public call, a private config/template handshake asks Rust which
exact packages the host must acquire and mount. The complete project revision—
FSH, parsed predefined resources, and authored site bytes—then crosses once in
`prepare`; Rust compiles and prepares it in one executor transaction. Rendering
is independent by path: it memoizes bytes but neither consumes nor advances the
handle.

Cycle and Publisher use that facade without pretending their execution models
are identical. Cycle receives a closed `cycle-site/v2` build and renders it with
its shared LiquidJS implementation, without callbacks. Publisher materializes
the exact template base chain, assembles page shells, `_data`, fixed runtime
assets, and authored overlays in Rust, then renders with
[`render_liquid`](crates/render_liquid/). Registered generated fragments resolve
synchronously through the immutable typed `ArtifactResolver`; discovery never
becomes a host callback or an affine successor handle.

The output catalog is complete before page rendering. JSON carries
`ContentRef`s rather than base64 bodies; the private binary ContentStore returns
bytes by digest and deduplicates aliases. Both renderers finish as the same
validated [`SiteOutput`](crates/site_build/src/site_output.rs) receipt.

The repository exposes this implementation through typed Rust crates, an
isolated WASM `Session`, and the native `fig` CLI. `crates/api_envelope` is only
the WASM/`fig --json` transport envelope. There is no global Session and no
`site.db`/`cycle-site/v1` compatibility architecture.

The overall toolchain intentionally has two Liquid engines:

| Renderer architecture | Liquid implementation |
| --- | --- |
| Publisher-compatible native templates | Rust `render_liquid` |
| Cycle external builder | Cycle LiquidJS |

See [`docs/hosting.md`](docs/hosting.md) for the host API and
[`docs/site-producer.md`](docs/site-producer.md) for Publisher assembly. The
editor's `ARCHITECTURE.md` is the normative cross-repository contract.

## Parity (the regression floor)

| Layer | Result |
|---|---|
| Snapshots (walk engine) | **955/955** byte-identical vs the Java oracle |
| SUSHI harvest | 326/326 (256/256 byte-identical) + harvested spot checks |
| Page corpora (whole HTML) | plan-net **678/678**, us-core **1332/1334** (+2 classified), cycle **72/72** |
| Fragments | full used-fragment set byte-parity across cycle/plan-net/us-core |
| Package resolver | 8/8 IG-closure gate |

Those figures are regression-oracle results from the focused snapshot,
fragment, and page-corpus harnesses. The supported host path is the closed
SiteEngine lifecycle below; it does not consume an oracle's mutable build tree.

## Native quickstart — the canonical lifecycle

Build a Publisher site from authored project bytes and an explicit materialized
package cache:

```sh
cargo build --release -p fig

SOURCE_DATE_EPOCH=1783555200 target/release/fig prepare <ig-dir> \
  --target publisher-site/v1 \
  --template hl7.fhir.template#1.0.0 \
  --cache <package-cache> \
  --out publisher-build

target/release/fig outputs publisher-build
target/release/fig render publisher-build en/index.html -o index.html
target/release/fig finalize publisher-build -o site
```

`prepare` captures one immutable project revision and exact resolver-scoped
package environment, calls `SiteEngine::prepare_project`, and atomically writes
a closed bundle:

```text
publisher-build/site-build.json
publisher-build/objects/sha256/<digest>
```

The object store contains every byte addressed by the `ClosedSiteBuild`, not
only source files: package material, prepared semantic documents, authored
roles, the materialized template, and Publisher runtime inputs are all in the
verified closure. The package lock itself roots the exact
deterministic PreparedPackage carrier used by execution, so no second package
payload or renderer-package evidence is needed. The live project and package
cache are not consulted by later operations.

Each native `outputs`, `render`, or `finalize` invocation can start in a fresh
process. It authenticates the closed build and object closure, reconstructs an
ordinary SiteEngine runtime with `SiteEngine::restore`, and then invokes the
corresponding host operation. `restore` is lifecycle construction, not a fifth
host operation or another domain value. `finalize` renders any remaining
declared Publisher paths and atomically publishes the canonical `SiteOutput`
and its exact files.

Cycle uses the same preparation boundary but intentionally keeps its external
renderer:

```sh
SOURCE_DATE_EPOCH=1783555200 target/release/fig prepare <ig-dir> \
  --target cycle-site/v2 \
  --cache <package-cache> \
  --out cycle-build

# LiquidJS consumes cycle-build and writes a private complete staging tree
# plus its typed external-finalization plan. The plan's required inputBuildId
# is the exact build id LiquidJS opened; Fig rejects a changed/replaced bundle.
target/release/fig finalize cycle-build \
  --site <private-staging> \
  --external-plan <plan.json> \
  --cache <optional-site-output-cache>
```

The external builder owns Cycle's catalog and output bytes, while Rust verifies
the complete staged inventory and remains the sole constructor of the canonical
`SiteOutput`. This is the intended second Liquid implementation, not a second
build model.

Other subcommands (add `--json` to any command for the shared envelope):

```sh
fig build <ig-dir> -o fsh-generated       # FSH -> resources (SUSHI)
fig snapshot <sd.json> --package p#v       # walk-engine snapshot
fig resolve --cache <dir> --project <ig>   # dependency closure
fig packages bundle --cache <d> -o <d> id#v   # CDN-mountable package bundles
fig packages prepare --cache <d> -o <d> id#v  # versioned binary warm-mount artifacts
fig expand <valueset.json>                 # tier-1 enumerable expansion
fig prepare <ig> --target <publisher-site/v1|cycle-site/v2> ...
fig outputs <publisher-bundle>             # complete declared output catalog
fig render <publisher-bundle> <path>       # one independently rendered output
fig finalize <bundle> ...                  # one canonical SiteOutput
```

The deleted fragment/materialization commands, mutable page-tree producer,
build-root renderer, template-directory escape hatch, and staged watcher are
not compatibility APIs. Generated Publisher fragments are private reads of one
immutable render runtime. Template resolution is package-coordinate based, and
all public output paths come from `outputs(handle)`.

## Where to look

| Need | Current source |
| --- | --- |
| Cross-repository editor/renderer contract | editor [`ARCHITECTURE.md`](../../ARCHITECTURE.md) |
| `SiteBuild`, artifact states, hashing, render plans, and closure | [`crates/site_build/README.md`](crates/site_build/README.md) |
| Native exact compile → closed target bundle | [`crates/fig/src/prepare.rs`](crates/fig/src/prepare.rs) |
| Canonical package identity, binary warm artifacts, and derived index shared by native/WASM | [`crates/package_store/src/material.rs`](crates/package_store/src/material.rs), [`prepared.rs`](crates/package_store/src/prepared.rs) |
| Hosting `Session`, `fig`, native templates, or external builders | [`docs/hosting.md`](docs/hosting.md) |
| Source-driven page-shell and `_data` production | [`docs/site-producer.md`](docs/site-producer.md) |
| Current versus historical engine documents | [`docs/README.md`](docs/README.md) |
| Runnable examples | [`examples/`](examples/) and `scripts/examples-gate.sh` |

Obsolete Cycle v1 plans and worklogs were deleted with their APIs. The remaining
dated audits preserve oracle or measurement evidence, not alternate contracts.

## Package Acquisition Tutorial

The lower-level acquisition CLI (CAS ingest/acquire, lock, materialize) lives in
`rust_sushi` (its dev/acquisition subcommands stay for one release; the
user-facing `build`/`resolve`/`bundle` are now `fig build`/`fig resolve`/
`fig packages bundle`).

`rust_sushi` separates package acquisition into three layers:

```text
resolver/acquirer -> content-addressed store -> materialized package cache
```

- The **content-addressed store** (CAS) keeps immutable package artifacts under a
  sha256 digest.
- The **materialized package cache** is an explicit directory shaped like
  `.fhir/packages`: `<cache>/<name>#<version>/package/...`.
- The existing compiler and `package_store` read only the materialized cache. They
  never default to the user's real `~/.fhir`.

The default CAS path is `${XDG_CACHE_HOME:-~/.cache}/fhir-rs/cas`; set `FHIR_CAS`
or pass `--cas <dir>` to use another location.

### Ingest a Local Package

Use `cas ingest` for an explicit local package artifact or unpacked package
directory. This is useful for tests, local development packages, and `file:`-style
dependency workflows.

```sh
cargo run -p rust_sushi -- \
  cas ingest example.fhir.pkg#1.0.0 ./path/to/package-dir \
  --cas temp/fhir-cas
```

The source may be either:

- a `.tgz` FHIR package artifact, or
- a directory containing `package/package.json` or `package.json`.

The command canonicalizes local directories, computes the artifact sha256, extracts
the package into CAS, and writes coordinate refs. It refuses to ingest paths under
the real `~/.fhir`.

### Acquire a Published Package

Use `cas acquire` to resolve and download from FHIR package registries:

```sh
cargo run -p rust_sushi -- \
  cas acquire hl7.fhir.uv.subscriptions-backport.r4#1.1.0 \
  --cas temp/fhir-cas
```

By default the resolver tries:

1. `https://packages.fhir.org`
2. `https://packages2.fhir.org/packages`

These defaults and the `build.fhir.org` base live in
`crates/package_acquisition/resolution-config.json`, alongside the URL templates
that preserve SUSHI/FPL's FHIR-registry exact-version fallback behavior.

Use `--registry <url>` for a custom registry. `FHIR_REGISTRY` or `FPL_REGISTRY`
also override the default chain; custom registries use FPL's NPM-style fallback
tarball path.

Supported coordinates:

- exact versions, such as `1.1.0`
- `latest`
- `M.N.x`
- `current` and `current$branch` via `build.fhir.org`
- `dev`, which uses an existing explicit CAS ref or falls back to `current`

Mutable coordinates are snapshotted to a concrete sha256 when acquired. Lock-based
materialization and `build --materialize` never advance them.

### Materialize a Package Cache

Materialize one package into an explicit cache root. If the coordinate is missing
from CAS and `--offline` is not set, `materialize --package` resolves and acquires
it first.

```sh
cargo run -p rust_sushi -- \
  materialize --package hl7.fhir.uv.subscriptions-backport.r4#1.1.0 \
  --cas temp/fhir-cas \
  --out temp/fhir-cache
```

This creates:

```text
temp/fhir-cache/hl7.fhir.uv.subscriptions-backport.r4#1.1.0/package/...
```

Materialization creates a local package-cache view. When the package's own
`.index.json` is usable, `<cache>/<name>#<version>/package` is a directory
symlink to the immutable CAS package, so setup is one filesystem entry. If the
package index is missing or empty, materialization falls back to a real wrapper
directory with hardlinked/copied package files and installs a normalized
`.index.json` generated from the actual top-level JSON resources. The immutable
CAS copy remains verbatim.

### Lock a Project

Project locks snapshot the full SUSHI-compatible dependency load set: low
automatic dependencies, configured dependencies, FHIR core, and high automatic
dependencies. The lock records the requested coordinate, effective version,
materialized cache label, source, and sha256 for each package.

```sh
cargo run -p rust_sushi -- \
  deps lock --project /path/to/ig \
  --lock /path/to/ig/fhir-deps.lock \
  --cas temp/fhir-cas
```

To advance mutable coordinates intentionally, update the lock explicitly:

```sh
# Update every mutable request: latest, M.N.x, current, current$branch, dev.
cargo run -p rust_sushi -- \
  deps update --project /path/to/ig --all-mutable \
  --cas temp/fhir-cas

# Or update selected mutable package requests.
cargo run -p rust_sushi -- \
  deps update --project /path/to/ig hl7.terminology.r4 \
  --cas temp/fhir-cas
```

`deps lock --offline` and `deps update --offline` never hit the network. They only
reuse existing CAS refs and locked digests, and fail loudly on misses.

### Materialize a Project

Materialize from a lock without re-resolving mutable coordinates:

```sh
cargo run -p rust_sushi -- \
  materialize --lock /path/to/ig/fhir-deps.lock \
  --cas temp/fhir-cas \
  --out temp/fhir-cache
```

Or ask the tool to use `/path/to/ig/fhir-deps.lock`, creating it if it does not
exist:

```sh
cargo run -p rust_sushi -- \
  materialize --project /path/to/ig \
  --cas temp/fhir-cas \
  --out temp/fhir-cache
```

When a lock exists, `materialize --project` is deterministic: it trusts locked
digests and does not rewrite the lock. If CAS content for a locked digest is
missing, non-offline mode can restore it from the lock's recorded source only when
the bytes hash back to the locked sha256.

### Build with a Materialized Cache

The compiler can read any explicit cache via `--cache`:

```sh
cargo run -p rust_sushi -- \
  build /path/to/ig -o temp/rust-ig \
  --cache temp/fhir-cache
```

For a one-command self-reliant build, let `rust_sushi` materialize first and then
compile against that cache:

```sh
cargo run -p rust_sushi -- \
  build /path/to/ig -o temp/rust-ig \
  --materialize \
  --cas temp/fhir-cas
```

By default this materializes into `<out>/.rust_sushi/fhir-cache`; pass
`--cache <dir>` with `--materialize` to choose another cache root. If
`fhir-deps.lock` already exists, `build --materialize` uses it without refreshing
mutable entries. If no lock exists, it creates one first unless `--offline` is set.

All acquisition and materialization targets remain explicit. The real `~/.fhir`
cache is not a source or target for acquisition, materialization, or builds.

### Inspect Package Resolution

Use `pkg-fish` to check what the materialized cache resolves for package resources
by id, name, or canonical URL:

```sh
cargo run -p rust_sushi -- \
  pkg-fish /path/to/ig temp/fhir-cache Patient \
  http://hl7.org/fhir/StructureDefinition/Patient
```

For regression checks, `harness/acquisition-pkg-fish.sh` locks and materializes
each selected IG, generates package queries from the materialized cache, and diffs
`rust_sushi pkg-fish` against stock SUSHI using the same package content.

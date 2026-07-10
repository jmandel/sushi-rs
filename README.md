# sushi-rs

This repository is the Rust semantic and native-template engine for the FHIR IG
browser toolchain. It compiles FHIR Shorthand, resolves packages, generates
snapshots, projects Cycle-compatible data, produces Publisher fragments and page
shells, and renders stock Publisher templates without Java or Jekyll.

It is a compatibility stack: when stock SUSHI or the Java IG Publisher and the
spec disagree, the pinned tool output is the oracle unless an intentional break
is recorded. The browser editor is at
<https://joshuamandel.com/fhir-ig-editor/>.

## Architecture at a glance

The repository exposes the same Rust implementation through three host
surfaces, but they are not all JSON wrappers:

| Surface | Contract | Where |
| --- | --- | --- |
| Rust libraries | typed Rust values, traits, and errors | workspace crates |
| `Session` | isolated in-memory engine; JSON result/error envelopes at the WASM boundary | [`crates/wasm_api`](crates/wasm_api/) |
| `fig` | native filesystem/process host; `--json` uses the same envelope shape as `Session` | [`crates/fig`](crates/fig/) |

`crates/api_envelope` is shared by `Session` and `fig --json`. It is not the
library API. A normal `Session::new()` owns isolated mutable engine state;
`Session::global()` is an explicitly named compatibility door for legacy native
callers.

Compilation has two current renderer branches. The closed value contract is
landed end to end for external builders; the native branch already uses its
typed artifact vocabulary but has not yet persisted discovered results as new
CAS-backed builds:

```text
exact authored bytes + exact resolved packages
                    |
             compileProject once
                    v
          compiled project revision
          /                     \
         v                       v
native session/template      external target projection
typed ArtifactResolver              |
+ page read sets                  SiteBuild
         |                          |
 future CAS SiteBuild          ClosedSiteBuild
 revisions                     + pure builder
```

[`site_build`](crates/site_build/) defines an immutable manifest over the exact
source and package closure, render target, typed artifacts, provenance,
diagnostics, and artifact read dependencies. Artifact bytes are addressed by
digest and live outside the manifest. `ClosedSiteBuild` proves that a render
plan's roots and transitive reads are all ready; callback-free external builders
should accept this closed wrapper. Persisting native demand-resolved fragment
bytes/read sets as new `SiteBuild` revisions remains explicit convergence work.

`site.db` is not the universal handoff. It is one compatibility artifact for the
Cycle target (`compat.site_db/rows.json`) inside a closed build. Native Publisher
templates may discover generated includes while evaluating Rust Liquid. At that
one compatibility edge, `render_page` translates the legacy include filename to
an `ArtifactKey`, calls an explicit `ArtifactResolver`, caches by the typed key,
and records attempted and successful reads in `PageArtifactReadSet`. Ordinary
authored and template includes remain files.

The overall toolchain intentionally has two Liquid implementations:

| Renderer architecture | Liquid implementation |
| --- | --- |
| native Publisher templates (`fig` and WASM stock renderer) | this repository's Rust [`render_liquid`](crates/render_liquid/) crate |
| Cycle external builder (native CLI and browser) | Cycle's shared LiquidJS content implementation |

There is one Liquid implementation per renderer architecture. Cycle does not
use `Session.renderLiquid`; its browser and CLI share Cycle's own renderer and
content policy over a `SiteBuildView`. See the workspace-level architecture for
the cross-repository seams.

## Parity (the regression floor)

| Layer | Result |
|---|---|
| Snapshots (walk engine) | **955/955** byte-identical vs the Java oracle |
| SUSHI harvest | 326/326 (256/256 byte-identical) + harvested spot checks |
| Page corpora (whole HTML) | plan-net **678/678**, us-core **1332/1334** (+2 classified), cycle **72/72** |
| Fragments | full used-fragment set byte-parity across cycle/plan-net/us-core |
| Package resolver | 8/8 IG-closure gate |

`fig render` reproduces those page numbers byte-for-byte — it composes the same
F5/F6 machinery the page corpora gate (`crates/render_page/src/bin/pagecorpus.rs`).

## Quickstart — `fig render`

Render a completed build tree to a static site at Publisher parity:

```sh
cargo build --release -p fig
target/release/fig render <build-dir> -o site/
#   build-dir = a staged build (temp/pages + output/ + .home/.fhir/packages +
#   input-cache/txcache), e.g. an F0 build. Byte-identical to the Java Publisher's
#   Jekyll output; 678 plan-net pages render in ~0.6s.

target/release/fig render <build-dir> -o site/ --template hl7.fhir.template#1.0.0
#   --template <id#ver> is the DRIVEN default: fetch + materialize any template
#   chain (walk `base`, union-copy, _append concat, config deep-merge) in pure
#   Rust — ZERO XSLT/ant. Byte-exact vs the Java Publisher's template/ tree
#   (gate: crates/package_store/tests/template_materialization_gate.rs).
#   --template-dir <dir> uses a pre-materialized tree (escape hatch).
```

Other subcommands (add `--json` to any for the shared envelope):

```sh
fig build <ig-dir> -o fsh-generated       # FSH -> resources (SUSHI)
fig snapshot <sd.json> --package p#v       # walk-engine snapshot
fig resolve --cache <dir> --project <ig>   # dependency closure
fig packages bundle --cache <d> -o <d> id#v   # CDN-mountable package bundles
fig packages bundle --template id#v -o t.json  # editor warm-start template artifact (loader-emitted)
fig expand <valueset.json>                 # tier-1 enumerable expansion
fig sitedb <ig> --sushi-out <d> --cache <d> -o site.db   # S1-S7 producer
fig prepare <ig> --target cycle-site/v1 --sushi-out <new> \
  --cache <d> --out <new> --build-date <epoch>  # sealed external-builder bundle
fig fragment <build-dir> <ref> <kind>      # ONE publisher-parity fragment
fig fragments <build-dir> -o _includes/    # materialize fragment files (escape hatch)
fig watch <build-dir> --serve :8080        # incremental dev loop + live-reload
```

The former `fig render --generator ts:<adapter.mjs>` runner has been removed.
It loaded a stale editor callback API, required a second Node-target WASM host,
and could compile inputs independently of the native build. Portable external
builders instead consume a verified `ClosedSiteBuild` and addressed objects.
Cycle uses that boundary in the browser, and `fig prepare` now emits the same
`cycle-site/v1` contract natively as `site-build.json` plus
`objects/sha256/<digest>`. It compiles once through `site_db::build`, derives
identity from the produced IG, and gives the build only a private filesystem
reconstructed from the captured source objects and normalized package objects.
The live authored tree/cache are never compile inputs after capture, so even an
A→B→A mutation cannot pair rows for B with A's manifest. Post-build live and
staged comparisons remain fail-closed diagnostics.
Both `--sushi-out` and `--out` must be new, disjoint directories; the package
cache is always explicit, no network or `~/.fhir` fallback is used, and a build
timestamp is required (`--build-date` or `SOURCE_DATE_EPOCH`).
Cycle's native consumer is then
`SITE_BUILD_DIR=<bundle> bun site-gen/build.tsx`; it verifies the manifest,
reachable artifact closure, digest, and byte length before rendering through
the same `CycleSiteRenderer` used in the browser.

`fig watch --serve` is the native twin of the browser editor: an mtime poll →
dirty cone (via the fragment read-set boundary) → re-render only dirtied pages →
serve with live-reload. Warm page edits re-render in ~270 ms on us-core.

## Where to look

| Need | Current source |
| --- | --- |
| Cross-repository editor/renderer contract | [`fhir-ig-editor/SPEC.md`](https://github.com/jmandel/fhir-ig-editor/blob/main/SPEC.md) |
| `SiteBuild`, artifact states, hashing, render plans, and closure | [`crates/site_build/README.md`](crates/site_build/README.md) |
| Native exact compile → closed external-builder bundle | [`crates/fig/src/prepare.rs`](crates/fig/src/prepare.rs) |
| Canonical package identity, derived index, and lock bytes shared by native/WASM | [`crates/package_store/src/material.rs`](crates/package_store/src/material.rs) |
| Hosting `Session`, `fig`, native templates, or external builders | [`docs/hosting.md`](docs/hosting.md) |
| Source-driven page-shell and `_data` production | [`docs/site-producer.md`](docs/site-producer.md) |
| Current versus historical engine documents | [`docs/README.md`](docs/README.md) |
| Runnable examples | [`examples/`](examples/) and `scripts/examples-gate.sh` |

The phase plans and render worklog remain useful derivation evidence, but they
do not define the current API. Their banners point back to the current
architecture.

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

# sushi-rs

Rust port of SUSHI, the FHIR Shorthand compiler. This repository is a
compatibility compiler: when stock SUSHI and the FSH spec disagree, stock SUSHI is
the oracle unless an intentional compatibility break is recorded.

## Package Acquisition Tutorial

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

Materialization verifies the CAS manifest, then hardlinks package files into the
cache when possible and falls back to copying across filesystems. The materialized
package gets a normalized `.index.json` generated from its actual top-level JSON
resources; the immutable CAS copy remains verbatim.

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

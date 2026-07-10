# Self-Reliant Package Acquisition: Content-Addressed Store + Materialize

> **Historical design plan.** Package acquisition, the CAS, locks, and explicit
> materialization are implemented. See the repository [`README.md`](../../README.md)
> for the current CLI and invariants; this document preserves the design and
> rollout rationale.

## Motivation
At the time of this plan, `package_store` only **read** a
`.fhir/packages`-shaped directory; packages got there via **stock SUSHI /
fhir-package-loader**. That was a self-reliance gap: the runtime depended on
another tool to acquire dependencies. The plan was to:

1. **Acquire packages ourselves** from FHIR registries (no stock SUSHI).
2. Keep an **immutable, content-addressed store (CAS)** of downloaded packages
   (like npm/pnpm content-addressable stores), shared across projects/builds.
3. Have a fast **`materialize`** that builds a `.fhir/packages`-shaped directory
   from the CAS — instant when the CAS is populated, populating it (downloading) on
   miss.
4. Default CAS path in the user's home, env-overridable; the **materialize target is
   always explicit** (we can recreate it cheaply, so we never default to real `~/.fhir`).

Grounding facts (verified): the FHIR registry is npm-style — `GET <registry>/<name>`
returns `{ versions: { "<v>": { dist: { shasum, tarball }, fhirVersion, url, ... } } }`.
`dist.shasum` (sha1 of the .tgz) is a ready content key; `dist.tarball` is the
download URL (often Simplifier-hosted via a `packages.fhir.org` redirect). Some
published tarballs ship an empty `.index.json` (see `package-store-notes.md`) — our
read-side dir-reconcile already handles that, so the CAS stores content **verbatim**.

## SUSHI / FPL source behavior to preserve

Stock SUSHI's content acquisition path is:

1. `app.ts` creates `FHIRDefinitions`, calls `loadExternalDependencies`, then loads
   predefined local resources and optimizes the FPL DB.
2. `FHIRDefinitions` constructs FPL with:
   - disk cache at `<home>/.fhir/packages` (we replace this with explicit materialize dirs),
   - `DefaultRegistryClient`,
   - `BuildDotFhirDotOrgClient`,
   - `DiskBasedPackageCache`.
3. `loadExternalDependencies` computes deterministic load order:
   - group configured dependencies by package id, sort same-id versions ascending,
   - append FHIR core from `fhirVersion`,
   - low automatic deps (`hl7.fhir.uv.tools.*`, `hl7.terminology.*`),
   - configured deps plus core,
   - high automatic deps (`hl7.fhir.uv.extensions.*`).
   The read-side resolver is LIFO, so this order is semantic.
4. FPL resolves/downloads:
   - default registry chain: `https://packages.fhir.org`, then
     `https://packages2.fhir.org/packages`;
   - custom registry: `FPL_REGISTRY`, treated as an npm registry, with optional
     bearer token `FPL_REGISTRY_TOKEN`;
   - `latest`: registry `dist-tags.latest`;
   - `M.N.x`: max satisfying version from registry metadata;
   - exact versions: used as-is;
   - `current` / `current$branch`: `https://build.fhir.org/ig/qas.json`, newest
     matching `package-id` and `main`/`master` (or explicit branch), then
     `<build-base>/package.tgz`;
   - `dev`: local cache only; if missing, FPL falls back to `current`.
5. FPL extraction cleans malformed packages by moving top-level extracted siblings
   into `package/` when a `package/` directory exists. We need the same cleanup
   before storing CAS content.

Implementation rule: the acquisition side may have better reproducibility metadata
than FPL, but it must produce the same package bytes/load set that stock SUSHI
would load for the same coordinates.

The server list and URL-shape rules are intentionally data-driven in
`crates/package_acquisition/resolution-config.json`: the default registries use
FHIR-style exact-version fallback downloads (`{registry}/{name}/{version}`), while
custom registries keep FPL's NPM-style fallback path
(`{registry}/{name}/-/{name}-{version}.tgz`). This externalizes the defaults
without changing SUSHI-compatible behavior.

## Architecture — three layers

```
resolver/acquirer  ->  CAS (immutable store)  ->  materializer (project)
 local/remote .tgz      addressed by digest        hardlink -> package-cache tree
```

### Layer 1 — Resolver / acquirer (`acquire`)
- **Registry chain** (configurable, env `FHIR_REGISTRY` / `--registry`): default
  `https://packages.fhir.org` (+ `https://packages2.fhir.org/packages` fallback);
  `build.fhir.org` for `current`/`current$branch` mutable coords. Custom/private
  registries supported.
- **Resolve**: `GET <registry>/<name>` -> pick version (exact, or a range / `latest`
  / mutable tag) -> `dist.tarball` + `dist.shasum` when supplied by that source.
- **Download + verify**: fetch the `.tgz`; verify `shasum` (sha1) and also compute a
  **sha256** (our canonical CAS key — stronger, and not provided by the registry).
- **Local sources**: explicit local `.tgz` artifacts use the same tarball sha256
  path. Explicit unpacked package directories are first converted to a deterministic
  canonical package artifact (sorted paths, normalized metadata) and then ingested,
  so every CAS entry still has one artifact digest.
- **Mutable coords** (`latest`, `M.N.x`, `current`, `current$branch`, `dev`): resolve
  to a concrete content digest **only when acquiring/updating the lock**. Ordinary
  materialize/build runs reuse the lock and never silently advance mutable refs.
  `dev` is satisfied only by an existing explicit CAS ref; if missing, fall back to
  `current` and record that fallback.
- **Dependency closure**: reuse the dep-resolution we already ported in
  `package_store` (auto FHIR core for `fhirVersion` + `loadAutomaticDependencies` +
  config `dependencies`, no transitive package.json walk — matches stock's *load*
  set). Iterate: resolve the set -> for each not in CAS, `acquire`.
- **`--offline`**: never hit the network; fail loudly if a needed package isn't in CAS.

### Layer 2 — CAS (immutable store)
- **Path**: default `${XDG_CACHE_HOME:-~/.cache}/fhir-rs/cas` (NOT `~/.fhir`),
  override `FHIR_CAS`. Write-once, read-only entries; safe to share concurrently.
- **Layout (package-granular, recommended first):**
  ```
  cas/
    packages/<sha256>/package/...     # extracted package tree, read-only
    packages/<sha256>/manifest.json   # file list + source metadata for verification
    tarballs/<sha256>.tgz             # original/canonical tarball, for re-export/audit
    refs/<encoded-coordinate>.json    # { sha256, shasum, source, fetched_at, mutable }
  ```
  - Key = **sha256 of the package artifact bytes** (downloaded `.tgz`, local `.tgz`,
    or canonicalized local directory artifact). `refs/` maps a human coordinate to
    a content sha; mutable coords may re-point over time, but each digest dir is
    immutable.
  - Extract once on ingest; `chmod -R a-w`. FHIR packages share few identical files,
    so package-granularity dedup is enough. (If disk pressure appears, evolve to a
    **pnpm-style file-granular CAS** `cas/files/<sha>` + per-package manifest; the
    materialize hardlink model below is unchanged.)
- **Integrity**: a digest dir is valid iff the stored artifact hashes to the dir name
  and the extracted tree matches the recorded manifest. Spot-check the manifest on
  materialize; full re-verify on demand.

### Layer 3 — Materializer (`materialize`)
- `rust_sushi materialize --out <cache-dir> (--project <ig> | --lock <lockfile>) [--offline]`:
  1. Determine the package set (project dep resolution, or the lockfile).
  2. Ensure each digest is in CAS. In `--lock` mode, a missing digest may be fetched
     only from the artifact source recorded in the lock and must verify to the locked
     sha256; `--offline` fails instead. In project/no-lock mode, acquisition may
     resolve coordinates and then write the lock.
  3. Build `<cache-dir>/<name>#<materialized_version>/package/...` by **hardlinking**
     each file from `cas/packages/<sha256>/package/...` (instant, zero extra disk,
     immutable source). Fall back to reflink/copy across filesystems.
  4. Rewrite `<package>/.index.json` in the materialized cache from the actual
     top-level JSON resources, sorted in FPL scan order, including SD metadata such
     as `derivation`. The CAS content remains verbatim; the materialized read side
     gets an index it can trust.
  5. Write/update the lockfile only for project mode when creating/completing a lock;
     `--lock` mode never mutates the lock.
- **Fast path**: CAS fully populated -> materialize is pure hardlinks (sub-second for
  a full dep set). This is the "fast when the store is populated" goal.
- The materialized `<cache-dir>` is the explicit `FHIR_CACHE` / package-cache root
  that `package_store` reads — unchanged read-side. Because materialize is cheap,
  builds use an **ephemeral, explicit** materialize dir; **never** real `~/.fhir`.

## Lockfile & reproducibility
`fhir-deps.lock` (per project / CI): the resolved graph + per package
`{ name, requested, effective_version, materialized_version, sha256, shasum,
source, registry, tarball_url, build_url, build_date, mutable, fallback,
fetched_at }`.
Resolution states:
- **locked** — exact version + tarball digest known (CI default; fully reproducible).
- **snapshotted** — a mutable coord (`current`/`dev`) resolved to a digest at time T
  (reproducible from CAS, labeled).
- **floating** — not a build/materialize guarantee. Raw mutable project requests may
  enter the resolver, but they must become locked/snapshotted before a package-cache
  tree is materialized.
`materialize --lock` is deterministic: it only materializes digests named in the
lock and never re-resolves mutable coordinates.

### Mutable coordinate policy

Mutable coordinates are never advanced as a side effect of `materialize --lock`,
`materialize --project` when a lock exists, or `build --materialize`. They advance
only through explicit lock update commands:

```sh
# Create or complete a lock. Missing entries are resolved/acquired; existing
# locked/snapshotted entries are reused.
rust_sushi deps lock --project <ig> [--lock fhir-deps.lock]

# Re-resolve selected mutable entries and update the lock/CAS.
rust_sushi deps update --project <ig> [--lock fhir-deps.lock] <package-id>...

# Re-resolve every mutable entry (`latest`, `M.N.x`, `current`, `current$branch`,
# and `dev` if it has an explicit CAS ref or falls back to current).
rust_sushi deps update --project <ig> [--lock fhir-deps.lock] --all-mutable
```

Rules:
- `latest` and `M.N.x` store `requested` as written, `effective_version` as the
  registry-resolved concrete version, and `materialized_version` as that concrete
  version.
- exact versions store the same value in `requested`, `effective_version`, and
  `materialized_version`.
- `current` / `current$branch` store `effective_version` as the mutable coord,
  `materialized_version` as the FPL cache label (`current` or `current$branch`),
  plus `build_url`, `build_date` if available, and sha256.
- `dev` stores `materialized_version=dev` only when an explicit CAS ref already
  exists. If not, it records `requested=dev`, `effective_version=current`,
  `fallback=true`, and materializes as `current` to match FPL fallback behavior.
- Updating a mutable entry is a lockfile diff. Developers review and commit it like
  any dependency bump.

## CAS population sources
CAS can be populated from:
- registry/build-server tarballs acquired by this tool;
- explicit local sources named by a command or dependency resolver entry, such as
  a `.tgz` package artifact or an unpacked package directory;
- explicitly named tarball fixtures in tests.

The resolver stack for acquisition/update is:

```text
lock digest -> explicit local source -> CAS coordinate ref -> remote registry/build resolver
```

For a locked materialize/build, the lock digest is authoritative and no resolver is
consulted. For acquisition/update, an explicit local source is ingested into CAS
before network resolution. This mirrors npm-style `file:`/tarball locality while
keeping the build side content-addressed.

There is **no** `cas import --from ~/.fhir/packages` path, and no code should ingest
a FHIR cache tree into CAS. Any load/import command must reject paths resolving
under the real home `.fhir`.

## Build integration
- `rust_sushi build <ig> -o <out> --materialize` : materialize the project's deps
  (CAS -> ephemeral dir) then build against it — fully self-reliant, one command.
- If `fhir-deps.lock` exists, `build --materialize` uses it deterministically and
  does not refresh mutable entries. If no lock exists, it creates one with the same
  semantics as `deps lock`.
- `rust_sushi build <ig> -o <out> --cache <dir>` : build against an already-materialized
  (or any explicit) cache — current behavior, still required by default (no implicit
  `~/.fhir`).

## Safety / determinism
- CAS dir + materialize target are explicit (env/arg); **never** default to `~/.fhir`;
  any code path that receives an import/materialize/cache source must reject the
  user's real `.fhir` tree.
- Verify sha256 on materialize (catch CAS corruption); CAS entries read-only.
- `--offline` for hermetic/CI builds.
- `--offline` with a lock may materialize only from existing CAS content, or may
  fail loudly if a named digest is absent; it never falls back to mutable lookup.
- The downloader must produce the **same load set** stock ends up with (gate:
  `pkg-fish` parity + a full build byte-diff against a stock-seeded cache).
- Package resource listing is shared through `package_store::package_resource_entries`:
  non-empty materialized indexes are trusted, empty/missing indexes fall back to
  the same sorted package JSON scan. IG dependency URL resolution uses this same
  helper, avoiding a separate scan path for `ImplementationGuide` metadata.

## Phases & gates
1. **Registry client** — resolve + download one package (+ verify shasum/sha256) to a
   temp dir. Gate: fetch `subscriptions-backport.r4@1.1.0`, sha matches `ca5e4f4c...`.
2. **CAS + materialize** — ingest -> hardlink a `.fhir/packages` tree. Gate: a
   materialized cache builds IPS byte-identical (665-corpus) to the stock-seeded cache.
3. **Dependency closure acquire** — resolve a project's full set, fetch all missing.
   Gate: materialize-from-empty-CAS for the 4 corpus + holdout IGs, then `full-dashboard`
   matches the stock-seeded numbers; `pkg-fish` parity.
4. **Lockfile + locked/snapshotted resolution states** + manifest, including
   `deps lock` and explicit `deps update --all-mutable`.
5. **`--offline`** hermetic materialize/build from CAS + lock.
6. **`build --materialize`** wiring.

Implementation status (2026-06-30):
- Implemented: registry/build acquire, local `.tgz` and unpacked-directory ingest,
  package-granular CAS, manifest verification, hardlink materialization, project
  lock/update, locked-source recovery, `--offline`, `materialize --lock|--project`,
  `build --cache`, `build --materialize`, and normalized materialized package
  indexes.
- Verified so far: focused unit tests, local package materialization tests,
  offline project lock/materialize/build smoke, one live registry package
  shasum/materialize gate, and acquisition-backed byte parity for the 4 tuning IGs
  (IPS/epi/mCODE/CRD). `us.nlm.vsac#0.19.0` resolves through the second default
  registry (`https://packages2.fhir.org/packages`) after the first default
  endpoint 404s. `harness/acquisition-dashboard.sh` compares Rust builds from the
  stock-seeded isolated cache vs Rust builds from acquisition-materialized caches;
  it is 1840/1840 byte-identical across the 4 tuning IGs + 8 holdouts. Genomics
  still hits the known compiler panic on both paths, but the generated subset is
  identical, so acquisition adds no extra drift there.
- `harness/acquisition-pkg-fish.sh` compares stock SUSHI package fishing against
  Rust `pkg-fish` using the same acquisition-materialized package content on both
  sides; it is 4800/4800 across the same 12 IGs. This harness deliberately avoids
  the older stock-seeded cache for package-content queries, because that isolated
  cache is missing three current `hl7.fhir.r4.core#4.0.1` helper definitions
  (`structuredefinition-{json,rdf,xml}-type`) that a fresh stock FPL download and
  acquisition both include.

## Risks
- Registry quirks: redirects (packages.fhir.org -> simplifier), auth/rate-limits,
  mutable-coord semantics. Mirror FPL's registry chain + error handling.
- Reproducibility of `current`/`dev` — snapshot-to-digest + label.
- Disk: package-granular dedup may be loose; revisit file-granular CAS if needed.
- The acquire-side load set MUST equal stock's (we own the read-side resolution;
  reuse it so acquire and load agree by construction).

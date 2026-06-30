# Self-Reliant Package Acquisition: Content-Addressed Store + Materialize

## Motivation
Today `package_store` only **reads** a `.fhir/packages`-shaped directory; the
packages get there via **stock SUSHI / fhir-package-loader**. That's a self-reliance
gap — our runtime depends on another tool to acquire dependencies. We want to:

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

## Architecture — three layers

```
registry client (acquire)  ->  CAS (immutable store)  ->  materializer (project)
   download .tgz, verify        addressed by digest        hardlink -> .fhir/packages tree
```

### Layer 1 — Registry client (`acquire`)
- **Registry chain** (configurable, env `FHIR_REGISTRY` / `--registry`): default
  `https://packages.fhir.org` (+ `packages2.fhir.org` fallback); `build.fhir.org`
  for `current`/`dev` mutable coords. Custom/private registries supported.
- **Resolve**: `GET <registry>/<name>` -> pick version (exact, or a range / `latest`
  / mutable tag) -> `dist.tarball` + `dist.shasum`.
- **Download + verify**: fetch the `.tgz`; verify `shasum` (sha1) and also compute a
  **sha256** (our canonical CAS key — stronger, and not provided by the registry).
- **Mutable coords** (`current`/`dev`): resolve to a concrete content digest **at
  fetch time** (snapshot), record the resolution timestamp + source, and label the
  guarantee level (see Lockfile).
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
    packages/<sha256>/            # the extracted `package/` tree of one tarball, read-only
    tarballs/<sha256>.tgz         # (optional) the original tarball, for re-export/audit
    refs/<name>#<version>.json    # { sha256, shasum, registry, tarball_url, fetched_at, mutable }
  ```
  - Key = **sha256 of the tarball bytes** (content address). `refs/` maps a
    human coordinate to a content sha (one coordinate can map to a digest; mutable
    coords may re-point over time, but each digest dir is immutable).
  - Extract once on ingest; `chmod -R a-w`. FHIR packages share few identical files,
    so package-granularity dedup is enough. (If disk pressure appears, evolve to a
    **pnpm-style file-granular CAS** `cas/files/<sha>` + per-package manifest; the
    materialize hardlink model below is unchanged.)
- **Integrity**: a digest dir is valid iff its content re-hashes to its name (cheap
  to spot-check; full re-verify on demand).

### Layer 3 — Materializer (`materialize`)
- `rust_sushi materialize --out <dir> (--project <ig> | --lock <lockfile>) [--offline]`:
  1. Determine the package set (project dep resolution, or the lockfile).
  2. Ensure each is in CAS (Layer 1 `acquire` on miss -> populates CAS).
  3. Build `<dir>/packages/<name>#<version>/package/...` by **hardlinking** each file
     from `cas/packages/<sha256>/` (instant, zero extra disk, immutable source). Fall
     back to reflink/copy across filesystems.
  4. Write/update the lockfile.
- **Fast path**: CAS fully populated -> materialize is pure hardlinks (sub-second for
  a full dep set). This is the "fast when the store is populated" goal.
- The materialized `<dir>` is the explicit `FHIR_CACHE` that `package_store` reads —
  unchanged read-side. Because materialize is cheap, builds use an **ephemeral,
  explicit** materialize dir; **never** real `~/.fhir`.

## Lockfile & reproducibility
`fhir-deps.lock` (per project / CI): the resolved graph + per package
`{ name, version, sha256, shasum, registry, tarball_url, mutable: bool, fetched_at }`.
Guarantee levels (from `sushi-rust-port-plan.md`):
- **locked** — exact version + tarball digest known (CI default; fully reproducible).
- **snapshotted** — a mutable coord (`current`/`dev`) resolved to a digest at time T
  (reproducible from CAS, labeled).
- **floating** — mutable, unpinned: allowed with a warning + a non-reproducible-build
  note in the manifest.
`materialize --lock` is deterministic: it only hardlinks digests named in the lock.

## Seeding the CAS from an existing `~/.fhir` (no re-download)
`rust_sushi cas import [--from ~/.fhir/packages]` (read-only on the source): for each
`<name>#<version>/`, re-tar (or hash the tree) -> compute sha256 -> store into CAS +
write `refs/`. Lets us populate the CAS from the user's existing 7.6 G cache offline,
instantly self-sufficient. (We read `~/.fhir` but **never write** it — the hard rule.)

## Build integration
- `rust_sushi build <ig> -o <out> --materialize` : materialize the project's deps
  (CAS -> ephemeral dir) then build against it — fully self-reliant, one command.
- `rust_sushi build <ig> -o <out> --cache <dir>` : build against an already-materialized
  (or any explicit) cache — current behavior, still required by default (no implicit
  `~/.fhir`).

## Safety / determinism
- CAS dir + materialize target are explicit (env/arg); **never** default to `~/.fhir`;
  reuse the `harness/_guard.sh` fail-loud guards for any `~/.fhir` access (read-only import only).
- Verify sha256 on materialize (catch CAS corruption); CAS entries read-only.
- `--offline` for hermetic/CI builds.
- The downloader must produce the **same load set** stock ends up with (gate:
  `pkg-fish` parity + a full build byte-diff against a stock-seeded cache).

## Phases & gates
1. **Registry client** — resolve + download one package (+ verify shasum/sha256) to a
   temp dir. Gate: fetch `subscriptions-backport.r4@1.1.0`, sha matches `ca5e4f4c...`.
2. **CAS + materialize** — ingest -> hardlink a `.fhir/packages` tree. Gate: a
   materialized cache builds IPS byte-identical (665-corpus) to the stock-seeded cache.
3. **Dependency closure acquire** — resolve a project's full set, fetch all missing.
   Gate: materialize-from-empty-CAS for the 4 corpus + holdout IGs, then `full-dashboard`
   matches the stock-seeded numbers; `pkg-fish` parity.
4. **Lockfile + guarantee levels** (locked/snapshotted/floating) + manifest.
5. **`cas import`** (seed from `~/.fhir`) + `--offline`.
6. **`build --materialize`** wiring.

## Risks
- Registry quirks: redirects (packages.fhir.org -> simplifier), auth/rate-limits,
  mutable-coord semantics. Mirror FPL's registry chain + error handling.
- Reproducibility of `current`/`dev` — snapshot-to-digest + label.
- Disk: package-granular dedup may be loose; revisit file-granular CAS if needed.
- The acquire-side load set MUST equal stock's (we own the read-side resolution;
  reuse it so acquire and load agree by construction).

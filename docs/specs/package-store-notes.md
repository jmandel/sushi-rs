# package_store implementation notes (Phase 1 prep)

Verified findings to make `package_store` tractable (it replicates the *read* side
of `fhir-package-loader` that SUSHI's `FHIRDefinitions` exposes).

## Cache layout (in the ISOLATED home `temp/fhir-home/.fhir/packages/`)
- One dir per package: `<name>#<version>/package/`.
- Each package dir has `package/.index.json`:
  ```json
  { "index-version": 2, "files": [
     { "filename": "StructureDefinition-Observation.json",
       "resourceType": "StructureDefinition", "id": "Observation",
       "url": "http://hl7.org/fhir/StructureDefinition/Observation",
       "version": "4.0.1", "kind": "resource" }, ... ] }
  ```
  Fields: `filename, resourceType, id, url, version, kind`. **NOTE: no `name`** —
  fish-by-name requires reading the resource file (fpl v2 builds a fuller index).
- Resource JSON files live alongside: `package/<filename>`.
- `package/package.json` holds `dependencies` (transitive graph).

## Dependency graph (what to load)
SUSHI loads: the auto FHIR core for `fhirVersion` (4.0.1 → `hl7.fhir.r4.core#4.0.1`),
plus `sushi-config.yaml` `dependencies:` (e.g. IPS: `hl7.fhir.uv.ipa#1.1.0`,
`hl7.fhir.uv.extensions.r4#5.3.0`) and the low/high automatic dependencies from
`sushi-ts/src/utils/Processing.ts`. Stock SUSHI v3.20.0 does **not** walk
transitive `package.json` dependencies from those loaded packages.

Read-side `PackageStore::for_project` resolves `latest`/`current` against the
explicit cache, and resolves `M.N.x` to the highest matching concrete cached
directory (for example `ihe.iti.mcsd#4.0.x` -> `ihe.iti.mcsd#4.0.0` in the NDH
holdout cache). Acquisition keeps those mutable coordinates unresolved until it
records a lock.

## Oracle (drive stock FHIRDefinitions from the dist)
```js
const sushi = require('fsh-sushi'); sushi.utils.logger.silent = true;
const defs = await sushi.fhirdefs.createFHIRDefinitions();
await defs.loadPackage('hl7.fhir.r4.core', '4.0.1');   // HOME must point at the isolated cache
const obs = defs.fishForFHIR('Observation', sushi.utils.Type.Resource, sushi.utils.Type.Type);
// => StructureDefinition/Observation, url http://hl7.org/fhir/StructureDefinition/Observation
```
Methods: `fishForFHIR`, `fishForMetadata(s)`, `fishForPredefinedResource(Metadata)`.
Run with `HOME=<repo>/temp/fhir-home` so it uses the ISOLATED cache (never real ~/.fhir).

## Rust mapping
- `package_store` uses `package_resource_entries` per resolved package: trust a
  non-empty `.index.json` as complete, otherwise sorted FPL-style directory scan.
  It builds maps by `url` (+ versioned url), `(resourceType,id)`, and `name`
  (prefix-read from the resource file when using `.index.json` metadata).
- `fish_for_fhir(item, &[Type])`: alias/version split, search package resources in
  the SUSHI type order, return the resource JSON (`serde_json::Value`).
- Gate: a `package-oracle.cjs` (load IPS dep set, fish a query list, dump JSON) vs
  `package_store::fish_for_fhir`. Build this when implementing Phase 1.
- Keep the cache dir EXPLICIT (constructor arg), never default to `~/.fhir` (hard rule).
- See spec `06-package-fhirdefs.md` for the full Fishable contract.


## Index reliability + provenance (verified 2026-06-30)
- Stock SUSHI / fhir-package-loader **never read `.index.json`** — they always
  directory-scan (sorted) and read each resource. `.index.json` is registry/mirror-
  dependent, NOT canonical to a package version.
- In our 7.6G cache: of 154 packages, exactly 1 ships an incomplete index —
  `hl7.fhir.uv.subscriptions-backport.r4#1.1.0` (`files:[]`, 23 real resources). The
  empty index is baked into the published .r4 tarball (verified: registry tarball sha
  `ca5e4f4c...` contains `files:[]`); an IG-Publisher artifact, not local corruption.
  (Base id `...subscriptions-backport` is a DIFFERENT package with a populated index.)
- Index ENTRIES accurate for fields we use (resourceType/id/url/version,+SD type/kind):
  0 mismatches in 394 sampled.
- Current policy: trust a non-empty index as a metadata fast path, and fall back to
  the sorted FPL scan only when `.index.json` is missing or `files:[]`. Materialized
  acquisition caches rewrite `.index.json` from actual package files, so the fast
  path is authoritative there. `PackageStore::index_package` and IG dependency URL
  lookup both use the same shared listing helper.
- Stale-cache warning: the older isolated stock cache used for build parity is
  missing three current `hl7.fhir.r4.core#4.0.1` helper definitions
  (`structuredefinition-{json,rdf,xml}-type`). A fresh stock FPL download and
  acquisition both include them, so package-fishing parity uses stock SUSHI backed
  by the same acquisition-materialized cache content.

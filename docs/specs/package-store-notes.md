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
`hl7.fhir.uv.extensions.r4#5.3.0`), plus their transitive `package.json` deps.
Resolution + auto-core in `sushi-ts/src/utils/Processing.ts` / `run` (loadExternalDependencies).

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
- `package_store` reads `.index.json` per resolved package, builds maps by
  `url` (+ versioned url), `(resourceType,id)`, and lazily `name` (read file once).
- `fish_for_fhir(item, &[Type])`: alias/version split, search package resources in
  the SUSHI type order, return the resource JSON (`serde_json::Value`).
- Gate: a `package-oracle.cjs` (load IPS dep set, fish a query list, dump JSON) vs
  `package_store::fish_for_fhir`. Build this when implementing Phase 1.
- Keep the cache dir EXPLICIT (constructor arg), never default to `~/.fhir` (hard rule).
- See spec `06-package-fhirdefs.md` for the full Fishable contract.

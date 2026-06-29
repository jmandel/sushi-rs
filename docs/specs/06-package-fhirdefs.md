# Porting Spec: package/fhirdefs subsystem

Scope: how core + dependency FHIR definitions are loaded, cached, and looked up
(the *read side* our `package_store` crate must replicate). All citations are
`path:line` relative to repo root. Upstream is READ-ONLY at `sushi-ts@v3.20.0`.

Note on external dependency: nearly all the heavy lifting (registry download,
disk cache at `~/.fhir`, the SQL.js package DB, resolution sort semantics) lives
in the external npm package **`fhir-package-loader` (FPL) `^2.2.3`**
(`sushi-ts/package.json:95`). FPL source is NOT in this repo (no `node_modules`
present), so behavioral claims below are limited to what SUSHI itself invokes
and configures. FPL internals (e.g. exact SQL queries) are an Open Question.

---

## 1. Purpose

This subsystem wraps FPL's `BasePackageLoader` in a SUSHI-specific class
`FHIRDefinitions` that loads FHIR core, configured dependencies, automatic
implicit dependencies, and the user's own predefined/local resources into a
queryable package database, then exposes "fishing" lookups by name/id/url and
type (`fishForFHIR` returns raw JSON, `fishForMetadata` returns a normalized
`Metadata` summary) (`sushi-ts/src/fhirdefs/FHIRDefinitions.ts:37`). It defines
SUSHI's deterministic resolution order across multiple loaded packages
(`FISHING_ORDER` / `DEFAULT_SORT`, `FHIRDefinitions.ts:22-32`). It injects a
handful of R5 resources into R4/R4B projects so "special types" can be
instantiated (`R5DefsForR4/index.ts:18`). It also handles cross-version (xver)
extension URL correction and produces guidance errors when an xver package is
missing (`FHIRDefinitions.ts:140-210`). The on-disk cache lives at
`~/.fhir/packages` (`FHIRDefinitions.ts:72`) — this is the directory our port
must isolate/redirect.

---

## 2. TS entry points

- `FHIRDefinitions` class (extends FPL `BasePackageLoader`, implements `Fishable`) — `sushi-ts/src/fhirdefs/FHIRDefinitions.ts:37`.
  - constructor: builds FPL options, package DB, disk cache, registry + build clients — `FHIRDefinitions.ts:41-78`.
  - `initialize()` (async; initializes the `SQLJSPackageDB`) — `FHIRDefinitions.ts:80-84`.
  - `allPredefinedResources()` — `FHIRDefinitions.ts:96-103`.
  - `fishForPredefinedResource` / `...Metadata` / `...Metadatas` — `FHIRDefinitions.ts:105-129`.
  - `fishForFHIR` — `FHIRDefinitions.ts:131-148`.
  - `fishForMetadata` — `FHIRDefinitions.ts:150-167`.
  - `fishForMetadatas` — `FHIRDefinitions.ts:169-187`.
  - `setFHIRPackageLoaderLogInterceptor` — `FHIRDefinitions.ts:92-94`.
  - `logXverExtensionDependencyError` (private) — `FHIRDefinitions.ts:189-210`.
- `createFHIRDefinitions(override?)` — async factory: `new FHIRDefinitions(...)` then `initialize()` — `FHIRDefinitions.ts:213-226`.
- Module helpers: `normalizeTypes` (`:228`), `convertInfoToMetadata` (`:233`), `logicalCharacteristic` (`:253`), `fixXverURL` (`:260`), `xverVersionToReleaseTag` (`:275`).
- `R5_DEFINITIONS_NEEDED_IN_R4` array (with `_timeTraveler` flag set) — `sushi-ts/src/fhirdefs/R5DefsForR4/index.ts:18-28`.
- Driver of loading (consumer, in `utils/`):
  - `loadExternalDependencies(defs, config)` — `sushi-ts/src/utils/Processing.ts:351-415`.
  - `loadAutomaticDependencies(...)` — `Processing.ts:417-499`.
  - `loadConfiguredDependencies(...)` — `Processing.ts:501-538`.
  - `fixCrossVersionDependencies(...)` — `Processing.ts:542-...`.
  - `AUTOMATIC_DEPENDENCIES` table — `Processing.ts:61-98`.
  - `configuredDependencyMatchesAutomaticDependency` — `Processing.ts:100-111`.
  - `isSupportedFHIRVersion` — `Processing.ts:113-117`.
- Predefined/local resource loading: `loadPredefinedResources` + `getPredefinedResourcePaths` — `sushi-ts/src/ig/predefinedResources.ts:81-100`, `:22-69`; constants `PREDEFINED_PACKAGE_NAME='sushi-local'`, `PREDEFINED_PACKAGE_VERSION='LOCAL'` — `predefinedResources.ts:8-9`.
- FHIR-version→package mapping: `getFHIRVersionInfo` + `VERSIONS` table — `sushi-ts/src/utils/FHIRVersionUtils.ts:9-157`.
- Read-side contracts: `Fishable`, `Type`, `Metadata` — `sushi-ts/src/utils/Fishable.ts:3-37`.
- Composition over fhirdefs (precedence layering): `MasterFisher` — `sushi-ts/src/utils/MasterFisher.ts:21`.
- Top-level wiring in app: `createFHIRDefinitions()` → `loadExternalDependencies` → `loadPredefinedResources` → `defs.optimize()` → `fishForFHIR('StructureDefinition', Type.Resource)` sanity check — `sushi-ts/src/app.ts:279-302`.

---

## 3. Key data structures

- `Type` enum (string-valued) — `Fishable.ts:3-15`. Values:
  `Profile, Extension, ValueSet, CodeSystem, Instance, Invariant, RuleSet,
  Mapping, Resource, Type, Logical`. `Invariant/RuleSet/Mapping` only exist in
  FSH tanks; `Type` only in FHIR defs (`Fishable.ts:9-14`).
- `Metadata` interface — `Fishable.ts:17-31`. Fields: `id` (required), `name?,
  sdType?, resourceType?, url?, parent?, imposeProfiles?, abstract?, version?,
  instanceUsage?, canBeTarget?, canBind?, resourcePath?`.
- `Fishable` interface — `Fishable.ts:33-37`: `fishForFHIR`, `fishForMetadata`,
  `fishForMetadatas` (note: `fishForPredefined*` are NOT in the interface; they
  are SUSHI extensions on `FHIRDefinitions`).
- `ResourceInfo` (FPL type) — fields consumed in `convertInfoToMetadata`
  (`FHIRDefinitions.ts:233-251`): `id, name, sdType, url, sdBaseDefinition,
  sdImposeProfiles, sdAbstract, version, resourceType, sdKind,
  sdCharacteristics, resourcePath`. Mapping rules:
  - `parent` ← `info.sdBaseDefinition` (`:241`).
  - `imposeProfiles` ← `info.sdImposeProfiles` (`:242`).
  - `abstract` ← `info.sdAbstract != null ? sdAbstract : undefined` (`:243`).
  - `canBeTarget`/`canBind` ← `logicalCharacteristic(info, 'can-be-target'|'can-bind')` (`:246-247`).
  - every falsy field is coerced to `undefined` via `X || undefined` (`:237-248`).
- `logicalCharacteristic`: returns a boolean ONLY when `info.sdKind === 'logical'`
  (membership test on `info.sdCharacteristics`); otherwise `undefined`
  (`FHIRDefinitions.ts:253-258`).
- FPL options object built by SUSHI — `FHIRDefinitions.ts:51-67`:
  - `resourceCacheSize: 200` (in-memory LRU of resolved resources).
  - `safeMode: SafeMode.FREEZE` (returned JSON is frozen, not cloned — see Gotchas).
  - `log`: routes FPL logs through SUSHI's `logger`, gated by `fplLogInterceptor`.
- Module constants — `FHIRDefinitions.ts:22-35`:
  - `FISHING_ORDER = [Resource, Logical, Type, Profile, Extension, ValueSet, CodeSystem]`.
  - `DEFAULT_SORT = [byType(...FISHING_ORDER), byLoadOrder(false)]`.
  - `XVER_EXTENSION_REGEX = /^http:\/\/hl7\.org\/fhir\/(\d+\.\d+)\/StructureDefinition\/extension-[^./]+\..+$/`.
- `AutomaticDependency` table `AUTOMATIC_DEPENDENCIES` — `Processing.ts:61-98`:
  6 entries keyed by `{packageId, version:'latest', fhirVersions[], priority}`.
  Low: `hl7.fhir.uv.tools.r4|r5`, `hl7.terminology.r4|r5`. High:
  `hl7.fhir.uv.extensions.r4|r5`.
- `FHIRVersionInfo` (`FHIRVersionUtils.ts:138-145`): `{name, packageId, version,
  packageString, isPreRelease, isSupported}`; `VERSIONS` regex table
  (`:9-134`) maps version string → core package id, e.g. `4.0.x →
  hl7.fhir.r4.core`, `4.3.x → r4b`, `5.x → r5`, `6.x → r6`, `current/dev → r5
  prerelease`, catch-all `??` unsupported.
- `R5_DEFINITIONS_NEEDED_IN_R4`: 7 bundled `StructureDefinition` JSONs (`Base,
  ActorDefinition, Requirements, SubscriptionTopic, TestPlan, CodeableReference,
  DataType`), each mutated to carry `_timeTraveler = true`
  (`R5DefsForR4/index.ts:18-28`). The JSON files are committed under
  `sushi-ts/src/fhirdefs/R5DefsForR4/*.json`.

---

## 4. Algorithms & control flow

### 4.1 Construction (`FHIRDefinitions.ts:41-84`, `createFHIRDefinitions :213-226`)
1. Build `options` with `resourceCacheSize:200`, `safeMode:FREEZE`, and a `log`
   closure that calls `this.fplLogInterceptor(level,message)` first; if the
   interceptor returns `false` the log is suppressed, else forwarded to
   `logger.log` (`:57-66`).
2. `packageDB = override ?? new SQLJSPackageDB()` (`:71`).
3. `fhirCache = path.join(os.homedir(), '.fhir', 'packages')` (`:72`) — the cache
   dir we must isolate.
4. `packageCache = new DiskBasedPackageCache(fhirCache, options)` (`:73`).
5. `registryClient = new DefaultRegistryClient(options)` (`:74`);
   `buildClient = new BuildDotFhirDotOrgClient(options)` (`:75`).
6. `super(packageDB, packageCache, registryClient, buildClient, options)` (`:76`).
7. `createFHIRDefinitions` then awaits `initialize()`, which awaits
   `SQLJSPackageDB.initialize()` (`:80-84`, `:224`).

### 4.2 Dependency load order (`loadExternalDependencies`, `Processing.ts:351-415`)
This ORDER is load-order-sensitive because resolution is LIFO (see 4.4).
1. Collect `config.dependencies` into `packageIdMap: Map<packageId,
   dependsOn[]>`, **resolving npm-alias syntax** `alias@npm:realId` first
   (replace `packageId` with the real id; warn if alias has invalid chars)
   (`:360-377`).
2. Flatten map in **insertion order**, and within each packageId **sort by
   `semver.compareLoose` ascending** so the latest version is loaded LAST (FPL
   is last-in-first-out) (`:379-383`).
3. Resolve FHIR core: pick the first supported `getFHIRVersionInfo(v)` from
   `config.fhirVersion`; warn if pre-release; push `{packageId, version}` for
   core onto `dependencies` (so core is at the END of the list) (`:386-394`).
4. `loadAutomaticDependencies(coreVersion, dependencies, defs, Low)` (`:397`).
5. `loadConfiguredDependencies(dependencies, coreVersion, configPath, defs)` —
   loads configured deps AND FHIR core (core last) (`:405`).
6. `loadAutomaticDependencies(coreVersion, dependencies, defs, High)` (`:409`).

Net load order (earliest→latest, latest wins on tie):
`[R5forR4 virtual (R4/R4B only)] → low auto-deps → configured deps → FHIR core
→ high auto-deps (extensions)`.

### 4.3 `loadAutomaticDependencies` (`Processing.ts:417-499`)
1. `fhirVersionName = getFHIRVersionInfo(fhirVersion).name` (`:423`).
2. If `priority===Low` and version is `R4`/`R4B`: build a Map id→def from
   `R5_DEFINITIONS_NEEDED_IN_R4`, wrap in an `InMemoryVirtualPackage`
   `sushi-r5forR4#1.0.0`, and `await defs.loadVirtualPackage(...)`. Loaded FIRST
   so it is lowest-priority (`:425-444`).
3. Filter `AUTOMATIC_DEPENDENCIES` by `priority`. For each auto-dep: if any
   configured dependency *matches* it (root-id match, see 4.6), substitute the
   configured dep(s) instead; else if `autoDep.fhirVersions` doesn't include the
   project's FHIR version name, drop it; else keep the auto-dep. Flatten,
   then `uniqWith(..., isEqual)` to dedupe (`:447-464`).
4. Load each serially. For non-user-configured (pure automatic) deps, install a
   log interceptor that suppresses `error`-level logs during load (`:472-477`);
   call `await defs.loadPackage(id, version)` (`:478`); on throw set
   status='FAILED' and debug-log stack (`:479-484`); always remove the
   interceptor afterward (`:485-490`). If status !== 'LOADED' for an automatic
   dep, `logger.warn("Failed to load automatically-provided ...")`, appending
   `process.env.FPL_REGISTRY` if set (`:491-497`).

### 4.4 `loadConfiguredDependencies` (`Processing.ts:501-538`)
1. `fixedDependencies = fixCrossVersionDependencies(dependencies)` (`:507`).
2. Serially, for each dep:
   - if `dep.version == null`: `logger.error` a long "No version specified"
     message and `continue` (`:511-523`).
   - else if it matches an automatic dependency (root-id match): `continue`
     (it will be loaded later in the High-priority pass) (`:524-528`).
   - else `await defs.loadPackage(id, version).catch(...)` → on error
     `logger.error("Failed to load id#version: msg")` + debug stack (`:530-535`).

### 4.5 Resolution / fishing semantics (`FHIRDefinitions.ts:96-187`)
All fishing delegates to FPL `findResourceJSON/findResourceInfo(s)` with a
`{type, scope?, sort}` options object. `sort = DEFAULT_SORT = [byType(FISHING_
ORDER), byLoadOrder(false)]`: when multiple loaded resources match the
name/id/url, results are ordered first by SUSHI's type preference
(Resource→Logical→Type→Profile→Extension→ValueSet→CodeSystem) then by
**reverse load order** (`byLoadOrder(false)` = latest loaded first = LIFO),
and the FIRST is returned. This is why "load core last, load high-priority
extensions after core" changes which definition wins.
- `normalizeTypes(types)`: if any requested type is `Type.Instance`, return
  `undefined` (treat Instance as a wildcard matching ANY type); otherwise pass
  the types through (`FHIRDefinitions.ts:228-231`).
- `fishForFHIR(item, ...types)` (`:131-148`):
  1. `findResourceJSON(item, {type:normalizeTypes(types), sort:DEFAULT_SORT})`.
  2. if found → return it.
  3. else if `XVER_EXTENSION_REGEX.test(item)` AND `Type.Extension` requested:
     compute `fixXverURL(item)`; if it changed, recurse `fishForFHIR(newURL,
     Type.Extension)`; else `logXverExtensionDependencyError(item)` and return
     `undefined`.
- `fishForMetadata` / `fishForMetadatas` mirror this, using
  `findResourceInfo(s)` + `convertInfoToMetadata`; `fishForMetadatas` returns
  `[]` when nothing found (`:150-187`).
- Predefined variants restrict `scope: PREDEFINED_PACKAGE_NAME` (`'sushi-local'`)
  and do NOT apply the xver fallback (`:105-129`).
- `allPredefinedResources()`: `findResourceJSONs('*', {scope:'sushi-local',
  sort:[byLoadOrder(true)]})` — note `byLoadOrder(true)` = FIFO (insertion
  order), intentionally different from the LIFO used everywhere else
  (`:96-103`).

### 4.6 `configuredDependencyMatchesAutomaticDependency` (`Processing.ts:100-111`)
Strip a trailing `.r4`..`.r9` segment from both ids (`/\.r[4-9]$/`), then compare
for equality. So `hl7.fhir.uv.extensions`, `.r4`, `.r5` are interchangeable.

### 4.7 xver URL handling (`FHIRDefinitions.ts:189-273`)
- `fixXverURL`: if URL ends with `[x]` or `%5Bx%5D`, strip it, `logger.warn` the
  correction, return new URL; else return unchanged (`:260-273`).
- `logXverExtensionDependencyError`: `decodeURI` + regex to extract the version
  segment; `source = xverVersionToReleaseTag(version)` (`1.0→r2`, `4.3→r4b`,
  else `r<major>`) (`:275-284`); `fhirVersion = fishForFHIR('StructureDefinition',
  Resource)?.fhirVersion`; `target = getFHIRVersionInfo(...).name` lowercased with
  `D?STU→r`; build `hl7.fhir.uv.xver-<source>.<target>`. If
  `findPackageInfos(xverPackage)` non-empty → "extension not found in package
  ..." error; else → "requires the cross-version extension package ... be
  declared" error (`:189-210`).

### 4.8 Predefined/local load (`predefinedResources.ts`)
- `getPredefinedResourcePaths`: from `resourceDir`, the dirs
  `capabilities, extensions, models, operations, profiles, resources,
  vocabulary, examples` that exist (`:27-39`), PLUS config `path-resource`
  parameters resolved against `projectDir`; trailing `/*` means recurse into
  subfolders (manual recursion, NOT `readdirSync(recursive)` — Node-version
  compat note) (`:40-67`). Returns a de-duplicated array (Set order).
- `loadPredefinedResources`: wrap those paths in a `DiskBasedVirtualPackage`
  `sushi-local#LOCAL` with `{log, allowNonResources:true, recursive:true}` and
  `await defs.loadVirtualPackage(...)`; returns `LoadStatus` (`:81-100`).

### 4.9 App wiring (`app.ts:279-302`)
`createFHIRDefinitions()` → `loadExternalDependencies(defs, config)` →
`loadPredefinedResources(defs, resolve(input,'..'), resolve(originalInput),
config.parameters)` → `defs.optimize()` → check
`fishForFHIR('StructureDefinition', Type.Resource)`; if null or its `.version`
fails `isSupportedFHIRVersion`, error about corrupt cache and `process.exit(1)`.

### 4.10 MasterFisher precedence (`MasterFisher.ts:38-108`) — read-side layering
fishForFHIR order: (a) `fhir.fishForPredefinedResource` FIRST (predefined
resources outrank everything), (b) the in-progress `pkg`, (c) if the item is in
the `tank`, return `undefined` (deliberately NOT falling through to external
FHIR), (d) external `fhir.fishForFHIR` (`:38-57`). fishForMetadata: predefined
first, then ordered fishables `[pkg, tank, fhir]` (`:68-82`).
`defaultFHIRVersion = fhir.fishForFHIR('StructureDefinition')?.fhirVersion ??
config.fhirVersion[0]` (`:28-29`).

---

## 5. Edge cases & gotchas

- **Cache directory is hard-coded** to `~/.fhir/packages`
  (`FHIRDefinitions.ts:72`). For our port this MUST be redirected/isolated
  (env override) so parity runs are hermetic. SUSHI exposes no override except
  the unit-test `override.packageCache` constructor param (`:43-49,73`).
- **`SafeMode.FREEZE`, not clone** (`FHIRDefinitions.ts:55`): fished JSON is the
  same frozen object instance shared across callers; callers that need to mutate
  `cloneDeep` it themselves (e.g. `IGExporter.ts:1016`). A naive port returning
  owned/cloned values changes nothing semantically but returning *mutable
  shared* references would diverge if a consumer mutates in place.
- **`_timeTraveler` flag is mutated onto the shared JSON** at module load
  (`R5DefsForR4/index.ts:28`) and later read by the exporter to REJECT R5
  parents in R4 IGs — except `Base` for Logicals (`StructureDefinitionExporter.ts:254-265`).
  The port must carry an equivalent per-definition flag for the 7 bundled R5
  defs and gate parent resolution on it.
- **R5-in-R4 injection only happens for R4/R4B and only on the Low pass**
  (`Processing.ts:425-444`); it is the lowest-priority package so any real
  package providing the same id overrides it (LIFO + loaded first).
- **`Type.Instance` is a wildcard** (`normalizeTypes`, `FHIRDefinitions.ts:228-231`):
  passing `Type.Instance` (even among other types) drops ALL type filtering.
- **LIFO resolution + load order coupling** (`DEFAULT_SORT` `byLoadOrder(false)`,
  `FHIRDefinitions.ts:32`). Multiple-version handling in
  `loadExternalDependencies` deliberately sorts duplicate package versions
  ascending so the newest loads last and wins (`Processing.ts:379-383`).
  Configured deps that shadow automatic deps are SKIPPED in the configured pass
  and reloaded in the High pass to keep them ahead of core
  (`Processing.ts:524-528`, `:447-461`).
- **`allPredefinedResources` uses FIFO (`byLoadOrder(true)`)** while every other
  fish uses LIFO (`FHIRDefinitions.ts:100` vs `:32`). The IGExporter relies on
  the predefined JSONs and their `ResourceInfo`s being in the SAME (FIFO) order
  to pair them index-by-index and to derive `resourcePath` by stripping the
  `virtual:{pkg}#{ver}:` prefix (`IGExporter.ts:1012-1024`). Port must preserve
  insertion order and the `resourcePath` prefix convention.
- **npm-alias syntax** `alias@npm:realPackageId` (`Processing.ts:362-371`):
  packageId is rewritten to the real id; alias is only validated for character
  set (warn-only). Easy to miss.
- **Old-style xver dep rewrite** `hl7.fhir.extensions.r<N>[b]` → official
  `hl7.fhir.uv.xver-<source>.<target>#latest` with a warn (`fixCrossVersionDependencies`,
  `Processing.ts:542-...`); also independently applied to URLs in
  `xverVersionToReleaseTag` (`:275-284`).
- **Log interceptor suppresses ONLY error-level logs for automatic deps**, and
  is reinstalled/removed per package in a try/finally (`Processing.ts:472-490`).
  User-configured packages that happen to equal an automatic dep do NOT get
  suppression (`isUserConfigured` test, `:467-469`).
- **`convertInfoToMetadata` coerces every falsy value to `undefined`**, returns
  `undefined` (not `null`) for the whole result when `info` is falsy, and only
  emits `canBeTarget`/`canBind` for `sdKind==='logical'`
  (`FHIRDefinitions.ts:233-258`). Booleans matter: `abstract` is preserved only
  when not null (`:243`).
- **`getFHIRVersionInfo` always returns a match** (catch-all `/.*/ → '??'`,
  unsupported) — never throws (`FHIRVersionUtils.ts:126-156`). `isSupportedFHIRVersion`
  explicitly rejects `4.0.0` and only allows `current|[456].x.y` (`Processing.ts:113-117`).
- **`fishForFHIR`/`fishForMetadata` xver fallback only triggers when
  `Type.Extension` is among requested types** (`FHIRDefinitions.ts:140,159,178`).
- **`optimize()` is called once after all loads** (`app.ts:291`) before any
  fishing — an FPL DB index build; our port's index must be finalized at the
  equivalent point.

---

## 6. Recommended Rust mapping

Crate: **`package_store`** (owns loading, caching, and the fishing read API).
`fhir_model` provides the resource JSON value type; `diagnostics` provides the
logger/interceptor; `compiler` consumes via a `MasterFisher` equivalent.

- `FHIRDefinitions` → `struct PackageStore` implementing a `Fishable` trait.
  - Internal index keyed for name/id/url lookup. Suggested: a `Vec<LoadedResource>`
    in load order plus `HashMap<String, Vec<usize>>` for each of name, id, url →
    candidate indices. Resolution = gather candidates, sort by
    `(fishing_order_rank(type), Reverse(load_index))`, take first. Encode
    `FISHING_ORDER` as a `fn rank(Type)->u8` (`FHIRDefinitions.ts:22-32`).
  - `LoadedResource { json: Arc<Value>, info: ResourceInfo, load_index: usize,
    scope: PackageId, resource_path: String, time_traveler: bool }`.
    `Arc<Value>` reproduces FREEZE-share semantics cheaply; consumers `clone`
    the inner value when mutating.
  - `ResourceInfo` mirrors the FPL fields actually read by
    `convertInfoToMetadata` (`id, name, sd_type, url, sd_base_definition,
    sd_impose_profiles, sd_abstract, version, resource_type, sd_kind,
    sd_characteristics, resource_path`).
- `Type` enum, `Metadata` struct, `Fishable` trait → `fsh_model` or a shared
  `core` module (used by both compiler and package_store). Keep `Type::Instance`
  wildcard logic in `normalize_types`.
- `getFHIRVersionInfo` / `VERSIONS` table → `package_store::fhir_version`
  (a `&[VersionMatcher]` with compiled `regex::Regex`, evaluated in order, first
  match wins, catch-all last).
- Loading pipeline (`loadExternalDependencies` et al.) → `package_store::load`
  functions mirroring the exact ORDER in §4.2-4.4. Keep `AUTOMATIC_DEPENDENCIES`
  as a `const`/`static` table and replicate the substitution+dedupe logic.
- **FPL replacement**: we must reimplement the registry download + disk cache +
  package DB ourselves (no Rust port of fhir-package-loader exists). Recommended:
  - Cache dir: read from env (e.g. `SUSHI_RS_FHIR_CACHE`) defaulting to
    `~/.fhir/packages`, so parity tests are hermetic. This is the explicit
    isolation the task calls out (`FHIRDefinitions.ts:72`).
  - A tarball reader for `.tgz` packages + a JSON index. The "package DB" is just
    our in-memory index; SQL.js is an implementation detail we need not copy.
  - Virtual packages (`InMemoryVirtualPackage`, `DiskBasedVirtualPackage`) →
    a `VirtualPackage` source enum (in-memory list, or disk dir set with
    `recursive`/`allow_non_resources` flags). Predefined load (`sushi-local#LOCAL`)
    and R5forR4 (`sushi-r5forR4#1.0.0`) both go through this.
- Connections to neighbors: `compiler::MasterFisher` wraps `package_store`
  (predefined-first, then pkg, then tank, then external — `MasterFisher.ts`).
  `json_emit`/IG exporter consume `all_predefined_resources()` (FIFO) and
  `resource_path`. `diagnostics` receives the routed log + interceptor.

---

## 7. Parity test ideas

Each should diff SUSHI's stdout/diagnostics and emitted JSON against the port,
with `~/.fhir` pointed at a controlled fixture cache.

1. **Resolution order / LIFO**: a tank declaring two packages that both define a
   profile with the same `name`+`url` at different versions; assert the
   later-loaded (and higher fishing-order type) one wins
   (`DEFAULT_SORT`, `Processing.ts:379-383`). Also a case where a `Type.Resource`
   and a `Type.Profile` share a name → Resource wins (`FISHING_ORDER`).
2. **Automatic vs core precedence**: R4 project with no configured extensions;
   verify `hl7.fhir.uv.extensions.r4` (High) shadows a same-url core definition,
   while `hl7.terminology.r4`/`hl7.fhir.uv.tools.r4` (Low) do NOT shadow core.
3. **Configured dep overriding an automatic dep**: declare
   `hl7.fhir.uv.extensions.r4#<pinned>`; assert it is skipped in the configured
   pass and loaded in the High pass (load order + which version resolves)
   (`Processing.ts:447-461,524-528`).
4. **R5-in-R4 time travel**: R4 project profiling `SubscriptionTopic`/`Requirements`
   etc.; (a) instance allowed, (b) using one as a profile `Parent` →
   `ParentNotDefinedError`, (c) `Logical` with `Parent: Base` allowed
   (`StructureDefinitionExporter.ts:254-265`).
5. **xver URL correction**: extension URL ending in `[x]` → warn + corrected URL
   used (`fixXverURL`); missing xver package → the two distinct guidance errors
   (`logXverExtensionDependencyError :197-209`).
6. **npm alias + old-style xver dep rewrite**: `foo@npm:hl7.fhir.x` and
   `hl7.fhir.extensions.r5` in config → assert rewrite warnings and which
   package actually loads (`Processing.ts:362-371,542-...`).
7. **Predefined precedence & FIFO ordering**: local `input/profiles/*` resource
   with same url as a dependency → predefined wins (`MasterFisher.ts:42`);
   `allPredefinedResources()` order and `resource_path` derivation match
   `IGExporter` index pairing (`IGExporter.ts:1012-1024`).
8. **Type.Instance wildcard**: fishing by url with `Type.Instance` returns a
   CodeSystem/ValueSet/etc. regardless of declared type
   (`normalizeTypes :228-231`).
9. **Metadata coercion**: a logical model with `can-be-target` characteristic →
   `canBeTarget:true`, `canBind:false`; a non-logical SD → both `undefined`;
   empty-string fields → `undefined` (`convertInfoToMetadata`, `logicalCharacteristic`).
10. **Missing/corrupt core**: empty cache → the exact "Valid StructureDefinition
    resource not found ..." message + exit code 1 (`app.ts:294-302`).
11. **Version mapping**: `fhirVersion: 4.0.0` rejected; `4.0.1→r4.core`,
    `4.3.0→r4b.core`, `5.0.0→r5.core`, `current→r5 prerelease` warn
    (`FHIRVersionUtils`, `Processing.ts:389-393`).

---

## 8. Open questions

1. **FPL internals not in repo**: exact match semantics of FPL's
   `findResourceJSON(item, ...)` — does `item` match against name OR id OR url,
   with what precedence, and how are versioned refs (`url|version`) handled? We
   only see SUSHI's `sort`/`scope`/`type` options; the matching predicate is FPL
   internal. Needs confirmation against FPL `^2.2.3` source or empirical tests.
2. **`byLoadOrder`/`byType` tie-breaking** when both keys are equal (e.g. two
   resources same type loaded in same package) — FPL-internal stable order?
3. **`optimize()` semantics**: purely a DB index build, or does it affect
   resolution results? Assumed index-only (`app.ts:291`).
4. **Registry/network**: do we need to replicate `DefaultRegistryClient`,
   `BuildDotFhirDotOrgClient`, `current`/`dev` build resolution, and
   `process.env.FPL_REGISTRY` (`Processing.ts:493`) for parity, or can the port
   assume a pre-populated cache for hermetic builds? Decision needed on whether
   network download is in-scope for v1.
5. **`safeMode` exposure**: do any SUSHI consumers depend on the returned JSON
   being frozen (i.e. expecting a throw on mutation)? If not, `Arc<Value>` +
   caller-side clone suffices.
6. **SQL.js DB export** (`exportDB`, `app.ts:306-312`) is commented out — out of
   scope, confirm we never need it.
7. **`resourceCacheSize:200` LRU**: needed for parity (it's a perf cache) or can
   we keep everything resident? Assumed perf-only, safe to ignore.

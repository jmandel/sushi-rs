# Hosting the engine

This is the current host guide for `sushi-rs`. The editor's current
[`SPEC.md`](https://github.com/jmandel/fhir-ig-editor/blob/main/SPEC.md) defines
the cross-repository browser/renderer contract. This guide covers the engine
host surfaces and explains why native Publisher templates and callback-free
external builders have different execution patterns.

## Host surfaces

| Surface | State and data boundary | Result boundary |
| --- | --- | --- |
| Rust crates | typed values owned by the caller | Rust return values and errors |
| WASM `Session` | one isolated in-memory engine per normal `Session::new()` | JSON result/error envelope strings |
| `fig` | native process and explicit filesystem/CAS inputs | human output, or the shared envelope with `--json` |

The library is not a JSON-envelope skin. `crates/api_envelope` is shared only by
the WASM boundary and `fig --json`, with schema equivalence pinned by
`crates/fig/tests/json_envelope.rs`.

## Session ownership

Construct one `Session` for each independently mutable engine lifetime:

```js
const mod = await import(`${base}pkg/wasm_api.js`);
await mod.default(`${base}pkg/wasm_api_bg.wasm`);
const session = new mod.Session();
```

`new Session()` maps to `Session::new()` and owns a fresh `Engine` in an
isolated `RefCell`. `Session.global()` is an explicitly named compatibility
door onto the old process-global engine. New browser and library hosts should
not use it.

The engine is mutable within a session. A browser worker should therefore own
the session, serialize state-changing operations, and prevent a superseded
request from publishing its result. Separate sessions may proceed
independently.

All instance methods return an envelope JSON string:

```jsonc
{ "apiVersion": 1, "ok": true,  "op": "compileProject", "result": {} }
{ "apiVersion": 1, "ok": false, "op": "compileProject", "error": { "message": "..." } }
```

Domain failures are `ok:false`; hosts must unwrap every call. `Session.version()`
is the one static build-info accessor and is not wrapped in the result/error
envelope.

```js
function unwrap(raw) {
  const message = JSON.parse(raw);
  if (message.apiVersion !== 1) throw new Error(`apiVersion ${message.apiVersion}`);
  if (!message.ok) throw new Error(`${message.op}: ${message.error.message}`);
  return message.result;
}
```

## Compile one exact project revision

The normal editor sequence is:

1. `init` or `mount` exact package bundles.
2. `resolveProject` until the required exact package closure is mounted.
3. `compileProject` once with all authored inputs.
4. Project that installed compile into a specific renderer boundary.

```js
unwrap(session.init(JSON.stringify(packageBundles)));
unwrap(session.resolveProject(configYaml, JSON.stringify(versionIndex)));

const compiled = unwrap(session.compileProject(
  JSON.stringify(fshByPath),
  configYaml,
  JSON.stringify(predefinedResourcesByPath),
  JSON.stringify(siteFilesAsBase64ByPath),
));
```

`compileProject` captures FSH, config, predefined resource objects, and the
authored site-file manifest as one session revision. Site-file names participate
in IG page export, so they must be present during this compile rather than
introduced by a later site projection.

`buildSiteDbFromCompile` and `buildSiteBuildFromCompile` accept the same authored
bodies as equality assertions, plus deterministic projection metadata such as
`build_epoch_secs`. They reject any FSH, config, predefined, or site-file bytes
that differ from the installed revision and cannot invoke the compiler. The
legacy `compile` and `buildSiteDb` calls remain migration APIs; new hosts should
use `compileProject` and an explicit projection.

## External builders: require a closed build

The Cycle external-builder boundary is:

```js
const result = unwrap(session.buildSiteBuildFromCompile(JSON.stringify({
  config: configYaml,
  fsh: fshByPath,
  predefined: predefinedResourcesByPath,
  site_files: siteFilesAsBase64ByPath,
  build_epoch_secs: buildEpochSecs,
  liquid_asset_dirs: ['input/includes'],
})));
```

The result contains:

- `siteBuild`: a `ClosedSiteBuild` for render target `cycle-site/v1`; and
- `siteDbJson`: canonical bytes addressed by its single required
  `compat.site_db/rows.json` artifact.

The host must recompute/verify the `SiteBuild` id and verify the addressed
bytes' digest and length before exposing them to the builder. After that handoff
the browser Cycle renderer uses a callback-free `SiteBuildView`; it does not use
the Rust Liquid engine, `renderFragment`, a filesystem, or a live compiler
callback.

This closed pattern is the rule for an external builder that can declare its
requirements. `SiteBuild` is renderer-neutral and may contain other artifact
shapes in the future; `site.db` is one compatibility artifact, not the contract
for every renderer.

## Native Publisher templates: typed late resolution

The native stock-template path deliberately differs because a template can
name a generated include only while Rust Liquid evaluates a page:

```js
unwrap(session.mountSite(JSON.stringify(authoredAndRuntimeFiles), JSON.stringify({
  artifactResolution: true,
})));
unwrap(session.mountTemplate('hl7.fhir.template#1.0.0'));
unwrap(session.produceStockSite());

const { pages } = unwrap(session.listPages());
const { html } = unwrap(session.renderPage(pages[0]));
```

At the native compatibility edge, `render_page` translates a registered legacy
Publisher include name once into a typed `ArtifactKey`. Its
`ArtifactResolver` produces the fragment, the generation cache is keyed by the
typed artifact identity, and `PageArtifactReadSet` records both attempted
requests and successful reads. Authored/template includes remain ordinary
files.

`mountSite` can set `artifactResolution:false`. In that mode a missing include
does not call the fragment engine. This is useful for callback-free consumers,
but Cycle's current browser architecture goes further: it does not mount a Rust
Liquid tree at all.

The native resolver currently stores materialized bytes and read sets in the
session generation. Promoting those results into CAS-backed artifacts in a new
immutable `SiteBuild` revision is remaining convergence work.

## Liquid implementations

There are two Liquid implementations in the overall stack, one for each
renderer architecture:

- Rust `render_liquid` serves native Publisher templates in `fig` and the WASM
  stock renderer. `Session.renderLiquid` is a generic entry to this native
  surface.
- Cycle uses LiquidJS behind Cycle's shared content policy. Its native CLI and
  browser preview use that same implementation over a `SiteBuildView`.

An external builder is not required to call back into Rust content or fragment
services. The editor's `SiteGeneratorAdapter` is a host-integration interface
for selecting/building/rendering a generator; it is not the semantic handoff.
For Cycle, that handoff is the verified `ClosedSiteBuild`. For stock templates,
the adapter is a thin host over the session's native page surface.

## Other Session operations

| Operation | Purpose |
| --- | --- |
| `init`, `mount` | replace or add immutable package bundles |
| `setLocalResources` | replace the local StructureDefinitions used by later standalone snapshot operations; clears complete-project identity |
| `snapshot` | snapshot an inline or installed `StructureDefinition` |
| `expandValueSet` | bounded in-engine enumerable expansion |
| `mountSite`, `mountTemplate`, `produceStockSite` | assemble native template state |
| `listPages`, `renderPage`, `renderFragment` | native Publisher output operations |
| `renderLiquid`, `renderMarkdown` | generic native content operations; not Cycle's content engine |

`mountSite` accepts `activeTables`, `runUuid`, `merge`,
`engineFirstIncludes`, and `artifactResolution`. The first two are deterministic
Publisher render context. `merge` overlays a mounted tree instead of replacing
it. `engineFirstIncludes` chooses whether registered generated artifacts or
ordinary staged files win, while `artifactResolution:false` removes the resolver
capability entirely.

The exact method names and argument comments live beside the bindings in
`crates/wasm_api/src/lib.rs`. The `site_build` wire invariants live in
[`crates/site_build/README.md`](../crates/site_build/README.md).

## Native CLI and non-JS hosts

`fig` is the supported native host. Its subcommands compose the same compiler,
snapshot, fragment, page, and package crates:

```sh
fig build <ig-dir> -o fsh-generated
fig snapshot <sd.json> --package hl7.fhir.r4.core#4.0.1
fig sitedb <ig-dir> --sushi-out <dir> --cache <dir> -o site.db
fig render <build-dir> -o site/
fig watch <build-dir> --serve :8080
```

Use `--json` when another process needs a stable result/error envelope. A
non-JS host can shell out to `fig`; the runnable Python example is
[`examples/shell-to-fig/render.py`](../examples/shell-to-fig/render.py). A
future in-process WASI binding may expose another surface, but shell-to-`fig` is
the tested non-JS route today.

The old `fig render --generator ts:<adapter.mjs>` compatibility runner has been
removed. It supplied the editor's former callback-oriented adapter context,
loaded a second Node-target WASM engine, and could compile inputs independently
of the native build. It was neither the current editor contract nor a safe
external-builder handoff.

Portable external builders declare a render plan and consume a verified
`ClosedSiteBuild` plus content-addressed objects. Cycle follows this law in the
browser, and Fig produces the same closed target for native builders:

```sh
fig prepare <ig-dir> \
  --target cycle-site/v1 \
  --sushi-out <new-compile-dir> \
  --cache <explicit-package-cache> \
  --out <new-bundle-dir> \
  --build-date <unix-epoch-or-RFC3339>
```

The result contains `<new-bundle-dir>/site-build.json` and one verified object
for every source, normalized package payload, and ready artifact at
`objects/sha256/<digest>`. Fig resolves the exact compile/context union, runs one
native `site_db::build`, derives the project id and FHIR version from the
produced IG and rows, and uses the shared `site_db_compat::close_projection`.
After capture, Fig reconstructs a private IG tree and package cache from those
exact addressed bytes; `site_db::build` receives only the staged paths, never
the live project/cache. Thus an A→B→A live mutation cannot influence execution
while retaining A's identity. Fig verifies the staged view before and after the
build and still compares the live trees afterward as mutation diagnostics.
Package normalization is the shared browser bundle round trip (`build_bundle`
then `read_bundle`) followed by the common
`package_store::normalize_package_material` boundary. Native Fig and WASM both
validate the mounted label against `package.json`, require string dependency
coordinates, regenerate the derived-index sidecar, and content-address the same
canonical compiler-visible top-level bytes. The raw/browser transport also
retains validated nested files needed by template packages. Current
`fig prepare --target cycle-site/v1` intentionally excludes that nested
transport because it is not a Cycle target input; template content must become
an explicit target artifact when the native-template path is closed into
`SiteBuild`.

The command never acquires packages or reads a default cache. Both output trees
must be new and disjoint, authored/nested package symlinks are rejected, a
package-root symlink may not leave the explicit cache, and the ambient
`SITE_LIQUID_ASSET_DIRS` override is rejected. The current target intentionally
records and uses `input/includes` as its only Liquid asset directory.

Cycle consumes the result through its closed-bundle entry point, with no engine
callback or second WASM instance:

```sh
SITE_BUILD_DIR=<new-bundle-dir> bun site-gen/build.tsx
```

## Template packages and WASM builds

`fig render --template <id#version>` resolves and materializes the exact
template chain as data. `--template-dir` accepts an already materialized tree.
The browser equivalent fetches exact template packages through the host package
transport and calls `mountTemplate`.

Build the module with:

```sh
cargo build -p wasm_api --target wasm32-unknown-unknown --release
wasm-bindgen target/wasm32-unknown-unknown/release/wasm_api.wasm \
  --target web --out-dir pkg --out-name wasm_api
```

Use `--target nodejs` for Bun/Node. The repository's runnable examples and
envelope schema live under [`examples/`](../examples/).
`scripts/examples-gate.sh` is the local aggregate runner; callers must invoke it
explicitly and supply optional WASM inputs when an example requires them.

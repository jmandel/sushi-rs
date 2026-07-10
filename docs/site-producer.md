# The site-producer — page shells + `_data` model, from source

> **Current-state correction (2026-07-09).** The original task write-up below
> predated the landed WASM integration. `Session.produceStockSite()` now calls
> this crate from the current compile and mounted template. The producer emits
> page shells plus eight derivable `_data` files; `artifacts.json` and page
> shells retain the corpus byte-parity gates. Generated fragment bodies are not
> hidden file-miss behavior owned by this crate: `render_page` translates a
> registered Publisher include name to a typed `ArtifactKey` and calls an
> explicit `ArtifactResolver`. See [`hosting.md`](hosting.md) and
> [`crates/site_build/README.md`](../crates/site_build/README.md) for the current
> execution contracts.

Crate: `crates/site_producer`. Gate:
`crates/site_producer/tests/producer_gate.rs`.

## 1. What this is / why

The stock-template render path used to mount a **pre-baked `-stock.json`** — a
snapshot of the Java IG-Publisher's `temp/pages` tree (page shells + `_data` +
`_includes`). To build the stock site *from a repo dir tree* (and make `fig
render <ig-source-dir>` work from source), the engine must synthesize the two
things the publisher generates that we didn't already produce:

* **(a) per-artifact page SHELLS** — the `<Type>-<id>.html` pages that
  `{% include <Type>-<id>-snapshot.xhtml %}` (etc.) pull fragment bodies into;
* **(b) the `_data/*.json` SITE-DATA MODEL** — what the stock layouts/fragments
  read via `site.data.*`.

Fragment **bodies** (`_includes/*.xhtml`) are not produced here. The native page
stack may materialize a registered fragment through
`render_page::ArtifactResolver`; authored/template includes remain tree files.
The producer emits **shells + `_data` only**. Downstream,
`render_page`/`fig::engine` or the WASM render state consumes that produced tree.

## 2. Where it lives

```
crates/site_producer/
  src/lib.rs       gather_inputs (dir) / ProducerInputs::from_memory (wasm) / produce / write_to
  src/config.rs    Defaults: config.json `defaults` + `extraTemplates`; find_config/get_property; sd_type
  src/resource.rs  Resource model + enumerate_resources
  src/shells.rs    page-shell emission  (the validated core)
  src/data.rs      _data builders (artifacts.json byte-exact; structuredefinitions model)
  tests/producer_gate.rs   byte-parity gate vs the US Core F0 temp/pages oracle
```

Consumed by `fig` (`fig produce`, and auto-produce inside `fig render`) and by
the WASM `Session.produceStockSite()` surface (§6).

## 3. Publisher parity model (cited — pinned publisher clone
`org.hl7.fhir.publisher.core`)

### 3.1 Page shells

The publisher generates one page per `(resource × layout)` in
`PublisherGenerator.makeTemplates` (`PublisherGenerator.java:1019`) →
`genWrapper` → `genWrapperInner` (`:1378`):

1. Resolve the resource's config (which layout + which output filename) via
   `IGKnowledgeProvider.findConfiguration` (`IGKnowledgeProvider.java:417`).
   * StructureDefinition flavor: `getSDType` (`:293`) — `extension` if
     `type==Extension`; `resourcedefn` if `kind==resource &&
     derivation==specialization`; else `kind` (+ `:abstract`). The stock
     `defaults` only keys `StructureDefinition` and `StructureDefinition:extension`,
     so plain profiles (`kind=resource, derivation=constraint` → `"resource"`,
     no `StructureDefinition:resource` key) fall through to `StructureDefinition`.
   * examples → the `example` default; else the type default; else `Any`.
2. `makeTemplates` emits, per resource: the **base** page (`template-base` →
   `base` filename), **definitions** (`template-defns` → `defns`), and each
   **extraTemplate** name (`template-<name>` → `<name>`), *skipping* `format`
   and `defns` in the loop (`:1029`). A layout whose `template-*` value is empty
   (`""`) emits nothing — this is how `StructureDefinition` suppresses
   `change-history` while canonicals suppress `profile-history`, purely from the
   config table.
3. `genWrapperInner`: read the layout file, run `doReplacements`
   (`IGKnowledgeProvider.java:147`) — `{{[title]}}` = `r.getTitle()` (the
   resource `name`, `PublisherIGLoader.java:3028`, falling back to `type/id`),
   `{{[name]}}` = `id[-fmt]-html`, `{{[id]}}`, `{{[type]}}`, `{{[uid]}}` =
   `type=id`, `{{[langsuffix]}}` = `""` — then write to `<tempDir>/<outputName>`.
4. Property precedence (`getProperty`, `:255`): resource's own config →
   `StructureDefinition:<flavor>` → type default → `Any`.

### 3.2 Resource processing order

The publisher walks the ImplementationGuide's `definition.resource[]` list;
`artifacts.json` key order and `structuredefinitions.json` `index` both follow
it. The producer parses that order (`ig_resource_order`) and applies it
(`order_resources`).

## 4. Parity scoreboard (vs the US Core F0 `temp/pages` oracle — the raw Java
IG-Publisher output at `/home/jmandel/hobby/sushi-rs-snapshot-f0-builds/us-core`)

| Artifact | Result |
|---|---|
| **Page shells** | **1297 / 1297 byte-identical** (base + definitions + mappings + testing + examples + profile-history + change-history across 442 resources) |
| **`_data/artifacts.json`** | **byte-identical** (442 entries, IG-resource order) |
| `_data/structuredefinitions.json` | emitted; load-bearing identity fields exact, with classified run-context/model gaps (§5) |
| other load-bearing `_data/*.json` | emitted from source; classified fidelity gaps remain (§5) |

> Oracle note: the editor's pre-baked bundle
> `fhir-ig-editor/site-bundles/uscore-stock.json` is a **filtered subset** of
> `temp/pages` — it drops the 372 `*.change.history.html` shells and all
> `*.profile.{xml,json,ttl}.html` format-dump pages. The producer reproduces the
> *raw* publisher `temp/pages` (superset); the editor filter is a bundling step,
> not a producer concern. Compare against `temp/pages`, not the bundle.

## 5. `_data` outputs and classified gaps

`data::emit_data` currently writes `artifacts.json`,
`structuredefinitions.json`, `resources.json`, `pages.json`, `fhir.json`,
`info.json`, `languages.json`, and `related.json`. The values are serialized for
Liquid to parse; except for the separately gated `artifacts.json`, matching the
Java pretty-printer's whitespace is not a page-rendering requirement.

The remaining known fidelity gaps are explicit in `src/data.rs`:

* `resources.json.identifiers` cannot reproduce Publisher-assigned OIDs that are
  absent from source; history/test-plan/test-script flags require audit/run
  context and currently remain false.
* `structuredefinitions.json.date` is Java `Date.toString()` in the build
  machine timezone and is not read by the template. Other load-bearing identity
  fields are pinned by the field-level gate.
* several `fhir.json` values are build-run context: validation error counts,
  tooling/version strings, repository source, and processed-file counters.
  They are stubbed or omitted when unavailable.
* `pages.json` does not yet reproduce the Publisher's global interleaving and
  hierarchical numbering for every narrative/artifact page. The residual
  affects small previous/next footer links and a heading-prefix value.

These gaps do not block page-shell generation, but they must remain classified
rather than being presented as full `_data` byte parity.

## 6. Surfaces

### Native (`fig`)

* `fig produce <ig-source-dir> [-o <pages-root>]` — synthesize the shell + `_data`
  tree (default output `<dir>/temp/pages`). Direct exposure of the producer.
* `fig render <ig-source-dir> -o <site/>` — when there is **no** staged
  `temp/pages` but the dir is an IG source tree (`template/config.json` present),
  auto-produces the shells + `_data` first, then runs the existing page pass.
  Demonstrated: US Core source → `produced 1297 shells + 8 _data files from
  source`, then `ok: true, pages: 1297`. (ValueSet expansion-cache misses seen in
  the demo are the fragment engine's tx-cache **input**, unrelated to the
  producer.)

### WASM / `Session` (landed)

The library is `std::fs`-free on the in-memory hot path via
`LayoutSource::Map`. The underlying entry point is:

```rust
let inputs = site_producer::ProducerInputs::from_memory(
    resources,     // Vec<Resource> parsed from the mounted resource files
    &config_json,  // the materialized template's config.json, as serde_json::Value
    layouts_map,   // HashMap<"template/layouts/layout-*.html", contents> from the template tree
    &ig_json,      // the ImplementationGuide resource (for order + publisher fallback)
    page_includes, // names that really exist, for intro/notes gating
    "en/",        // browser stock page path prefix
);
let out = site_producer::produce(&inputs)?;   // out.pages, out.data (relpath -> bytes)
```

`Session.produceStockSite()` is the landed wrapper. After `compileProject`,
`mountSite`, and `mountTemplate`, it gathers the current render resources,
template config/layouts, and staged intro/notes names; produces the tree; merges
shells and `_data` into `/site`; stages generated `menu.xml` when available; and
invalidates the render state. If a generated ImplementationGuide resource is
absent on the WASM path, the wrapper synthesizes the minimal IG context needed
for page ordering and metadata.

This producer integration is native-render state, not yet a complete stock
`SiteBuild` target. Promoting stock pages, assets, and typed resolver results to
addressed artifacts in an immutable manifest remains architecture work.

## 7. Quirks registered

* **Q-SP1** `change-history`/`profile-history` shell suppression is config-driven
  (empty `template-*` value), not a code gate — matches `temp/pages` exactly.
* **Q-SP2** `{{[title]}}` resolves to the resource `name` (not `title`), falling
  back to `type/id` for complex-`name` resources (Patient examples).
* **Q-SP3** the editor `-stock.json` bundle is a filtered subset of `temp/pages`
  (no change-history / format-dump pages) — oracle is `temp/pages`.
* **Q-SP4** `structuredefinitions.json.date` is Java-`Date.toString()` in the
  build TZ — a run-context value, excluded from the field-derivation gate.

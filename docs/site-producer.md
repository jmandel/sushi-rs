# The site-producer — page shells + `_data` model, from source

> Task #44. Status: page shells + `artifacts.json` at **byte-parity** (native +
> wasm-ready); the rest of the `_data` model designed with a cited run-context
> gap catalog. Crate: `crates/site_producer`. Gate:
> `crates/site_producer/tests/producer_gate.rs`.

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

Fragment **bodies** (`_includes/*.xhtml`) are NOT produced here — they fill live
via the fragment engine's first-include-miss (`render_sd::engine::FragmentEngine`).
The producer emits **shells + `_data` only**. Downstream, the existing
`render_page`/`fig::engine` page pass consumes the produced tree unchanged.

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

Consumed by `fig` (`fig produce`, and auto-produce inside `fig render`) and
ready for the wasm `Session` surface (§6).

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
| `_data/structuredefinitions.json` | field-derivable (~90%); load-bearing identity fields exact; see gap catalog |
| other `_data/*.json` | designed; run-context gaps cataloged (§5) |

> Oracle note: the editor's pre-baked bundle
> `fhir-ig-editor/site-bundles/uscore-stock.json` is a **filtered subset** of
> `temp/pages` — it drops the 372 `*.change.history.html` shells and all
> `*.profile.{xml,json,ttl}.html` format-dump pages. The producer reproduces the
> *raw* publisher `temp/pages` (superset); the editor filter is a bundling step,
> not a producer concern. Compare against `temp/pages`, not the bundle.

## 5. `_data` gap catalog (what needs info the source+resources+config lack)

The `_data` files that are not yet byte-emitted are **derivable in shape** but
carry per-field **run-context** values the Java publisher injects. These are the
specific, cited gaps (report-as-gap per house rules):

* **`structuredefinitions.json`** — field-derivable now (`structuredefinitions_model`),
  exact for url/name/title/kind/type/base/status/abstract/derivation/path/
  description/copyright. Remaining for byte-parity:
  1. the publisher's `JsonObject` pretty-printer (`"key" : value`, 2-space
     indent, `{\n  }` empties) — characterizable, not yet ported;
  2. `basename`/`basepath` when a profile's `baseDefinition` is a **core R4**
     type — needs the core FHIR package loaded to resolve the base SD's name +
     the `hl7.org/fhir/R4/<type>.html` spec path (57/70 US Core profiles);
  3. extension `contexts`/`extension-contexts` (derivable from `SD.context`);
  4. **`date`** — Java `Date.toString()` in the **build machine's timezone**
     (`"Tue Oct 17 00:00:00 CDT 2023"`): a timezone-formatted run-context value;
  5. the special `maturities` aggregate key (not a resource row);
  6. `publisher` falls back to the **IG-level** publisher when the SD omits it.
* **`resources.json`** — `source` is an **absolute build-machine path**
  (`/home/.../ImplementationGuide-...json`): run-context, not portable.
* **`fhir.json`** — substantial run-context: `genDate`/`genDay` (build clock),
  `errorCount` (validation-run result), `revision`/`versionFull`/tooling* (build
  tooling), `totalFiles`/`processedFiles` (run counters), `repoSource` (git
  remote), the `ver`/tx-server maps (dependency-resolution context). Derivable
  parts: the `ig` block, `canonical`, `igVer`, `resourceTypes`/`dataTypes` (from
  the core package).
* **`info.json`** — mostly IG `template-parameters`, but `copyrightyear` uses the
  **build year**.
* **`pages.json`** — derivable from the IG `definition.page` tree + breadcrumbs
  (`addPageData`, `PublisherGenerator.java:3583`), large but source-derivable;
  the history/example row gating (`historyTemplates`/`exampleTemplates` +
  `r.getAudits().isEmpty()`/`getStatedExamples().isEmpty()`, `:3609`) is the
  same logic as the shell pass.

None of these blocks the shells (the novel engine piece). They are the follow-up
for full `_data` byte-parity; the two load-bearing, fully-derivable files
(shells, `artifacts.json`) ship gated now.

## 6. Surfaces

### Native (`fig`)

* `fig produce <ig-source-dir> [-o <pages-root>]` — synthesize the shell + `_data`
  tree (default output `<dir>/temp/pages`). Direct exposure of the producer.
* `fig render <ig-source-dir> -o <site/>` — when there is **no** staged
  `temp/pages` but the dir is an IG source tree (`template/config.json` present),
  auto-produces the shells + `_data` first, then runs the existing page pass.
  Demonstrated: US Core source → `produced 1297 shells + 1 _data files from
  source`, then `ok: true, pages: 1297`. (ValueSet expansion-cache misses seen in
  the demo are the fragment engine's tx-cache **input**, unrelated to the
  producer.)

### wasm / `Session` (for the editor's source-driven stock adapter — follow-up)

The library is `std::fs`-free on the hot path via `LayoutSource::Map`. The
Session-facing entry point is:

```rust
let inputs = site_producer::ProducerInputs::from_memory(
    resources,     // Vec<Resource> parsed from the mounted resource files
    &config_json,  // the materialized template's config.json, as serde_json::Value
    layouts_map,   // HashMap<"template/layouts/layout-*.html", contents> from the template tree
    &ig_json,      // the ImplementationGuide resource (for order + publisher fallback)
);
let out = site_producer::produce(&inputs)?;   // out.pages, out.data (relpath -> bytes)
```

The editor's stock adapter would call this after `Session.mountTemplate(id#ver)`
(which already materializes the template tree in memory), merge `out.pages` +
`out.data` into the site tree at `temp/pages/…`, and render — replacing the
hand-curated `-stock.json` warm-start bundle. Exposing a thin
`Session.produceStockSite()` wrapper over `from_memory` is the one wasm-binding
line the follow-up adds.

## 7. Quirks registered

* **Q-SP1** `change-history`/`profile-history` shell suppression is config-driven
  (empty `template-*` value), not a code gate — matches `temp/pages` exactly.
* **Q-SP2** `{{[title]}}` resolves to the resource `name` (not `title`), falling
  back to `type/id` for complex-`name` resources (Patient examples).
* **Q-SP3** the editor `-stock.json` bundle is a filtered subset of `temp/pages`
  (no change-history / format-dump pages) — oracle is `temp/pages`.
* **Q-SP4** `structuredefinitions.json.date` is Java-`Date.toString()` in the
  build TZ — a run-context value, excluded from the field-derivation gate.

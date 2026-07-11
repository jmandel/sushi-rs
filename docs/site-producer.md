# Publisher site production

`crates/site_producer` is the Publisher-compatible assembly layer between a
prepared guide and Rust Liquid rendering. It owns two related jobs:

1. derive Publisher page shells and the `_data` model from the current guide;
2. assemble the fixed Publisher runtime namespace from the exact core package,
   selected template chain, and a small audited embedded payload.

It does not define another handoff value. Its results are private inputs while
the facade prepares a `SiteBuild`; final bytes are described by `SiteOutput`.

## Data flow

```text
PreparedGuide + exact PackageLock
              + materialized TemplateTree
              |
              +-> produce() -> page shells + _data
              |
              +-> PublisherRuntime::assemble()
                    -> runtime/core/template assets
                    -> deterministic HTML finishing rules
              |
              +-> authored overlays
              v
       immutable RenderState + complete output catalog
              |
       render(handle, path)
              v
         ContentRef in ContentStore
```

Precedence is explicit and collision checked:

```text
embedded runtime < exact FHIR core < selected template < authored files
```

The producer selects the compiler's explicit primary ImplementationGuide.
Additional ImplementationGuide resources remain ordinary artifacts and cannot
replace project identity through traversal order.

## Shell and `_data` production

`produce(&ProducerInputs)` emits resource page shells and eight derivable data
files. Shell layout selection and output names follow the Publisher's config
fallback rules. Resource ordering follows the primary IG's
`definition.resource[]` order. The focused corpus gates preserve byte parity for
all page shells and `artifacts.json`.

Generated fragment bodies are not files made by this crate. During Publisher
Liquid evaluation, `render_page` maps a registered include name to a typed
`ArtifactKey` and resolves it synchronously through the captured
`ArtifactResolver`. Authored and template includes are ordinary mounted files.
This distinction keeps late Publisher compatibility internal without making the
host participate in a file-miss protocol.

The other `_data` files intentionally expose known Publisher run-context gaps
where source data is insufficient, such as Publisher-assigned OIDs, validation
counters, tooling strings, and exact global previous/next interleaving. Those
are classified model gaps, not claims of byte parity.

## Publisher runtime assembly

`publisher_runtime::PublisherRuntime::assemble` consumes:

- the exact resolved FHIR core coordinate and package source;
- the fully materialized template tree;
- 25 audited irreducible embedded runtime files (150,112 bytes).

Standard tree icons, fixed table images, and FHIR CSS come from the exact core
package. Template-owned fonts and scripts come from the selected template.
Only irreducible files are embedded, with their license notices. Generated
`tbl_bck*` backgrounds use deterministic inline SVG rather than a hidden editor
asset bundle. The narrow jQuery compatibility transform is gated by exact input
byte pairs and order, then applied after Liquid and before content addressing.

The runtime exposes a recipe digest binding all selected bytes, provenance,
licenses, transform versions, and the core coordinate. That digest participates
in the renderer recipe and output lookup identity.

Publisher pages commonly use relative asset URLs under `en/`. Preparation
therefore declares page-relative aliases (for example `en/assets/app.css`) that
point to the same `ContentRef` as the canonical asset. The private ContentStore
keeps one body per digest, so URL closure does not duplicate bytes.

## Public Rust entry points

The crate's reusable typed APIs are:

- `ProducerInputs::from_prepared`, `from_memory`, and `gather_inputs`;
- `produce` and `SiteProducerOutput::write_to`;
- `PublisherRuntime::assemble`, `files`, `recipe_sha256`, and `finish_html`.

The WASM facade composes these internally during Publisher `prepare`. Hosts do
not mount page shells, runtime assets, or templates through separate site APIs.
Native `fig render` composes the same producer/rendering crates directly.

## Verification

Run:

```sh
cargo test -p site_producer
cargo test -p package_store template_loader
cargo test -p wasm_api site_facade_tests --lib
```

The facade tests require a complete pre-render catalog, independent render
order, handle isolation, exact external finalization, synthetic end-to-end
template/core/authored assembly, relative asset closure, and verified canonical
`SiteOutput` creation.

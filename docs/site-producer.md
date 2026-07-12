# Publisher site production

`crates/site_producer` is an internal Publisher-compatible assembly component
used by `site_engine`. It derives page shells and `_data`, and it assembles the
fixed Publisher runtime namespace. It does not define a host handoff, site
database, staged page tree, or alternate build lifecycle.

The editor's [`ARCHITECTURE.md`](../../../ARCHITECTURE.md) is normative. In that
model, this crate operates inside the `PreparedGuide -> SiteBuild` transition:

```text
PreparedGuide + exact PackageEnvironment + selected template coordinate
                              |
                    SiteEngine::prepare
                              |
             +----------------+----------------+
             |                                 |
       produce()                         PublisherRuntime::assemble()
       page shells + _data               core/template/runtime files
             |                                 |
             +----------- authored files ------+
                              |
                  immutable RenderState
                  complete output catalog
                  closed SiteBuild + objects
                              |
              outputs -> render -> finalize
                              |
                  SiteOutput + ContentStore
```

The shell/data/runtime results are private preparation artifacts. Every byte
needed to reconstruct Publisher execution is rooted in the closed `SiteBuild`
and stored by `ContentRef`; hosts never exchange these pieces individually.

## Shell and `_data` production

The production path constructs `ProducerInputs` with
`ProducerInputs::from_prepared`. It selects the compiler's explicit primary
ImplementationGuide, verifies resource identity, preserves the primary guide's
`definition.resource[]` order, and consumes prepared navigation and authored
roles.

`produce(&ProducerInputs)` emits resource page shells and the derivable `_data`
model. Shell layout selection and output names follow Publisher configuration
fallback rules. The historical F0 US Core oracle established byte parity for
1,297 page shells and `artifacts.json`; current tests cover the in-memory
`PreparedGuide` boundary and the full browser gate exercises US Core through
the canonical SiteEngine path. Other data files explicitly classify Publisher
run-context gaps where source semantics are insufficient, such as
Publisher-assigned OIDs, validation counters, tooling strings, and global
previous/next interleaving.

There is no production `produce -> write temp/pages -> reopen` transition.
Shells and data move directly into the immutable render model and closed build.

## Generated fragments are renderer-private

Generated fragment bodies are not an output directory produced by this crate.
During Publisher Liquid evaluation, `render_page` maps a registered include
name to a typed `ArtifactKey` and resolves it synchronously through the captured
immutable `ArtifactResolver`. Authored and template includes are ordinary
mounted inputs with exact provenance.

This late discovery remains inside one Publisher runtime. A ready value or typed
terminal observation is recorded in the page's reads; the host never receives a
file-miss callback and never materializes a fragment escape-hatch directory.

## Publisher runtime assembly

`publisher_runtime::PublisherRuntime::assemble` consumes:

- the exact resolved FHIR core coordinate and renderer-visible package view;
- the fully materialized, authenticated template base chain;
- the small audited irreducible embedded runtime payload.

Precedence is explicit and collision checked:

```text
embedded runtime < exact FHIR core < selected template < authored files
```

Standard tree icons, fixed table images, and FHIR CSS come from the exact core
package. Template-owned fonts and scripts come from the selected template.
Only irreducible files are embedded, with license notices. Generated
`tbl_bck*` backgrounds use deterministic inline SVG rather than an editor-only
asset side channel. The narrow jQuery compatibility transform is gated by exact
input bytes and order, then applied after Liquid and before content addressing.

The runtime recipe binds selected bytes, provenance, licenses, transform
versions, and core coordinate. It participates in renderer and output identity.
Page-relative aliases such as `en/assets/app.css` may point to the same
`ContentRef` as a canonical asset, so URL closure does not duplicate bytes.

## Fresh-process restoration

Publisher preparation closes the semantic documents, authored roles,
materialized template files, runtime files, and renderer package evidence into
the `SiteBuild` object closure. `SiteEngine::restore` verifies those objects and
recipes, reconstructs the same producer/runtime/model/render state, and installs
an ordinary handle. The restored host then uses only `outputs`, `render`, and
`finalize`.

The original project tree, package cache, and preparing process are not runtime
dependencies. Forward and reverse render orders must yield identical content
references and canonical `SiteOutput` bytes.

## Rust API scope

The production composition points are:

- `ProducerInputs::from_prepared`;
- `produce`;
- `PublisherRuntime::assemble`, `files`, `recipe_sha256`, and `finish_html`.

`from_memory` remains useful for focused captured-value fixtures. The former
filesystem `gather_inputs`, `LayoutSource::Dir`, and
`SiteProducerOutput::write_to` seams and their staged-tree tests are deleted.
Oracle harvesting is separate tooling and cannot become a production host API.

## Verification

Run:

```sh
cargo test -p site_producer
cargo test -p site_engine
cargo test -p package_store template_loader
cargo test -p wasm_api site_facade_tests --lib
```

The gates cover complete pre-render catalogs, collision handling, independent
render order, exact subject metadata, Publisher closure and fresh-process
restore, relative asset closure, external finalization, and verified canonical
`SiteOutput` construction.

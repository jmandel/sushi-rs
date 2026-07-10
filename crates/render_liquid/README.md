# render_liquid — native Publisher-template Liquid

This Rust workspace crate interprets the Liquid subset used by native Publisher
templates, matching **Jekyll 4.4.1 / Liquid 4.0.4** byte-for-byte on the measured
survey cutline plus the US Core T2 layer. It began as task F1c of the now
historical `docs/stock-template-renderer-plan.md`; the current composition is
documented in the repository README and `docs/hosting.md`.

`render_liquid` powers both `fig` native Publisher rendering and the WASM stock
template surface (`Session.renderPage` / `Session.renderLiquid`). It is a member
of the root Rust workspace, not a standalone workspace.

It does **not** power Cycle. The Cycle external builder intentionally uses
LiquidJS through one shared Cycle content implementation for both its native CLI
and browser renderer. Thus the overall stack has two Liquid implementations,
one per renderer architecture, rather than two competing implementations of the
same native path.

When Jekyll's Liquid diverges from the Shopify spec, **Jekyll wins** — every such
case is pinned by the oracle (`scripts/liquid-oracle.rb`), not docs.

## Layout

```
crates/render_liquid/
  Cargo.toml            # crate manifest; versions/dependencies come from root workspace
  src/
    value.rs            # Value model + EXACT truthiness/coercion/compare/to_s
    lexer.rs            # {{ }} / {% %} tokenizer + whitespace control (-%}),
                        #   verbatim {% raw %} capture
    ast.rs              # AST (nodes, expr/filter pipeline, conditions)
    parser.rs           # tokens -> AST (Liquid grammar: filters, if/for/case,
                        #   include params, index-with-filters, {{}}-in-tag)
    filters.rs          # string + array/collection filters (Liquid + Jekyll)
    render.rs           # tree-walking evaluator: scopes, forloop, blank-block
                        #   skip, include resolution, raw quirk
    provider.rs         # DataProvider trait (the pluggable seam) + JsonProvider
    lib.rs              # public API `render_with(...)` + `tag_registry()`
    bin/render.rs       # CLI mirroring the oracle, for the differential gate
  tests/
    semantics.rs        # unit tests pinning load-bearing behaviors
    fixtures/           # 34 synthetic per-construct fixtures (+contexts)
    corpus/             # gate manifest + generated contexts/overlays
scripts/
  liquid-oracle.rb        # the ORACLE: Jekyll 4.4.1 filter set + options
  liquid-build-context.rb # materialize site.data the Jekyll way (CSV/YAML)
  liquid-diff.sh          # per-file byte diff (fixtures | corpus | one)
  liquid-gate-setup.sh    # build corpus contexts/overlays + manifest
  liquid-gate.sh          # full differential gate + diff classifier
```

## The `DataProvider` seam

Dynamic lookups are supplied through a small synchronous trait. The crate has no
filesystem, database, compiler, or `SiteBuild` dependency:

```rust
pub trait DataProvider {
    fn site_data(&self, path: &[&str]) -> Option<Value>;   // {{ site.data.fhir.path }}
    fn site(&self, path: &[&str]) -> Option<Value>;        // other site.*
    fn include_source(&self, name: &str) -> Option<String>;// {% include NAME %}
}
```

`page.*` / `include.*` / loop vars are ordinary globals/scoped variables.

```rust
let out = render_liquid::render_with(src, &provider, &[("page", page_value)], Options::default());
```

In the native page stack, `render_page::PageProvider` implements this trait over
the mounted `_data` and include trees. A registered Publisher fragment name may
cross a separate, explicit boundary in `render_page`: the legacy filename is
translated to a typed `site_build::ArtifactKey`, resolved by an
`ArtifactResolver`, cached by that key, and recorded in `PageArtifactReadSet`.
This crate only asks its provider for include source; it does not know whether a
caller served an ordinary file or a typed materialized artifact.

`site.db` is not the backing model for this trait. In the current architecture it
is one compatibility artifact for the closed Cycle render target. Native
Publisher `site.data.*` comes from the produced/mounted `_data` model.

## Scope (what's IN)

**T1** — `include` (registry-resolved, parameterized, dynamic `{{path}}.md`),
`assign`, `capture`, `comment`, `raw`, `{{ output }}`, ~17 string filters,
`site.data.*`/`page.*`/`include.*` via the provider.

**T2** — `for` (`forloop.*`, `offset:`/`limit:`/`reversed`, `break`/`continue`,
for-else), full `if`/`elsif`/`else`/`unless` (`== != <> < > <= >= contains`,
`.size`, `and`/`or`), array filters `split | where | where_exp | sort |
sort_natural | uniq | map | join | size | first | last | reverse | compact |
concat | group_by`, `case`/`when`, parameterized includes.

Behaviors matched to Jekyll (cited in-code, verified via the oracle):
- truthiness (only `nil`/`false` falsy); `nil == empty` is **false**; `x ==
  blank` is **always false** (no `blank?` in plain Jekyll Liquid);
- **blank-block skip**: an `if`/`for`/`case` whose whole body is whitespace +
  blank tags (assign/capture/comment/raw) emits nothing;
- `sort` nils-**first**; `trim` is **not** a filter (passthrough);
- filters inside index brackets (`item["title" | trim]`); `{{ }}` interpolated
  inside a tag (`{% assign x = {{v}} | append %}`); `site.data.[expr]`;
- `{% raw %}` preserved **verbatim** (exact spacing) by default.

## The `{% raw %}` Publisher quirk

The Java Publisher evaluates `{% fragment %}`/`{% include %}` even inside
`{% raw %}` (survey nasty #4). This engine does the **correct** thing by default
(raw verbatim); set `Options { publisher_raw_quirk: true }` to reproduce the wart
(the raw body is re-parsed + evaluated). Documented in `tag_registry()`.

## Out of scope (handled by a surrounding layer or rejected)

- **Producing artifact `.xhtml` includes** (dependency-table, globals-table,
  cross-version-analysis, ip-statements, expansion-params, table-*, …) — the
  outer native page provider's typed artifact resolver owns production.
- **Example-resource includes** (`{% include(_relative) X.json|xml %}` of
  checked-in/Publisher example instances) unless the provider supplies them as
  ordinary include files.
- `{% sql %}` / `{% sqlToData %}` — not part of native Publisher Liquid. Cycle's
  separate LiquidJS implementation may explicitly inject legacy read-only SQL
  in its trusted native CLI; portable/browser Cycle mode does not.
- `highlight` / `tablerow` / `cycle` — measured **zero** in the corpus.
- Layout inheritance / `{% layout %}` — the `page` crate (F5), not Liquid-core.

## Differential gate

Oracle = real Jekyll 4.4.1 (`Liquid::Template.register_filter(Jekyll::Filters)`,
`error_mode: warn`, `strict_*: false`), booted with a `Jekyll::Site` so
`where`/`sort`/`group_by` (which read `site.filter_cache`) work exactly. Both
engines consume one shared JSON context, so only the ENGINE differs;
`markdownify` is stubbed to a marker (`MD…/MD`) on both sides (markdown is
`render_md`, a separate crate).

```
cargo test --manifest-path crates/render_liquid/Cargo.toml   # unit + fixtures
bash scripts/liquid-diff.sh fixtures                          # 34/34 byte-equal
bash scripts/liquid-gate-setup.sh && bash scripts/liquid-gate.sh   # full corpus
```

Result (10 IGs, all liquid-bearing authored pages + includes on the cutline —
US Core's full T2 set + T1 pages, plus every other IG's T2 files):

```
PASS (byte-identical) : 388
E1 artifact .xhtml miss:   7   (out of scope: F4/F5 fragment store)
E2 example-resource miss: 54   (out of scope: F4/F5 example inclusion)
E3 publisher-custom tag :   4   (out of scope: sql/sqlToData, lang-fragment)
UNEXPLAINED            :   0
```

Every residual diff is an accounted out-of-scope class; **0 unexplained on the
cutline**. The `cargo test -p render_liquid` and 34 synthetic-fixture commands
above are the authoritative current gates rather than a duplicated test count.

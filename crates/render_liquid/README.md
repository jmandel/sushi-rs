# render_liquid — just-enough Jekyll/Liquid (T1+T2) at Publisher parity

Task **F1c** of `docs/stock-template-renderer-plan.md`. A standalone Rust crate
(its own `[workspace]`; the root workspace is untouched) that interprets the
Liquid subset the FHIR IG Publisher actually runs, matching
**Jekyll 4.4.1 / Liquid 4.0.4** byte-for-byte on the survey cutline **plus the
US-Core T2 layer** (Josh's 2026-07-03 scope decision).

When Jekyll's Liquid diverges from the Shopify spec, **Jekyll wins** — every such
case is pinned by the oracle (`scripts/liquid-oracle.rb`), not docs.

## Layout

```
crates/render_liquid/
  Cargo.toml            # OWN [workspace]; does not touch root Cargo.toml
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
    semantics.rs        # 23 unit tests pinning load-bearing behaviors
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

T1's dynamic surfaces are served through a trait so the same engine is backed by
an in-memory context today and `site.db` later (F5):

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

## Out of scope (fail-loud / emit-nothing, not silently mis-rendered)

- **Artifact `.xhtml` includes** (dependency-table, globals-table,
  cross-version-analysis, ip-statements, expansion-params, table-*, …) — these
  are Publisher-generated fragments, the `page`/fragment crate's job (F4/F5).
- **Example-resource includes** (`{% include(_relative) X.json|xml %}` of
  checked-in/Publisher example instances).
- `{% sql %}` / `{% sqlToData %}` — IG-Guidance/genomics extension (survey (d));
  plain Jekyll can't parse them either.
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
cutline**. Plus 25 `cargo test` unit tests and 34 synthetic fixtures, all
byte-equal to the oracle.

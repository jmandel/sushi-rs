# publisher_sql

`publisher_sql` is the isolated SQLite capability required by Publisher
templates. It is a pre-render close step, not a Liquid extension and not a page
renderer service.

The caller supplies the complete ordered set of this guide's compiled FHIR
resources. `SqlRuntime` projects those resources into an in-memory,
read-only Publisher-inspired database. `expand_publisher_sql` then scans the
eligible page/include sources once in bytewise path order and returns:

- rewritten ordinary Liquid sources;
- raw-isolated generated includes for direct `{% sql ... %}` results; and
- global generated `_data/<name>.json` for `{% sqlToData ... %}`.

After those files are collision-checked into the closed Publisher catalog, the
database is dropped. `render_page` and `render_liquid` never see it. A later
successful `sqlToData` definition wins in deterministic path order; malformed
sources remain untouched; errors become visible inline markup. Source, site,
row, cell, query, byte, VM-progress, and statement bounds keep the capability
finite in both native and WASM builds.

## Supported query surface

The current closed snapshot populates and authorizes reads from:

- `Resources` for this guide's own compiled resources;
- `Properties`, `Concepts`, `ConceptProperties`, and `Designations` for its own
  CodeSystems; and
- `ConceptMappings` for its own basic ConceptMaps.

SQLite JSON1 scalar functions and read-only CTE/select composition are
available. Writes, schema mutation, attachment, ambient pragmas, filesystem or
network access, extension loading, and unbounded work are denied.

This is a Rust-characterized subset, not a claim of Java Publisher
`package.db` compatibility. The provisional schema names `Metadata`,
`ValueSet_Codes`, and the CodeSystem/ValueSet list-view tables because real
Publisher queries may mention them, but they are not yet backed by complete
closed inputs. Reads fail
explicitly instead of returning plausible empty results. Dependency resources,
terminology expansions, Publisher registry metadata, configured web-path
parity, and context-aware Canonical/Resource/Coding cell rendering likewise
remain unsupported until their inputs and a pinned Java schema/query/result/error
oracle are added.

The SQLite dependency materially increases the optimized WASM. That cost is
accepted for real-template compatibility and must remain visible in release
measurements; do not add another SQL implementation or move database lifecycle
back into Liquid/page rendering.

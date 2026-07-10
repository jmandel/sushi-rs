# Engine documentation map

Documentation in this directory spans several implementation phases. Use the
following precedence when statements conflict:

1. the editor's current
   [`SPEC.md`](https://github.com/jmandel/fhir-ig-editor/blob/main/SPEC.md) for
   the cross-repository browser/renderer contract;
2. the engine [`README.md`](../README.md),
   [`crates/site_build/README.md`](../crates/site_build/README.md), and current
   guides below for engine-specific contracts; then
3. dated plans, audits, surveys, and worklogs as historical evidence.

## Current guides and contracts

| Document | Authority |
| --- | --- |
| [`hosting.md`](hosting.md) | Current guide to isolated `Session` ownership, compile/projection sequencing, closed external builds, native typed resolution, CLI hosting, and envelopes. |
| [`site-producer.md`](site-producer.md) | Current implementation note for source-driven Publisher page shells and `_data`, including the landed WASM path and known model gaps. |
| [`package-derived-index.md`](package-derived-index.md) | Implemented native CAS derived-index design and invariants. |
| [`designs/package-acquisition-plan.md`](designs/package-acquisition-plan.md) | Historical design for the now-implemented CAS/acquisition/materialization subsystem; current commands live in the root README. |

The API definitions themselves remain authoritative: `crates/site_build`,
`crates/wasm_api`, `crates/render_page`, and `crates/fig`.

## Reference evidence

These are still useful inputs, but they describe a pinned corpus, tool version,
or investigated surface rather than today's architecture:

| Document | What it preserves |
| --- | --- |
| [`ig-jekyll-surface-survey.md`](ig-jekyll-surface-survey.md) | Empirical Liquid/Jekyll feature distribution. |
| [`publisher-fragments-notes.md`](publisher-fragments-notes.md) | Pinned Java Publisher fragment-generation behavior. |
| [`rust-fragment-generator-feasibility.md`](rust-fragment-generator-feasibility.md) | Pre-implementation feasibility study. |
| [`template-machinery-notes.md`](template-machinery-notes.md) | Investigation that led to the driven template loader. |
| [`layer-b-audit.md`](layer-b-audit.md) | Pinned Publisher/core versioning and projection audit. |
| [`perf31.md`](perf31.md), [`perf-snapshot-gen.md`](perf-snapshot-gen.md) | Performance methodology and measured findings. |

## Historical plans and ledgers

These explain how the current code was derived. They are not active API or
product specifications:

- [`stock-template-renderer-plan.md`](stock-template-renderer-plan.md)
- [`unified-cli-plan.md`](unified-cli-plan.md)
- [`cycle-package-db-plan.md`](cycle-package-db-plan.md)
- [`wasm-editor-plan.md`](wasm-editor-plan.md)
- [`fhir-ig-editor-spec.md`](fhir-ig-editor-spec.md)
- [`render-worklog.md`](render-worklog.md)
- [`simplification-ledger.md`](simplification-ledger.md)
- [`opfs-cas-design.md`](opfs-cas-design.md)

Their shipped findings may still be correct, but terms such as “all adapters
consume site.db,” “first include miss,” “global engine,” or milestone/branch
status must not be treated as the current contract. In particular, `SiteBuild`
is now renderer-neutral, Cycle receives a verified `ClosedSiteBuild`, and native
Publisher pages use a typed `ArtifactResolver`.

## Point-in-time corpus and performance records

`harvest-findings.md`, `holdout-findings.md`, `mining-findings.md`,
`top20-findings.md`, `next20-findings.md`, `perf-map.md`, `perf-notes.md`, and
`perf-protocol.md` are dated validation/performance records. The JSON files in
this directory are corresponding fixture/configuration data, not narrative
contracts.

For gate and oracle scripts, see [`scripts/README.md`](../scripts/README.md).
For snapshot-specific history, see `snapshot/AGENTS.md` and
`snapshot/REWORK-PLAN.md`.

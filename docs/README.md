# Engine documentation map

Documentation in this directory includes current contracts and dated evidence.
Use the following precedence when statements conflict:

1. the editor's current [`ARCHITECTURE.md`](../../../ARCHITECTURE.md) for
   the cross-repository browser/renderer contract;
2. the engine [`README.md`](../README.md),
   [`crates/site_build/README.md`](../crates/site_build/README.md), and current
   guides below for engine-specific contracts; then
3. dated audits, surveys, and performance records as historical evidence.

## Current guides and contracts

| Document | Authority |
| --- | --- |
| [`hosting.md`](hosting.md) | The four-operation site facade, immutable handles, ContentStore plumbing, generator differences, and removed APIs. |
| [`site-producer.md`](site-producer.md) | Publisher page/data/runtime assembly, asset provenance, relative aliases, and known model gaps. |
| [`package-derived-index.md`](package-derived-index.md) | Implemented native CAS derived-index design and invariants. |
| [`opfs-cas-design.md`](opfs-cas-design.md) | Implemented cross-host `ContentStore`, binary `PreparedPackage`, OPFS warm path, resolution-lock rules, and benchmark commands. |
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
| [`layer-b-audit.md`](layer-b-audit.md) | Pinned Publisher/core versioning and projection audit. |
| [`perf31.md`](perf31.md), [`perf-snapshot-gen.md`](perf-snapshot-gen.md) | Performance methodology and measured findings. |
| [`sushi-rust-port-plan.md`](../sushi-rust-port-plan.md) | Original compiler-port design brief and pre-port performance evidence; superseded as architecture. |

Obsolete implementation plans and worklogs were removed when their v1 APIs were
deleted. Historical documents that remain preserve unique oracle or measurement
evidence; they do not define a supported host surface.

## Point-in-time corpus and performance records

`harvest-findings.md`, `holdout-findings.md`, `mining-findings.md`,
`top20-findings.md`, `next20-findings.md`, `perf-map.md`, `perf-notes.md`, and
`perf-protocol.md` are dated validation/performance records. The JSON files in
this directory are corresponding fixture/configuration data, not narrative
contracts.

For gate and oracle scripts, see [`scripts/README.md`](../scripts/README.md).
For snapshot-specific history, see `snapshot/AGENTS.md` and
`snapshot/REWORK-PLAN.md`.

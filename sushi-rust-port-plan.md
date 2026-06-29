# Rust SUSHI Port Plan

Date: 2026-06-29
Worktree: `/home/jmandel/periodicity/.worktrees/sushi-package-index`
Primary benchmark IG: `/home/jmandel/periodicity/temp/ips-ig`

## Purpose

This document sketches a performance-focused Rust implementation of SUSHI that
aims for byte-identical output and equivalent QA/diagnostics to the current
TypeScript `fsh-sushi` implementation.

The point is not to make a cleaner FSH compiler by interpretation. The point is
to make the current SUSHI behavior fast and reproducible, while preserving the
real behavior that IGs rely on: rule ordering, soft indexing, insert-rule
expansion, caret behavior, diagnostics, JSON property order, and all generated
resources.

## Current Evidence

The IPS benchmark establishes the scale of the opportunity:

| Mode | IPS wall time | Output bar |
|---|---:|---|
| Stock TypeScript SUSHI with local packages | 57.22s | Baseline |
| Indexed package DB only, warm | 44.40s | Byte-identical |
| Current TypeScript spike, full output and full QA | ~5.23s | Byte-identical |

The current TypeScript spike is not skipping instances or diagnostics. It keeps
full generated output and SUSHI QA. The winning opt-in stack includes:

- SQLite package index for dependency loading.
- Trusted indexed package state for warm local package caches.
- Cached resource queries over package facts.
- Dynamic caret setter planning for `ElementDefinition` metadata paths.
- Immutable templates for repeated `ValueSet`/`CodeSystem` own-SD lookups.
- StructureDefinition tree/path/slice caches.
- Cached path parsing and split operations.
- Fast required-element validation.
- Post-insert-rule `FSHTank` lookup cache.

The latest valid IPS run with this stack:

```text
real 5.229s
user 7.506s
sys  0.654s
```

The strict output gate was:

```sh
diff -rq temp/sushi-ips-stock/fsh-generated/resources \
         temp/sushi-ips-fast-tankcache/fsh-generated/resources
```

No diff output.

The remaining sampled CPU profile is now spread across:

- lodash `cloneDeep` / assignment machinery;
- `StructureDefinition` and `ElementDefinition` `fromJSON`, `toJSON`, diff, and
  tree work;
- FSH parser / ANTLR work;
- residual `replaceField` and cleanup walks;
- package-loader freezing and metadata handling;
- some remaining path/tree lookup costs.

That remaining shape matters. A Rust port is not mainly competing with the
original 57s anymore; a serious Rust port must beat a strengthened TypeScript
architecture that already runs IPS in about 5 seconds.

## Performance Target

For the "packages already local and indexed" case, a well-designed Rust SUSHI
should plausibly target:

| Target | Meaning |
|---|---|
| 2.5s | Conservative first production-quality goal for IPS. |
| 1.5s | Ambitious but credible goal after model, parser, and serializer tuning. |
| <1.0s | Stretch goal requiring very tight parsing, parallelism, and minimal allocation. |

The target includes full output and full QA/diagnostics. Timings that skip
instances, skip validation, skip diagnostics, or emit partial output do not
count.

For a cold network cache that must download packages, SUSHI is not the only
cost. The Rust compiler should report separate timings for:

- package acquisition;
- package indexing;
- FSH parse/import;
- FSH-to-FHIR export;
- validation/diagnostics;
- JSON emission.

## Philosophy

### Behavior First

The Rust port is a compatibility compiler. It must reproduce observed SUSHI
behavior before it tries to improve on it.

When the FSH spec and current SUSHI differ, current SUSHI wins unless we make an
explicit compatibility-break decision.

### Global Shape Before Local Optimizations

The main TS SUSHI cost came from dynamic scans and generic mutation APIs. The
Rust design should avoid reproducing that shape and then patching around it.

The first design pass should establish:

- typed AST and typed rule structures;
- stable interning for strings, canonicals, paths, and ids;
- arena-backed FHIR model trees;
- first-class lookup indexes;
- immutable package/base templates;
- explicit overlay/edit plans;
- deterministic JSON serialization.

Only after this shape exists should agents chase local hotspots.

### Make the Fast Path the Only Path When Possible

The current TS spike uses opt-in fast paths with fallbacks because it is patching
an existing dynamic implementation. A Rust implementation can often make the
fast path the normal implementation.

If a path is unsupported, fail loudly in a development gate. Do not silently
fall back to a slow generic path unless the fallback is intentionally counted,
reported, and covered by a parity test.

### Pay for Work Once

Repeated work should become persistent facts:

- parsed package resources become package DB facts;
- FSH names, urls, ids, and versions become tank indexes;
- FSH paths become interned path plans;
- StructureDefinition element ids and paths become indexed element handles;
- slice metadata becomes slice indexes;
- base StructureDefinitions become immutable templates;
- output JSON property ordering becomes schema-guided emitters.

## System Boundaries

The Rust implementation should be split into crates with clear boundaries:

```text
fsh_lexer_parser
  Reads FSH text and emits a typed AST with source spans.

fsh_model
  Defines FSH entities, rules, rule sets, aliases, paths, and source metadata.

fhir_model
  Defines FHIR resource structs used by SUSHI plus dynamic extension/value[x]
  support where required.

package_store
  Resolves local FHIR package facts from the package DB/cache/lock model.

compiler
  Expands insert rules, resolves tank references, exports FSH definitions to
  FHIR resources, applies deferred rules, and produces diagnostics.

diagnostics
  Owns diagnostic codes, messages, source spans, severity, stable ordering, and
  compatibility formatting.

json_emit
  Emits byte-stable JSON with SUSHI-compatible property ordering.

cli_or_adapter
  Provides a command/API compatible with our integrated `runSushiBuild` use.
```

The port should first be callable from Bun/TS through a narrow adapter. The
existing publisher can then run old and new SUSHI side by side.

## Core Data Structures

### String and Symbol Interner

Most hot paths compare repeated strings:

- FSH names and ids;
- canonical URLs;
- rule paths;
- caret paths;
- element ids;
- element paths;
- type codes;
- extension URLs;
- slice names.

Use a process-local interner:

```rust
struct Symbol(u32);
struct Interner {
    strings: Vec<Box<str>>,
    map: HashMap<Box<str>, Symbol>,
}
```

Store symbols in hot structures and keep original strings for emission and
diagnostics.

### Source Spans

Diagnostics must stay honest. Every AST node and rule should carry:

```rust
struct SourceSpan {
    file: FileId,
    start_line: u32,
    start_col: u32,
    end_line: u32,
    end_col: u32,
}
```

Applied insert-rule spans also need `applied_file` and `applied_location`,
matching SUSHI behavior.

### FSH AST and Entity Arena

Represent imported FSH as typed entities:

```rust
enum EntityKind {
    Profile,
    Extension,
    Logical,
    Resource,
    Instance,
    ValueSet,
    CodeSystem,
    Invariant,
    RuleSet,
    Mapping,
}

struct Entity {
    id: EntityId,
    kind: EntityKind,
    name: Symbol,
    id_value: Symbol,
    parent: Option<Symbol>,
    usage: Option<Usage>,
    rules: Vec<RuleId>,
    source: SourceSpan,
}
```

Rules live in an arena so rule sets can be expanded by copying compact rule ids
or by cloning only when path/context/source metadata must change.

### Rule Indexes

Avoid repeated `findLast` over rules. For each entity, build a small rule index:

```rust
struct RuleIndex {
    last_assignment_by_path: HashMap<PathSymbol, RuleId>,
    last_caret_by_path: HashMap<(PathSymbol, CaretPathSymbol), RuleId>,
    insert_rules: Vec<RuleId>,
    rules_by_kind: EnumMap<RuleKind, Vec<RuleId>>,
}
```

The index must be rebuilt or incrementally updated after insert-rule expansion.
This replaces the TypeScript pattern where `getValueFromRules` repeatedly scans
rules.

### Tank Index

`FSHTank.fish` was a meaningful remaining hot path in TypeScript. In Rust, build
indexes as the normal representation:

```rust
struct TankIndex {
    by_name: HashMap<(EntityKind, Symbol), EntityId>,
    by_id: HashMap<(EntityKind, Symbol), EntityId>,
    by_url: HashMap<(EntityKind, CanonicalKey), EntityId>,
    by_versioned_url: HashMap<(EntityKind, CanonicalKey, VersionSymbol), EntityId>,
    instances_by_name: HashMap<Symbol, EntityId>,
    instances_by_id: HashMap<Symbol, EntityId>,
    rule_sets_by_name: HashMap<Symbol, EntityId>,
    aliases: HashMap<Symbol, Symbol>,
}
```

This index should be built in two phases:

1. Initial index after parse/import, sufficient for insert-rule expansion.
2. Final index after insert-rule expansion, including url/name/version values
   derived from rules.

Lookups should never scan all entities in normal operation.

### Package Store

Use the package-index design as a first-class dependency service. The Rust
compiler should not walk `~/.fhir/packages` as source of truth.

```rust
struct PackageStore {
    lock: PackageLock,
    db: PackageIndexDb,
    graph: PackageGraph,
    resource_index: PackageResourceIndex,
}
```

Resource facts should include:

- package name/version/role;
- source tarball digest when available;
- manifest digest;
- resource type/id/url/version/status;
- canonical lookup keys;
- compact JSON blob or offset into package archive/cache;
- precomputed searchable metadata needed by SUSHI.

The store should support mutable coordinates (`current`, `dev`) with explicit
guarantee levels:

- `locked`: name/version/registry/tarball digest known;
- `snapshotted`: mutable coordinate resolved to a content digest at a point in
  time;
- `floating`: allowed only with a warning and non-reproducible build manifest.

### FHIR StructureDefinition Arena

The biggest structural opportunity is to stop representing
StructureDefinitions as arrays that every operation scans.

Use an arena:

```rust
struct SdArena {
    elements: Vec<ElementNode>,
    root: ElementId,
    by_id: HashMap<ElementIdSymbol, ElementId>,
    by_path: HashMap<PathSymbol, SmallVec<[ElementId; 2]>>,
    children: Vec<SmallVec<[ElementId; 8]>>,
    parent: Vec<Option<ElementId>>,
    slices: SliceIndex,
}

struct ElementNode {
    id: ElementIdSymbol,
    path: PathSymbol,
    slice_name: Option<Symbol>,
    type_codes: SmallVec<[Symbol; 2]>,
    min: u32,
    max: MaxCardinality,
    data: ElementData,
}
```

`ElementData` carries the full serializable ElementDefinition fields. The arena
owns structural links; operations should not rediscover them by filtering arrays.

### Immutable Templates and Overlays

FHIR core and package StructureDefinitions are mostly immutable templates.
Profiles create mutable overlays.

```rust
struct SdTemplate {
    arena: Arc<SdArena>,
    json_order: Arc<JsonOrderPlan>,
    package_key: PackageResourceKey,
}

struct SdWork {
    base: Arc<SdTemplate>,
    overlay: OverlayArena,
    inserted_elements: Vec<ElementId>,
    dirty_indexes: DirtyFlags,
}
```

The key design question: avoid deep clone of the entire base unless mutation
requires it. Most rules touch a small subset of elements. A copy-on-write
overlay lets path validation and rule application share base data.

When output must be emitted, materialize the final element order deterministically
from base plus overlay plus insertions.

### Path Plans

Path parsing should be a compile-time-like operation in the run:

```rust
struct FshPathPlan {
    original: Symbol,
    parts: Box<[PathPart]>,
    has_soft_index: bool,
    has_slice_ref: bool,
    target_hint: TargetHint,
}
```

Caret paths get a separate plan:

```rust
struct CaretPlan {
    original: Symbol,
    steps: Box<[CaretStep]>,
    value_kind: ValueKind,
    setter_shape: SetterShape,
}
```

The TypeScript spike showed dynamic caret setter planning was a major win. In
Rust, every caret path should be planned once and then executed by an indexed
setter.

### Diagnostic Store

Diagnostics should be data, not side effects:

```rust
struct Diagnostic {
    severity: Severity,
    code: DiagnosticCode,
    message: String,
    source: SourceSpan,
    order: u64,
}
```

Stable ordering is important. The compiler should assign monotonic diagnostic
order values at the same logical points as SUSHI.

## High-Level Compiler Pipeline

```text
1. Load config and package lock/context.
2. Resolve package graph and open package index.
3. Read FSH files and config.
4. Parse FSH into AST and source spans.
5. Build initial tank indexes.
6. Check duplicate names and other import-time diagnostics.
7. Expand insert rules globally, preserving source/applied source behavior.
8. Build final rule indexes and tank indexes.
9. Load package/core templates lazily through PackageStore.
10. Export StructureDefinitions, CodeSystems, ValueSets, Instances, Mappings.
11. Apply deferred rules.
12. Run required validation and compatibility diagnostics.
13. Emit resources with SUSHI-compatible JSON ordering.
14. Emit FSH index and IG scaffolding artifacts needed by the publisher.
15. Write metrics and manifest.
```

The pipeline should expose phase timings from day one.

## Low-Level Algorithms

### Insert-Rule Expansion

Current SUSHI mutates `rules` arrays in place and recursively expands inserted
RuleSets. Rust should model this directly:

1. Iterate original rules in order.
2. Copy non-insert rules to an expanded vector.
3. For each insert rule, resolve RuleSet by name or applied-parameter key.
4. Detect circular insertion by rule-set identifier stack.
5. Recursively expand nested inserts.
6. Clone rule-set rules only when path context, path array, or applied source
   metadata changes.
7. Preserve `[+]` to `[=]` context behavior after the first applied rule.
8. Replace the entity rule vector atomically.
9. Rebuild rule indexes for affected entities.

This is a global phase. Do not interleave final tank indexing with mutable rule
expansion.

### Tank Fishing

After insert-rule expansion, fishing should be pure indexed lookup:

1. Resolve alias once.
2. Split version suffix once.
3. Query direct maps for requested entity kinds.
4. If a kind can be represented by a definition instance, query the instance
   indexes with the same compatibility predicates SUSHI uses.
5. Preserve type search order and stop-on-first behavior.

For `fishAll`, return values in SUSHI-compatible entity order. A sorted or
hash-map-native order is not acceptable unless it matches current output and
diagnostic behavior.

### StructureDefinition Rule Application

The TS hot path is repeated path validation and tree rediscovery. Rust should use
the arena and path plans:

1. Resolve parent template.
2. Create `SdWork` overlay.
3. For each rule in order:
   - validate rule type against entity kind;
   - use `FshPathPlan`/`CaretPlan`;
   - locate target element by id/path/slice index;
   - apply value through a typed setter;
   - update only affected indexes.
4. Defer rules that SUSHI defers today.
5. Compute differential from overlay and changed element metadata, not from a
   full `cloneDeep` diff if possible.

### Slice Handling

Slice operations need first-class indexes:

```rust
struct SliceIndex {
    by_sliced_element: HashMap<ElementId, SmallVec<[ElementId; 4]>>,
    by_slice_name: HashMap<(ElementId, Symbol), ElementId>,
    by_discriminator: HashMap<ElementId, DiscriminatorPlan>,
}
```

Adding a slice should:

- allocate new element nodes in output order;
- set parent/child links;
- update `by_id`, `by_path`, and slice maps;
- record the insertion for deterministic emission.

No operation should call "find connected elements" by scanning all elements.

### Instance Export

Instances must remain in scope. The Rust port should not treat them as a second
phase that can be skipped.

Use a schema-guided builder:

```rust
struct InstanceWork {
    resource_type: Symbol,
    root: JsonNodeId,
    path_index: InstancePathIndex,
    contained_index: ContainedResourceIndex,
}
```

Assignments should use path plans and schema facts from the target
StructureDefinition. Required validation should traverse precomputed required
plans rather than recursively inspecting generic JS-like objects.

### ValueSet and CodeSystem Export

These are smaller but still need parity:

- metadata source from config/rules;
- concept ordering;
- include/exclude ordering;
- caret assignment behavior;
- JSON ordering.

Use typed structures, but keep extension/value[x] escape hatches.

### JSON Emission

Byte-identical output requires deterministic property order. Do not use
generic map iteration for resources.

Use generated or hand-written emit plans:

```rust
struct JsonEmitPlan {
    resource_type: ResourceType,
    fields: &'static [FieldEmitter],
}
```

Rules:

- emit resource fields in SUSHI-compatible FHIR order;
- emit primitive sibling fields immediately after their primitive field when
  SUSHI does;
- preserve extension order;
- preserve array order;
- preserve number/string/boolean formatting;
- end files with the same newline convention.

The emitter should support "compat mode" snapshots generated from current SUSHI
goldens if hand-maintaining order becomes risky.

## Package and Cache Architecture

The Rust compiler should share the project package infrastructure:

- project or CI lockfile is source of reproducibility truth;
- `~/.fhir/packages` is an optional seed/import/export cache;
- package SQLite index is the source of query facts;
- package tarball digests and manifest digests are recorded;
- mutable coordinates are allowed but labeled by guarantee level.

The compiler must report:

- package graph;
- package acquisition source;
- index hits/misses;
- package facts loaded;
- resource facts queried;
- floating/mutable package coordinates;
- whether the run is reproducible.

This lets the same binary support:

- strict CI;
- local warm development;
- intentionally floating `current`/`dev` workflows;
- offline replay.

## Parallelism

Parallelism should come after deterministic single-threaded parity.

Safe candidates:

- parse FSH files in parallel, then merge by stable file order;
- index package resources in parallel, then commit sorted facts;
- export independent ValueSets/CodeSystems in parallel after global indexes are
  built;
- precompute required validation plans in parallel.

Riskier candidates:

- StructureDefinition export, because profiles can fish/export each other;
- instance export, because diagnostics and generated contained resources must
  remain stable;
- deferred rules, because order can affect diagnostics and output.

Parallel phases must preserve deterministic output and diagnostic order. Use
logical sequence numbers if work is parallelized.

## Test Strategy

### Golden IG Corpus

Start with the same corpus used for package.db parity work:

- Cycle
- IPS
- SDC
- CRD
- US Core
- mCODE

For each IG, keep a stock SUSHI output directory as the oracle. The primary gate:

```sh
diff -rq <stock-sushi>/fsh-generated/resources <rust-sushi>/fsh-generated/resources
```

No diff output is the target. If byte-identical output is too strict in an early
phase, every diff must be classified with an explicit issue and a plan to close
it. Do not normalize silently.

### Diagnostic Parity

Full output is not enough. SUSHI QA behavior is part of the product.

Capture from stock SUSHI:

- errors;
- warnings;
- message text;
- severity;
- source file and location;
- ordering.

Rust must match these for the corpus. When messages differ, classify:

- missing diagnostic;
- extra diagnostic;
- changed text;
- changed location;
- changed ordering.

### Unit Fixtures

Create focused fixtures for:

- insert-rule expansion;
- parameterized RuleSets;
- nested RuleSets and circular insertion;
- soft indexing `[+]`, `[=]`, and named indices;
- caret paths with extensions and value[x];
- slicing and reslicing;
- profile-discriminated references;
- instances as definitions;
- ValueSet and CodeSystem caret metadata;
- primitive sibling fields;
- contained references;
- invariant severity/expression handling;
- duplicate-name warnings.

Every fixture should run against both stock TypeScript SUSHI and Rust SUSHI.

### Property and Differential Tests

Use property tests where behavior is structural:

- path parser round-trips;
- JSON emitter preserves parse tree;
- package lock digest verification;
- slice index invariants;
- parent/child tree consistency;
- no dangling element ids after insertion.

Use differential tests where behavior is compatibility-sensitive:

- random small FSH snippets compiled by both compilers;
- compare resources and diagnostics;
- shrink failing snippets.

### Performance Tests

Performance tests should be automated but not brittle:

- run each benchmark at least three times;
- record median and min;
- separate package load/index/export/write phases;
- save CPU profiles when a regression exceeds threshold;
- compare against checked-in baseline metrics.

Minimum benchmark set:

- IPS full output/full QA;
- CRD full output/full QA;
- SDC full output/full QA;
- one package-heavy cold-index scenario;
- one mutable-coordinate scenario, labeled non-reproducible.

## Phased Development

### Phase 0: Harness and Truth Tables

Goal: no Rust compiler yet; build the honesty framework.

Deliverables:

- command wrapper that runs stock TS SUSHI and captures outputs/diagnostics;
- corpus config for Cycle/IPS/SDC/CRD/US Core/mCODE;
- byte-diff reporter for resources;
- diagnostic-diff reporter;
- phase timing schema;
- baseline metrics committed as artifacts or generated under `temp/`.

Exit criteria:

- one command can regenerate stock goldens;
- one command can compare a candidate compiler to stock;
- IPS stock and current TS spike numbers are recorded.

### Phase 1: Package Store and JSON Emitter Skeleton

Goal: prove reproducible package access and byte-stable JSON emission.

Deliverables:

- Rust package store reads the package DB/lock;
- resolves core and dependency resources by canonical/type/id;
- emits selected package resources byte-identically after parse/re-emit when
  possible;
- reports package graph and reproducibility mode.

Exit criteria:

- package resolution for IPS/CRD matches current package graph;
- no scanning of `~/.fhir/packages` in warm indexed mode;
- JSON emitter passes resource-order fixtures.

### Phase 2: FSH Parser Compatibility

Goal: parse real IG FSH into a typed AST with source spans.

Deliverables:

- lexer/parser for the FSH subset used by the corpus;
- AST dump comparable to TS import model;
- alias, entity, rule, and source-span capture;
- duplicate-name checks.

Exit criteria:

- all corpus FSH files parse;
- parse diagnostics match stock for clean IGs;
- parse phase is timed separately;
- parser design is stable enough before export work begins.

### Phase 3: Insert Rules and Tank Indexes

Goal: implement the global shape that eliminates repeated rule/entity scans.

Deliverables:

- initial and final tank indexes;
- insert-rule expansion with parameterized RuleSets;
- rule indexes per entity;
- tank fishing parity tests.

Exit criteria:

- focused insert-rule fixtures match TS output and diagnostics;
- `fish`/`fishAll` results match TS for corpus queries captured from stock;
- no generic entity scans in normal post-expansion fishing.

### Phase 4: ValueSet and CodeSystem Export

Goal: start with smaller resource families.

Deliverables:

- metadata export;
- concept/include/exclude export;
- caret assignment support for these resources;
- JSON output.

Exit criteria:

- IPS ValueSets match stock SUSHI;
- SDC/CRD ValueSets match stock SUSHI;
- diagnostic parity for focused fixtures.

### Phase 5: StructureDefinition Arena and Simple Profiles

Goal: establish the central model architecture before chasing edge cases.

Deliverables:

- immutable base templates;
- `SdWork` overlay;
- element id/path/child/slice indexes;
- path and caret plans;
- metadata/rule application for simple profiles/logicals/resources.

Exit criteria:

- simple local fixtures match;
- selected Cycle profiles match;
- phase metrics show no broad element-array scans.

### Phase 6: Full StructureDefinition Compatibility

Goal: close SUSHI's hard profile behavior.

Deliverables:

- slicing/reslicing;
- add element rules;
- bind/only/contains/obeys/card rules;
- extension caret paths;
- deferred rules;
- differential generation;
- validation diagnostics.

Exit criteria:

- IPS profiles match stock;
- SDC profiles match stock;
- CRD profiles match stock;
- diagnostic parity is within classified zero-drift target.

### Phase 7: Instance Export and Required QA

Goal: full output means full instances and QA.

Deliverables:

- instance assignment path planner;
- contained resource handling;
- implied properties;
- required-element validation;
- multiple-choice checks;
- nameless slice checks;
- reference rewriting where SUSHI does it.

Exit criteria:

- IPS 148 instances match stock;
- CRD/SDC instances match stock;
- required-validation diagnostics match stock;
- no "skip QA" mode counts as passing.

### Phase 8: Full Corpus Parity

Goal: run the real corpus end to end.

Deliverables:

- all configured IGs compile;
- generated resources byte-identical or every diff classified;
- diagnostics match or every diff classified;
- performance metrics collected.

Exit criteria:

- zero unclassified output diffs;
- zero unclassified diagnostic diffs;
- IPS <= 2.5s local indexed median, or a profile-backed explanation of the
  remaining gap.

### Phase 9: Optimization Loop

Goal: only now chase local hotspots.

Possible work:

- arena allocation tuning;
- faster hash maps (`hashbrown`, `rustc_hash`, `ahash`) after determinism review;
- schema-generated JSON emitters;
- parallel parse/export phases;
- zero-copy package JSON access;
- SIMD/string scanner improvements;
- allocator profiling.

Exit criteria:

- each optimization has a before/after benchmark;
- each optimization preserves corpus parity;
- regressions are caught automatically.

## Closed-Loop Agent Workflow

Agents working on this port should follow this loop:

1. State the exact behavior/perf hypothesis.
2. Pick the smallest corpus slice that can prove or disprove it.
3. Run stock TS SUSHI to refresh the oracle if needed.
4. Implement the Rust change.
5. Run focused unit fixtures.
6. Run resource byte diff against stock for the affected corpus slice.
7. Run diagnostic diff.
8. Run timing with phase metrics.
9. If output differs, classify the diff before optimizing further.
10. If performance does not improve, revert or mark the experiment rejected.
11. Update the design/perf note with evidence.

Agents must not:

- skip instances for a passing result;
- skip diagnostics for a passing result;
- silently normalize output diffs;
- accept unordered map output;
- optimize before knowing the global data shape involved;
- keep a fallback path without metrics and a test proving it is unused or
  acceptable.

Recommended per-change command shape:

```sh
# focused fixtures
cargo test -p rust_sushi --test <fixture>

# corpus slice
cargo run -p rust_sushi -- compile --project temp/ips-ig --out temp/rust-sushi-ips

# resource parity
diff -rq temp/sushi-ips-stock/fsh-generated/resources \
         temp/rust-sushi-ips/fsh-generated/resources

# diagnostic parity
bun site-gen/publisher/compare-sushi-diagnostics.ts \
  temp/sushi-ips-stock/sushi-diagnostics.json \
  temp/rust-sushi-ips/sushi-diagnostics.json

# timing
cargo run --release -p rust_sushi -- compile \
  --project temp/ips-ig \
  --out temp/rust-sushi-ips \
  --metrics temp/site-gen/rust-sushi-ips.metrics.json
```

## Open Design Questions

1. Parser strategy:
   - Port the grammar from SUSHI/ANTLR behavior exactly?
   - Use a Rust parser generator?
   - Hand-write a parser for better diagnostics/perf?

2. JSON model:
   - Fully typed FHIR resources for all emitted resource types?
   - Hybrid typed core plus dynamic extension/value[x] nodes?
   - Generated structs from FHIR definitions?

3. Differential generation:
   - Reproduce SUSHI's current diff algorithm directly?
   - Compute from overlay dirty sets and then compatibility-adjust?

4. Diagnostic compatibility:
   - Match message text byte-for-byte?
   - Or classify stable diagnostic codes first, then text?

5. Package facts:
   - Store compact JSON blobs in SQLite?
   - Store offsets into package tarballs/cache files?
   - Store pre-parsed binary facts for the subset SUSHI needs?

6. Parallelism:
   - Keep first production Rust compiler single-threaded for simpler parity?
   - Add deterministic parallel parse/index early?

## Risks

### Compatibility Drift

The main risk is a "better" compiler that does not behave like SUSHI. The
countermeasure is differential testing against stock TS SUSHI from the first
week.

### Scope Creep

FHIR computation is deep. The port should implement only what SUSHI needs to
compile the corpus, then broaden. It should not become a general FHIR validator
or terminology engine.

### Over-Indexing

Indexes can become complex and stale. Each index needs:

- an owner;
- a rebuild/update rule;
- invariant checks in debug builds;
- metrics proving it matters.

### Byte-Order Surprises

JSON ordering is observable. Emitters must be designed, not left to map order.

### Mutable Coordinates

`current`/`dev` are real workflows. The Rust tool must support them, but it must
label the guarantee level and write enough manifest evidence to explain why a
run is or is not reproducible.

## Near-Term Recommendation

Do not start by translating SUSHI file by file. Start with the global
architecture:

1. Build the parity/perf harness.
2. Build package store and JSON emit foundations.
3. Build parser plus AST/source spans.
4. Build insert-rule and tank indexes.
5. Build StructureDefinition arena/overlay.

Only after those are in place should local optimization loops begin.

The TypeScript spike already proved the core hypothesis: SUSHI gets fast when
its repeated scans and generic dynamic setters become explicit indexes and
plans. Rust should make those explicit structures the default design, which is
why an IPS target around 1.5-2.5s is plausible without sacrificing full output
or QA.

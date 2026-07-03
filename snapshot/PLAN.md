# Pure Snapshot Generator — Plan

Goal: a Rust implementation of FHIR's **Layer-A** StructureDefinition snapshot algorithm
(`ProfileUtilities.generateSnapshot`) that is **structurally identical** to the Java FHIR-core
output, built with the same oracle-driven closed loop we used to match SUSHI. **Layer B (version
pinning, narrative, validation) is explicitly out of scope** — see `snapshot-fodder/SPECIFICATION.md`
Part 0: Layer A is policy-free and deterministic, a pure function of *(base, derived, knobs, context)*.

## Why this is tractable
- The algorithm is fully specified, pass-by-pass, in `snapshot-fodder/SPECIFICATION.md` (Part III)
  with `file:line` provenance into the actual Java engine; `NON-OBVIOUS-BEHAVIORS.md` maps the traps.
- The oracle is the real engine, callable directly (proven this spike): a Java driver
  (`snapshot/oracle/SnapOracle.java`) builds a `SimpleWorkerContext` from the package cache and calls
  `ProfileUtilities.generateSnapshot(base, derived, ...)` → emits `derived.snapshot`. The wrapper defaults
  to an explicit `sortDifferential` input-normalization step before the pure call; `--direct` preserves raw
  Java behavior. No IG Publisher, so **no Layer-B pinning** — exactly the pure target.
- Element representation is our own: `serde_json::Value` (preserve_order) as the substrate, with
  helper accessors in `merge.rs`/`walk/`. We deliberately do NOT reuse the sushi-rs `fhir_model`
  typed model — the walk engine mirrors Java's `ElementDefinition` field-by-field over ordered JSON,
  and a typed layer would only get in the way of byte-parity with the oracle. (A typed layer remains a
  possible later optimization, not part of this rework — see REWORK-PLAN.md §2.)

## Methodology (mirrors the SUSHI port)
oracle → fixtures (simple→complex) → committed goldens → Rust generator → **structural-diff gate**
→ iterate per spec pass. Diff is SEMANTIC on `snapshot.element[]` (order significant; per-element
field-by-field), with a normalizer for known-irrelevant noise (e.g. `text`, generated ids) classified
explicitly — never silently.

## Phase 0 — Oracle harness (FIRST)
1. Fix the oracle context load: create the builder with `withAllowLoadingDuplicates(true)` before
   `fromPackage(...)` (FHIR-core's r5.core ships a duplicate `spdx-license` CodeSystem; setting the flag
   on `ctx` after `fromPackage` is too late). Then load core + any dep packages (multi-package context).
2. Wrap the driver in `snapshot/oracle/gen-snapshot.sh <input.json> <out.json> [pkg#ver ...]` using the
   ISOLATED cache (`temp/fhir-home/.fhir/packages`, never real ~/.fhir). Default mode runs
   `sortDifferential`; `--direct` skips it.
3. `snapshot/gen-goldens.sh`: for every fixture, produce `goldens/<name>.snapshot.json` plus
   `goldens/<name>.snapshot.messages.json`.
4. Rust side: `cargo run -p snapshot_gen -- <input.json>` prints the generated snapshot; the Rust
   integration test now compares the full `snapshot.element[]` array semantically against each golden
   (order significant), plus normalized differential order. Version target: match the **Publisher
   native internal path**. R4 profiles are parsed/converted into the R5 model path for generation;
   R4 artifact downconversion is separate from the Layer-A target.
5. Build a fixture ladder (each isolates one spec pass):
   - cardinality/flags only (no structure change) — `r5-patient-min` exists.
   - type constraint / `[x]` slicing; profiled type (`type.profile`).
   - `contains`/slicing (discriminator, ordered, open/closed) + reslicing.
   - contentReference (recursive structures, e.g. Questionnaire.item).
   - extension elements; complex-type expansion (unfold).
   - real profiles pulled from r5.core + a couple of published IGs (strip `snapshot`, regenerate).

## Phases 1..N — implement Layer A pass by pass (spec Part III)
Build `crates/snapshot_gen` incrementally, each pass gated against goldens:
1. **Clone-and-overlay core** (§1.2): deep-copy base snapshot; lockstep walk base×differential by path;
   `updateFromDefinition` per-element merge (`PU:~2585`). Smallest fixtures first.
   Keep R4/R5 fixture coverage in the loop, but avoid preserving two production
   implementations. The one right path is Publisher native internal generation;
   any R4 output shape is a separate projection/downconversion step.
2. **Path processing / recursion** (`ProfilePathProcessor.processPaths`, `PPP`): simple-path vs
   sliced-base branches, child maps, limits, contextPath.
3. **Slicing**: `slicingMatches`, type-slice groups (`BaseTypeSlice`/`TypeSlice`), `checkToSeeIfSlicingExists`,
   `[x]` handling, reslicing; `SnapshotGenerationPreProcessor` (push trailing slice-group props down).
4. **Type expansion / unfold** + **contentReference** redirection (`ElementRedirection`).
5. **MappingAssistant** merge; the `userData` side-channel state (UserDataNames) — easy to miss.
6. The structural self-validation Layer A does on its own output (§3.6).

## Gates / invariants
- Per-pass: all current fixtures' Rust snapshot == oracle golden (semantic). Completeness invariant
  (every base element represented; every differential element consumed exactly once — spec §1.1/PC-1).
- No silent normalization. Classify any residual diff with a spec citation + a plan to close it.
- Determinism: same inputs → same output.

## Deliverables
- `snapshot/oracle/` (Java driver + scripts) — the pure oracle.
- `snapshot/fixtures/` + `snapshot/goldens/` — the ladder + pre-computed pure snapshots.
- `crates/snapshot_gen/` — the Rust Layer-A generator (own model over `serde_json::Value`).
- `snapshot/AGENTS.md` — operating manual (oracle commands, version target, parity-trap log), like the SUSHI port.

## Open decisions / cleanup
- Transitional CLI/oracle modes are still useful for fixture comparison, but the
  final Rust path should collapse onto Publisher native internal generation.
  Keep `--output-r4` only if we intentionally support downconverted artifact
  output as a separate step.
- Multi-package context shape for fixtures needing deps (extensions, terminology).
- Diff normalizer scope (what counts as "irrelevant" — keep it minimal and explicit). Current strict
   full-snapshot diff should stay strict; any ignored field must be named here with a source-backed
   reason.

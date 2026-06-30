# Pure Snapshot Generator — Plan

Goal: a Rust implementation of FHIR's **Layer-A** StructureDefinition snapshot algorithm
(`ProfileUtilities.generateSnapshot`) that is **structurally identical** to the Java FHIR-core
output, built with the same oracle-driven closed loop we used to match SUSHI. **Layer B (version
pinning, narrative, validation) is explicitly out of scope** — see `snapshot-fodder/SPECIFICATION.md`
Part 0: Layer A is policy-free and deterministic, a pure function of *(base, derived, knobs, context)*.

## Why this is tractable
- The algorithm is fully specified, pass-by-pass, in `snapshot-fodder/SPECIFICATION.md` (Part III)
  with `file:line` provenance into the actual Java engine; `NON-OBVIOUS-BEHAVIORS.md` maps the traps.
- The oracle is the real engine, callable directly (proven this spike): a ~25-line Java driver
  (`snapshot/oracle/SnapOracle.java`) builds a `SimpleWorkerContext` from the package cache and calls
  `ProfileUtilities.generateSnapshot(base, derived, ...)` → emits `derived.snapshot`. No IG Publisher,
  so **no Layer-B pinning** — exactly the pure target.
- We reuse the sushi-rs `fhir_model` crate (`StructureDefinition`/`ElementDefinition`, ordered JSON
  element maps, id→path, slicing) — the snapshot generator is a new crate on top.

## Methodology (mirrors the SUSHI port)
oracle → fixtures (simple→complex) → committed goldens → Rust generator → **structural-diff gate**
→ iterate per spec pass. Diff is SEMANTIC on `snapshot.element[]` (order significant; per-element
field-by-field), with a normalizer for known-irrelevant noise (e.g. `text`, generated ids) classified
explicitly — never silently.

## Phase 0 — Oracle harness (FIRST)
1. Fix the oracle context load: call `context.setAllowLoadingDuplicates(true)` before loading packages
   (FHIR-core's r5.core ships a duplicate `spdx-license` CodeSystem; the validator sets this flag too).
   Then load core + any dep packages (multi-package context).
2. Wrap the driver in `snapshot/oracle/gen-snapshot.sh <input.json> <out.json> [pkg#ver ...]` using the
   ISOLATED cache (`temp/fhir-home/.fhir/packages`, never real ~/.fhir).
3. `snapshot/gen-goldens.sh`: for every fixture, produce `goldens/<name>.snap.json` (the pure snapshot).
4. Rust side: `cargo run -p snapshot_gen -- <input.json>` prints the generated snapshot; a Rust
   integration test compares to the golden (semantic element diff). Decide FHIR version target:
   **start R5** (matches the fodder exactly, no R4↔R5 conversion); add R4 later via the publisher's
   conversion path.
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
- `crates/snapshot_gen/` — the Rust Layer-A generator (reuses `fhir_model`).
- `snapshot/AGENTS.md` — operating manual (oracle commands, version target, parity-trap log), like the SUSHI port.

## Open decisions (resolve in Phase 0)
- Version target order (R5 first vs R4-with-conversion). Recommend **R5 first**.
- Multi-package context shape for fixtures needing deps (extensions, terminology).
- Diff normalizer scope (what counts as "irrelevant" — keep it minimal and explicit).

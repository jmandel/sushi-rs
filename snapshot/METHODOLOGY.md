# Methodology — Pure Snapshot Generator (oracle-driven, like the SUSHI port)

This explains *how* we build the Rust Layer-A snapshot generator. It is the same closed loop that
took the Rust SUSHI port to 665/665 byte-identical output: **make the reference implementation the
oracle, gate every step against it, trust nothing, classify every diff.**

## 1. The core idea: the Java engine is the oracle
We are not re-deriving FHIR's snapshot rules from prose — we are **matching a specific implementation's
behavior**: `org.hl7.fhir.r5.conformance.profile.ProfileUtilities.generateSnapshot`. That function is
the de-facto normative algorithm and it is **callable in isolation**, which is what makes this tractable.

- `snapshot/oracle/SnapOracle.java` builds a `SimpleWorkerContext` from the package cache and calls
  `generateSnapshot(base, derived, …)` directly. The shell wrapper defaults to an explicit
  `sortDifferential` input-normalization step before that call; `--direct` skips it for raw Java behavior.
  Because we bypass the IG Publisher, we get **Layer A only**: no version pinning, no narrative, no
  validation edits (`SPECIFICATION.md` Part 0). The emitted `derived.snapshot` is the *pure* target.
- Purity is structural, not magic: Layer A is a pure function of *(base, derived, knobs, context)*. We
  pin the context (fixed package set, isolated cache) so the same fixture always yields the same golden.

## 2. The closed loop (per change)
1. **Pick the smallest fixture** that exercises the one behavior under test (one spec pass).
2. **Pre-compute its pure snapshot** with the oracle → commit as a golden
   (`snapshot/goldens/<name>.snapshot.json`) plus captured messages.
3. **Implement / extend** the Rust generator (`crates/snapshot_gen`).
4. **Gate**: Rust snapshot vs golden, **semantic element diff** — `snapshot.element[]` compared in order,
   field-by-field. Pass = identical.
5. **Classify any diff** against the spec (`SPECIFICATION.md` Part III + `NON-OBVIOUS-BEHAVIORS.md`
   `file:line`), fix, repeat. **No silent normalization** — if we ever ignore a field, it's named and justified.
6. Commit the working increment.

Same discipline as the SUSHI port: every claim is verified against the oracle on real inputs, not asserted.

## 3. Fixture ladder (isolate one pass at a time)
Each fixture targets exactly one mechanism so a failure points at one pass:
cardinality/flags → type/`[x]` slicing → profiled types → `contains`/slicing+reslicing → contentReference
→ unfold/complex-type expansion → mappings → real profiles (strip `snapshot`, regenerate, diff). The
target is the **Publisher native internal path**: R4 inputs are converted into the R5 model for snapshot
generation, and any R4 artifact shape is a separate downconversion/projection concern.

## 4. Layering discipline (what we do and don't build)
- **In scope: Layer A** — the structural merge (inheritance, slicing, type expansion, contentReference).
  Deterministic, policy-free.
- **Out of scope: Layer B** — version pinning (`url|x.y.z`), narrative, validation. These are *post-passes*
  the publisher runs over the finished snapshot; our oracle deliberately doesn't run them, and neither do we.
  This keeps the target clean and the generator a reusable pure function.

## 5. Element model — own substrate, plus decision traces
The generator uses its **own** model over `serde_json::Value` (preserve_order), not the sushi-rs
`fhir_model` typed model: the walk engine mirrors Java's `ElementDefinition` field-by-field over
ordered JSON, which is what buys byte-parity with the oracle (REWORK-PLAN §2).

The decisive anti-overfitting gate is **decision traces**, not just output diffs. The fhir-core
checkout is instrumented (branch `snap-trace`) to emit one JSONL record per `processPaths` step
(`{diffId|baseId, branch, cloneSource, slicing decision, consumed diff ids}`); the Rust walk emits the
same records and `snapshot/diff-trace.cjs` compares them. Two engines can agree on outputs while
disagreeing on decisions — trace parity on the fixture ladder + real IGs is what proves the walk
matches Java's *decisions*, which is the whole point of the rework. See REWORK-PLAN.md §4.

## 6. Provenance of the spec
`snapshot-fodder/` is an extract of the FHIR-core R5 snapshot engine (source files under `files/`, plus
`00-ALL-IN-ONE.txt`), reverse-engineered into `SPECIFICATION.md` (normative, pass-by-pass, with `PU:`/`PPP:`
line anchors) and `NON-OBVIOUS-BEHAVIORS.md` (the load-bearing helpers + `userData` side-channel traps).
When the spec and our output disagree, the **oracle** decides; the spec tells us *why* and *where to look*.

## 7. Status / next
- **Cutover complete (2026-07-02, REWORK-PLAN wave 4).** The decision-isomorphic
  walk engine (`crates/snapshot_gen/src/walk/`) is the **only** engine — the legacy
  diff-order patch interpreter, its quirk registry, and all transitional CLI/output
  flags are deleted. `generate_snapshot` IS the walk. The engine reaches full-corpus
  parity with the pinned oracle (~955 profiles across 34 harvested IGs + the 17-rung
  fixture ladder) with an **EMPTY quirk registry** — every behavior traces to a
  Java branch or a Java-cited policy-list constant (`walk/consts.rs`) /
  load-time fixup (`walk/resolve.rs fix_loaded_resource`, PackageHackerR5). The
  per-IG scorecard lives in REWORK-PLAN.md §9; the per-increment derivation log is
  `snapshot/specs/walk-worklog.md`.
- **Next** (post-rework roadmap, REWORK-PLAN §7 "After the rework"): merge
  origin/main into snapshot-gen; a perf + clarity/simplification pass over both
  sushi and the generator; then the WASM editor demo; then Layer B as a separate
  opt-in overlay. See REWORK-PLAN.md.

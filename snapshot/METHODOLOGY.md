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

## 5. Why reuse `fhir_model`
The snapshot algorithm operates on the same `StructureDefinition`/`ElementDefinition` model the SUSHI
port already implements (ordered element maps, id→path derivation, slice handling, find-by-path). The
generator is a new crate (`snapshot_gen`) layered on `fhir_model`, not a from-scratch model.

## 6. Provenance of the spec
`snapshot-fodder/` is an extract of the FHIR-core R5 snapshot engine (source files under `files/`, plus
`00-ALL-IN-ONE.txt`), reverse-engineered into `SPECIFICATION.md` (normative, pass-by-pass, with `PU:`/`PPP:`
line anchors) and `NON-OBVIOUS-BEHAVIORS.md` (the load-bearing helpers + `userData` side-channel traps).
When the spec and our output disagree, the **oracle** decides; the spec tells us *why* and *where to look*.

## 7. Status / next
- Done: worktree, oracle driver compiles + runs, R5/R4 Publisher-path contexts load from the isolated
  cache, goldens are generated, and `crates/snapshot_gen` gates strict semantic `snapshot.element[]`
  parity for the fixture ladder plus harvested real R4 profiles: IPS 29/29, mCODE 46/46,
  CRD 22/22, and SDC 73/73.
- Next: keep broadening real profile coverage and close remaining Java pass structure gaps
  (richer slicing/reslicing, type-slice groups, mapping/userData side-channel behavior, and structural
  self-validation). Transitional output flags should be cleaned up once the migration no longer needs
  old R4 fixture comparisons. See `PLAN.md` for phases.

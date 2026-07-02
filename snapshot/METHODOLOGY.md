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
  CRD 22/22, SDC 73/73, plus holdout IGs CARIN BB 6/6, Genomics 33/33, DTR 21/21,
  eCR 28/28, NDH 50/50, and PAS 73/73. Published-package harvesting also gates
  PDDI 1/1, MHD 42/42, DeID 1/1, EU EPS 23/23, EU MPD 4/4,
  AU Core 2.0.0 26/26, AU PS 17/17, gematik ePA medication 49/49 usable profiles,
  Belgium Vaccination 7/7, and PACIO TOC 4/4, DARTS 1/1, DAPL 26/26,
  Radiation Dose Summary 4/4,
  Taiwan PAS 43/43, US Core 9.0.0 70/70, IPA 1.1.0 12/12,
  QI-Core 6.0.0 63/63 usable profiles, SMART App Launch 2.2.0 6/6,
  Da Vinci CDex 2.1.0 8/8, Da Vinci PDex 2.1.0 37/37,
  Da Vinci Plan Net 1.2.0 22/22, Da Vinci Drug Formulary 2.1.0
  19/19 usable profiles, Da Vinci PAS 2.2.1 80/80 usable profiles, plus
  Subscriptions Backport R4 1.1.0 9/9 usable profiles. SAFR, Bulk Data 2.0.0,
  CDS Hooks 3.0.0-ballot, Application Feature,
  IHE VHL, C-CDA R5.0, C-CDA on FHIR, PH Query, and Order Catalog harvested 0
  usable R4 constraint StructureDefinitions through the published-package path.
  The 2026-07-02 TWPAS fix pass reached Taiwan PAS 43/43; after the later
  Coding-anchor refinement, the affected TWPAS coding profiles were rerun
  20/20 plus focused Patient/Practitioner checks. The same pass was regression
  checked against AU Core 26/26, AU PS 17/17, MHD 42/42, and EU EPS 23/23.
  The official Da Vinci PAS pass added batch Java/Rust harness paths, an
  empty-`.index.json` package scan fallback needed by subscriptions-backport.r4,
  and tightened direct-slice child unfolding; regression checks stayed green for
  PDDI 1/1, TWPAS 43/43, AU Core 26/26, AU PS 17/17, MHD 42/42, EU EPS 23/23,
  and PAS 80/80.
  Genomics, eCR, NDH, and the published-package goldens were checked against the
  current Publisher-path oracle after extension-root condition/doco drift and R4/R5
  dependency-closure ambiguity were found. The later 2026-07-02 native-extension
  and direct-slice/MS pass rechecked IPS 29/29, mCODE 46/46, CARIN BB 6/6,
  Genomics 33/33, eCR 28/28, NDH 50/50, PAS 73/73, Taiwan PAS 43/43, US Core
  70/70, QI-Core 63/63, CRD 22/22, SDC 73/73, DTR 21/21, AU Core 26/26, and
  the package-harvested sweep after tightening fhirQueryPattern,
  variable-extension, direct Coding/Identifier slice materialization, and
  direct-slice MS propagation behavior.
- Next: keep broadening real profile coverage and close remaining Java pass structure gaps
  (richer slicing/reslicing, type-slice groups, mapping/userData side-channel behavior, and structural
  self-validation). Transitional output flags should be cleaned up once the migration no longer needs
  old R4 fixture comparisons. See `PLAN.md` for phases.

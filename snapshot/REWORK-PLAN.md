# Snapshot Generator Rework — Decision-Isomorphic Walk Engine

> **Read this first.** This is the shared plan for the ground-up rework of
> `crates/snapshot_gen`. Every subagent working on the rework reads this file
> before touching anything. It records the architecture decisions, the stage
> gates, and the hard rules. Update it when facts change.

## 0. Why we are reworking

The current generator (now `legacy`) matches the Java oracle on ~1,900 golden
profiles across ~40 IGs, but it is a **diff-order patch interpreter** over a
flat element list, while the oracle (`org.hl7.fhir.r5.conformance.profile.*`)
is a **forward recursive lockstep walk**. Every architectural mismatch became a
corpus-fitted heuristic: 5 mustSupport add/remove blocks, `should_prune_*` /
`should_materialize_*` predicate piles, global post-passes that literally stamp
eCR slice names (`checkSuspectedDisorder`) into every run. That approach fits
outputs, not decisions, and does not converge.

The rework matches Java's **decisions** with a decision-isomorphic engine on a
clean substrate. The legacy engine stays runnable during migration for A/B.

## 1. The oracle (pinned)

- Java engine: `org.hl7.fhir.r5.conformance.profile.ProfileUtilities.generateSnapshot`
  via `ProfilePathProcessor` (the Publisher path, `setNewSlicingProcessing(true)`).
- Source: `/home/jmandel/hobby/fhir-perf/repos/fhir-core`, **commit `5c4d5a0ff`**
  (2026-06-10), jar `org.hl7.fhir.r5-6.9.10-SNAPSHOT.jar`. Changing the oracle
  version is a deliberate re-pin event: record here + regenerate goldens.
- Key Java files (all in `org.hl7.fhir.r5/src/main/java/org/hl7/fhir/r5/`):
  - `conformance/profile/ProfileUtilities.java` (~5054 lines) — generateSnapshot
    orchestration, updateFromDefinition per-element merge, updateURLs, unfolding.
  - `conformance/profile/ProfilePathProcessor.java` (~1739) — the recursive walk.
  - `conformance/profile/SnapshotGenerationPreProcessor.java` (~1227) — differential
    pre-normalization.
  - `conformance/profile/MappingAssistant.java`, `BaseTypeSlice.java`, `TypeSlice.java`,
    `ProfilePathProcessorState.java`.
- Reverse-engineered spec: `snapshot-fodder/SPECIFICATION.md` (+
  `NON-OBVIOUS-BEHAVIORS.md`), with `PU:`/`PPP:` line anchors. When spec and
  oracle disagree, **the oracle wins**; the spec tells you where to look.
- Oracle driver: `snapshot/oracle/SnapOracleR4.java` + `gen-snapshot.sh`
  (parses R4 JSON → converts to R5 model → sortDifferential → generateSnapshot →
  emits native-R5 internal JSON by default).

## 2. Target architecture (the pipeline)

Mirror the Publisher's actual dataflow as explicit, separately-gated stages:

```
 stage 1: LOAD      PackageContext: cache + local dirs -> R4/R5 JSON (memoized)
 stage 2: CONVERT   R4 SD JSON -> R5-internal-model JSON  (port of VersionConvertor_40_50
                    behavior for StructureDefinition; includes xpath->extension,
                    empty-string primitive drops, additionalBinding, etc.)
                    R5 inputs pass through unchanged.
 stage 3: PREPROCESS SnapshotGenerationPreProcessor equivalent + sortDifferential
                    (existing sort_differential_by_base grows into this).
 stage 4: WALK      decision-isomorphic ProfilePathProcessor port. Recursive,
                    forward-only, diff-cursor + base-window. Consumes stage-2/3
                    output. Emits snapshot elements in walk order + messages.
 stage 5: FINALIZE  generateSnapshot pre/post steps outside the walk
                    (updateURLs/markdown, mappings via MappingAssistant order,
                    extension doco checks, obligation snapshot-source, sorting checks).
 stage 6: PROJECT   optional R5 -> R4 artifact downconversion (separate command;
                    NOT part of the core path).
```

Decisions already made — do not relitigate without updating this doc:

- **Everything is R5 internally.** Dependencies, local profiles, generated-on-
  demand base snapshots: converted at load (stage 2), snapshot-generated in R5,
  never round-tripped mid-pipeline. This kills the legacy `structure_with_r4_snapshot`
  R4-form recursion and relocates the scattered "native R5 projection" special
  cases into stage 2 where the Publisher actually does them.
- **`userData` side-channels become explicit sidecar annotations** (one
  `Annotations` struct keyed by element index/id), never extra JSON keys.
- **State goes in one `WalkContext`**, not parallel HashSets threaded through
  10-arg functions.
- **Unconsumed differential rows are an error** (collected into messages, gate-
  checked) — never silently dropped. Port Java's message wording where gated.
- **Quirk policy:** any behavior that survives root-cause analysis but has no
  clean home lives in `quirks.rs` as a registry entry with (a) the fixture that
  demands it and (b) a Java `file:line` citation. A quirk without a Java citation
  is a hole in our understanding and is tracked as debt in this file. Hardcoded
  profile URLs / slice names in engine code are FORBIDDEN outside `quirks.rs`.
- Element representation stays `serde_json::Value` (preserve_order) for now, with
  helper accessors; a typed layer is a later optimization, not part of this rework.

## 3. Engines during migration

- `--engine legacy` (current code, default until parity) and `--engine walk`.
- `check-harvested-r4.sh` accepts `ENGINE=walk` to gate the new engine against
  the same goldens. CI-of-record = full corpus on legacy until walk reaches
  parity, then flip default, then delete legacy (task 9).

## 4. Decision traces (the anti-overfitting gate)

The fhir-core checkout is patched (branch `snap-trace` off `5c4d5a0ff`) to emit
a per-element decision trace as JSONL when `-Dsnapshot.trace=<file>` is set:
one record per processPaths step: `{diffId|baseId, branch, cloneSource, slicing
decision, consumed diff ids}` (exact schema in `snapshot/specs/trace-schema.md`).
`gen-snapshot.sh TRACE=1` writes `<out>.trace.jsonl`. The Rust walk emits the
same records; `snapshot/diff-trace.cjs` compares. Trace parity is gated on the
fixture ladder + at least one full IG; output parity alone is NOT sufficient to
call a behavior done, because two engines can agree on outputs while disagreeing
on decisions — that is exactly the failure mode this rework exists to eliminate.

## 5. Gates (run these; do not weaken them)

```sh
cargo test -p snapshot_gen                        # fixture ladder + units
bash snapshot/check-harvested-r4.sh <dir> <pkgs>  # per-IG golden gate (RUST_BATCH=1)
node snapshot/diff-snapshot.cjs <exp> <act>       # semantic element diff
```
Per-IG package lists: see snapshot/AGENTS.md "Rust Commands". Full corpus =
every `snapshot/harvested/r4/*/manifest.json` directory.

## 6. Hard rules (inherited from the port + new)

- NEVER touch real `~/.fhir` or `$HOME`. Isolated cache only
  (`temp/fhir-home/.fhir/packages`, guarded by `harness/_guard.sh`).
- No silent normalization; classify every diff. No skipping fixtures to make a
  gate pass. Never edit goldens to match Rust output — goldens only change when
  the ORACLE's output changes (re-pin event).
- Subagents never invent FHIR behavior: every behavioral claim cites Java
  `file:line` (fhir-core pinned commit) or an oracle experiment they ran.
- Do not modify `sushi-ts/`, `snapshot-fodder/` (read-only reference material).
- fhir-core changes ONLY on the `snap-trace` branch; never commit to its master.
- Keep the legacy engine byte-identical while it is the default (refactors must
  keep all gates green).

## 7. Execution roadmap (waves, owners, gates)

Coordination model: one coordinator (main session) + Opus subagents per work
package. The coordinator writes specs into agent prompts, reviews every
deliverable against the gates PERSONALLY (never trusts an agent's own report),
and is the only one who commits to the snapshot repo. Corpus gates are the
arbiter for everything.

### Wave 1 — foundations (parallel, no file overlap)
| # | Work package | Deliverables | Gate to accept |
|---|---|---|---|
| W1a | **fhir-core trace instrumentation** (fhir-core branch `snap-trace`) | SnapshotTracer + hooks in ProfilePathProcessor/ProfileUtilities/PreProcessor; rebuilt r5 jar; `TRACE=1` in gen-snapshot.sh; `snapshot/specs/trace-schema.md`; `snapshot/diff-trace.cjs` | Oracle output byte-identical with/without tracing; valid JSONL on an IPS fixture; every branch label cites Java file:line |
| W1b | **Conversion-stage oracle + spec** | `--dump-converted` oracle mode; `snapshot/converted-goldens/` for IPS + 10 hard fixtures; `snapshot/specs/r4-to-r5-conversion.md` | 3 fixtures hand-verified: every R4→converted diff class explained by the spec with Java citations |
| W1c | **Legacy modularization** (crate) | lib.rs → cli / package (+memoized fetch) / merge / projection / legacy/{engine,quirks}; `--engine` flag plumbing; `ENGINE=` in check-harvested-r4.sh | cargo test green + ips 29/29, us-core 70/70, qicore 63/63, sdc 73/73, ecr 28/28 byte-identical |
| W1d | **Walk decision-tree spec** | `snapshot/specs/walk-decision-tree.md`: orchestration, state, full branch tree w/ clone-timing, updateFromDefinition map to existing leaf fns, userData inventory, message points, Rust skeleton, ambiguity/experiment list, trace points | Every claim carries pinned-commit file:line; dead-vs-live branches marked for the oracle's exact configuration |

### Wave 2 — build the new engine (starts as W1 lands)
| # | Work package | Depends on | Gate |
|---|---|---|---|
| W2a | Rust `convert.rs` (stage 2) | W1b, W1c | Converted-goldens parity for all dumped fixtures; unit tests in crate |
| W2b | `walk/` engine core: preprocess + walk + finalize on R5 substrate, behind `--engine walk`, emitting Rust-side decision traces | W1c, W1d (W1a for traces) | Fixture-ladder goldens + **IPS 29/29** on `ENGINE=walk`; trace parity vs Java on fixture ladder |
| W2c | Trace comparison loop wired into dev workflow | W1a, W2b | `diff-trace.cjs` clean on fixture ladder |

### Wave 3 — corpus scale-out (iterative, one IG batch per agent run)
Order: mCODE → CRD → SDC → CARIN → DTR → eCR → NDH → PAS → Genomics, then the
published-package sweep (US Core, QI-Core, AU/EU/TW, Da Vinci set, …).
Protocol per IG: run `ENGINE=walk` gate → for each failing profile, diff traces
first, outputs second → root-cause in Java → fix in walk (or register a cited
quirk) → re-run THIS IG + the previously-green set (no-regression rule).
Gate: same ok=N/N numbers as legacy for every corpus dir.

### Wave 4 — cutover & cleanup
1. Default engine := walk; full-corpus final gate (every manifest.json dir).
2. Quirk audit: `quirks.rs` entries each have fixture + Java citation; count
   reported in this file. Legacy engine + transitional flags DELETED (incl.
   `--native-r5` transitional modes per snapshot/AGENTS.md scope note).
3. Docs: METHODOLOGY.md/PLAN.md/AGENTS.md rewritten around the pipeline;
   PLAN.md's stale "reuses fhir_model" claim replaced by the actual decision.

### After the rework (user-set roadmap, 2026-07-02; tasks #11–#13)
1. **Merge latest origin/main into snapshot-gen** (re-fetch first — network
   was down when scheduled); merge gate = full-corpus walk scorecard + tests.
2. **Perf + clarity review pass across sushi AND snapshot generator** —
   profile the walk engine on the biggest corpora (levers: cross-item
   generated-snapshot memoization, id→index side maps); re-run the sushi
   31-IG perf harness; then a clarity/simplification review with gates green
   at every step.
3. **Only after perf is covered: the WASM demo** (docs/wasm-editor-plan.md
   P0). Perf pass moved OUT of wave 4 into this roadmap step.
4. **After the editor demo (task #17): Layer B as a separate OPT-IN stage** —
   the Publisher post-passes over finished snapshots (canonical version
   pinning `url|x.y.z` first; narrative/validation-edit scope decided by an
   audit pass). This deliberately stays OUT of the Layer-A engine: Layer A
   remains the pure, policy-free function; Layer B is a composable overlay.
   Oracle shift: bare `generateSnapshot` cannot see these passes — the
   harness must drive the Publisher's wrapper path (own pin, own goldens).
   Layer B is known quirk-dense: same registry discipline from day one
   (every behavior cited to Publisher code + a fixture; undocumented
   behavior is debt, never silent).

### Non-negotiable protocol rules
- A wave item is DONE only when the coordinator has re-run its gates personally.
- Goldens never change to make Rust pass; only a deliberate oracle re-pin
  regenerates them.
- Any agent-discovered behavior lands as (a) walk code matching a Java branch,
  or (b) a cited quirks.rs entry, or (c) an open question in §8 — never as an
  uncited heuristic.
- Previously-green corpora are re-gated after every walk change (regression set
  grows as IGs turn green).

## 8. Open questions / debt (keep current)

- **Walk quirk registry: EMPTY at cutover (2026-07-02).** The walk engine reached
  full-corpus parity (~955 profiles across 34 IGs + the 17-rung ladder) with NO
  `quirks.rs` (the file is deleted). Grep audit of `crates/snapshot_gen/src`
  confirms zero IG-specific profile URLs / slice names in engine code: the only
  policy-list constants are the Java-cited `walk/consts.rs`
  (NON_INHERITED/DEFAULT_INHERITED/OVERRIDING/NON_OVERRIDING_ED_URLS, verbatim from
  ProfileUtilities) and the url-keyed core fixups in `walk/resolve.rs`
  `fix_loaded_resource` (PackageHackerR5.java:14-135, each gated by exact
  Java-cited url + version or owning package id). Every §8 seed quirk below was
  re-derived to a Java branch during Wave 3 (see snapshot/specs/walk-worklog.md):
  the eCR PlanDefinition stamps became the cross-SD contentReference walk into
  core PlanDefinition.action (Increment 11); cqf-fhirQueryPattern /
  us-ph-named-eventtype doco became the unconditional Java-exact checkExtensionDoco
  (Increment 11); the extension-root condition/xpath lists became the
  updateFromDefinition profile-root-doco + local-snapshot generation paths; the
  NATIVE_R5_VARIABLE_COMMENT / fhirquerypattern doco stamps were shown to be STALE
  goldens (the fresh pinned oracle emits generic doco, which the walk matches).

- (seed — RESOLVED) Legacy hardcoded behaviors re-derived during Wave 3: eCR
  PlanDefinition stamps (`checkSuspectedDisorder`/`checkReportable`), cqf-
  fhirQueryPattern quirks, extension-root condition URL lists
  (mcode-histology-morphology-behavior, condition-related, alternate-reference,
  us-ph-named-eventtype-extension, ndh base-ext-org-alias-*), codeOptions /
  artifact-versionAlgorithm generic-doco list, NATIVE_R5_VARIABLE_COMMENT.
- (seed — RESOLVED, Increment 2) Package-loaded vs local-dir R4 resources DO take
  different conversion paths: local-dir + the input profile get the full
  VersionConvertor_40_50 conversion (`to_r5_internal`), while package-loaded R4
  resources are read R5-parser-lenient (R4-only props like `constraint.xpath`
  silently dropped) — mirroring SnapOracleR4's `loadLocalR4CanonicalResources` vs
  `SimpleWorkerContextBuilder.fromPackage(loader==null)`. See `walk/resolve.rs`.
  Caveat: packages with an empty `.index.json` (e.g. subscriptions-backport.r4)
  fall through the scan path, which the oracle treats as the full-conversion path;
  `package.rs` marks scan-loaded resources `local:true` to match.

## 9. Status log (update as tasks land)

- 2026-07-02: plan written; oracle pinned at `5c4d5a0ff` / 6.9.10-SNAPSHOT.
  Legacy engine at full-corpus parity (see snapshot/AGENTS.md numbers).
  Wave 1 launched (4 parallel agents): W1a trace, W1b conversion oracle,
  W1c modularization, W1d walk spec.
- 2026-07-02 (later): **Wave 1 + W2a landed** (commits 7c3230b..d7a24a1).
  - W1c modularization: gates byte-identical (ips/us-core/qicore/sdc/ecr).
  - W1b conversion oracle: `--dump-converted`, 39 goldens, spec.
  - W1d walk spec: 901 lines; anchor-drift caveat added (authored against
    tracer-instrumented tree; anchors ±10 lines, resolve on `snap-trace`).
  - W1a trace: fhir-core `snap-trace` @ `047763f89`, 51 branch labels,
    TRACE=1, trace-schema.md, diff-trace.cjs. Deployed r5+utilities jars in
    FHIR_CORE_REPO target/ are now the INSTRUMENTED build (verified: output
    byte-identical trace on/off AND batch output matches committed goldens).
  - W2a conversion impl: convert.rs 39/39 converted goldens (order-
    sensitive), 338-fixture smoke, `--dump-converted` CLI.
  - **GOTCHA fixed:** SnapOracleR4 single-run mode dropped the LAST package
    arg (off-by-one) since inception — single-mode runs silently missed e.g.
    extensions.r4 and produced false diffs vs goldens. All goldens were
    batch-generated (unaffected). Fixed; single mode now reproduces goldens.
    Rule stands: prefer batch mode for golden generation; single mode is fine
    for trace debugging now.

- **2026-07-02/03: Wave 3 + Wave 4 COMPLETE — walk engine is the only engine.**
  Wave 3 (batch 1 IGs + batch 2 published-package sweep) brought every corpus IG
  to legacy parity on the walk engine with an EMPTY quirk registry (see
  snapshot/specs/walk-worklog.md, Increments 2-12). Wave 4 cutover (Increment 13):
  - **Fix 1 (Java-exactness):** `PackageHackerR5.fixLoadedResource` extensions.r4
    R5-only-datatype removeIf re-scoped from "every R4-loaded SD" to the Java-exact
    owning-package gate `packageInfo.getId()=="hl7.fhir.uv.extensions.r4"`
    (PackageHackerR5.java:115). `package.rs` now records each resource's owning npm
    package id; `resolve.rs fix_loaded_resource(sd, package_id)` gates on it. A
    regression (davinci-pas/profile-subscription 80→79, from a side-effect flip of
    scan-fallback `local` flag) was caught by the corpus gate and fixed
    (scan resources stay `local:true`; only package_id newly recorded).
  - **Engine cutover:** `generate_snapshot` IS the walk. Deleted `src/legacy.rs`
    (3994 lines), `src/quirks.rs` (426), `src/projection.rs` (1008), the legacy
    gate `tests/snapshot_parity.rs` (120), the `Engine` enum + `--engine` flag +
    `ENGINE` env + `run_engine`, the transitional `--native-r5`/`--output-r5`
    flags and the `native_r5`/`apply_extension_root_doco` `SnapshotOptions` fields,
    and every lib.rs item only legacy used. `check-harvested-r4.sh` lost its
    `ENGINE`/`--native-r5` plumbing. Walk-used merge/text/convert/walk pieces stay.
  - **Quirk audit:** grep of `crates/snapshot_gen/src` = ZERO IG-specific profile
    URLs / slice names outside the Java-cited `walk/consts.rs` policy lists and
    `walk/resolve.rs` url-keyed PackageHackerR5 core fixups. **Walk quirk registry:
    EMPTY at cutover** (§8).

  **FINAL SCORECARD** (single-engine CLI: no `--engine`, no `ENGINE`, no
  `--native-r5`; every number equals the wave-3 legacy-parity counts):

  | IG | ok/total | | IG | ok/total |
  |---|---|---|---|---|
  | ips | 29/29 | | pddi | 1/1 |
  | mcode | 46/46 | | deid | 1/1 |
  | genomics | 33/33 | | darts | 1/1 |
  | crd | 22/22 | | radiation-dose-summary | 4/4 |
  | sdc | 73/73 | | be-vaccination | 7/7 |
  | carinbb | 6/6 | | smart-app-launch | 6/6 |
  | dtr | 21/21 | | cdex | 8/8 |
  | ecr | 28/28 | | plan-net | 22/22 |
  | ndh | 50/50 | | pdex | 37/37 |
  | pas | 73/73 | | drug-formulary | 19/19 |
  | mhd | 42/42 | | subscriptions-backport | 9/9 |
  | eu-eps | 23/23 | | twpas | 43/43 |
  | eu-mpd | 4/4 | | davinci-pas | 80/80 |
  | au-ps | 17/17 | | gematik-epa-medication | 49/49 |
  | pacio-toc | 4/4 | | au-core | 26/26 |
  | dapl | 26/26 | | ipa | 12/12 |
  | us-core | 70/70 | | qicore | 63/63 |

  **Corpus total: 955/955 usable profiles across 34 IGs, all failed=0.** Plus:
  17-rung fixture ladder green (`walk_parity`), `convert_parity` green,
  `cargo test --workspace` all suites green. Compiler side confirmed untouched:
  `rust_sushi` (release) builds IPS with 32/32 StructureDefinition byte-parity and
  117/118 overall vs committed stock (the 1 diff is a pre-existing example-Bundle
  divergence, independent of `snapshot_gen`, which the compiler does not link).
  (Zero-profile packages — SAFR, Bulk Data, CDS Hooks, Application Feature, IHE
  VHL, C-CDA R5.0, C-CDA on FHIR, PH Query, Order Catalog — remain 0 usable R4
  constraint SDs per AGENTS.md and are not in the scorecard.)

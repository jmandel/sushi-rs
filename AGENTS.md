# AGENTS.md — Operating Manual for the Rust SUSHI Port

> **Read this first every session.** It is the durable memory of how this port
> is built, what works, what the gotchas are, and where we are. Keep it updated
> as facts change — it must survive context compaction. When you discover a new
> command, gotcha, or finish a phase, edit this file in the same turn.

## 0. HANDOFF — current state (read FIRST, updated 2026-06-30)

**SCORE — LEAD WITH IT.** The validation corpus is now **31 IGs** (12 core + 6 top-20 +
13 next-20), all in `harness/gate1.sh`. Current after the predefined-resource merge,
OnlyRule type dedupe, and harvested invalid-input alignment: **31-IG = 3401/3401
byte-identical (100.0%) + 4 tracked compat-breaks**. The last ccda-cda diff was duplicate
`ProblemObservation` in an `only` rule; stock `ElementDefinition.constrainType` starts with
`uniqWith(rule.types, isEqual)`, and Rust now mirrors that first-occurrence dedupe. The
**18-IG core (12+6top20) = 2491/2491 byte-identical** — the hard non-regression floor; NONE
may drop. PLUS a **permanent 326-case harvest of SUSHI's own unit tests**
(`tests/sushi-harvest/`, gate `harness/harvest-gate.sh`) at **256/256 resources (100.0%) /
326/326 cases (100.0%)**. Session
started at 1800 (12-IG). Caches: 12-IG → `temp/fhir-home`; 6 top-20 → `temp/top20-cache`;
13 next-20 → `temp/next20/fhir-home` (gate1.sh routes per-IG via IGCACHE/N20ROOT).
Validation reports: `docs/{holdout,top20,next20,harvest}-findings.md`. Don't single out
the old 4-IG "665" — retired.

**PREDEFINED RESOURCE PACKAGE — DONE (2026-06-30):** stock's
`DiskBasedVirtualPackage` / `predefinedResources.ts` behavior is mirrored by
`compiler::predefined`: JSON + SD-guided FHIR XML resources under `input/resources` and
the other stock predefined folders are loaded as a fishable virtual package. XML conversion
is StructureDefinition-guided (arrays from `max`, primitive `_value` sidecars, xmlAttr
handling, resource-contained resources, numeric/XML entity decode including `&#xA;`).
Fisher precedence is stock-aligned: predefined full bodies are fished before local/package
defs, but arbitrary non-conformance resources expose full bodies only (not metadata) so
SearchParameters do not shadow local extension metadata (plannet guard). Closed the target
files: `application-feature` **13/13** and `safr` **25/25**.

**SUSHI HARVEST PARITY — DONE (2026-06-30):** the permanent harvest of stock SUSHI unit
cases is now **256/256 resources and 326/326 cases byte-identical**. Closed by aligning
stock's invalid-input behavior instead of waiving it: invalid ValueSet compose URIs skip
the whole ValueSet, invalid binding/code systems are ignored like stock, integer primitives
reject non-integral floats and sign violations, AddElement target-type/flag ambiguity follows
ANTLR recovery, AddElement invalid target types produce no differential element, MS on
specialization AddElements stops after the stock partial mutation point, and empty assigned
pattern/fixed values are cleaned at final SD serialization rather than during rule application.

**SELF-RELIANT PACKAGE ACQUISITION — DONE & MERGED (2026-06-30).** The
`package_acquisition` crate (registry→CAS→materialize) is integrated; `rust_sushi build
<ig> --materialize` is the canonical self-reliant build. `harness/acquisition-dashboard.sh`
= **2065/2065, 0 mismatches** (our-acquired cache == stock-seeded cache, all 12 IGs). We no
longer depend on stock SUSHI to manage artifacts. See §3 env facts + README. CAS guard:
`reject_real_fhir_path` in `package_acquisition` `ensure_layout` + materialize/source paths.

**COMPAT-BREAK MECHANISM (2026-06-30):** intentional divergences where STOCK emits invalid/
buggy output are tracked in `docs/compat-breaks.json` + `tests/compat-golden/<ig>/<file>.diff`,
and counted SEPARATELY from byte-parity (not failures). It is **DIFF-BASED**: the gate asserts
the current `diff <stock> <ours>` EQUALS the recorded `.diff` — only the specific recorded
difference is tolerated; any other change in that file is flagged UNEXPECTED (regression) or
RESOLVED (stock now matches). Dashboard reports both **byte-identical** AND **EQUIVALENT
(parity + tracked divergences)**. First entry: **4 pas files** where stock emits an empty
required-extension scaffold (`{url}`-only, violates FHIR **ext-1**) as an order-dependent side-
effect of its shared `sdCache` — stock build is SILENT (0 err/0 warn). We emit valid FHIR. So
**Root Cause C is RESOLVED via compat-break, NOT the risky shared-cache work** (which existed
only to reproduce invalid output where stock violates ext-1).

**WHERE WE ARE (this session: 1800 → 2490/2491 + 4 compat-breaks):** the 12-IG corpus is 100%
equivalent; the 6 new top-20 IGs (bulk 13/13, pdex 179/179, plannet 110/110, formulary 86/86,
cdshooks 8/8, subscriptions 33/34) are all complete except the 1 file above. Fixed this
session via the **investigate-then-align loop** (deep-dive stock's algorithm → port it; NEVER
spot-fix): N7, G4, N1, G2 (carinbb perfect, genomics +64), G14/G11, G9, G5, narrative, G13
(instance order = InstanceOf snapshot order), VS/CS carets via replaceReferences/Canonical,
`unfoldChoiceElementTypes` for multi-type `value[x]`, underscore-sibling diff, IG
`normalizeResourceReference` + `.x`-version maxSatisfying + R5→R4 copyrightLabel translation,
SD inline-instance→pattern, strict soft-indexing (`convertSoftIndicesStrict`), cross-slice
extension scoping, DefIndex instance-CS url, xhtml attr whitespace, ecr `entry.resource`
full-resource embedding, sdc/dtr extension fixedUri on unfold (`profileToUse` guard), and the
**top-20 X-family** (X1 `fixCrossVersionDependencies` legacy `extensions.r5`→xver at load +
dependsOn; X2/X3 fish+unfold the SD behind `extension contains <URL>`, skip-if-unfishable; X5
copyrightLabel; the `findConnectedSliceElement` instance path) — PLUS the package-acquisition
merge + acquisition leniency. Catalogs: `docs/holdout-findings.md` (G1-G14), `docs/mining-
findings.md` (N1-N7), `docs/top20-findings.md` (X1-X6).

**REMAINING:**
- **ZERO real fails on the 18-IG set** — all 2491/2491 byte-identical (+4 compat-breaks). The
  last file (subscriptions CapStmt: X6 url-referenced extension on a primitive + the path
  sliceName-rewrite that orders implied metadata after `rest`) is FIXED (commits `7cd9756` then
  `a513bd8` — ORDER MATTERS: the rewrite must precede the url-resolution or it regresses 61).
- **NEXT-20 batch IN PROGRESS** — agent validating `docs/igs-to-test-with-next-20.json`
  (international/IHE/CDA set) → `docs/next20-findings.md`. Integrate/triage when it lands.
- **6 of the FIRST top-20 are NOT FSH** on their default branch (US Core/CDex/IPA/QI-Core/
  AU Core/SMART) — need an FSH branch/tag to test. Future expansion.

**USER-OWNED worktrees — DO NOT TOUCH:** `../sushi-rs-diagnostics` (`diagnostics-parity`),
`../sushi-rs-snapshot` (`snapshot-gen`).

**Longer-tail backlog:** N2/N4/N5 (decimal/empty-value/id-sanitization — mining, corpus-
invisible), **L1** leniency (reject invalid FSH like stock — tie to diagnostics worktree),
scaling SUSHI-test mining to the EXPORT suite. FIXED earlier: G1/G3 (T1 SD-driven
TypeResolver), G6 (T2 dir-reconcile).

## 1. What we are doing

Porting **SUSHI** (FHIR Shorthand compiler) from TypeScript to Rust, targeting
**byte-identical output and equivalent QA/diagnostics** vs stock SUSHI, but much
faster. Full design rationale: [`sushi-rust-port-plan.md`](./sushi-rust-port-plan.md).

Core principle: **this is a compatibility compiler.** When the FSH spec and stock
SUSHI disagree, **stock SUSHI wins** unless we make an explicit, recorded
compat-break decision. Never silently normalize a diff. Never skip instances or
diagnostics to make a gate pass.

## 2. Repo layout

```
sushi-rs/
  AGENTS.md                  <- this file (operating manual)
  sushi-rust-port-plan.md    <- the full port plan / methodology
  Cargo.toml                 <- Rust workspace
  crates/
    diagnostics/    SourceSpan, Severity, DiagnosticCode, DiagnosticSink (stable order)
    fsh_model/      Interner/Symbol, EntityKind; AST entities + rules (Phase 2)
    fsh_lexer_parser/  FSH lexer + parser -> typed AST (Phase 2)
    fhir_model/     StructureDefinition/ElementDefinition arena (Phase 5)
    package_store/  package DB/lock/cache access (Phase 1)
    json_emit/      byte-stable JSON emission, SUSHI property order (Phase 1/4)
    compiler/       insert expansion, tank indexes, export (Phase 3+)
    rust_sushi/     CLI binary `rust_sushi` (compile subcommand grows per phase)
  harness/          Phase 0 honesty harness (bash)
  docs/specs/       per-subsystem porting specs (from analysis subagents)
  temp/             scratch: stock oracles + candidate outputs (gitignored)
  sushi-ts/         <- git SUBMODULE: upstream FHIR/sushi @ v3.20.0 (the oracle source)
```

`sushi-ts/` is the **reference TypeScript implementation** we port from, pinned at
the exact version of the stock binary we diff against (v3.20.0). Read it; do not
edit it.

## 3. Environment facts (verified 2026-06-29)

- `cargo`/`rustc` **1.96.0**. `node` v24, `bun`, `npm` available.
- **Stock SUSHI binary** (the oracle): `/home/jmandel/periodicity/node_modules/fsh-sushi/dist/app.js`, **v3.20.0** — matches the submodule. Run via `node <app.js> build <ig> -o <out>`.
- **Benchmark IG (IPS)**: `/home/jmandel/periodicity/temp/ips-ig` (123 .fsh files).
- **FHIR packages — WE ARE SELF-RELIANT (since 2026-06-30).** The `package_acquisition`
  crate acquires packages itself from the FHIR registry → content-addressed store
  (CAS, default `~/.cache/fhir-rs/cas`, NEVER `~/.fhir`) → `materialize` a `.fhir/packages`
  tree → build. `rust_sushi build <ig> --materialize` is the canonical self-reliant build.
  PROVEN byte-identical: `harness/acquisition-dashboard.sh` = **2065/2065, 0 mismatches**
  across all 12 IGs (registry-acquired cache == stock-seeded cache). The isolated
  stock-seeded cache `temp/fhir-home/.fhir/packages` is now ONLY a convenience/offline
  cache + the speed path for the correctness gates; it is NOT a build dependency. The
  real `~/.fhir/packages` (~145 pkgs) is the user's and is NEVER read/written by us.
- **GOTCHA:** `/usr/bin/time` is NOT installed. Use bash/`date` timing.
- **GOTCHA:** `sushi build <ig> -o <OUT>` writes resources to **`<OUT>/fsh-generated/resources`** (SUSHI appends its own `fsh-generated`). So pass `-o temp/ips-stock`, then look in `temp/ips-stock/fsh-generated/resources`.
- This repo dir (`/home/jmandel/hobby/sushi-rs`) is its own git repo; `sushi-ts` is a submodule. The env banner "Is a git repository: false" is stale — it IS a git repo now.

## 4. The oracle (Phase 0 truth tables)

Stock SUSHI baseline for IPS (warm shared cache), recorded in
`temp/ips-stock/timing.json`:

| Metric | Value |
|---|---|
| resources generated | **118** |
| errors / warnings | 0 / 0 |
| wall time (stock, warm, no SQLite index) | ~39s |

(The plan's ~5.2s figure is a *strengthened* TS spike with a SQLite package
index; plain stock is the ~39s baseline. Our Rust target is 1.5–2.5s.)

**Stock oracles generated (in `temp/<ig>-stock/`, gitignored — regen via
`run-stock.sh`):** resource counts by type —
| IG | SD | VS | CS | Instances | total |
|---|--:|--:|--:|--:|--:|
| ips | 32 | 36 | 0 | 50 | 118 |
| epi | 28 | 24 | 5 | 55 | 112 |
| mcode | 53 | 103 | 1 | 193 | 350 |
| crd | 27 | 28 | 3 | 27 | 85 |
Export gate: `harness/diff-resources-glob.sh temp/<ig>-stock temp/rust-<ig> <Prefix...>`.

**Rust port performance (perf week, best-of-5 warm, main):** ips 0.74 / epi 0.58 /
mcode 0.84 / crd 0.66s — vs stock SUSHI ~39s (~50x) and ~-95% vs the pre-Phase-9 14s.
**No maintained warm artifact:** package_store builds its index in-memory from each
package's `.index.json` every run (no SQLite/persisted index); a build writes ONLY to
the output dir; cold vs warm OS page cache is ~3% (CPU-bound). Needs only a normal
extracted `.fhir/packages` cache. Perf log: docs/perf-protocol.md; map: docs/perf-map.md.

**31-IG self-reliant two-phase perf (2026-06-30):** `harness/perf31.sh` measures
CAS+lock→materialized cache separately from build-from-materialized-cache. Current
one-pass score (`temp/perf31` CAS; some packages predate derived-index caching, so
materialization is conservative): **50.8s materialize + 30.8s build = 81.6s total**
across all 31 IGs. Before this pass: **64.1s + 37.7s = 101.8s**. Landed perf work:
CAS `derived/materialized-index-v2.json`, opt-in `RUST_SUSHI_VERIFY_CAS=1` for old
per-materialize manifest checks, removed redundant per-file `mkdir`, and
CodeSystem concept duplicate detection `Vec`→`FxHashSet` (tw-pas build ~10.5s→2.4s).
Remaining materialization cost is mostly filesystem entry creation (`linkat` for
every file); a true next step is direct CAS-backed `PackageStore`/IG dependency
metadata, skipping physical `.fhir/packages` materialization for Rust builds.

## 5. Commands / methodology (the closed loop)

Per-change loop (from the plan): hypothesize → smallest corpus slice →
refresh oracle if needed → implement → unit fixtures → **resource byte diff** →
diagnostic diff → timing → classify any diff before optimizing.

### Harness commands
```sh
# Regenerate stock oracle for an IG (warm shared cache):
bash harness/run-stock.sh <ig-dir> <out-dir>
#   e.g. bash harness/run-stock.sh /home/jmandel/periodicity/temp/ips-ig temp/ips-stock

# Cold/isolated cache run (own ~/.fhir under RUN_HOME):
RUN_HOME=temp/iso-home bash harness/run-stock.sh <ig-dir> <out-dir>

# Byte-parity gate (target: no diff output):
bash harness/diff-resources.sh <stock-out> <candidate-out>
#   e.g. bash harness/diff-resources.sh temp/ips-stock temp/rust-ips
```

### Diagnostic parity (QA output)
```sh
# Normalize a captured SUSHI console log to ordered JSON (basename'd file refs):
node harness/diag.cjs normalize <out>/sushi-console.log --levels error,warn
# Order-sensitive diff of two runs' diagnostics:
node harness/diag.cjs diff <stock>/sushi-console.log <cand>/sushi-console.log --levels error,warn
```
Parses winston format `<level> <msg>` + `  File:`/`  Line:`/`  Applied …` footers.

### Parser oracle (Phase 2 golden)
```sh
# Dump stock SUSHI's import AST as stable JSON (Maps->{__map}, BigInt->{__bigint},
# class instances tagged __type). Logs silenced; stdout is pure JSON.
node harness/parse-oracle.cjs <file.fsh ...> > ast.json
node harness/parse-oracle.cjs --dir <dir-of-fsh> > ast.json
```
Verified: full IPS corpus = 123 docs, ~4.95MB AST. **Rule/value type frequency
in IPS (parser priority order):** CaretValueRule 2311, AssignmentRule 2224,
FshCode 1972, FshReference 270, CardRule 223, FlagRule 222, OnlyRule 171,
Instance 148, ValueSetFilterComponentRule 112, FshValueSet/FshQuantity/BindingRule 36,
Profile 29, ContainsRule 28, MappingRule/InsertRule/AddElementRule 16,
ValueSetConceptComponentRule 14, ObeysRule 9, Invariant 7, RuleSet 5, Logical 3,
FshCanonical 2, Mapping 1. **Key parity fact:** `* name 1..* MS` expands to TWO
rules (CardRule + FlagRule) sharing one source span.

### Lexer oracle (Phase 2 token golden)
```sh
# Dump stock SUSHI's exact ANTLR token stream (incl. HIDDEN whitespace; skipped
# comments absent). type=symbolic name, line=1-based, col=0-based UTF-16,
# start/stop=0-based inclusive UTF-16 offsets. Appends \n like the importer.
node harness/lex-oracle.cjs <file.fsh> > tokens.json
node harness/lex-oracle.cjs --text 'Profile: Foo' > tokens.json
```
The Rust lexer must reproduce this stream byte-for-byte. Confirmed: STAR token
is `"\n* "` (folds preceding newline+indent), keyword tokens include the colon
(`"Profile:"`), `CODE`/`REFERENCE`/`CANONICAL` are single multi-char tokens.

### Lexer parity check (Rust vs oracle, any file)
```sh
cargo build -p rust_sushi --release
diff <(node harness/lex-oracle.cjs <f.fsh>) <(target/release/rust_sushi lex <f.fsh>)
# empty diff = byte-exact token parity
```

### Rust commands
```sh
cargo build --workspace
cargo test  --workspace
cargo test  -p <crate>                 # focused
cargo run   -p rust_sushi -- <args>
```

### 31-IG two-phase perf
```sh
# Setup locks + populate temp CAS (not timed):
OFFLINE=0 harness/perf31.sh prepare

# Time CAS->materialized cache, then build-from-materialized cache:
RUNS=3 OFFLINE=1 harness/perf31.sh bench

# Profile one phase/IG (use frame pointers for useful call graphs):
CARGO_PROFILE_RELEASE_DEBUG=1 RUSTFLAGS="-C force-frame-pointers=yes" cargo build --release -q
harness/perf31.sh profile build tw-pas
harness/perf31.sh profile materialize crd
```
See `docs/perf31.md`. Keep `PERF31_WORK`/`FHIR_CAS` under `temp/` unless deliberately
using another scratch path; never use real `~/.fhir`.

### Cache isolation policy (SAFETY — non-negotiable)
**Never touch the user's real `~/.fhir`.** All runs use an **isolated FHIR home**
at `temp/fhir-home/` (override `FHIR_HOME=`, must be under repo `temp/`). The
isolated cache is **seeded by hardlinking** from the real cache (`cp -al`):
instant, zero extra disk (7.5G shared by inode), and it does **not** write to the
source. `harness/_guard.sh` enforces:
- **pre-run guard** `assert_isolated_fhir_home`: aborts (exit 99) if the FHIR home
  is the real home or not under repo `temp/`.
- **post-run guard** `assert_real_fhir_untouched`: aborts (exit 98) if ANY file
  under real `~/.fhir` changed during the run (catches leaks via shared inodes).

Use `NO_SEED=1` for a genuinely cold cache. Verified: IPS run under isolation =
118 resources, real `~/.fhir` 0 files modified.

## 6. Orchestration style (how I, the agent, work here)

I am the **orchestrator/manager** but I get my hands dirty on foundational,
cohesion-critical code (workspace shape, diagnostics, parser core).

- **Delegate** broad reads of the 25k-LOC TS source and parallelizable scaffolding
  to **subagents / Workflow** (ultracode), so my own context stays clean. I keep
  the conclusions (specs in `docs/specs/`), not the file dumps.
- I **verify** every delegated result against the oracle myself before trusting it.
- Subagents must **never** invent FSH/FHIR behavior — they cite `sushi-ts/` source
  (file:line) for every claim.

## 7. Phase status (update as we go)

> **🎉 PORT COMPLETE (2026-06-30): 665/665 byte-identical** across IPS/epi/mCODE/CRD
> (`bash harness/diff-resources.sh` = PARITY for all 4; `bash harness/parity-dashboard.sh`
> = 665/665). Every resource family matches stock SUSHI v3.20.0: StructureDefinition
> 140/140, ValueSet 191/191, CodeSystem 9/9, Instance 325/325, ImplementationGuide 4/4.
> **Phase 9 perf DONE**: warm IPS 14s→1.57s (target 1.5–2.5s; stock ~39s), mCODE 2.73s.
> `cargo test --workspace` green (18 suites). ALL plan phases 0–9 complete.
> **One documented gap (low ROI, corpus-unexercised):** winston-format *diagnostic
> emission* — exporters collect message wording but the CLI doesn't print it; this
> corpus is diagnostic-free (3/4 IGs 0/0; epi has 1 config warning). See §9.
> The per-phase notes below are historical detail (some "gaps" listed were later closed).

Phases from the plan (0–9). Current state:

- [x] **Scaffold** — workspace builds green, diagnostics + interner done, submodule pinned.
- [x] **Phase 0 — harness** — DONE: `run-stock.sh` (isolated cache), `diff-resources.sh`, `diag.cjs` (diagnostic normalize/diff), `lex-oracle.cjs` + `parse-oracle.cjs`, timing.json schema, IPS oracle. Remaining (deferred, not blocking): add SDC/CRD/US Core/mCODE/Cycle IGs when available locally (only IPS present now); full `compile` candidate wrapper grows with the compiler.
- [x] **Phase 1 — package_store + JSON emitter** — DONE & verified. json_emit
  landed in Phase 4; `package_store` now complete.
  - `crates/package_store/src/lib.rs`: `PackageStore::for_project(ig_dir, cache_dir)`
    resolves the dep graph exactly like `Processing.ts loadExternalDependencies`
    and indexes every resolved `<cache>/<id>#<ver>/package/.index.json`. Fishes via
    `fish_for_fhir` / `fish_for_metadata`. CLI: `rust_sushi pkg-fish <ig> <cache> <q...>`.
  - **Gate**: `harness/package-oracle.cjs` (HOME=isolated) vs `pkg-fish`, diffed by
    `harness/diff-pkg.cjs` → **PARITY 22/22** (core resources/datatypes, dep profile
    ipa-patient by url+id+name, transitive THO CS, high-auto extension @5.3.0,
    versioned url, negative). Resolves in ~0.1s. `cargo test --workspace` green.
  - **Dep resolution** (matches oracle's exact 6-package load order, confirmed via
    FPL `findPackageInfos`): `[sushi-r5forR4 virtual (SKIPPED, see gap)] → low auto
    (tools.r4@latest, terminology.r4@latest) → configured (ipa#1.1.0; extensions.r4
    SKIPPED here as it matches a High auto-dep) → FHIR core (last in configured pass)
    → high auto (extensions.r4 — substituted with the configured 5.3.0)`. `latest`
    = highest cached semver (terminology.r4 → 7.2.0). Fishing = gather id|name|url
    candidates, filter by type (+version), sort by `FISHING_ORDER` rank then LIFO
    (reverse global load seq); first wins. SD classification from `.index.json`
    `derivation`/`kind`/`type`. Metadata key order/falsy-omission ports
    `convertInfoToMetadata`.
  - **SURPRISE vs the task brief**: SUSHI's `defs.loadPackage(id,ver)` does **NOT**
    walk transitive `package.json` deps — the oracle loads exactly the 6 packages
    above; ipa's own deps (THO 6.2.0, ext 5.2.0, smart-app-launch 2.0.0) are NOT
    pulled in (smart-app-launch isn't even in cache). So no transitive walk is
    implemented (it would diverge from stock).
  - **KNOWN GAPS** (deferred, don't affect the gate): (a) bundled R5-in-R4 virtual
    package (`sushi-r5forR4#1.0.0`, 7 R5 type defs) not loaded — JSONs live in
    `sushi-ts/src/fhirdefs/R5DefsForR4/`, not the cache; queries for
    Base/CodeableReference/SubscriptionTopic/etc. would miss. (b) predefined/local
    `sushi-local#LOCAL` + MasterFisher precedence not here (compiler-side). (c) xver
    `[x]` URL fallback, npm-alias warnings, fixCrossVersionDependencies rewrite not
    ported. (d) name index is eager over all SD/VS/CS (fine now; revisit Phase 9).
  - Notes: `docs/specs/package-store-notes.md`, `06-package-fhirdefs.md`.
- [x] **Phase 2 — FSH parser + AST** — DONE & verified.
  - **Lexer**: `lex.rs` (~900 lines, FSHLexer.g4 port). Byte-exact vs ANTLR oracle
    on 423 files. Gate: `cargo test -p fsh_lexer_parser` (lex_parity 8/8).
  - **Parser+importer+dumper**: `fsh_model/ast.rs` (AST types), `parser.rs`
    (~3270 lines, FSH.g4 + FSHImporter: two-pass global aliases/param-RuleSets,
    extractStartStop span math, soft-index, alias res, MiniFSH param expansion),
    `dump.rs` (oracle-shape JSON). Gate: ast_parity 8/8. Independently verified
    semantic AST parity vs parse-oracle on **178 real files (123 IPS + 55 diverse
    IGs), 0 diffs** (agent reported ~1450 clean).
  - CLI: `rust_sushi {lex,ast} <file>`; comparator `harness/cmp-ast.cjs`.
  - **KNOWN GAPS for later** (from parser agent): (a) NO diagnostics emitted yet
    (gate is AST-only — `logger.error/warn` + FSHErrorListener catalog deferred);
    (b) AddElement/addCRElement + MappingRule lightly corpus-exercised — want
    fixtures; (c) bigint huge-magnitude edge cases; (d) nested `[[{param}]]` insert
    params; (e) ANTLR error-recovery not byte-matched. None block Phase 3.
- [x] **Phase 3 — insert rules + tank indexes** — DONE & verified.
  - `compiler::expand_to_json` (`crates/compiler/src/lib.rs`): imports via the
    Phase-2 parser, runs `applyInsertRules` over every entity in FHIRExporter
    order (invariants → profiles → extensions → logicals → resources → CS → VS →
    instances → mappings), serializes post-expansion AST via `fsh_lexer_parser::dump`.
    Gate `cargo test -p compiler` (expand_parity 7/7). Oracle:
    `harness/expand-oracle.cjs`; spec `docs/specs/08-insert-rules-tank.md`.
  - Design: tank = owned `Vec<FshDocument>`; borrow-safe in-place RuleSet mutation
    via **take/expand/replace** (`std::mem::take` the rules out of the entity or
    RuleSet so `&mut docs` is free for recursive fishing). `RsLoc`/`Field` locators
    index into docs. `is_allowed_rule` static table, `DefKind` discriminant,
    `fish_ruleset`/`fish_applied`/`resolve_alias` mirror FSHTank. Helper methods
    added to `fsh_model::Rule` (`source_info_mut`, `path`, `set_path`, `is_insert`,
    `constructor_name`). `rust_sushi expand <f...>` drives it for corpus diffs.
  - PARITY TRAPS that bit: (1) appliedFile/appliedLocation are stamped on the
    **shared** RuleSet rule (mutation persists in the tank, observable in the
    ruleSets map; last-insert-wins, e.g. e03/e07). (2) `[+]→[=]` handoff is on
    `context` *after the first pushed rule* — distinct from resolveSoftIndexing
    (which is EXPORT-time NUMBER resolution, NOT in this gate; goldens keep
    [+]/[=] literal). (3) ConceptRule-with-NO-system into a ValueSet is
    **rejected** by isAllowedRule (e06 → empty rules), NOT converted; conversion
    only fires when the concept carries a system. (4) circular detection keyed on
    the `JSON.stringify([name,...params])` identifier string in a `Vec`
    (Array.includes), checked AFTER fishing succeeds.
  - **Breadth check** (`rust_sushi expand` vs oracle, semantic JSON eq): MATCH on
    IPS (123/123), fhir-ips, CARIN-BB, mCODE, and **SDC per-file 212/212**.
  - **FIXED** the SDC nested-parameterized-insert gap: `parse_generated_ruleset`
    now merges the temp doc's `appliedRuleSets` into the parent (FSHImporter.ts:
    2016-2018). Gated by fixture `09_nested_param_insert`.
  - **KNOWN RESIDUAL** (deferred, narrow): whole-IG *multi-file* SDC still has a
    single off-by-one — `appliedLocation.endColumn` (165 vs 166) on a rule inside a
    **doubly-nested** applied RuleSet. Span-rebasing edge in nested param inserts
    (`rebase_rule` adjusts lines, not columns). Low impact; revisit when chasing
    diagnostic span parity.
  - NOT COVERED (deferred): diagnostics are collected into a `Vec<String>` sink but
    NOT emitted/gated (exact wording ported though); `fishForMetadata`/full
    `internalFish` matcher (only RuleSet fishing needed here).
- [x] **Phase 4 — ValueSet/CodeSystem export** — DONE (byte parity except 1
  package-dependent VS). `compiler::build_project` reads `sushi-config.yaml`
  (`config.rs`), imports+expands all `input/fsh/**/*.fsh`, and writes
  `ValueSet-*.json` / `CodeSystem-*.json` via `json_emit::to_fhir_json_string`.
  - `json_emit`: `ordered_clone_deep` (underscore-sibling gluing, `common.ts:1571`)
    + `to_fhir_json_string` = `serde_json::to_string_pretty` (2-space) + `\n`.
    serde_json (preserve_order) matches `JSON.stringify(_,null,2)` byte-for-byte
    here (no non-ASCII/`/` escaping, empty `[]`/`{}`).
  - `compiler::export` ports ValueSet/CodeSystem exporters: setMetadata order
    (constructor-seeded `status` for VS; `status`+`content='complete'` for CS,
    THEN name,id,title,description,[version if FSHOnly],url), caret rules, compose
    (include/exclude, concept dedupe-merge `addConceptComposeElement`, filters),
    CS concepts (+hierarchy) and `updateCount`. `id` is the recomputed getter
    (findLast non-instance `^id`). `url = {canonical}/{Type}/{id}`.
  - Caret engine: embedded element-type table for VS/CS + datatypes (Meta,
    Identifier, ContactDetail/Point, CodeableConcept, Coding, Extension w/
    `value[x]` choice) instead of fishing the real SD (no packages). Port of
    `setPropertyOnInstance` array/slice handling incl. `extension[url]` slice.
  - **Gate**: IPS **35/36 VS**, epi **24/24 VS + 5/5 CS**. `cargo test --workspace` green.
  - PARITY TRAPS: (1) CS/VS key order is insertion order seeded by the TWO/THREE
    constructor-initialized fields, NOT a PROPS array. (2) VS runs setCaretRules
    BEFORE setCompose; CS runs setConcepts BEFORE setCaretPathRules. (3) `op`
    `descendant`→`descendent` already normalized in parser; alias-resolved code
    `system`s already URLs in AST. (4) version-less compose `version` set to
    undefined → dropped. (5) FshCode→CodeableConcept/Coding coercion key order
    (code,system,version,display). (6) inline-concept includes with same
    system+version MERGE into one `compose.include` entry.
  - **KNOWN RESIDUAL** (package-dependent, not fixable w/o FHIR cache):
    `ValueSet-problem-type-uv-ips` references CodeSystem **by bare name**
    `ConditionCategoryCodes` (THO `condition-category`); stock fishes its url
    from packages, we fall back to the literal name. The only IPS VS that needs
    an external (non-local, non-alias) CodeSystem-name→url resolution.
  - **Phase 4.1 — caret key-ordering parity (DONE, 2026-06-29).** VS/CS now
    **ips 35/36, epi 29/29, mcode 104/104, crd 28/31** (was mcode 44, crd 15).
    Root cause (confirmed): NOT class-field declaration order. The compiled dist
    constructors only seed resourceType+status(+content), so key order is JS
    insertion order. Stock's `setCaretRules`/`setCaretPathRules` run
    `setImpliedPropertiesOnInstance` BEFORE the caret value-assignment loop; the
    only implied/fixed value for VS/CS metadata carets is an
    `extension`/`modifierExtension` slice's fixed `url`. Setting that url in the
    pre-pass inserts the `extension` key in element order (early), ahead of later
    metadata caret keys (`copyright`/`experimental`/…) even when the extension
    rule appears AFTER them in source (e.g. an inserted RuleSet sets copyright+
    experimental, then `^extension[FMM]`). Fix in `export.rs`: `precreate_implied`
    pre-pass (creates extension slice urls) runs before the value loop in both
    `export_value_set` and `export_code_system`.
  - **CRD CS (0/3 → 3/3, DONE):** ported `^property[0].*` (top-level
    CodeSystemProperty) + **concept-level caret rules** via `find_concept_path`
    (`CodeSystemExporter.findConceptPath`: pathArray of `#code`s →
    `concept[i].concept[j]` prefix into the built concept tree; concept-level
    `^property.valueBoolean` → CodeSystemConceptProperty value[x]). Added
    CodeSystem `property`/`concept`/`filter` + the concept/property/designation
    datatypes to `field_def`, and CodeSystemConceptProperty to `resolve_choice`.
  - **REMAINING VS failures (4)** — all the same external-name-resolution class
    (needs package_store fishing wired into the VS exporter; out of export.rs-only
    scope): ips `problem-type-uv-ips`; crd `locationAddressType` (AddressType),
    `orderDetail` (ServiceType / ExampleVisionPrescriptionProductCodes),
    `serviceRequestCodes` (SNOMED_CT) — bare external CS/VS names, no local def,
    no alias.
- [x] **Phase 5/6 — StructureDefinition export** — IN PROGRESS (byte parity:
  **epi 26/28, ips 27/32, crd 19/27 SD = 72/87**). VS/CS gates stay green; `cargo
  test --workspace` green. Differential-only output.
  - `fhir_model` (`lib.rs`+`props.rs`): `StructureDefinition`/`ElementDefinition`
    modeled as **ordered `serde_json` maps** (not typed structs) with a captured
    `_original` map. `path` derived from `id`. Ports `fromJSON` (PROPS filter),
    `findElementByPath` (fast-path + unfold + `findMatchingSlice` + `sliceMatchingValueX`
    + no-bracket slice filter), `unfold` (contentReference + sliceName + type-fish
    branches), `addElement` (regex-style slice/child insertion ordering), `addSlice`,
    `sliceIt`, `hasDiff`/`calculateDiff` (PROPS order, ADDITIVE mapping/constraint,
    choice-slice `type`), `toJSON` differential. ED type objects re-ordered to
    `ElementDefinitionType.toJSON` order (code first, extension last).
  - `compiler/src/sd_export.rs`: `SdContext` exports SDs on demand in tank order
    (profiles→extensions→logicals→resources). `FisherView` = exported locals +
    in-progress **early-push metadata** (circular fishing) + package_store. Ports
    getStructureDefinition, setMetadata (UNINHERITED_SD_EXTENSIONS, ext root short/
    definition, url.fixedUri), resetParentElements, preprocess (ext 0..0 inference),
    setRules dispatch: Card (w/ slice→parent min propagation), Flag, **OnlyRule**
    (constrainType: getTypeLineage + findTypeMatch + applyTypeIntersection + applyProfiles,
    Reference>CodeableReference preference), Binding (bindToVS REPLACES binding),
    Assignment (assignValue + discriminator-path min→1), Obeys (constraint +
    invariant-rules-as-`constraint[i].*` carets), ContainsRule (extension + plain
    slicing), CaretValue (element + SD-body). setContext (default Element).
  - `compiler/src/caret_schema.rs`: embedded FHIR element-type table (SD + ElementDefinition
    + datatypes) → reuses `export.rs` `apply`/`coerce`. Handles `[x]` choice keys,
    extension-by-url slices, numeric indices, and **primitive-sibling `_targetProfile`/
    `_profile`** redirect for `^type[n].targetProfile[m].extension`. Alias brackets
    (`extension[$fmm]`) resolved via a global alias map.
  - `compiler/src/paths.rs`: `resolveSoftIndexing` (`[+]/[=]`→numbers on path+caretPath),
    `parseFSHPath`/`splitOnPathPeriods`.
  - **Cache wiring**: `build_project` resolves the cache via `FHIR_CACHE` env or
    `temp/fhir-home/.fhir/packages` (cwd-relative), fail-loud, never `~/.fhir`.
  - **PARITY TRAPS that bit**: (1) ED type-object key order = `ElementDefinitionType.toJSON`
    (code first), not FHIR/PROPS order. (2) `hasDiff` slice/child walk uses `.`-children
    ONLY (slices excluded) — counting slices as children put spurious `{id,path}` sliced
    elements in the diff. (3) bindToVS REPLACES the whole binding `{strength,valueSet}`
    (drops inherited description/extension). (4) assigning a value/pattern discriminator
    path forces the element min→1. (5) invariant Severity/Expression/XPath come from the
    invariant's **assignment rules** (→`constraint[i].*` carets), not just keywords. (6)
    constrainCardinality on a slice bumps the sliced element's min to the sum of slice mins.
    (7) `^extension[url][n]` is the n-th occurrence of that url. (8) circular SD refs need
    early-push metadata so an in-progress SD's url is fishable. (9) primitive `.extension`
    children go to the `_x` sibling array.
  - **NOT-YET / KNOWN GAPS** (the remaining 15 SD failures): (a) **AddElementRule**
    (logicals/resources) is a stub → ips Document/DocumentSection/IPSSectionsLM + crd
    CRDMetricData MISSING (also need the bundled **R5-in-R4 `Base`** def, package_store
    gap a). (b) **predefined `input/resources/*.{xml,json}`** not loaded → crd bindings to
    those VS (nutritionorder etc.) resolve to wrong/literal urls. (c) deep **nested entry
    slicing** (ips Bundle/Composition, epi composition/MedicinalProductDefinition,
    crd servicerequest/medicationrequest) — reslicing + `_profile`/`_targetProfile`
    rebuild on constrainType not fully ported. (d) nested **contentReference ordering**
    (epi composition section.section). (e) `setContext` only does the default/quoted
    cases (no fishing-based element/url contexts). (f) VS/CS caret rules still don't
    resolve `$alias` brackets (pre-existing Phase-4 gap; shows up only in crd VS, which
    was never gated). (g) AssignmentRule of an Instance / deferred `^contained` carets
    not handled.
- [x] **Phase 6 — full SD compatibility** (DONE 665/665; notes historical) — **137/140 SDs byte-identical (mcode
  53/53, ips 31/32, crd 26/27, epi 27/28)** after the 2026-06-29 SD-parity pass
  (dashboard 638→654/665). Remaining 3 SD: ips `Composition-uv-ips` + epi
  `composition-epi-type1` (deep `section.section` reslicing + nested
  contentReference min ordering — hardest, not started); crd `profile-nutritionorder`
  (binds to ValueSets defined as **predefined `input/resources/*.xml`** — needs
  predefined-resource (incl. XML) loading, out of sd_export scope).
  - **2026-06-29 SD-parity pass fixes (16 resources: 13 SD + 3 SD-blocked instances):**
    1. **fhirVersion for Base-parented logicals** (`get_structure_definition`): when
       `baseDefinition == .../StructureDefinition/Base` (the time-traveling R5-in-R4
       bundle, fhirVersion 5.0.0), override with `config.fhir_version()` (4.0.1).
       Propagates through the export chain (Document fixed → IPSSectionsLM inherits).
       Fixed ips Document/DocumentSection.
    2. **maxValueSet `valueCanonical`** (6 mcode): `resolve_canonical_caret` resolves
       `Canonical(localName)` in element/SD-body caret values against the fisher (SD)
       then local `vs_url`/`cs_url` closures (mirrors replaceReferences). Was emitting
       the bare name.
    3. **patternQuantity from a FshCode on a Quantity `value[x]`** (mcode radiotherapy
       + unblocks 3 instances: Procedure×2 + Bundle-jenny-m): `is_quantity_type` branch
       in `assign_value_inner` coerce → `{code,system,unit}` (`FshCode.toFHIRQuantity`).
    4. **`extension[alias/url/id]` → inherited sliceName** (mcode cancer-patient
       birthsex): `find_element_with_ext_fallback` in `set_rules` (findMatchingSlice
       fishForFHIR branch) + `StructureDefinition::find_slice_by_profile_url`. Kept the
       generic `find_matching_slice` UNTOUCHED (instance_export relies on it returning
       None to trigger its own bracket-rewrite — changing it regressed 9 mcode instances).
    5. **AddElementRule** (ips IPSSectionsLM, crd CRDMetricData): `apply_add_element` +
       `initialize_element_type` (port of newElement + applyAddElementRule). base +
       Element root constraint are SKIPPED (excluded from the differential anyway — a
       new element has no captured original, so set keys = type/min/max/short/definition
       all show; base/constraint would spuriously show, so don't set them).
    6. **Binding fishes ValueSet only** (`fish_for_metadata_vs`, new Fisher-trait method
       w/ default): `* type from ResourceType` was matching a StructureDefinition; stock
       does `fishForMetadata(_, Type.ValueSet)`.
    7. **matchesLogicalType profile suppression** (`apply_profiles` logical branch): a
       logical-model type code is a URL; do NOT add it as profile/targetProfile
       (`ElementDefinition.ts:1582-1589`). Fixed the spurious `profile:[url]` on
       IPSSectionsLM section elements.
    8. **Mapping export** (`SdContext::export_mappings`/`export_mapping`, run after
       export_all): port of MappingExporter — SD-level `mapping` (identity/name/uri/
       comment, with parent-merge check) + element-level `mapping` (identity/map/
       comment/language) via findElementByPath. `original_mapping` diff infra already
       existed. (element lookups use an empty local-exported fisher to dodge the
       &mut/&borrow conflict — fine since mapping paths are direct element names.)
    9. **getActualCode for the FHIRPath-primitive check** (`constrain_type`): the
       `fhirPathPrimitive` guard must use the RAW `code` field (`getActualCode`), NOT the
       fhir-type-ext-resolved `type_code`. `* url only uri` was rewriting Extension.url's
       `code` from `System.String` to `uri`. Fixed crd ext-coverage-information.
       **GENERIC change to constrain_type — re-ran full dashboard, no regression.**
- [x] **Phase 7 — instance export** (DONE 665/665; notes historical) — LARGELY DONE & verified. Byte parity (2026-06-29
  update): **mcode 189/193, epi 54/54, ips 49/49, crd 26/26 real instances** — only
  remaining instance misses are 3 mcode (SD-blocked, see below) + the per-IG
  ImplementationGuide resource (separate IGExporter, not ported). SD/VS/CS gates +
  `cargo test --workspace` (18 suites) stay green. Gate
  `harness/diff-instances.sh temp/<ig>-stock temp/rust-<ig>`.
  - **2026-06-29 nested-extension + canonical fixes (19→3 instance failures, dashboard
    620→638/665), all in `instance_export.rs`:**
    1. **Alias-resolving fisher** (`AliasFisher`): every fish in instance export now
       resolves FSH aliases first (mirrors `FSHTank.fish` `resolveAlias`). The
       FisherView/package fisher had no alias map, so `extension[USCoreRace]`-style
       bracket fishes returned None.
    2. **Nested extension slice match by profile url** (`find_ext_slice_by_profile`):
       in `validate_value_at_path`'s extension fallback, when the bracket (alias/url/id)
       doesn't equal a sliceName, match an EXISTING inherited slice whose
       `type[0].profile[0]` == the fished url and rewrite the bracket to that sliceName
       (ports `findMatchingSlice` fishForFHIR branch StructureDefinition.ts:908-913 +
       path-rewrite 671-681). Fixed mcode Patient×6 + Condition×2 (`extension[USCoreRace]
       .extension[ombCategory]...` → inherited `race` slice, then existing unfold
       machinery resolves the nested sub-extension). NOTE: the generic
       `find_matching_slice` in fhir_model still lacks this fallback (left untouched to
       avoid SD-parity churn); the fix lives entirely in the instance engine.
    3. **`Canonical()` local resolution**: `replace_references` now resolves
       `Canonical(name)` against local ValueSet (`vs_url`), CodeSystem (`cs_url`), and
       Instance (`InstanceIndex.inst_url`, from each instance's `* url =` rule) urls —
       only when the package/SD fisher can't resolve an SD url first (stock precedence,
       ElementDefinition.ts:2006). Fixed mcode ConceptMap (sourceCanonical/source) + ips
       ActorDefinition-Server (capabilities=Canonical(ips-server),
       derivedFrom=Canonical(Creator)).
    4. **`assignedResourcePaths` wired** (was passed `&[]`): set_assigned_values now
       collects inline-instance assignment paths and passes them to
       `set_implied_properties_on_instance`, so implied/fixed values are NOT injected
       into embedded resources (common.ts:518). Fixed both epi type3 Bundles (the
       embedded ClinicalUseDefinition got a spurious fixed `type` injected first +
       fullUrl/resource key order swapped).
  - **REMAINING 3 mcode instance failures are SD-export-blocked** (NOT instance bugs):
    Procedure×2 + Bundle-jenny-m all embed `mcode-radiotherapy-dose-delivered-to-volume`,
    whose `totalDoseDelivered.value[x]` should carry `patternQuantity {code:cGy,
    system:ucum}` from `* extension[totalDoseDelivered].valueQuantity = UCUM#cGy` (an
    AssignmentRule of a FshCode to a Quantity-typed `value[x]` choice). Our SD export
    drops it (this SD is 1 of the 8 failing mcode SDs), so the instance can't apply the
    implied code/system → wrong valueQuantity key order + url placement. Fix belongs in
    `sd_export.rs` (Phase-6 cleanup), which was out of scope for this pass.
  - `compiler/src/instance_export.rs` (~2200 lines) ports `InstanceExporter` + the
    `common.ts` machinery. `Exporter` = memoized recursive export (RefCell memo)
    over `SdContext` (fishes the InstanceOf snapshot via `ctx.fish_sd_json`/
    `FisherView`). Per instance: resourceType/id/meta prefix → `set_assigned_values`
    → `apply_meta_profile` → `clean_resource` → `order_instance` (resourceType,id,meta
    prefix then insertion order via `json_emit::ordered_clone_deep`). Written = usage
    != Inline; non-resource InstanceOf forced Inline.
  - Ported: `validate_value_at_path` (path walk + find_element_by_path + `0`-bracket
    array insertion + primitive flag + extension-slice creation + leaf coerce),
    `coerce_value` (FshCode→code/Coding/CodeableConcept/Quantity/CodeableReference by
    element type; Quantity/Ratio/Reference/Canonical; xhtml minify),
    `set_implied_properties_on_instance` (BFS ElementTrace + sliceTree/effectiveMins +
    connected elements + requirementRoot sort), `set_property_on_instance`
    (arrays/slices/`_x` primitive siblings/`assignComplexValue` w/ partial-match merge),
    `create_useful_slices`+`determine_known_slices`, `replace_references`,
    `clean_resource` (+ contained-ref `#id` rewrite), lodash `merge`. Config respects
    `instanceOptions.{manualSliceOrdering,setMetaProfile,setId}`; `Usage:#definition`
    sets url/title/description; inline-instance assignment embeds memoized bodies.
  - **PARITY TRAPS that bit (record!)**: (1) **`serde_json::Map::remove` is
    swap_remove** → reorders keys; MUST use `shift_remove` (broke every sliced
    instance). (2) **unfold/add_element shift the elements Vec** → the implied-property
    traversal MUST key ElementTrace by element **id**, re-resolving the index each
    iteration (index caching caused runaway `id.id.id…` recursion). (3) instance key
    order = `createUsefulSlices`/implied placeholders FIRST (mutate instanceDef before
    rules), THEN `merge(instanceDef, ruleInstance)` appends rule-only keys in rule
    order — NOT pure rule order; **manualSliceOrdering (epi/crd) → createUsefulSlices
    + SKIP the implied sort; ips → determineKnownSlices + sort**. (4) `assignComplexValue`
    needs the partial-match branch or you get duplicate codings. (5) xhtml `text.div`
    runs through html-minifier (collapseWhitespace, attr quotes→`"`, ` />`→`/>`,
    block-vs-inline) — ported in `minify_xhtml`. (6) reference resolution is by
    **effective id** (last `* id =` rule), not declared name; numeric inline ids use
    raw_value + Resource-element MismatchedType recovery.
  - **KNOWN GAPS**: (a) R5 `ActorDefinition` instances (ips 3, crd 5) not in R4 cache
    (package_store gap a). (b) `ImplementationGuide` resource (separate IGExporter).
    (c) 2 epi Bundles with a deeply-nested inline ClinicalUseDefinition: `type`/`fullUrl`
    ordering diff (standalone cud passes; embedded copy differs). (d) instance
    QA/diagnostics (validateRequiredElements/checkForMultipleChoice/nameless-slice)
    collected-as-wording but NOT emitted/gated.
- [x] **Phase 7b — ImplementationGuide resource export** (DONE, 2026-06-30). New
  module `crates/compiler/src/ig_export.rs` writes `ImplementationGuide-<id>.json`;
  wired into `build_project` AFTER all other resources (it references them all).
  **All 4 IGs byte-identical** (ips/epi/mcode/crd). Dashboard 654→**662/665**.
  - Design: `build_project` collects per-resource IG metadata as it exports —
    conformance (SD profiles/ext/logicals + VS + CS): `name = title ?? name ?? id`,
    `description`; instances: `IgInstanceMeta` returned from `export_instances`
    (`reference_key`, name=`inst.title ?? body.title ?? inst.name`, description,
    usage, logical?, `instance_of_url`=sd.url, `meta.profile`). `ig_export::export_ig`
    ports `initIG`/`fixDependsOn`/`addResources`/`addPackageResource`/
    `addPredefinedResources`/`addConfiguredResources`/`sortResources`/
    `addConfiguredGroups`/page+parameter builders + the R5/R4 transforms. Config is
    read straight from `serde_yaml::Value` (full yaml), not the typed `Config`.
  - **PARITY TRAPS that bit:** (1) `ctx.exported` lists an SD MORE THAN ONCE
    (on-demand re-export during circular fishing) → must DEDUP conformance by id
    (stock's pkg.profiles has each once) — this was the epi 4-dup bug. (2) **R4 vs
    R5 IG shape differs** (epi is R5/5.0.0): R5 uses `isExample` (not
    `exampleBoolean`), `parameter.code` is a `{code,system:ig-parameters}` Coding,
    and `page` keys are `name`/`sourceUrl` ordered AFTER `page[]`; R4 uses
    `exampleBoolean`/string `code`/`nameUrl`. (3) **dependsOn key order** =
    parsed-config order (id,packageId,uri,version) minus `reason`, plus missing
    uri/id appended, plus the reason→`extension[valueMarkdown]` appended LAST (R4).
    `uri` when absent is fished from the dependency package's IG `url` via its
    `.index.json` (`find_dependency_ig_url`). (4) **resource ORDER** = stable sort
    by `name.toUpperCase()` (none of our IGs hit the config/group full-sort paths).
    (5) **predefined resources** (crd `input/resources`, 33 files incl. **XML**):
    needed a lightweight FHIR-XML field extractor (resourceType/id/title/name/
    description) — non-examples key order is `reference, description, exampleBoolean,
    name`; Binary/* config entries skip the predefined file + are pushed verbatim by
    addConfiguredResources. (6) **group resources are BARE names** → normalize to
    `Type/id` (fish against built entries) before setting `groupingId`/group-sort.
    (7) **disk-scanned pages** (ips uses `menu`, no `pages` → `addOtherPageContent`
    scans `input/pagecontent`): title = `title-case(lodash.words(name))` — ported
    `title-case@3.0.3` (SMALL_WORDS keep "in"/"of"/"the" lowercase) + lodash `words`;
    `nameUrl` keeps the original filename; `-intro`/`-notes` + `index` filtered;
    sorted by numeric-prefix then `localeCompare`. epi/mcode/crd use `pages` config.
  - Files I own/touched: `ig_export.rs` (new), `lib.rs` `build_project` wiring +
    `conformance_from_body`/`sd_ig_name` helpers, `instance_export.rs`
    (`IgInstanceMeta`/`InstanceExport`, `export_instances` return type). NO change to
    SD/VS/CS/instance body output (gates unchanged).
  - **KNOWN GAPS (not exercised by the 4 IGs):** path-resource `/*` recursion, meta
    `instance-name`/`instance-description` extensions on predefined non-conformance
    resources, R5 `guide-parameter-code` VS membership (hardcoded ig-parameters —
    fine since epi's codes aren't in it), useContext/template/global beyond
    passthrough, menu.xml/page-file generation (not needed for the JSON resource).
- [x] **Phase 8 — full corpus parity** — scorecard `harness/parity-dashboard.sh`.
  **Current: 665/665 byte-identical — COMPLETE** (662 before the 2026-06-30 final-3 SD
  pass; 654 before IG export; 638 before the 2026-06-29 SD-parity pass; 620 pre
  instance nested-ext/canonical; 547 pre-Phase-4.1; 247 pre-SD). SD now mcode 53/53,
  ips 32/32, crd 27/27, epi 28/28 (140/140). VS/CS/instances/IGs all clean. `cargo
  test --workspace` green (18 suites).
  - **2026-06-30 final-3 SD fixes (the last 3 SD edge cases):**
    1. **crd `profile-nutritionorder`** — bindings `* path from <Name>` to ValueSets
       defined ONLY as **predefined `input/resources/*.xml`** (TypesOfEdibleSubstances→
       edible-substance-type, DietCodes→diet-type, NutrientCodes→nutrient-code)
       resolved to a wrong same-named THO/core VS (or not at all). Fix: load a
       predefined-VS name/id/url→canonical-url map (`ig_export::predefined_vs_map`,
       reuses the predefined scanner + a new `url` field on the XML/JSON extractor)
       and consult it in `FisherView::fish_for_metadata_vs` BEFORE the package fish
       (SUSHI FHIRDefinitions precedence: predefined wins over packages). Threaded
       via `build_sd_context(..., predefined_vs)` → `SdContext::set_predefined_vs`.
    2. **epi `composition-epi-type1`** — nested contentReference (`section.section` →
       `#Composition.section`) was cloning children from the ALREADY-CONSTRAINED
       parent snapshot, so diffs were relative to the constrained section (dropped
       `min:1` on `.id`, spurious `min:0` on `.title`, missing `.code.coding.system`
       etc.). Fix: port `ElementDefinition.unfold`'s two-branch logic — clone from the
       constrained snapshot ONLY when the referenced element carries the
       elementdefinition-profile-element extension in the differential
       (`has_profile_element_extension`, rare SDC case); otherwise clone from the
       **unconstrained base resource** (`clone_children_from_def` over a
       `StructureDefinition::from_json` of the fished base type). Diffs now taken vs
       base cardinalities. `type:[BackboneElement]` is re-set from the base ref.
    3. **ips `Composition-uv-ips`** — named section slices (`section:sectionProblems`
       …) dropped their inherited `extension:section-note` sub-slice (16 missing
       reslice elements). Root cause: the sliceName-unfold path cloned children with
       `recapture=true`, so the slice extension recaptured its constrained state as
       original → `hasDiff` false → omitted. Fix: port `cloneChildren`'s per-child
       `shouldCaptureOriginal = recaptureSliceExtensions || sliceName==null ||
       !path.endsWith('.extension')` (`reclone_capture` helper); the sliceName unfold
       now passes `recaptureSliceExtensions=false` (`ElementDefinition.ts:2742`,2814),
       so slice extensions keep their inherited (base) original and show as diffs.
    All three are in `fhir_model/src/lib.rs` (unfold/clone) + `compiler/src/sd_export.rs`
    (binding fisher) + `compiler/src/ig_export.rs` (predefined scanner) + `lib.rs`
    wiring. Full dashboard re-run after each — NO regression to the 130+ passing SDs.
- [x] **Phase 9 — optimization loop** — DONE (2026-06-30). Warm IPS **14s → 1.57s
  (8.9x)**, mcode **13.5s → 2.73s**, epi 1.11s, crd 1.63s; all in/near the 1.5–2.5s
  target (stock ~39s). **Parity preserved: 665/665 byte-identical, `cargo test
  --workspace` green.** Full write-up + before/after profiles in
  `docs/perf-notes.md`. Four parity-preserving changes:
  1. **Cache `id`/`path` as String fields on `ElementDefinition`** (mirror
     `map["id"]/["path"]`, written only by new/from_json/set_id) — kills the 62%
     IndexMap/SipHash from `e.id()`/`e.path()` in every linear scan. 14s→1.88s.
  2. Hoist `format!` out of `fhir_model` hot filter loops (find_element_by_path,
     get_slices). →1.77s.
  3. **Lazy `FxHashMap<id,index>` side index** for `index_of_id`/`path_of_id`
     (rebuilt on element-count change + per-lookup id-verify self-heal;
     `reset_parent_elements`'s in-place `set_id` loop calls new
     `StructureDefinition::invalidate_id_index()`). O(n²) find_element_by_path →
     O(n). →1.64s.
  4. Hoist two per-element `format!("{x}.")` out of `.find()` loops in
     `instance_export` (`set_implied_properties_on_instance` +
     `set_assigned_values`) — the big mcode wins (13.5s→4.2s→2.8s).
  mcode is now lexer-bound (`lex::m_ref` ~26%, out of scope). NOT committed.

## 7b. Porting specs + cross-cutting parity traps

Full cited specs live in `docs/specs/` (read the relevant one before porting a
subsystem). Each cites `sushi-ts/...:line`. The traps most likely to break a
naive port (distilled from the specs):

- **STAR token swallows the preceding newline + indent** (`FSHLexer.g4:65`). A
  rule's `startLine = STAR.line + 1`; indentation is recovered from the STAR
  token text (`length - lastIndexOf('\n') - 2`), NOT parser columns. Get this
  wrong and every rule span + soft-index context is wrong. (spec 01)
- **Two span conventions coexist**: `extractStartStop` end-col is `+1`; the
  error listener end-col is `column + text.length` (no `+1`). Do not unify. (01)
- **Importer is two-pass + global**: pass 1 gathers aliases + parameterized
  RuleSets across ALL files; duplicate names are **first-wins** tank-wide. (01)
- **Parameterized RuleSets are substituted + re-parsed at PARSE time**, cached in
  `FSHDocument.appliedRuleSets` keyed by `JSON.stringify([name,...params])`;
  export-time `applyInsertRules` only looks them up. (08)
- **Numbers split types**: assignment/caret values use arbitrary-precision
  `bigint`; quantities use `parseFloat`. (01) Our oracle tags bigint as `{__bigint}`.
- **`id` is a recomputed getter** scanning rules with `findLast` (last-wins:
  `^id` CaretValueRule for SD/VS/CS, `id` AssignmentRule for Instance/Invariant);
  Mapping.id is a field. Don't snapshot id at construction. (02)
- **Type discriminant is `constructorName` string**, not instanceof. `InsertRule`
  is allowed nowhere (must be expanded); Logical/Resource allow AddElementRule but
  forbid ContainsRule. (02)
- **`FSHTank.fish` is order-sensitive AND mutating**: alias→split `|`→fixed
  10-type order→entities-before-instances→first-hit; `fishForMetadata` calls
  `applyInsertRules` (side effects). (02/08)
- **SD model is flat + string-id-driven**: `ElementDefinition.path` derived from
  `id`; tree links are lazy id-prefix caches; **lookup (`findElementByPath`)
  mutates the SD** (unfold/sliceMatchingValueX add elements, leave residue on
  failed deeper lookups). Snapshot+differential both derive from one `elements`
  array. (03)
- **Export order is load-bearing** (`FHIRExporter.export`): ALL `applyInsertRules`
  → SD → CS → VS → Instance → SD `applyDeferredRules` → Mapping last. Mappings
  mutate already-exported SDs; deferred caret rules run after instances. (04)
- **JSON has 3 ordering regimes**: SD/ElementDefinition use fixed `PROPS` arrays;
  InstanceDefinition = `resourceType,id,meta` prefix then JS insertion order;
  CS/VS their own. Never rely on map order. (05)
- **package layer = external `fhir-package-loader` v2** (npm). Its internal
  match/best-version predicate is the thing `package_store` must replicate;
  OPEN QUESTION — not fully verifiable from source alone. (06)
- **Diagnostics**: winston format order is ignore-check → count → footer(`File:`/
  `Line:`) → print; ignore-list matches the BARE message; ignored msgs don't
  count as error/warn. (07)

## 8. Hard rules (do not violate)

- No silent normalization of output diffs. Classify every diff.
- No "skip instances / skip QA" passing results.
- No unordered map iteration for JSON output — emission order is observable.
- No fallback path without metrics + a test proving it's unused or acceptable.
- Don't optimize before the global data shape is known.
- Keep `sushi-ts` pinned at v3.20.0 = the stock oracle version. If the oracle
  binary version changes, re-pin the submodule and re-record §4.
- **NEVER use the real `~/.fhir` cache or real `$HOME`.** Always isolate (§5).
  This applies to Rust code too: `package_store` must **require an explicit cache
  dir** and **fail loud** if it's missing — never silently default to `~/.fhir`.
  Defensive, fail-loud everywhere; never let defaults "slip" to real home.

# AGENTS.md — Operating Manual for the Rust SUSHI Port

> **Read this first every session.** It is the durable memory of how this port
> is built, what works, what the gotchas are, and where we are. Keep it updated
> as facts change — it must survive context compaction. When you discover a new
> command, gotcha, or finish a phase, edit this file in the same turn.

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
- Shared warm FHIR cache: `~/.fhir/packages` (~145 packages already present).
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

Phases from the plan (0–9). Current state:

- [x] **Scaffold** — workspace builds green, diagnostics + interner done, submodule pinned.
- [x] **Phase 0 — harness** — DONE: `run-stock.sh` (isolated cache), `diff-resources.sh`, `diag.cjs` (diagnostic normalize/diff), `lex-oracle.cjs` + `parse-oracle.cjs`, timing.json schema, IPS oracle. Remaining (deferred, not blocking): add SDC/CRD/US Core/mCODE/Cycle IGs when available locally (only IPS present now); full `compile` candidate wrapper grows with the compiler.
- [ ] **Phase 1 — package store + JSON emitter skeleton**
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
- [ ] **Phase 4 — ValueSet/CodeSystem export**
- [ ] **Phase 5 — SD arena + simple profiles**
- [ ] **Phase 6 — full SD compatibility**
- [ ] **Phase 7 — instance export + required QA**
- [ ] **Phase 8 — full corpus parity**
- [ ] **Phase 9 — optimization loop**

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

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

### Rust commands
```sh
cargo build --workspace
cargo test  --workspace
cargo test  -p <crate>                 # focused
cargo run   -p rust_sushi -- <args>
```

### Cache isolation policy
Default harness runs use the **shared warm `~/.fhir`** (matches the plan's primary
benchmark scenario — "packages already local and indexed"). Use `RUN_HOME=<dir>`
only when deliberately testing cold acquisition / reproducibility. Don't pollute
the shared cache with experiments.

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
- [~] **Phase 0 — harness** — `run-stock.sh` + `diff-resources.sh` done; IPS oracle captured. TODO: diagnostic-diff reporter, more corpus IGs (SDC/CRD/US Core/mCODE/Cycle), candidate-run wrapper.
- [ ] **Phase 1 — package store + JSON emitter skeleton**
- [ ] **Phase 2 — FSH parser + AST** (next big build)
- [ ] **Phase 3 — insert rules + tank indexes**
- [ ] **Phase 4 — ValueSet/CodeSystem export**
- [ ] **Phase 5 — SD arena + simple profiles**
- [ ] **Phase 6 — full SD compatibility**
- [ ] **Phase 7 — instance export + required QA**
- [ ] **Phase 8 — full corpus parity**
- [ ] **Phase 9 — optimization loop**

## 8. Hard rules (do not violate)

- No silent normalization of output diffs. Classify every diff.
- No "skip instances / skip QA" passing results.
- No unordered map iteration for JSON output — emission order is observable.
- No fallback path without metrics + a test proving it's unused or acceptable.
- Don't optimize before the global data shape is known.
- Keep `sushi-ts` pinned at v3.20.0 = the stock oracle version. If the oracle
  binary version changes, re-pin the submodule and re-record §4.

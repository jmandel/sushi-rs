# Perf-Enhancement Week — protocol (agents + curator)

A pool of subagents propose performance improvements, each in its OWN git
worktree. The curator (main session) evaluates and integrates selectively based on
measured gain vs maintenance cost. **Parity is sacred** — speed never trades correctness.

## Baseline (best-of-5 warm; curator re-baselines after each integration)
| IG | initial (8b7fb77) | after round-1 A (98762d8) |
|---|--:|--:|
| ips | 1.374 | 1.289 |
| epi | 1.056 | 0.983 |
| mcode | 1.898 | 1.881 |
| crd | 1.538 | **1.052** |

### Integration log
- **perf/A** (3 commits, cherry-picked → main 98762d8): package_store byte-scan name
  extractor (verified equiv over 281k cache files) — crd -32%; reuse `_`-sibling
  buffer in fhir_model hot loops (all IGs); move (not clone) index-entry fields.
  Parity 665/665, tests green.

## HARD GATES (every agent, every change — non-negotiable)
1. **665/665 byte parity** — all 4 IGs `diff-resources.sh` print PARITY:
   ```
   for ig in ips epi mcode crd; do
     FHIR_CACHE=/home/jmandel/hobby/sushi-rs/temp/fhir-home/.fhir/packages \
       target/release/rust_sushi build /home/jmandel/periodicity/temp/$ig-ig -o /tmp/wt-$ig
     bash harness/diff-resources.sh /home/jmandel/hobby/sushi-rs/temp/$ig-stock /tmp/wt-$ig
   done
   ```
2. **`cargo test --workspace` green** (18 suites).
3. If the lexer/parser is touched: **byte-exact vs the ANTLR oracle** (`diff <(node harness/lex-oracle.cjs f) <(rust_sushi lex f)` == empty) on the IPS+mCODE corpus.
4. **No output changes, no nondeterminism** — never iterate a HashMap where order drives emission; hashmaps for LOOKUP only.

A change that fails any gate is rejected outright — do not weaken a gate.

## Worktree mechanics (IMPORTANT — the worktree has no `temp/`)
`temp/` (the FHIR cache + stock oracles) is gitignored and NOT in your worktree.
Use the MAIN repo's copies by absolute path, read-only:
- FHIR cache: `export FHIR_CACHE=/home/jmandel/hobby/sushi-rs/temp/fhir-home/.fhir/packages` (NEVER ~/.fhir).
- Stock oracles: `/home/jmandel/hobby/sushi-rs/temp/<ig>-stock` (diff target).
- IG sources: `/home/jmandel/periodicity/temp/<ig>-ig`.
- Build candidate output to `/tmp/...` scratch.
- `node harness/...` oracles work from your worktree (harness/ is committed; SUSHI_ROOT is absolute).

## Timing method
Warm, **best-of-5** per IG (discard first if cold). Report seconds per IG vs the
baseline table. Focus on the slow IGs (mcode, crd). Profile to justify:
`perf record -g -F 2000 -o /tmp/p.data -- <build>; perf report -i /tmp/p.data --stdio | head -25`.

## Deliverable per agent (so the curator can evaluate + integrate)
1. **Commit your change in your worktree** on its branch (so the curator can
   `git diff`/cherry-pick it from the shared .git). Do NOT push or touch main.
2. Final report (concise): (a) one-line idea; (b) before→after best-of-5 per IG;
   (c) parity PASS + cargo test PASS (state explicitly); (d) files changed + net
   LOC; (e) perf-report top symbols before/after; (f) **maintenance assessment**
   (how invasive / risky / understandable is the change); (g) your branch name.
3. If an idea fails (no gain or breaks parity), report it as REJECTED with why —
   negative results are useful and prevent re-tries.

## Curator integration policy
Integrate when: measurable gain (≥~5% on a slow IG, or removes a clear hotspot)
AND maintenance cost is acceptable (localized, understandable, parity-robust).
Prefer small, clearly-correct changes. Reject high-churn/high-risk changes with
modest gains. Re-verify parity + timing in the main tree before committing.

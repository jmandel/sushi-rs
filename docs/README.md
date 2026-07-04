# docs/ — current-state map

What each doc is, and whether it drives current work or is history. Written
2026-07-03 (Consolidation Pass 1). When plans supersede each other, the newer one
wins and the older carries an inline SUPERSEDED marker.

## Start here (active plans + specs)

| Doc | Kind | State | What it governs |
|---|---|---|---|
| `hosting.md` | **GUIDE** | **ACTIVE** | How to host the engine: browser worker, Bun/Node custom generators, non-JS shell-to-fig, the envelope schema, the adapter contract, template-as-data. Every example is CI-executed (`scripts/examples-gate.sh` over `examples/`). |
| `unified-cli-plan.md` | **PLAN** | **SHIPPED** | The `fig` unified CLI (Consolidation Pass 2): one binary, subcommands = the Session op surface, `--json` = the shared envelope, render/watch/runner. Folds in snapshot_gen/site_db binaries. |
| `stock-template-renderer-plan.md` | **COMMITTED PLAN** | **ACTIVE** | The F0–F6 plan: in-browser Publisher-parity page rendering (xhtml/tables/sd/md/liquid/page substrates → editor integration at F6). The current center of gravity. |
| `fhir-ig-editor-spec.md` | SPEC | ACTIVE | The editor demo repo (`jmandel/fhir-ig-editor`) — M1/M2 milestones, worker protocol, UI scope. |
| `simplification-ledger.md` | LEDGER | ACTIVE | Complexity-by-accretion candidates + consolidation-pass results. Runs at phase boundaries with gate discipline. |
| `render-worklog.md` | WORKLOG | ACTIVE | Per-increment derivation log for the F2–F5 render port (byte-parity scorecards, quirk case-law). |

## Feasibility + design notes (reference; not schedules)

| Doc | State | Notes |
|---|---|---|
| `rust-fragment-generator-feasibility.md` | REFERENCE | Task #23 study the renderer plan operationalizes — read for the evidence behind F-phases. |
| `ig-jekyll-surface-survey.md` | REFERENCE | Empirical Jekyll/Liquid feature survey (the F1c cutline). |
| `publisher-fragments-notes.md` | REFERENCE | How the Java Publisher decides which xhtml fragments to emit (the F2–F4 spec source). |
| `cycle-package-db-plan.md` | REFERENCE | Gap analysis: Rust pipeline as cycle's package.db producer (site_db lineage). |
| `package-derived-index.md` | REFERENCE | Design note for the CAS derived-index (clarity pass #12b). |
| `layer-b-audit.md` | REFERENCE | Canonical version pinning + the R4-artifact projection (resolver lineage). |

## Superseded / historical

| Doc | State | Pointer |
|---|---|---|
| `wasm-editor-plan.md` | **PARTLY SUPERSEDED** | P0/P1/P2 DONE (keep for rationale + DONE evidence); P3/P4 → `stock-template-renderer-plan.md` F5/F6 + `fhir-ig-editor-spec.md`. |

## Performance dossier (Phase-9 perf week + ongoing)

| Doc | State | Notes |
|---|---|---|
| `perf-protocol.md` | HISTORICAL | The perf-week agent/curator protocol. |
| `perf-map.md` | HISTORICAL | Perf opportunity map by area (lane assignment). |
| `perf-notes.md` | HISTORICAL | Phase-9 perf findings (DONE 2026-06-30). |
| `perf31.md` | REFERENCE | The 31-IG two-phase performance harness writeup. |
| `perf-snapshot-gen.md` | REFERENCE | Perf log for the snapshot_gen walk engine (#12a); "future levers" still cited. |

## Corpus / validation findings (point-in-time, 2026-06-30)

Snapshots of parity-sweep results — evidence, not plans. `harvest-findings.md`,
`holdout-findings.md`, `mining-findings.md`, `top20-findings.md`,
`next20-findings.md`.

---

For scripts (gates, oracles, harvest, dashboards) see **`scripts/README.md`**.
For the snapshot walk engine's own notes see **`snapshot/AGENTS.md`** +
**`snapshot/REWORK-PLAN.md`**.

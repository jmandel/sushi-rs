# Diagnostics Parity Plan

Goal: emit FSH validation diagnostics (errors / warnings / info) that **match stock
SUSHI v3.20.0** — message text, severity, source location, ordering, and the
error/warn counts + summary box — using the same **oracle-driven, byte-parity**
methodology that got the resource output to 665/665.

## Why & scope
Diagnostics are a primary SUSHI product for IG authors. Surface in `sushi-ts`:
**~282 error/warn call sites** (183 `logger.error` + 99 `logger.warn`; +45 info,
+45 debug) and **55 typed `Error` classes**, concentrated in:
- `src/import` (81) — syntax (`FSHErrorListener`) + import-semantic (duplicate name,
  metadata, alias, missing `InstanceOf`, parameterized-RuleSet errors, soft-index).
- `src/export` (103) — VALIDATION (cardinality, binding, type/choice, slicing,
  required-element, reference, invariant, mapping...).
- `src/fhirtypes` (39), `src/utils` (25), `src/ig` (25), `src/fhirdefs` (3).

**We will NOT blindly port all 282.** We prioritize by what actually FIRES on real
IGs (frequency), backed by synthesized fixtures for correctness on each class.

## Methodology (same as the port)
Compatibility compiler: stock SUSHI wins. Every diagnostic is gated against a
golden captured from stock. Two complementary oracles:
1. **Per-fixture oracle** — a tiny FSH snippet / mini-IG that deliberately triggers
   ONE diagnostic; run stock, capture its normalized diagnostic(s) → golden. Proves
   we emit the exact text/severity/location for that class.
2. **Corpus oracle** — run stock on the 4 IGs + the holdout IGs, capture all
   diagnostics (`diag.cjs` already does this), histogram them. Drives prioritization
   and is the integration gate (zero unclassified diagnostic diffs).

Harness we already have: `diagnostics` crate (SourceSpan/Severity/DiagnosticCode/
DiagnosticSink w/ stable order — currently UNWIRED), and `harness/diag.cjs`
(normalize stock console log → ordered JSON; order-sensitive diff). Reused as-is.

## Phases

### D0 — Oracle + harness for diagnostics
- **`harness/diag-oracle.cjs`**: given a tiny FSH project dir (or a single `.fsh` +
  generated minimal `sushi-config.yaml`), run stock SUSHI's relevant phase and emit
  the normalized diagnostic list `[{level, code?, message, file(basename), startLine,
  endLine, appliedFile?, appliedLine?}]`. Two modes:
  - *import-only* (fast): drive `sushiImport.importText` + the importer's logger →
    covers all `src/import` diagnostics with no packages needed.
  - *full build*: `harness/run-stock.sh` + `diag.cjs normalize` → covers export/
    validation diagnostics (needs config + the isolated cache).
- **Rust side**: `rust_sushi build --diagnostics` prints diagnostics to stderr in
  the SAME winston format; a `--diagnostics-json` mode emits the normalized list for
  diffing. Gate: `harness/diff-diag.sh <stock> <rust>` (order-sensitive).

### D1 — Discovery: the diagnostic catalog (fan-out subagents, read-only)
Systematically extract, from `sushi-ts`, every error/warn into a catalog
`docs/specs/diagnostics-catalog.md`. Per entry: **id**, **severity**, **message
template** (with interpolation points), **trigger condition**, **source-location
rule** (which span, + the two span conventions), **phase** (import vs export), and
the `Error` class if any. Fan out by area (import / export-SD / export-instance /
export-VS-CS / fhirtypes / ig). Every entry cites `path:line`. Output is the spec
for D4 and the checklist for D2.
- Also capture the **logger format** exactly (`utils/FSHLogger`: ignore-list →
  count → `File:`/`Line:`/`Applied` footer → printer `<level> msg`; the summary box;
  the ignore-list semantics — see spec 07).

### D2 — Synthesize fixtures (the gate)
For each catalog class, author a minimal FSH fixture that triggers it; run the D0
oracle → commit `tests/fixtures/diag/<class>.fsh` (+ `sushi-config.yaml` if export-
phase) and golden `tests/goldens/diag/<class>.diag.json`. Prioritize by D5 corpus
frequency (do the classes that actually fire first; rare validation errors later).
Property/edge variants where useful (e.g. span conventions, ordering between two
diagnostics, ignore-list, applied-location for inserted rules).

### D3 — Architecture (wire it into our pipeline)
- Thread a single **`DiagnosticSink`** (from the `diagnostics` crate) through
  importer → insert-expansion → exporters, replacing the ad-hoc `Vec<String> diag`
  in `compiler/src/lib.rs` and the discarded wording in the exporters.
- **Attach `SourceSpan`** to every diagnostic (the parser already carries spans on
  AST nodes/rules; thread them). Honor BOTH span conventions (extractStartStop vs
  the error-listener `column+len`) and `appliedFile`/`appliedLocation` for inserted
  rules.
- **Ordering**: assign a monotonic order at the SAME logical points SUSHI emits
  (import order, then FHIRExporter phase order: invariants→SD→CS→VS→Instance→Mapping).
- **Emission**: a printer that reproduces winston format byte-for-byte + the
  error/warn counts + the summary box; an `--diagnostics-json` for the gate.

### D4 — Implement by category, gated incrementally (oracle loop per class)
Order by frequency + dependency:
- **D4a Parse/syntax** — port the `FSHErrorListener` substitution catalog (friendly
  messages + relocated spans) on top of our lexer/parser's existing error recovery.
- **D4b Import-semantic** — duplicate-name (first-wins), metadata-already-declared,
  alias errors/warnings, missing `InstanceOf`, usage defaults, parameterized-RuleSet
  count/unknown + the aggregated sub-parse message, soft-index warnings. (Much text
  is already collected; wire + span it.)
- **D4c Export/validation** — the big bucket. Each fires only when we run the
  corresponding CHECK during export; implementing the warning = implementing the
  check (cardinality, binding strength/type, type/choice, slicing/min-sum, required-
  element, reference target, invariant, mapping). Port check-by-check, gated by the
  D2 fixtures. NOTE: we already produce correct RESOURCES, so these checks are
  additive flags, not output changes — they must NOT alter the 665/665 resource
  parity (run the resource gate alongside).
- **D4d** counts, summary box, ignore-list, final ordering reconciliation.

### D5 — Corpus gate (the truth table)
Capture stock diagnostics across the 4 IGs + holdout IGs (`diag.cjs`), **histogram
the message classes** (drives D2/D4 priority), then gate: `diff-diag.sh` stock vs
rust = zero unclassified diffs. The holdout IGs (which emit REAL warnings, unlike
our clean 4) are the primary validation corpus — capture their `sushi-console.log`
now (we already do in `temp/holdout/*-stock`) and mine it.

## Risks / subtleties
- **Message text is the product** — must be byte-exact (interpolation, pluralization,
  EOL in aggregated messages).
- **Ordering** is subtle (emission points; idempotent fishing can reorder — see spec
  08 §fishForMetadata side effects).
- **Validation depth**: D4c means porting SUSHI's validation logic, not just text —
  the largest part; scope to what fires.
- **No resource regression**: diagnostics are additive; the 665/665 gate must stay
  green throughout (a wrong check could also change output).
- **Determinism**: stable ordering; never let HashMap order leak into emission.

## Sequencing recommendation
D0 + D1 (oracle + catalog) first — cheap, fan-out-able, and produces the spec +
priority list. Then D5 corpus histogram to rank. Then D2→D4 by frequency, parse-side
first (self-contained, no packages), export/validation second. Mirror the port's
loop: oracle → fixture gate → implement → corpus diff → classify any diff.

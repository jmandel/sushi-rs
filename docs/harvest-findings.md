# SUSHI-harvest regression corpus — findings (2026-06-30)

A permanent, oracle-backed regression suite harvested from **stock SUSHI v3.20.0's own
unit tests**. Each case is a self-contained FSH snippet lifted from a stock `it(...)`,
materialized as a minimal IG, and gated byte-for-byte against the **stock SUSHI CLI**
oracle. Lives in `tests/sushi-harvest/`; gated by `harness/harvest-gate.sh`.

## Corpus
**326 cases**, deduped, each producing the stock-CLI oracle in `expected/`.

| Source (stock test file)                       | cases |
|------------------------------------------------|------:|
| test/import/FSHImporter.ValueSet.test.ts       |    66 |
| test/import/FSHImporter.context.test.ts        |    48 |
| test/import/FSHImporter.Logical.test.ts        |    30 |
| test/import/FSHImporter.test.ts                |    28 |
| test/import/FSHImporter.Resource.test.ts       |    24 |
| test/import/FSHImporter.Extension.test.ts      |    23 |
| test/import/FSHImporter.CodeSystem.test.ts     |    22 |
| test/import/FSHImporter.Instance.test.ts       |    21 |
| test/import/FSHImporter.Alias.test.ts          |    20 |
| test/import/FSHErrorListener.test.ts           |    16 |
| test/import/FSHImporter.Profile.test.ts        |    15 |
| test/run/FshToFhir.test.ts                     |    12 |
| test/import/FSHImporter.ParamRuleSet.test.ts   |     1 |
| **TOTAL**                                       | **326** |

Harvesting (`harness/harvest-extract.cjs`): a tokenizer-aware scan of the stock test
sources extracts every substitution-free template literal that (a) is an argument to a
FSH helper (`leftAlign`/`fshToFhir`/`RawFSH`/`importSingleText`) **or** declares a FHIR
entity, applying `leftAlign()` only where the test does. Each snippet is keyed to its
enclosing `describe`/`it` (see `tests/sushi-harvest/manifest.json`). 17 literals using
`${}` substitutions (dynamic FSH) are not statically harvestable and were skipped.

### Out of scope
`sushi-ts/test/export/*.test.ts` (**11 files, ~1138 `it` cases**) build entities
**programmatically** (`new Profile(...)`, `.rules.push(...)`) with **no FSH text** — they
cannot be harvested without a builder→FSH transpiler. Noted for a future effort; that is
where the deepest SD/Instance export behavior lives.

## Oracle = stock CLI, not `fshToFhir`
The port is a **CLI** whose contract is byte-identical output to stock `sushi build`.
`fshToFhir()` is *not* that contract: it bakes `config.status = undefined` (so SDs omit
`status`) and `version = undefined`, whereas the stock CLI defaults FSHOnly `status` to
`'draft'` (`importConfiguration.ts:132-134`) and honors the yaml `version`. Using
`fshToFhir` as the oracle would flag `status`/`version` on **every** case as false
divergences. The oracle is therefore the **stock CLI** (`harness/harvest-oracle.sh`,
parallel `app.js build`, FSHOnly, isolated `$HOME`) run with the same minimal
`sushi-config.yaml` the port reads (`canonical http://example.org`, `status active`,
`version 0.1.0`, `fhirVersion 4.0.1`, `FSHOnly true`). The `fshToFhir` export pipeline is
identical to the CLI's *underneath* this config layer.

## Parity (current)
```
cases:           326   (clean 237, diverging 89)
resources match: 229
resource DIFF:    23   resource MISS: 4   resource EXTRA: 67
byte-identical:  229 / 256  = 89.5%   (resources the oracle emits)
case parity:     237 / 326  = 72.7%
```
Case parity is dragged down almost entirely by the **leniency** class below, which fires
only on **intentionally-invalid** FSH (these import tests exist to exercise stock's error
reporting). On valid input the port is essentially at parity.

## Pilot bugs now FIXED (confirmed by this corpus)
- **N7 — `FSHOnly` not honored (was SYSTEMIC).** FIXED. Across all 326 cases the port
  emits **zero** `ImplementationGuide` resources (no IG EXTRA anywhere). This alone
  was the pilot's biggest swamp.
- **N1 — Quantity/Ratio literal → `pattern[x]`.** FIXED. All 6 corpus cases containing
  `Quantity`/`Ratio` are byte-clean (the only one that diverges, `misc-025`, does so
  purely on N2 decimal formatting, not on the Quantity structure).

(N3 — `Canonical(localName)` — is **not exercised** by this corpus, so unconfirmed here.)

## Remaining divergences, by root cause (prioritized by count)

### 1. L1 — leniency: port emits where stock rejects (≈72 cases) — LARGEST
Stock's exporter errors on invalid/unresolved input and **skips** the resource; the port
accepts it and emits. Fires only on invalid FSH (low real-IG impact), but it is the bulk
of the gap. Sub-cases:
- **Unresolved code system in a ValueSet** (`* ZOO#hippo …`, `from system $LOINCZ`): ~55
  cases (most `valueset-*`, `context-04x`, `alias-012/013`). Stock fails to fish the
  system (`ValueSetExporter.ts:83` `fishForMetadata(...Type.CodeSystem)`; on failure the
  rule throws and the whole VS is skipped, `ValueSetExporter.ts:404,433`); the port emits
  the VS with the bare system string.
- **Binding to an unresolved value-set name** (`* code from LAINC`): `alias-004/005/006/
  007/015/016` (6). Stock drops the binding (element stays bare); the port emits
  `binding.valueSet: "LAINC"`.
- **Invalid code/system literals**: `misc-003` (system with `\` →
  `https://breakfast.com/good\\food`), `misc-008` (`#" Leading whitespace…"`). Stock
  emits `null`/bare element; the port emits a populated `patternCodeableConcept`.
- **Reserved-keyword element names**: `logical-027` (`* SU …`, `* D …`). Stock drops
  elements whose names collide with flag keywords (`SU`, `D`); the port emits them.

### 2. P — parser divergences: port emits LESS / mis-parses where stock is lenient (7)
- **Caret value with no space before `=`** (`* component ^short ="Component1"`):
  `errorlistener-012/013`. Stock parses the value as the literal string `="Component1"`
  and applies the rule; the port drops the rule entirely. (Matches the pilot's
  `diverge.json` s011/s012.)
- **Indented (non-`leftAlign`) rule blocks**: `logical-025/028/029/030` — snippets the
  stock test fed verbatim with 8-space indentation. Stock and the port resolve the
  indentation-driven rule context differently (port adds `short`/`definition`/
  `mustSupport`/extra elements stock does not). Edge: real IGs are not uniformly indented.
- **Dropped nested caret rule**: `context-027` — under `* . 0..1`, stock keeps both
  `^short` and `^definition`; the port keeps `^short` but drops `^definition`.

### 3. N4 — empty `compose` serialized as `{include:[]}` instead of `null` (2)
`valueset-013/015`. When every include errors out, stock emits `"compose": null`
(`ValueSet.toJSON` → `orderedCloneDeep`, `fhirtypes/ValueSet.ts:74`); the port emits
`"compose": {"include": []}`. KNOWN (pilot N4), still present.

### 4. N5 — id / filename sanitization (4 resources / 3 cases)
`valueset-001` (`Id: SimpleVS_ID` → stock id+file `SimpleVS-ID`, port keeps underscore),
`valueset-003`, `misc-012` (colon-bearing ids → stock filesystem-safe
`StructureDefinition-2000-10-31T00-01-02.json`, port keeps `:`). Stock applies
`sanitize(..., {replacement:'-'})` in `getFileName()` and to the id; the port does not.
KNOWN (pilot N5), still present.

### 5. N2 — decimal serialization (2)
`misc-025` (`0.00000155` stock vs `1.55e-6` port), `codesystem-020`
(`valueInteger = 0.4500` → stock nulls the invalid-integer value; port emits `0.45`).
Stock round-trips decimals through a JS `Number`; the port preserves/Rust-formats the
lexeme. KNOWN (pilot N2), still present.

### 6. N6 — `\uXXXX` surrogate-pair escapes left literal (1)
`misc-004`: `🀱` → stock decodes to `🀱`; the port leaves the literal
`🀱`. KNOWN (pilot N6), still present.

## Reproduce / regate
```
cargo build --release -q
# regenerate oracle (slow; only on demand; needs network into an ISOLATED HOME):
ISO_HOME=$PWD/temp/harvest-home harness/harvest-oracle.sh tests/sushi-harvest
# gate (fast, re-runnable, never touches real ~/.fhir):
harness/harvest-gate.sh                 # all cases
harness/harvest-gate.sh valueset-013    # one case
```
Re-harvest from the stock sources: `harness/harvest-extract.cjs` →
`harness/harvest-materialize.cjs`.

## Safety
Every oracle/gate run uses an isolated `$HOME` (`temp/harvest-home`); the real `~/.fhir`
is asserted untouched before and after (guards in `harness/_guard.sh` +
`assert_real_fhir_untouched`). Confirmed clean for this session.

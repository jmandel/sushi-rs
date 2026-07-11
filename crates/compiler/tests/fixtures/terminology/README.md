# Tier-1 terminology evaluator — oracle fixtures & goldens

These fixtures drive `crates/compiler/tests/oracle_tx.rs`, the ORACLE gate for
`compiler::terminology::expand_enumerable`.

## What's here

- `*.vs.json` — the ValueSet resource under test (the compose to expand).
- `*.cs.json` — a CodeSystem the evaluator resolves locally (and that we hand to
  tx.fhir.org via the `tx-resource` expand parameter so BOTH expanders see the
  same complete content for the synthetic/local cases).
- `../goldens/terminology/<name>.golden.json` — the tx.fhir.org `$expand`
  response, **committed** with its `expansion.parameter`, including the
  code-system versions the server used. These are the AUTHORITY the evaluator is gated
  against.

## Provenance (per §3 cache discipline)

- **Server:** `https://tx.fhir.org/r4` (`ValueSet/$expand`, POST).
- **Fetched:** 2026-07-03 (one deliberate request per fixture).
- Refresh is a deliberate maintainer action: re-run
  `scripts/refresh-terminology-goldens.py` and commit the new goldens. CI never
  calls a tx server.

## Normalization (explicit — see oracle_tx.rs)

tx.fhir.org and the tier-1 evaluator agree on the DOMAIN (which `(system, code)`
pairs are members) but differ in two observable ways, normalized EXPLICITLY,
never silently:

1. **Ordering.** tx returns authored/insertion order per include; the evaluator
   returns a stable `(system, code)` sort. The gate sorts BOTH sides by
   `(system, code)` before comparing membership.
2. **Displays.** For external systems (SNOMED), tx substitutes the server's
   canonical display for the code; the evaluator passes the *authored* display
   through (tier-1 has no external CS to look displays up in). The gate compares
   the **code set** exactly and reports display equality separately as an
   informational classification (a display diff on an external system is
   EXPECTED and does not fail the gate; a code-set diff DOES).

For local/synthetic CodeSystems supplied to both sides, displays DO match and
are compared exactly.

# SUSHI-test mining findings (pilot, 2026-06-30)

Extracted 349 self-contained FSH snippets from `sushi-ts/test/import/*` +
`test/run/FshToFhir.test.ts`, built stock-vs-port. **273 built on stock; 50 diverged
(18.3%); 0 panics.** Fixtures in `temp/sushi-tests/` (gitignored). These are NEW bugs
in constructs the 4-IG + 8-holdout corpora never exercised — high yield; worth scaling.

## True output bugs (fix these)
- **N1 — Quantity/Ratio literal -> `pattern[x]` in a profile (11 snippets, biggest).**
  `* valueQuantity = 1.5 'mm'` / `* valueRatio = 130 'mg':1 'dL'`. (a) patternQuantity
  key order wrong (rust {value,system,code} vs stock {value,code,system}); (b)
  `valueRatio` literal -> rust emits NOTHING (empty type array); (c) value-less ratio
  mis-typed to patternQuantity, denominator lost. Root: SD-export assignValue for
  FshQuantity/FshRatio -> pattern[x] incompletely ported (corpus only made
  patternQuantity from a FshCode).
- **N2 — decimal/number serialization.** `155e-8`->stock `0.00000155` (rust `1.55e-6`);
  `155.0`->stock `155` (rust keeps trailing zero). Stock round-trips through JS Number;
  we preserve the FSH lexeme. Affects SD + instance.
- **N3 — `Canonical(localName)` on a uri/canonical element (5) [≈G2].** Stock resolves
  the local name to its url + emits patternUri; rust emits patternCanonical:"<bare>".
- **N4 — empty value not omitted.** `version:""` kept; empty `compose` -> `{include:[]}`
  vs stock `null`.
- **N5 — id/filename sanitization.** `Id: SimpleVS_ID` -> stock id `SimpleVS-ID`
  (underscore->hyphen); colon-bearing id -> stock filesystem-safe filename.
- **N6 — `\uXXXX`/surrogate-pair escapes** left literal (edge).
- **N7 — `FSHOnly: true` not honored for ImplementationGuide (SYSTEMIC).** Stock emits
  0 IG resources under FSHOnly; rust emits an ImplementationGuide for every IG.
  `ig_export` runs unconditionally. (Excluded from the 18.3% to avoid swamping.)

## Leniency class (L1, 27 snippets) — policy decision
Rust ACCEPTS invalid FSH that stock REJECTS+skips: invalid URIs in binding/assignment,
invalid AddElement types, malformed AddElementRule, flags on logical elements. As a
compatibility compiler we should reject identically — but only fires on INVALID input
(real IGs are clean), so low real-IG impact. Tie to the diagnostics effort.

## Recommendation
Fix N1-N5+N7 (isolated, high-confidence, oracles already in temp/sushi-tests). Then
mine the EXPORT test suite (InstanceExporter/StructureDefinitionExporter build entities
programmatically — needs a builder->FSH transpiler or driving stock's fshToFhir API);
that's where the deep instance/SD behavior lives.

//! FSH->FHIR compiler: tank indexes, insert-rule expansion, fishing, export.
//! Phase 3 starts here (insert expansion + FSHTank). See docs/specs/08-insert-rules-tank.md.

/// Import FSH files, build the FSHTank, run `applyInsertRules` on every entity in
/// FHIRExporter order (invariants, SDs, codeSystems, valueSets, instances,
/// mappings), and serialize the POST-EXPANSION import AST to the oracle JSON
/// shape (matching `harness/expand-oracle.cjs`, incl. appliedFile/appliedLocation
/// on inserted rules). Gated by `tests/expand_parity.rs`.
///
/// IMPLEMENTATION PENDING — port FSHTank + applyInsertRules + soft indexing.
pub fn expand_to_json(_files: &[(&str, &str)]) -> serde_json::Value {
    todo!("port FSHTank + applyInsertRules (docs/specs/08) — make tests/expand_parity.rs green")
}

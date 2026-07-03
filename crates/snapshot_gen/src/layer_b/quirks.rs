//! Layer-B quirk registry — case-law behaviors that are NOT clean principled
//! rules but are demanded by the Java oracle. Every entry cites Publisher /
//! fhir-core `file:line` (fhir-core `6.9.10-SNAPSHOT`, checkout
//! `/home/jmandel/hobby/fhir-perf/repos/fhir-core`) and names the fixture that
//! demands it. This mirrors the Layer-A discipline (REWORK-PLAN §6): a quirk
//! without a Java citation is debt, never silent.
//!
//! The registry EXISTS from day one (task #17, Layer B B0+B1) even though the
//! cycle corpus only exercises a subset — the discipline is the point.
//!
//! These are DATA, not behavior: the pin/project code paths reference the same
//! facts inline with the citation, and [`REGISTRY`] is the auditable index a
//! coordinator (or a future `quirk-audit` gate) reads. `layer_b_quirks()` is
//! covered by a unit test so the registry cannot silently empty out.

/// One quirk-registry entry.
#[derive(Debug, Clone, Copy)]
pub struct Quirk {
    /// Stable id used in tests / audits.
    pub id: &'static str,
    /// Which Layer-B stage owns it.
    pub stage: QuirkStage,
    /// Java `file:line` citation (fhir-core 6.9.10-SNAPSHOT).
    pub citation: &'static str,
    /// The fixture / measurement that demands it.
    pub fixture: &'static str,
    /// Principled vs case-law classification (audit §5).
    pub kind: QuirkKind,
    /// One-line description of the behavior.
    pub note: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuirkStage {
    /// B1 — CoreVersionPinner (mechanism A).
    Pin,
    /// B0 — R4-artifact projection.
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuirkKind {
    /// Deterministic, keyed on a clean rule; safe to implement as a rule.
    Principled,
    /// Real behavioral asymmetry / carve-out; only against the oracle.
    CaseLaw,
}

/// The Layer-B quirk registry. Audited by `layer_b_quirks_registry_is_populated`.
pub const REGISTRY: &[Quirk] = &[
    Quirk {
        id: "pin.tho-asymmetry",
        stage: QuirkStage::Pin,
        citation: "CoreVersionPinner.java:87 (pinCoreVersionVS) & :78 (pinCoreVersionCS) \
                   contain `!contains(\"terminology.hl7.org\")`; :97 (pinCoreVersionSD) does NOT",
        fixture: "cycle period-tracking-fact (THO SDs would pin; THO VS/CS never do)",
        kind: QuirkKind::CaseLaw,
        note: "THO carve-out asymmetry: ValueSet/CodeSystem canonicals containing \
               terminology.hl7.org are skipped by the pinner, but StructureDefinition \
               canonicals are NOT — a real, non-obvious asymmetry.",
    },
    Quirk {
        id: "pin.walk-everything-gate-on-resolution",
        stage: QuirkStage::Pin,
        citation: "CoreVersionPinner.java:31-38 walks baseDefinition + BOTH differential \
                   AND snapshot; :100-105 gate on `sd.hasVersion()` after fetch",
        fixture: "cycle period-tracking-fact (0 pins in differential, 45 in snapshot)",
        kind: QuirkKind::CaseLaw,
        note: "Whole-resource traversal with selective effect: the pinner visits the \
               differential too, but authored differential canonicals come out clean \
               because they don't resolve to a versioned in-context resource — NOT \
               because differential is skipped. We mirror `walk everything, gate on \
               resolution` rather than `pin snapshot only`.",
    },
    Quirk {
        id: "pin.value-alternatives-thoroughness",
        stage: QuirkStage::Pin,
        citation: "CoreVersionPinner.java:52-54 pins ed.valueAlternatives via \
                   pinCoreVersionSD `for thoroughness` though R5-only and unused in core",
        fixture: "(none in cycle) — R5-only ED field",
        kind: QuirkKind::CaseLaw,
        note: "valueAlternatives[] is pinned as SD canonicals though it is R5-only and \
               unused in core. Low-value but reproduced for isomorphism.",
    },
    Quirk {
        id: "project.xpath-restore",
        stage: QuirkStage::Project,
        citation: "ElementDefinition40_50.java:567-568 \
                   `tgt.setXpath(readStringExtension(src, EXT_XPATH_CONSTRAINT))` \
                   (R5->R4); R5 ED has no xpath (r5/model/ElementDefinition.java)",
        fixture: "cycle period-tracking-fact (59 constraint.xpath in the stored snapshot)",
        kind: QuirkKind::Principled,
        note: "constraint.xpath is an R4-only field the R5->R4 downconvert re-emits from \
               the carried EXT_XPATH_CONSTRAINT extension. Present in package.db ONLY \
               because the IG is R4; version-conditional.",
    },
    Quirk {
        id: "project.r4-key-order",
        stage: QuirkStage::Project,
        citation: "r4/model/ElementDefinition.java @Child order 0-33 + \
                   r4/model/StructureDefinition.java @Child order (R4 JsonParser \
                   emission order); PublisherBase.java:427 re-parses R4 JSON",
        fixture: "cycle period-tracking-fact (walk emits mustSupport/mapping in R5-ish \
                  positions; the stored R4 blob orders ED keys 0-33)",
        kind: QuirkKind::Principled,
        note: "The R5->R4 model downconvert + R4 JsonParser re-serialization normalizes \
               every ElementDefinition's key order to the R4 @Child order (e.g. \
               mustSupport before isModifier; mapping last; fixed/pattern before \
               constraint). The native-R5 walk emits in walk/merge order, so projection \
               must re-sort keys into R4 order.",
    },
];

/// Accessor used by the audit test and any future `quirk-audit` gate.
pub fn layer_b_quirks() -> &'static [Quirk] {
    REGISTRY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_b_quirks_registry_is_populated() {
        // The registry exists from day one; every entry carries a citation + fixture.
        assert!(
            !REGISTRY.is_empty(),
            "Layer-B quirk registry must not be empty"
        );
        for q in REGISTRY {
            assert!(!q.citation.is_empty(), "quirk {} needs a citation", q.id);
            assert!(!q.fixture.is_empty(), "quirk {} needs a fixture", q.id);
            assert!(!q.note.is_empty(), "quirk {} needs a note", q.id);
        }
        // The THO asymmetry is the headline case-law entry; assert it is present.
        assert!(
            REGISTRY.iter().any(|q| q.id == "pin.tho-asymmetry"),
            "the THO SD-vs-VS/CS asymmetry must be registered"
        );
    }
}

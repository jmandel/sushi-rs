//! Type-code -> spec URL resolution, reproducing the publisher's
//! `context.getPkp().getLinkFor(corePath, code)` for the FHIR core types.
//!
//! For an R4-context IG (us-core, plan-net) the core types resolve to their
//! `hl7.fhir.r4.core` webPaths, which the publisher's package loader assigns
//! from the R4 spec map. Those follow a deterministic page layout:
//!   - primitives + complex datatypes -> `<spec>/datatypes.html#<code>`
//!   - resources                       -> `<spec>/<resource-lowercase>.html`
//!   - `Extension` (the base type)     -> `<spec>/extensibility.html#Extension`
//! where `<spec>` for R4 core is `http://hl7.org/fhir/R4`.
//!
//! This is core-spec *data* (the R4 page structure), not IG-specific behavior.
//! It is validated against the golden type cells. When the fork's getLinkFor
//! spec lands, any deviation is reconciled here with a citation.

/// The R4 core spec web root, as the publisher's SpecMapManager assigns for
/// hl7.fhir.r4.core (verified against golden type links).
pub const R4_SPEC: &str = "http://hl7.org/fhir/R4";

/// The FHIR R4 complex (non-primitive) datatypes that live on datatypes.html.
fn is_r4_complex_datatype(code: &str) -> bool {
    matches!(
        code,
        "Address"
            | "Age"
            | "Annotation"
            | "Attachment"
            | "BackboneElement"
            | "CodeableConcept"
            | "Coding"
            | "ContactDetail"
            | "ContactPoint"
            | "Contributor"
            | "Count"
            | "DataRequirement"
            | "Distance"
            | "Dosage"
            | "Duration"
            | "Element"
            | "ElementDefinition"
            | "Expression"
            | "Extension"
            | "HumanName"
            | "Identifier"
            | "Meta"
            | "Money"
            | "Narrative"
            | "ParameterDefinition"
            | "Period"
            | "Quantity"
            | "Range"
            | "Ratio"
            | "Reference"
            | "RelatedArtifact"
            | "SampledData"
            | "Signature"
            | "SimpleQuantity"
            | "Timing"
            | "TriggerDefinition"
            | "UsageContext"
    )
}

/// Reproduce `getLinkFor(corePath, code)` for a type code in an R4 IG.
/// Returns the href, or None (the type has no link -> plain text).
pub fn link_for_r4(code: &str) -> Option<String> {
    if code.is_empty() {
        return None;
    }
    // FHIRPath System.* primitives (used for element.id / url) get NO link in
    // the type cell (they don't appear as a type link in the grid). But when a
    // System.String slips through, resolve to the abstract System page — the
    // grid never renders these, so returning None is safe.
    if code.starts_with("http://hl7.org/fhirpath/System.") {
        return None;
    }
    // getOverride table (IGKnowledgeProvider.loadSpecPaths, 9 entries): these
    // take precedence over the spec.internals paths map.
    if let Some(page) = override_page(code) {
        return Some(format!("{}/{}", R4_SPEC, page));
    }
    if crate::sdmodel::is_fhir_primitive(code) {
        return Some(format!("{}/datatypes.html#{}", R4_SPEC, code));
    }
    if is_r4_complex_datatype(code) {
        return Some(format!("{}/datatypes.html#{}", R4_SPEC, code));
    }
    // Otherwise treat as a resource -> <resource-lowercase>.html.
    // (Resource names are UpperCamel; the page is the all-lowercase name.)
    Some(format!("{}/{}.html", R4_SPEC, code.to_lowercase()))
}

/// The `getOverride` table (IGKnowledgeProvider): canonical name -> page#anchor.
fn override_page(code: &str) -> Option<&'static str> {
    Some(match code {
        "Reference" => "references.html#Reference",
        "Extension" => "extensibility.html#Extension",
        "DataRequirement" => "metadatatypes.html#DataRequirement",
        "ContactDetail" => "metadatatypes.html#ContactDetail",
        "Contributor" => "metadatatypes.html#Contributor",
        "ParameterDefinition" => "metadatatypes.html#ParameterDefinition",
        "RelatedArtifact" => "metadatatypes.html#RelatedArtifact",
        "TriggerDefinition" => "metadatatypes.html#TriggerDefinition",
        "UsageContext" => "metadatatypes.html#UsageContext",
        _ => return None,
    })
}

/// The root-base link (SDR:2344): for a constraint on Extension the base SD is
/// the core Extension, webPath `extensibility.html#Extension`, name "Extension".
pub fn base_type_link_r4(name: &str) -> Option<String> {
    if name == "Extension" {
        return Some(format!("{}/extensibility.html#Extension", R4_SPEC));
    }
    // Other base types resolve as their datatype/resource link.
    link_for_r4(name)
}

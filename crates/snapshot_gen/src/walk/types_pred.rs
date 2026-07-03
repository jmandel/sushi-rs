//! Type predicates (isDataType, isPrimitive, isBaseResource, isExtension,
//! working_code) — consult the SD store where available, else the hard-coded
//! fallback lists Java uses when SDs aren't loaded (PU:3619/3646).

use serde_json::Value;

use super::context::WalkContext;
use super::resolve::fetch_sd;

const DATA_TYPES: &[&str] = &[
    "Address", "Age", "Annotation", "Attachment", "CodeableConcept", "Coding", "ContactPoint",
    "Count", "Distance", "Duration", "HumanName", "Identifier", "Money", "Period", "Quantity",
    "Range", "Ratio", "Reference", "SampledData", "Signature", "Timing", "ContactDetail",
    "Contributor", "DataRequirement", "Expression", "ParameterDefinition", "RelatedArtifact",
    "TriggerDefinition", "UsageContext", "CodeableReference",
];

const PRIMITIVES: &[&str] = &[
    "base64Binary", "boolean", "canonical", "code", "date", "dateTime", "decimal", "id", "instant",
    "integer", "integer64", "markdown", "oid", "positiveInt", "string", "time", "unsignedInt",
    "uri", "url", "uuid",
];

/// ElementDefinition.TypeRefComponent.getWorkingCode(): the fhir-type extension
/// valueUrl overrides `code` (used for `id`/System.String etc).
pub(crate) fn working_code(type_ref: &Value) -> Option<String> {
    if let Some(exts) = type_ref.get("extension").and_then(Value::as_array) {
        for ext in exts {
            if ext.get("url").and_then(Value::as_str)
                == Some("http://hl7.org/fhir/StructureDefinition/structuredefinition-fhir-type")
            {
                if let Some(v) = ext.get("valueUrl").and_then(Value::as_str) {
                    return Some(v.to_string());
                }
            }
        }
    }
    type_ref.get("code").and_then(Value::as_str).map(str::to_string)
}

/// The list of working codes on an element.
pub(crate) fn type_codes(element: &Value) -> Vec<String> {
    element
        .get("type")
        .and_then(Value::as_array)
        .map(|types| types.iter().filter_map(working_code).collect())
        .unwrap_or_default()
}

fn sd_kind_derivation(ctx: &WalkContext, name: &str) -> Option<(String, String)> {
    let sd = fetch_sd(ctx.pkg, name)?;
    let kind = sd.get("kind").and_then(Value::as_str)?.to_string();
    let derivation = sd
        .get("derivation")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Some((kind, derivation))
}

pub(crate) fn is_data_type_str(ctx: &WalkContext, value: &str) -> bool {
    match sd_kind_derivation(ctx, value) {
        Some((kind, derivation)) => kind == "complex-type" && derivation == "specialization",
        None => DATA_TYPES.contains(&value),
    }
}

pub(crate) fn is_primitive_str(ctx: &WalkContext, value: &str) -> bool {
    match sd_kind_derivation(ctx, value) {
        Some((kind, _)) => kind == "primitive-type",
        None => PRIMITIVES.contains(&value),
    }
}

/// PU:2137 isDataType(List).
pub(crate) fn is_data_type(ctx: &WalkContext, element: &Value) -> bool {
    let codes = type_codes(element);
    if codes.is_empty() {
        return false;
    }
    codes
        .iter()
        .all(|t| is_data_type_str(ctx, t) || is_primitive_str(ctx, t))
}

/// PU:1715 isBaseResource.
pub(crate) fn is_base_resource(element: &Value) -> bool {
    let codes = type_codes(element);
    if codes.is_empty() {
        return false;
    }
    !codes.iter().any(|t| t == "Resource")
}

/// PU:2436 isExtension (by path).
pub(crate) fn is_extension_path(path: &str) -> bool {
    path.ends_with(".extension") || path.ends_with(".modifierExtension")
}

pub(crate) fn has_content_reference(element: &Value) -> bool {
    element.get("contentReference").is_some()
}

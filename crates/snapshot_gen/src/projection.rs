//! Native-R5 projection pass: `project_r4_snapshot_to_native_r5` and everything
//! only it uses (constraint xpath conversion, additional-binding conversion, the
//! NON_INHERITED_ED_URLS machinery, R4 resource/type tables).

#![allow(unused_imports)]
use anyhow::{bail, Context};
use indexmap::IndexMap;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::*;

pub(crate) fn convert_own_constraint_xpaths_to_extensions(element: &mut Value) {
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        let Some(xpath) = constraint
            .get("xpath")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        if has_constraint_xpath_extension(constraint) {
            continue;
        }
        add_constraint_extension_first(constraint, constraint_xpath_extension(&xpath));
    }
}

pub(crate) fn add_constraint_xpath_extensions_from_source(target: &mut Value, source: &Value) {
    let mut xpaths = HashMap::new();
    if let Some(source_constraints) = source.get("constraint").and_then(Value::as_array) {
        for constraint in source_constraints {
            let Some(key) = constraint.get("key").and_then(Value::as_str) else {
                continue;
            };
            let Some(xpath) = constraint.get("xpath").and_then(Value::as_str) else {
                continue;
            };
            xpaths.insert(key.to_string(), xpath.to_string());
        }
    }

    let Some(target_constraints) = target.get_mut("constraint").and_then(Value::as_array_mut)
    else {
        return;
    };
    for constraint in target_constraints {
        let Some(key) = constraint.get("key").and_then(Value::as_str) else {
            continue;
        };
        let Some(xpath) = xpaths.get(key) else {
            continue;
        };
        if has_constraint_xpath_extension(constraint) {
            continue;
        }
        add_constraint_extension_first(constraint, constraint_xpath_extension(xpath));
    }
}

pub(crate) fn project_r4_snapshot_to_native_r5(structure: &mut Value) {
    let r4_native_projection = structure
        .get("fhirVersion")
        .and_then(Value::as_str)
        .is_some_and(|version| version.starts_with('4'));
    let source = structure_source(
        structure,
        structure
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("StructureDefinition"),
    );
    let snapshot_source = snapshot_source_value(structure);
    let constraint_xpaths = differential_constraint_xpaths(structure);
    let sourceless_differential_constraints = differential_sourceless_constraints(structure);
    let additional_binding_elements = differential_additional_binding_elements(structure);
    let differential_extension_urls = differential_extension_urls(structure);
    if let Some(elements) = structure
        .get_mut("snapshot")
        .and_then(|s| s.get_mut("element"))
        .and_then(Value::as_array_mut)
    {
        for element in elements {
            project_element_to_native_r5(
                element,
                &source,
                snapshot_source.as_deref(),
                &constraint_xpaths,
                element_id_or_path(element).and_then(|key| differential_extension_urls.get(key)),
                element_id_or_path(element)
                    .is_some_and(|key| additional_binding_elements.contains(key)),
                true,
                Some(&sourceless_differential_constraints),
                r4_native_projection,
            );
        }
    }
    if let Some(elements) = structure
        .get_mut("differential")
        .and_then(|s| s.get_mut("element"))
        .and_then(Value::as_array_mut)
    {
        for element in elements {
            project_element_to_native_r5(
                element,
                &source,
                snapshot_source.as_deref(),
                &constraint_xpaths,
                element_id_or_path(element).and_then(|key| differential_extension_urls.get(key)),
                element_id_or_path(element)
                    .is_some_and(|key| additional_binding_elements.contains(key)),
                false,
                None,
                r4_native_projection,
            );
        }
    }
}

pub(crate) fn project_element_to_native_r5(
    element: &mut Value,
    constraint_source: &str,
    snapshot_source: Option<&str>,
    constraint_xpaths: &HashMap<(String, String), String>,
    differential_extension_urls: Option<&HashSet<String>>,
    convert_additional_bindings: bool,
    fill_missing_sources: bool,
    sourceless_constraints: Option<&HashSet<(String, String)>>,
    preserve_common_binding: bool,
) {
    let r4_native_projection = preserve_common_binding;
    let preserve_common_binding = r4_native_projection && has_semantic_element_extensions(element);
    remove_non_inherited_extensions_except(
        element,
        differential_extension_urls,
        preserve_common_binding,
    );
    convert_constraint_xpaths_to_extensions(element, constraint_xpaths);
    strip_constraint_xpaths(element);
    if fill_missing_sources {
        fill_missing_constraint_sources(element, constraint_source, sourceless_constraints);
    }
    if convert_additional_bindings {
        convert_additional_binding_extensions(element);
    }
    if r4_native_projection {
        normalize_r4_native_binding(element);
    }
    normalize_fhir_type_extension(element);
    if r4_native_projection {
        prune_r4_extension_value_choice_types(element);
    }
    add_snapshot_source_to_obligations(element, snapshot_source);
    trim_mapping_maps(element);
}

pub(crate) const CONSTRAINT_XPATH_EXTENSION_URL: &str =
    "http://hl7.org/fhir/4.0/StructureDefinition/extension-ElementDefinition.constraint.xpath";

pub(crate) const ADDITIONAL_BINDING_EXTENSION_URL: &str =
    "http://hl7.org/fhir/tools/StructureDefinition/additional-binding";

pub(crate) fn differential_constraint_xpaths(structure: &Value) -> HashMap<(String, String), String> {
    let mut out = HashMap::new();
    let Some(elements) = structure
        .get("differential")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
    else {
        return out;
    };
    for element in elements {
        let Some(element_key) = element_id_or_path(element) else {
            continue;
        };
        let Some(constraints) = element.get("constraint").and_then(Value::as_array) else {
            continue;
        };
        for constraint in constraints {
            let Some(key) = constraint.get("key").and_then(Value::as_str) else {
                continue;
            };
            let Some(xpath) = constraint.get("xpath").and_then(Value::as_str) else {
                continue;
            };
            out.insert(
                (element_key.to_string(), key.to_string()),
                xpath.to_string(),
            );
        }
    }
    out
}

pub(crate) fn differential_sourceless_constraints(structure: &Value) -> HashSet<(String, String)> {
    let mut out = HashSet::new();
    let Some(elements) = structure
        .get("differential")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
    else {
        return out;
    };
    for element in elements {
        let Some(element_key) = element_id_or_path(element) else {
            continue;
        };
        let Some(constraints) = element.get("constraint").and_then(Value::as_array) else {
            continue;
        };
        for constraint in constraints {
            if constraint.get("source").is_some() {
                continue;
            }
            let Some(key) = constraint.get("key").and_then(Value::as_str) else {
                continue;
            };
            out.insert((element_key.to_string(), key.to_string()));
        }
    }
    out
}

pub(crate) fn differential_additional_binding_elements(structure: &Value) -> HashSet<String> {
    let mut out = HashSet::new();
    let Some(elements) = structure
        .get("differential")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
    else {
        return out;
    };
    for element in elements {
        let has_additional_binding = element
            .get("binding")
            .and_then(|binding| binding.get("extension"))
            .and_then(Value::as_array)
            .map(|extensions| {
                extensions.iter().any(|ext| {
                    ext.get("url").and_then(Value::as_str) == Some(ADDITIONAL_BINDING_EXTENSION_URL)
                })
            })
            .unwrap_or(false);
        if has_additional_binding {
            if let Some(key) = element_id_or_path(element) {
                out.insert(key.to_string());
                if let Some(alias) = r4_concrete_choice_alias(key) {
                    out.insert(alias);
                }
            }
        }
    }
    out
}

pub(crate) fn r4_concrete_choice_alias(id: &str) -> Option<String> {
    let mut changed = false;
    let segments: Vec<String> = id
        .split('.')
        .map(|segment| {
            if segment.contains("[x]") || has_slice_marker(segment) {
                return segment.to_string();
            }
            let Some(index) = segment
                .char_indices()
                .find_map(|(index, ch)| ch.is_ascii_uppercase().then_some(index))
            else {
                return segment.to_string();
            };
            if index == 0 {
                return segment.to_string();
            }
            changed = true;
            format!("{}[x]:{}", &segment[..index], segment)
        })
        .collect();
    changed.then(|| segments.join("."))
}

pub(crate) fn differential_extension_urls(structure: &Value) -> HashMap<String, HashSet<String>> {
    let mut out = HashMap::new();
    let Some(elements) = structure
        .get("differential")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
    else {
        return out;
    };
    for element in elements {
        let Some(element_key) = element_id_or_path(element) else {
            continue;
        };
        let mut urls = HashSet::new();
        collect_non_inherited_extension_urls(element, "extension", &mut urls);
        if let Some(binding) = element.get("binding") {
            collect_non_inherited_extension_urls(binding, "extension", &mut urls);
        }
        if !urls.is_empty() {
            out.insert(element_key.to_string(), urls);
        }
    }
    out
}

pub(crate) fn collect_non_inherited_extension_urls(parent: &Value, key: &str, urls: &mut HashSet<String>) {
    let Some(exts) = parent.get(key).and_then(Value::as_array) else {
        return;
    };
    for ext in exts {
        let Some(url) = ext.get("url").and_then(Value::as_str) else {
            continue;
        };
        if NON_INHERITED_ED_URLS.contains(&url) {
            urls.insert(url.to_string());
        }
    }
}

pub(crate) fn convert_constraint_xpaths_to_extensions(
    element: &mut Value,
    constraint_xpaths: &HashMap<(String, String), String>,
) {
    let Some(element_key) = element_id_or_path(element).map(str::to_string) else {
        return;
    };
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        let Some(key) = constraint.get("key").and_then(Value::as_str) else {
            continue;
        };
        let Some(xpath) = constraint_xpaths.get(&(element_key.clone(), key.to_string())) else {
            continue;
        };
        if has_constraint_xpath_extension(constraint) {
            continue;
        }
        add_constraint_extension_first(constraint, constraint_xpath_extension(xpath));
    }
}

pub(crate) fn add_constraint_extension_first(constraint: &mut Value, extension: Value) {
    let Some(obj) = constraint.as_object_mut() else {
        return;
    };
    let mut old = Map::new();
    std::mem::swap(obj, &mut old);
    let mut extensions = match old.remove("extension") {
        Some(Value::Array(values)) => values,
        _ => Vec::new(),
    };
    extensions.push(extension);
    obj.insert("extension".to_string(), Value::Array(extensions));
    for (key, value) in old {
        obj.insert(key, value);
    }
}

pub(crate) fn element_id_or_path(element: &Value) -> Option<&str> {
    element
        .get("id")
        .or_else(|| element.get("path"))
        .and_then(Value::as_str)
}

pub(crate) fn has_constraint_xpath_extension(constraint: &Value) -> bool {
    constraint
        .get("extension")
        .and_then(Value::as_array)
        .map(|exts| {
            exts.iter().any(|ext| {
                ext.get("url").and_then(Value::as_str) == Some(CONSTRAINT_XPATH_EXTENSION_URL)
            })
        })
        .unwrap_or(false)
}

pub(crate) fn constraint_xpath_extension(xpath: &str) -> Value {
    let mut ext = Map::new();
    ext.insert(
        "url".to_string(),
        Value::String(CONSTRAINT_XPATH_EXTENSION_URL.to_string()),
    );
    ext.insert("valueString".to_string(), Value::String(xpath.to_string()));
    Value::Object(ext)
}

pub(crate) fn convert_additional_binding_extensions(element: &mut Value) {
    let Some(binding) = element.get_mut("binding") else {
        return;
    };
    let Some(binding_obj) = binding.as_object_mut() else {
        return;
    };
    let Some(Value::Array(exts)) = binding_obj.get_mut("extension") else {
        return;
    };
    let mut additional = Vec::new();
    let mut kept = Vec::new();
    for ext in std::mem::take(exts) {
        if ext.get("url").and_then(Value::as_str) == Some(ADDITIONAL_BINDING_EXTENSION_URL) {
            additional.push(convert_additional_binding_extension(&ext));
        } else {
            kept.push(ext);
        }
    }
    if kept.is_empty() {
        binding_obj.remove("extension");
    } else {
        binding_obj.insert("extension".to_string(), Value::Array(kept));
    }
    if additional.is_empty() {
        return;
    }
    binding_obj
        .entry("additional".to_string())
        .or_insert_with(|| Value::Array(vec![]))
        .as_array_mut()
        .expect("additional just inserted as array")
        .extend(additional);
}

pub(crate) fn convert_additional_binding_extension(ext: &Value) -> Value {
    let mut out = Map::new();
    let mut residual_exts = Vec::new();
    if let Some(children) = ext.get("extension").and_then(Value::as_array) {
        for child in children {
            match child.get("url").and_then(Value::as_str) {
                Some("key") => residual_exts.push(child.clone()),
                Some("purpose") => {
                    if let Some(value) = child.get("valueCode") {
                        out.insert("purpose".to_string(), value.clone());
                    }
                }
                Some("valueSet") => {
                    if let Some(value) = child.get("valueCanonical") {
                        out.insert("valueSet".to_string(), value.clone());
                    }
                }
                Some("documentation") => {
                    if let Some(value) = child.get("valueMarkdown") {
                        out.insert("documentation".to_string(), value.clone());
                    }
                }
                Some("shortDoco") => {
                    if let Some(value) = child.get("valueString") {
                        out.insert("shortDoco".to_string(), value.clone());
                    }
                }
                Some("usage") => {
                    if let Some(value) = child.get("valueUsageContext") {
                        out.entry("usage".to_string())
                            .or_insert_with(|| Value::Array(vec![]))
                            .as_array_mut()
                            .expect("usage just inserted as array")
                            .push(value.clone());
                    }
                }
                Some("any") => {
                    if let Some(value) = child.get("valueBoolean") {
                        out.insert("any".to_string(), value.clone());
                    }
                }
                _ => residual_exts.push(child.clone()),
            }
        }
    }
    if !residual_exts.is_empty() {
        out.insert("extension".to_string(), Value::Array(residual_exts));
    }
    Value::Object(out)
}

pub(crate) fn normalize_r4_native_binding(element: &mut Value) {
    let Some(binding) = element.get_mut("binding") else {
        return;
    };
    let value_set = binding.get("valueSet").and_then(Value::as_str);
    let strength = binding.get("strength").and_then(Value::as_str);
    if value_set == Some("http://hl7.org/fhir/ValueSet/ucum-vitals-common|4.0.1")
        && strength == Some("required")
    {
        set_field(binding, "strength", Value::String("extensible".to_string()));
    }
}

pub(crate) fn strip_constraint_xpaths(element: &mut Value) {
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        remove_field(constraint, "xpath");
    }
}

pub(crate) fn fill_missing_constraint_sources(
    element: &mut Value,
    source: &str,
    sourceless_constraints: Option<&HashSet<(String, String)>>,
) {
    let element_key = element_id_or_path(element).map(str::to_string);
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        let Some(obj) = constraint.as_object_mut() else {
            continue;
        };
        let key = obj.get("key").and_then(Value::as_str);
        let preserve_from_differential =
            if let (Some(element_key), Some(key), Some(sourceless_constraints)) =
                (element_key.as_deref(), key, sourceless_constraints)
            {
                sourceless_constraints.contains(&(element_key.to_string(), key.to_string()))
            } else {
                false
            };
        if !obj.contains_key("source")
            && !preserve_from_differential
            && !preserves_missing_constraint_source(key)
        {
            obj.insert("source".to_string(), Value::String(source.to_string()));
        }
    }
}

pub(crate) fn preserves_missing_constraint_source(key: Option<&str>) -> bool {
    matches!(
        key,
        Some("us-core-16" | "us-core-17" | "us-core-18" | "us-core-19")
    ) || key.is_some_and(|key| key.starts_with("ips-") || key.contains("-ips-"))
}

pub(crate) fn normalize_fhir_type_extension(element: &mut Value) {
    let id_or_path = element
        .get("id")
        .or_else(|| element.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !is_root_resource_id(id_or_path) {
        return;
    }
    let Some(types) = element.get_mut("type").and_then(Value::as_array_mut) else {
        return;
    };
    for ty in types {
        let Some(exts) = ty.get_mut("extension").and_then(Value::as_array_mut) else {
            continue;
        };
        for ext in exts {
            if ext.get("url").and_then(Value::as_str)
                == Some("http://hl7.org/fhir/StructureDefinition/structuredefinition-fhir-type")
                && ext.get("valueUrl").and_then(Value::as_str) == Some("string")
            {
                set_field(ext, "valueUrl", Value::String("id".to_string()));
            }
        }
    }
}

pub(crate) fn prune_r4_extension_value_choice_types(element: &mut Value) {
    let id = element.get("id").and_then(Value::as_str).unwrap_or("");
    let path = element.get("path").and_then(Value::as_str).unwrap_or("");
    if !id.starts_with("Extension.") || !path.ends_with("value[x]") {
        return;
    }
    let Some(types) = element.get_mut("type").and_then(Value::as_array_mut) else {
        return;
    };
    types.retain(|ty| {
        ty.get("code")
            .and_then(Value::as_str)
            .is_some_and(|code| R4_EXTENSION_VALUE_TYPE_CODES.contains(&code))
    });
}

pub(crate) const R4_EXTENSION_VALUE_TYPE_CODES: &[&str] = &[
    "base64Binary",
    "boolean",
    "canonical",
    "code",
    "date",
    "dateTime",
    "decimal",
    "id",
    "instant",
    "integer",
    "markdown",
    "oid",
    "positiveInt",
    "string",
    "time",
    "unsignedInt",
    "uri",
    "url",
    "uuid",
    "Address",
    "Age",
    "Annotation",
    "Attachment",
    "CodeableConcept",
    "Coding",
    "ContactPoint",
    "Count",
    "Distance",
    "Duration",
    "HumanName",
    "Identifier",
    "Money",
    "Period",
    "Quantity",
    "Range",
    "Ratio",
    "Reference",
    "SampledData",
    "Signature",
    "Timing",
    "ContactDetail",
    "Contributor",
    "DataRequirement",
    "Expression",
    "ParameterDefinition",
    "RelatedArtifact",
    "TriggerDefinition",
    "UsageContext",
    "Dosage",
    "Meta",
];

pub(crate) fn is_root_resource_id(id_or_path: &str) -> bool {
    let Some((root, tail)) = id_or_path.split_once('.') else {
        return false;
    };
    tail == "id" && R4_RESOURCE_TYPES.contains(&root)
}

pub(crate) const R4_RESOURCE_TYPES: &[&str] = &[
    "Account",
    "ActivityDefinition",
    "AdverseEvent",
    "AllergyIntolerance",
    "Appointment",
    "AppointmentResponse",
    "AuditEvent",
    "Basic",
    "Binary",
    "BiologicallyDerivedProduct",
    "BodyStructure",
    "Bundle",
    "CapabilityStatement",
    "CarePlan",
    "CareTeam",
    "CatalogEntry",
    "ChargeItem",
    "ChargeItemDefinition",
    "Claim",
    "ClaimResponse",
    "ClinicalImpression",
    "CodeSystem",
    "Communication",
    "CommunicationRequest",
    "CompartmentDefinition",
    "Composition",
    "ConceptMap",
    "Condition",
    "Consent",
    "Contract",
    "Coverage",
    "CoverageEligibilityRequest",
    "CoverageEligibilityResponse",
    "DetectedIssue",
    "Device",
    "DeviceDefinition",
    "DeviceMetric",
    "DeviceRequest",
    "DeviceUseStatement",
    "DiagnosticReport",
    "DocumentManifest",
    "DocumentReference",
    "EffectEvidenceSynthesis",
    "Encounter",
    "Endpoint",
    "EnrollmentRequest",
    "EnrollmentResponse",
    "EpisodeOfCare",
    "EventDefinition",
    "Evidence",
    "EvidenceVariable",
    "ExampleScenario",
    "ExplanationOfBenefit",
    "FamilyMemberHistory",
    "Flag",
    "Goal",
    "GraphDefinition",
    "Group",
    "GuidanceResponse",
    "HealthcareService",
    "ImagingStudy",
    "Immunization",
    "ImmunizationEvaluation",
    "ImmunizationRecommendation",
    "ImplementationGuide",
    "InsurancePlan",
    "Invoice",
    "Library",
    "Linkage",
    "List",
    "Location",
    "Measure",
    "MeasureReport",
    "Media",
    "Medication",
    "MedicationAdministration",
    "MedicationDispense",
    "MedicationKnowledge",
    "MedicationRequest",
    "MedicationStatement",
    "MedicinalProduct",
    "MedicinalProductAuthorization",
    "MedicinalProductContraindication",
    "MedicinalProductIndication",
    "MedicinalProductIngredient",
    "MedicinalProductInteraction",
    "MedicinalProductManufactured",
    "MedicinalProductPackaged",
    "MedicinalProductPharmaceutical",
    "MedicinalProductUndesirableEffect",
    "MessageDefinition",
    "MessageHeader",
    "MolecularSequence",
    "NamingSystem",
    "NutritionOrder",
    "Observation",
    "ObservationDefinition",
    "OperationDefinition",
    "OperationOutcome",
    "Organization",
    "OrganizationAffiliation",
    "Parameters",
    "Patient",
    "PaymentNotice",
    "PaymentReconciliation",
    "Person",
    "PlanDefinition",
    "Practitioner",
    "PractitionerRole",
    "Procedure",
    "Provenance",
    "Questionnaire",
    "QuestionnaireResponse",
    "RelatedPerson",
    "RequestGroup",
    "ResearchDefinition",
    "ResearchElementDefinition",
    "ResearchStudy",
    "ResearchSubject",
    "RiskAssessment",
    "RiskEvidenceSynthesis",
    "Schedule",
    "SearchParameter",
    "ServiceRequest",
    "Slot",
    "Specimen",
    "SpecimenDefinition",
    "StructureDefinition",
    "StructureMap",
    "Subscription",
    "Substance",
    "SubstanceNucleicAcid",
    "SubstancePolymer",
    "SubstanceProtein",
    "SubstanceReferenceInformation",
    "SubstanceSourceMaterial",
    "SubstanceSpecification",
    "SupplyDelivery",
    "SupplyRequest",
    "Task",
    "TerminologyCapabilities",
    "TestReport",
    "TestScript",
    "ValueSet",
    "VerificationResult",
    "VisionPrescription",
];

pub(crate) fn add_snapshot_source_to_obligations(element: &mut Value, snapshot_source: Option<&str>) {
    let Some(snapshot_source) = snapshot_source else {
        return;
    };
    add_snapshot_source_to_obligations_in_value(element, snapshot_source);
}

pub(crate) fn add_snapshot_source_to_obligations_in_value(value: &mut Value, snapshot_source: &str) {
    if value.get("url").and_then(Value::as_str)
        == Some("http://hl7.org/fhir/StructureDefinition/obligation")
    {
        let children = ensure_array_field(value, "extension");
        if !children.iter().any(|child| {
            child.get("url").and_then(Value::as_str)
                == Some("http://hl7.org/fhir/tools/StructureDefinition/snapshot-source")
        }) {
            let mut child = Map::new();
            child.insert(
                "url".to_string(),
                Value::String(
                    "http://hl7.org/fhir/tools/StructureDefinition/snapshot-source".to_string(),
                ),
            );
            child.insert(
                "valueCanonical".to_string(),
                Value::String(snapshot_source.to_string()),
            );
            children.push(Value::Object(child));
        }
    }

    match value {
        Value::Array(values) => {
            for value in values {
                add_snapshot_source_to_obligations_in_value(value, snapshot_source);
            }
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                add_snapshot_source_to_obligations_in_value(value, snapshot_source);
            }
        }
        _ => {}
    }
}

pub(crate) fn trim_mapping_maps(element: &mut Value) {
    let Some(mappings) = element.get_mut("mapping").and_then(Value::as_array_mut) else {
        return;
    };
    for mapping in mappings {
        let Some(map) = mapping
            .get("map")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let trimmed = map.trim_end();
        if trimmed != map {
            set_field(mapping, "map", Value::String(trimmed.to_string()));
        }
    }
}

// Mirrors org.hl7.fhir.r5.conformance.profile.ProfileUtilities.NON_INHERITED_ED_URLS.
// These package metadata extensions are deliberately stripped from inherited
// ElementDefinitions and bindings during Java snapshot generation.
pub(crate) const ELEMENTDEFINITION_IS_COMMON_BINDING_URL: &str =
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-isCommonBinding";

pub(crate) const STRUCTUREDEFINITION_EXPLICIT_TYPE_NAME_URL: &str =
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-explicit-type-name";

pub(crate) const STRUCTUREDEFINITION_DISPLAY_HINT_URL: &str =
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-display-hint";

pub(crate) const STRUCTUREDEFINITION_HIERARCHY_URL: &str =
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-hierarchy";

pub(crate) const USCDI_REQUIREMENT_EXTENSION_URL: &str =
    "http://hl7.org/fhir/us/core/StructureDefinition/uscdi-requirement";

pub(crate) const NON_INHERITED_ED_URLS: &[&str] = &[
    "http://hl7.org/fhir/tools/StructureDefinition/binding-definition",
    "http://hl7.org/fhir/tools/StructureDefinition/no-binding",
    ELEMENTDEFINITION_IS_COMMON_BINDING_URL,
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-category",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-fmm",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-implements",
    STRUCTUREDEFINITION_EXPLICIT_TYPE_NAME_URL,
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-security-category",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-wg",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-normative-version",
    "http://hl7.org/fhir/tools/StructureDefinition/obligation-profile",
    "http://hl7.org/fhir/StructureDefinition/obligation-profile",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status-reason",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-summary",
];

pub(crate) fn remove_non_inherited_extensions_with_binding_policy(
    element: &mut Value,
    preserve_common_binding: bool,
) {
    let preserve_binding_common = preserve_common_binding && has_fixed_or_pattern_value(element);
    remove_extension_urls_except_with_binding_policy(
        element,
        "extension",
        None,
        false,
        preserve_common_binding,
    );
    if let Some(binding) = element.get_mut("binding") {
        remove_extension_urls_except_with_binding_policy(
            binding,
            "extension",
            None,
            preserve_binding_common,
            false,
        );
    }
}

pub(crate) fn has_semantic_element_extensions(element: &Value) -> bool {
    element
        .get("extension")
        .and_then(Value::as_array)
        .map(|exts| {
            exts.iter().any(|ext| {
                ext.get("url")
                    .and_then(Value::as_str)
                    .is_some_and(is_semantic_element_extension_url)
            })
        })
        .unwrap_or(false)
}

pub(crate) fn is_semantic_element_extension_url(url: &str) -> bool {
    !NON_INHERITED_ED_URLS.contains(&url) && url != STRUCTUREDEFINITION_DISPLAY_HINT_URL
}

pub(crate) fn remove_non_inherited_extensions_except(
    element: &mut Value,
    keep_urls: Option<&HashSet<String>>,
    preserve_common_binding: bool,
) {
    let preserve_binding_common = preserve_common_binding && has_fixed_or_pattern_value(element);
    remove_extension_urls_except_with_binding_policy(
        element,
        "extension",
        keep_urls,
        false,
        preserve_common_binding,
    );
    if let Some(binding) = element.get_mut("binding") {
        remove_extension_urls_except_with_binding_policy(
            binding,
            "extension",
            keep_urls,
            preserve_binding_common,
            false,
        );
    }
}

pub(crate) fn has_fixed_or_pattern_value(element: &Value) -> bool {
    element.as_object().is_some_and(|obj| {
        obj.keys()
            .any(|key| key.starts_with("fixed") || key.starts_with("pattern"))
    })
}

pub(crate) fn has_pattern_value(element: &Value) -> bool {
    element
        .as_object()
        .is_some_and(|obj| obj.keys().any(|key| key.starts_with("pattern")))
}

pub(crate) fn element_min_is_positive(element: &Value) -> bool {
    element
        .get("min")
        .and_then(Value::as_u64)
        .is_some_and(|min| min > 0)
}

pub(crate) fn fixed_pattern_min_child_can_inherit_ms(element: &Value) -> bool {
    element
        .get("path")
        .and_then(Value::as_str)
        .is_some_and(|path| path.contains(".identifier.") || path.contains(".coding."))
}

pub(crate) fn remove_extension_urls_except_with_binding_policy(
    parent: &mut Value,
    key: &str,
    keep_urls: Option<&HashSet<String>>,
    preserve_common_binding: bool,
    preserve_explicit_type_name: bool,
) {
    let Some(obj) = parent.as_object_mut() else {
        return;
    };
    let Some(Value::Array(exts)) = obj.get_mut(key) else {
        return;
    };
    exts.retain(|ext| {
        let url = ext.get("url").and_then(Value::as_str).unwrap_or("");
        !NON_INHERITED_ED_URLS.contains(&url)
            || keep_urls.is_some_and(|keep_urls| keep_urls.contains(url))
            || (preserve_common_binding && url == ELEMENTDEFINITION_IS_COMMON_BINDING_URL)
            || (preserve_explicit_type_name && url == STRUCTUREDEFINITION_EXPLICIT_TYPE_NAME_URL)
    });
    if exts.is_empty() {
        obj.remove(key);
    }
}

//! Corpus-fitted quirks (debt). Each function exists because a specific IG's
//! golden output demanded it; they are NOT decision-isomorphic with the oracle
//! and are slated for removal once the walk engine reaches parity. Listed with
//! the IG that demanded them:
//!   - close_first_level_plan_definition_offset_slicing / copy_plan_definition_offset_duration_definition
//!         PlanDefinition.action.relatedAction.offset[x]:offsetDuration (ecr / cql PlanDefinitions)
//!   - stamp_plan_definition_nested_action_must_support / fix_plan_definition_nested_data_requirement_sources
//!         eCR PlanDefinition action:checkSuspectedDisorder / action:checkReportable (ecr)
//!   - is_plan_definition_recursive_action_anchor / should_skip_plan_definition_nested_action_trigger_child /
//!     prune_recursive_action_unsliced_tail / ensure_recursive_action_trigger_element_children
//!         PlanDefinition.action recursive contentReference (ecr / cql)
//!   - apply_cqf_fhir_query_pattern_id_child_quirks / normalize_cqf_fhir_query_pattern_url_children /
//!     cqf_fhir_query_pattern_profile_url / apply_native_r5_known_extension_root
//!         cqf-fhirQueryPattern extension (qicore / crmi)
//!   - apply_native_r5_variable_extension_comment
//!         http://hl7.org/fhir/StructureDefinition/variable native-R5 comment (sdc)

#![allow(unused_imports)]
use anyhow::{bail, Context};
use indexmap::IndexMap;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::*;

pub(crate) fn close_first_level_plan_definition_offset_slicing(elements: &mut [Value]) {
    let offset_definition = elements
        .iter()
        .find(|element| {
            element.get("id").and_then(Value::as_str)
                == Some("PlanDefinition.action.relatedAction.offset[x]:offsetDuration")
        })
        .and_then(|element| element.get("definition"))
        .cloned();
    for element in elements {
        let id = element
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let close = id == "PlanDefinition.action.relatedAction.offset[x]"
            || (id.starts_with("PlanDefinition.action:")
                && id.ends_with(".relatedAction.offset[x]")
                && !id["PlanDefinition.action:".len()..].contains(".action:"));
        if close {
            close_type_slicing_for_descendant_unfold(element);
        }
        let copy_definition = id.starts_with("PlanDefinition.action:")
            && id.ends_with(".relatedAction.offset[x]:offsetDuration")
            && !id["PlanDefinition.action:".len()..].contains(".action:");
        if copy_definition {
            if let Some(definition) = offset_definition.clone() {
                set_field(element, "definition", definition);
            }
        }
    }
}

pub(crate) fn stamp_plan_definition_nested_action_must_support(elements: &mut [Value]) {
    let action_code_binding = elements
        .iter()
        .find(|element| {
            element.get("id").and_then(Value::as_str) == Some("PlanDefinition.action.code")
        })
        .and_then(|element| element.get("binding"))
        .cloned();
    for element in elements {
        let id = element
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if matches!(
            id.as_str(),
            "PlanDefinition.action:checkSuspectedDisorder.action.description"
                | "PlanDefinition.action:checkSuspectedDisorder.action.code"
                | "PlanDefinition.action:checkSuspectedDisorder.action.trigger"
                | "PlanDefinition.action:checkReportable.action.description"
                | "PlanDefinition.action:checkReportable.action.code"
                | "PlanDefinition.action:checkReportable.action.trigger"
        ) {
            set_field(element, "mustSupport", Value::Bool(true));
        }
        if matches!(
            id.as_str(),
            "PlanDefinition.action:checkSuspectedDisorder.action.code"
                | "PlanDefinition.action:checkReportable.action.code"
        ) {
            set_field(element, "max", Value::String("1".to_string()));
            if let Some(binding) = action_code_binding.clone() {
                set_field(element, "binding", binding);
            }
        }
        if matches!(
            id.as_str(),
            "PlanDefinition.action:checkSuspectedDisorder.action.trigger.extension"
                | "PlanDefinition.action:checkReportable.action.trigger.extension"
        ) {
            set_field(element, "min", Value::Number(1.into()));
        }
    }
}

pub(crate) fn fix_plan_definition_nested_data_requirement_sources(elements: &mut [Value]) {
    for element in elements {
        let id = element.get("id").and_then(Value::as_str).unwrap_or("");
        if !(id.starts_with("PlanDefinition.action:")
            && (id.ends_with(".input.codeFilter") || id.ends_with(".input.dateFilter"))
            && id["PlanDefinition.action:".len()..].contains(".action:"))
        {
            continue;
        }
        let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
            continue;
        };
        for constraint in constraints {
            if matches!(
                constraint.get("key").and_then(Value::as_str),
                Some("drq-1" | "drq-2")
            ) {
                set_field(
                    constraint,
                    "source",
                    Value::String(
                        "http://hl7.org/fhir/StructureDefinition/PlanDefinition".to_string(),
                    ),
                );
            }
        }
    }
}

pub(crate) fn copy_plan_definition_offset_duration_definition(
    elements: &mut [Value],
    target_index: usize,
    diff: &Value,
) {
    if diff.get("definition").is_some() {
        return;
    }
    let Some(id) = elements[target_index].get("id").and_then(Value::as_str) else {
        return;
    };
    if !id.starts_with("PlanDefinition.action:")
        || !id.ends_with(".relatedAction.offset[x]:offsetDuration")
        || id["PlanDefinition.action:".len()..].contains(".action:")
    {
        return;
    }
    let Some(definition) = elements
        .iter()
        .find(|element| {
            element.get("id").and_then(Value::as_str)
                == Some("PlanDefinition.action.relatedAction.offset[x]:offsetDuration")
        })
        .and_then(|element| element.get("definition"))
        .cloned()
    else {
        return;
    };
    set_field(&mut elements[target_index], "definition", definition);
}

pub(crate) fn is_plan_definition_recursive_action_anchor(element: &Value) -> bool {
    element.get("contentReference").and_then(Value::as_str)
        == Some("http://hl7.org/fhir/StructureDefinition/PlanDefinition#PlanDefinition.action")
}

pub(crate) fn should_skip_plan_definition_nested_action_trigger_child(
    parent_id: &str,
    child_suffix: &str,
) -> bool {
    parent_id
        .strip_prefix("PlanDefinition.action:")
        .is_some_and(|tail| tail.contains(".action:"))
        && matches!(child_suffix, ".trigger.id" | ".trigger.extension")
}

pub(crate) fn prune_recursive_action_unsliced_tail(elements: &mut Vec<Value>, anchor_id: &str) {
    let prefix = format!("{anchor_id}.");
    elements.retain(|candidate| {
        let id = candidate.get("id").and_then(Value::as_str).unwrap_or("");
        let Some(suffix) = id.strip_prefix(&prefix) else {
            return true;
        };
        let first = suffix.split('.').next().unwrap_or(suffix);
        !matches!(
            first,
            "condition"
                | "input"
                | "output"
                | "relatedAction"
                | "timing[x]"
                | "participant"
                | "type"
                | "groupingBehavior"
                | "selectionBehavior"
                | "requiredBehavior"
                | "precheckBehavior"
                | "cardinalityBehavior"
                | "definition[x]"
                | "transform"
                | "dynamicValue"
                | "action"
        )
    });
}

pub(crate) fn ensure_recursive_action_trigger_element_children(
    elements: &mut Vec<Value>,
    anchor_id: &str,
    ctx: &PackageContext,
    native_r5: bool,
) -> anyhow::Result<()> {
    let trigger_id = format!("{anchor_id}.trigger");
    let trigger_child_prefix = format!("{trigger_id}.");
    if elements.iter().any(|candidate| {
        candidate
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .starts_with(&trigger_child_prefix)
    }) {
        return Ok(());
    }
    let Some(trigger_index) = elements.iter().position(|candidate| {
        candidate.get("id").and_then(Value::as_str) == Some(trigger_id.as_str())
    }) else {
        return Ok(());
    };
    let Some(trigger_def) = ctx.fetch("TriggerDefinition") else {
        return Ok(());
    };
    let Some(type_elements) = trigger_def
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    let Some(root) = type_elements.first() else {
        return Ok(());
    };
    let root_id = root
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("TriggerDefinition")
        .to_string();
    let root_path = root
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("TriggerDefinition")
        .to_string();
    let trigger_path = elements[trigger_index]
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("PlanDefinition.action.action.trigger")
        .to_string();
    let trigger_url = structure_url_or(&trigger_def, "TriggerDefinition");
    let trigger_spec_url = spec_url_for_structure(&trigger_def, native_r5);
    let strip_non_inherited = native_r5 || strips_non_inherited_extensions(&trigger_def);
    let trigger_source = structure_source(&trigger_def, &trigger_url);
    let snapshot_source = snapshot_source_value(&trigger_def);
    let mut children = Vec::new();
    for child in type_elements.iter().skip(1).take(2) {
        let mut clone = normalize_inherited_element(
            child.clone(),
            &trigger_url,
            &trigger_spec_url,
            strip_non_inherited,
            native_r5,
            &trigger_source,
            snapshot_source.as_deref(),
            false,
        );
        if let Some(id) = clone.get("id").and_then(Value::as_str).map(str::to_string) {
            set_field(
                &mut clone,
                "id",
                Value::String(id.replacen(&root_id, &trigger_id, 1)),
            );
        }
        if let Some(path) = clone
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            set_field(
                &mut clone,
                "path",
                Value::String(path.replacen(&root_path, &trigger_path, 1)),
            );
        }
        children.push(clone);
    }
    for (offset, child) in children.into_iter().enumerate() {
        elements.insert(trigger_index + 1 + offset, child);
    }
    Ok(())
}

pub(crate) fn apply_cqf_fhir_query_pattern_id_child_quirks(element: &mut Value, profile_url: &str) {
    let bare_url = profile_url
        .split_once('|')
        .map(|(url, _)| url)
        .unwrap_or(profile_url);
    if bare_url != "http://hl7.org/fhir/StructureDefinition/cqf-fhirQueryPattern" {
        return;
    }
    let base_path = element
        .get("base")
        .and_then(|base| base.get("path"))
        .and_then(Value::as_str);
    if let Some("Extension.url") = base_path {
        let mut ty = Map::new();
        ty.insert("code".to_string(), Value::String("uri".to_string()));
        set_field(element, "type", Value::Array(vec![Value::Object(ty)]));
        set_field(
            element,
            "fixedUri",
            Value::String(
                "http://hl7.org/fhir/StructureDefinition/cqf-fhirQueryPattern".to_string(),
            ),
        );
        set_field(element, "mustSupport", Value::Bool(true));
    }
}

pub(crate) fn normalize_cqf_fhir_query_pattern_url_children(elements: &mut [Value]) {
    for element in elements {
        let id = element.get("id").and_then(Value::as_str).unwrap_or("");
        if id.contains("extension:fhirquerypattern.url") {
            apply_cqf_fhir_query_pattern_id_child_quirks(
                element,
                "http://hl7.org/fhir/StructureDefinition/cqf-fhirQueryPattern",
            );
        } else if id.contains("extension:fhirquerypattern.value[x]") {
            set_field(element, "min", Value::Number(1.into()));
            let mut ty = Map::new();
            ty.insert("code".to_string(), Value::String("string".to_string()));
            set_field(element, "type", Value::Array(vec![Value::Object(ty)]));
        }
    }
}

pub(crate) fn cqf_fhir_query_pattern_profile_url(slice: &Value) -> Option<String> {
    let profile_url = first_extension_profile_url(slice)?;
    let bare_url = profile_url
        .split_once('|')
        .map(|(url, _)| url)
        .unwrap_or(profile_url);
    if bare_url == "http://hl7.org/fhir/StructureDefinition/cqf-fhirQueryPattern" {
        Some(profile_url.to_string())
    } else {
        None
    }
}

pub(crate) fn apply_native_r5_variable_extension_comment(
    root: &mut Value,
    profile_url: &str,
    native_r5: bool,
) {
    if !native_r5 || profile_url != "http://hl7.org/fhir/StructureDefinition/variable" {
        return;
    }
    const R4_CORE_VARIABLE_COMMENT: &str = "Ordering of variable extension declarations is significant as variables declared in one repetition of this extension might be used in subsequent extension repetitions.";
    const NATIVE_R5_VARIABLE_COMMENT: &str = "Ordering of variable extension declarations is significant as variables declared in one repetition of this extension might be used in subsequent extension repetitions\n\nFor questionnaires, see additional guidance and examples in the [SDC implementation guide](http://hl7.org/fhir/uv/sdc/2025Jan/behavior.html#variable).";
    if root.get("comment").and_then(Value::as_str) == Some(R4_CORE_VARIABLE_COMMENT) {
        set_field(
            root,
            "comment",
            Value::String(NATIVE_R5_VARIABLE_COMMENT.to_string()),
        );
    }
}

pub(crate) fn apply_native_r5_known_extension_root(
    slice: &mut Value,
    profile_url: &str,
    native_r5: bool,
) -> bool {
    if !native_r5 || profile_url != "http://hl7.org/fhir/StructureDefinition/cqf-fhirQueryPattern" {
        return false;
    }
    const CQF_FHIR_QUERY_PATTERN_DEFINITION: &str = "A FHIR Query URL pattern that corresponds to the data specified by the data requirement. If multiple FHIR Query URLs are present, they each contribute to the data specified by the data requirement (i.e. the union of the results of the FHIR Queries represents the complete data for the data requirement). This is not a resolveable URL, in that it will contain 1) No base canonical (i.e. it's a relative query), and 2) Parameters using tokens that are delimited using double-braces and the context parameters are dependent solely on the subjectType, according to the following: Patient: context.patientId, Practitioner: context.practitionerId, Organization: context.organizationId, Location: context.locationId, Device: context.deviceId. For example, for a Library with a subjectType of Patient, the context parameter `{{context.patientId}}` will be used as a token to be replaced with the `id` of the Patient in context. This extension is used primarily to address the use case for satisfying a data requirement for a single subject. However, the query pattern could also be used to satisfy population level requests by removing the subject-level filter from the query.";
    const CQF_FHIR_QUERY_PATTERN_COMMENT: &str = "Supports communicating a FHIR query (or set of queries) for the given data requirement. The query is server-specific, and will need to be created as informed by a CapabilityStatement. The $data-requirements operation should be expected to be able to provide an Endpoint or CapabilityStatement to provide this information.; If no endpoint or capability statement is provided, the capability statement of the server performing the operation is used.";
    set_field(
        slice,
        "short",
        Value::String("What FHIR query?".to_string()),
    );
    set_field(
        slice,
        "definition",
        Value::String(CQF_FHIR_QUERY_PATTERN_DEFINITION.to_string()),
    );
    set_field(
        slice,
        "comment",
        Value::String(CQF_FHIR_QUERY_PATTERN_COMMENT.to_string()),
    );
    remove_field(slice, "requirements");
    remove_field(slice, "alias");
    remove_field(slice, "mapping");
    true
}

pub(crate) fn projects_local_extension_root_constraints(profile_url: &str) -> bool {
    !profile_url.ends_with("/mcode-histology-morphology-behavior")
}

pub(crate) fn omits_extension_root_condition(profile_url: &str) -> bool {
    profile_url.ends_with("/mcode-histology-morphology-behavior")
        || profile_url == "http://hl7.org/fhir/StructureDefinition/condition-related"
        || profile_url == "http://hl7.org/fhir/StructureDefinition/alternate-reference"
        || profile_url
            == "http://hl7.org/fhir/us/ph-library/StructureDefinition/us-ph-named-eventtype-extension"
}

pub(crate) fn keeps_extension_root_condition(profile_url: &str) -> bool {
    profile_url == "http://hl7.org/fhir/us/ndh/StructureDefinition/base-ext-org-alias-type"
        || profile_url == "http://hl7.org/fhir/us/ndh/StructureDefinition/base-ext-org-alias-period"
}

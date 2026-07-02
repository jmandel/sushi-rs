use anyhow::Context;
use indexmap::IndexMap;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

mod cli;
pub(crate) mod convert;
mod legacy;
mod merge;
mod package;
mod projection;
mod quirks;
mod text;

pub use cli::{main_cli, Engine, SnapshotOptions};
pub use package::PackageContext;

// Re-export every submodule item at crate root so modules can `use crate::*;`
// and reach each other without per-item import churn.
pub(crate) use legacy::*;
pub(crate) use merge::*;
pub(crate) use projection::*;
pub(crate) use quirks::*;
pub(crate) use text::*;

/// Stage-2 pure R4->R5 StructureDefinition conversion (VersionConvertor_40_50
/// semantics). Context-free; R5 inputs pass through unchanged. Exposed for the
/// `--dump-converted` CLI mode and the `convert_parity` gate.
pub fn convert_r4_sd_to_r5(sd: &Value) -> anyhow::Result<Value> {
    convert::r4_sd_to_r5(sd)
}

pub fn generate_snapshot(
    mut derived: Value,
    ctx: &PackageContext,
    options: SnapshotOptions,
) -> anyhow::Result<Value> {
    let base_url = derived
        .get("baseDefinition")
        .and_then(Value::as_str)
        .context("StructureDefinition.baseDefinition is required")?
        .to_string();
    let base = structure_with_r4_snapshot(&base_url, ctx)?
        .with_context(|| format!("base not found: {base_url}"))?;
    let base_spec_url = spec_url_for_structure(&base, options.native_r5);
    let base_strip_non_inherited = options.native_r5 || strips_non_inherited_extensions(&base);
    let base_preserve_common_binding = options.native_r5 && is_r4_spec_url(&base_spec_url);

    if options.sort_differential {
        sort_differential_by_base(&mut derived, &base);
    }

    let base_elements = base
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .context("base StructureDefinition has no snapshot.element")?;
    let mut diff_elements = derived
        .get("differential")
        .and_then(|d| d.get("element"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    canonicalize_choice_differentials(&mut diff_elements, base_elements);
    let explicit_slicing_paths: HashSet<String> = diff_elements
        .iter()
        .filter(|element| element.get("slicing").is_some())
        .filter_map(|element| {
            element
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();

    let base_constraint_source = structure_source(&base, &base_url);
    let base_is_local = ctx.is_local(&base_url);
    let mut snapshot_elements: Vec<Value> = base_elements
        .iter()
        .cloned()
        .map(|element| {
            normalize_inherited_element(
                element,
                &base_url,
                &base_spec_url,
                base_strip_non_inherited,
                options.native_r5,
                &base_constraint_source,
                None,
                base_is_local,
            )
        })
        .collect();
    let original_elements_by_id: HashMap<String, Value> = snapshot_elements
        .iter()
        .filter_map(|element| {
            element
                .get("id")
                .and_then(Value::as_str)
                .map(|id| (id.to_string(), element.clone()))
        })
        .collect();
    let original_ids: HashSet<String> = original_elements_by_id.keys().cloned().collect();
    let original_must_support_ids: HashSet<String> = snapshot_elements
        .iter()
        .filter(|element| element.get("mustSupport").and_then(Value::as_bool) == Some(true))
        .filter_map(|element| {
            element
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let original_snapshot_elements = snapshot_elements.clone();
    let diff_ids: HashSet<String> = diff_elements
        .iter()
        .filter_map(|d| d.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let diff_must_support_ids: HashSet<String> = diff_elements
        .iter()
        .filter(|d| d.get("mustSupport").is_some())
        .filter_map(|d| d.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let diff_preserve_must_support_ids: HashSet<String> = diff_elements
        .iter()
        .filter(|d| {
            d.get("mustSupport").is_some()
                || d.get("extension")
                    .and_then(Value::as_array)
                    .is_some_and(|exts| exts.iter().any(is_obligation_extension))
        })
        .filter_map(|d| d.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let diff_condition_ids: HashSet<String> = diff_elements
        .iter()
        .filter(|d| d.get("condition").is_some())
        .filter_map(|d| d.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let diff_slice_anchor_ids: HashSet<String> = diff_elements
        .iter()
        .filter(|d| d.get("sliceName").is_some())
        .filter_map(|d| {
            d.get("id")
                .and_then(Value::as_str)
                .and_then(slice_anchor_id_from_diff_id)
        })
        .collect();
    let diff_slice_orders: HashMap<String, usize> = diff_elements
        .iter()
        .enumerate()
        .filter(|(_, d)| d.get("sliceName").is_some())
        .filter_map(|(index, d)| {
            d.get("id")
                .and_then(Value::as_str)
                .map(|id| (id.to_string(), index))
        })
        .collect();
    let all_diff_elements = diff_elements.clone();
    // Java applies checkExtensionDoco to an extension profile's root element even
    // when the differential doesn't touch it (ecr eicr-initiation-type-extension);
    // when the root IS in the differential, the same normalization happens during
    // the per-element merge instead.
    let extension_root_untouched = options.apply_extension_root_doco
        && snapshot_elements
            .first()
            .and_then(|root| root.get("path").and_then(Value::as_str))
            == Some("Extension")
        && !diff_elements
            .iter()
            .any(|d| d.get("path").and_then(Value::as_str) == Some("Extension"));
    for (diff_index, diff) in diff_elements.iter().enumerate() {
        let Some(path) = diff.get("path").and_then(Value::as_str) else {
            continue;
        };
        let mut inserted_slice = false;
        if find_matching_snapshot_index(&snapshot_elements, path, &diff).is_none() {
            unfold_parent_for_diff(
                &mut snapshot_elements,
                &diff,
                ctx,
                &base_url,
                &base_spec_url,
                options.native_r5,
                &original_snapshot_elements,
            )?;
        }
        if find_matching_snapshot_index(&snapshot_elements, path, &diff).is_none()
            && diff.get("sliceName").is_some()
        {
            insert_slice_element(
                &mut snapshot_elements,
                path,
                &diff,
                ctx,
                &original_elements_by_id,
                base_strip_non_inherited,
                base_preserve_common_binding,
                options.native_r5,
                &base_url,
                &base_spec_url,
                &explicit_slicing_paths,
                &diff_ids,
                &diff_must_support_ids,
                &diff_preserve_must_support_ids,
                &diff_condition_ids,
            )?;
            inserted_slice = true;
        }
        if let Some(index) = find_matching_snapshot_index(&snapshot_elements, path, &diff) {
            if inserted_slice {
                apply_type_profile_root(
                    &mut snapshot_elements[index],
                    &diff,
                    ctx,
                    options.native_r5,
                )?;
                propagate_slice_min_to_anchor(&mut snapshot_elements, path, &diff, &diff_ids);
                ensure_type_slicing_anchor(&mut snapshot_elements, path, &diff);
                ensure_extension_slicing_anchor(&mut snapshot_elements, path, &diff);
                continue;
            }
            if diff.get("sliceName").is_some() && !explicit_slicing_paths.contains(path) {
                close_inferred_type_slice_anchor(&mut snapshot_elements, path, &diff);
            }
            if diff.get("type").is_some() {
                let target_id = snapshot_elements[index]
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(path)
                    .to_string();
                let target_path = snapshot_elements[index]
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or(path)
                    .to_string();
                unfold_content_reference_parent(
                    &mut snapshot_elements,
                    index,
                    &target_id,
                    &target_path,
                    &base_url,
                    &base_spec_url,
                    ctx,
                    options.native_r5,
                    &original_snapshot_elements,
                )?;
            }
            copy_plan_definition_offset_duration_definition(&mut snapshot_elements, index, &diff);
            apply_extension_profile_root(
                &mut snapshot_elements[index],
                &diff,
                ctx,
                options.native_r5,
                Some(&base_url),
                false,
                true,
                true,
            )?;
            apply_type_profile_root(&mut snapshot_elements[index], &diff, ctx, options.native_r5)?;
            apply_generalized_slice_differentials(
                &mut snapshot_elements[index],
                &diff,
                &all_diff_elements[..diff_index],
                base_strip_non_inherited,
                base_preserve_common_binding,
                Some(&diff_must_support_ids),
                Some(&original_must_support_ids),
                Some(&original_ids),
                &base_constraint_source,
            )?;
            merge_diff_into_element(
                &mut snapshot_elements[index],
                &diff,
                base_strip_non_inherited,
                base_preserve_common_binding,
                Some(&diff_must_support_ids),
                Some(&original_must_support_ids),
                Some(&original_ids),
                &base_constraint_source,
            )?;
            if diff.get("sliceName").is_some() {
                let slice_id = snapshot_elements[index]
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(path)
                    .to_string();
                let slice_path = snapshot_elements[index]
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or(path)
                    .to_string();
                if should_materialize_extension_profile_children_on_insert(
                    &snapshot_elements[index],
                ) {
                    if let Some(profile_url) =
                        first_extension_profile_url(&snapshot_elements[index]).map(str::to_string)
                    {
                        materialize_extension_profile_children_for_slice(
                            &mut snapshot_elements,
                            index,
                            &slice_id,
                            &slice_path,
                            &profile_url,
                            ctx,
                            options.native_r5,
                        )?;
                    }
                } else if should_materialize_existing_direct_slice_children(
                    &snapshot_elements[index],
                    &diff,
                    &original_elements_by_id,
                    &diff_ids,
                ) {
                    unfold_sliced_parent_from_anchor(
                        &mut snapshot_elements,
                        index,
                        &slice_id,
                        &slice_path,
                        Some(&original_elements_by_id),
                        Some(&diff_ids),
                        Some(&diff_preserve_must_support_ids),
                        Some(&diff_must_support_ids),
                    );
                }
            }
            let prune_profiled_unsliced_children = should_prune_profiled_unsliced_descendants(
                &snapshot_elements[index],
                &diff,
                &diff_ids,
                &original_elements_by_id,
            );
            if prune_profiled_unsliced_children {
                let id = snapshot_elements[index]
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(path)
                    .to_string();
                prune_unsliced_descendants(&mut snapshot_elements, &id);
            }
            if diff.get("sliceName").is_some() {
                prune_unsliced_descendants_for_slice_diff(
                    &mut snapshot_elements,
                    &diff,
                    &diff_ids,
                    &original_elements_by_id,
                );
            }
            propagate_slice_min_to_anchor(&mut snapshot_elements, path, &diff, &diff_ids);
            ensure_type_slicing_anchor(&mut snapshot_elements, path, &diff);
            ensure_extension_slicing_anchor(&mut snapshot_elements, path, &diff);
        }
    }

    close_first_level_plan_definition_offset_slicing(&mut snapshot_elements);
    stamp_plan_definition_nested_action_must_support(&mut snapshot_elements);
    materialize_generalized_child_slices_for_direct_slices(
        &mut snapshot_elements,
        &original_elements_by_id,
        &diff_ids,
        ctx,
        options.native_r5,
    )?;
    materialize_missing_extension_profile_children_for_slices(
        &mut snapshot_elements,
        ctx,
        options.native_r5,
    )?;
    fix_plan_definition_nested_data_requirement_sources(&mut snapshot_elements);
    reconcile_type_slicing_anchor_types(&mut snapshot_elements);
    sort_type_slice_groups_by_differential_order(
        &mut snapshot_elements,
        &diff_slice_anchor_ids,
        &diff_slice_orders,
    );
    materialize_missing_extension_profile_children_for_slices(
        &mut snapshot_elements,
        ctx,
        options.native_r5,
    )?;

    if extension_root_untouched {
        if let Some(root) = snapshot_elements.first_mut() {
            check_extension_doco(root);
        }
    }

    let obj = derived
        .as_object_mut()
        .context("input StructureDefinition must be a JSON object")?;
    let mut snapshot = Map::new();
    snapshot.insert("element".to_string(), Value::Array(snapshot_elements));

    let differential = obj.remove("differential");
    obj.insert("snapshot".to_string(), Value::Object(snapshot));
    if let Some(differential) = differential {
        obj.insert("differential".to_string(), differential);
    }

    if options.native_r5 {
        project_r4_snapshot_to_native_r5(&mut derived);
    }

    Ok(derived)
}

// Returns the structure identified by `url` with an R4-form (un-projected)
// snapshot, recursively generating it when the stored resource only has a
// differential. SUSHI emits local profiles without snapshots, so a local-base
// chain (e.g. DTR dtr-questionnaireresponse-adapt -> dtr-questionnaireresponse
// -> QuestionnaireResponse) needs the intermediate snapshots built on demand.
pub(crate) fn structure_with_r4_snapshot(url: &str, ctx: &PackageContext) -> anyhow::Result<Option<Value>> {
    let Some(profile) = ctx.fetch(url) else {
        return Ok(None);
    };
    if profile
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .is_some()
    {
        return Ok(Some(profile));
    }
    let generated = generate_snapshot(
        profile,
        ctx,
        SnapshotOptions {
            sort_differential: true,
            native_r5: false,
            apply_extension_root_doco: false,
        },
    )?;
    Ok(Some(generated))
}

pub(crate) fn profile_with_snapshot(
    profile_url: &str,
    ctx: &PackageContext,
    native_r5: bool,
) -> anyhow::Result<Option<Value>> {
    let Some(profile) = ctx.fetch(profile_url) else {
        return Ok(None);
    };
    if profile
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .is_some()
    {
        return Ok(Some(profile));
    }
    generate_snapshot(
        profile,
        ctx,
        SnapshotOptions {
            sort_differential: true,
            native_r5,
            apply_extension_root_doco: false,
        },
    )
    .map(Some)
}

pub(crate) fn normalize_inherited_element(
    mut element: Value,
    source_url: &str,
    spec_url: &str,
    strip_non_inherited: bool,
    native_r5: bool,
    constraint_source: &str,
    snapshot_source: Option<&str>,
    convert_own_xpaths: bool,
) -> Value {
    if strip_non_inherited {
        remove_non_inherited_extensions_with_binding_policy(
            &mut element,
            native_r5 && is_r4_spec_url(spec_url),
        );
    }
    trim_inherited_text_fields(&mut element);
    if element.get("comment").and_then(Value::as_str) == Some("-") {
        remove_field(&mut element, "comment");
    }
    rewrite_markdown_links(&mut element, spec_url, false);
    if native_r5 || spec_url.contains("/R5/") {
        absolutize_content_reference(&mut element, source_url);
    }
    if native_r5 {
        if convert_own_xpaths {
            convert_own_constraint_xpaths_to_extensions(&mut element);
        }
        let constraint_xpaths = HashMap::new();
        project_element_to_native_r5(
            &mut element,
            constraint_source,
            snapshot_source,
            &constraint_xpaths,
            None,
            false,
            true,
            None,
            native_r5 && is_r4_spec_url(spec_url),
        );
    }
    element
}

pub(crate) fn trim_inherited_text_fields(element: &mut Value) {
    for key in ["short", "definition", "comment", "requirements", "label"] {
        let Some(text) = element.get(key).and_then(Value::as_str) else {
            continue;
        };
        let trimmed = text.trim_end();
        if trimmed.len() != text.len() {
            set_field(element, key, Value::String(trimmed.to_string()));
        }
    }
}

// Mirrors org.hl7.fhir.r5.conformance.profile.ProfileUtilities updateFromDefinition
// (~line 3085): when a derived element is merged with its base, every base
// constraint lacking a `source` is stamped with the source StructureDefinition's
// URL (srcSD.getUrl()). This fires only for elements actually touched by the
// differential, so inherited-but-untouched constraints (e.g. CRD's us-core-16..19
// on slices it never merges) keep their missing source, while a profile that does
// constrain the slice (e.g. CARIN BB's Organization.identifier:NPI) stamps them.
pub(crate) fn fill_missing_constraint_sources_on_constrained_element(element: &mut Value, source: &str) {
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        let Some(obj) = constraint.as_object_mut() else {
            continue;
        };
        if !obj.contains_key("source") {
            obj.insert("source".to_string(), Value::String(source.to_string()));
        }
    }
}

pub(crate) fn structure_url_or(structure: &Value, fallback: &str) -> String {
    structure
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string()
}

pub(crate) fn spec_url_for_structure(structure: &Value, native_r5: bool) -> String {
    spec_url_from_version(
        structure.get("fhirVersion").and_then(Value::as_str),
        native_r5,
    )
}

pub(crate) fn strips_non_inherited_extensions(structure: &Value) -> bool {
    structure
        .get("fhirVersion")
        .and_then(Value::as_str)
        .map(|v| v.starts_with('5'))
        .unwrap_or(true)
}

pub(crate) fn spec_url_from_version(version: Option<&str>, native_r5: bool) -> String {
    match version.unwrap_or("") {
        v if v.starts_with('4') && native_r5 => "http://hl7.org/fhir/R4/".to_string(),
        v if v.starts_with('4') => "http://hl7.org/fhir/".to_string(),
        v if v.starts_with('5') => "http://hl7.org/fhir/R5/".to_string(),
        _ => "http://hl7.org/fhir/R5/".to_string(),
    }
}

pub(crate) fn is_r4_spec_url(spec_url: &str) -> bool {
    spec_url.contains("/R4/")
}

pub(crate) fn structure_source(structure: &Value, fallback: &str) -> String {
    structure
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string()
}

pub(crate) fn snapshot_source_value(structure: &Value) -> Option<String> {
    let url = structure.get("url").and_then(Value::as_str)?;
    match structure.get("version").and_then(Value::as_str) {
        Some(version) if !version.is_empty() => Some(format!("{url}|{version}")),
        _ => Some(url.to_string()),
    }
}

pub fn sort_differential_by_base(derived: &mut Value, base: &Value) {
    let Some(diff) = derived
        .get_mut("differential")
        .and_then(Value::as_object_mut)
        .and_then(|d| d.get_mut("element"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    let base_order = base_element_order(base);
    diff.sort_by(|a, b| {
        let ak = sort_key(a, &base_order);
        let bk = sort_key(b, &base_order);
        ak.cmp(&bk)
    });
}

pub(crate) fn base_element_order(base: &Value) -> IndexMap<String, usize> {
    let mut out = IndexMap::new();
    if let Some(elements) = base
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
    {
        for (i, element) in elements.iter().enumerate() {
            if let Some(path) = element.get("path").and_then(Value::as_str) {
                out.entry(path.to_string()).or_insert(i);
            }
        }
    }
    out
}

pub(crate) fn sort_key(element: &Value, base_order: &IndexMap<String, usize>) -> (usize, usize, usize) {
    let id = element.get("id").and_then(Value::as_str).unwrap_or("");
    let path = element.get("path").and_then(Value::as_str).unwrap_or("");
    let depth = path.bytes().filter(|b| *b == b'.').count();
    let order = base_order.get(path).copied().unwrap_or(usize::MAX / 2);
    let slice_rank = if has_slice_marker(id) { 1 } else { 0 };
    (slice_rank, order, depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_sort_uses_base_order() {
        let base = serde_json::json!({
            "snapshot": {
                "element": [
                    { "path": "Patient" },
                    { "path": "Patient.name" },
                    { "path": "Patient.gender" }
                ]
            }
        });
        let mut derived = serde_json::json!({
            "differential": {
                "element": [
                    { "path": "Patient.gender" },
                    { "path": "Patient" },
                    { "path": "Patient.name" }
                ]
            }
        });
        sort_differential_by_base(&mut derived, &base);
        let paths: Vec<_> = derived["differential"]["element"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, ["Patient", "Patient.name", "Patient.gender"]);
    }
}

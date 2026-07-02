//! The legacy diff-order snapshot engine: the driver-adjacent slicing, unfolding,
//! choice canonicalization, `merge_diff_into_element` + its mustSupport heuristics,
//! and the extension/type profile root overlays. Preserved verbatim; this is the
//! engine the walk engine will replace. Do not add cleanup here.

#![allow(unused_imports)]
use anyhow::{bail, Context};
use indexmap::IndexMap;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::*;

#[derive(Clone)]
pub(crate) struct ChoiceSegment {
    prefix: String,
    choice_segment: String,
    actual_segment: String,
    type_code: String,
}

pub(crate) fn canonicalize_choice_differentials(diff_elements: &mut [Value], base_elements: &[Value]) {
    let choices = collect_choice_segments(base_elements);
    if choices.is_empty() {
        return;
    }
    let direct_choice_slices: HashSet<String> = diff_elements
        .iter()
        .flat_map(|diff| {
            ["id", "path"]
                .into_iter()
                .filter_map(|key| direct_choice_slice_key(diff.get(key)?.as_str()?, &choices))
        })
        .collect();
    for diff in diff_elements {
        canonicalize_choice_field(diff, "path", &choices, &direct_choice_slices, false);
        canonicalize_choice_field(diff, "id", &choices, &direct_choice_slices, true);
    }
}

pub(crate) fn collect_choice_segments(base_elements: &[Value]) -> Vec<ChoiceSegment> {
    let mut out = Vec::new();
    for element in base_elements {
        let Some(id) = element.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some((prefix, choice_segment)) = id.rsplit_once('.') else {
            continue;
        };
        let Some(choice_base) = choice_segment.strip_suffix("[x]") else {
            continue;
        };
        let Some(types) = element.get("type").and_then(Value::as_array) else {
            continue;
        };
        for ty in types {
            let Some(code) = ty.get("code").and_then(Value::as_str) else {
                continue;
            };
            let Some(suffix) = choice_type_suffix(code) else {
                continue;
            };
            out.push(ChoiceSegment {
                prefix: prefix.to_string(),
                choice_segment: choice_segment.to_string(),
                actual_segment: format!("{choice_base}{suffix}"),
                type_code: code.to_string(),
            });
        }
    }
    out
}

pub(crate) fn choice_type_suffix(code: &str) -> Option<String> {
    let mut tail = code
        .rsplit('/')
        .next()
        .unwrap_or(code)
        .rsplit('.')
        .next()
        .unwrap_or(code)
        .to_string();
    if tail.is_empty() {
        return None;
    }
    if let Some(first) = tail.chars().next() {
        if first.is_ascii_lowercase() {
            tail.replace_range(0..first.len_utf8(), &first.to_ascii_uppercase().to_string());
        }
    }
    Some(tail)
}

pub(crate) fn direct_choice_slice_key(value: &str, choices: &[ChoiceSegment]) -> Option<String> {
    let segments: Vec<&str> = value.split('.').collect();
    for index in 0..segments.len() {
        let prefix = segments[..index].join(".");
        let Some(choice) = matching_choice(&prefix, segments[index], choices) else {
            continue;
        };
        if index + 1 == segments.len() && !has_slice_marker(&prefix) {
            return Some(choice_slice_key(&prefix, choice));
        }
    }
    None
}

pub(crate) fn matching_choice<'a>(
    prefix: &str,
    segment: &str,
    choices: &'a [ChoiceSegment],
) -> Option<&'a ChoiceSegment> {
    choices.iter().find(|choice| {
        choice.actual_segment == segment
            && (choice.prefix == prefix
                || unsliced_element_id(prefix)
                    .as_deref()
                    .is_some_and(|unsliced| unsliced == choice.prefix))
    })
}

pub(crate) fn choice_slice_key(prefix: &str, choice: &ChoiceSegment) -> String {
    format!(
        "{}.{}:{}",
        prefix, choice.choice_segment, choice.actual_segment
    )
}

pub(crate) fn choice_type_value(code: &str) -> Value {
    let mut ty = Map::new();
    ty.insert("code".to_string(), Value::String(code.to_string()));
    Value::Array(vec![Value::Object(ty)])
}

pub(crate) fn canonicalize_choice_field(
    diff: &mut Value,
    key: &str,
    choices: &[ChoiceSegment],
    direct_choice_slices: &HashSet<String>,
    add_direct_slice: bool,
) {
    let Some(original) = diff.get(key).and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let mut segments: Vec<String> = original.split('.').map(str::to_string).collect();
    let mut direct_choice: Option<(String, String, String, bool)> = None;
    for index in 0..segments.len() {
        let prefix = segments[..index].join(".");
        let Some(choice) = matching_choice(&prefix, &segments[index], choices) else {
            continue;
        };
        let actual = segments[index].clone();
        let slice_key = choice_slice_key(&prefix, choice);
        if add_direct_slice
            && index + 1 < segments.len()
            && direct_choice_slices.contains(&slice_key)
        {
            segments[index] = format!("{}:{}", choice.choice_segment, actual);
        } else {
            segments[index] = choice.choice_segment.clone();
        }
        if add_direct_slice && index + 1 == segments.len() {
            direct_choice = Some((
                choice.choice_segment.clone(),
                actual,
                choice.type_code.clone(),
                !has_slice_marker(&prefix),
            ));
        }
    }
    let mut canonical = segments.join(".");
    if let Some((choice_segment, actual, type_code, add_slice_marker)) = direct_choice {
        if add_slice_marker && diff.get("sliceName").is_none() {
            canonical =
                canonical.replacen(&choice_segment, &format!("{choice_segment}:{actual}"), 1);
            set_field(diff, "sliceName", Value::String(actual));
        }
        if diff.get("type").is_none() {
            set_field(diff, "type", choice_type_value(&type_code));
        }
    }
    if canonical != original {
        set_field(diff, key, Value::String(canonical));
    }
}

pub(crate) fn find_matching_snapshot_index(elements: &[Value], path: &str, diff: &Value) -> Option<usize> {
    let diff_id = diff.get("id").and_then(Value::as_str);
    if let Some(diff_id) = diff.get("id").and_then(Value::as_str) {
        if let Some(index) = elements
            .iter()
            .position(|candidate| candidate.get("id").and_then(Value::as_str) == Some(diff_id))
        {
            return Some(index);
        }
        if diff_id.contains(':') || diff_id.contains('/') {
            return None;
        }
    }
    let diff_slice = diff.get("sliceName").and_then(Value::as_str);
    elements.iter().position(|candidate| {
        if candidate.get("path").and_then(Value::as_str) != Some(path)
            || candidate.get("sliceName").and_then(Value::as_str) != diff_slice
        {
            return false;
        }
        if diff_id.is_some_and(|id| !has_slice_marker(id))
            && candidate
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(has_slice_marker)
        {
            return false;
        }
        true
    })
}

pub(crate) fn apply_generalized_slice_differentials(
    target: &mut Value,
    diff: &Value,
    prior_diff_elements: &[Value],
    strip_non_inherited: bool,
    preserve_common_binding: bool,
    diff_must_support_ids: Option<&HashSet<String>>,
    inherited_must_support_ids: Option<&HashSet<String>>,
    original_ids: Option<&HashSet<String>>,
    constraint_source: &str,
) -> anyhow::Result<()> {
    let Some(diff_id) = diff.get("id").and_then(Value::as_str) else {
        return Ok(());
    };
    if !has_slice_marker(diff_id) {
        return Ok(());
    }
    if is_direct_slice_id(diff_id) {
        return Ok(());
    }
    let diff_path = diff.get("path").and_then(Value::as_str);
    for generalized in prior_diff_elements {
        if generalized.get("slicing").is_some() {
            continue;
        }
        if generalized.get("path").and_then(Value::as_str) != diff_path {
            continue;
        }
        let Some(generalized_id) = generalized.get("id").and_then(Value::as_str) else {
            continue;
        };
        if generalized_id == diff_id
            || !differential_id_generalizes_sliced_id(generalized_id, diff_id)
        {
            continue;
        }
        merge_diff_into_element(
            target,
            &generalized_diff_for_sliced_target(generalized, generalized_id, diff_id),
            strip_non_inherited,
            preserve_common_binding,
            diff_must_support_ids,
            inherited_must_support_ids,
            original_ids,
            constraint_source,
        )?;
    }
    Ok(())
}

pub(crate) fn generalized_diff_for_sliced_target(
    generalized: &Value,
    generalized_id: &str,
    sliced_id: &str,
) -> Value {
    if has_slice_marker(generalized_id) || !has_slice_marker(sliced_id) {
        return generalized.clone();
    }
    let mut cloned = generalized.clone();
    remove_field(&mut cloned, "short");
    if generalized_id.ends_with("[x]") && sliced_id.ends_with("[x]") {
        remove_type_extensions(&mut cloned);
    }
    cloned
}

pub(crate) fn remove_type_extensions(element: &mut Value) {
    let Some(types) = element.get_mut("type").and_then(Value::as_array_mut) else {
        return;
    };
    for ty in types {
        remove_field(ty, "extension");
    }
}

pub(crate) fn differential_id_generalizes_sliced_id(generalized_id: &str, sliced_id: &str) -> bool {
    let generalized_segments: Vec<&str> = generalized_id.split('.').collect();
    let sliced_segments: Vec<&str> = sliced_id.split('.').collect();
    if generalized_segments.len() != sliced_segments.len() {
        return false;
    }

    let mut specialized = false;
    for (generalized, sliced) in generalized_segments.iter().zip(sliced_segments.iter()) {
        let (generalized_base, generalized_has_slice) = segment_base_and_slice_marker(generalized);
        let (sliced_base, sliced_has_slice) = segment_base_and_slice_marker(sliced);
        if generalized_base != sliced_base {
            return false;
        }
        if generalized_has_slice {
            if generalized != sliced {
                return false;
            }
        } else if sliced_has_slice {
            specialized = true;
        } else if generalized != sliced {
            return false;
        }
    }
    specialized
}

pub(crate) fn segment_base_and_slice_marker(segment: &str) -> (&str, bool) {
    if let Some((base, _)) = segment.split_once(':') {
        return (base, true);
    }
    if let Some((base, _)) = segment.split_once('/') {
        return (base, true);
    }
    (segment, false)
}

pub(crate) fn close_inferred_type_slice_anchor(elements: &mut [Value], path: &str, diff: &Value) {
    let expected_anchor_id = diff
        .get("id")
        .and_then(Value::as_str)
        .and_then(slice_anchor_id_from_diff_id);
    let index = if let Some(anchor_id) = expected_anchor_id.as_deref() {
        elements
            .iter()
            .position(|candidate| candidate.get("id").and_then(Value::as_str) == Some(anchor_id))
    } else {
        elements.iter().position(|candidate| {
            candidate.get("path").and_then(Value::as_str) == Some(path)
                && candidate.get("sliceName").is_none()
        })
    };
    if let Some(index) = index {
        close_type_slicing_for_descendant_unfold(&mut elements[index]);
    }
}

pub(crate) fn reconcile_type_slicing_anchor_types(elements: &mut [Value]) {
    for index in 0..elements.len() {
        if !is_type_slicing(&elements[index]) {
            continue;
        }
        let Some(anchor_id) = elements[index]
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let Some(anchor_path) = elements[index]
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let mut live_type_codes = HashSet::new();
        let mut saw_zero_slice = false;
        let mut saw_direct_slice = false;
        let mut required_sum = 0u64;
        for slice in elements.iter() {
            if slice.get("path").and_then(Value::as_str) != Some(anchor_path.as_str()) {
                continue;
            }
            let Some(id) = slice.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !is_direct_slice_of(id, &anchor_id) {
                continue;
            }
            saw_direct_slice = true;
            required_sum += slice.get("min").and_then(Value::as_u64).unwrap_or(0);
            if slice.get("max").and_then(Value::as_str) == Some("0") {
                saw_zero_slice = true;
                continue;
            }
            if let Some(types) = slice.get("type").and_then(Value::as_array) {
                for ty in types {
                    if let Some(code) = ty.get("code").and_then(Value::as_str) {
                        live_type_codes.insert(code.to_string());
                    }
                }
            }
        }
        if live_type_codes.is_empty() {
            continue;
        }
        let Some(types) = elements[index].get("type").and_then(Value::as_array) else {
            continue;
        };
        let mut active_types: Vec<Value> = types.clone();
        if is_extension_value_anchor(&anchor_id, &anchor_path) && saw_direct_slice {
            let pruned: Vec<Value> = active_types
                .iter()
                .filter(|ty| {
                    ty.get("code")
                        .and_then(Value::as_str)
                        .is_some_and(|code| live_type_codes.contains(code))
                })
                .cloned()
                .collect();
            if !pruned.is_empty() && pruned.len() < active_types.len() {
                set_field(&mut elements[index], "type", Value::Array(pruned.clone()));
                close_type_slicing_for_descendant_unfold(&mut elements[index]);
                continue;
            }
        }
        let anchor_min = elements[index]
            .get("min")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let required_slices_cover_anchor =
            saw_direct_slice && required_sum > 0 && required_sum >= anchor_min;
        if saw_zero_slice {
            let pruned: Vec<Value> = active_types
                .iter()
                .filter(|ty| {
                    ty.get("code")
                        .and_then(Value::as_str)
                        .is_some_and(|code| live_type_codes.contains(code))
                })
                .cloned()
                .collect();
            if !pruned.is_empty() && pruned.len() < active_types.len() {
                set_field(&mut elements[index], "type", Value::Array(pruned.clone()));
                active_types = pruned;
            }
        }
        if required_slices_cover_anchor {
            if required_sum > anchor_min {
                set_field(
                    &mut elements[index],
                    "min",
                    Value::Number(required_sum.into()),
                );
            }
            let pruned: Vec<Value> = active_types
                .iter()
                .filter(|ty| {
                    ty.get("code")
                        .and_then(Value::as_str)
                        .is_some_and(|code| live_type_codes.contains(code))
                })
                .cloned()
                .collect();
            if !pruned.is_empty() && pruned.len() < active_types.len() {
                set_field(&mut elements[index], "type", Value::Array(pruned.clone()));
                active_types = pruned;
            }
        }
        let direct_slices_cover_types = saw_direct_slice
            && active_types.iter().all(|ty| {
                ty.get("code")
                    .and_then(Value::as_str)
                    .is_some_and(|code| live_type_codes.contains(code))
            });
        if direct_slices_cover_types {
            close_type_slicing_for_descendant_unfold(&mut elements[index]);
        }
    }
}

pub(crate) fn is_extension_value_anchor(id: &str, path: &str) -> bool {
    id == "Extension.value[x]" || path == "Extension.value[x]"
}

pub(crate) fn sort_type_slice_groups_by_differential_order(
    elements: &mut Vec<Value>,
    diff_slice_anchor_ids: &HashSet<String>,
    diff_slice_orders: &HashMap<String, usize>,
) {
    let mut anchor_index = 0;
    while anchor_index < elements.len() {
        if !is_type_slicing(&elements[anchor_index]) {
            anchor_index += 1;
            continue;
        }
        let Some(anchor_id) = elements[anchor_index]
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            anchor_index += 1;
            continue;
        };
        if !diff_slice_anchor_ids.contains(&anchor_id) {
            anchor_index += 1;
            continue;
        }
        let Some(anchor_path) = elements[anchor_index]
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            anchor_index += 1;
            continue;
        };

        let mut groups = Vec::new();
        let mut pos = anchor_index + 1;
        while pos < elements.len() {
            let Some(id) = elements[pos].get("id").and_then(Value::as_str) else {
                break;
            };
            if !is_slice_or_descendant_of(id, &anchor_id) {
                break;
            }
            if elements[pos].get("path").and_then(Value::as_str) == Some(anchor_path.as_str())
                && is_direct_slice_of(id, &anchor_id)
            {
                let start = pos;
                let slice_id = id.to_string();
                pos += 1;
                while pos < elements.len() {
                    let Some(next_id) = elements[pos].get("id").and_then(Value::as_str) else {
                        break;
                    };
                    if is_direct_slice_of(next_id, &anchor_id) {
                        break;
                    }
                    if !is_slice_or_descendant_of(next_id, &slice_id) {
                        break;
                    }
                    pos += 1;
                }
                groups.push(TypeSliceGroup {
                    start,
                    end: pos,
                    order: diff_slice_orders
                        .get(&slice_id)
                        .copied()
                        .unwrap_or(usize::MAX),
                });
            } else {
                pos += 1;
            }
        }

        let mut segment_start = 0;
        while segment_start < groups.len() {
            let mut segment_end = segment_start + 1;
            while segment_end < groups.len()
                && groups[segment_end - 1].end == groups[segment_end].start
            {
                segment_end += 1;
            }
            if segment_end - segment_start > 1 {
                sort_adjacent_type_slice_segment(elements, &groups[segment_start..segment_end]);
            }
            segment_start = segment_end;
        }

        anchor_index += 1;
    }
}

#[derive(Clone, Copy)]
pub(crate) struct TypeSliceGroup {
    start: usize,
    end: usize,
    order: usize,
}

pub(crate) fn sort_adjacent_type_slice_segment(elements: &mut Vec<Value>, groups: &[TypeSliceGroup]) {
    let start = groups[0].start;
    let end = groups[groups.len() - 1].end;
    let mut reordered: Vec<(usize, usize, Vec<Value>)> = groups
        .iter()
        .enumerate()
        .map(|(original, group)| {
            (
                group.order,
                original,
                elements[group.start..group.end].to_vec(),
            )
        })
        .collect();
    reordered.sort_by_key(|(order, original, _)| (*order, *original));
    let replacement: Vec<Value> = reordered
        .into_iter()
        .flat_map(|(_, _, values)| values)
        .collect();
    elements.splice(start..end, replacement);
}

pub(crate) fn insert_slice_element(
    elements: &mut Vec<Value>,
    path: &str,
    diff: &Value,
    ctx: &PackageContext,
    original_elements_by_id: &HashMap<String, Value>,
    strip_non_inherited: bool,
    preserve_common_binding: bool,
    native_r5: bool,
    host_extension_source: &str,
    base_spec_url: &str,
    explicit_slicing_paths: &HashSet<String>,
    diff_ids: &HashSet<String>,
    diff_must_support_ids: &HashSet<String>,
    diff_preserve_must_support_ids: &HashSet<String>,
    diff_condition_ids: &HashSet<String>,
) -> anyhow::Result<()> {
    let expected_anchor_id = diff
        .get("id")
        .and_then(Value::as_str)
        .and_then(slice_anchor_id_from_diff_id);
    let anchor = elements
        .iter()
        .position(|candidate| {
            expected_anchor_id
                .as_deref()
                .is_some_and(|id| candidate.get("id").and_then(Value::as_str) == Some(id))
        })
        .or_else(|| {
            elements.iter().position(|candidate| {
                candidate.get("path").and_then(Value::as_str) == Some(path)
                    && candidate.get("sliceName").is_none()
            })
        })
        .with_context(|| format!("slice anchor not found for {path}"))?;

    let anchor_id = elements[anchor]
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(path)
        .to_string();
    let unsliced_anchor_id = anchor_id.clone();
    // Java only drops the inherited unsliced datatype children when the base
    // element was already sliced (ProfilePathProcessor.processPathWithSlicedBase,
    // e.g. CRD Practitioner.identifier). When this profile newly introduces the
    // slicing on a previously-unsliced datatype element, processSimplePathDefault
    // keeps the unsliced children (e.g. CARIN BB Patient.identifier).
    let base_anchor_was_sliced = original_elements_by_id
        .get(&unsliced_anchor_id)
        .map(|element| element.get("slicing").is_some())
        .unwrap_or(false);
    let diff_id = diff.get("id").and_then(Value::as_str);
    let mut slice = diff_id
        .and_then(|id| original_elements_by_id.get(id).cloned())
        .unwrap_or_else(|| {
            if anchor_id.contains(':') || anchor_id.contains('/') {
                unsliced_element_id(&anchor_id)
                    .and_then(|id| original_elements_by_id.get(&id).cloned())
                    .unwrap_or_else(|| elements[anchor].clone())
            } else {
                original_elements_by_id
                    .get(&anchor_id)
                    .cloned()
                    .unwrap_or_else(|| elements[anchor].clone())
            }
        });
    fill_missing_constraint_sources_on_constrained_element(&mut slice, host_extension_source);
    remove_field(&mut slice, "slicing");
    if let Some(id) = diff.get("id") {
        set_field(&mut slice, "id", id.clone());
    }
    if let Some(path) = diff.get("path") {
        set_field(&mut slice, "path", path.clone());
    }
    if diff.get("min").is_none() {
        set_field(&mut slice, "min", Value::Number(0.into()));
    }
    reset_slice_condition_to_original(
        &mut slice,
        diff_id,
        &unsliced_anchor_id,
        &elements[anchor],
        original_elements_by_id,
        diff_condition_ids,
    );
    inherit_resolved_content_reference_state(&mut slice, &elements[anchor]);
    apply_content_reference_slice_root_type(
        &mut slice,
        &elements[anchor],
        diff,
        elements,
        host_extension_source,
        ctx,
    );
    if first_extension_profile_url(diff).is_some() {
        if let Some(t) = diff.get("type") {
            set_field(&mut slice, "type", t.clone());
        }
        let inherited_slicing = original_elements_by_id
            .get(&unsliced_anchor_id)
            .map(|element| element.get("slicing").is_some())
            .unwrap_or_else(|| elements[anchor].get("slicing").is_some())
            && !explicit_slicing_paths.contains(path);
        let allow_condition = extension_condition_context_allows(elements, path, inherited_slicing);
        apply_extension_profile_root(
            &mut slice,
            diff,
            ctx,
            native_r5,
            Some(host_extension_source),
            true,
            !inherited_slicing,
            allow_condition,
        )?;
    }
    merge_diff_into_element(
        &mut slice,
        diff,
        strip_non_inherited,
        preserve_common_binding,
        Some(diff_must_support_ids),
        None,
        None,
        host_extension_source,
    )?;

    let anchor_path = elements[anchor]
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or(path)
        .to_string();
    materialize_content_reference_children_for_slice_anchor(
        elements,
        anchor,
        &anchor_id,
        &anchor_path,
        host_extension_source,
        base_spec_url,
        ctx,
        native_r5,
    )?;
    let anchor_had_inherited_slicing = original_elements_by_id
        .get(&anchor_id)
        .map(|element| element.get("slicing").is_some())
        .unwrap_or(false);
    if !anchor_had_inherited_slicing && !explicit_slicing_paths.contains(path) {
        close_type_slicing_for_descendant_unfold(&mut elements[anchor]);
    }
    if is_plan_definition_recursive_action_anchor(&elements[anchor]) {
        prune_recursive_action_unsliced_tail(elements, &anchor_id);
        ensure_recursive_action_trigger_element_children(elements, &anchor_id, ctx, native_r5)?;
    }

    // Java only drops the inherited unsliced datatype children when the
    // differential adds a slice without constraining any of those unsliced
    // children (CRD Practitioner.identifier, TWPAS identifier slices). An
    // anchor-only slicing/cardinality row is not enough to keep them, except
    // for newly introduced Coding slicing where Java keeps the unsliced coding
    // children alongside the new slices (AU Core Medication.code.coding). An
    // unsliced descendant constraint also keeps them (ndh
    // Organization.identifier.assigner / .extension:identifier-status).
    let unsliced_child_prefix = format!("{unsliced_anchor_id}.");
    let differential_constrains_unsliced_child = diff_ids
        .iter()
        .any(|id| id.starts_with(&unsliced_child_prefix))
        || (diff_ids.contains(&unsliced_anchor_id)
            && should_prune_newly_sliced_coding_descendants(&elements[anchor]));
    if (base_anchor_was_sliced || should_prune_newly_sliced_coding_descendants(&elements[anchor]))
        && !differential_constrains_unsliced_child
        && should_prune_unsliced_descendants_for_slice_anchor(&elements[anchor])
    {
        prune_unsliced_descendants(elements, &anchor_id);
    }

    let mut insert_at = anchor + 1;
    while insert_at < elements.len()
        && elements[insert_at]
            .get("id")
            .and_then(Value::as_str)
            .map(|id| is_slice_or_descendant_of(id, &anchor_id))
            .unwrap_or(false)
    {
        insert_at += 1;
    }
    let materialize_extension_children =
        should_materialize_extension_profile_children_on_insert(&slice);
    let slice_id = slice
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(path)
        .to_string();
    let slice_path = slice
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or(path)
        .to_string();
    let extension_profile = first_extension_profile_url(&slice).map(str::to_string);
    elements.insert(insert_at, slice);
    if !materialize_extension_children
        && should_eagerly_unfold_direct_slice_children(&elements[insert_at])
        && (diff_ids
            .iter()
            .any(|diff_id| diff_id.starts_with(&format!("{slice_id}.")))
            || should_materialize_direct_slice_from_unsliced_children(
                &elements[insert_at],
                diff_ids,
            ))
    {
        unfold_sliced_parent_from_anchor(
            elements,
            insert_at,
            &slice_id,
            &slice_path,
            Some(original_elements_by_id),
            Some(diff_ids),
            Some(diff_preserve_must_support_ids),
            Some(diff_must_support_ids),
        );
    }
    if materialize_extension_children {
        if let Some(profile_url) = extension_profile {
            materialize_extension_profile_children_for_slice(
                elements,
                insert_at,
                &slice_id,
                &slice_path,
                &profile_url,
                ctx,
                native_r5,
            )?;
        }
    }
    Ok(())
}

pub(crate) fn should_eagerly_unfold_direct_slice_children(slice: &Value) -> bool {
    if slice.get("contentReference").is_some() {
        return false;
    }
    let Some(types) = slice.get("type").and_then(Value::as_array) else {
        return false;
    };
    !types.iter().any(|ty| {
        matches!(
            ty.get("code").and_then(Value::as_str),
            Some("BackboneElement" | "Element")
        )
    })
}

pub(crate) fn inherit_resolved_content_reference_state(slice: &mut Value, anchor: &Value) {
    if slice.get("contentReference").is_none() || anchor.get("contentReference").is_some() {
        return;
    }
    remove_field(slice, "contentReference");
    if slice.get("type").is_none() {
        if let Some(t) = anchor.get("type") {
            set_field(slice, "type", t.clone());
        }
    }
}

pub(crate) fn apply_content_reference_slice_root_type(
    slice: &mut Value,
    anchor: &Value,
    diff: &Value,
    elements: &[Value],
    base_url: &str,
    ctx: &PackageContext,
) {
    if diff.get("contentReference").is_some()
        || slice.get("contentReference").is_none()
        || anchor.get("contentReference").is_none()
    {
        return;
    }
    let Some(content_reference) = anchor.get("contentReference").and_then(Value::as_str) else {
        return;
    };
    let Some(target) = content_reference_target(content_reference, base_url, elements, ctx) else {
        return;
    };
    remove_field(slice, "contentReference");
    if slice.get("type").is_none() {
        if let Some(t) = target.get("type") {
            set_field(slice, "type", t.clone());
        }
    }
}

pub(crate) fn content_reference_target(
    content_reference: &str,
    base_url: &str,
    elements: &[Value],
    ctx: &PackageContext,
) -> Option<Value> {
    let (source_url, target_id) = split_content_reference(content_reference, base_url)?;
    if source_url == base_url {
        return elements
            .iter()
            .find(|candidate| candidate.get("id").and_then(Value::as_str) == Some(&target_id))
            .cloned();
    }
    let source = ctx.fetch(&source_url)?;
    source
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .and_then(|source_elements| {
            source_elements
                .iter()
                .find(|candidate| candidate.get("id").and_then(Value::as_str) == Some(&target_id))
                .cloned()
        })
}

pub(crate) fn should_materialize_existing_direct_slice_children(
    slice: &Value,
    diff: &Value,
    original_elements_by_id: &HashMap<String, Value>,
    diff_ids: &HashSet<String>,
) -> bool {
    let Some(id) = slice.get("id").and_then(Value::as_str) else {
        return false;
    };
    if !has_slice_marker(id) {
        return false;
    }
    if original_elements_by_id.contains_key(id) && is_must_support_only_slice_diff(diff) {
        return false;
    }
    let child_prefix = format!("{id}.");
    if original_elements_by_id
        .keys()
        .any(|original_id| original_id.starts_with(&child_prefix))
    {
        return false;
    }
    if !diff_ids
        .iter()
        .any(|diff_id| diff_id.starts_with(&child_prefix))
        && !should_materialize_direct_slice_from_unsliced_children(slice, diff_ids)
    {
        return false;
    }
    if slice
        .get("type")
        .and_then(Value::as_array)
        .and_then(|types| types.first())
        .and_then(|ty| ty.get("code"))
        .and_then(Value::as_str)
        == Some("Coding")
        && !should_materialize_coding_slice_from_unsliced_children(slice, diff_ids)
    {
        return false;
    }
    should_eagerly_unfold_direct_slice_children(slice)
}

pub(crate) fn is_must_support_only_slice_diff(diff: &Value) -> bool {
    let Some(obj) = diff.as_object() else {
        return false;
    };
    diff.get("mustSupport").is_some()
        && obj
            .keys()
            .all(|key| matches!(key.as_str(), "id" | "path" | "sliceName" | "mustSupport"))
}

pub(crate) fn should_materialize_identifier_slice_from_unsliced_children(
    slice: &Value,
    diff_ids: &HashSet<String>,
) -> bool {
    let is_identifier_path = slice
        .get("path")
        .and_then(Value::as_str)
        .is_some_and(|path| path.ends_with(".identifier"));
    if slice.get("max").and_then(Value::as_str) == Some("0")
        || !has_fixed_or_pattern_value(slice)
        || !is_identifier_path
    {
        return false;
    }
    let Some(id) = slice.get("id").and_then(Value::as_str) else {
        return false;
    };
    let Some(anchor_id) = immediate_slice_anchor_id(id) else {
        return false;
    };
    diff_ids.contains(&format!("{anchor_id}.system"))
        || diff_ids.contains(&format!("{anchor_id}.value"))
}

pub(crate) fn should_materialize_direct_slice_from_unsliced_children(
    slice: &Value,
    diff_ids: &HashSet<String>,
) -> bool {
    should_materialize_identifier_slice_from_unsliced_children(slice, diff_ids)
        || should_materialize_coding_slice_from_unsliced_children(slice, diff_ids)
}

pub(crate) fn should_materialize_coding_slice_from_unsliced_children(
    slice: &Value,
    diff_ids: &HashSet<String>,
) -> bool {
    if slice.get("max").and_then(Value::as_str) == Some("0") {
        return false;
    }
    if slice.get("binding").is_none() && !has_fixed_or_pattern_value(slice) {
        return false;
    }
    if slice
        .get("type")
        .and_then(Value::as_array)
        .and_then(|types| types.first())
        .and_then(|ty| ty.get("code"))
        .and_then(Value::as_str)
        != Some("Coding")
    {
        return false;
    }
    let Some(id) = slice.get("id").and_then(Value::as_str) else {
        return false;
    };
    let Some(anchor_id) = immediate_slice_anchor_id(id) else {
        return false;
    };
    diff_ids.contains(&format!("{anchor_id}.system"))
        || diff_ids.contains(&format!("{anchor_id}.code"))
        || (slice.get("max").and_then(Value::as_str) == Some("*")
            && slice.get("binding").is_some()
            && has_semantic_element_extensions(slice))
}

pub(crate) fn should_materialize_extension_profile_children_on_insert(slice: &Value) -> bool {
    let Some(profile_url) = first_extension_profile_url(slice) else {
        return false;
    };
    let bare_url = profile_url
        .split_once('|')
        .map(|(url, _)| url)
        .unwrap_or(profile_url);
    bare_url == "http://hl7.org/fhir/StructureDefinition/cqf-fhirQueryPattern"
        && slice
            .get("base")
            .and_then(|b| b.get("path"))
            .and_then(Value::as_str)
            == Some("Element.extension")
}

pub(crate) fn materialize_extension_profile_children_for_slice(
    elements: &mut Vec<Value>,
    slice_index: usize,
    slice_id: &str,
    slice_path: &str,
    profile_url: &str,
    ctx: &PackageContext,
    native_r5: bool,
) -> anyhow::Result<()> {
    let child_prefix = format!("{slice_id}.");
    if elements.iter().any(|candidate| {
        candidate
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .starts_with(&child_prefix)
    }) {
        return Ok(());
    }
    let Some(profile) = profile_with_snapshot(profile_url, ctx, native_r5)? else {
        return Ok(());
    };
    let Some(profile_elements) = profile
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    let Some(root) = profile_elements.first() else {
        return Ok(());
    };
    let root_id = root
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("Extension")
        .to_string();
    let root_path = root
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("Extension")
        .to_string();
    let profile_url = structure_url_or(&profile, profile_url);
    let profile_spec_url = spec_url_for_structure(&profile, native_r5);
    let strip_non_inherited = native_r5 || strips_non_inherited_extensions(&profile);
    let profile_source = structure_source(&profile, &profile_url);
    let snapshot_source = snapshot_source_value(&profile);
    let mut children = Vec::new();
    for child in profile_elements.iter().skip(1) {
        let mut clone = normalize_inherited_element(
            child.clone(),
            &profile_url,
            &profile_spec_url,
            strip_non_inherited,
            native_r5,
            &profile_source,
            snapshot_source.as_deref(),
            false,
        );
        if let Some(id) = clone.get("id").and_then(Value::as_str).map(str::to_string) {
            set_field(
                &mut clone,
                "id",
                Value::String(id.replacen(&root_id, slice_id, 1)),
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
                Value::String(path.replacen(&root_path, slice_path, 1)),
            );
        }
        apply_cqf_fhir_query_pattern_id_child_quirks(&mut clone, &profile_url);
        children.push(clone);
    }
    for (offset, child) in children.into_iter().enumerate() {
        elements.insert(slice_index + 1 + offset, child);
    }
    Ok(())
}

pub(crate) fn materialize_content_reference_children_for_slice_anchor(
    elements: &mut Vec<Value>,
    anchor_index: usize,
    anchor_id: &str,
    anchor_path: &str,
    base_url: &str,
    base_spec_url: &str,
    ctx: &PackageContext,
    native_r5: bool,
) -> anyhow::Result<()> {
    let child_prefix = format!("{anchor_id}.");
    if elements.iter().any(|candidate| {
        candidate
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .starts_with(&child_prefix)
    }) {
        return Ok(());
    }

    let Some(content_reference) = elements[anchor_index]
        .get("contentReference")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Ok(());
    };
    let Some((source_url, target_id)) = split_content_reference(&content_reference, base_url)
    else {
        return Ok(());
    };

    let (target, source_children, source_spec_url, source_strip_non_inherited) =
        if source_url == base_url {
            let (target, children) = collect_content_reference_source(elements, &target_id);
            (
                target,
                children,
                base_spec_url.to_string(),
                native_r5 || base_spec_url.contains("/R5/"),
            )
        } else {
            let Some(source) = ctx.fetch(&source_url) else {
                return Ok(());
            };
            let source_spec_url = spec_url_for_structure(&source, native_r5);
            let source_strip_non_inherited = native_r5 || strips_non_inherited_extensions(&source);
            let source_owned = source
                .get("snapshot")
                .and_then(|s| s.get("element"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let (target, children) = collect_content_reference_source(&source_owned, &target_id);
            (
                target,
                children,
                source_spec_url,
                source_strip_non_inherited,
            )
        };
    let Some(target) = target else {
        return Ok(());
    };
    let Some(target_path) = target
        .get("path")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Ok(());
    };

    let source_snapshot_source = if source_url == base_url {
        None
    } else {
        ctx.fetch(&source_url)
            .and_then(|source| snapshot_source_value(&source))
    };
    let target_prefix = format!("{target_id}.");
    let mut children = Vec::new();
    for child in source_children {
        let child_id = child.get("id").and_then(Value::as_str).unwrap_or("");
        if !child_id.starts_with(&target_prefix) {
            continue;
        }
        let mut clone = normalize_inherited_element(
            child.clone(),
            &source_url,
            &source_spec_url,
            source_strip_non_inherited,
            native_r5,
            &source_url,
            source_snapshot_source.as_deref(),
            false,
        );
        if let Some(id) = clone.get("id").and_then(Value::as_str).map(str::to_string) {
            set_field(
                &mut clone,
                "id",
                Value::String(id.replacen(&target_id, anchor_id, 1)),
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
                Value::String(path.replacen(&target_path, anchor_path, 1)),
            );
        }
        absolutize_content_reference(&mut clone, &source_url);
        children.push(clone);
    }

    let insert_at = anchor_index + 1;
    for (offset, child) in children.into_iter().enumerate() {
        elements.insert(insert_at + offset, child);
    }
    Ok(())
}

pub(crate) fn should_prune_unsliced_descendants_for_slice_anchor(anchor: &Value) -> bool {
    let Some(type_code) = anchor
        .get("type")
        .and_then(Value::as_array)
        .and_then(|types| types.first())
        .and_then(|ty| ty.get("code"))
        .and_then(Value::as_str)
    else {
        return false;
    };
    !matches!(type_code, "BackboneElement" | "Element" | "Extension")
}

pub(crate) fn should_prune_newly_sliced_coding_descendants(anchor: &Value) -> bool {
    let path = anchor.get("path").and_then(Value::as_str).unwrap_or("");
    let type_code = anchor
        .get("type")
        .and_then(Value::as_array)
        .and_then(|types| types.first())
        .and_then(|ty| ty.get("code"))
        .and_then(Value::as_str);
    path.ends_with(".coding") && type_code == Some("Coding")
}

pub(crate) fn prune_unsliced_descendants(elements: &mut Vec<Value>, anchor_id: &str) {
    let prefix = format!("{anchor_id}.");
    elements.retain(|candidate| {
        let id = candidate.get("id").and_then(Value::as_str).unwrap_or("");
        !id.starts_with(&prefix)
    });
}

pub(crate) fn prune_unsliced_descendants_for_slice_diff(
    elements: &mut Vec<Value>,
    diff: &Value,
    diff_ids: &HashSet<String>,
    original_elements_by_id: &HashMap<String, Value>,
) {
    let Some(anchor_id) = diff
        .get("id")
        .and_then(Value::as_str)
        .and_then(slice_anchor_id_from_diff_id)
    else {
        return;
    };
    let Some(anchor_index) = elements
        .iter()
        .position(|element| element.get("id").and_then(Value::as_str) == Some(anchor_id.as_str()))
    else {
        return;
    };
    let unsliced_child_prefix = format!("{anchor_id}.");
    if diff_ids
        .iter()
        .any(|id| id.starts_with(&unsliced_child_prefix))
        || (diff_ids.contains(&anchor_id)
            && should_prune_newly_sliced_coding_descendants(&elements[anchor_index]))
    {
        return;
    }
    let base_anchor_was_sliced = original_elements_by_id
        .get(&anchor_id)
        .map(|element| element.get("slicing").is_some())
        .unwrap_or(false);
    if (base_anchor_was_sliced
        || should_prune_newly_sliced_coding_descendants(&elements[anchor_index]))
        && should_prune_unsliced_descendants_for_slice_anchor(&elements[anchor_index])
    {
        prune_unsliced_descendants(elements, &anchor_id);
    }
}

pub(crate) fn should_prune_profiled_unsliced_descendants(
    element: &Value,
    diff: &Value,
    diff_ids: &HashSet<String>,
    original_elements_by_id: &HashMap<String, Value>,
) -> bool {
    if diff.get("sliceName").is_some() || first_non_extension_profile_url(diff).is_none() {
        return false;
    }
    let Some(id) = element.get("id").and_then(Value::as_str) else {
        return false;
    };
    if has_slice_marker(id) {
        return false;
    }
    let inherited_slicing = original_elements_by_id
        .get(id)
        .map(|original| original.get("slicing").is_some())
        .unwrap_or(false);
    if !inherited_slicing || !should_prune_unsliced_descendants_for_slice_anchor(element) {
        return false;
    }
    let child_prefix = format!("{id}.");
    !diff_ids
        .iter()
        .any(|candidate| candidate.starts_with(&child_prefix))
}

pub(crate) fn reset_slice_condition_to_original(
    slice: &mut Value,
    diff_id: Option<&str>,
    unsliced_anchor_id: &str,
    current_anchor: &Value,
    original_elements_by_id: &HashMap<String, Value>,
    diff_condition_ids: &HashSet<String>,
) {
    let original_condition = diff_id
        .and_then(|id| original_elements_by_id.get(id))
        .and_then(|element| element.get("condition"))
        .or_else(|| {
            original_elements_by_id
                .get(unsliced_anchor_id)
                .and_then(|element| element.get("condition"))
        })
        .or_else(|| {
            (!diff_condition_ids.contains(unsliced_anchor_id))
                .then(|| current_anchor.get("condition"))
                .flatten()
        })
        .or_else(|| {
            unsliced_element_id(unsliced_anchor_id).and_then(|id| {
                original_elements_by_id
                    .get(&id)
                    .and_then(|element| element.get("condition"))
            })
        })
        .cloned();
    if let Some(condition) = original_condition {
        set_field(slice, "condition", condition);
    } else {
        remove_field(slice, "condition");
    }
}

pub(crate) fn extension_condition_context_allows(
    elements: &[Value],
    extension_path: &str,
    inherited_slicing: bool,
) -> bool {
    if inherited_slicing {
        return false;
    }
    let Some(parent_path) = extension_path
        .strip_suffix(".extension")
        .or_else(|| extension_path.strip_suffix(".modifierExtension"))
    else {
        return true;
    };
    if !parent_path.contains('.') {
        return true;
    }
    elements
        .iter()
        .find(|element| element.get("path").and_then(Value::as_str) == Some(parent_path))
        .and_then(|element| element.get("type").and_then(Value::as_array))
        .map(|types| {
            types.iter().any(|ty| {
                matches!(
                    ty.get("code").and_then(Value::as_str),
                    Some("BackboneElement")
                )
            })
        })
        .unwrap_or(false)
}

pub(crate) fn unfold_parent_for_diff(
    elements: &mut Vec<Value>,
    diff: &Value,
    ctx: &PackageContext,
    base_url: &str,
    base_spec_url: &str,
    native_r5: bool,
    original_elements: &[Value],
) -> anyhow::Result<()> {
    let Some(diff_id) = diff.get("id").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(dot) = diff_id.rfind('.') else {
        return Ok(());
    };
    let parent_id = &diff_id[..dot];
    unfold_parent_id(
        elements,
        parent_id,
        ctx,
        base_url,
        base_spec_url,
        native_r5,
        original_elements,
    )
}

pub(crate) fn unfold_parent_id(
    elements: &mut Vec<Value>,
    parent_id: &str,
    ctx: &PackageContext,
    base_url: &str,
    base_spec_url: &str,
    native_r5: bool,
    original_elements: &[Value],
) -> anyhow::Result<()> {
    let mut parent_index = elements
        .iter()
        .position(|candidate| candidate.get("id").and_then(Value::as_str) == Some(parent_id));
    if parent_index.is_none() {
        if let Some(dot) = parent_id.rfind('.') {
            unfold_parent_id(
                elements,
                &parent_id[..dot],
                ctx,
                base_url,
                base_spec_url,
                native_r5,
                original_elements,
            )?;
            parent_index = elements.iter().position(|candidate| {
                candidate.get("id").and_then(Value::as_str) == Some(parent_id)
            });
        }
    }
    let Some(parent_index) = parent_index else {
        return Ok(());
    };
    close_type_slicing_for_descendant_unfold(&mut elements[parent_index]);

    let child_prefix = format!("{parent_id}.");
    let has_children = elements.iter().any(|candidate| {
        candidate
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .starts_with(&child_prefix)
    });
    if has_children {
        let existing_parent_path = elements[parent_index]
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(parent_id)
            .to_string();
        if unfold_content_reference_parent(
            elements,
            parent_index,
            parent_id,
            &existing_parent_path,
            base_url,
            base_spec_url,
            ctx,
            native_r5,
            original_elements,
        )? {
            return Ok(());
        }
        let original_elements_by_id: HashMap<String, Value> = original_elements
            .iter()
            .filter_map(|element| {
                element
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|id| (id.to_string(), element.clone()))
            })
            .collect();
        unfold_sliced_parent_from_anchor(
            elements,
            parent_index,
            parent_id,
            &existing_parent_path,
            Some(&original_elements_by_id),
            None,
            None,
            None,
        );
        return Ok(());
    }

    let Some(parent_path) = elements[parent_index]
        .get("path")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Ok(());
    };

    let original_elements_by_id: HashMap<String, Value> = original_elements
        .iter()
        .filter_map(|element| {
            element
                .get("id")
                .and_then(Value::as_str)
                .map(|id| (id.to_string(), element.clone()))
        })
        .collect();
    if unfold_sliced_parent_from_anchor(
        elements,
        parent_index,
        parent_id,
        &parent_path,
        Some(&original_elements_by_id),
        None,
        None,
        None,
    ) {
        return Ok(());
    }

    if unfold_content_reference_parent(
        elements,
        parent_index,
        parent_id,
        &parent_path,
        base_url,
        base_spec_url,
        ctx,
        native_r5,
        original_elements,
    )? {
        return Ok(());
    }

    let Some(type_entries) = elements[parent_index].get("type").and_then(Value::as_array) else {
        return Ok(());
    };
    let parent_profile_url = single_non_extension_profile_url(&elements[parent_index])
        .or_else(|| first_extension_profile_url(&elements[parent_index]))
        .map(str::to_string);
    let type_code = if parent_profile_url.is_none() && type_entries.len() > 1 {
        "Element".to_string()
    } else {
        let Some(type_code) = type_entries
            .first()
            .and_then(|t| t.get("code"))
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return Ok(());
        };
        type_code
    };
    let type_def = parent_profile_url
        .as_deref()
        .and_then(|url| profile_with_snapshot(url, ctx, native_r5).transpose())
        .transpose()?
        .or_else(|| ctx.fetch(&type_code));
    let Some(type_def) = type_def else {
        return Ok(());
    };
    let Some(type_elements) = type_def
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
        .unwrap_or(&type_code)
        .to_string();
    let root_path = root
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or(&type_code)
        .to_string();

    let type_url = structure_url_or(
        &type_def,
        parent_profile_url.as_deref().unwrap_or(&type_code),
    );
    let type_spec_url = spec_url_for_structure(&type_def, native_r5);
    let type_strip_non_inherited = native_r5 || strips_non_inherited_extensions(&type_def);
    let type_source = structure_source(&type_def, &type_url);
    let type_snapshot_source = snapshot_source_value(&type_def);
    let parent_has_local_profile = parent_profile_url
        .as_deref()
        .map(|url| ctx.is_local(url))
        .unwrap_or(false);
    let mut children = Vec::new();
    for child in type_elements.iter().skip(1) {
        let mut clone = normalize_inherited_element(
            child.clone(),
            &type_url,
            &type_spec_url,
            type_strip_non_inherited,
            native_r5,
            &type_source,
            type_snapshot_source.as_deref(),
            parent_has_local_profile,
        );
        rehome_unfolded_type_constraint_sources(&mut clone, &type_url, base_url);
        if let Some(id) = clone.get("id").and_then(Value::as_str).map(str::to_string) {
            set_field(
                &mut clone,
                "id",
                Value::String(id.replacen(&root_id, parent_id, 1)),
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
                Value::String(path.replacen(&root_path, &parent_path, 1)),
            );
        }
        if let Some(profile_url) = parent_profile_url.as_deref() {
            apply_cqf_fhir_query_pattern_id_child_quirks(&mut clone, profile_url);
        }
        children.push(clone);
    }

    let insert_at = parent_index + 1;
    for (offset, child) in children.into_iter().enumerate() {
        elements.insert(insert_at + offset, child);
    }
    Ok(())
}

pub(crate) fn close_type_slicing_for_descendant_unfold(element: &mut Value) {
    if !is_type_slicing(element) {
        return;
    }
    if let Some(slicing) = element.get_mut("slicing") {
        set_field(slicing, "rules", Value::String("closed".to_string()));
    }
}

pub(crate) fn is_type_slicing(element: &Value) -> bool {
    element
        .get("slicing")
        .and_then(|s| s.get("discriminator"))
        .and_then(Value::as_array)
        .map(|discriminators| {
            discriminators.iter().any(|d| {
                d.get("type").and_then(Value::as_str) == Some("type")
                    && d.get("path").and_then(Value::as_str) == Some("$this")
            })
        })
        .unwrap_or(false)
}

pub(crate) fn unfold_sliced_parent_from_anchor(
    elements: &mut Vec<Value>,
    parent_index: usize,
    parent_id: &str,
    parent_path: &str,
    original_elements_by_id: Option<&HashMap<String, Value>>,
    diff_ids: Option<&HashSet<String>>,
    diff_preserve_must_support_ids: Option<&HashSet<String>>,
    diff_must_support_ids: Option<&HashSet<String>>,
) -> bool {
    let Some(unsliced_id) = immediate_slice_anchor_id(parent_id) else {
        return false;
    };
    if unsliced_id == parent_id {
        return false;
    }
    let Some(anchor) = elements.iter().find(|candidate| {
        candidate.get("id").and_then(Value::as_str) == Some(unsliced_id.as_str())
    }) else {
        return false;
    };
    let Some(unsliced_path) = anchor
        .get("path")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return false;
    };

    let child_prefix = format!("{unsliced_id}.");
    let path_prefix = format!("{unsliced_path}.");
    let anchor_has_recursive_content_reference =
        has_descendant_content_reference_to_anchor(elements, &unsliced_id);
    let mut existing_ids: HashSet<String> = elements
        .iter()
        .filter_map(|element| {
            element
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let mut children = Vec::new();
    for child in elements.iter() {
        let child_id = child.get("id").and_then(Value::as_str).unwrap_or("");
        if !child_id.starts_with(&child_prefix) {
            continue;
        }
        let child_suffix = &child_id[unsliced_id.len()..];
        let rehomed_id = format!("{parent_id}{child_suffix}");
        if existing_ids.contains(&rehomed_id) {
            continue;
        }
        if should_skip_plan_definition_nested_action_trigger_child(parent_id, child_suffix) {
            continue;
        }
        let same_differential_child_slice = !anchor_has_recursive_content_reference
            && has_slice_marker(child_suffix)
            && original_elements_by_id.is_some_and(|original| !original.contains_key(child_id));
        if same_differential_child_slice
            && should_skip_same_differential_child_slice_for_target(
                child_id,
                parent_id,
                &unsliced_id,
                diff_ids,
            )
        {
            continue;
        }
        let mut clone = if !anchor_has_recursive_content_reference
            && should_clone_original_extension_anchor(child_id)
        {
            original_elements_by_id
                .and_then(|original| original.get(child_id))
                .cloned()
                .unwrap_or_else(|| child.clone())
        } else {
            child.clone()
        };
        strip_diff_owned_type_extensions_on_sliced_choice_child(
            &mut clone,
            parent_id,
            child_id,
            original_elements_by_id,
        );
        let preserve_must_support = diff_must_support_ids
            .is_some_and(|ids| ids.contains(child_id) || ids.contains(&rehomed_id))
            || diff_preserve_must_support_ids
                .is_some_and(|ids| ids.contains(child_id) || ids.contains(&rehomed_id))
            || should_preserve_identifier_slice_child_must_support(
                parent_id,
                parent_path,
                child_suffix,
                diff_must_support_ids,
            );
        if diff_must_support_ids.is_some() && !preserve_must_support {
            remove_field(&mut clone, "mustSupport");
        }
        set_field(&mut clone, "id", Value::String(rehomed_id.clone()));
        if let Some(child_path) = clone
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            if child_path.starts_with(&path_prefix) {
                set_field(
                    &mut clone,
                    "path",
                    Value::String(format!(
                        "{parent_path}{}",
                        &child_path[unsliced_path.len()..]
                    )),
                );
            }
        }
        let expected_content_reference = format!("#{unsliced_id}");
        if clone.get("contentReference").and_then(Value::as_str)
            == Some(expected_content_reference.as_str())
        {
            set_field(
                &mut clone,
                "contentReference",
                Value::String(format!("#{parent_id}")),
            );
        }
        children.push(clone);
        existing_ids.insert(rehomed_id);
    }
    if children.is_empty() {
        return false;
    }

    let insert_at = parent_index + 1;
    for (offset, child) in children.into_iter().enumerate() {
        elements.insert(insert_at + offset, child);
    }
    true
}

pub(crate) fn should_preserve_identifier_slice_child_must_support(
    parent_id: &str,
    parent_path: &str,
    child_suffix: &str,
    diff_must_support_ids: Option<&HashSet<String>>,
) -> bool {
    parent_path.ends_with(".identifier")
        && matches!(child_suffix, ".system" | ".value")
        && diff_must_support_ids.is_some_and(|ids| ids.contains(parent_id))
}

pub(crate) fn should_skip_same_differential_child_slice_for_target(
    child_id: &str,
    parent_id: &str,
    unsliced_id: &str,
    diff_ids: Option<&HashSet<String>>,
) -> bool {
    let Some(diff_ids) = diff_ids else {
        return true;
    };
    let child_suffix = child_id.strip_prefix(unsliced_id).unwrap_or("");
    let rehomed_id = format!("{parent_id}{child_suffix}");
    let Some(target_anchor_id) = immediate_slice_anchor_id(&rehomed_id) else {
        return true;
    };
    diff_ids.iter().any(|diff_id| {
        diff_id == &target_anchor_id
            || is_direct_slice_of(diff_id, &target_anchor_id)
            || diff_id.starts_with(&format!("{target_anchor_id}."))
    })
}

pub(crate) fn materialize_generalized_child_slices_for_direct_slices(
    elements: &mut Vec<Value>,
    original_elements_by_id: &HashMap<String, Value>,
    diff_ids: &HashSet<String>,
    ctx: &PackageContext,
    native_r5: bool,
) -> anyhow::Result<()> {
    let source_elements = elements.clone();
    let mut existing_ids: HashSet<String> = elements
        .iter()
        .filter_map(|element| {
            element
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    for child in source_elements {
        let Some(child_id) = child.get("id").and_then(Value::as_str) else {
            continue;
        };
        if original_elements_by_id.contains_key(child_id) {
            continue;
        }
        let Some(child_anchor_id) = immediate_slice_anchor_id(child_id) else {
            continue;
        };
        if !is_direct_slice_of(child_id, &child_anchor_id) {
            continue;
        }
        let Some((container_id, _)) = child_anchor_id.rsplit_once('.') else {
            continue;
        };
        if original_elements_by_id
            .get(container_id)
            .is_some_and(|element| element.get("slicing").is_some())
        {
            continue;
        }
        let child_suffix = child_id.strip_prefix(container_id).unwrap_or("");
        let target_slice_ids: Vec<String> = elements
            .iter()
            .filter_map(|element| element.get("id").and_then(Value::as_str))
            .filter(|id| is_direct_slice_of(id, container_id))
            .map(str::to_string)
            .collect();
        for target_slice_id in target_slice_ids {
            let rehomed_id = format!("{target_slice_id}{child_suffix}");
            if existing_ids.contains(&rehomed_id) {
                if should_materialize_extension_profile_children_on_insert(&child) {
                    if let Some(existing_index) = elements.iter().position(|element| {
                        element.get("id").and_then(Value::as_str) == Some(rehomed_id.as_str())
                    }) {
                        let slice_path = elements[existing_index]
                            .get("path")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        if let Some(profile_url) =
                            first_extension_profile_url(&child).map(str::to_string)
                        {
                            materialize_extension_profile_children_for_slice(
                                elements,
                                existing_index,
                                &rehomed_id,
                                &slice_path,
                                &profile_url,
                                ctx,
                                native_r5,
                            )?;
                        }
                    }
                }
                continue;
            }
            if should_skip_same_differential_child_slice_for_target(
                child_id,
                &target_slice_id,
                container_id,
                Some(diff_ids),
            ) {
                continue;
            }
            let Some(target_anchor_id) = immediate_slice_anchor_id(&rehomed_id) else {
                continue;
            };
            let Some(target_anchor_index) = elements.iter().position(|element| {
                element.get("id").and_then(Value::as_str) == Some(target_anchor_id.as_str())
            }) else {
                continue;
            };
            if target_anchor_id.ends_with(".extension")
                || target_anchor_id.ends_with(".modifierExtension")
            {
                let ordered_false =
                    extension_anchor_uses_ordered_false_slicing(elements, target_anchor_index);
                let target_anchor = &mut elements[target_anchor_index];
                check_extension_doco(target_anchor);
                if target_anchor.get("slicing").is_none() {
                    set_field(
                        target_anchor,
                        "slicing",
                        extension_url_slicing(ordered_false),
                    );
                }
            }
            let mut clone = child.clone();
            set_field(&mut clone, "id", Value::String(rehomed_id.clone()));
            let slice_path = clone
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let extension_profile =
                if should_materialize_extension_profile_children_on_insert(&clone) {
                    first_extension_profile_url(&clone).map(str::to_string)
                } else {
                    None
                };
            let mut insert_at = target_anchor_index + 1;
            while insert_at < elements.len()
                && elements[insert_at]
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| is_slice_or_descendant_of(id, &target_anchor_id))
            {
                insert_at += 1;
            }
            elements.insert(insert_at, clone);
            if let Some(profile_url) = extension_profile {
                materialize_extension_profile_children_for_slice(
                    elements,
                    insert_at,
                    &rehomed_id,
                    &slice_path,
                    &profile_url,
                    ctx,
                    native_r5,
                )?;
            }
            existing_ids.insert(rehomed_id);
        }
    }
    Ok(())
}

pub(crate) fn materialize_missing_extension_profile_children_for_slices(
    elements: &mut Vec<Value>,
    ctx: &PackageContext,
    native_r5: bool,
) -> anyhow::Result<()> {
    let mut index = 0;
    while index < elements.len() {
        if let Some(profile_url) = cqf_fhir_query_pattern_profile_url(&elements[index]) {
            let slice_id = elements[index]
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let slice_path = elements[index]
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            materialize_extension_profile_children_for_slice(
                elements,
                index,
                &slice_id,
                &slice_path,
                &profile_url,
                ctx,
                native_r5,
            )?;
            materialize_children_from_generalized_leaf_slice(elements, index, &slice_id);
        }
        index += 1;
    }
    normalize_cqf_fhir_query_pattern_url_children(elements);
    Ok(())
}

pub(crate) fn materialize_children_from_generalized_leaf_slice(
    elements: &mut Vec<Value>,
    slice_index: usize,
    slice_id: &str,
) -> bool {
    let Some(source_id) = generalized_id_preserving_leaf_slice(slice_id) else {
        return false;
    };
    if source_id == slice_id {
        return false;
    }
    let target_prefix = format!("{slice_id}.");
    if elements.iter().any(|candidate| {
        candidate
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .starts_with(&target_prefix)
    }) {
        return false;
    }
    let source_prefix = format!("{source_id}.");
    let children: Vec<Value> = elements
        .iter()
        .filter_map(|child| {
            let id = child.get("id").and_then(Value::as_str)?;
            if !id.starts_with(&source_prefix) {
                return None;
            }
            let mut clone = child.clone();
            set_field(
                &mut clone,
                "id",
                Value::String(id.replacen(&source_id, slice_id, 1)),
            );
            Some(clone)
        })
        .collect();
    if children.is_empty() {
        return false;
    }
    for (offset, child) in children.into_iter().enumerate() {
        elements.insert(slice_index + 1 + offset, child);
    }
    true
}

pub(crate) fn generalized_id_preserving_leaf_slice(id: &str) -> Option<String> {
    if !has_slice_marker(id) {
        return None;
    }
    let mut segments: Vec<String> = id.split('.').map(str::to_string).collect();
    if segments.len() < 2 {
        return None;
    }
    let last = segments.len() - 1;
    for segment in &mut segments[..last] {
        *segment = unsliced_segment(segment).to_string();
    }
    Some(segments.join("."))
}

pub(crate) fn unsliced_segment(segment: &str) -> &str {
    segment
        .split_once(':')
        .map(|(base, _)| base)
        .or_else(|| segment.split_once('/').map(|(base, _)| base))
        .unwrap_or(segment)
}

pub(crate) fn should_clone_original_extension_anchor(id: &str) -> bool {
    let last_segment = id.rsplit('.').next().unwrap_or(id);
    matches!(last_segment, "extension" | "modifierExtension")
}

pub(crate) fn has_descendant_content_reference_to_anchor(elements: &[Value], anchor_id: &str) -> bool {
    let child_prefix = format!("{anchor_id}.");
    let relative = format!("#{anchor_id}");
    elements.iter().any(|element| {
        let id = element.get("id").and_then(Value::as_str).unwrap_or("");
        if !id.starts_with(&child_prefix) {
            return false;
        }
        element
            .get("contentReference")
            .and_then(Value::as_str)
            .is_some_and(|content_reference| {
                content_reference == relative || content_reference.ends_with(&relative)
            })
    })
}

pub(crate) fn strip_diff_owned_type_extensions_on_sliced_choice_child(
    clone: &mut Value,
    parent_id: &str,
    child_id: &str,
    original_elements_by_id: Option<&HashMap<String, Value>>,
) {
    if !has_slice_marker(parent_id) || !child_id.ends_with("[x]") {
        return;
    }
    let Some(original) = original_elements_by_id.and_then(|elements| elements.get(child_id)) else {
        return;
    };
    let original_types = original
        .get("type")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let Some(types) = clone.get_mut("type").and_then(Value::as_array_mut) else {
        return;
    };
    for ty in types {
        let Some(code) = ty.get("code").and_then(Value::as_str) else {
            continue;
        };
        let original_has_extension = original_types.iter().any(|original_ty| {
            original_ty.get("code").and_then(Value::as_str) == Some(code)
                && original_ty.get("extension").is_some()
        });
        if !original_has_extension {
            remove_field(ty, "extension");
        }
    }
}

pub(crate) fn immediate_slice_anchor_id(id: &str) -> Option<String> {
    let dot = id.rfind('.');
    let (prefix, last) = match dot {
        Some(dot) => (&id[..=dot], &id[dot + 1..]),
        None => ("", id),
    };
    let base = last
        .split_once(':')
        .map(|(base, _)| base)
        .or_else(|| last.split_once('/').map(|(base, _)| base));
    if let Some(base) = base {
        return Some(format!("{prefix}{base}"));
    }
    unsliced_element_id(id)
}

pub(crate) fn unsliced_element_id(id: &str) -> Option<String> {
    if !id.contains(':') && !id.contains('/') {
        return None;
    }
    let mut out = String::with_capacity(id.len());
    for (i, segment) in id.split('.').enumerate() {
        if i > 0 {
            out.push('.');
        }
        let base = segment
            .split_once(':')
            .map(|(base, _)| base)
            .unwrap_or(segment)
            .split_once('/')
            .map(|(base, _)| base)
            .unwrap_or_else(|| {
                segment
                    .split_once(':')
                    .map(|(base, _)| base)
                    .unwrap_or(segment)
            });
        out.push_str(base);
    }
    Some(out)
}

pub(crate) fn slice_anchor_id_from_diff_id(id: &str) -> Option<String> {
    let dot = id.rfind('.');
    let (prefix, last) = match dot {
        Some(dot) => (&id[..=dot], &id[dot + 1..]),
        None => ("", id),
    };
    let base = last
        .split_once(':')
        .map(|(base, _)| base)
        .or_else(|| last.split_once('/').map(|(base, _)| base))?;
    Some(format!("{prefix}{base}"))
}

pub(crate) fn is_slice_or_descendant_of(id: &str, anchor_id: &str) -> bool {
    id.starts_with(&format!("{anchor_id}."))
        || id.starts_with(&format!("{anchor_id}:"))
        || id.starts_with(&format!("{anchor_id}/"))
}

pub(crate) fn propagate_slice_min_to_anchor(
    elements: &mut [Value],
    path: &str,
    diff: &Value,
    diff_ids: &HashSet<String>,
) {
    if diff.get("sliceName").is_none() {
        return;
    }
    let expected_anchor_id = diff
        .get("id")
        .and_then(Value::as_str)
        .and_then(slice_anchor_id_from_diff_id);
    let Some(anchor_index) = elements
        .iter()
        .position(|candidate| {
            expected_anchor_id
                .as_deref()
                .is_some_and(|id| candidate.get("id").and_then(Value::as_str) == Some(id))
        })
        .or_else(|| {
            elements.iter().position(|candidate| {
                candidate.get("path").and_then(Value::as_str) == Some(path)
                    && candidate.get("sliceName").is_none()
            })
        })
    else {
        return;
    };
    let anchor_id = elements[anchor_index]
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(path)
        .to_string();
    if diff_ids.contains(&anchor_id) {
        return;
    }
    let anchor_path = elements[anchor_index]
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or(path)
        .to_string();
    let mut slice_count = 0usize;
    let mut required_sum = 0u64;
    for element in elements.iter() {
        if element.get("path").and_then(Value::as_str) != Some(anchor_path.as_str()) {
            continue;
        }
        let Some(id) = element.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !is_direct_slice_of(id, &anchor_id) {
            continue;
        }
        slice_count += 1;
        required_sum += element.get("min").and_then(Value::as_u64).unwrap_or(0);
    }
    if slice_count == 0 {
        return;
    }
    let current = elements[anchor_index]
        .get("min")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if required_sum > current {
        set_field(
            &mut elements[anchor_index],
            "min",
            Value::Number(required_sum.into()),
        );
    }
}

pub(crate) fn ensure_type_slicing_anchor(elements: &mut [Value], path: &str, diff: &Value) {
    if diff.get("sliceName").is_none() || !path.ends_with("[x]") {
        return;
    }
    let expected_anchor_id = diff
        .get("id")
        .and_then(Value::as_str)
        .and_then(slice_anchor_id_from_diff_id);
    let Some(anchor_index) = elements
        .iter()
        .position(|candidate| {
            expected_anchor_id
                .as_deref()
                .is_some_and(|id| candidate.get("id").and_then(Value::as_str) == Some(id))
        })
        .or_else(|| {
            elements.iter().position(|candidate| {
                candidate.get("path").and_then(Value::as_str) == Some(path)
                    && candidate.get("sliceName").is_none()
            })
        })
    else {
        return;
    };
    if elements[anchor_index].get("slicing").is_some() {
        return;
    }
    set_field(&mut elements[anchor_index], "slicing", type_slicing());
}

pub(crate) fn type_slicing() -> Value {
    let mut slicing = Map::new();
    let mut discriminator = Map::new();
    discriminator.insert("type".to_string(), Value::String("type".to_string()));
    discriminator.insert("path".to_string(), Value::String("$this".to_string()));
    slicing.insert(
        "discriminator".to_string(),
        Value::Array(vec![Value::Object(discriminator)]),
    );
    slicing.insert("ordered".to_string(), Value::Bool(false));
    slicing.insert("rules".to_string(), Value::String("open".to_string()));
    Value::Object(slicing)
}

pub(crate) fn ensure_extension_slicing_anchor(elements: &mut [Value], path: &str, diff: &Value) {
    if diff.get("sliceName").is_none() {
        return;
    }
    let expected_anchor_id = diff
        .get("id")
        .and_then(Value::as_str)
        .and_then(slice_anchor_id_from_diff_id);
    let Some(anchor_index) = elements
        .iter()
        .position(|candidate| {
            expected_anchor_id
                .as_deref()
                .is_some_and(|id| candidate.get("id").and_then(Value::as_str) == Some(id))
        })
        .or_else(|| {
            elements.iter().position(|candidate| {
                candidate.get("path").and_then(Value::as_str) == Some(path)
                    && candidate.get("sliceName").is_none()
            })
        })
    else {
        return;
    };
    if elements[anchor_index].get("slicing").is_some() {
        return;
    }
    let anchor_path = elements[anchor_index]
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or(path);
    if !anchor_path.ends_with(".extension") && !anchor_path.ends_with(".modifierExtension") {
        return;
    }
    check_extension_doco(&mut elements[anchor_index]);
    let ordered_false = extension_anchor_uses_ordered_false_slicing(elements, anchor_index);
    set_field(
        &mut elements[anchor_index],
        "slicing",
        extension_url_slicing(ordered_false),
    );
}

pub(crate) fn extension_anchor_uses_ordered_false_slicing(elements: &[Value], anchor_index: usize) -> bool {
    let Some(anchor_path) = elements[anchor_index].get("path").and_then(Value::as_str) else {
        return false;
    };
    let Some(parent_path) = anchor_path
        .strip_suffix(".extension")
        .or_else(|| anchor_path.strip_suffix(".modifierExtension"))
    else {
        return false;
    };
    if !parent_path.contains('.') {
        return true;
    }
    elements
        .iter()
        .find(|element| element.get("path").and_then(Value::as_str) == Some(parent_path))
        .and_then(|element| element.get("type").and_then(Value::as_array))
        .map(|types| {
            types.iter().any(|ty| {
                ty.get("code")
                    .and_then(Value::as_str)
                    .is_some_and(extension_anchor_parent_type_uses_ordered_false)
            })
        })
        .unwrap_or(false)
}

pub(crate) fn extension_anchor_parent_type_uses_ordered_false(code: &str) -> bool {
    matches!(
        code,
        "BackboneElement"
            | "base64Binary"
            | "boolean"
            | "canonical"
            | "code"
            | "date"
            | "dateTime"
            | "decimal"
            | "id"
            | "instant"
            | "integer"
            | "markdown"
            | "oid"
            | "positiveInt"
            | "string"
            | "time"
            | "unsignedInt"
            | "uri"
            | "url"
            | "uuid"
            | "xhtml"
    )
}

pub(crate) fn extension_url_slicing(ordered_false: bool) -> Value {
    let mut slicing = Map::new();
    let mut discriminator = Map::new();
    discriminator.insert("type".to_string(), Value::String("value".to_string()));
    discriminator.insert("path".to_string(), Value::String("url".to_string()));
    slicing.insert(
        "discriminator".to_string(),
        Value::Array(vec![Value::Object(discriminator)]),
    );
    if ordered_false {
        slicing.insert("ordered".to_string(), Value::Bool(false));
    } else {
        slicing.insert(
            "description".to_string(),
            Value::String("Extensions are always sliced by (at least) url".to_string()),
        );
    }
    slicing.insert("rules".to_string(), Value::String("open".to_string()));
    Value::Object(slicing)
}

pub(crate) fn normalize_copied_slicing(element: &mut Value) {
    let type_slicing = is_type_slicing(element);
    let extension_anchor = element
        .get("path")
        .and_then(Value::as_str)
        .is_some_and(|path| path.ends_with(".extension") || path.ends_with(".modifierExtension"));
    let top_level_extension_anchor =
        element.get("path").and_then(Value::as_str) == Some("Extension.extension");
    let extension_url_slicing = element
        .get("slicing")
        .is_some_and(has_extension_url_slicing);
    let choice_type_slicing = type_slicing
        && element
            .get("id")
            .or_else(|| element.get("path"))
            .and_then(Value::as_str)
            .is_some_and(|id| id.contains("[x]"));
    let Some(slicing) = element.get_mut("slicing") else {
        return;
    };
    if choice_type_slicing && slicing.get("ordered").is_none() {
        set_field(slicing, "ordered", Value::Bool(false));
    }
    if extension_anchor
        && extension_url_slicing
        && top_level_extension_anchor
        && slicing.get("ordered").is_none()
        && slicing.get("description").is_none()
    {
        set_field(
            slicing,
            "description",
            Value::String("Extensions are always sliced by (at least) url".to_string()),
        );
    }
}

pub(crate) fn has_extension_url_slicing(slicing: &Value) -> bool {
    slicing
        .get("discriminator")
        .and_then(Value::as_array)
        .map(|discriminators| {
            discriminators.iter().any(|d| {
                d.get("type").and_then(Value::as_str) == Some("value")
                    && d.get("path").and_then(Value::as_str) == Some("url")
            })
        })
        .unwrap_or(false)
}

pub(crate) fn is_direct_slice_of(id: &str, anchor_id: &str) -> bool {
    let Some(rest) = id.strip_prefix(anchor_id) else {
        return false;
    };
    let Some(first) = rest.as_bytes().first() else {
        return false;
    };
    if *first != b':' && *first != b'/' {
        return false;
    }
    !rest[1..].contains('.') && !rest[1..].contains(':') && !rest[1..].contains('/')
}

pub(crate) fn unfold_content_reference_parent(
    elements: &mut Vec<Value>,
    parent_index: usize,
    parent_id: &str,
    parent_path: &str,
    base_url: &str,
    base_spec_url: &str,
    ctx: &PackageContext,
    native_r5: bool,
    original_elements: &[Value],
) -> anyhow::Result<bool> {
    let original_content_reference = || {
        original_elements
            .iter()
            .find(|element| element.get("id").and_then(Value::as_str) == Some(parent_id))
            .or_else(|| {
                unsliced_element_id(parent_id).and_then(|unsliced_id| {
                    original_elements.iter().find(|element| {
                        element.get("id").and_then(Value::as_str) == Some(unsliced_id.as_str())
                    })
                })
            })
            .and_then(|element| element.get("contentReference"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let Some(content_reference) = elements[parent_index]
        .get("contentReference")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(original_content_reference)
    else {
        return Ok(false);
    };
    let Some((source_url, target_id)) = split_content_reference(&content_reference, base_url)
    else {
        return Ok(false);
    };

    let (target, source_children, source_spec_url, source_strip_non_inherited) = if source_url
        == base_url
    {
        let (target, children) = collect_content_reference_source(original_elements, &target_id);
        (
            target,
            children,
            base_spec_url.to_string(),
            native_r5 || base_spec_url.contains("/R5/"),
        )
    } else {
        let Some(source) = ctx.fetch(&source_url) else {
            return Ok(false);
        };
        let source_spec_url = spec_url_for_structure(&source, native_r5);
        let source_strip_non_inherited = native_r5 || strips_non_inherited_extensions(&source);
        let source_owned = source
            .get("snapshot")
            .and_then(|s| s.get("element"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let (target, children) = collect_content_reference_source(&source_owned, &target_id);
        (
            target,
            children,
            source_spec_url,
            source_strip_non_inherited,
        )
    };
    let Some(target) = target else {
        return Ok(false);
    };
    let Some(target_path) = target
        .get("path")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Ok(false);
    };

    remove_field(&mut elements[parent_index], "contentReference");
    if let Some(t) = target.get("type") {
        set_field(&mut elements[parent_index], "type", t.clone());
    }

    let source_snapshot_source = if source_url == base_url {
        None
    } else {
        ctx.fetch(&source_url)
            .and_then(|source| snapshot_source_value(&source))
    };
    let target_prefix = format!("{target_id}.");
    let existing_ids: HashSet<String> = elements
        .iter()
        .filter_map(|element| {
            element
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let mut children = Vec::new();
    for child in source_children {
        let child_id = child.get("id").and_then(Value::as_str).unwrap_or("");
        if !child_id.starts_with(&target_prefix) {
            continue;
        }
        let mut clone = normalize_inherited_element(
            child.clone(),
            &source_url,
            &source_spec_url,
            source_strip_non_inherited,
            native_r5,
            &source_url,
            source_snapshot_source.as_deref(),
            false,
        );
        if let Some(id) = clone.get("id").and_then(Value::as_str).map(str::to_string) {
            set_field(
                &mut clone,
                "id",
                Value::String(id.replacen(&target_id, parent_id, 1)),
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
                Value::String(path.replacen(&target_path, parent_path, 1)),
            );
        }
        if clone
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| existing_ids.contains(id))
        {
            continue;
        }
        absolutize_content_reference(&mut clone, &source_url);
        children.push(clone);
    }

    let insert_at = unfolded_child_insert_index(elements, parent_index, parent_id);
    for (offset, child) in children.into_iter().enumerate() {
        elements.insert(insert_at + offset, child);
    }
    Ok(true)
}

pub(crate) fn unfolded_child_insert_index(elements: &[Value], parent_index: usize, parent_id: &str) -> usize {
    let child_prefix = format!("{parent_id}.");
    let mut insert_at = parent_index + 1;
    while insert_at < elements.len()
        && elements[insert_at]
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with(&child_prefix))
    {
        insert_at += 1;
    }
    insert_at
}

pub(crate) fn collect_content_reference_source(
    elements: &[Value],
    target_id: &str,
) -> (Option<Value>, Vec<Value>) {
    let target = elements
        .iter()
        .find(|candidate| candidate.get("id").and_then(Value::as_str) == Some(target_id))
        .cloned();
    let target_prefix = format!("{target_id}.");
    let children = elements
        .iter()
        .filter(|candidate| {
            candidate
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .starts_with(&target_prefix)
        })
        .cloned()
        .collect();
    (target, children)
}

pub(crate) fn rehome_unfolded_type_constraint_sources(element: &mut Value, type_url: &str, base_url: &str) {
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        let key = constraint.get("key").and_then(Value::as_str);
        if matches!(key, Some("ele-1" | "ext-1")) {
            continue;
        }
        if constraint.get("source").and_then(Value::as_str) == Some(type_url) {
            set_field(constraint, "source", Value::String(base_url.to_string()));
        }
    }
}

pub(crate) fn split_content_reference(content_reference: &str, default_url: &str) -> Option<(String, String)> {
    if let Some(fragment) = content_reference.strip_prefix('#') {
        return Some((default_url.to_string(), fragment.to_string()));
    }
    let hash = content_reference.find('#')?;
    Some((
        content_reference[..hash].to_string(),
        content_reference[hash + 1..].to_string(),
    ))
}

pub(crate) fn absolutize_content_reference(element: &mut Value, source_url: &str) {
    let Some(content_reference) = element
        .get("contentReference")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    if let Some(fragment) = content_reference.strip_prefix('#') {
        let base_url = content_reference_base_url(source_url, fragment);
        set_field(
            element,
            "contentReference",
            Value::String(format!("{base_url}#{fragment}")),
        );
    }
}

pub(crate) fn content_reference_base_url(source_url: &str, fragment: &str) -> String {
    if source_url.starts_with("http://hl7.org/fhir/StructureDefinition/") {
        let target_root = fragment.split('.').next().unwrap_or("");
        let source_tail = source_url.rsplit('/').next().unwrap_or("");
        if target_root != source_tail
            && target_root
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_uppercase())
        {
            return format!("http://hl7.org/fhir/StructureDefinition/{target_root}");
        }
    }
    source_url.to_string()
}

pub(crate) fn merge_diff_into_element(
    target: &mut Value,
    diff: &Value,
    strip_non_inherited: bool,
    preserve_common_binding: bool,
    diff_must_support_ids: Option<&HashSet<String>>,
    inherited_must_support_ids: Option<&HashSet<String>>,
    original_ids: Option<&HashSet<String>>,
    constraint_source: &str,
) -> anyhow::Result<()> {
    let is_extension_doco = check_extension_doco(target);
    let is_slice_descendant = diff
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(is_child_below_slice_id);
    if diff
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(is_direct_slice_id)
        && diff_has_obligation_extension(diff)
    {
        remove_unprovenanced_obligation_extensions(target);
    }
    merge_extensions_from_definition(target, diff, strip_non_inherited, preserve_common_binding);
    let source_child_has_differential_ms = is_slice_descendant
        && diff.get("id").and_then(Value::as_str).is_some_and(|id| {
            diff_must_support_ids.is_some_and(|ids| {
                unsliced_element_id(id).is_some_and(|unsliced| ids.contains(&unsliced))
                    || ids
                        .iter()
                        .any(|ms_id| differential_id_generalizes_sliced_id(ms_id, id))
                    || (is_direct_extension_slice_value_id(id)
                        && ids.iter().any(|ms_id| ms_id.starts_with(&format!("{id}."))))
            })
        });
    if is_slice_descendant {
        dedupe_extension_values(target, "extension");
        let has_must_support_slice_ancestor =
            diff.get("id").and_then(Value::as_str).is_some_and(|id| {
                diff_constrains_must_support_shape(diff)
                    && (unsliced_id_or_non_slice_root_ancestor_has_must_support(
                        id,
                        inherited_must_support_ids,
                    ) || extension_slice_root_has_differential_must_support(
                        id,
                        diff_must_support_ids,
                    ))
                    && diff_must_support_ids.is_some_and(|ids| {
                        ids.iter().any(|ms_id| {
                            is_direct_slice_id(ms_id) && id.starts_with(&format!("{ms_id}."))
                        })
                    })
            });
        let diff_has_obligation = diff_has_obligation_extension(diff);
        let diff_is_existing_slice_descendant = diff_constrains_must_support_shape(diff)
            && diff
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| original_ids.is_some_and(|ids| ids.contains(id)));
        if diff.get("mustSupport").is_none()
            && (diff_constrains_must_support_shape(diff) || diff_has_text_fields(diff))
            && !source_child_has_differential_ms
            && !has_must_support_slice_ancestor
            && !diff_has_obligation
            && !diff_is_existing_slice_descendant
            && !is_comment_only_slice_descendant_diff(diff)
            && target.get("mustSupport").and_then(Value::as_bool) != Some(false)
        {
            remove_field(target, "mustSupport");
        }
    }
    if is_explicit_slice_descendant_without_extensions(diff) {
        remove_obligation_extensions(target);
    }

    merge_text_field(target, diff, "short", TextMerge::Replace);
    merge_text_field(target, diff, "definition", TextMerge::Markdown);
    merge_text_field(target, diff, "comment", TextMerge::Markdown);
    merge_text_field(target, diff, "label", TextMerge::String);
    merge_text_field(target, diff, "requirements", TextMerge::Markdown);

    merge_unique_array_strings(target, diff, "alias");
    merge_unique_array_strings(target, diff, "condition");
    fill_missing_constraint_sources_on_constrained_element(target, constraint_source);
    merge_unique_by_key(target, diff, "constraint", "key");
    merge_unique_values(target, diff, "example");
    merge_unique_values_prepend(target, diff, "mapping");
    merge_unique_array_strings(target, diff, "valueAlternatives");

    copy_if_present(target, diff, "sliceName");
    copy_if_present(target, diff, "min");
    merge_max_cardinality(target, diff);
    copy_if_present(target, diff, "maxLength");
    copy_if_present(target, diff, "mustSupport");
    if is_slice_descendant
        && diff.get("mustSupport").is_none()
        && target.get("mustSupport").and_then(Value::as_bool) != Some(false)
        && diff.get("id").and_then(Value::as_str).is_some_and(|id| {
            source_child_has_differential_ms
                || (diff_constrains_must_support_shape(diff)
                    && (unsliced_id_or_non_slice_root_ancestor_has_must_support(
                        id,
                        inherited_must_support_ids,
                    ) || extension_slice_root_has_differential_must_support(
                        id,
                        diff_must_support_ids,
                    ))
                    && diff_must_support_ids.is_some_and(|ids| {
                        ids.iter().any(|ms_id| {
                            is_direct_slice_id(ms_id) && id.starts_with(&format!("{ms_id}."))
                        })
                    }))
                || (!diff_constrains_must_support_shape(diff)
                    && has_fixed_or_pattern_value(target)
                    && element_min_is_positive(target)
                    && unsliced_or_slice_anchor_ancestor_has_must_support(
                        id,
                        inherited_must_support_ids,
                    ))
        })
    {
        set_field(target, "mustSupport", Value::Bool(true));
    }
    if is_slice_descendant
        && diff.get("mustSupport").is_none()
        && !diff_constrains_must_support_shape(diff)
        && diff_has_text_fields(diff)
        && !diff_has_obligation_extension(diff)
        && !is_comment_only_slice_descendant_diff(diff)
    {
        remove_field(target, "mustSupport");
    }
    if is_slice_descendant
        && diff.get("mustSupport").is_none()
        && has_fixed_or_pattern_value(diff)
        && diff.get("min").is_none()
        && diff.get("max").is_none()
        && !source_child_has_differential_ms
        && diff.get("id").and_then(Value::as_str).is_some_and(|id| {
            !unsliced_exact_element_has_must_support(id, inherited_must_support_ids)
        })
    {
        remove_field(target, "mustSupport");
    }
    copy_if_present(target, diff, "mustHaveValue");
    copy_if_present(target, diff, "contentReference");
    copy_if_present(target, diff, "slicing");
    // The Publisher's R4->R5 parse drops empty-string primitives, so a
    // differential slicing.description of "" never reaches the snapshot
    // (ndh HealthcareService.category).
    if let Some(slicing) = target.get_mut("slicing") {
        if slicing.get("description").and_then(Value::as_str) == Some("") {
            remove_field(slicing, "description");
        }
    }
    normalize_copied_slicing(target);

    copy_choice_prefix(target, diff, "fixed");
    copy_choice_prefix(target, diff, "pattern");
    copy_choice_prefix(target, diff, "minValue");
    copy_choice_prefix(target, diff, "maxValue");
    if is_slice_descendant
        && diff.get("mustSupport").is_none()
        && target.get("mustSupport").and_then(Value::as_bool) != Some(false)
        && has_pattern_value(target)
        && element_min_is_positive(target)
        && fixed_pattern_min_child_can_inherit_ms(target)
        && diff.get("id").and_then(Value::as_str).is_some_and(|id| {
            unsliced_or_slice_anchor_ancestor_has_must_support(id, inherited_must_support_ids)
        })
    {
        set_field(target, "mustSupport", Value::Bool(true));
    }
    if is_slice_descendant
        && diff.get("mustSupport").is_none()
        && is_identifier_type_pattern_diff(diff)
    {
        remove_field(target, "mustSupport");
    }

    if diff.get("isSummary").is_some() && target.get("isSummary") != diff.get("isSummary") {
        bail!(
            "isSummary changes are a hard Layer-A error at {}",
            diff.get("path")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>")
        );
    }

    if is_extension_doco {
        copy_if_present(target, diff, "isModifier");
        copy_if_present(target, diff, "isModifierReason");
    }

    if diff.get("binding").is_some() {
        merge_binding(target, diff);
    }

    if let Some(t) = diff.get("type") {
        merge_type_entries(target, t);
        if target.get("contentReference").is_some() {
            remove_field(target, "contentReference");
        }
    }
    normalize_type_slicing(target, diff);
    if target.get("binding").is_some() && !has_bindable_type(target) {
        remove_field(target, "binding");
    }

    if is_root_element(target) {
        remove_field(target, "requirements");
    }

    Ok(())
}

pub(crate) fn is_explicit_slice_descendant_without_extensions(diff: &Value) -> bool {
    diff.get("extension").is_none()
        && diff
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(is_slice_descendant_id)
}

pub(crate) fn diff_constrains_must_support_shape(diff: &Value) -> bool {
    let Some(obj) = diff.as_object() else {
        return false;
    };
    obj.keys().any(|key| {
        !matches!(
            key.as_str(),
            "id" | "path"
                | "sliceName"
                | "short"
                | "definition"
                | "comment"
                | "label"
                | "requirements"
                | "alias"
                | "mapping"
        )
    })
}

pub(crate) fn diff_has_text_fields(diff: &Value) -> bool {
    diff.as_object().is_some_and(|obj| {
        obj.keys().any(|key| {
            matches!(
                key.as_str(),
                "short" | "definition" | "comment" | "label" | "requirements" | "alias" | "mapping"
            )
        })
    })
}

pub(crate) fn is_comment_only_slice_descendant_diff(diff: &Value) -> bool {
    let Some(obj) = diff.as_object() else {
        return false;
    };
    diff.get("id")
        .and_then(Value::as_str)
        .is_some_and(is_slice_descendant_id)
        && obj.contains_key("comment")
        && obj
            .keys()
            .all(|key| matches!(key.as_str(), "id" | "path" | "comment"))
}

pub(crate) fn is_identifier_type_pattern_diff(diff: &Value) -> bool {
    diff.get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| id.contains(".identifier:") && id.ends_with(".type"))
        && diff.get("patternCodeableConcept").is_some()
}

pub(crate) fn unsliced_id_or_non_slice_root_ancestor_has_must_support(
    id: &str,
    inherited_must_support_ids: Option<&HashSet<String>>,
) -> bool {
    let Some(inherited_must_support_ids) = inherited_must_support_ids else {
        return false;
    };
    let direct_slice_anchor = first_slice_anchor_id(id);
    let mut current = unsliced_element_id(id).unwrap_or_else(|| id.to_string());
    loop {
        if inherited_must_support_ids.contains(&current)
            && direct_slice_anchor.as_deref() != Some(current.as_str())
        {
            return true;
        }
        let Some(dot) = current.rfind('.') else {
            return false;
        };
        current.truncate(dot);
    }
}

pub(crate) fn unsliced_exact_element_has_must_support(
    id: &str,
    inherited_must_support_ids: Option<&HashSet<String>>,
) -> bool {
    let Some(inherited_must_support_ids) = inherited_must_support_ids else {
        return false;
    };
    let unsliced = unsliced_element_id(id).unwrap_or_else(|| id.to_string());
    inherited_must_support_ids.contains(&unsliced)
}

pub(crate) fn unsliced_or_slice_anchor_ancestor_has_must_support(
    id: &str,
    inherited_must_support_ids: Option<&HashSet<String>>,
) -> bool {
    let Some(inherited_must_support_ids) = inherited_must_support_ids else {
        return false;
    };
    let mut current = unsliced_element_id(id).unwrap_or_else(|| id.to_string());
    loop {
        if inherited_must_support_ids.contains(&current) {
            return true;
        }
        let Some(dot) = current.rfind('.') else {
            return false;
        };
        current.truncate(dot);
    }
}

pub(crate) fn first_slice_anchor_id(id: &str) -> Option<String> {
    let mut out = String::new();
    for (index, segment) in id.split('.').enumerate() {
        if index > 0 {
            out.push('.');
        }
        let (base, has_slice) = segment_base_and_slice_marker(segment);
        out.push_str(base);
        if has_slice {
            return Some(out);
        }
    }
    None
}

pub(crate) fn extension_slice_root_has_differential_must_support(
    _id: &str,
    _diff_must_support_ids: Option<&HashSet<String>>,
) -> bool {
    false
}

pub(crate) fn is_direct_extension_slice_value_id(id: &str) -> bool {
    let mut segments = id.rsplit('.');
    if segments.next() != Some("value[x]") {
        return false;
    }
    segments.next().is_some_and(|segment| {
        let (base, has_slice) = segment_base_and_slice_marker(segment);
        has_slice && matches!(base, "extension" | "modifierExtension")
    })
}

pub(crate) fn is_slice_descendant_id(id: &str) -> bool {
    id.find([':', '/'])
        .is_some_and(|index| id[index + 1..].contains('.'))
}

pub(crate) fn is_child_below_slice_id(id: &str) -> bool {
    if !has_slice_marker(id) {
        return false;
    }
    let last_segment = id.rsplit('.').next().unwrap_or(id);
    !has_slice_marker(last_segment)
}

pub(crate) fn is_direct_slice_id(id: &str) -> bool {
    if !has_slice_marker(id) {
        return false;
    }
    let last_segment = id.rsplit('.').next().unwrap_or(id);
    has_slice_marker(last_segment)
}

pub(crate) fn diff_has_obligation_extension(diff: &Value) -> bool {
    diff.get("extension")
        .and_then(Value::as_array)
        .is_some_and(|exts| exts.iter().any(is_obligation_extension))
}

pub(crate) fn remove_obligation_extensions(target: &mut Value) {
    let Some(obj) = target.as_object_mut() else {
        return;
    };
    let Some(Value::Array(exts)) = obj.get_mut("extension") else {
        return;
    };
    exts.retain(|ext| !is_obligation_extension(ext));
    if exts.is_empty() {
        obj.remove("extension");
    }
}

pub(crate) fn remove_unprovenanced_obligation_extensions(target: &mut Value) {
    let Some(obj) = target.as_object_mut() else {
        return;
    };
    let Some(Value::Array(exts)) = obj.get_mut("extension") else {
        return;
    };
    exts.retain(|ext| !is_obligation_extension(ext) || obligation_has_snapshot_source(ext));
    if exts.is_empty() {
        obj.remove("extension");
    }
}

pub(crate) fn obligation_has_snapshot_source(ext: &Value) -> bool {
    ext.get("extension")
        .and_then(Value::as_array)
        .is_some_and(|children| {
            children.iter().any(|child| {
                child.get("url").and_then(Value::as_str)
                    == Some("http://hl7.org/fhir/tools/StructureDefinition/snapshot-source")
            })
        })
}

pub(crate) fn normalize_type_slicing(element: &mut Value, diff: &Value) {
    if diff
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(is_child_below_slice_id)
    {
        return;
    }
    let Some(types) = diff.get("type").and_then(Value::as_array) else {
        return;
    };
    let has_reference = types
        .iter()
        .any(|ty| ty.get("code").and_then(Value::as_str) == Some("Reference"));
    let has_codeable_concept = types
        .iter()
        .any(|ty| ty.get("code").and_then(Value::as_str) == Some("CodeableConcept"));
    if !has_reference || !has_codeable_concept {
        return;
    }
    let is_type_slicing = diff
        .get("slicing")
        .and_then(|s| s.get("discriminator"))
        .and_then(Value::as_array)
        .map(|discriminators| {
            discriminators.iter().any(|d| {
                d.get("type").and_then(Value::as_str) == Some("type")
                    && d.get("path").and_then(Value::as_str) == Some("$this")
            })
        })
        .unwrap_or(false);
    if !is_type_slicing {
        return;
    }
    if let Some(slicing) = element.get_mut("slicing") {
        set_field(slicing, "rules", Value::String("closed".to_string()));
    }
}

pub(crate) fn check_extension_doco(element: &mut Value) -> bool {
    let path = element
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let base_path = element
        .get("base")
        .and_then(|b| b.get("path"))
        .and_then(Value::as_str);
    let is_extension = (path == "Extension"
        || path.ends_with(".extension")
        || path.ends_with(".modifierExtension"))
        && base_path != Some("II.extension")
        && !has_profiled_extension_type(element);
    if is_extension {
        set_field(
            element,
            "definition",
            Value::String("An Extension".to_string()),
        );
        set_field(element, "short", Value::String("Extension".to_string()));
        remove_field(element, "comment");
        remove_field(element, "requirements");
        remove_field(element, "alias");
        remove_field(element, "mapping");
    }
    is_extension
}

pub(crate) fn first_extension_profile_url(element: &Value) -> Option<&str> {
    let ty = element
        .get("type")
        .and_then(Value::as_array)?
        .iter()
        .find(|t| t.get("code").and_then(Value::as_str) == Some("Extension"))?;
    ty.get("profile")
        .and_then(Value::as_array)?
        .first()?
        .as_str()
}

pub(crate) fn has_profiled_extension_type(element: &Value) -> bool {
    first_extension_profile_url(element).is_some()
}

pub(crate) fn apply_extension_profile_root(
    slice: &mut Value,
    diff: &Value,
    ctx: &PackageContext,
    native_r5: bool,
    host_extension_source: Option<&str>,
    copy_profile_root_condition: bool,
    allow_local_root_constraints: bool,
    allow_local_root_condition: bool,
) -> anyhow::Result<()> {
    let diff_supplies_extension_profile = first_extension_profile_url(diff).is_some();
    let Some(profile_url_owned) = first_extension_profile_url(diff)
        .or_else(|| first_extension_profile_url(slice))
        .map(str::to_string)
    else {
        return Ok(());
    };
    let profile_url = profile_url_owned.as_str();
    if uses_generic_extension_doco_profile(profile_url) {
        apply_generic_extension_doco(slice);
        return Ok(());
    }
    let is_local_profile = ctx.is_local(profile_url);
    let Some(profile) = profile_with_snapshot(profile_url, ctx, native_r5)? else {
        if apply_native_r5_known_extension_root(slice, profile_url, native_r5) {
            return Ok(());
        }
        apply_missing_profile_extension_slice_doco(slice);
        return Ok(());
    };
    let Some(root) = profile
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    else {
        return Ok(());
    };
    let mut root = root.clone();
    let root_explicit_is_summary = root.get("isSummary").is_some();
    // Extension-root doco keeps Publisher's known-relative links (e.g.
    // workflow-extensions.html#instantiation) as-is; only freshly applied
    // extension slices reach here. Inherited copies of the same slice go through
    // normalize_inherited_element, which rewrites them to the spec URL.
    rewrite_markdown_links(
        &mut root,
        &spec_url_for_structure(&profile, native_r5),
        true,
    );
    let project_local_root_constraints =
        allow_local_root_constraints && projects_local_extension_root_constraints(profile_url);
    let local_profile_had_loaded_snapshot =
        is_local_profile && ctx.resource_has_loaded_snapshot(profile_url);
    if native_r5 && is_local_profile {
        if project_local_root_constraints {
            if local_profile_had_loaded_snapshot {
                if let Some(r4_root) =
                    profile_with_snapshot(profile_url, ctx, false)?.and_then(|profile| {
                        profile
                            .get("snapshot")
                            .and_then(|s| s.get("element"))
                            .and_then(Value::as_array)
                            .and_then(|a| a.first())
                            .cloned()
                    })
                {
                    add_constraint_xpath_extensions_from_source(&mut root, &r4_root);
                } else {
                    convert_own_constraint_xpaths_to_extensions(&mut root);
                }
            } else {
                strip_constraint_extensions(&mut root);
            }
            strip_constraint_xpaths(&mut root);
        }
        if root.get("isSummary").is_none() {
            set_field(&mut root, "isSummary", Value::Bool(false));
        }
    }
    if diff_supplies_extension_profile
        && is_core_extension_profile(profile_url)
        && root.get("isSummary").is_none()
    {
        set_field(&mut root, "isSummary", Value::Bool(false));
    }
    adjust_extension_root_constraint_sources(
        &mut root,
        slice,
        is_local_profile && project_local_root_constraints,
        profile_url,
        host_extension_source,
    );
    if !copy_profile_root_condition {
        remove_field(&mut root, "condition");
    }
    if (!is_local_profile && !allow_local_root_constraints)
        || (is_local_profile
            && !allow_local_root_condition
            && !keeps_extension_root_condition(profile_url))
        || omits_extension_root_condition(profile_url)
    {
        remove_field(&mut root, "condition");
    }
    if !allow_local_root_constraints {
        remove_field(&mut root, "constraint");
    } else if is_local_profile && !project_local_root_constraints {
        retain_base_extension_constraints(&mut root);
    }
    let is_modifier_extension_slice = slice
        .get("path")
        .or_else(|| diff.get("path"))
        .and_then(Value::as_str)
        .is_some_and(|path| path.ends_with(".modifierExtension"));
    if is_local_profile
        && root.get("comment").is_some()
        && !has_semantic_element_extensions(slice)
        && !has_semantic_element_extensions(diff)
        && !is_modifier_extension_slice
    {
        strip_constraint_extensions(&mut root);
    }
    fill_missing_constraint_sources_on_constrained_element(
        &mut root,
        host_extension_source.unwrap_or(profile_url),
    );
    apply_native_r5_variable_extension_comment(&mut root, profile_url, native_r5);

    let mut overlay_keys = vec![
        "short",
        "definition",
        "comment",
        "requirements",
        "alias",
        "isModifier",
        "isModifierReason",
        "mapping",
    ];
    if copy_profile_root_condition {
        overlay_keys.push("condition");
    }
    for key in overlay_keys {
        if let Some(value) = root.get(key) {
            set_field(slice, key, extension_root_overlay_value(key, value));
        } else {
            remove_field(slice, key);
        }
    }
    merge_min_cardinality(slice, &root);
    merge_max_cardinality(slice, &root);
    // isSummary is never stripped by Java's root overlay: the slice keeps whatever
    // it inherits (a stored slice like us-core birthsex carries none; a fresh slice
    // cloned from the unsliced extension element carries false). Synthetic native
    // R5 defaults added above do not count as explicit root values.
    if root_explicit_is_summary {
        let Some(value) = root.get("isSummary") else {
            return Ok(());
        };
        set_field(slice, "isSummary", value.clone());
    }

    if let Some(root_constraints) = root.get("constraint").and_then(Value::as_array) {
        let existing = slice
            .get("constraint")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        remove_field(slice, "constraint");
        let target_constraints = ensure_array_field(slice, "constraint");
        for constraint in root_constraints {
            target_constraints.push(constraint.clone());
        }
        for constraint in existing {
            let key = constraint.get("key").and_then(Value::as_str);
            if key.is_some_and(|key| {
                target_constraints
                    .iter()
                    .any(|existing| existing.get("key").and_then(Value::as_str) == Some(key))
            }) {
                continue;
            }
            target_constraints.push(constraint);
        }
    }
    Ok(())
}

pub(crate) fn extension_root_overlay_value(key: &str, value: &Value) -> Value {
    if matches!(key, "short" | "definition" | "comment" | "requirements") {
        if let Some(text) = value.as_str() {
            return Value::String(text.trim_end().to_string());
        }
    }
    value.clone()
}

pub(crate) fn retain_base_extension_constraints(element: &mut Value) {
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    constraints.retain(|constraint| {
        matches!(
            constraint.get("key").and_then(Value::as_str),
            Some("ele-1" | "ext-1")
        )
    });
}

pub(crate) fn strip_constraint_extensions(element: &mut Value) {
    let Some(constraints) = element.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        remove_field(constraint, "extension");
    }
}

pub(crate) fn apply_generic_extension_doco(element: &mut Value) {
    set_field(element, "short", Value::String("Extension".to_string()));
    set_field(
        element,
        "definition",
        Value::String("An Extension".to_string()),
    );
    remove_field(element, "comment");
    remove_field(element, "requirements");
    remove_field(element, "alias");
    remove_field(element, "condition");
    remove_field(element, "mapping");
}

pub(crate) fn apply_missing_profile_extension_slice_doco(element: &mut Value) {
    let is_slice = element.get("sliceName").is_some()
        || element
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(has_slice_marker);
    if !is_slice {
        return;
    }
    let path = element.get("path").and_then(Value::as_str).unwrap_or("");
    if !path.ends_with(".extension") && !path.ends_with(".modifierExtension") {
        return;
    }
    set_field(
        element,
        "definition",
        Value::String("An Extension".to_string()),
    );
    remove_field(element, "comment");
    remove_field(element, "requirements");
    remove_field(element, "alias");
    remove_field(element, "mapping");
}

pub(crate) fn adjust_extension_root_constraint_sources(
    root: &mut Value,
    slice: &Value,
    local_profile: bool,
    profile_url: &str,
    host_extension_source: Option<&str>,
) {
    let Some(source) = extension_slice_ext_constraint_source(
        slice,
        local_profile,
        profile_url,
        host_extension_source,
    ) else {
        return;
    };
    let Some(constraints) = root.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for constraint in constraints {
        if constraint.get("key").and_then(Value::as_str) == Some("ext-1")
            && constraint.get("source").is_none()
        {
            set_field(constraint, "source", Value::String(source.clone()));
        }
    }
}

pub(crate) fn extension_slice_ext_constraint_source(
    slice: &Value,
    local_profile: bool,
    profile_url: &str,
    host_extension_source: Option<&str>,
) -> Option<String> {
    let path = slice.get("path").and_then(Value::as_str)?;
    if !path.ends_with(".extension") && !path.ends_with(".modifierExtension") {
        return None;
    }
    if local_profile || !is_core_extension_profile(profile_url) {
        Some(
            host_extension_source
                .unwrap_or_else(|| {
                    path.split_once('.')
                        .map(|(root, _)| root)
                        .unwrap_or("Extension")
                })
                .to_string(),
        )
    } else {
        Some("http://hl7.org/fhir/StructureDefinition/Extension".to_string())
    }
}

pub(crate) fn is_core_extension_profile(profile_url: &str) -> bool {
    profile_url.starts_with("http://hl7.org/fhir/StructureDefinition/")
        || (profile_url.starts_with("http://hl7.org/fhir/5.0/StructureDefinition/extension-"))
}

pub(crate) fn uses_generic_extension_doco_profile(profile_url: &str) -> bool {
    let bare_url = profile_url
        .split_once('|')
        .map(|(url, _)| url)
        .unwrap_or(profile_url);
    bare_url == "http://hl7.org/fhir/StructureDefinition/codeOptions"
        || profile_url == "http://hl7.org/fhir/StructureDefinition/artifact-versionAlgorithm|5.2.0"
}

pub(crate) fn apply_type_profile_root(
    target: &mut Value,
    diff: &Value,
    ctx: &PackageContext,
    native_r5: bool,
) -> anyhow::Result<()> {
    let Some(profile_url) = first_non_extension_profile_url(diff) else {
        return Ok(());
    };
    let Some(profile) = ctx.fetch(profile_url) else {
        return Ok(());
    };
    let root_candidate = if let Some((root, source, snapshot_source)) =
        local_differential_slice_resource_type_root(target, diff, &profile, profile_url, ctx)?
    {
        Some((root, source, snapshot_source))
    } else {
        if !uses_profile_root_overlay(&profile) {
            return Ok(());
        }
        profile_root_element(&profile, ctx)?.map(|root| {
            (
                root,
                profile_url.to_string(),
                snapshot_source_value(&profile),
            )
        })
    };
    let Some((mut root, root_source, root_snapshot_source)) = root_candidate else {
        return Ok(());
    };
    let root_must_support = root
        .get("mustSupport")
        .cloned()
        .or_else(|| profile_root_must_support(&profile));
    if native_r5 {
        let constraint_xpaths = HashMap::new();
        let preserve_common_binding =
            native_r5 && is_r4_spec_url(&spec_url_for_structure(&profile, native_r5));
        project_element_to_native_r5(
            &mut root,
            &root_source,
            root_snapshot_source.as_deref(),
            &constraint_xpaths,
            None,
            false,
            true,
            None,
            preserve_common_binding,
        );
    }
    fill_missing_constraint_sources_on_constrained_element(&mut root, &root_source);

    // The short/definition/comment/requirements/alias/mapping overlay always
    // applies (ProfileUtilities.updateFromDefinition PU:2657-2671). The
    // isModifier/isModifierReason/isSummary/condition root values only carry over
    // when the element narrows to a single profiled type (the type-redirect path);
    // a multi-typed element (e.g. DTR Parameters.parameter:order.resource with 9
    // candidate profiles) keeps its inherited isSummary/condition.
    let single_type = diff
        .get("type")
        .and_then(Value::as_array)
        .map(|types| types.len() == 1)
        .unwrap_or(false);
    let mut keys: Vec<&str> = vec![
        "short",
        "definition",
        "comment",
        "requirements",
        "alias",
        "mapping",
    ];
    if single_type {
        keys.extend(["condition", "isModifier", "isModifierReason", "isSummary"]);
    }
    for key in keys {
        if diff.get(key).is_some() {
            continue;
        }
        if let Some(value) = root.get(key) {
            set_field(target, key, value.clone());
        } else if key != "comment" {
            remove_field(target, key);
        }
    }
    if single_type
        && target
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with(".resource"))
    {
        if let Some(value) = root.get("mustSupport").or(root_must_support.as_ref()) {
            set_field(target, "mustSupport", value.clone());
        }
    }
    Ok(())
}

pub(crate) fn profile_root_must_support(profile: &Value) -> Option<Value> {
    profile
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|root| root.get("mustSupport"))
        .cloned()
        .or_else(|| {
            profile
                .get("differential")
                .and_then(|s| s.get("element"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|root| root.get("mustSupport"))
                .cloned()
        })
}

pub(crate) fn local_differential_slice_resource_type_root(
    target: &Value,
    diff: &Value,
    profile: &Value,
    profile_url: &str,
    ctx: &PackageContext,
) -> anyhow::Result<Option<(Value, String, Option<String>)>> {
    let root_diff = profile
        .get("differential")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .and_then(|elements| elements.first());
    if !(ctx.is_local(profile_url)
        && profile.get("snapshot").is_none()
        && root_diff.is_some_and(|root| is_profile_root_diff(profile, root))
        && root_diff.is_none_or(|root| !root_diff_has_profile_text_overlay(root))
        && target
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with(".resource"))
        && diff
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(is_slice_descendant_id))
    {
        return Ok(None);
    }
    let Some(type_code) = first_non_extension_type_code(diff) else {
        return Ok(None);
    };
    let Some(type_def) = ctx.fetch(type_code) else {
        return Ok(None);
    };
    let source = structure_source(&type_def, type_code);
    let snapshot_source = snapshot_source_value(&type_def);
    let mut root = profile_root_element(&type_def, ctx)?;
    if let Some(root) = root.as_mut() {
        if let Some(profile_root) = profile_root_element(profile, ctx)? {
            prepend_profile_only_mappings(root, &profile_root);
        }
    }
    Ok(root.map(|root| (root, source, snapshot_source)))
}

pub(crate) fn root_diff_has_profile_text_overlay(root: &Value) -> bool {
    ["short", "definition", "comment", "requirements", "alias"]
        .into_iter()
        .any(|key| root.get(key).is_some())
}

pub(crate) fn prepend_profile_only_mappings(root: &mut Value, profile_root: &Value) {
    let Some(profile_mappings) = profile_root.get("mapping").and_then(Value::as_array) else {
        return;
    };
    let existing = root
        .get("mapping")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut additions = Vec::new();
    for mapping in profile_mappings {
        if !existing.iter().any(|candidate| candidate == mapping)
            && !additions.iter().any(|candidate| candidate == mapping)
        {
            additions.push(mapping.clone());
        }
    }
    if additions.is_empty() {
        return;
    }
    additions.extend(existing);
    set_field(root, "mapping", Value::Array(additions));
}

pub(crate) fn first_non_extension_type_code(element: &Value) -> Option<&str> {
    element
        .get("type")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|t| t.get("code").and_then(Value::as_str))
        .find(|code| *code != "Extension")
}

pub(crate) fn uses_profile_root_overlay(profile: &Value) -> bool {
    matches!(
        profile.get("kind").and_then(Value::as_str),
        Some("resource" | "logical")
    )
}

pub(crate) fn profile_root_element(profile: &Value, ctx: &PackageContext) -> anyhow::Result<Option<Value>> {
    if let Some(root) = profile
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    {
        return Ok(Some(root.clone()));
    }

    let profile_url = structure_url_or(profile, "");
    let diff_root = profile
        .get("differential")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .and_then(|a| a.first());
    let has_root_diff = diff_root.is_some_and(|diff| is_profile_root_diff(profile, diff));
    if ctx.is_local(&profile_url) && !has_root_diff {
        let generated = generate_snapshot(
            profile.clone(),
            ctx,
            SnapshotOptions {
                sort_differential: true,
                native_r5: false,
                apply_extension_root_doco: false,
            },
        )?;
        return Ok(generated
            .get("snapshot")
            .and_then(|s| s.get("element"))
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .cloned());
    }
    let base_is_differential_only = profile
        .get("baseDefinition")
        .and_then(Value::as_str)
        .and_then(|base_url| ctx.fetch(base_url))
        .is_some_and(|base| base.get("snapshot").is_none());
    if ctx.is_local(&profile_url) && base_is_differential_only {
        let generated = generate_snapshot(
            profile.clone(),
            ctx,
            SnapshotOptions {
                sort_differential: true,
                native_r5: false,
                apply_extension_root_doco: false,
            },
        )?;
        return Ok(generated
            .get("snapshot")
            .and_then(|s| s.get("element"))
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .cloned());
    }

    let base_root = profile
        .get("baseDefinition")
        .and_then(Value::as_str)
        .and_then(|base_url| ctx.fetch(base_url))
        .and_then(|base| {
            base.get("snapshot")
                .and_then(|s| s.get("element"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .cloned()
                .map(|root| (root, strips_non_inherited_extensions(&base)))
        });

    match (base_root, diff_root) {
        (Some((mut root, strip_non_inherited)), Some(diff_root))
            if is_profile_root_diff(profile, diff_root) =>
        {
            let profile_constraint_source = structure_url_or(profile, "");
            merge_diff_into_element(
                &mut root,
                diff_root,
                strip_non_inherited,
                false,
                None,
                None,
                None,
                &profile_constraint_source,
            )?;
            Ok(Some(root))
        }
        (Some((root, _)), _) => Ok(Some(root)),
        (None, Some(diff_root)) if is_profile_root_diff(profile, diff_root) => {
            Ok(Some(diff_root.clone()))
        }
        _ => {
            // No usable root via the shallow path (e.g. a local profile whose
            // differential has no root element and whose base also lacks a stored
            // snapshot). Generate the profile's full R4 snapshot recursively and
            // take its root, matching the Java oracle which resolves the fully
            // generated profile before overlaying its root.
            let generated = generate_snapshot(
                profile.clone(),
                ctx,
                SnapshotOptions {
                    sort_differential: true,
                    native_r5: false,
                    apply_extension_root_doco: false,
                },
            )?;
            Ok(generated
                .get("snapshot")
                .and_then(|s| s.get("element"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .cloned())
        }
    }
}

pub(crate) fn is_profile_root_diff(profile: &Value, diff: &Value) -> bool {
    let Some(profile_type) = profile.get("type").and_then(Value::as_str) else {
        return false;
    };
    diff.get("id")
        .or_else(|| diff.get("path"))
        .and_then(Value::as_str)
        == Some(profile_type)
}

pub(crate) fn first_non_extension_profile_url(element: &Value) -> Option<&str> {
    let ty = element
        .get("type")
        .and_then(Value::as_array)?
        .iter()
        .find(|t| {
            t.get("code")
                .and_then(Value::as_str)
                .map(|code| code != "Extension")
                .unwrap_or(false)
                && t.get("profile")
                    .and_then(Value::as_array)
                    .map(|p| !p.is_empty())
                    .unwrap_or(false)
        })?;
    ty.get("profile")
        .and_then(Value::as_array)?
        .first()?
        .as_str()
}

pub(crate) fn single_non_extension_profile_url(element: &Value) -> Option<&str> {
    let types = element.get("type").and_then(Value::as_array)?;
    if types.len() != 1 {
        return None;
    }
    let ty = types.first()?;
    if ty.get("code").and_then(Value::as_str) == Some("Extension") {
        return None;
    }
    let profiles = ty.get("profile").and_then(Value::as_array)?;
    if profiles.len() != 1 {
        return None;
    }
    profiles.first()?.as_str()
}

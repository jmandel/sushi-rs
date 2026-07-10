//! Leaf merge layer: the low-level element/field merge primitives used by the
//! walk engine (`updateFromDefinition`). Kept dependency-light and free of
//! policy heuristics — the walk drives all decisions.

#![allow(unused_imports)]
use anyhow::{bail, Context};
use indexmap::IndexMap;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::*;

pub(crate) const STRUCTUREDEFINITION_HIERARCHY_URL: &str =
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-hierarchy";

pub(crate) fn is_obligation_extension(ext: &Value) -> bool {
    ext.get("url").and_then(Value::as_str)
        == Some("http://hl7.org/fhir/StructureDefinition/obligation")
}

pub(crate) fn is_structuredefinition_hierarchy_extension(ext: &Value) -> bool {
    ext.get("url").and_then(Value::as_str) == Some(STRUCTUREDEFINITION_HIERARCHY_URL)
}

pub(crate) fn merge_string(base: Option<&str>, derived: &str) -> String {
    if derived.starts_with("...") {
        // R5 mergeStrings passes appendDerivedTextToBase arguments in the opposite
        // order from mergeMarkdown. Preserve that quirk.
        let suffix_source = base.unwrap_or("");
        if suffix_source.starts_with("...") {
            format!("{derived}\r\n{}", &suffix_source[3..])
        } else {
            format!("{derived}\r\n{suffix_source}")
        }
    } else if derived.is_empty() {
        base.unwrap_or("").to_string()
    } else {
        derived.to_string()
    }
}

pub(crate) fn append_derived_text_to_base(base: Option<&str>, derived: &str) -> String {
    let derived_tail = derived.strip_prefix("...").unwrap_or(derived);
    match base {
        Some(base) if !base.is_empty() => format!("{base}\r\n{derived_tail}"),
        _ => derived.to_string(),
    }
}

pub(crate) fn merge_unique_array_strings(target: &mut Value, diff: &Value, key: &str) {
    let Some(derived) = diff.get(key).and_then(Value::as_array) else {
        return;
    };
    let target_arr = ensure_array_field(target, key);
    for item in derived {
        if !target_arr.contains(item) {
            target_arr.push(item.clone());
        }
    }
}

pub(crate) fn merge_unique_values(target: &mut Value, diff: &Value, key: &str) {
    let Some(derived) = diff.get(key).and_then(Value::as_array) else {
        return;
    };
    let target_arr = ensure_array_field(target, key);
    for item in derived {
        if !target_arr.contains(item) {
            target_arr.push(item.clone());
        }
    }
}

pub(crate) fn merge_binding(target: &mut Value, diff: &Value) {
    let Some(derived) = diff.get("binding").and_then(Value::as_object) else {
        return;
    };
    let mut nb = target
        .get("binding")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    if let Some(obj) = nb.as_object_mut() {
        obj.remove("extension");
        obj.remove("description");
        if let Some(ext) = derived.get("extension") {
            obj.insert("extension".to_string(), ext.clone());
        }
        for key in ["strength", "description", "valueSet"] {
            if let Some(v) = derived.get(key) {
                obj.insert(key.to_string(), v.clone());
            }
        }
        if let Some(additional) = derived.get("additional").and_then(Value::as_array) {
            let entry = obj
                .entry("additional".to_string())
                .or_insert_with(|| Value::Array(vec![]));
            let Some(target_additional) = entry.as_array_mut() else {
                return;
            };
            for item in additional {
                merge_additional_binding(target_additional, item);
            }
        }
        if matches!(obj.get("extension"), Some(Value::Array(a)) if a.is_empty()) {
            obj.remove("extension");
        }
    }
    set_field(target, "binding", nb);
}

pub(crate) fn merge_additional_binding(target: &mut Vec<Value>, source: &Value) {
    let source_vs = source.get("valueSet");
    let source_purpose = source.get("purpose");
    let source_has_usage = source
        .get("usage")
        .and_then(Value::as_array)
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if !source_has_usage {
        if let Some(existing) = target
            .iter_mut()
            .find(|item| item.get("valueSet") == source_vs && item.get("purpose") == source_purpose)
        {
            if let (Some(existing_obj), Some(source_obj)) =
                (existing.as_object_mut(), source.as_object())
            {
                for key in ["shortDoco", "documentation", "any"] {
                    if let Some(v) = source_obj.get(key) {
                        existing_obj.insert(key.to_string(), v.clone());
                    }
                }
                if let Some(source_usage) = source_obj.get("usage").and_then(Value::as_array) {
                    let usage = existing_obj
                        .entry("usage".to_string())
                        .or_insert_with(|| Value::Array(vec![]));
                    if let Some(usage) = usage.as_array_mut() {
                        for u in source_usage {
                            if !usage.contains(u) {
                                usage.push(u.clone());
                            }
                        }
                    }
                }
            }
            return;
        }
    }
    target.push(source.clone());
}

pub(crate) fn has_bindable_type(element: &Value) -> bool {
    let Some(types) = element.get("type").and_then(Value::as_array) else {
        return false;
    };
    types.iter().any(|t| {
        matches!(
            t.get("code").and_then(Value::as_str),
            Some(
                "Coding"
                    | "CodeableConcept"
                    | "Quantity"
                    | "uri"
                    | "string"
                    | "code"
                    | "CodeableReference"
            )
        )
    })
}

pub(crate) fn merge_type_entries(target: &mut Value, derived: &Value) {
    let Some(derived_types) = derived.as_array() else {
        set_field(target, "type", derived.clone());
        return;
    };
    let inherited_types = target
        .get("type")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut merged = Vec::new();
    for derived_type in derived_types {
        let mut next = derived_type.clone();
        if let Some(code) = derived_type.get("code").and_then(Value::as_str) {
            if let Some(inherited_type) = inherited_types
                .iter()
                .find(|candidate| candidate.get("code").and_then(Value::as_str) == Some(code))
            {
                merge_type_extensions(&mut next, inherited_type);
            }
        }
        merged.push(next);
    }
    set_field(target, "type", Value::Array(merged));
}

pub(crate) fn merge_type_extensions(derived_type: &mut Value, inherited_type: &Value) {
    let Some(inherited_exts) = inherited_type.get("extension").and_then(Value::as_array) else {
        return;
    };
    let Some(derived_obj) = derived_type.as_object_mut() else {
        return;
    };
    let mut merged = derived_obj
        .get("extension")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for ext in inherited_exts.iter().filter(|ext| {
        !is_obligation_extension(ext) && !is_structuredefinition_hierarchy_extension(ext)
    }) {
        if !merged.contains(ext) {
            merged.push(ext.clone());
        }
    }
    for ext in inherited_exts.iter().filter(|ext| {
        is_obligation_extension(ext) && !is_structuredefinition_hierarchy_extension(ext)
    }) {
        if !merged.contains(ext) {
            merged.push(ext.clone());
        }
    }
    if !merged.is_empty() {
        derived_obj.insert("extension".to_string(), Value::Array(merged));
    }
}

pub(crate) fn copy_if_present(target: &mut Value, diff: &Value, key: &str) {
    if let Some(value) = diff.get(key) {
        set_field(target, key, value.clone());
    }
}

pub(crate) fn copy_choice_prefix(target: &mut Value, diff: &Value, prefix: &str) {
    let Some(obj) = diff.as_object() else {
        return;
    };
    for (key, value) in obj {
        if key.starts_with(prefix) {
            remove_choice_prefix(target, prefix);
            set_field(target, key, value.clone());
        }
    }
}

pub(crate) fn remove_choice_prefix(target: &mut Value, prefix: &str) {
    let Some(obj) = target.as_object_mut() else {
        return;
    };
    let keys: Vec<String> = obj
        .keys()
        .filter(|key| key.starts_with(prefix))
        .cloned()
        .collect();
    for key in keys {
        obj.remove(&key);
    }
}

pub(crate) fn set_field(target: &mut Value, key: &str, value: Value) {
    let Some(target) = target.as_object_mut() else {
        return;
    };
    target.insert(key.to_string(), value);
}

pub(crate) fn remove_field(target: &mut Value, key: &str) {
    if let Some(target) = target.as_object_mut() {
        target.remove(key);
    }
}

pub(crate) fn ensure_array_field<'a>(target: &'a mut Value, key: &str) -> &'a mut Vec<Value> {
    let Some(target) = target.as_object_mut() else {
        panic!("element is not an object");
    };
    let entry = target
        .entry(key.to_string())
        .or_insert_with(|| Value::Array(vec![]));
    if !entry.is_array() {
        *entry = Value::Array(vec![]);
    }
    entry.as_array_mut().expect("array just inserted")
}

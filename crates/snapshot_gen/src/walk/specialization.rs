//! Publisher `ProfileUtilities` specialization phases.
//!
//! Constraint profiles primarily walk an inherited element tree. A
//! specialization instead introduces a new type: Q20 renames the inherited
//! root, Q2 inserts new differential children (expanding a declared type when
//! the differential walks into it), and Q9 completes base metadata for new
//! elements. Keeping those phases together leaves `walk::mod` as orchestration.

use anyhow::Context as _;
use serde_json::Value;

use super::{
    consts, context::WalkContext, emit, paths, preprocess, resolve, types_pred, updatefromdef,
};

pub(super) fn type_name(definition: &Value) -> Option<&str> {
    definition
        .get("type")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| value.rsplit('/').next().unwrap_or(value))
}

/// ProfileUtilities Q20 (`cloneSnapshot`): specialize inherited ids and paths
/// under the derived type while retaining original `ElementDefinition.base`.
pub(super) fn clone_snapshot(elements: &mut [Value], base_type: &str, derived_type: &str) {
    for element in elements {
        let Some(object) = element.as_object_mut() else {
            continue;
        };
        for field in ["id", "path"] {
            let Some(value) = object.get(field).and_then(Value::as_str) else {
                continue;
            };
            object.insert(
                field.to_string(),
                Value::String(value.replacen(base_type, derived_type, 1)),
            );
        }
    }
}

/// ProfileUtilities Q2 (PU:842-867): specialization differentials may declare
/// children that do not exist in the base snapshot. The ordinary path walk can
/// only consume inherited rows, so add or update the remaining rows in their
/// parent's subtree after the walk. When a newly declared element is followed
/// by constraints on its children, materialize that element's type snapshot
/// first, exactly as `addInheritedElementsForSpecialization` does.
pub(super) fn apply_additions(
    ctx: &mut WalkContext,
    derived: &Value,
    url: &str,
    derived_versioned_url: &str,
) -> anyhow::Result<()> {
    let profile_name = derived
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let differential = ctx.diff.clone();
    for diff_index in 0..differential.len() {
        if ctx.diff_consumed.get(diff_index).copied().unwrap_or(false) {
            continue;
        }
        let source = differential[diff_index].clone();
        let path = paths::path_of(&source).to_string();
        if !path.contains('.') {
            continue;
        }

        let source_id = source.get("id").and_then(Value::as_str);
        let existing = element_in_current_context(&path, &ctx.output);
        if let Some(index) = existing {
            let mut updated = ctx.output[index].clone();
            updatefromdef::update_from_definition(
                ctx,
                &mut updated,
                &source,
                &profile_name,
                false,
                url,
                false,
            );
            ctx.output[index] = updated;
            ctx.mark_consumed(diff_index);
            continue;
        }

        let insertion = insertion_point(&ctx.output, &path)?;
        let mut outcome = source.clone();
        emit::update_urls(&mut outcome, url, &ctx.spec_url);
        preprocess::mark_extensions(&mut outcome, derived_versioned_url);
        ctx.insert_into_result(insertion, outcome.clone(), Some(diff_index));

        let walks_into = differential
            .get(diff_index + 1)
            .map(|next| paths::path_of(next).starts_with(&format!("{path}.")))
            .unwrap_or(false);
        if walks_into {
            let type_codes = types_pred::type_codes(&outcome);
            if type_codes.len() > 1 {
                anyhow::bail!(
                    "Unsupported scenario: specialization walks into multiple types at {}",
                    source_id.unwrap_or(&path)
                );
            }
            if let Some(type_code) = type_codes.first() {
                add_inherited_elements(ctx, insertion, type_code, &path, url)?;
            }
        }
    }
    Ok(())
}

/// `ProfileUtilities.getElementInCurrentContext` searches backwards and stops
/// as soon as it leaves the current parent subtree. A global first-path or id
/// match can select a same-path slice from an earlier context.
fn element_in_current_context(path: &str, elements: &[Value]) -> Option<usize> {
    for (index, element) in elements.iter().enumerate().rev() {
        let candidate = paths::path_of(element);
        if candidate == path {
            return Some(index);
        }
        let head = candidate
            .rfind('.')
            .map(|dot| &candidate[..=dot])
            .unwrap_or(candidate);
        if !path.starts_with(head) {
            return None;
        }
    }
    None
}

fn insertion_point(elements: &[Value], path: &str) -> anyhow::Result<usize> {
    let parent = path
        .rsplit_once('.')
        .map(|(parent, _)| parent)
        .context("specialization child has no parent path")?;
    let parent_index = elements
        .iter()
        .position(|element| paths::path_of(element) == parent)
        .with_context(|| format!("specialization parent not found: {parent}"))?;
    let mut insertion = parent_index + 1;
    // Publisher's findLastChildForParent uses raw startsWith(parentPath), not a
    // dotted child prefix. Preserve even its prefix-collision ordering behavior.
    while insertion < elements.len() && paths::path_of(&elements[insertion]).starts_with(parent) {
        insertion += 1;
    }
    Ok(insertion)
}

fn add_inherited_elements(
    ctx: &mut WalkContext,
    focus_index: usize,
    type_code: &str,
    path: &str,
    url: &str,
) -> anyhow::Result<()> {
    let Some(definition) = resolve::resolve_with_snapshot(ctx, type_code)? else {
        return Ok(());
    };
    let source_type = type_name(&definition)
        .with_context(|| format!("specialization type {type_code} has no type"))?
        .to_string();
    let versioned_source = match (
        definition.get("url").and_then(Value::as_str),
        definition.get("version").and_then(Value::as_str),
    ) {
        (Some(source_url), Some(version)) if !version.is_empty() => {
            format!("{source_url}|{version}")
        }
        (Some(source_url), _) => source_url.to_string(),
        _ => type_code.to_string(),
    };
    let elements = definition
        .pointer("/snapshot/element")
        .and_then(Value::as_array)
        .cloned()
        .with_context(|| format!("specialization type {type_code} has no snapshot"))?;

    for source in elements {
        let source_path = paths::path_of(&source);
        if !source_path.contains('.') {
            merge_root(ctx, focus_index, &source, &versioned_source);
            continue;
        }
        let mut outcome = source;
        emit::update_urls(&mut outcome, url, &ctx.spec_url);
        // Java String.replace replaces every occurrence, including a repeated
        // type token in a descendant segment.
        let inherited_path = paths::path_of(&outcome).replace(&source_type, path);
        if let Some(object) = outcome.as_object_mut() {
            object.insert("path".to_string(), Value::String(inherited_path));
        }
        preprocess::mark_extensions(&mut outcome, &versioned_source);
        // Publisher appends inherited type rows to the snapshot; it does not
        // splice them beside the newly introduced focus element.
        ctx.insert_into_result(ctx.output.len(), outcome, None);
    }
    Ok(())
}

fn merge_root(ctx: &mut WalkContext, focus_index: usize, source: &Value, versioned_source: &str) {
    let mut source = source.clone();
    preprocess::mark_extensions(&mut source, versioned_source);
    let Some(focus) = ctx
        .output
        .get_mut(focus_index)
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    if let Some(inherited) = source.get("constraint").and_then(Value::as_array) {
        focus
            .entry("constraint".to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("constraint is an array")
            .extend(inherited.iter().cloned());
    }
    if let Some(inherited) = source.get("extension").and_then(Value::as_array) {
        let extensions = focus
            .entry("extension".to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("extension is an array");
        for extension in inherited {
            let Some(extension_url) = extension.get("url").and_then(Value::as_str) else {
                continue;
            };
            if consts::NON_INHERITED_ED_URLS.contains(&extension_url)
                || extensions.iter().any(|existing| {
                    existing.get("url").and_then(Value::as_str) == Some(extension_url)
                })
            {
                continue;
            }
            extensions.push(extension.clone());
        }
        if extensions.is_empty() {
            focus.remove("extension");
        }
    }
}

/// ProfileUtilities Q9 (PU:969-975): every element introduced by a
/// specialization gets self-referential base cardinality when it did not
/// inherit a base element.
pub(super) fn ensure_bases(ctx: &mut WalkContext) {
    for element in &mut ctx.output {
        if element.get("base").is_some() {
            continue;
        }
        let path = paths::path_of(element).to_string();
        let min = element.get("min").cloned().unwrap_or(Value::from(0));
        let max = element.get("max").cloned();
        if let Some(object) = element.as_object_mut() {
            let mut base = serde_json::Map::new();
            base.insert("path".to_string(), Value::String(path));
            base.insert("min".to_string(), min);
            if let Some(max) = max {
                base.insert("max".to_string(), max);
            }
            object.insert("base".to_string(), Value::Object(base));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{element_in_current_context, insertion_point};
    use serde_json::json;

    #[test]
    fn current_context_selects_the_last_same_path() {
        let elements = vec![
            json!({ "id": "Model.item:first", "path": "Model.item" }),
            json!({ "id": "Model.item:first.child", "path": "Model.item.child" }),
            json!({ "id": "Model.item:second", "path": "Model.item" }),
        ];
        assert_eq!(element_in_current_context("Model.item", &elements), Some(2));
    }

    #[test]
    fn current_context_does_not_cross_a_sibling_subtree() {
        let elements = vec![
            json!({ "id": "Model.item", "path": "Model.item" }),
            json!({ "id": "Model.group.child", "path": "Model.group.child" }),
        ];
        assert_eq!(element_in_current_context("Model.item", &elements), None);
    }

    #[test]
    fn insertion_matches_publishers_raw_parent_prefix_scan() {
        let elements = vec![
            json!({ "path": "Model" }),
            json!({ "path": "Model.a" }),
            json!({ "path": "Model.ab" }),
            json!({ "path": "Model.b" }),
        ];
        assert_eq!(insertion_point(&elements, "Model.a.child").unwrap(), 3);
    }
}

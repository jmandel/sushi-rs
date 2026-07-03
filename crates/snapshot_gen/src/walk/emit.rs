//! Emission-time leaf helpers used when a base element is cloned into the
//! output: updateFromBase, updateConstraintSources, checkExtensions,
//! markExtensions (source stamp is annotation-only for now), markDerived.
//! Citations: ProfileUtilities.java on `snap-trace`.

use serde_json::{Map, Value};

use super::consts::NON_INHERITED_ED_URLS;

/// PU:2024 updateFromBase — stamp base bookkeeping + fill `.base`.
pub(crate) fn update_from_base(derived: &mut Value, base: &Value) {
    let (bpath, bmin, bmax) = if let Some(b) = base.get("base") {
        (
            b.get("path").cloned(),
            b.get("min").cloned(),
            b.get("max").cloned(),
        )
    } else {
        (
            base.get("path").cloned(),
            base.get("min").cloned().or(Some(Value::from(0))),
            base.get("max").cloned(),
        )
    };
    let Some(obj) = derived.as_object_mut() else {
        return;
    };
    let mut base_obj = obj
        .get("base")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(p) = bpath {
        base_obj.insert("path".to_string(), p);
    }
    if let Some(m) = bmin {
        base_obj.insert("min".to_string(), m);
    }
    if let Some(m) = bmax {
        base_obj.insert("max".to_string(), m);
    }
    obj.insert("base".to_string(), Value::Object(base_obj));
}

/// PU:1610 updateConstraintSources — fill missing constraint.source.
pub(crate) fn update_constraint_sources(ed: &mut Value, url: &str) {
    let Some(constraints) = ed.get_mut("constraint").and_then(Value::as_array_mut) else {
        return;
    };
    for c in constraints {
        if let Some(obj) = c.as_object_mut() {
            if !obj.contains_key("source") {
                obj.insert("source".to_string(), Value::String(url.to_string()));
            }
        }
    }
}

/// PU:5032 checkExtensions — strip NON_INHERITED extensions (element + binding).
pub(crate) fn check_extensions(ed: &mut Value) {
    strip_non_inherited(ed);
    if let Some(binding) = ed.get_mut("binding") {
        strip_non_inherited(binding);
    }
}

fn strip_non_inherited(parent: &mut Value) {
    let Some(obj) = parent.as_object_mut() else {
        return;
    };
    if let Some(Value::Array(exts)) = obj.get_mut("extension") {
        exts.retain(|ext| {
            let url = ext.get("url").and_then(Value::as_str).unwrap_or("");
            !NON_INHERITED_ED_URLS.contains(&url)
        });
        if exts.is_empty() {
            obj.remove("extension");
        }
    }
}

/// Deep-clone an element for emission (Java `.copy()`). We keep the JSON value;
/// annotations are tracked separately in the context.
pub(crate) fn clone_element(ed: &Value) -> Value {
    ed.clone()
}

/// PU:2155 updateURLs — rewrite relative markdown links to absolute using the
/// context spec url, and turn `#`-prefixed valueSet/profile into absolute using
/// the derived url. `processRelatives=true` in the walk.
pub(crate) fn update_urls(element: &mut Value, url: &str, spec_url: &str) {
    // valueSet #-prefix -> url + valueSet
    let vs_rest = element
        .get("binding")
        .and_then(|b| b.get("valueSet"))
        .and_then(Value::as_str)
        .and_then(|vs| vs.strip_prefix('#'))
        .map(str::to_string);
    if let Some(rest) = vs_rest {
        if let Some(binding) = element.get_mut("binding") {
            let full = format!("{url}#{rest}");
            crate::merge::set_field(binding, "valueSet", Value::String(full));
        }
    }
    // markdown link rewriting (definition/comment/requirements/meaningWhenMissing/binding.description/ext.valueMarkdown)
    crate::text::rewrite_markdown_links(element, spec_url, false);
}

/// Set an element's id to null (Java `setPath`/`setId(null)`), i.e. remove it so
/// Q6 setIds regenerates it.
pub(crate) fn clear_id(ed: &mut Value) {
    if let Some(obj) = ed.as_object_mut() {
        obj.remove("id");
    }
}

pub(crate) fn empty_object() -> Value {
    Value::Object(Map::new())
}

//! `updateFromDefinition` — per-field merge of one diff element onto one base
//! clone (PU:2605-3157). Faithful port; reuses the clean `merge.rs` leaves.
//! Deliberately-skipped (documented) branches:
//!   - obligation-profile element merges (obligationProfiles empty under oracle → DEAD)
//!   - tx-server binding subset validation (PU:2989-3029): messages only, needs a
//!     terminology server; spec §4 row 24 marks it SKIPPABLE.
//!   - EXT_TRANSLATABLE dup-drop (PU:2631) and EXT_PROFILE_ELEMENT handling (rare).
//! Type-profile-root override (PU:2679-2717) lives in `simple.rs`'s template
//! selection for the walk (clone source), so here we only do the field merges
//! that always run.

use serde_json::Value;

use super::consts::{
    DEFAULT_INHERITED_ED_URLS, NON_INHERITED_ED_URLS, NON_OVERRIDING_ED_URLS, OVERRIDING_ED_URLS,
};
use super::context::{Severity, WalkContext};
use super::trace;
use crate::merge::*;
use crate::{append_derived_text_to_base, merge_string};

/// PU:1963 checkExtensionDoco — Java-exact walk port. NOTE: the legacy engine's
/// shared `crate::check_extension_doco` adds a `has_profiled_extension_type`
/// guard that Java does NOT have (a legacy corpus heuristic); Java normalizes
/// the doco for ANY `.extension`/`.modifierExtension` path (except II.extension
/// bases) even when the element carries a profiled Extension type
/// (eCR `PlanDefinition.action.trigger.extension:namedEventType`).
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
        && base_path != Some("II.extension");
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

fn has(ed: &Value, key: &str) -> bool {
    ed.get(key).map(|v| !v.is_null()).unwrap_or(false)
}

fn deep_eq(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a == b,
        (None, None) => true,
        _ => false,
    }
}

/// Merge markdown the Java way: dest=derived, source=base. If derived starts
/// "..." append base tail; if derived empty use base.
fn merge_markdown_java(derived: &str, base: Option<&str>) -> String {
    if derived.is_empty() {
        base.unwrap_or("").to_string()
    } else if derived.starts_with("...") {
        append_derived_text_to_base(base, derived)
    } else {
        derived.to_string()
    }
}

/// PU:2605 updateFromDefinition. `dest` is the base clone being built; `source`
/// is the diff element. `src_sd_url` stamps constraint sources. `from_slicer`
/// gates the mustSupport/mustHaveValue weakening error.
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_from_definition(
    ctx: &mut WalkContext,
    dest: &mut Value,
    source: &Value,
    profile_name: &str,
    trim_differential: bool,
    src_sd_url: &str,
    from_slicer: bool,
) {
    if trace::active() {
        let x = serde_json::json!({
            "fromSlicer": from_slicer,
            "trimDifferential": trim_differential,
            "isSliceRoot": has(source, "sliceName"),
            "srcSD": src_sd_url,
        });
        trace::rec(
            "updateFromDefinition",
            "updateFromDefinition.entry",
            trace::id(dest).as_deref(),
            trace::id(source).as_deref(),
            Some(x),
        );
    }
    let is_extension = check_extension_doco(dest);
    if is_extension && trace::active() {
        trace::rec(
            "updateFromDefinition",
            "updateFromDefinition.checkExtensionDoco",
            trace::id(dest).as_deref(),
            trace::id(source).as_deref(),
            None,
        );
    }

    // PU:2631 hack: if dest has exactly two EXT_TRANSLATABLE, drop the 2nd.
    drop_second_translatable(dest);
    // updateExtensionsFromDefinition(dest, source) — element-level extensions.
    update_extensions_from_definition(dest, source);

    // PU:2648-2717 profile-on-type root override: when the (base slice's or the
    // diff's) single type profile resolves to an Extension / resource SD, the
    // profile snapshot root's doco overwrites the base clone's doco (restoring
    // it after checkExtensionDoco normalized it away).
    apply_profile_root_doco(ctx, dest, source);

    let path = dest.get("path").and_then(Value::as_str).unwrap_or("").to_string();
    let derived_path = source.get("path").and_then(Value::as_str).unwrap_or("").to_string();

    // sliceName copy
    if let Some(sn) = source.get("sliceName") {
        set_field(dest, "sliceName", sn.clone());
    }

    // short: plain override (deep-compare)
    if let Some(d) = source.get("short") {
        if dest.get("short") != Some(d) {
            set_field(dest, "short", d.clone());
        }
    }
    // definition: mergeMarkdown
    if let Some(d) = source.get("definition").and_then(Value::as_str) {
        if dest.get("definition").and_then(Value::as_str) != Some(d) {
            let base = dest.get("definition").and_then(Value::as_str);
            set_field(dest, "definition", Value::String(merge_markdown_java(d, base)));
        }
    }
    // comment: mergeMarkdown
    if let Some(d) = source.get("comment").and_then(Value::as_str) {
        if dest.get("comment").and_then(Value::as_str) != Some(d) {
            let base = dest.get("comment").and_then(Value::as_str);
            set_field(dest, "comment", Value::String(merge_markdown_java(d, base)));
        }
    }
    // label: mergeStrings
    if let Some(d) = source.get("label").and_then(Value::as_str) {
        if dest.get("label").and_then(Value::as_str) != Some(d) {
            let base = dest.get("label").and_then(Value::as_str);
            set_field(dest, "label", Value::String(merge_string(base, d)));
        }
    }
    // requirements: mergeMarkdown
    if let Some(d) = source.get("requirements").and_then(Value::as_str) {
        if dest.get("requirements").and_then(Value::as_str) != Some(d) {
            let base = dest.get("requirements").and_then(Value::as_str);
            set_field(dest, "requirements", Value::String(merge_markdown_java(d, base)));
        }
    }
    // sdf-9: drop requirements on root (path has no ".") in both
    if !path.contains('.') {
        remove_field(dest, "requirements");
    }

    // alias: additive union
    if source.get("alias").and_then(Value::as_array).is_some() {
        if source.get("alias") != dest.get("alias") {
            merge_unique_array_strings(dest, source, "alias");
        }
    }

    // min: override; ERROR if derived.min < base.min and not a slice
    if let Some(dmin) = source.get("min").and_then(Value::as_u64) {
        if dest.get("min").and_then(Value::as_u64) != Some(dmin) {
            let bmin = dest.get("min").and_then(Value::as_u64).unwrap_or(0);
            if dmin < bmin && !has(source, "sliceName") {
                ctx.add_message(
                    Severity::Error,
                    &format!("{profile_name}.{derived_path}"),
                    format!("Element {path}: derived min ({dmin}) cannot be less than the base min ({bmin}) in {src_sd_url}"),
                );
            }
            set_field(dest, "min", Value::from(dmin));
        }
    }
    // max: override; ERROR if isLargerMax
    if let Some(dmax) = source.get("max").and_then(Value::as_str) {
        if dest.get("max").and_then(Value::as_str) != Some(dmax) {
            let bmax = dest.get("max").and_then(Value::as_str).unwrap_or("*");
            if is_larger_max(dmax, bmax) {
                ctx.add_message(
                    Severity::Error,
                    &format!("{profile_name}.{derived_path}"),
                    format!("Element {path}: derived max ({dmax}) cannot be greater than the base max ({bmax})"),
                );
            }
            set_field(dest, "max", Value::String(dmax.to_string()));
        }
    }

    // fixed[x] / pattern[x]: override (choice-typed field)
    copy_choice_prefix(dest, source, "fixed");
    copy_choice_prefix(dest, source, "pattern");

    // example[]: additive merge (PU:2827-2856). Each derived example not already
    // present in base (by label+value) is appended. The EXT_ED_SUPPRESS delete
    // path ($all / suppress) is rare and not yet exercised; append-if-missing.
    if let Some(derived_examples) = source.get("example").and_then(Value::as_array) {
        let mut base_examples = dest
            .get("example")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for ex in derived_examples {
            let found = base_examples.iter().any(|be| {
                be.get("label") == ex.get("label")
                    && example_value(be) == example_value(ex)
            });
            if !found {
                base_examples.push(ex.clone());
            }
        }
        if !base_examples.is_empty() {
            set_field(dest, "example", Value::Array(base_examples));
        }
    }

    // maxLength / maxValue[x] / minValue[x]: override
    if source.get("maxLength").is_some() && source.get("maxLength") != dest.get("maxLength") {
        copy_if_present(dest, source, "maxLength");
    }
    copy_choice_prefix(dest, source, "maxValue");
    copy_choice_prefix(dest, source, "minValue");

    // mustSupport: ERROR if base MS true and derived MS false and !fromSlicer
    if let Some(dms) = source.get("mustSupport").and_then(Value::as_bool) {
        let bms = dest.get("mustSupport").and_then(Value::as_bool);
        if bms != Some(dms) {
            if bms == Some(true) && !dms && !from_slicer {
                ctx.add_message(
                    Severity::Error,
                    &format!("{profile_name}.{derived_path}"),
                    "Illegal constraint [must-support = false] when [must-support = true] in the base profile".to_string(),
                );
            }
            set_field(dest, "mustSupport", Value::Bool(dms));
        }
    }
    // mustHaveValue: like mustSupport
    if let Some(dmhv) = source.get("mustHaveValue").and_then(Value::as_bool) {
        let bmhv = dest.get("mustHaveValue").and_then(Value::as_bool);
        if bmhv != Some(dmhv) {
            if bmhv == Some(true) && !dmhv && !from_slicer {
                ctx.add_message(
                    Severity::Error,
                    &format!("{profile_name}.{derived_path}"),
                    "Illegal constraint [must-have-value = false] when [must-have-value = true] in the base profile".to_string(),
                );
            }
            set_field(dest, "mustHaveValue", Value::Bool(dmhv));
        }
    }
    // valueAlternatives: additive union
    if source.get("valueAlternatives").and_then(Value::as_array).is_some()
        && source.get("valueAlternatives") != dest.get("valueAlternatives")
    {
        merge_unique_array_strings(dest, source, "valueAlternatives");
    }

    // isModifier / isModifierReason: only if isExtension
    if is_extension {
        if let Some(im) = source.get("isModifier") {
            if dest.get("isModifier") != Some(im) {
                set_field(dest, "isModifier", im.clone());
            }
        }
        if let Some(imr) = source.get("isModifierReason") {
            if dest.get("isModifierReason") != Some(imr) {
                set_field(dest, "isModifierReason", imr.clone());
            }
        }
        if dest.get("isModifier").and_then(Value::as_bool) == Some(true)
            && !has(dest, "isModifierReason")
        {
            set_field(dest, "isModifierReason", Value::String(
                "Modifier extensions are labelled as such because they modify the meaning or interpretation of the resource or element that contains them".to_string()));
        }
    }

    // binding: only-narrow merge
    if has(source, "binding") {
        if !has(dest, "binding") || !deep_eq(source.get("binding"), dest.get("binding")) {
            if dest.get("binding").and_then(|b| b.get("strength")).and_then(Value::as_str) == Some("required")
                && source.get("binding").and_then(|b| b.get("strength")).and_then(Value::as_str) != Some("required")
            {
                ctx.add_message(
                    Severity::Error,
                    &format!("{profile_name}.{derived_path}"),
                    format!("illegal attempt to change the binding on {derived_path}"),
                );
            }
            merge_binding(dest, source);
        }
    } else if has(dest, "binding") {
        // PU:3061 else-if: strip NON_INHERITED from base binding extensions.
        if let Some(binding) = dest.get_mut("binding") {
            if let Some(arr) = binding.get_mut("extension").and_then(Value::as_array_mut) {
                arr.retain(|ext| {
                    let url = ext.get("url").and_then(Value::as_str).unwrap_or("");
                    !NON_INHERITED_ED_URLS.contains(&url)
                });
                if arr.is_empty() {
                    remove_field(binding, "extension");
                }
            }
        }
    }

    // isSummary: override; Error if base has isSummary and changed (version != 1.4.0)
    if let Some(is) = source.get("isSummary") {
        if dest.get("isSummary") != Some(is) {
            set_field(dest, "isSummary", is.clone());
        }
    }

    // type: replace wholesale
    if let Some(dtypes) = source.get("type") {
        if dest.get("type") != Some(dtypes) {
            merge_type_entries(dest, dtypes);
        }
    }

    // mapping: MappingAssistant.merge(derived, base) at PU:3111 — differential
    // element mappings come FIRST, then inherited base mappings, deduped by
    // (identity, map) with map trimmed (R4/non-R5Plus path; rename map omitted —
    // not exercised by current corpus, would need SD-level mapping declarations).
    merge_mappings(dest, source);

    // constraint: stamp base SNAPSHOT_IS_DERIVED + fill source; then additive
    fill_constraint_sources(dest, src_sd_url);
    if let Some(dcons) = source.get("constraint").and_then(Value::as_array) {
        let existing_keys: Vec<String> = dest
            .get("constraint")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| c.get("key").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let dcons = dcons.clone();
        for c in &dcons {
            let key = c.get("key").and_then(Value::as_str);
            if key.is_none() || !existing_keys.iter().any(|k| Some(k.as_str()) == key) {
                ensure_array_field(dest, "constraint").push(c.clone());
            }
        }
    }
    // condition: additive
    if source.get("condition").and_then(Value::as_array).is_some() {
        merge_unique_values(dest, source, "condition");
    }

    // delete binding if no bindable type after merge
    if has(dest, "binding") && !has_bindable_type(dest) {
        remove_field(dest, "binding");
    }
}

/// PU:3228 updateExtensionsFromDefinition (element-level).
fn update_extensions_from_definition(dest: &mut Value, source: &Value) {
    let source_urls: Vec<String> = source
        .get("extension")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| e.get("url").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if let Some(arr) = dest.get_mut("extension").and_then(Value::as_array_mut) {
        arr.retain(|ext| {
            let url = ext.get("url").and_then(Value::as_str).unwrap_or("");
            !(NON_INHERITED_ED_URLS.contains(&url)
                || (DEFAULT_INHERITED_ED_URLS.contains(&url) && source_urls.iter().any(|u| u == url)))
        });
        if arr.is_empty() {
            remove_field(dest, "extension");
        }
    }
    let Some(source_exts) = source.get("extension").and_then(Value::as_array) else {
        return;
    };
    let source_exts = source_exts.clone();
    for ext in &source_exts {
        let url = ext.get("url").and_then(Value::as_str).unwrap_or("");
        let dest_has = dest
            .get("extension")
            .and_then(Value::as_array)
            .map(|a| a.iter().any(|e| e.get("url").and_then(Value::as_str) == Some(url)))
            .unwrap_or(false);
        if !dest_has {
            ensure_array_field(dest, "extension").push(ext.clone());
        } else if NON_OVERRIDING_ED_URLS.contains(&url) {
            // do nothing (keep dest's) — PU:3234-3238.
        } else if OVERRIDING_ED_URLS.contains(&url) {
            // PU:3239-3241: set value on the first existing extension with this url.
            if let Some(arr) = dest.get_mut("extension").and_then(Value::as_array_mut) {
                if let Some(existing) = arr
                    .iter_mut()
                    .find(|e| e.get("url").and_then(Value::as_str) == Some(url))
                {
                    if let Some(obj) = existing.as_object_mut() {
                        let value_keys: Vec<String> = obj
                            .keys()
                            .filter(|k| k.starts_with("value"))
                            .cloned()
                            .collect();
                        for k in value_keys {
                            obj.remove(&k);
                        }
                        if let Some(src_obj) = ext.as_object() {
                            for (k, v) in src_obj {
                                if k.starts_with("value") {
                                    obj.insert(k.clone(), v.clone());
                                }
                            }
                        }
                    }
                }
            }
        } else if let Some(arr) = dest.get_mut("extension").and_then(Value::as_array_mut) {
            // Default: append a duplicate (Java `else`, PU:3242-3244).
            arr.push(ext.clone());
        }
    }
}

/// PU:2648-2717. `dest` = base clone being built; `source` = diff element.
fn apply_profile_root_doco(ctx: &mut WalkContext, dest: &mut Value, source: &Value) {
    fn single_type_profile(ed: &Value) -> Option<String> {
        let types = ed.get("type").and_then(Value::as_array)?;
        let first = types.first()?;
        first
            .get("profile")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    let mut profile: Option<std::rc::Rc<Value>> = None;
    // Branch 1 (PU:2650-2652): dest is a named slice with exactly one profiled type.
    if has(dest, "sliceName") {
        let dest_type_count = dest
            .get("type")
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or(0);
        if dest_type_count == 1 {
            if let Some(purl) = single_type_profile(dest) {
                profile = super::resolve::resolve_with_snapshot(ctx, &purl).ok().flatten();
            }
        }
    }
    // Branch 2 (PU:2653-2678): the diff's first type profile.
    if profile.is_none() {
        if let Some(purl) = single_type_profile(source) {
            if let Ok(Some(p)) = super::resolve::resolve_with_snapshot(ctx, &purl) {
                let ptype = p.get("type").and_then(Value::as_str).unwrap_or("");
                let kind = p.get("kind").and_then(Value::as_str).unwrap_or("");
                if ptype == "Extension" || kind == "resource" || kind == "logical" {
                    profile = Some(p);
                }
                // else: deliberately do NOT override (PU:2672-2677).
            }
        }
    }
    let Some(profile) = profile else { return };
    let ptype = profile.get("type").and_then(Value::as_str).unwrap_or("");
    let kind = profile.get("kind").and_then(Value::as_str).unwrap_or("");
    if kind != "resource" && ptype != "Extension" {
        return;
    }
    let Some(root) = profile
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    else {
        return;
    };
    // Java (PU:2686/2688) rewrites the copied root definition / binding.description
    // markdown links via processRelativeUrls(..., true) against the context spec url.
    if let Some(d) = root.get("definition").and_then(Value::as_str) {
        let rewritten =
            crate::text::process_relative_markdown_urls(d, &ctx.spec_url, true);
        set_field(dest, "definition", Value::String(rewritten));
    }
    if let Some(bd) = root.get("binding").and_then(|b| b.get("description")).and_then(Value::as_str) {
        if let Some(binding) = dest.get_mut("binding") {
            let rewritten =
                crate::text::process_relative_markdown_urls(bd, &ctx.spec_url, true);
            set_field(binding, "description", Value::String(rewritten));
        }
    }
    // base.setShort(e.getShort()) — unconditional in Java; extension roots always
    // carry a short in practice, so copy-if-present (else clear).
    match root.get("short") {
        Some(s) => set_field(dest, "short", s.clone()),
        None => remove_field(dest, "short"),
    }
    if let Some(c) = root.get("comment") {
        set_field(dest, "comment", c.clone());
    }
    if let Some(r) = root.get("requirements") {
        set_field(dest, "requirements", r.clone());
    }
    // alias / mapping: clear + addAll (replace wholesale).
    match root.get("alias") {
        Some(a) => set_field(dest, "alias", a.clone()),
        None => remove_field(dest, "alias"),
    }
    match root.get("mapping") {
        Some(m) => set_field(dest, "mapping", m.clone()),
        None => remove_field(dest, "mapping"),
    }
}

/// PU:3111 MappingAssistant.merge(derived, base) — build [diff-mappings ++
/// base-mappings] deduped by (identity, trimmed map). `dest` holds the base
/// (inherited) mappings; `source` is the differential element. If the diff has
/// no mappings, dest is unchanged.
fn merge_mappings(dest: &mut Value, source: &Value) {
    let Some(diff_mappings) = source.get("mapping").and_then(Value::as_array) else {
        return;
    };
    if diff_mappings.is_empty() {
        return;
    }
    let base_mappings = dest.get("mapping").and_then(Value::as_array).cloned().unwrap_or_default();
    let map_key = |m: &Value| -> (String, String) {
        (
            m.get("identity").and_then(Value::as_str).unwrap_or("").to_string(),
            m.get("map").and_then(Value::as_str).unwrap_or("").trim().to_string(),
        )
    };
    let mut list: Vec<Value> = Vec::new();
    // addMappings(list, derived-element mappings) first (Java's `base` param).
    for m in diff_mappings {
        let k = map_key(m);
        if !list.iter().any(|d| map_key(d) == k) {
            list.push(m.clone());
        }
    }
    // then addMappings(list, base-element mappings).
    for m in &base_mappings {
        let k = map_key(m);
        if !list.iter().any(|d| map_key(d) == k) {
            list.push(m.clone());
        }
    }
    set_field(dest, "mapping", Value::Array(list));
}

/// Extract an example component's polymorphic `value[x]` (key, value) for
/// dedup comparison (Base.compareDeep on getValue()).
fn example_value(ex: &Value) -> Option<(String, Value)> {
    ex.as_object()?.iter().find_map(|(k, v)| {
        if let Some(rest) = k.strip_prefix("value") {
            if rest.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
                return Some((k.clone(), v.clone()));
            }
        }
        None
    })
}

const EXT_TRANSLATABLE: &str = "http://hl7.org/fhir/StructureDefinition/elementdefinition-translatable";

/// PU:2631 hack workaround for R5 snapshots: if `dest` carries exactly two
/// EXT_TRANSLATABLE extensions, remove the second.
fn drop_second_translatable(dest: &mut Value) {
    let Some(arr) = dest.get_mut("extension").and_then(Value::as_array_mut) else {
        return;
    };
    let positions: Vec<usize> = arr
        .iter()
        .enumerate()
        .filter(|(_, e)| e.get("url").and_then(Value::as_str) == Some(EXT_TRANSLATABLE))
        .map(|(i, _)| i)
        .collect();
    if positions.len() == 2 {
        arr.remove(positions[1]);
    }
}

fn fill_constraint_sources(ed: &mut Value, url: &str) {
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

/// PU:3409 isLargerMax.
fn is_larger_max(derived: &str, base: &str) -> bool {
    if base == "*" {
        false
    } else if derived == "*" {
        true
    } else {
        let d = derived.parse::<i64>().unwrap_or(i64::MAX);
        let b = base.parse::<i64>().unwrap_or(i64::MAX);
        d > b
    }
}

//! `processSimplePath` (PPP:296) — dispatch for an unsliced base element, and
//! its four sub-branches (§3.3.1-3.3.4).

use serde_json::Value;
use std::rc::Rc;

use super::context::WalkContext;
use super::contentref;
use super::emit::*;
use super::frame::{SlicingParams, WalkCursor, WalkFrame};
use super::paths::*;
use super::resolve::resolve_with_snapshot;
use super::trace;
use super::types::{self, TypeSlice};
use super::types_pred::*;
use super::updatefromdef::update_from_definition;
use crate::merge::set_field;

/// PPP:1909 oneMatchingElementInDifferential.
fn one_matching_element(slicing_done: bool, path: &str, diff_matches: &[Value]) -> bool {
    if diff_matches.len() != 1 {
        return false;
    }
    if slicing_done {
        return true;
    }
    if is_implicit_slicing(&diff_matches[0], path) {
        return false;
    }
    let d = &diff_matches[0];
    !(has_slicing(d) || (is_extension_path(path_of(d)) && has_slice_name(d)))
}

/// PU:1811 isImplicitSlicing.
fn is_implicit_slicing(ed: &Value, path: &str) -> bool {
    let ep = path_of(ed);
    if ep.is_empty() || path == ep {
        return false;
    }
    path.ends_with("[x]") && ep.starts_with(&path[..path.len() - 3])
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn process_simple_path(
    ctx: &mut WalkContext,
    cur: &mut WalkCursor,
    frame: &WalkFrame,
    current_base: &Value,
    current_base_path: &str,
    diff_match_idx: &[usize],
    type_list: &mut Vec<TypeSlice>,
    slicer: Option<&Value>,
) -> anyhow::Result<Option<usize>> {
    let diff_matches: Vec<Value> = diff_match_idx.iter().map(|&i| ctx.diff[i].clone()).collect();
    let mut res = None;

    if diff_matches.is_empty() {
        trace::rec(
            "processSimplePath",
            "processSimplePath.emptyDiffMatches",
            trace::id(current_base).as_deref(),
            None,
            None,
        );
        process_simple_path_empty(ctx, cur, frame, current_base, current_base_path)?;
    } else if one_matching_element(frame.slicing.done, current_base_path, &diff_matches) {
        trace::rec(
            "processSimplePath",
            "processSimplePath.oneMatchingElement",
            trace::id(current_base).as_deref(),
            trace::id(&diff_matches[0]).as_deref(),
            None,
        );
        res = Some(process_simple_path_one_match(
            ctx,
            cur,
            frame,
            current_base,
            current_base_path,
            diff_match_idx,
            slicer,
        )?);
    } else if types::diffs_constrain_types(ctx, &diff_matches, current_base_path, type_list) {
        trace::rec(
            "processSimplePath",
            "processSimplePath.diffsConstrainTypes",
            trace::id(current_base).as_deref(),
            trace::id(&diff_matches[0]).as_deref(),
            None,
        );
        types::process_simple_path_where_diffs_constrain_types(
            ctx,
            cur,
            frame,
            current_base_path,
            diff_match_idx,
            type_list,
        )?;
    } else {
        trace::rec(
            "processSimplePath",
            "processSimplePath.default",
            trace::id(current_base).as_deref(),
            trace::id(&diff_matches[0]).as_deref(),
            None,
        );
        super::slicing::process_simple_path_default(
            ctx,
            cur,
            frame,
            current_base,
            current_base_path,
            diff_match_idx,
        )?;
    }
    Ok(res)
}

/// §3.3.1 processSimplePathWithEmptyDiffMatches (PPP:1166).
fn process_simple_path_empty(
    ctx: &mut WalkContext,
    cur: &mut WalkCursor,
    frame: &WalkFrame,
    current_base: &Value,
    current_base_path: &str,
) -> anyhow::Result<()> {
    let mut outcome = clone_element(current_base);
    update_urls(&mut outcome, &frame.url, &frame.spec_url);
    let new_path = fixed_path_dest(
        frame.context_path_target.as_deref(),
        path_of(&outcome),
        &frame.redirector,
        frame.context_path_source.as_deref(),
    );
    set_field(&mut outcome, "path", Value::String(new_path));
    update_from_base(&mut outcome, current_base);
    update_constraint_sources(&mut outcome, &frame.source_sd_url);
    check_extensions(&mut outcome);
    // markExtensions / updateFromObligationProfiles: source stamp is annotation-only.
    if cur.result_path_base.is_none() {
        cur.result_path_base = Some(path_of(&outcome).to_string());
    } else if !path_of(&outcome).starts_with(cur.result_path_base.as_deref().unwrap()) {
        anyhow::bail!("ADDING_WRONG_PATH: {} not under {:?}", path_of(&outcome), cur.result_path_base);
    }
    ctx.add_to_result(outcome.clone(), None);

    if has_inner_diff_matches(&ctx.diff, current_base_path, cur.diff_cursor, frame.diff_limit, true) {
        if base_has_children(&cur.base, cur.base_cursor) {
            trace::rec(
                "processSimplePathWithEmptyDiffMatches",
                "processEmptyDiffMatches.walkIntoBaseChildren",
                trace::id(current_base).as_deref(),
                ctx.diff.get(cur.diff_cursor).and_then(trace::id).as_deref(),
                None,
            );
            let mut new_base_limit = cur.base_cursor + 1;
            let parent_dot = format!("{}.", path_of(&cur.base[cur.base_cursor]));
            while new_base_limit < cur.base.len()
                && path_of(&cur.base[new_base_limit]).starts_with(&parent_dot)
            {
                new_base_limit += 1;
            }
            let mut ncur = WalkCursor {
                base_source_url: cur.base_source_url.clone(),
                base: cur.base.clone(),
                base_cursor: cur.base_cursor + 1,
                diff_cursor: cur.diff_cursor,
                context_name: cur.context_name.clone(),
                result_path_base: cur.result_path_base.clone(),
            };
            let mut nframe = frame.clone();
            nframe.slicing = SlicingParams::default();
            nframe.base_limit = new_base_limit - 1;
            super::loop_::process_paths(ctx, &mut ncur, &nframe, None)?;
            cur.base_cursor = new_base_limit - 1;
            cur.diff_cursor = ncur.diff_cursor;
        } else {
            // walk into a new type / contentReference
            let types_len = outcome.get("type").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
            if types_len == 0 && !has_content_reference(&outcome) {
                anyhow::bail!("_HAS_NO_CHILDREN__AND_NO_TYPES at {current_base_path}");
            }
            if !path_starts_with(
                path_of(&ctx.diff[cur.diff_cursor]),
                &format!("{current_base_path}."),
            ) {
                cur.diff_cursor += 1;
            }
            let start = cur.diff_cursor;
            let dot = format!("{current_base_path}.");
            while cur.diff_cursor < ctx.diff.len()
                && path_starts_with(path_of(&ctx.diff[cur.diff_cursor]), &dot)
            {
                cur.diff_cursor += 1;
            }
            if has_content_reference(&outcome) {
                contentref::walk_into_content_reference(
                    ctx, cur, frame, &mut outcome, current_base_path, start, false,
                )?;
            } else {
                unfold_type_empty(ctx, cur, frame, &outcome, current_base_path, start)?;
            }
        }
    }
    cur.base_cursor += 1;
    Ok(())
}

/// Data-type unfold from the empty-diff branch (PPP:1214-1244 equivalent).
fn unfold_type_empty(
    ctx: &mut WalkContext,
    cur: &mut WalkCursor,
    frame: &WalkFrame,
    outcome: &Value,
    current_base_path: &str,
    start: usize,
) -> anyhow::Result<()> {
    let (dt, dt_url) = resolve_type_sd(ctx, outcome)?;
    cur.context_name = dt_url.clone();
    let dt_elements = snapshot_elements(&dt);
    trace::rec(
        "processSimplePathWithEmptyDiffMatches",
        "processEmptyDiffMatches.unfoldType",
        trace::id(&cur.base[cur.base_cursor]).as_deref(),
        ctx.diff.get(cur.diff_cursor).and_then(trace::id).as_deref(),
        Some(serde_json::json!({ "typeSD": dt_url })),
    );
    let mut ncur = WalkCursor {
        base_source_url: dt_url.clone(),
        base: Rc::new(dt_elements.clone()),
        base_cursor: 1,
        diff_cursor: start,
        context_name: cur.context_name.clone(),
        result_path_base: cur.result_path_base.clone(),
    };
    let mut nframe = frame.clone();
    nframe.base_limit = dt_elements.len().saturating_sub(1);
    nframe.diff_limit = cur.diff_cursor as isize - 1;
    nframe.context_path_source = Some(current_base_path.to_string());
    nframe.context_path_target = Some(path_of(outcome).to_string());
    nframe.redirector = Vec::new();
    nframe.slicing = SlicingParams::default();
    super::loop_::process_paths(ctx, &mut ncur, &nframe, None)?;
    Ok(())
}

/// §3.3.2 processSimplePathWithOneMatchingElementInDifferential (PPP:750).
#[allow(clippy::too_many_arguments)]
pub(crate) fn process_simple_path_one_match(
    ctx: &mut WalkContext,
    cur: &mut WalkCursor,
    frame: &WalkFrame,
    current_base: &Value,
    current_base_path: &str,
    diff_match_idx: &[usize],
    slicer: Option<&Value>,
) -> anyhow::Result<usize> {
    let from_slicer = slicer.is_some();
    let diff0_idx = diff_match_idx[0];
    let diff0 = ctx.diff[diff0_idx].clone();

    // Template selection.
    let mut template: Option<Value> = None;
    // Reslice (lid contains '/').
    let id = diff0.get("id").and_then(Value::as_str).unwrap_or("");
    let lid = tail(id);
    if lid.contains('/') {
        trace::rec(
            "processSimplePathWithOneMatchingElementInDifferential",
            "processOneMatch.templateFromReslice",
            trace::id(current_base).as_deref(),
            trace::id(&diff0).as_deref(),
            None,
        );
        super::ids::generate_ids(&mut ctx.output);
        let base_id = format!(
            "{}{}",
            &id[..id.len() - lid.len()],
            &lid[..lid.find('/').unwrap()]
        );
        template = ctx
            .output
            .iter()
            .find(|e| e.get("id").and_then(Value::as_str) == Some(base_id.as_str()))
            .cloned();
    } else if let Some(profile_template) =
        try_profile_template(ctx, frame, current_base, &diff0)?
    {
        template = Some(profile_template);
    }

    let (mut outcome, _template_sd) = match template {
        None => {
            trace::rec(
                "processSimplePathWithOneMatchingElementInDifferential",
                "processOneMatch.templateFromBase",
                trace::id(current_base).as_deref(),
                trace::id(&diff0).as_deref(),
                Some(serde_json::json!({ "cloneSource": cur.base_source_url })),
            );
            (clone_element(current_base), cur.base_source_url.clone())
        }
        Some(t) => {
            // fillOutFromBase(template, currentBase): fill-missing-only from base.
            let filled = fill_out_from_base(&t, current_base);
            (filled, cur.base_source_url.clone())
        }
    };

    update_urls(&mut outcome, &frame.url, &frame.spec_url);
    let new_path = fixed_path_dest(
        frame.context_path_target.as_deref(),
        path_of(&outcome),
        &frame.redirector,
        frame.context_path_source.as_deref(),
    );
    set_field(&mut outcome, "path", Value::String(new_path));
    update_from_base(&mut outcome, current_base);

    if has_slice_name(&diff0) {
        trace::rec(
            "processSimplePathWithOneMatchingElementInDifferential",
            "processOneMatch.applySliceName",
            trace::id(current_base).as_deref(),
            trace::id(&diff0).as_deref(),
            Some(serde_json::json!({ "sliceName": diff0.get("sliceName") })),
        );
        // checkToSeeIfSlicingExists may synthesize an anchor into result.
        super::slicing::check_to_see_if_slicing_exists(ctx, &diff0);
        if let Some(sn) = diff0.get("sliceName") {
            set_field(&mut outcome, "sliceName", sn.clone());
        }
        if diff0.get("min").is_none() {
            let closed_parent = frame
                .slicing
                .element_definition
                .as_deref()
                .and_then(|ed| ed.get("slicing"))
                .and_then(|s| s.get("rules"))
                .and_then(Value::as_str)
                == Some("closed");
            if !closed_parent && !has_slice_name(current_base) {
                if !current_base_path.ends_with("xtension.value[x]") {
                    set_field(&mut outcome, "min", Value::from(0u64));
                }
            } else if closed_parent && frame.slicing.slices.len() > 1 {
                set_field(&mut outcome, "min", Value::from(0u64));
            }
        }
    }

    update_from_definition(
        ctx,
        &mut outcome,
        &diff0,
        &frame.profile_name,
        frame.trim_differential,
        &frame.source_sd_url,
        from_slicer,
    );

    // PPP:911 slicer max clamp (LIVE: APPLY_PROPERTIES_FROM_SLICER=false makes
    // the !APPLY guard true): if outcome.max > slicer.max, take the slicer's max.
    if let Some(slicer) = slicer {
        let max_as_int = |ed: &Value| -> i64 {
            match ed.get("max").and_then(Value::as_str) {
                Some("*") => i64::MAX,
                Some(n) => n.parse::<i64>().unwrap_or(0),
                None => 0,
            }
        };
        if max_as_int(&outcome) > max_as_int(slicer) {
            if let Some(m) = slicer.get("max") {
                set_field(&mut outcome, "max", m.clone());
            }
        }
    }
    // setSlicing(null): merge case never carries slicing.
    if let Some(obj) = outcome.as_object_mut() {
        obj.remove("slicing");
    }

    if cur.result_path_base.is_none() {
        cur.result_path_base = Some(path_of(&outcome).to_string());
    } else if !path_of(&outcome).starts_with(cur.result_path_base.as_deref().unwrap()) {
        anyhow::bail!("ADDING_WRONG_PATH: {}", path_of(&outcome));
    }
    ctx.add_to_result(outcome.clone(), Some(diff0_idx));
    let out_idx = ctx.output.len() - 1;

    cur.base_cursor += 1;
    cur.diff_cursor = diff0_idx + 1;

    // Descend into children / type / contentReference.
    let out_path = path_of(&outcome).to_string();
    let walks_in = frame.diff_limit >= cur.diff_cursor as isize
        && out_path.contains('.')
        && (is_data_type(ctx, &outcome) || is_base_resource(&outcome) || has_content_reference(&outcome));
    if walks_in
        && cur.diff_cursor < ctx.diff.len()
        && path_starts_with(path_of(&ctx.diff[cur.diff_cursor]), &format!("{}.", path_of(&diff0)))
        && !base_walks_into(&cur.base, cur.base_cursor)
    {
        trace::rec(
            "processSimplePathWithOneMatchingElementInDifferential",
            "processOneMatch.walkIntoChildren",
            trace::id(current_base).as_deref(),
            trace::id(&diff0).as_deref(),
            Some(serde_json::json!({
                "hasContentReference": has_content_reference(&outcome),
                "typeCount": outcome.get("type").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0),
            })),
        );
        // PPP:929-943 — the diff walks into a polymorphic `[x]` via a concrete
        // choice path (e.g. base `component.value[x]`, diff `component.valueQuantity`):
        // narrow the outcome type list to that single concrete type before
        // unfolding, so we recurse into Quantity (not the Element fallback).
        let type_count = outcome.get("type").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
        if type_count > 1 {
            let out_tail = tail(path_of(&outcome)).to_string();
            let diff_tail = tail(path_of(&diff0)).to_string();
            if out_tail.ends_with("[x]") && !diff_tail.ends_with("[x]") && diff_tail.len() >= out_tail.len() - 3 {
                // t = diff_tail.substring(out_tail.len() - 3)
                let mut t = diff_tail[(out_tail.len() - 3)..].to_string();
                if super::types_pred::is_primitive_str(ctx, &uncapitalize_local(&t)) {
                    t = uncapitalize_local(&t);
                }
                // getByTypeName: keep matching type entries; else synthesize.
                let ntr: Vec<Value> = outcome
                    .get("type")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter(|tr| working_code(tr).as_deref() == Some(t.as_str()))
                            .cloned()
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let ntr = if ntr.is_empty() {
                    vec![serde_json::json!({ "code": t })]
                } else {
                    ntr
                };
                set_field(&mut outcome, "type", Value::Array(ntr));
                ctx.output[out_idx] = outcome.clone();
            }
        }
        let start = cur.diff_cursor;
        let dot = format!("{}.", path_of(&diff0));
        while (cur.diff_cursor as isize) <= frame.diff_limit
            && cur.diff_cursor < ctx.diff.len()
            && path_starts_with(path_of(&ctx.diff[cur.diff_cursor]), &dot)
        {
            cur.diff_cursor += 1;
        }
        if has_content_reference(&outcome) {
            contentref::walk_into_content_reference_onematch(
                ctx, cur, frame, &mut outcome, current_base_path, &diff0, start,
            )?;
        } else {
            unfold_type_one_match(ctx, cur, frame, &outcome, &diff0, start)?;
        }
    }

    Ok(out_idx)
}

fn unfold_type_one_match(
    ctx: &mut WalkContext,
    cur: &mut WalkCursor,
    frame: &WalkFrame,
    outcome: &Value,
    diff0: &Value,
    start: usize,
) -> anyhow::Result<()> {
    let (dt, dt_url) = resolve_type_sd(ctx, outcome)?;
    cur.context_name = dt_url.clone();
    let dt_elements = snapshot_elements(&dt);
    trace::rec(
        "processSimplePathWithOneMatchingElementInDifferential",
        "processOneMatch.unfoldType",
        trace::id(&cur.base[cur.base_cursor.saturating_sub(1)]).as_deref(),
        trace::id(diff0).as_deref(),
        Some(serde_json::json!({ "typeSD": dt_url })),
    );
    let mut ncur = WalkCursor {
        base_source_url: dt_url.clone(),
        base: Rc::new(dt_elements.clone()),
        base_cursor: 1,
        diff_cursor: start,
        context_name: cur.context_name.clone(),
        result_path_base: cur.result_path_base.clone(),
    };
    let mut nframe = frame.clone();
    nframe.base_limit = dt_elements.len().saturating_sub(1);
    nframe.diff_limit = cur.diff_cursor as isize - 1;
    nframe.profile_name = format!("{}{}", frame.profile_name, path_tail(diff0));
    nframe.context_path_source = Some(path_of(diff0).to_string());
    nframe.context_path_target = Some(path_of(outcome).to_string());
    nframe.redirector = Vec::new();
    nframe.slicing = SlicingParams::default();
    super::loop_::process_paths(ctx, &mut ncur, &nframe, None)?;
    Ok(())
}

/// Profile-on-type-root template (PPP:772-857) — resolve the diff's single
/// non-Reference profile and, for Extension/Resource base types, clone the
/// profile snapshot root as the template.
fn try_profile_template(
    ctx: &mut WalkContext,
    _frame: &WalkFrame,
    current_base: &Value,
    diff0: &Value,
) -> anyhow::Result<Option<Value>> {
    let types = diff0.get("type").and_then(Value::as_array);
    let Some(types) = types else { return Ok(None) };
    if types.len() != 1 {
        return Ok(None);
    }
    let tr = &types[0];
    let profile = tr
        .get("profile")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str);
    let Some(profile_url) = profile else {
        return Ok(None);
    };
    let working = working_code(tr).unwrap_or_default();
    if working == "Reference" {
        return Ok(None);
    }
    // If base already has this profile, skip.
    let base_profile = current_base
        .get("type")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|t| t.get("profile"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str);
    if base_profile == Some(profile_url) {
        return Ok(None);
    }
    // PPP:778-811: resolve the profile SD FIRST — generating its snapshot on
    // demand if absent (a visible nested generateSnapshot in the trace) — and
    // only THEN gate template use on the base type summary (PPP:843).
    let Some(sd) = resolve_with_snapshot(ctx, profile_url)? else {
        return Ok(None);
    };
    let base_type_summary = type_codes(current_base)
        .first()
        .cloned()
        .unwrap_or_default();
    if base_type_summary != "Extension" && base_type_summary != "Resource" {
        return Ok(None);
    }
    let kind = sd.get("kind").and_then(Value::as_str).unwrap_or("");
    let root = snapshot_elements(&sd)
        .first()
        .cloned()
        .unwrap_or_else(empty_object);
    let mut template = root;
    // PPP:836-840: for a resource-kind profile root, the constraints can't be
    // migrated (the sense of %resource changes) — clear them.
    if !path_of(&template).contains('.') && kind == "resource" {
        if let Some(obj) = template.as_object_mut() {
            obj.remove("constraint");
        }
    }
    set_field(&mut template, "path", Value::String(path_of(current_base).to_string()));
    if let Some(obj) = template.as_object_mut() {
        obj.remove("sliceName");
    }
    if working != "Extension" {
        if let Some(m) = current_base.get("min") {
            set_field(&mut template, "min", m.clone());
        }
        if let Some(m) = current_base.get("max") {
            set_field(&mut template, "max", m.clone());
        }
    }
    trace::rec(
        "processSimplePathWithOneMatchingElementInDifferential",
        "processOneMatch.templateFromProfile",
        trace::id(current_base).as_deref(),
        trace::id(diff0).as_deref(),
        Some(serde_json::json!({ "profileSD": sd.get("url"), "srcElement": trace::id(&template) })),
    );
    Ok(Some(template))
}

// ---- shared helpers ----

pub(crate) fn tail(path: &str) -> &str {
    match path.rfind('.') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// Utilities.uncapitalize: lowercase the first char.
fn uncapitalize_local(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// PU:1984 pathTail.
pub(crate) fn path_tail(d: &Value) -> String {
    let p = path_of(d);
    let s = match p.rfind('.') {
        Some(i) => &p[i + 1..],
        None => p,
    };
    let profile = d
        .get("type")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|t| t.get("profile"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str);
    match profile {
        Some(pr) => format!(".{s}[{pr}]"),
        None => format!(".{s}"),
    }
}

/// PU:1897 baseWalksInto.
pub(crate) fn base_walks_into(base: &[Value], cursor: usize) -> bool {
    if cursor >= base.len() {
        return false;
    }
    let path = path_of(&base[cursor]);
    let prev = path_of(&base[cursor - 1]);
    path.starts_with(&format!("{prev}."))
}

/// PU:1906 fillOutFromBase — fill-missing-only from `usage` into a copy of
/// `profile`, restricted to Java's exact field allow-list (notably NOT
/// `condition`, `type`, `base`, `slicing`, `mapping`, `id`, `path`).
fn fill_out_from_base(profile: &Value, usage: &Value) -> Value {
    let mut out = profile.clone();
    let Some(out_obj) = out.as_object_mut() else { return out };
    let Some(usage_obj) = usage.as_object() else { return out };

    // scalar fill-if-missing
    for k in [
        "sliceName", "label", "definition", "short", "comment", "requirements",
        "min", "max", "maxLength", "mustSupport", "isSummary", "isModifier",
        "isModifierReason", "mustHaveValue", "binding",
    ] {
        if !out_obj.contains_key(k) {
            if let Some(v) = usage_obj.get(k) {
                out_obj.insert(k.to_string(), v.clone());
            }
        }
    }
    // polymorphic fill-if-missing: fixed[x]/pattern[x]/minValue[x]/maxValue[x]
    for prefix in ["fixed", "pattern", "minValue", "maxValue"] {
        let has = out_obj.keys().any(|k| is_choice_key(k, prefix));
        if !has {
            if let Some((k, v)) = usage_obj.iter().find(|(k, _)| is_choice_key(k, prefix)) {
                out_obj.insert(k.clone(), v.clone());
            }
        }
    }
    // example[]: fill only if absent (Java setExample when !res.hasExample()).
    if !out_obj.contains_key("example") {
        if let Some(v) = usage_obj.get("example") {
            out_obj.insert("example".to_string(), v.clone());
        }
    }
    // code[]: additive by value
    additive_array(out_obj, usage_obj, "code", |a, b| a == b);
    // alias[]: additive by value
    additive_array(out_obj, usage_obj, "alias", |a, b| a == b);
    // constraint[]: additive by key
    additive_array(out_obj, usage_obj, "constraint", |a, b| {
        a.get("key") == b.get("key")
    });
    // extension[]: additive by url
    additive_array(out_obj, usage_obj, "extension", |a, b| {
        a.get("url") == b.get("url")
    });
    out
}

/// True if `key` is a `prefix` + capitalized-type polymorphic field (e.g.
/// prefix "fixed" matches "fixedString" but not "fixed" or "fixedxyz").
fn is_choice_key(key: &str, prefix: &str) -> bool {
    key.strip_prefix(prefix)
        .and_then(|rest| rest.chars().next())
        .map(|c| c.is_ascii_uppercase())
        .unwrap_or(false)
}

/// Additive array merge into `out[key]`: append each `usage[key]` entry not
/// already present per `same`.
fn additive_array(
    out: &mut serde_json::Map<String, Value>,
    usage: &serde_json::Map<String, Value>,
    key: &str,
    same: impl Fn(&Value, &Value) -> bool,
) {
    let Some(src) = usage.get(key).and_then(Value::as_array) else { return };
    if src.is_empty() {
        return;
    }
    let mut existing = out.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
    for s in src {
        if !existing.iter().any(|d| same(d, s)) {
            existing.push(s.clone());
        }
    }
    out.insert(key.to_string(), Value::Array(existing));
}

pub(crate) fn snapshot_elements(sd: &Value) -> Vec<Value> {
    sd.get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Resolve the datatype SD for `outcome`'s type(s) (PPP:998-1021 logic).
pub(crate) fn resolve_type_sd(
    ctx: &mut WalkContext,
    outcome: &Value,
) -> anyhow::Result<(Rc<Value>, String)> {
    let types = outcome.get("type").and_then(Value::as_array).cloned().unwrap_or_default();
    let codes: Vec<String> = types.iter().filter_map(working_code).collect();
    let distinct: std::collections::BTreeSet<&String> = codes.iter().collect();
    let (query, profile): (String, Option<String>) = if types.len() > 1 {
        if distinct.len() == 1 {
            (codes[0].clone(), first_profile(&types[0]))
        } else {
            ("Element".to_string(), None)
        }
    } else if let Some(t) = types.first() {
        (working_code(t).unwrap_or_default(), first_profile(t))
    } else {
        anyhow::bail!("no type to unfold");
    };
    // Prefer the profile if a single one and it resolves with a snapshot.
    if let Some(profile_url) = profile {
        if types.len() == 1
            && types[0]
                .get("profile")
                .and_then(Value::as_array)
                .map(|a| a.len() == 1)
                .unwrap_or(false)
        {
            if let Some(sd) = resolve_with_snapshot(ctx, &profile_url)? {
                let url = sd.get("url").and_then(Value::as_str).unwrap_or(&profile_url).to_string();
                return Ok((sd, url));
            }
        }
    }
    let Some(sd) = resolve_with_snapshot(ctx, &query)? else {
        anyhow::bail!("_HAS_CHILDREN__FOR_TYPE__BUT_CANT_FIND_TYPE: {query}");
    };
    let url = sd.get("url").and_then(Value::as_str).unwrap_or(&query).to_string();
    Ok((sd, url))
}

fn first_profile(tr: &Value) -> Option<String> {
    tr.get("profile")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .map(str::to_string)
}

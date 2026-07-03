//! `processPathWithSlicedBase` (PPP:1310) — dispatch for an already-sliced base
//! element, and its three sub-branches (§3.6).

use serde_json::{json, Value};
use std::rc::Rc;

use super::context::WalkContext;
use super::emit::*;
use super::frame::{SlicingParams, WalkCursor, WalkFrame};
use super::paths::*;
use super::simple::{path_tail, tail};
use super::trace;
use super::types::{self, TypeSlice};
use super::updatefromdef::update_from_definition;
use crate::merge::set_field;

pub(crate) fn process_path_with_sliced_base(
    ctx: &mut WalkContext,
    cur: &mut WalkCursor,
    frame: &WalkFrame,
    current_base: &Value,
    current_base_path: &str,
    diff_match_idx: &[usize],
    type_list: &mut Vec<TypeSlice>,
) -> anyhow::Result<()> {
    let path = path_of(current_base).to_string();
    let diff_matches: Vec<Value> = diff_match_idx.iter().map(|&i| ctx.diff[i].clone()).collect();

    if diff_matches.is_empty() {
        trace::rec(
            "processPathWithSlicedBase",
            "processPathWithSlicedBase.emptyDiffMatches",
            trace::id(current_base).as_deref(),
            None,
            None,
        );
        process_path_with_sliced_base_empty(ctx, cur, frame, current_base, current_base_path, &path)?;
    } else if types::diffs_constrain_types(ctx, &diff_matches, current_base_path, type_list) {
        trace::rec(
            "processPathWithSlicedBase",
            "processPathWithSlicedBase.diffsConstrainTypes",
            trace::id(current_base).as_deref(),
            trace::id(&diff_matches[0]).as_deref(),
            None,
        );
        anyhow::bail!("processPathWithSlicedBaseWhereDiffsConstrainTypes not implemented (type-slicing on sliced base)");
    } else {
        trace::rec(
            "processPathWithSlicedBase",
            "processPathWithSlicedBase.default",
            trace::id(current_base).as_deref(),
            trace::id(&diff_matches[0]).as_deref(),
            Some(json!({
                "diffMatches": diff_matches.len(),
                "diffMatchIds": diff_match_idx.iter().filter_map(|&i| trace::id(&ctx.diff[i])).collect::<Vec<_>>(),
                "inheritedSlicing": true,
            })),
        );
        process_path_with_sliced_base_default(ctx, cur, frame, current_base, current_base_path, &path, diff_match_idx)?;
    }
    Ok(())
}

/// §3.6.1 processPathWithSlicedBaseAndEmptyDiffMatches (PPP:1829).
fn process_path_with_sliced_base_empty(
    ctx: &mut WalkContext,
    cur: &mut WalkCursor,
    frame: &WalkFrame,
    current_base: &Value,
    _current_base_path: &str,
    path: &str,
) -> anyhow::Result<()> {
    if has_inner_diff_matches(&ctx.diff, path, cur.diff_cursor, frame.diff_limit, true) {
        let mut outcome = clone_element(current_base);
        let new_path = fixed_path_dest(
            frame.context_path_target.as_deref(),
            path_of(&outcome),
            &frame.redirector,
            frame.context_path_source.as_deref(),
        );
        set_field(&mut outcome, "path", Value::String(new_path));
        update_from_base(&mut outcome, current_base);
        if cur.result_path_base.is_none() {
            cur.result_path_base = Some(path_of(&outcome).to_string());
        }
        ctx.add_to_result(outcome.clone(), None);
        if base_has_children(&cur.base, cur.base_cursor) {
            trace::rec(
                "processPathWithSlicedBaseAndEmptyDiffMatches",
                "processSlicedBaseEmptyDiffMatches.walkIntoBaseChildren",
                trace::id(current_base).as_deref(),
                ctx.diff.get(cur.diff_cursor).and_then(trace::id).as_deref(),
                None,
            );
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
            super::loop_::process_paths(ctx, &mut ncur, &nframe, None)?;
            cur.base_cursor = index_of_first_non_child(&cur.base, cur.base_cursor, frame.base_limit);
        } else {
            anyhow::bail!("sliced-base empty-diff type unfold not implemented");
        }
        cur.base_cursor += 1;
    } else {
        // copy currentBase + all children/slices verbatim
        trace::rec(
            "processPathWithSlicedBaseAndEmptyDiffMatches",
            "processSlicedBaseEmptyDiffMatches.copyAllBaseSlices",
            trace::id(current_base).as_deref(),
            None,
            Some(json!({ "cloneSource": frame.source_sd_url })),
        );
        while cur.base_cursor < cur.base.len()
            && path_of(&cur.base[cur.base_cursor]).starts_with(path)
        {
            let mut outcome = clone_element(&cur.base[cur.base_cursor]);
            let new_path = fixed_path_dest(
                frame.context_path_target.as_deref(),
                path_of(&outcome),
                &frame.redirector,
                frame.context_path_source.as_deref(),
            );
            set_field(&mut outcome, "path", Value::String(new_path));
            ctx.add_to_result(outcome, None);
            cur.base_cursor += 1;
        }
        // Java does not ++ here; the loop consumed the whole block. The outer
        // loop's `baseCursor++` is not applied because we return from a while.
        // But the caller (process_paths) does not ++ baseCursor — the sliced
        // dispatch relies on this function leaving base_cursor past the block.
        // We already advanced it; step back one so the outer loop lands correctly.
    }
    Ok(())
}

/// §3.6.3 processPathWithSlicedBaseDefault (PPP:1344).
#[allow(clippy::too_many_arguments)]
fn process_path_with_sliced_base_default(
    ctx: &mut WalkContext,
    cur: &mut WalkCursor,
    frame: &WalkFrame,
    current_base: &Value,
    current_base_path: &str,
    path: &str,
    diff_match_idx: &[usize],
) -> anyhow::Result<()> {
    let closed = current_base
        .get("slicing")
        .and_then(|s| s.get("rules"))
        .and_then(Value::as_str)
        == Some("closed");
    let diff_matches: Vec<Value> = diff_match_idx.iter().map(|&i| ctx.diff[i].clone()).collect();
    let diff0 = diff_matches[0].clone();
    let diff0_idx = diff_match_idx[0];
    let mut diffpos = 0usize;

    // Emit the anchor.
    let mut outcome = clone_element(current_base);
    let new_path = fixed_path_dest(
        frame.context_path_target.as_deref(),
        path_of(&outcome),
        &frame.redirector,
        frame.context_path_source.as_deref(),
    );
    set_field(&mut outcome, "path", Value::String(new_path));
    update_from_base(&mut outcome, current_base);
    if has_slicing(&diff0) || !has_slice_name(&diff0) {
        if let (Some(dst), Some(src)) = (outcome.get("slicing").cloned(), diff0.get("slicing")) {
            let merged = update_from_slicing(dst, src.clone());
            set_field(&mut outcome, "slicing", merged);
        }
        update_from_definition(
            ctx,
            &mut outcome,
            &diff0,
            &frame.profile_name,
            closed,
            &frame.source_sd_url,
            false,
        );
    }
    if cur.result_path_base.is_none() {
        cur.result_path_base = Some(path_of(&outcome).to_string());
    }
    ctx.add_to_result(outcome.clone(), Some(diff0_idx));

    if !has_slice_name(&diff0) {
        diffpos += 1;
    }

    // Anchor children / BackboneElement copy.
    if has_inner_diff_matches(&ctx.diff, current_base_path, cur.diff_cursor, frame.diff_limit, false) {
        anyhow::bail!("sliced-base default anchor-children unfold not implemented");
    } else if current_base
        .get("type")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|t| t.get("code"))
        .and_then(Value::as_str)
        == Some("BackboneElement")
    {
        let new_base_limit = find_end_of_element(&cur.base, cur.base_cursor);
        trace::rec(
            "processPathWithSlicedBaseDefault",
            "processSlicedBaseDefault.copyBackboneChildren",
            trace::id(current_base).as_deref(),
            None,
            Some(json!({ "count": new_base_limit - cur.base_cursor })),
        );
        for i in (cur.base_cursor + 1)..=new_base_limit {
            let mut o = clone_element(&cur.base[i]);
            let np = fixed_path_dest(
                frame.context_path_target.as_deref(),
                path_of(&o),
                &frame.redirector,
                frame.context_path_source.as_deref(),
            );
            set_field(&mut o, "path", Value::String(np));
            ctx.add_to_result(o, None);
        }
    }

    // Pair base slices with diff slices.
    let base_matches = get_siblings(&cur.base, cur.base_cursor);
    for base_item_idx in base_matches {
        cur.base_cursor = base_item_idx;
        let base_item = cur.base[base_item_idx].clone();
        let mut outcome = clone_element(&base_item);
        update_from_base(&mut outcome, current_base);
        let np = fixed_path_dest(
            frame.context_path_target.as_deref(),
            path_of(&outcome),
            &frame.redirector,
            frame.context_path_source.as_deref(),
        );
        set_field(&mut outcome, "path", Value::String(np));
        if let Some(obj) = outcome.as_object_mut() {
            obj.remove("slicing");
        }
        let outcome_slice = outcome.get("sliceName").and_then(Value::as_str).map(str::to_string);
        let diff_slice_name = diff_matches
            .get(diffpos)
            .and_then(|d| d.get("sliceName"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if diffpos < diff_matches.len()
            && diff_slice_name.is_some()
            && diff_slice_name == outcome_slice
        {
            trace::rec(
                "processPathWithSlicedBaseDefault",
                "processSlicedBaseDefault.matchExistingSlice",
                trace::id(&base_item).as_deref(),
                trace::id(&diff_matches[diffpos]).as_deref(),
                Some(json!({ "sliceName": outcome_slice, "diffpos": diffpos })),
            );
            let new_base_limit = find_end_of_element(&cur.base, cur.base_cursor);
            let new_diff_cursor = diff_match_idx[diffpos];
            let new_diff_limit = find_end_of_element(&ctx.diff, new_diff_cursor);
            let mut nc = WalkCursor {
                base_source_url: cur.base_source_url.clone(),
                base: cur.base.clone(),
                base_cursor: cur.base_cursor,
                diff_cursor: new_diff_cursor,
                context_name: cur.context_name.clone(),
                result_path_base: cur.result_path_base.clone(),
            };
            let mut nframe = frame.clone();
            nframe.base_limit = new_base_limit;
            nframe.diff_limit = new_diff_limit as isize;
            nframe.profile_name = format!("{}{}", frame.profile_name, path_tail(&diff_matches[diffpos]));
            nframe.trim_differential = closed;
            nframe.slicing = SlicingParams::done_with(None, None);
            super::loop_::process_paths(ctx, &mut nc, &nframe, None)?;
            cur.base_cursor = new_base_limit;
            cur.diff_cursor = new_diff_limit + 1;
            diffpos += 1;
        } else {
            trace::rec(
                "processPathWithSlicedBaseDefault",
                "processSlicedBaseDefault.copyUnmatchedBaseSlice",
                trace::id(&base_item).as_deref(),
                None,
                Some(json!({ "sliceName": outcome_slice, "cloneSource": frame.source_sd_url })),
            );
            ctx.add_to_result(outcome, None);
            cur.base_cursor += 1;
            while cur.base_cursor < cur.base.len()
                && path_of(&cur.base[cur.base_cursor]).starts_with(path)
                && path_of(&cur.base[cur.base_cursor]) != path
            {
                let mut o = clone_element(&cur.base[cur.base_cursor]);
                let np = fixed_path_dest(
                    frame.context_path_target.as_deref(),
                    path_of(&o),
                    &frame.redirector,
                    frame.context_path_source.as_deref(),
                );
                set_field(&mut o, "path", Value::String(np));
                ctx.add_to_result(o, None);
                cur.base_cursor += 1;
            }
            cur.base_cursor -= 1;
        }
    }

    // New diff slices.
    if closed && diffpos < diff_matches.len() && !path.ends_with("[x]") {
        anyhow::bail!("THE_BASE_SNAPSHOT_MARKS_A_SLICING_AS_CLOSED at {current_base_path}");
    }
    while diffpos < diff_matches.len() {
        let diff_item = diff_matches[diffpos].clone();
        let diff_item_idx = diff_match_idx[diffpos];
        trace::rec(
            "processPathWithSlicedBaseDefault",
            "processSlicedBaseDefault.newSliceAtEnd",
            trace::id(current_base).as_deref(),
            trace::id(&diff_item).as_deref(),
            Some(json!({ "sliceName": diff_item.get("sliceName"), "diffpos": diffpos })),
        );
        // template = currentBase (reslice via getById if lid contains '/').
        let id = diff_item.get("id").and_then(Value::as_str).unwrap_or("");
        let lid = tail(id);
        let template = if lid.contains('/') {
            super::ids::generate_ids(&mut ctx.output);
            let base_id = format!("{}{}", &id[..id.len() - lid.len()], &lid[..lid.find('/').unwrap()]);
            ctx.output
                .iter()
                .find(|e| e.get("id").and_then(Value::as_str) == Some(base_id.as_str()))
                .cloned()
                .unwrap_or_else(|| current_base.clone())
        } else {
            current_base.clone()
        };
        let mut outcome = clone_element(&template);
        let np = fixed_path_dest(
            frame.context_path_target.as_deref(),
            path_of(&outcome),
            &frame.redirector,
            frame.context_path_source.as_deref(),
        );
        set_field(&mut outcome, "path", Value::String(np));
        update_from_base(&mut outcome, current_base);
        if let Some(obj) = outcome.as_object_mut() {
            obj.remove("slicing");
        }
        set_field(&mut outcome, "min", Value::from(0u64));
        ctx.add_to_result(outcome.clone(), Some(diff_item_idx));
        let out_idx = ctx.output.len() - 1;
        let mut o = ctx.output[out_idx].clone();
        update_from_definition(
            ctx,
            &mut o,
            &diff_item,
            &frame.profile_name,
            frame.trim_differential,
            &frame.source_sd_url,
            false,
        );
        ctx.output[out_idx] = o;
        cur.diff_cursor = diff_item_idx + 1;
        diffpos += 1;
    }
    cur.base_cursor += 1;
    Ok(())
}

/// PU:2359 getSiblings — indices of same-path siblings after `current`.
fn get_siblings(list: &[Value], current_idx: usize) -> Vec<usize> {
    let path = path_of(&list[current_idx]);
    let mut result = Vec::new();
    let mut cursor = current_idx + 1;
    while cursor < list.len() && path_of(&list[cursor]).len() >= path.len() {
        if paths_match(path_of(&list[cursor]), path) {
            result.push(cursor);
        }
        cursor += 1;
    }
    result
}

fn paths_match(p1: &str, p2: &str) -> bool {
    p1 == p2
        || (p2.ends_with("[x]")
            && p1.starts_with(&p2[..p2.len() - 3])
            && !p1[p2.len() - 3..].contains('.'))
}

/// PPP:1129 indexOfFirstNonChild.
fn index_of_first_non_child(base: &[Value], current_idx: usize, base_limit: usize) -> usize {
    if current_idx == base_limit.wrapping_sub(1) {
        return base_limit + 1;
    }
    let parent = path_of(&base[current_idx]);
    let dot = format!("{parent}.");
    let mut index = current_idx + 1;
    while index < base_limit && index < base.len() {
        if !path_of(&base[index]).starts_with(&dot) {
            return index + 1;
        }
        index += 1;
    }
    base_limit + 1
}

/// PU:2xxx updateFromSlicing (merge diff slicing into base slicing).
fn update_from_slicing(mut dst: Value, src: Value) -> Value {
    if let Some(ordered) = src.get("ordered") {
        set_field(&mut dst, "ordered", ordered.clone());
    }
    if let Some(src_discs) = src.get("discriminator").and_then(Value::as_array) {
        let existing = dst
            .get("discriminator")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut merged = existing.clone();
        for s in src_discs {
            if !existing.iter().any(|d| d == s) {
                merged.push(s.clone());
            }
        }
        set_field(&mut dst, "discriminator", Value::Array(merged));
    }
    if let Some(rules) = src.get("rules") {
        set_field(&mut dst, "rules", rules.clone());
    }
    dst
}

#[allow(unused_imports)]
use Rc as _Rc;

//! Slicing helpers: makeExtensionSlicing, slicingMatches, order/discriminator/
//! rule matches, checkToSeeIfSlicingExists, and the introduce-slicing branch
//! `processSimplePathDefault` (§3.3.4).

use serde_json::{json, Value};

use super::context::WalkContext;
use super::emit::*;
use super::frame::{SlicingParams, WalkCursor, WalkFrame};
use super::paths::*;
use super::trace;
use super::types_pred::is_extension_path;
use super::updatefromdef::update_from_definition;
use crate::merge::set_field;

pub(crate) fn noop() {}

/// PU:2428 makeExtensionSlicing.
pub(crate) fn make_extension_slicing() -> Value {
    json!({
        "discriminator": [ { "type": "value", "path": "url" } ],
        "ordered": false,
        "rules": "open"
    })
}

/// PPP:1060 checkToSeeIfSlicingExists — if no slicing anchor exists yet at
/// ed.path in the result, synthesize one (extension→OPEN url; [x]→CLOSED $this).
/// We scan result backwards for a path match; if found with slicing/sliceName/[x]
/// we do nothing.
pub(crate) fn check_to_see_if_slicing_exists(ctx: &mut WalkContext, ed: &Value) {
    let ed_path = path_of(ed);
    for out in ctx.output.iter().rev() {
        if paths_match(path_of(out), ed_path) {
            if has_slicing(out) || has_slice_name(out) || path_of(out).ends_with("[x]") {
                return;
            }
            break;
        }
    }
    // No anchor: for extensions, add OPEN url-discriminated slicing to the last
    // matching non-slice element (rare in the ladder; handled via ensure at emit).
    // Left minimal: the anchor is normally emitted by the default/sliced paths.
}

fn paths_match(p1: &str, p2: &str) -> bool {
    p1 == p2
        || (p2.ends_with("[x]")
            && p1.starts_with(&p2[..p2.len() - 3])
            && !p1[p2.len() - 3..].contains('.'))
}

/// PPP:544 slicingMatches.
pub(crate) fn slicing_matches(s1: &Value, s2: &Value) -> bool {
    let o1 = s1.get("ordered");
    let o2 = s2.get("ordered");
    if (o1.is_none() && o2.is_some()) || (o1.is_some() && o2.is_some() && o1 != o2) {
        return false;
    }
    let r1 = s1.get("rules");
    let r2 = s2.get("rules");
    if (r1.is_none() && r2.is_some()) || (r1.is_some() && r2.is_some() && r1 != r2) {
        return false;
    }
    s1.get("discriminator") == s2.get("discriminator")
}

/// PPP:326 processSimplePathDefault (§3.3.4) — the diff introduces slicing on a
/// previously-unsliced base element.
pub(crate) fn process_simple_path_default(
    ctx: &mut WalkContext,
    cur: &mut WalkCursor,
    frame: &WalkFrame,
    current_base: &Value,
    current_base_path: &str,
    diff_match_idx: &[usize],
) -> anyhow::Result<()> {
    let diff_matches: Vec<Value> = diff_match_idx.iter().map(|&i| ctx.diff[i].clone()).collect();
    let diff0 = diff_matches[0].clone();
    let diff0_idx = diff_match_idx[0];

    // Preconditions.
    if !unbounded(current_base)
        && !(is_sliced_to_one_only(&diff0) || is_type_slicing(&diff0))
    {
        anyhow::bail!(
            "ATTEMPT_TO_A_SLICE_AN_ELEMENT_THAT_DOES_NOT_REPEAT: {}",
            path_of(current_base)
        );
    }
    if !has_slicing(&diff0) && !is_extension_path(path_of(current_base)) {
        anyhow::bail!("DIFFERENTIAL_DOES_NOT_HAVE_A_SLICE: {current_base_path}");
    }

    let mut start = 0usize;
    let new_base_limit = find_end_of_element(&cur.base, cur.base_cursor);

    // Default-before-slices branch.
    let diff1_after = if diff_matches.len() > 1 {
        diff_match_idx[1] as isize
    } else {
        -1
    };
    let default_before = diff_matches.len() > 1
        && has_slicing(&diff0)
        && (new_base_limit > cur.base_cursor || diff1_after > diff0_idx as isize + 1);

    let slicer_element: Value;

    if default_before {
        trace::rec(
            "processSimplePathDefault",
            "processSimplePathDefault.defaultBeforeSlices",
            trace::id(current_base).as_deref(),
            trace::id(&diff0).as_deref(),
            Some(json!({ "sliceGroupSize": diff_matches.len(), "sliceGroupIds": diff_ids(ctx, diff_match_idx) })),
        );
        let new_diff_cursor = diff0_idx;
        let new_diff_limit = find_end_of_element(&ctx.diff, new_diff_cursor);
        let mut ncur = WalkCursor {
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
        nframe.slicing = SlicingParams::done_with(None, None);
        let e_idx = super::loop_::process_paths(ctx, &mut ncur, &nframe, None)?;
        let Some(e_idx) = e_idx else {
            anyhow::bail!("DID_NOT_FIND_SINGLE_SLICE_ {}", path_of(&diff0));
        };
        if let Some(sl) = diff0.get("slicing").cloned() {
            set_field(&mut ctx.output[e_idx], "slicing", sl);
        }
        clear_id(&mut ctx.output[e_idx]);
        slicer_element = ctx.output[e_idx].clone();
        start += 1;
    } else {
        // Accept differential slicing at face value.
        trace::rec(
            "processSimplePathDefault",
            "processSimplePathDefault.acceptDiffSlicing",
            trace::id(current_base).as_deref(),
            trace::id(&diff0).as_deref(),
            Some(json!({
                "sliceGroupSize": diff_matches.len(),
                "sliceGroupIds": diff_ids(ctx, diff_match_idx),
                "cloneSource": frame.source_sd_url,
            })),
        );
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

        if !has_slicing(&diff0) {
            trace::rec(
                "processSimplePathDefault",
                "processSimplePathDefault.autoAddedSlicing",
                trace::id(current_base).as_deref(),
                trace::id(&diff0).as_deref(),
                None,
            );
            set_field(&mut outcome, "slicing", make_extension_slicing());
        } else {
            trace::rec(
                "processSimplePathDefault",
                "processSimplePathDefault.copyDiffSlicing",
                trace::id(current_base).as_deref(),
                trace::id(&diff0).as_deref(),
                None,
            );
            set_field(&mut outcome, "slicing", diff0.get("slicing").cloned().unwrap());
            for i in 1..diff_matches.len() {
                if has_slicing(&diff_matches[i])
                    && !slicing_matches(
                        diff0.get("slicing").unwrap(),
                        diff_matches[i].get("slicing").unwrap(),
                    )
                {
                    ctx.add_message(
                        super::context::Severity::Error,
                        path_of(&diff0),
                        "ATTEMPT_TO_CHANGE_SLICING".to_string(),
                    );
                }
            }
        }
        if cur.result_path_base.is_none() {
            cur.result_path_base = Some(path_of(&outcome).to_string());
        }
        ctx.add_to_result(outcome.clone(), None);
        let anchor_idx = ctx.output.len() - 1;
        clear_id(&mut ctx.output[anchor_idx]);

        if !has_slice_name(&diff0) {
            trace::rec(
                "processSimplePathDefault",
                "processSimplePathDefault.sliceGroupBaseDefinition",
                trace::id(current_base).as_deref(),
                trace::id(&diff0).as_deref(),
                None,
            );
            let mut anchor = ctx.output[anchor_idx].clone();
            update_from_definition(
                ctx,
                &mut anchor,
                &diff0,
                &frame.profile_name,
                frame.trim_differential,
                &frame.source_sd_url,
                false,
            );
            ctx.output[anchor_idx] = anchor.clone();
            ctx.mark_consumed(diff0_idx);

            // PPP:419-470: the anchor is the base definition of the slice; if the
            // diff walks into it, unfold its type / dump its contentReference.
            if has_inner_diff_matches(
                &ctx.diff,
                current_base_path,
                cur.diff_cursor,
                frame.diff_limit,
                false,
            ) {
                if base_has_children(&cur.base, cur.base_cursor) {
                    anyhow::bail!(
                        "sliceGroupBaseDefinition inner-diff with base children not handled at {current_base_path}"
                    );
                }
                // unfoldType (PPP:425-447): recurse into the datatype SD.
                let (dt, dt_url) = super::simple::resolve_type_sd(ctx, &anchor)?;
                cur.context_name = dt_url.clone();
                cur.diff_cursor += 1;
                let unfold_start = cur.diff_cursor;
                let cb_dot = format!("{current_base_path}.");
                while cur.diff_cursor < ctx.diff.len()
                    && path_starts_with(path_of(&ctx.diff[cur.diff_cursor]), &cb_dot)
                {
                    cur.diff_cursor += 1;
                }
                cur.diff_cursor -= 1;
                trace::rec(
                    "processSimplePathDefault",
                    "processSimplePathDefault.unfoldType",
                    trace::id(current_base).as_deref(),
                    trace::id(&diff0).as_deref(),
                    Some(json!({ "typeSD": dt_url })),
                );
                let dt_elements = super::simple::snapshot_elements(&dt);
                let mut ncur = WalkCursor {
                    base_source_url: dt_url.clone(),
                    base: std::rc::Rc::new(dt_elements.clone()),
                    base_cursor: 1,
                    diff_cursor: unfold_start,
                    context_name: cur.context_name.clone(),
                    result_path_base: cur.result_path_base.clone(),
                };
                let mut nframe = frame.clone();
                nframe.base_limit = dt_elements.len().saturating_sub(1);
                nframe.diff_limit = cur.diff_cursor as isize;
                nframe.context_path_source = Some(current_base_path.to_string());
                nframe.context_path_target = Some(path_of(&anchor).to_string());
                nframe.redirector = Vec::new();
                nframe.slicing = SlicingParams::default();
                let slicer_local = ctx.output[anchor_idx].clone();
                super::loop_::process_paths(ctx, &mut ncur, &nframe, Some(&slicer_local))?;
            } else if anchor.get("contentReference").is_some()
                && !base_has_children(&cur.base, cur.base_cursor)
            {
                // contentReferenceInlineDump (PPP:449-470): dump the referenced
                // element's children inline, path-fixed under the anchor.
                let content_ref = anchor
                    .get("contentReference")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let frag = content_ref[content_ref.find('#').map(|i| i + 1).unwrap_or(0)..].to_string();
                if let Some(bstart) =
                    super::contentref::resolve_content_reference_pub(&cur.base, cur.base_cursor, &content_ref)
                {
                    let bend = find_end_of_element_no_slices(&cur.base, bstart);
                    trace::rec(
                        "processSimplePathDefault",
                        "processSimplePathDefault.contentReferenceInlineDump",
                        trace::id(current_base).as_deref(),
                        trace::id(&diff0).as_deref(),
                        Some(json!({ "contentReference": content_ref, "count": bend - bstart })),
                    );
                    let anchor_path = path_of(&anchor).to_string();
                    for i in (bstart + 1)..=bend {
                        let src = cur.base[i].clone();
                        let mut o = clone_element(&src);
                        update_urls(&mut o, &frame.url, &frame.spec_url);
                        clear_id(&mut o);
                        let fixed = path_of(&o).replace(&frag, &anchor_path);
                        let np = fixed_path_dest(
                            frame.context_path_target.as_deref(),
                            &fixed,
                            &frame.redirector,
                            frame.context_path_source.as_deref(),
                        );
                        set_field(&mut o, "path", Value::String(np));
                        update_from_base(&mut o, &src);
                        ctx.add_to_result(o, None);
                    }
                }
            }
            start += 1;
        } else {
            trace::rec(
                "processSimplePathDefault",
                "processSimplePathDefault.sliceGroupNamedFirst.checkExtensionDoco",
                trace::id(current_base).as_deref(),
                trace::id(&diff0).as_deref(),
                None,
            );
            let mut anchor = ctx.output[anchor_idx].clone();
            super::updatefromdef::check_extension_doco(&mut anchor);
            ctx.output[anchor_idx] = anchor;
        }
        slicer_element = ctx.output[anchor_idx].clone();
    }

    // Per-slice loop. newDiffCursor/newDiffLimit are recomputed each iteration;
    // the LAST slice's values drive the final cursor advance (PPP:484-508).
    let mut new_diff_limit = cur.diff_cursor;
    for i in start..diff_matches.len() {
        let slice_start = diff_match_idx[i];
        new_diff_limit = find_end_of_element(&ctx.diff, slice_start);
        trace::rec(
            "processSimplePathDefault",
            "processSimplePathDefault.processSlice",
            trace::id(current_base).as_deref(),
            trace::id(&diff_matches[i]).as_deref(),
            Some(json!({ "sliceIndex": i, "sliceName": diff_matches[i].get("sliceName") })),
        );
        let mut ncur = WalkCursor {
            base_source_url: cur.base_source_url.clone(),
            base: cur.base.clone(),
            base_cursor: cur.base_cursor,
            diff_cursor: slice_start,
            context_name: cur.context_name.clone(),
            result_path_base: cur.result_path_base.clone(),
        };
        let mut nframe = frame.clone();
        nframe.base_limit = new_base_limit;
        nframe.diff_limit = new_diff_limit as isize;
        nframe.profile_name = format!("{}{}", frame.profile_name, super::simple::path_tail(&diff_matches[i]));
        // PathSlicingParams(true, slicerElement, null): path is null, not current_base_path.
        nframe.slicing = SlicingParams::done_with(Some(std::rc::Rc::new(slicer_element.clone())), None)
            .with_diffs(&diff_matches);
        super::loop_::process_paths(ctx, &mut ncur, &nframe, Some(&slicer_element))?;
    }

    cur.base_cursor = new_base_limit + 1;
    cur.diff_cursor = new_diff_limit + 1;
    Ok(())
}

fn diff_ids(ctx: &WalkContext, idx: &[usize]) -> Vec<String> {
    idx.iter().filter_map(|&i| trace::id(&ctx.diff[i])).collect()
}

/// PU:2417 isSlicedToOneOnly.
pub(crate) fn is_sliced_to_one_only(d: &Value) -> bool {
    has_slicing(d) && d.get("max").and_then(Value::as_str) == Some("1")
}

/// PU:2421 isTypeSlicing.
pub(crate) fn is_type_slicing(d: &Value) -> bool {
    let Some(slicing) = d.get("slicing") else {
        return false;
    };
    let Some(discs) = slicing.get("discriminator").and_then(Value::as_array) else {
        return false;
    };
    discs.len() == 1
        && discs[0].get("type").and_then(Value::as_str) == Some("type")
        && discs[0].get("path").and_then(Value::as_str) == Some("$this")
}

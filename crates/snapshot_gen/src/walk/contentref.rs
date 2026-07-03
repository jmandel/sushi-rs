//! contentReference resolution + redirector (§3.9). replaceFromContentReference
//! (PU:1885) and the walk-into recursion for a contentReference outcome.

use serde_json::Value;
use std::rc::Rc;

use super::context::WalkContext;
use super::emit::update_from_base;
use super::frame::{ElementRedirection, SlicingParams, WalkCursor, WalkFrame};
use super::paths::*;
use super::trace;
use crate::merge::set_field;

/// PU:520 resolveContentReference — find the base element the `#frag` points to.
fn resolve_content_reference(base: &[Value], current_index: usize, content_ref: &str) -> Option<usize> {
    let frag = &content_ref[content_ref.find('#').map(|i| i + 1).unwrap_or(0)..];
    let mut res = current_index as isize - 1;
    while res >= 0 {
        let ed = &base[res as usize];
        if path_of(ed) == frag && !has_slice_name(ed) {
            return Some(res as usize);
        }
        res -= 1;
    }
    None
}

/// PU:1885 replaceFromContentReference — clear contentReference, copy target types.
pub(crate) fn replace_from_content_reference(outcome: &mut Value, tgt: &Value) {
    trace::rec(
        "replaceFromContentReference",
        "replaceFromContentReference",
        trace::id(tgt).as_deref(),
        trace::id(outcome).as_deref(),
        Some(serde_json::json!({
            "contentReference": outcome.get("contentReference"),
            "targetPath": path_of(tgt),
        })),
    );
    if let Some(obj) = outcome.as_object_mut() {
        obj.remove("contentReference");
        if let Some(t) = tgt.get("type") {
            obj.insert("type".to_string(), t.clone());
        } else {
            obj.remove("type");
        }
    }
}

/// From the one-match walk-into branch: resolve the contentReference and recurse
/// into the target's children (same-SD case; cross-SD swaps the base list).
pub(crate) fn walk_into_content_reference_onematch(
    ctx: &mut WalkContext,
    cur: &mut WalkCursor,
    frame: &WalkFrame,
    outcome: &mut Value,
    current_base_path: &str,
    diff0: &Value,
    start: usize,
) -> anyhow::Result<()> {
    let content_ref = outcome
        .get("contentReference")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let Some(tgt_idx) = resolve_content_reference(&cur.base, cur.base_cursor.saturating_sub(1) + 1, &content_ref)
        .or_else(|| resolve_by_path(&cur.base, &content_ref))
    else {
        anyhow::bail!("UNABLE_TO_RESOLVE_REFERENCE_TO_ {content_ref}");
    };
    let tgt = cur.base[tgt_idx].clone();
    replace_from_content_reference(outcome, &tgt);
    // Re-write the emitted outcome (its type/contentReference changed).
    let out_idx = ctx.output.len() - 1;
    ctx.output[out_idx] = outcome.clone();

    let tgt_path = path_of(&tgt).to_string();
    let new_base_cursor = tgt_idx + 1;
    let mut new_base_limit = new_base_cursor;
    let dot = format!("{tgt_path}.");
    while new_base_limit < cur.base.len() && path_of(&cur.base[new_base_limit]).starts_with(&dot) {
        new_base_limit += 1;
    }
    let mut ncur = WalkCursor {
        base_source_url: cur.base_source_url.clone(),
        base: cur.base.clone(),
        base_cursor: new_base_cursor,
        diff_cursor: start.saturating_sub(1),
        context_name: cur.context_name.clone(),
        result_path_base: cur.result_path_base.clone(),
    };
    let mut nframe = frame.clone();
    nframe.base_limit = new_base_limit - 1;
    nframe.diff_limit = cur.diff_cursor as isize - 1;
    nframe.context_path_source = Some(tgt_path.clone());
    nframe.context_path_target = Some(path_of(diff0).to_string());
    nframe.redirector = redirector_stack(&frame.redirector, outcome, current_base_path);
    nframe.slicing = SlicingParams::default();
    super::loop_::process_paths(ctx, &mut ncur, &nframe, None)?;
    Ok(())
}

/// From the empty-diff branch.
pub(crate) fn walk_into_content_reference(
    ctx: &mut WalkContext,
    cur: &mut WalkCursor,
    frame: &WalkFrame,
    outcome: &mut Value,
    current_base_path: &str,
    start: usize,
    _slices: bool,
) -> anyhow::Result<()> {
    let content_ref = outcome
        .get("contentReference")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let Some(tgt_idx) = resolve_by_path(&cur.base, &content_ref) else {
        anyhow::bail!("UNABLE_TO_RESOLVE_REFERENCE_TO_ {content_ref}");
    };
    let tgt = cur.base[tgt_idx].clone();
    replace_from_content_reference(outcome, &tgt);
    let out_idx = ctx.output.len() - 1;
    ctx.output[out_idx] = outcome.clone();

    let tgt_path = path_of(&tgt).to_string();
    let new_base_cursor = tgt_idx + 1;
    let mut new_base_limit = new_base_cursor;
    let dot = format!("{tgt_path}.");
    while new_base_limit < cur.base.len() && path_of(&cur.base[new_base_limit]).starts_with(&dot) {
        new_base_limit += 1;
    }
    // PPP:1256-1264 same-SD branch: contextPathTarget = outcome.getPath(),
    // diffCursor = start (the cross-SD branch uses start-1; we only support same-SD).
    let outcome_path = path_of(outcome).to_string();
    let mut ncur = WalkCursor {
        base_source_url: cur.base_source_url.clone(),
        base: cur.base.clone(),
        base_cursor: new_base_cursor,
        diff_cursor: start,
        context_name: cur.context_name.clone(),
        result_path_base: cur.result_path_base.clone(),
    };
    let mut nframe = frame.clone();
    nframe.base_limit = new_base_limit - 1;
    nframe.diff_limit = cur.diff_cursor as isize - 1;
    nframe.context_path_source = Some(tgt_path.clone());
    nframe.context_path_target = Some(outcome_path);
    nframe.redirector = redirector_stack(&frame.redirector, outcome, current_base_path);
    nframe.slicing = SlicingParams::default();
    let _ = update_from_base; // kept for parity with Java re-home path
    super::loop_::process_paths(ctx, &mut ncur, &nframe, None)?;
    Ok(())
}

fn resolve_by_path(base: &[Value], content_ref: &str) -> Option<usize> {
    let frag = &content_ref[content_ref.find('#').map(|i| i + 1).unwrap_or(0)..];
    base.iter()
        .position(|ed| path_of(ed) == frag && !has_slice_name(ed))
}

/// PU:1867 redirectorStack.
fn redirector_stack(
    redirector: &[ElementRedirection],
    outcome: &Value,
    path: &str,
) -> Vec<ElementRedirection> {
    let mut result = redirector.to_vec();
    result.push(ElementRedirection {
        path: path.to_string(),
        element: Rc::new(outcome.clone()),
    });
    result
}

#[allow(unused_imports)]
use set_field as _set_field;

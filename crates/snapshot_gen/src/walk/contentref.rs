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

/// Public wrapper: resolve `currentBase`'s contentReference target (walking
/// backwards from `current_index`, skipping slices). Mirrors Java
/// `resolveContentReference(base, currentBase)` where start = indexOf(currentBase).
pub(crate) fn resolve_content_reference_pub(
    base: &[Value],
    current_index: usize,
    content_ref: &str,
) -> Option<usize> {
    resolve_content_reference(base, current_index, content_ref)
}

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

/// PU:3553 getElementById — resolve a contentReference to
/// `(elements, index, swapped_source_url)`. An absolute `url#frag` whose url
/// differs from the frame's sourceStructureDefinition fetches THAT SD (with
/// snapshot) and searches its snapshot instead (the cross-SD case). Match is by
/// element **id** (`"#"+ed.getId() == contentReference`), not path.
fn get_element_by_id(
    ctx: &mut WalkContext,
    base: &Rc<Vec<Value>>,
    source_sd_url: &str,
    content_ref: &str,
) -> anyhow::Result<Option<(Rc<Vec<Value>>, usize, Option<String>)>> {
    let mut frag = content_ref.to_string();
    let mut elements: Rc<Vec<Value>> = base.clone();
    let mut swapped: Option<String> = None;
    if !content_ref.starts_with('#') && content_ref.contains('#') {
        let hash = content_ref.find('#').unwrap();
        let url = &content_ref[..hash];
        frag = content_ref[hash..].to_string();
        if url != source_sd_url {
            let Some(sd) = super::resolve::resolve_with_snapshot(ctx, url)? else {
                return Ok(None);
            };
            let resolved = sd
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or(url)
                .to_string();
            elements = Rc::new(super::simple::snapshot_elements(&sd));
            swapped = Some(resolved);
        }
    }
    let id = frag.trim_start_matches('#');
    for (i, ed) in elements.iter().enumerate() {
        if ed.get("id").and_then(Value::as_str) == Some(id) {
            return Ok(Some((elements.clone(), i, swapped)));
        }
    }
    Ok(None)
}

/// From the one-match walk-into branch (PPP:958-996): resolve the
/// contentReference via getElementById; cross-SD swaps the base list to the
/// target SD's snapshot AND the frame's sourceStructureDefinition (PPP:963-978).
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
    let Some((tgt_list, tgt_idx, swapped)) =
        get_element_by_id(ctx, &cur.base, &frame.source_sd_url, &content_ref)?
    else {
        anyhow::bail!("UNABLE_TO_RESOLVE_REFERENCE_TO_ {content_ref}");
    };
    let tgt = tgt_list[tgt_idx].clone();
    replace_from_content_reference(outcome, &tgt);
    // Re-write the emitted outcome (its type/contentReference changed).
    let out_idx = ctx.output.len() - 1;
    ctx.output[out_idx] = outcome.clone();

    let tgt_path = path_of(&tgt).to_string();
    let new_base_cursor = tgt_idx + 1;
    let mut new_base_limit = new_base_cursor;
    let dot = format!("{tgt_path}.");
    while new_base_limit < tgt_list.len() && path_of(&tgt_list[new_base_limit]).starts_with(&dot) {
        new_base_limit += 1;
    }
    let mut ncur = WalkCursor {
        base_source_url: swapped
            .clone()
            .unwrap_or_else(|| cur.base_source_url.clone()),
        base: tgt_list,
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
    if let Some(src) = &swapped {
        // PPP:977 withSourceStructureDefinition(target.getSource()).
        nframe.source_sd_url = src.clone();
    }
    nframe.slicing = SlicingParams::default();
    super::loop_::process_paths(ctx, &mut ncur, &nframe, None)?;
    Ok(())
}

/// From the empty-diff branch (PPP:1228-1266). Cross-SD (PPP:1233-1250):
/// resolve via getElementById; NOTE Java MUTATES `cursors.base` to the target
/// SD's snapshot (the caller's base list stays swapped afterwards), keeps
/// `cursors.baseSource` unchanged, uses nested diffCursor `start - 1` and
/// contextPathTarget = diffMatches[0].path, and sets the frame's
/// sourceStructureDefinition to the target SD. Same-SD (PPP:1251-1266): nested
/// diffCursor `start`, contextPathTarget = outcome.path.
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
    let Some((tgt_list, tgt_idx, swapped)) =
        get_element_by_id(ctx, &cur.base, &frame.source_sd_url, &content_ref)?
    else {
        anyhow::bail!("UNABLE_TO_RESOLVE_REFERENCE_TO_ {content_ref}");
    };
    let tgt = tgt_list[tgt_idx].clone();
    replace_from_content_reference(outcome, &tgt);
    let out_idx = ctx.output.len() - 1;
    ctx.output[out_idx] = outcome.clone();

    let tgt_path = path_of(&tgt).to_string();
    let new_base_cursor = tgt_idx + 1;
    let mut new_base_limit = new_base_cursor;
    let dot = format!("{tgt_path}.");
    while new_base_limit < tgt_list.len() && path_of(&tgt_list[new_base_limit]).starts_with(&dot) {
        new_base_limit += 1;
    }
    let outcome_path = path_of(outcome).to_string();
    if swapped.is_some() {
        // PPP:1234 cursors.base = tgt.getSource().getSnapshot() — persists.
        cur.base = tgt_list.clone();
    }
    let mut ncur = WalkCursor {
        base_source_url: cur.base_source_url.clone(),
        base: tgt_list,
        base_cursor: new_base_cursor,
        diff_cursor: if swapped.is_some() { start.saturating_sub(1) } else { start },
        context_name: cur.context_name.clone(),
        result_path_base: cur.result_path_base.clone(),
    };
    let mut nframe = frame.clone();
    nframe.base_limit = new_base_limit - 1;
    nframe.diff_limit = cur.diff_cursor as isize - 1;
    nframe.context_path_source = Some(tgt_path.clone());
    nframe.context_path_target = Some(outcome_path);
    nframe.redirector = redirector_stack(&frame.redirector, outcome, current_base_path);
    if let Some(src) = &swapped {
        nframe.source_sd_url = src.clone();
    }
    nframe.slicing = SlicingParams::default();
    let _ = update_from_base; // kept for parity with Java re-home path
    super::loop_::process_paths(ctx, &mut ncur, &nframe, None)?;
    Ok(())
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

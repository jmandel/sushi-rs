//! Type-slicing: diffsConstrainTypes predicate (PU:1821) and the
//! processSimplePathWhereDiffsConstrainTypes branch (§3.3.3, PPP:554).
//! Implemented incrementally; the full type-slice synthesis is needed for the
//! choice rung.

use serde_json::{json, Value};
use std::rc::Rc;

use super::context::WalkContext;
use super::frame::{SlicingParams, WalkCursor, WalkFrame};
use super::paths::*;
use super::simple::{snapshot_elements, tail};
use super::trace;
use super::types_pred::*;
use crate::merge::set_field;

#[derive(Clone, Debug)]
pub(crate) struct TypeSlice {
    #[allow(dead_code)]
    pub ed_path: String,
    pub ed_idx_in_diff_matches: usize,
    pub type_: Option<String>,
}

/// PU:1821 diffsConstrainTypes. Fills `type_list`; returns whether the diff rows
/// constrain individual types of a `[x]` element.
pub(crate) fn diffs_constrain_types(
    ctx: &WalkContext,
    diff_matches: &[Value],
    cpath: &str,
    type_list: &mut Vec<TypeSlice>,
) -> bool {
    let p = path_of(&diff_matches[0]);
    if !p.ends_with("[x]") && !cpath.ends_with("[x]") {
        return false;
    }
    type_list.clear();
    let rn_full = tail(cpath);
    let rn = &rn_full[..rn_full.len().saturating_sub(3)];
    for (i, ed) in diff_matches.iter().enumerate() {
        let n = tail(path_of(ed));
        if !n.starts_with(rn) {
            return false;
        }
        let s = &n[rn.len()..];
        if !s.contains('.') {
            let sn = has_slice_name(ed);
            let type_count = ed.get("type").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
            if sn && type_count == 1 {
                type_list.push(TypeSlice {
                    ed_path: path_of(ed).to_string(),
                    ed_idx_in_diff_matches: i,
                    type_: working_code(&ed.get("type").and_then(Value::as_array).unwrap()[0]),
                });
            } else if sn && type_count == 0 {
                if is_data_type_str(ctx, s) {
                    type_list.push(TypeSlice { ed_path: path_of(ed).to_string(), ed_idx_in_diff_matches: i, type_: Some(s.to_string()) });
                } else if is_primitive_str(ctx, &uncapitalize(s)) {
                    type_list.push(TypeSlice { ed_path: path_of(ed).to_string(), ed_idx_in_diff_matches: i, type_: Some(uncapitalize(s)) });
                } else if let Some(slice_name) = ed.get("sliceName").and_then(Value::as_str) {
                    let tn = &slice_name[n.len().min(slice_name.len())..];
                    if is_data_type_str(ctx, tn) {
                        type_list.push(TypeSlice { ed_path: path_of(ed).to_string(), ed_idx_in_diff_matches: i, type_: Some(tn.to_string()) });
                    } else if is_primitive_str(ctx, &uncapitalize(tn)) {
                        type_list.push(TypeSlice { ed_path: path_of(ed).to_string(), ed_idx_in_diff_matches: i, type_: Some(uncapitalize(tn)) });
                    }
                }
            } else if !sn && s != "[x]" {
                if is_data_type_str(ctx, s) {
                    type_list.push(TypeSlice { ed_path: path_of(ed).to_string(), ed_idx_in_diff_matches: i, type_: Some(s.to_string()) });
                } else if is_primitive_str(ctx, &uncapitalize(s)) {
                    type_list.push(TypeSlice { ed_path: path_of(ed).to_string(), ed_idx_in_diff_matches: i, type_: Some(uncapitalize(s)) });
                }
            } else if !sn && s == "[x]" {
                type_list.push(TypeSlice { ed_path: path_of(ed).to_string(), ed_idx_in_diff_matches: i, type_: None });
            }
        }
    }
    true
}

fn uncapitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// §3.3.3 processSimplePathWhereDiffsConstrainTypes (PPP:554).
/// Under oracle config (R4+/newSlicingProcessing) the shortcut synthesizes a
/// no-types CLOSED $this-TYPE anchor, then processes the root and each type slice.
pub(crate) fn process_simple_path_where_diffs_constrain_types(
    ctx: &mut WalkContext,
    cur: &mut WalkCursor,
    frame: &WalkFrame,
    current_base_path: &str,
    diff_match_idx: &[usize],
    type_list: &mut [TypeSlice],
) -> anyhow::Result<()> {
    let new_base_limit = find_end_of_element(&cur.base, cur.base_cursor);
    let new_diff_cursor = diff_match_idx[0];
    let short_cut = !type_list.is_empty() && type_list[0].type_.is_some();

    trace::rec(
        "processSimplePathWhereDiffsConstrainTypes",
        "processSimplePathWhereDiffsConstrainTypes.entry",
        trace::id(&cur.base[cur.base_cursor]).as_deref(),
        trace::id(&ctx.diff[new_diff_cursor]).as_deref(),
        Some(json!({ "shortCut": short_cut, "typeSlices": type_list.len(), "basePath": current_base_path })),
    );

    // Build a working diff-matches list (values) — we may prepend a synthesized anchor.
    let mut diff_values: Vec<Value> = diff_match_idx.iter().map(|&i| ctx.diff[i].clone()).collect();
    let mut inserted_anchor = false;
    let mut new_diff_limit = find_end_of_element(&ctx.diff, new_diff_cursor);

    if short_cut {
        // R4 branch: synthesized element specifies no types (all base types allowed).
        let anchor = json!({
            "path": path_of(&ctx.diff[new_diff_cursor]),
            "slicing": {
                "discriminator": [ { "type": "type", "path": "$this" } ],
                "rules": "closed",
                "ordered": false
            }
        });
        // Insert into ctx.diff at new_diff_cursor and into diff_values front.
        let mut new_diff = (*ctx.diff).clone();
        new_diff.insert(new_diff_cursor, anchor.clone());
        ctx.diff = Rc::new(new_diff);
        ctx.diff_consumed.insert(new_diff_cursor, false);
        ctx.diff_injected.insert(new_diff_cursor, true);
        diff_values.insert(0, anchor);
        inserted_anchor = true;
        new_diff_limit += 1;
    }

    // Process the root with the synthesized slice active.
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
    nframe.diff_limit = new_diff_cursor as isize; // just the anchor row
    nframe.slicing = SlicingParams::done_with(None, Some(current_base_path.to_string()));
    let root_idx = super::loop_::process_paths(ctx, &mut ncur, &nframe, None)?;

    // Re-stamp slicing on the anchor: $this / TYPE / CLOSED / unordered.
    if let Some(idx) = root_idx {
        set_field(
            &mut ctx.output[idx],
            "slicing",
            json!({
                "discriminator": [ { "type": "type", "path": "$this" } ],
                "rules": "closed",
                "ordered": false
            }),
        );
    }
    let slicer_element = root_idx.map(|i| ctx.output[i].clone());

    // Process each type slice sibling.
    let start = if inserted_anchor { 1 } else { 0 };
    let total_slices = type_list.len();
    for (k, ts) in type_list.iter().enumerate() {
        let dm_index = ts.ed_idx_in_diff_matches + if inserted_anchor { 1 } else { 0 };
        let slice_diff = &diff_values[dm_index];
        let real_diff_idx = index_in_diff(ctx, slice_diff);
        trace::rec(
            "processSimplePathWhereDiffsConstrainTypes",
            "processSimplePathWhereDiffsConstrainTypes.processTypeSlice",
            trace::id(&cur.base[cur.base_cursor]).as_deref(),
            trace::id(slice_diff).as_deref(),
            Some(json!({ "sliceIndex": k, "sliceName": slice_diff.get("sliceName") })),
        );
        let Some(real_idx) = real_diff_idx else { continue };
        let slice_end = find_end_of_element(&ctx.diff, real_idx);
        let mut scur = WalkCursor {
            base_source_url: cur.base_source_url.clone(),
            base: cur.base.clone(),
            base_cursor: cur.base_cursor,
            diff_cursor: real_idx,
            context_name: cur.context_name.clone(),
            result_path_base: cur.result_path_base.clone(),
        };
        let mut sframe = frame.clone();
        sframe.base_limit = new_base_limit;
        sframe.diff_limit = slice_end as isize;
        sframe.slicing = SlicingParams::done_with(
            slicer_element.clone().map(Rc::new),
            Some(current_base_path.to_string()),
        )
        .with_diffs(&diff_values[start..]);
        let _ = total_slices;
        super::loop_::process_paths(ctx, &mut scur, &sframe, slicer_element.as_ref())?;
    }

    // Remove the synthesized anchor row from differential.
    if inserted_anchor {
        let mut new_diff = (*ctx.diff).clone();
        new_diff.remove(new_diff_cursor);
        ctx.diff = Rc::new(new_diff);
        ctx.diff_consumed.remove(new_diff_cursor);
        ctx.diff_injected.remove(new_diff_cursor);
        new_diff_limit -= 1;
    }
    let _ = (capitalize(""), snapshot_elements(&Value::Null));

    cur.base_cursor = new_base_limit + 1;
    cur.diff_cursor = new_diff_limit + 1;
    Ok(())
}

fn index_in_diff(ctx: &WalkContext, ed: &Value) -> Option<usize> {
    ctx.diff.iter().position(|d| std::ptr::eq(d, ed) || d == ed)
}

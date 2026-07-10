//! Type-slicing: diffsConstrainTypes predicate (PU:1821) and
//! processSimplePathWhereDiffsConstrainTypes (PPP:554-748), ported line-by-line.

use serde_json::{json, Value};
use std::rc::Rc;

use super::context::WalkContext;
use super::frame::{SlicingParams, WalkCursor, WalkFrame};
use super::paths::*;
use super::simple::{path_tail, tail};
use super::trace;
use super::types_pred::*;
use crate::merge::set_field;

#[derive(Clone, Debug)]
pub(crate) struct TypeSlice {
    /// Index into the ORIGINAL diff-matches list (pre anchor-insert).
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
            let type_count = ed
                .get("type")
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0);
            if sn && type_count == 1 {
                type_list.push(TypeSlice {
                    ed_idx_in_diff_matches: i,
                    type_: working_code(&ed.get("type").and_then(Value::as_array).unwrap()[0]),
                });
            } else if sn && type_count == 0 {
                if is_data_type_str(ctx, s) {
                    type_list.push(TypeSlice {
                        ed_idx_in_diff_matches: i,
                        type_: Some(s.to_string()),
                    });
                } else if is_primitive_str(ctx, &uncapitalize(s)) {
                    type_list.push(TypeSlice {
                        ed_idx_in_diff_matches: i,
                        type_: Some(uncapitalize(s)),
                    });
                } else if let Some(slice_name) = ed.get("sliceName").and_then(Value::as_str) {
                    let tn = &slice_name[n.len().min(slice_name.len())..];
                    if is_data_type_str(ctx, tn) {
                        type_list.push(TypeSlice {
                            ed_idx_in_diff_matches: i,
                            type_: Some(tn.to_string()),
                        });
                    } else if is_primitive_str(ctx, &uncapitalize(tn)) {
                        type_list.push(TypeSlice {
                            ed_idx_in_diff_matches: i,
                            type_: Some(uncapitalize(tn)),
                        });
                    }
                }
            } else if !sn && s != "[x]" {
                if is_data_type_str(ctx, s) {
                    type_list.push(TypeSlice {
                        ed_idx_in_diff_matches: i,
                        type_: Some(s.to_string()),
                    });
                } else if is_constrained_data_type(ctx, s) {
                    type_list.push(TypeSlice {
                        ed_idx_in_diff_matches: i,
                        type_: Some(base_type_of(ctx, s)),
                    });
                } else if is_primitive_str(ctx, &uncapitalize(s)) {
                    type_list.push(TypeSlice {
                        ed_idx_in_diff_matches: i,
                        type_: Some(uncapitalize(s)),
                    });
                }
            } else if !sn && s == "[x]" {
                type_list.push(TypeSlice {
                    ed_idx_in_diff_matches: i,
                    type_: None,
                });
            }
        }
    }
    true
}

fn is_constrained_data_type(ctx: &WalkContext, value: &str) -> bool {
    match super::resolve::fetch_sd(ctx.pkg, value) {
        Some(sd) => {
            sd.get("kind").and_then(Value::as_str) == Some("complex-type")
                && sd.get("derivation").and_then(Value::as_str) == Some("constraint")
        }
        None => matches!(value, "SimpleQuantity" | "MoneyQuantity"),
    }
}

fn base_type_of(ctx: &WalkContext, value: &str) -> String {
    match super::resolve::fetch_sd(ctx.pkg, value) {
        Some(sd) => sd
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(value)
            .to_string(),
        None => "Quantity".to_string(),
    }
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

/// PU:1797 rootName.
fn root_name(cpath: &str) -> String {
    tail(cpath).replace("[x]", "")
}

/// PU:1803 determineTypeSlicePath.
fn determine_type_slice_path(path: &str, cpath: &str) -> String {
    let head_p = &path[..path.rfind('.').unwrap_or(0)];
    let tail_c = &cpath[cpath.rfind('.').map(|i| i + 1).unwrap_or(0)..];
    format!("{head_p}.{tail_c}")
}

fn diff_mutate(ctx: &mut WalkContext, idx: usize, f: impl FnOnce(&mut Value)) {
    let mut new_diff = (*ctx.diff).clone();
    if let Some(v) = new_diff.get_mut(idx) {
        f(v);
    }
    ctx.diff = Rc::new(new_diff);
}

/// PPP:554 processSimplePathWhereDiffsConstrainTypes.
pub(crate) fn process_simple_path_where_diffs_constrain_types(
    ctx: &mut WalkContext,
    cur: &mut WalkCursor,
    frame: &WalkFrame,
    current_base_path: &str,
    diff_match_idx: &[usize],
    type_list: &mut [TypeSlice],
) -> anyhow::Result<()> {
    let new_base_limit = find_end_of_element(&cur.base, cur.base_cursor);
    let mut new_diff_cursor = diff_match_idx[0];
    let short_cut = !type_list.is_empty() && type_list[0].type_.is_some();
    let path = path_of(&ctx.diff[diff_match_idx[0]]).to_string();

    trace::rec(
        "processSimplePathWhereDiffsConstrainTypes",
        "processSimplePathWhereDiffsConstrainTypes.entry",
        None,
        trace::id(&ctx.diff[diff_match_idx[0]]).as_deref(),
        Some(
            json!({ "shortCut": short_cut, "typeSlices": type_list.len(), "basePath": current_base_path }),
        ),
    );

    // Working diff-match indices (into the LIVE ctx.diff). After anchor insert,
    // original indices >= new_diff_cursor shift +1.
    let mut dm_idx: Vec<usize> = diff_match_idx.to_vec();
    let mut inserted_anchor = false;

    if short_cut {
        // R4+/newSlicingProcessing branch (PPP:583-596): synthesized element
        // specifies NO types (= all base types allowed), $this/TYPE/CLOSED/unordered.
        let anchor = json!({
            "path": determine_type_slice_path(&path, current_base_path),
            "slicing": {
                "discriminator": [ { "type": "type", "path": "$this" } ],
                "rules": "closed",
                "ordered": false
            }
        });
        let mut new_diff = (*ctx.diff).clone();
        new_diff.insert(new_diff_cursor, anchor);
        ctx.diff = Rc::new(new_diff);
        ctx.diff_consumed.insert(new_diff_cursor, false);
        ctx.diff_injected.insert(new_diff_cursor, true);
        for i in dm_idx.iter_mut() {
            if *i >= new_diff_cursor {
                *i += 1;
            }
        }
        dm_idx.insert(0, new_diff_cursor);
        inserted_anchor = true;
    } else {
        // Path tail must match (PPP:597-603).
        let t1 = &current_base_path[current_base_path.rfind('.').map(|i| i + 1).unwrap_or(0)..];
        let t2 = &path[path.rfind('.').map(|i| i + 1).unwrap_or(0)..];
        if t1 != t2 {
            anyhow::bail!("ED_PATH_WRONG_TYPE_MATCH: {path} vs {current_base_path}");
        }
    }
    let mut new_diff_limit = find_end_of_element(&ctx.diff, new_diff_cursor);

    // Slicing legality on diffMatches[0].slicing (PPP:608-623).
    let anchor_slicing = ctx.diff[dm_idx[0]]
        .get("slicing")
        .cloned()
        .unwrap_or(Value::Null);
    if anchor_slicing.get("ordered").and_then(Value::as_bool) == Some(true) {
        anyhow::bail!("TYPE_SLICING_WITH_SLICINGORDERED_TRUE at {current_base_path}");
    }
    if let Some(discs) = anchor_slicing
        .get("discriminator")
        .and_then(Value::as_array)
    {
        if discs.len() != 1
            || discs[0].get("type").and_then(Value::as_str) != Some("type")
            || discs[0].get("path").and_then(Value::as_str) != Some("$this")
        {
            anyhow::bail!("TYPE_SLICING_WITH_BAD_DISCRIMINATOR at {current_base_path}");
        }
    }

    // Slice-name/type coherence (PPP:624-645) — mutates the LIVE diff rows.
    for ts in type_list.iter() {
        let Some(tn_type) = &ts.type_ else { continue };
        let live_idx = dm_idx[ts.ed_idx_in_diff_matches + if inserted_anchor { 1 } else { 0 }];
        let tn = format!("{}{}", root_name(current_base_path), capitalize(tn_type));
        let row = &ctx.diff[live_idx];
        match row.get("sliceName").and_then(Value::as_str) {
            None => {
                diff_mutate(ctx, live_idx, |v| {
                    set_field(v, "sliceName", Value::String(tn.clone()))
                });
            }
            Some(existing) if existing != tn => {
                // autoFixSliceNames=false under oracle → throw (PPP:634).
                anyhow::bail!(
                    "ERROR_AT_PATH__SLICE_NAME_MUST_BE__BUT_IS_: at {current_base_path} slice name must be {tn} but is {existing}"
                );
            }
            _ => {}
        }
        let row = &ctx.diff[live_idx];
        let types = row
            .get("type")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if types.is_empty() {
            let code = tn_type.clone();
            diff_mutate(ctx, live_idx, |v| {
                set_field(v, "type", json!([{ "code": code }]));
            });
        } else if types.len() > 1 {
            anyhow::bail!(
                "ERROR_AT_PATH__SLICE_FOR_TYPE__HAS_MORE_THAN_ONE_TYPE_ at {current_base_path}"
            );
        } else if types[0].get("code").and_then(Value::as_str) != Some(tn_type.as_str()) {
            anyhow::bail!("ERROR_AT_PATH__SLICE_FOR_TYPE__HAS_WRONG_TYPE_ at {current_base_path}");
        }
    }

    // Process the root (PPP:648-668).
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
    nframe.profile_name = format!("{}{}", frame.profile_name, path_tail(&ctx.diff[dm_idx[0]]));
    nframe.slicing = SlicingParams::done_with(None, None);
    let root_idx = super::loop_::process_paths(ctx, &mut ncur, &nframe, None)?;
    let Some(root_idx) = root_idx else {
        anyhow::bail!("DID_NOT_FIND_TYPE_ROOT_: {path}");
    };
    // Re-stamp slicing: $this / TYPE / CLOSED / unordered (PPP:661-664).
    set_field(
        &mut ctx.output[root_idx],
        "slicing",
        json!({
            "discriminator": [ { "type": "type", "path": "$this" } ],
            "rules": "closed",
            "ordered": false
        }),
    );
    let mut slicer_element = ctx.output[root_idx].clone();
    let root_type_count = ctx.output[root_idx]
        .get("type")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    if root_type_count > 1 {
        set_field(&mut slicer_element, "min", Value::from(0u64));
    }

    let start = 1usize;

    // Per-type-slice siblings (PPP:672-704).
    let mut fixed_type: Option<String> = None;
    for i in start..dm_idx.len() {
        let live_idx = dm_idx[i];
        let row = ctx.diff[live_idx].clone();
        if row.get("min").and_then(Value::as_u64).unwrap_or(0) > 0 {
            if dm_idx.len() > i + 1 {
                anyhow::bail!("INVALID_SLICING__MIN__1 at {}", path_of(&row));
            } else {
                set_field(&mut ctx.output[root_idx], "min", Value::from(1u64));
            }
            fixed_type = Some(determine_fixed_type(ctx, &row)?);
        }
        new_diff_cursor = live_idx;
        new_diff_limit = find_end_of_element(&ctx.diff, new_diff_cursor);
        trace::rec(
            "processSimplePathWhereDiffsConstrainTypes",
            "processSimplePathWhereDiffsConstrainTypes.processTypeSlice",
            None,
            trace::id(&row).as_deref(),
            Some(json!({ "sliceIndex": i, "sliceName": row.get("sliceName") })),
        );
        let mut scur = WalkCursor {
            base_source_url: cur.base_source_url.clone(),
            base: cur.base.clone(),
            base_cursor: cur.base_cursor,
            diff_cursor: new_diff_cursor,
            context_name: cur.context_name.clone(),
            result_path_base: cur.result_path_base.clone(),
        };
        let mut sframe = frame.clone();
        sframe.base_limit = new_base_limit;
        sframe.diff_limit = new_diff_limit as isize;
        sframe.profile_name = format!("{}{}", frame.profile_name, path_tail(&row));
        let dm_values: Vec<Value> = dm_idx.iter().map(|&x| ctx.diff[x].clone()).collect();
        sframe.slicing =
            SlicingParams::done_with(Some(Rc::new(ctx.output[root_idx].clone())), None)
                .with_diffs(&dm_values);
        let slice_res =
            super::loop_::process_paths(ctx, &mut scur, &sframe, Some(&slicer_element))?;
        if type_list.len() > start + 1 {
            if let Some(si) = slice_res {
                set_field(&mut ctx.output[si], "min", Value::from(0u64));
            }
        }
    }

    // Remove the synthesized anchor from the differential (PPP:705-708).
    if inserted_anchor {
        let anchor_pos = dm_idx[0];
        let mut new_diff = (*ctx.diff).clone();
        new_diff.remove(anchor_pos);
        ctx.diff = Rc::new(new_diff);
        ctx.diff_consumed.remove(anchor_pos);
        ctx.diff_injected.remove(anchor_pos);
        new_diff_limit -= 1;
    }

    // fixedType pruning on the root element (PPP:709-716).
    if let Some(ft) = &fixed_type {
        if let Some(types) = ctx.output[root_idx]
            .get_mut("type")
            .and_then(Value::as_array_mut)
        {
            types.retain(|tr| tr.get("code").and_then(Value::as_str) == Some(ft.as_str()));
        }
    }

    // Allowed-types check / OPEN relaxation (PPP:717-743).
    if ctx.output[root_idx].get("max").and_then(Value::as_str) != Some("0") {
        let mut allowed_types: Vec<String> = ctx.output[root_idx]
            .get("type")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.get("code").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        for ts in type_list.iter() {
            let dm_pos = ts.ed_idx_in_diff_matches + if inserted_anchor { 1 } else { 0 };
            if let Some(t) = &ts.type_ {
                allowed_types.retain(|x| x != t);
            } else if let Some(&live_idx) = dm_idx.get(dm_pos) {
                // Adjust for a removed synthesized anchor before this row.
                let live_idx = if inserted_anchor && live_idx > dm_idx[0] {
                    live_idx - 1
                } else {
                    live_idx
                };
                if let Some(row) = ctx.diff.get(live_idx) {
                    if has_slice_name(row) {
                        let codes: Vec<String> = row
                            .get("type")
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .filter_map(|t| {
                                        t.get("code").and_then(Value::as_str).map(str::to_string)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        if codes.len() == 1 {
                            allowed_types.retain(|x| x != &codes[0]);
                        }
                    }
                }
            }
        }
        if !allowed_types.is_empty() {
            if current_base_path.contains("xtension.value") && short_cut {
                if let Some(types) = ctx.output[root_idx]
                    .get_mut("type")
                    .and_then(Value::as_array_mut)
                {
                    types.retain(|tr| {
                        !allowed_types
                            .iter()
                            .any(|a| tr.get("code").and_then(Value::as_str) == Some(a.as_str()))
                    });
                }
            } else {
                trace::rec(
                    "processSimplePathWhereDiffsConstrainTypes",
                    "processSimplePathWhereDiffsConstrainTypes.overwriteSlicingToOpen",
                    None,
                    trace::id(&ctx.output[root_idx]).as_deref(),
                    Some(json!({ "unusedTypes": java_hashset_order(&allowed_types) })),
                );
                if let Some(slicing) = ctx.output[root_idx].get_mut("slicing") {
                    set_field(slicing, "rules", Value::String("open".to_string()));
                }
            }
        }
    }

    // Cursor advance (PPP:746-747).
    cur.base_cursor = new_base_limit + 1;
    cur.diff_cursor = new_diff_limit + 1;
    Ok(())
}

/// Emulate `java.util.HashSet<String>` iteration order (default capacity 16,
/// no resize for small sets): bucket = (h ^ h>>>16) & 15 with Java's
/// String.hashCode; buckets in order, insertion order within a bucket. Used
/// ONLY for trace payload parity — Java's tracer serializes the HashSet from
/// PU:1619 getListOfTypes; the set contents (the actual decision) are identical.
fn java_hashset_order(items: &[String]) -> Vec<String> {
    fn java_string_hash(s: &str) -> i32 {
        let mut h: i32 = 0;
        for c in s.encode_utf16() {
            h = h.wrapping_mul(31).wrapping_add(c as i32);
        }
        h
    }
    let mut buckets: Vec<Vec<&String>> = vec![Vec::new(); 16];
    for item in items {
        let h = java_string_hash(item);
        let h = h ^ ((h as u32) >> 16) as i32;
        buckets[(h & 15) as usize].push(item);
    }
    buckets.into_iter().flatten().cloned().collect()
}

/// BaseTypeSlice (BTS): an existing type-slice on the already-sliced base.
#[derive(Clone, Debug)]
struct BaseTypeSlice {
    type_: String,
    start: usize,
    end: usize,
    handled: bool,
}

/// PU:findBaseSlices — enumerate existing type slices on a sliced base anchor.
fn find_base_slices(list: &[Value], start: usize) -> Vec<BaseTypeSlice> {
    let mut res = Vec::new();
    let base_path = path_of(&list[start]).to_string();
    let dot = format!("{base_path}.");
    let mut i = start + 1;
    // skip the anchor's own children
    while i < list.len() && path_of(&list[i]).starts_with(&dot) {
        i += 1;
    }
    // each following same-path element with a sliceName is a base type slice
    while i < list.len() && path_of(&list[i]) == base_path && has_slice_name(&list[i]) {
        let s = i;
        i += 1;
        while i < list.len() && path_of(&list[i]).starts_with(&dot) {
            i += 1;
        }
        let type_ = list[s]
            .get("type")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|t| t.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        res.push(BaseTypeSlice {
            type_,
            start: s,
            end: i - 1,
            handled: false,
        });
    }
    res
}

/// PPP:1650 processPathWithSlicedBaseWhereDiffsConstrainTypes (§3.6.2).
#[allow(clippy::too_many_arguments)]
pub(crate) fn process_path_with_sliced_base_where_diffs_constrain_types(
    ctx: &mut WalkContext,
    cur: &mut WalkCursor,
    frame: &WalkFrame,
    current_base_path: &str,
    diff_match_idx: &[usize],
    type_list: &mut [TypeSlice],
) -> anyhow::Result<()> {
    let new_base_limit = find_end_of_element(&cur.base, cur.base_cursor);
    let mut new_diff_cursor = diff_match_idx[0];
    let path = path_of(&ctx.diff[diff_match_idx[0]]).to_string();
    let diff0_has_slice_name = has_slice_name(&ctx.diff[diff_match_idx[0]]);
    let diff0_has_slicing = has_slicing(&ctx.diff[diff_match_idx[0]]);
    let short_cut = (!type_list.is_empty() && type_list[0].type_.is_some())
        || (diff0_has_slice_name && !diff0_has_slicing);

    trace::rec(
        "processPathWithSlicedBaseWhereDiffsConstrainTypes",
        "processPathWithSlicedBaseWhereDiffsConstrainTypes.entry",
        None,
        trace::id(&ctx.diff[diff_match_idx[0]]).as_deref(),
        Some(
            json!({ "shortCut": short_cut, "typeSlices": type_list.len(), "basePath": current_base_path }),
        ),
    );

    let mut dm_idx: Vec<usize> = diff_match_idx.to_vec();
    let mut inserted_anchor = false;

    if short_cut {
        // R4+/newSlicingProcessing: anchor specifies NO types, CLOSED $this slicing.
        let anchor = json!({
            "path": determine_type_slice_path(&path, current_base_path),
            "slicing": {
                "discriminator": [ { "type": "type", "path": "$this" } ],
                "rules": "closed",
                "ordered": false
            }
        });
        let mut new_diff = (*ctx.diff).clone();
        new_diff.insert(new_diff_cursor, anchor);
        ctx.diff = Rc::new(new_diff);
        ctx.diff_consumed.insert(new_diff_cursor, false);
        ctx.diff_injected.insert(new_diff_cursor, true);
        for i in dm_idx.iter_mut() {
            if *i >= new_diff_cursor {
                *i += 1;
            }
        }
        dm_idx.insert(0, new_diff_cursor);
        inserted_anchor = true;
    }
    let mut new_diff_limit = find_end_of_element(&ctx.diff, new_diff_cursor);

    // Slicing legality on diffMatches[0].slicing (PPP:1697-1712).
    let anchor_slicing = ctx.diff[dm_idx[0]]
        .get("slicing")
        .cloned()
        .unwrap_or(Value::Null);
    if anchor_slicing.get("ordered").and_then(Value::as_bool) == Some(true) {
        anyhow::bail!("TYPE_SLICING_WITH_SLICINGORDERED_TRUE at {current_base_path}");
    }
    if let Some(discs) = anchor_slicing
        .get("discriminator")
        .and_then(Value::as_array)
    {
        if discs.len() != 1
            || discs[0].get("type").and_then(Value::as_str) != Some("type")
            || discs[0].get("path").and_then(Value::as_str) != Some("$this")
        {
            anyhow::bail!("TYPE_SLICING_WITH_BAD_DISCRIMINATOR at {current_base_path}");
        }
    }

    // Slice-name/type coherence (PPP:1716-1740) — mutates live diff rows.
    for ts in type_list.iter() {
        let Some(tn_type) = &ts.type_ else { continue };
        let live_idx = dm_idx[ts.ed_idx_in_diff_matches + if inserted_anchor { 1 } else { 0 }];
        let n = tail(current_base_path);
        let tn = format!("{}{}", n.replace("[x]", ""), capitalize(tn_type));
        let row = &ctx.diff[live_idx];
        match row.get("sliceName").and_then(Value::as_str) {
            None => {
                diff_mutate(ctx, live_idx, |v| {
                    set_field(v, "sliceName", Value::String(tn.clone()))
                });
            }
            Some(existing) if existing != tn => {
                anyhow::bail!(
                    "ERROR_AT_PATH__SLICE_NAME_MUST_BE__BUT_IS_: at {current_base_path} must be {tn} but is {existing}"
                );
            }
            _ => {}
        }
        let row = &ctx.diff[live_idx];
        let types = row
            .get("type")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if types.is_empty() {
            let code = tn_type.clone();
            diff_mutate(ctx, live_idx, |v| {
                set_field(v, "type", json!([{ "code": code }]))
            });
        } else if types.len() > 1 {
            anyhow::bail!(
                "ERROR_AT_PATH__SLICE_FOR_TYPE__HAS_MORE_THAN_ONE_TYPE_ at {current_base_path}"
            );
        } else if types[0].get("code").and_then(Value::as_str) != Some(tn_type.as_str()) {
            anyhow::bail!("ERROR_AT_PATH__SLICE_FOR_TYPE__HAS_WRONG_TYPE_ at {current_base_path}");
        }
    }

    // Process the root (PPP:1743-1761): copy the root diff + children.
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
    nframe.profile_name = format!("{}{}", frame.profile_name, path_tail(&ctx.diff[dm_idx[0]]));
    nframe.slicing = SlicingParams::done_with(None, Some(current_base_path.to_string()));
    let root_idx = super::loop_::process_paths(ctx, &mut ncur, &nframe, None)?;
    let Some(root_idx) = root_idx else {
        anyhow::bail!("DID_NOT_FIND_TYPE_ROOT_: {path}");
    };
    // Re-stamp slicing on the root (PPP:1752-1756).
    set_field(
        &mut ctx.output[root_idx],
        "slicing",
        json!({
            "discriminator": [ { "type": "type", "path": "$this" } ],
            "rules": "closed",
            "ordered": false
        }),
    );
    let slicer_element = Rc::new(ctx.output[root_idx].clone());

    let start = 1usize;
    let mut fixed_type: Option<String> = None;
    let mut base_slices = find_base_slices(&cur.base, cur.base_cursor);

    // Per-diff-slice loop (PPP:1765-1793), each may bind to a base slice.
    for i in start..dm_idx.len() {
        let live_idx = dm_idx[i];
        let row = ctx.diff[live_idx].clone();
        let ty = determine_fixed_type(ctx, &row)?;
        if row.get("min").and_then(Value::as_u64).unwrap_or(0) > 0 {
            if dm_idx.len() > i + 1 {
                anyhow::bail!("INVALID_SLICING__MIN__1 at {}", path_of(&row));
            }
            fixed_type = Some(ty.clone());
        }
        new_diff_cursor = live_idx;
        new_diff_limit = find_end_of_element(&ctx.diff, new_diff_cursor);
        let mut s_start = cur.base_cursor;
        let mut s_end = new_base_limit;
        let matched = base_slices.iter().position(|bs| bs.type_ == ty);
        if let Some(bi) = matched {
            s_start = base_slices[bi].start;
            s_end = base_slices[bi].end;
            base_slices[bi].handled = true;
        }
        trace::rec(
            "processPathWithSlicedBaseWhereDiffsConstrainTypes",
            "processPathWithSlicedBaseWhereDiffsConstrainTypes.processTypeSlice",
            None,
            trace::id(&row).as_deref(),
            Some(json!({ "sliceIndex": i, "type": ty, "matchedBaseSlice": matched.is_some() })),
        );
        let mut scur = WalkCursor {
            base_source_url: cur.base_source_url.clone(),
            base: cur.base.clone(),
            base_cursor: s_start,
            diff_cursor: new_diff_cursor,
            context_name: cur.context_name.clone(),
            result_path_base: cur.result_path_base.clone(),
        };
        let mut sframe = frame.clone();
        sframe.base_limit = s_end;
        sframe.diff_limit = new_diff_limit as isize;
        sframe.profile_name = format!("{}{}", frame.profile_name, path_tail(&row));
        sframe.slicing = SlicingParams {
            done: true,
            element_definition: Some(slicer_element.clone()),
            path: Some(current_base_path.to_string()),
            slices: Vec::new(),
        };
        super::loop_::process_paths(ctx, &mut scur, &sframe, None)?;
    }

    // Remove the synthesized anchor from the differential (PPP:1795-1798).
    if inserted_anchor {
        let anchor_pos = dm_idx[0];
        let mut new_diff = (*ctx.diff).clone();
        new_diff.remove(anchor_pos);
        ctx.diff = Rc::new(new_diff);
        ctx.diff_consumed.remove(anchor_pos);
        ctx.diff_injected.remove(anchor_pos);
        new_diff_limit -= 1;
    }

    // fixedType pruning on the root (PPP:1799-1806).
    if let Some(ft) = &fixed_type {
        if let Some(types) = ctx.output[root_idx]
            .get_mut("type")
            .and_then(Value::as_array_mut)
        {
            types.retain(|tr| tr.get("code").and_then(Value::as_str) == Some(ft.as_str()));
        }
    }

    // Unhandled base slices → fake-diff replay (PPP:1807-1824).
    for bs in base_slices.iter() {
        if bs.handled {
            continue;
        }
        let bs_path = path_of(&cur.base[bs.start]).to_string();
        trace::rec(
            "processPathWithSlicedBaseWhereDiffsConstrainTypes",
            "processPathWithSlicedBaseWhereDiffsConstrainTypes.unhandledBaseSliceFakeDiff",
            trace::id(&cur.base[bs.start]).as_deref(),
            None,
            Some(json!({ "basePath": bs_path })),
        );
        let bs_tail = tail(&bs_path).to_string();
        // Swap in a one-element fake differential {path: bs.path}.
        let saved_diff = ctx.diff.clone();
        let saved_consumed = std::mem::take(&mut ctx.diff_consumed);
        let saved_injected = std::mem::take(&mut ctx.diff_injected);
        ctx.diff = Rc::new(vec![json!({ "path": bs_path })]);
        ctx.diff_consumed = vec![false];
        ctx.diff_injected = vec![false];

        let mut fcur = WalkCursor {
            base_source_url: cur.base_source_url.clone(),
            base: cur.base.clone(),
            base_cursor: bs.start,
            diff_cursor: 0,
            context_name: cur.context_name.clone(),
            result_path_base: cur.result_path_base.clone(),
        };
        let mut fframe = frame.clone();
        fframe.base_limit = bs.end;
        fframe.diff_limit = 0;
        fframe.profile_name = format!("{}{}", frame.profile_name, bs_tail);
        fframe.slicing = SlicingParams {
            done: true,
            element_definition: Some(slicer_element.clone()),
            path: Some(current_base_path.to_string()),
            slices: Vec::new(),
        };
        let replay = super::loop_::process_paths(ctx, &mut fcur, &fframe, None);

        // Restore the real differential state.
        ctx.diff = saved_diff;
        ctx.diff_consumed = saved_consumed;
        ctx.diff_injected = saved_injected;
        replay?;
    }

    // Cursor advance (PPP:1826-1827).
    cur.base_cursor = base_slices
        .last()
        .map(|bs| bs.end + 1)
        .unwrap_or(new_base_limit + 1);
    cur.diff_cursor = new_diff_limit + 1;
    Ok(())
}

/// PU:1727 determineFixedType.
fn determine_fixed_type(ctx: &WalkContext, row: &Value) -> anyhow::Result<String> {
    let types = row
        .get("type")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if types.is_empty() && has_slice_name(row) {
        let n = tail(path_of(row)).replace("[x]", "");
        let slice_name = row.get("sliceName").and_then(Value::as_str).unwrap_or("");
        let t = &slice_name[n.len().min(slice_name.len())..];
        if is_data_type_str(ctx, t) {
            Ok(t.to_string())
        } else if is_primitive_str(ctx, &uncapitalize(t)) {
            Ok(uncapitalize(t))
        } else {
            anyhow::bail!("UNEXPECTED_CONDITION_IN_DIFFERENTIAL: {t}");
        }
    } else if types.len() == 1 {
        Ok(types[0]
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    } else {
        anyhow::bail!(
            "UNEXPECTED_CONDITION_IN_DIFFERENTIAL_TYPESLICETYPELISTSIZE__1 at {}",
            path_of(row)
        );
    }
}

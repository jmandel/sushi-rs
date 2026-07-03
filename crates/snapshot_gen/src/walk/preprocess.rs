//! SnapshotGenerationPreProcessor equivalent (§3.0). The live corpus path is
//! `processSlices` slice-group trailing-property push-down. Implemented lazily:
//! for the fixture ladder no push-down fires (verified via trace parity — the
//! preprocess.* records are absent). The additional-base path (§3.0b) is DEAD
//! under the oracle config. This is a documented gap; extend when a corpus
//! profile requires slice-group property push-down.

use serde_json::{json, Value};

use super::context::WalkContext;
use super::paths::{has_slice_name, has_slicing, path_of};
use super::trace;

const EXT_OBLIGATION_CORE: &str = "http://hl7.org/fhir/StructureDefinition/obligation";
const EXT_OBLIGATION_TOOLS: &str = "http://hl7.org/fhir/tools/StructureDefinition/obligation";
const EXT_OBLIGATION_SOURCE: &str = "http://hl7.org/fhir/tools/StructureDefinition/snapshot-source";

/// SnapshotGenerationPreProcessor.processSlices (SGPP:693-746): slice-group
/// trailing-property push-down + the final markExtensions pass. Mutates `diff`
/// in place (may insert SNAPSHOT_PREPROCESS_INJECTED rows — returned as a set of
/// indices). The additional-base path (SGPP:137-152) is DEAD under oracle config.
pub(crate) fn process(
    ctx: &mut WalkContext,
    diff: &mut Vec<Value>,
    derived_versioned_url: &str,
) -> anyhow::Result<Vec<bool>> {
    let injected = process_slices(ctx, diff, derived_versioned_url)?;
    Ok(injected)
}

// ---------------- processSlices (SGPP:693) ----------------

/// SliceInfo (SGPP:86-121), realized over stable row tags.
struct SliceInfo {
    parent: Option<usize>,
    path: String,
    closed: bool,
    slicer_tag: u64,
    slice_stuff: Vec<u64>,
    slices: Option<Vec<u64>>,
}

fn si_add(slicings: &mut [SliceInfo], si_idx: usize, tag: u64) {
    if slicings[si_idx].slices.is_none() {
        slicings[si_idx].slice_stuff.push(tag);
    }
    if let Some(parent) = slicings[si_idx].parent {
        si_add(slicings, parent, tag);
    }
}

fn si_new_slice(slicings: &mut [SliceInfo], si_idx: usize, tag: u64) {
    slicings[si_idx].slices.get_or_insert_with(Vec::new).push(tag);
    if let Some(parent) = slicings[si_idx].parent {
        si_add(slicings, parent, tag);
    }
}

/// SGPP:1102 getSlicing.
fn get_slicing(slicings: &mut [SliceInfo], ed_path: &str) -> Option<usize> {
    for i in (0..slicings.len()).rev() {
        if !slicings[i].closed {
            if slicings[i].path.len() > ed_path.len() {
                slicings[i].closed = true;
            } else if ed_path.starts_with(&slicings[i].path) {
                return Some(i);
            }
        }
    }
    None
}

/// SGPP:1091 isExtensionSlicing.
fn is_extension_slicing(ed: &Value) -> bool {
    let name = {
        let p = path_of(ed);
        match p.rfind('.') {
            Some(i) => &p[i + 1..],
            None => p,
        }
    };
    // NB: "modiferExtension" typo is verbatim Java (SGPP:1092).
    if name != "extension" && name != "modiferExtension" {
        return false;
    }
    let Some(slicing) = ed.get("slicing") else { return false };
    if slicing.get("rules").and_then(Value::as_str) != Some("open") {
        return false;
    }
    // (!hasOrdered || ordered) fails => require ordered present AND false.
    if slicing.get("ordered").and_then(Value::as_bool) != Some(false) {
        return false;
    }
    let Some(discs) = slicing.get("discriminator").and_then(Value::as_array) else {
        return false;
    };
    discs.len() == 1
        && discs[0].get("type").and_then(Value::as_str) == Some("value")
        && discs[0].get("path").and_then(Value::as_str) == Some("url")
}

const TAG: &str = "__pp_tag";

fn tag_of(ed: &Value) -> u64 {
    ed.get(TAG).and_then(Value::as_u64).unwrap_or(u64::MAX)
}

fn pos_of(elements: &[Value], tag: u64) -> Option<usize> {
    elements.iter().position(|e| tag_of(e) == tag)
}

fn process_slices(
    ctx: &mut WalkContext,
    diff: &mut Vec<Value>,
    derived_versioned_url: &str,
) -> anyhow::Result<Vec<bool>> {
    // Tag rows for stable identity across insertions.
    for (i, ed) in diff.iter_mut().enumerate() {
        if let Some(obj) = ed.as_object_mut() {
            obj.insert(TAG.to_string(), Value::from(i as u64));
        }
    }
    let mut next_tag = diff.len() as u64;
    let mut injected_tags: Vec<u64> = Vec::new();

    let strip = |diff: &mut Vec<Value>, injected_tags: &[u64]| -> Vec<bool> {
        let mut injected = vec![false; diff.len()];
        for (i, ed) in diff.iter_mut().enumerate() {
            if injected_tags.contains(&tag_of(ed)) {
                injected[i] = true;
            }
            if let Some(obj) = ed.as_object_mut() {
                obj.remove(TAG);
            }
        }
        injected
    };

    // First pass (SGPP:695-716).
    let mut slicings: Vec<SliceInfo> = Vec::new();
    for cursor in 0..diff.len() {
        let ed = &diff[cursor];
        let tag = tag_of(ed);
        let si = get_slicing(&mut slicings, path_of(ed));
        match si {
            None => {
                if has_slicing(ed) && !is_extension_slicing(ed) {
                    slicings.push(SliceInfo {
                        parent: None,
                        path: path_of(ed).to_string(),
                        closed: false,
                        slicer_tag: tag,
                        slice_stuff: Vec::new(),
                        slices: None,
                    });
                }
            }
            Some(si_idx) => {
                if has_slice_name(ed) && path_of(ed) == slicings[si_idx].path {
                    si_new_slice(&mut slicings, si_idx, tag);
                } else if has_slicing(ed) && !is_extension_slicing(ed) {
                    si_add(&mut slicings, si_idx, tag);
                    slicings.push(SliceInfo {
                        parent: Some(si_idx),
                        path: path_of(ed).to_string(),
                        closed: false,
                        slicer_tag: tag,
                        slice_stuff: Vec::new(),
                        slices: None,
                    });
                } else {
                    si_add(&mut slicings, si_idx, tag);
                }
            }
        }
    }

    // Complexity guard (SGPP:718-728): early return skips push-down AND the
    // markExtensions pass (Java `return` at SGPP:724).
    for si in &slicings {
        if !si.slice_stuff.is_empty() && si.slices.is_some() {
            for &tag in &si.slice_stuff {
                if let Some(pos) = pos_of(diff, tag) {
                    let ed = &diff[pos];
                    if has_slicing(ed) && !is_extension_slicing(ed) {
                        let injected = strip(diff, &injected_tags);
                        return Ok(injected);
                    }
                }
            }
        }
    }

    // Backward pass (SGPP:731-741).
    for i in (0..slicings.len()).rev() {
        if !slicings[i].slice_stuff.is_empty() && slicings[i].slices.is_some() {
            let stuff = slicings[i].slice_stuff.clone();
            let slices = slicings[i].slices.clone().unwrap();
            let slicer_tag = slicings[i].slicer_tag;
            for slice_tag in slices {
                merge_elements(ctx, diff, &stuff, slice_tag, slicer_tag, &mut next_tag, &mut injected_tags)?;
            }
        }
    }

    // markExtensions pass (SGPP:743-745).
    for ed in diff.iter_mut() {
        mark_extensions(ed, derived_versioned_url);
    }

    let injected = strip(diff, &injected_tags);
    Ok(injected)
}

/// SGPP:992 findEndOfSlice.
fn find_end_of_slice(elements: &[Value], slice_pos: usize) -> usize {
    let slice = &elements[slice_pos];
    let slice_path = path_of(slice).to_string();
    let slice_name = slice.get("sliceName").and_then(Value::as_str).unwrap_or("");
    let dot = format!("{slice_path}.");
    for i in slice_pos..elements.len() {
        let e = &elements[i];
        let same_slice = path_of(e) == slice_path
            && e.get("sliceName").and_then(Value::as_str) == Some(slice_name);
        if !(path_of(e).starts_with(&dot) || same_slice) {
            return i - 1;
        }
    }
    elements.len() - 1
}

/// SGPP:822 elementsMatch.
fn elements_match(ed1: &Value, ed2: &Value) -> bool {
    if !paths_match_pp(path_of(ed1), path_of(ed2)) {
        return false;
    }
    let s1 = ed1.get("sliceName").and_then(Value::as_str);
    let s2 = ed2.get("sliceName").and_then(Value::as_str);
    match (s1, s2) {
        (Some(a), Some(b)) => a == b,
        (None, None) => true,
        _ => false,
    }
}

/// SGPP:834 pathsMatch ([x]-aware).
fn paths_match_pp(path1: &str, path2: &str) -> bool {
    if path1 == path2 {
        return true;
    }
    if let Some(stem) = path1.strip_suffix("[x]") {
        if path2.starts_with(stem) && !path2[stem.len()..].contains('.') {
            return true;
        }
    }
    if let Some(stem) = path2.strip_suffix("[x]") {
        if path1.starts_with(stem) && !path1[stem.len()..].contains('.') {
            return true;
        }
    }
    false
}

/// SGPP:748 mergeElements.
#[allow(clippy::too_many_arguments)]
fn merge_elements(
    ctx: &mut WalkContext,
    elements: &mut Vec<Value>,
    all_slices: &[u64],
    slice_tag: u64,
    slicer_tag: u64,
    next_tag: &mut u64,
    injected_tags: &mut Vec<u64>,
) -> anyhow::Result<()> {
    let Some(slice_pos) = pos_of(elements, slice_tag) else { return Ok(()) };
    let start_of_slice = slice_pos + 1;
    let mut end_of_slice = find_end_of_slice(elements, slice_pos);

    // Which sliceStuff rows are present in the slice?
    let mut handled: Vec<u64> = Vec::new();
    for &stuff_tag in all_slices {
        let Some(stuff_pos) = pos_of(elements, stuff_tag) else { continue };
        let stuff = elements[stuff_pos].clone();
        for j in start_of_slice..=end_of_slice.min(elements.len().saturating_sub(1)) {
            if elements_match(&elements[j], &stuff) {
                handled.push(stuff_tag);
                trace::rec(
                    "SnapshotGenerationPreProcessor",
                    "preprocess.merge.setField",
                    trace::id(&stuff).as_deref(),
                    trace::id(&elements[j]).as_deref(),
                    None,
                );
                merge_fill_missing(&mut elements[j], &stuff);
            }
        }
    }

    // Inject the missing ones (SGPP:801-817).
    let slicer_id = elements[pos_of(elements, slicer_tag).unwrap_or(slice_pos)]
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let slice_id = elements[pos_of(elements, slice_tag).unwrap_or(slice_pos)]
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    for &stuff_tag in all_slices {
        if handled.contains(&stuff_tag) {
            continue;
        }
        let Some(stuff_pos) = pos_of(elements, stuff_tag) else { continue };
        let stuff = elements[stuff_pos].clone();
        let ed_def = analyse_path(ctx, &stuff)?;
        let source_id = stuff.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        let id = source_id.replace(&slicer_id, &slice_id);
        let index = determine_insertion_point(
            elements,
            start_of_slice,
            end_of_slice,
            &id,
            &ed_def,
        );
        let mut edc = stuff.clone();
        if let Some(obj) = edc.as_object_mut() {
            obj.insert("id".to_string(), Value::String(id.clone()));
            obj.insert(TAG.to_string(), Value::from(*next_tag));
        }
        injected_tags.push(*next_tag);
        *next_tag += 1;
        trace::rec(
            "SnapshotGenerationPreProcessor",
            "preprocess.mergeElements.insert",
            None,
            Some(&id),
            Some(json!({ "index": index, "path": path_of(&stuff), "sourceId": source_id })),
        );
        elements.insert(index, edc);
        end_of_slice += 1;
    }
    Ok(())
}

/// SGPP:857 determineInsertionPoint.
fn determine_insertion_point(
    elements: &[Value],
    start_of_slice: usize,
    end_of_slice: usize,
    id: &str,
    ed_def: &[Analysis],
) -> usize {
    let p: Vec<&str> = id.split('.').collect();
    for i in (1..p.len()).rev() {
        let sub_id = p[..=i].join(".");
        let peers: Vec<usize> = (start_of_slice..=end_of_slice.min(elements.len().saturating_sub(1)))
            .filter(|&j| {
                elements[j]
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|eid| eid.starts_with(&sub_id))
                    .unwrap_or(false)
            })
            .collect();
        if !peers.is_empty() {
            for &peer in &peers {
                if comes_after_this(id, ed_def, &elements[peer]) {
                    return peer;
                }
            }
            return peers[peers.len() - 1] + 1;
        }
    }
    end_of_slice + 1
}

/// SGPP:901 comesAfterThis.
fn comes_after_this(id: &str, ed_def: &[Analysis], peer: &Value) -> bool {
    let p1: Vec<&str> = id.split('.').collect();
    let peer_id = peer.get("id").and_then(Value::as_str).unwrap_or("");
    let p2: Vec<&str> = peer_id.split('.').collect();
    let min = p1.len().min(p2.len());
    for i in 0..min {
        if p1[i] != p2[i] {
            let Some(sed) = ed_def.get(i.wrapping_sub(1)) else { return false };
            let i1 = index_of_name(sed, p1[i]);
            let i2 = index_of_name(sed, p2[i]);
            if i == min - 1 && i1 == i2 && !p1[i].contains(':') && p2[i].contains(':') {
                return true;
            }
            return i1 < i2;
        }
    }
    p1.len() < p2.len()
}

fn index_of_name(sed: &Analysis, name: &str) -> i64 {
    let name = match name.find(':') {
        Some(i) => &name[..i],
        None => name,
    };
    for (i, child) in sed.children.iter().enumerate() {
        if child == name {
            return i as i64;
        }
    }
    -1
}

/// ElementAnalysis-lite: per path segment, the ordered child names of that node
/// (utils.getChildMap semantics: children within the same SD; if none, walk into
/// the (single/overridden) type SD; follow contentReference).
pub(crate) struct Analysis {
    children: Vec<String>,
}

fn analyse_path(ctx: &mut WalkContext, ed: &Value) -> anyhow::Result<Vec<Analysis>> {
    let path = path_of(ed).to_string();
    let segments: Vec<&str> = path.split('.').collect();
    let mut res: Vec<Analysis> = Vec::new();
    // Current position: SD elements + element path within it (+ type override).
    let mut cur_sd: Option<std::rc::Rc<Value>> = None;
    let mut cur_path = String::new();
    let mut cur_type_override: Option<String> = None;

    for pn in segments {
        if cur_sd.is_none() {
            let Some(sd) = super::resolve::resolve_with_snapshot(ctx, pn)? else {
                anyhow::bail!("UNKNOWN_TYPE__AT_: {pn} at {}", path_of(ed));
            };
            cur_path = pn.to_string();
            cur_sd = Some(sd);
            res.push(Analysis { children: Vec::new() });
            continue;
        }
        // compute children of current node
        let (children, child_source) =
            children_of(ctx, cur_sd.clone().unwrap(), &cur_path, cur_type_override.as_deref())?;
        // find the child named pn (or [x] stem)
        let mut found: Option<(String, Option<String>)> = None;
        for (name, _p) in &children {
            if name == pn {
                found = Some((name.clone(), None));
                break;
            }
            if let Some(rn) = name.strip_suffix("[x]") {
                if pn.starts_with(rn) {
                    let tn = &pn[rn.len()..];
                    let uncap = {
                        let mut c = tn.chars();
                        match c.next() {
                            Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
                            None => String::new(),
                        }
                    };
                    let type_override = if super::types_pred::is_primitive_str(ctx, &uncap) {
                        uncap
                    } else {
                        tn.to_string()
                    };
                    found = Some((name.clone(), Some(type_override)));
                    break;
                }
            }
        }
        // record analysis (children of the PREVIOUS node) — matches Java where
        // analysePathSegment computes children of res.last() then pushes.
        if let Some(last) = res.last_mut() {
            last.children = children.iter().map(|(n, _)| n.clone()).collect();
        }
        let Some((child_name, type_override)) = found else {
            anyhow::bail!("UNKNOWN_PROPERTY: {pn} in {}", path_of(ed));
        };
        let (src_sd, src_path) = child_source;
        cur_path = format!("{src_path}.{child_name}");
        cur_sd = Some(src_sd);
        cur_type_override = type_override;
        res.push(Analysis { children: Vec::new() });
    }
    // fill children for the last node too (used by indexOfName at the leaf level)
    if let (Some(sd), Some(last)) = (cur_sd.clone(), res.last_mut()) {
        if last.children.is_empty() {
            if let Ok((children, _)) = children_of(ctx, sd, &cur_path, cur_type_override.as_deref())
            {
                last.children = children.into_iter().map(|(n, _)| n).collect();
            }
        }
    }
    Ok(res)
}

/// getChildMap-lite: ordered (name, path) children of `element_path` in `sd`; if
/// none, walk into the element's (overridden/single) type SD or contentReference.
#[allow(clippy::type_complexity)]
fn children_of(
    ctx: &mut WalkContext,
    sd: std::rc::Rc<Value>,
    element_path: &str,
    type_override: Option<&str>,
) -> anyhow::Result<(Vec<(String, String)>, (std::rc::Rc<Value>, String))> {
    let elements = sd
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // direct children in this SD (depth+1 only, skipping slices of the element itself)
    let dot = format!("{element_path}.");
    let depth = element_path.matches('.').count() + 1;
    let mut children: Vec<(String, String)> = Vec::new();
    let mut content_ref: Option<String> = None;
    for e in &elements {
        let p = path_of(e);
        if p == element_path && e.get("contentReference").is_some() {
            content_ref = e
                .get("contentReference")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if p.starts_with(&dot) && p.matches('.').count() == depth && !has_slice_name(e) {
            let name = p[p.rfind('.').map(|i| i + 1).unwrap_or(0)..].to_string();
            if !children.iter().any(|(n, _)| n == &name) {
                children.push((name, p.to_string()));
            }
        }
    }
    if !children.is_empty() {
        return Ok((children, (sd.clone(), element_path.to_string())));
    }
    // contentReference within this SD
    if let Some(cr) = content_ref {
        let frag = cr[cr.find('#').map(|i| i + 1).unwrap_or(0)..].to_string();
        return children_of(ctx, sd, &frag, None);
    }
    // type walk
    let type_name = match type_override {
        Some(t) => Some(t.to_string()),
        None => {
            let el = elements.iter().find(|e| path_of(e) == element_path);
            el.and_then(|e| {
                let types = e.get("type").and_then(Value::as_array)?;
                if types.len() == 1 {
                    super::types_pred::working_code(&types[0])
                } else {
                    None
                }
            })
        }
    };
    let Some(tn) = type_name else {
        return Ok((Vec::new(), (sd, element_path.to_string())));
    };
    let Some(tsd) = super::resolve::resolve_with_snapshot(ctx, &tn)? else {
        return Ok((Vec::new(), (sd, element_path.to_string())));
    };
    let root_path = tsd
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .map(|e| path_of(e).to_string())
        .unwrap_or(tn.clone());
    children_of(ctx, tsd, &root_path, None)
}

/// SGPP:1003 merge — fill-missing-only field copy.
fn merge_fill_missing(focus: &mut Value, base: &Value) {
    let simple_fields = [
        "label", "short", "definition", "comment", "requirements", "min", "max",
        "meaningWhenMissing", "orderMeaning", "maxLength", "mustHaveValue", "mustSupport",
        "isModifier", "isModifierReason", "isSummary", "binding",
    ];
    for key in simple_fields {
        if base.get(key).is_some() && focus.get(key).is_none() {
            if let Some(v) = base.get(key) {
                crate::merge::set_field(focus, key, v.clone());
            }
        }
    }
    let array_fields = ["code", "alias", "type", "example", "constraint", "valueAlternatives"];
    for key in array_fields {
        if base.get(key).is_some() && focus.get(key).is_none() {
            if let Some(v) = base.get(key) {
                crate::merge::set_field(focus, key, v.clone());
            }
        }
    }
    let choice_prefixes = ["defaultValue", "fixed", "pattern", "minValue", "maxValue"];
    for prefix in choice_prefixes {
        let base_has = base
            .as_object()
            .map(|o| o.keys().any(|k| k.starts_with(prefix)))
            .unwrap_or(false);
        let focus_has = focus
            .as_object()
            .map(|o| o.keys().any(|k| k.starts_with(prefix)))
            .unwrap_or(false);
        if base_has && !focus_has {
            if let Some(base_obj) = base.as_object() {
                let entries: Vec<(String, Value)> = base_obj
                    .iter()
                    .filter(|(k, _)| k.starts_with(prefix))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                for (k, v) in entries {
                    crate::merge::set_field(focus, &k, v);
                }
            }
        }
    }
}

/// PU:5040 markExtensions (visible effects only): element extensions, binding
/// extensions, and type extensions each get the obligation snapshot-source
/// stamp when they are obligation extensions without one.
pub(crate) fn mark_extensions(ed: &mut Value, versioned_url: &str) {
    if let Some(exts) = ed.get_mut("extension").and_then(Value::as_array_mut) {
        for ext in exts {
            mark_extension_source(ext, versioned_url);
        }
    }
    if let Some(exts) = ed
        .get_mut("binding")
        .and_then(|b| b.get_mut("extension"))
        .and_then(Value::as_array_mut)
    {
        for ext in exts {
            mark_extension_source(ext, versioned_url);
        }
    }
    if let Some(types) = ed.get_mut("type").and_then(Value::as_array_mut) {
        for tr in types {
            if let Some(exts) = tr.get_mut("extension").and_then(Value::as_array_mut) {
                for ext in exts {
                    mark_extension_source(ext, versioned_url);
                }
            }
        }
    }
}

/// PU:3215 markExtensionSource, JSON-visible part: obligation extensions get an
/// appended `snapshot-source` sub-extension when none exists.
fn mark_extension_source(ext: &mut Value, versioned_url: &str) {
    let url = ext.get("url").and_then(Value::as_str).unwrap_or("");
    if url != EXT_OBLIGATION_CORE && url != EXT_OBLIGATION_TOOLS {
        return;
    }
    let has_sub = ext
        .get("extension")
        .and_then(Value::as_array)
        .map(|subs| {
            subs.iter().any(|s| {
                matches!(
                    s.get("url").and_then(Value::as_str),
                    Some(EXT_OBLIGATION_SOURCE) | Some("source")
                )
            })
        })
        .unwrap_or(false);
    if has_sub {
        return;
    }
    let Some(obj) = ext.as_object_mut() else { return };
    let subs = obj
        .entry("extension".to_string())
        .or_insert_with(|| Value::Array(vec![]));
    if let Some(arr) = subs.as_array_mut() {
        arr.push(json!({ "url": EXT_OBLIGATION_SOURCE, "valueCanonical": versioned_url }));
    }
}

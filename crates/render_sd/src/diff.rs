//! The diff-view element list: `supplementMissingDiffElements` (r5
//! SnapshotGenerationPreProcessor.java:1102-1181), a PURE function of the
//! profile's differential (it sets NO userData).
//!
//! Steps (SGPP:1102):
//! 1. list = copy(differential.element)
//! 2. if empty: single root {path=typeName, id=typeName}
//!    else if list[0].path contains ".": prepend {path=typeName, id=typeName}
//! 3. insertMissingSparseElements(list, typeName): backfill any missing
//!    intermediate path nodes so every element's parent exists in the list.
//!
//! The diff render reads each element's `SNAPSHOT_DERIVATION_POINTER` (the base
//! element it was merged into during snapshot generation) to (a) fill missing
//! cardinality/type/short from the base and (b) dim (`opacity: 0.5`) values that
//! came from the base rather than the differential. Because our input is JSON
//! (no in-memory userData), we RECONSTRUCT the pointer as the element in the
//! profile's OWN snapshot whose id equals the diff element's id — for any field
//! the diff did not restate, `snapshot[id].field == base[id].field`, so the
//! own-snapshot element reproduces the base text/cardinality the publisher dimmed.

use crate::sdmodel::Sd;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Reconstruct the `SNAPSHOT_DERIVATION_POINTER` for every DIFFERENTIAL element
/// (PU:2591: derived.setUserData(POINTER, base), base = the base clone that
/// becomes the output snapshot element). Returns `diff element id -> index into
/// sd.snapshot_elements()`. Same aliasing the diff table path uses
/// (render_table): exact id, then the `base[x]:baseType`->`baseType` sliced-
/// choice alias, then the unsliced camelCase `stem[x]` rewrite.
pub fn reconstruct_diff_pointers(sd: &Sd) -> HashMap<String, usize> {
    let snap = sd.snapshot_elements();
    let mut exact: HashMap<&str, usize> = HashMap::new();
    let mut alias: HashMap<String, usize> = HashMap::new();
    for (i, e) in snap.iter().enumerate() {
        exact.insert(e.id(), i);
        let mut changed = false;
        let alias_id: Vec<String> = e
            .id()
            .split('.')
            .map(|seg| {
                if let Some((l, r)) = seg.split_once("[x]:") {
                    if r.starts_with(l) {
                        changed = true;
                        return r.to_string();
                    }
                }
                seg.to_string()
            })
            .collect();
        if changed {
            alias.insert(alias_id.join("."), i);
        }
    }
    let mut map: HashMap<String, usize> = HashMap::new();
    for d in sd.differential_elements() {
        let id = d.id();
        if let Some(i) = exact.get(id).or_else(|| alias.get(id)) {
            map.insert(id.to_string(), *i);
            continue;
        }
        for cand in crate::table::dechoice_candidates_pub(id) {
            if let Some(i) = exact.get(cand.as_str()) {
                map.insert(id.to_string(), *i);
                break;
            }
        }
    }
    map
}

/// Build the diff-view element list (owned JSON values). Order and synthetic
/// path/id fill match SGPP exactly.
pub fn supplement_missing_diff_elements(sd: &Sd) -> Vec<Value> {
    let type_name = sd.type_name();
    let mut list: Vec<Value> = sd
        .differential_elements()
        .iter()
        .map(|e| e.v.clone())
        .collect();

    if list.is_empty() {
        list.push(json!({ "path": type_name, "id": type_name }));
    } else if path_of(&list[0]).contains('.') {
        list.insert(0, json!({ "path": type_name, "id": type_name }));
    }
    insert_missing_sparse_elements(&mut list, type_name);
    list
}

fn path_of(e: &Value) -> &str {
    e.get("path").and_then(|x| x.as_str()).unwrap_or("")
}
fn id_of(e: &Value) -> &str {
    e.get("id").and_then(|x| x.as_str()).unwrap_or("")
}

/// `insertMissingSparseElements` (SGPP:1120-1156).
fn insert_missing_sparse_elements(list: &mut Vec<Value>, type_name: &str) {
    if list.is_empty() || path_of(&list[0]).contains('.') {
        list.insert(0, json!({ "path": type_name }));
    }
    let mut i = 1usize;
    while i < list.len() {
        let path_current: Vec<String> = path_of(&list[i]).split('.').map(String::from).collect();
        let path_last: Vec<String> = path_of(&list[i - 1]).split('.').map(String::from).collect();
        let mut first_diff = 0usize;
        while first_diff < path_current.len()
            && first_diff < path_last.len()
            && path_current[first_diff] == path_last[first_diff]
        {
            first_diff += 1;
        }
        if !(is_sibling(&path_current, &path_last, first_diff)
            || is_child(&path_current, &path_last, first_diff))
        {
            // findParent(list, i, list[i].path)
            let (parent_path, parent_id) = find_parent(list, i, path_of(&list[i]));
            let parent_depth = char_count(&parent_path, '.') + 1;
            let child_depth = char_count(path_of(&list[i]), '.') + 1;
            if child_depth > parent_depth + 1 {
                let base_path = parent_path;
                let base_id = parent_id;
                // for index = parentDepth down to firstDiff (inclusive), insert at i.
                let mut index = parent_depth as isize;
                while index >= first_diff as isize {
                    let mtail = make_tail(&path_current, parent_depth, index as usize);
                    let root = json!({
                        "path": format!("{}.{}", base_path, mtail),
                        "id": format!("{}.{}", base_id, mtail),
                    });
                    list.insert(i, root);
                    index -= 1;
                }
            }
        }
        i += 1;
    }
}

/// `findParent(list, i, path)` (SGPP:1159): walk `i` down while
/// `!path.startsWith(list[i].path + ".")`; return (path, id) of the stop node.
fn find_parent(list: &[Value], mut i: usize, path: &str) -> (String, String) {
    while i > 0 && !path.starts_with(&format!("{}.", path_of(&list[i]))) {
        i -= 1;
    }
    (path_of(&list[i]).to_string(), id_of(&list[i]).to_string())
}

fn is_sibling(pc: &[String], pl: &[String], first_diff: usize) -> bool {
    pc.len() == pl.len() && first_diff == pc.len().saturating_sub(1)
}
fn is_child(pc: &[String], pl: &[String], first_diff: usize) -> bool {
    pc.len() == pl.len() + 1 && first_diff == pl.len()
}
fn make_tail(pc: &[String], start: usize, index: usize) -> String {
    let mut parts = Vec::new();
    let mut i = start;
    while i <= index && i < pc.len() {
        parts.push(pc[i].clone());
        i += 1;
    }
    parts.join(".")
}
fn char_count(s: &str, c: char) -> usize {
    s.chars().filter(|&x| x == c).count()
}

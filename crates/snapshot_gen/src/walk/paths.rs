//! Pure path/window helpers — exact ports of the `ProfileUtilities` string
//! helpers used by the walk loop. Citations against fhir-core `snap-trace`
//! (base `5c4d5a0ff`), file ProfileUtilities.java unless noted.

use serde_json::Value;

use super::frame::ElementRedirection;

pub(crate) fn path_of(ed: &Value) -> &str {
    ed.get("path").and_then(Value::as_str).unwrap_or("")
}

pub(crate) fn has_slice_name(ed: &Value) -> bool {
    ed.get("sliceName").and_then(Value::as_str).is_some()
}

pub(crate) fn has_slicing(ed: &Value) -> bool {
    ed.get("slicing").is_some()
}

/// PU:2052 fixedPathSource.
pub(crate) fn fixed_path_source(
    context_path: Option<&str>,
    p: &str,
    redirector: &[ElementRedirection],
) -> String {
    let Some(context_path) = context_path else {
        return p.to_string();
    };
    if let Some(last) = redirector.last() {
        let ptail = if context_path.len() >= p.len() {
            &p[p.find('.').map(|i| i + 1).unwrap_or(0)..]
        } else {
            &p[context_path.len() + 1..]
        };
        format!("{}.{}", last.path, ptail)
    } else {
        let ptail = &p[p.find('.').map(|i| i + 1).unwrap_or(0)..];
        format!("{context_path}.{ptail}")
    }
}

/// PU:2071 fixedPathDest.
pub(crate) fn fixed_path_dest(
    context_path: Option<&str>,
    p: &str,
    redirector: &[ElementRedirection],
    redirect_source: Option<&str>,
) -> String {
    let Some(context_path) = context_path else {
        return p.to_string();
    };
    if !redirector.is_empty() {
        let redirect_source = redirect_source.unwrap_or("");
        let ptail = if redirect_source.len() >= p.len() {
            &p[p.find('.').map(|i| i + 1).unwrap_or(0)..]
        } else {
            &p[redirect_source.len() + 1..]
        };
        format!("{context_path}.{ptail}")
    } else {
        let ptail = &p[p.find('.').map(|i| i + 1).unwrap_or(0)..];
        format!("{context_path}.{ptail}")
    }
}

/// PU:2043 pathStartsWith.
pub(crate) fn path_starts_with(p1: &str, p2: &str) -> bool {
    p1.starts_with(p2) || (p2.ends_with("[x].") && p1.starts_with(&p2[..p2.len() - 4]))
}

/// PU:2507 isSameBase.
pub(crate) fn is_same_base(p: &str, sp: &str) -> bool {
    (p.ends_with("[x]") && sp.starts_with(&p[..p.len() - 3]))
        || (sp.ends_with("[x]") && p.starts_with(&sp[..sp.len() - 3]))
}

/// PU:2464 getDiffMatches — returns indices (into `diff`) in `[start,end]` that
/// path-match `path` at the same segment depth.
pub(crate) fn get_diff_matches(diff: &[Value], path: &str, start: usize, end: isize) -> Vec<usize> {
    let mut result = Vec::new();
    if end < 0 {
        return result;
    }
    let end = end as usize;
    let p: Vec<&str> = path.split('.').collect();
    for i in start..=end.min(diff.len().saturating_sub(1)) {
        let stated = path_of(&diff[i]);
        let sp: Vec<&str> = stated.split('.').collect();
        let mut ok = sp.len() == p.len();
        for j in 0..p.len() {
            ok = ok && sp.len() > j && (p[j] == sp[j] || is_same_base(p[j], sp[j]));
        }
        if ok {
            result.push(i);
        }
    }
    result
}

/// PU:2440 hasInnerDiffMatches — is some diff row in `[start,end]` a strict
/// descendant of `path`?
pub(crate) fn has_inner_diff_matches(
    diff: &[Value],
    path: &str,
    start: usize,
    end: isize,
    allow_slices: bool,
) -> bool {
    if diff.is_empty() {
        return false;
    }
    let end = (end.min(diff.len() as isize)).max(-1);
    if end < 0 {
        return false;
    }
    let end = end as usize;
    let start = start.max(0);
    let dot = format!("{path}.");
    for i in start..=end.min(diff.len().saturating_sub(1)) {
        let stated = path_of(&diff[i]);
        if !allow_slices && stated == path && has_slice_name(&diff[i]) {
            return false;
        } else if stated.starts_with(&dot) {
            return true;
        } else if path.ends_with("[x]") && stated.starts_with(&path[..path.len() - 3]) {
            return true;
        } else if i != start && !allow_slices && !stated.starts_with(&dot) {
            return false;
        } else if i != start && allow_slices && !stated.starts_with(path) {
            return false;
        }
    }
    false
}

/// PU:2511/2521 findEndOfElement — last index whose path startsWith list[cursor].path+"."
pub(crate) fn find_end_of_element(list: &[Value], cursor: usize) -> usize {
    let mut result = cursor;
    if cursor >= list.len() {
        return result;
    }
    let dot = format!("{}.", path_of(&list[cursor]));
    while result < list.len() - 1 && path_of(&list[result + 1]).starts_with(&dot) {
        result += 1;
    }
    result
}

/// PU:2529 findEndOfElementNoSlices.
pub(crate) fn find_end_of_element_no_slices(list: &[Value], cursor: usize) -> usize {
    let mut result = cursor;
    if cursor >= list.len() {
        return result;
    }
    let dot = format!("{}.", path_of(&list[cursor]));
    while result < list.len() - 1
        && path_of(&list[result + 1]).starts_with(&dot)
        && !has_slice_name(&list[result + 1])
    {
        result += 1;
    }
    result
}

/// PPP:1157 isChildOf.
pub(crate) fn is_child_of(sub: &str, focus: &str) -> bool {
    if let Some(stem) = focus.strip_suffix("[x]") {
        sub.starts_with(stem)
    } else {
        sub.starts_with(&format!("{focus}."))
    }
}

/// PPP:1148 baseHasChildren — the base element at `index` is followed by a child.
pub(crate) fn base_has_children(base: &[Value], index: usize) -> bool {
    if index + 1 >= base.len() {
        return false;
    }
    is_child_of(path_of(&base[index + 1]), path_of(&base[index]))
}

/// PU:2537 unbounded.
pub(crate) fn unbounded(ed: &Value) -> bool {
    match ed.get("max").and_then(Value::as_str) {
        Some("1") | Some("0") | None => false,
        Some(_) => true,
    }
}

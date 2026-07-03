//! Faithful port of Java `sortDifferential` (PU:3844) + ElementDefinitionHolder
//! (PU:3701) + ElementDefinitionComparer (PU:3749) + processElementsIntoTree
//! (PU:3917) + sortElements (PU:3939) + getComparer (PU:3959) + writeElements +
//! compareDiffs (PU:3900). Replaces the earlier ad-hoc ancestor-index sort.
//!
//! Live error outputs under oracle config (debug=false): compareDiffs
//! "out of order" / size-changed messages and the final "Sort failed: counts
//! differ" (PU:3897). The comparer's internal `find` errors are only surfaced
//! under debug (PU:3945-3947) and are therefore not collected.

use serde_json::Value;
use std::rc::Rc;

use super::context::WalkContext;
use super::paths::path_of;
use super::resolve::resolve_with_snapshot;
use super::types_pred::working_code;

const MAX_RECURSION_LIMIT: usize = 10;

struct Holder {
    self_idx: Option<usize>, // index into the original diff list; None = placeholder
    path: String,
    base_index: usize,
    base_index_set: bool,
    children: Vec<Holder>,
}

impl Holder {
    fn placeholder(path: String) -> Holder {
        Holder {
            self_idx: None,
            path,
            base_index: 0,
            base_index_set: false,
            children: Vec::new(),
        }
    }
    fn real(idx: usize, path: String) -> Holder {
        Holder {
            self_idx: Some(idx),
            path,
            base_index: 0,
            base_index_set: false,
            children: Vec::new(),
        }
    }
}

/// The comparer state (PU:3749). `snapshot` is the SD element list sibling order
/// is checked against; `base` + `prefix_length` re-anchor diff paths into it.
struct Comparer {
    src_url: String,
    snapshot: Rc<Vec<Value>>,
    prefix_length: usize,
    base: String,
}

fn url_tail(profile: &str) -> &str {
    match profile.rfind('/') {
        Some(i) => &profile[i + 1..],
        None => profile,
    }
}

impl Comparer {
    fn new(src_url: String, snapshot: Rc<Vec<Value>>, base: &str, prefix_length: usize) -> Comparer {
        let base = if base.contains("://") || base.starts_with("http:") || base.starts_with("urn:") {
            url_tail(base).to_string()
        } else {
            base.to_string()
        };
        Comparer {
            src_url,
            snapshot,
            prefix_length,
            base,
        }
    }

    /// PU:3784 find — locate `path` in `snapshot`, following contentReference
    /// re-anchoring. Returns 0 when not found (Java records an error only under
    /// debug).
    fn find(&self, path: &str) -> anyhow::Result<usize> {
        let op = path.to_string();
        let mut path = path.to_string();
        let mut lc = 0usize;
        let mut actual = format!(
            "{}{}",
            self.base,
            &path[self.prefix_length.min(path.len())..]
        );
        let mut i = 0usize;
        while i < self.snapshot.len() {
            let p = path_of(&self.snapshot[i]);
            if p == actual {
                return Ok(i);
            }
            if p.ends_with("[x]")
                && actual.starts_with(&p[..p.len() - 3])
                && !actual.ends_with("[x]")
                && actual.len() >= p.len() - 3
                && !actual[p.len() - 3..].contains('.')
            {
                return Ok(i);
            }
            if actual.ends_with("[x]")
                && p.starts_with(&actual[..actual.len() - 3])
                && p.len() >= actual.len() - 3
                && !p[actual.len() - 3..].contains('.')
            {
                return Ok(i);
            }
            let p_dot = format!("{p}.");
            if path.starts_with(&p_dot) && self.snapshot[i].get("contentReference").is_some() {
                let ref_ = self.snapshot[i]
                    .get("contentReference")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let tail_after_p = &path[p.len() + 1..];
                if ref_.len() >= 2 && ref_[1..2].chars().all(|c| c.is_uppercase()) {
                    let joined = format!("{}.{}", &ref_[1..], tail_after_p);
                    actual = format!(
                        "{}{}",
                        self.base,
                        &joined[self.prefix_length.min(joined.len())..]
                    );
                    path = actual.clone();
                } else if ref_.starts_with("http:") {
                    let frag = &ref_[ref_.find('#').map(|x| x + 1).unwrap_or(0)..];
                    let joined = format!("{frag}.{tail_after_p}");
                    actual = format!(
                        "{}{}",
                        self.base,
                        &joined[self.prefix_length.min(joined.len())..]
                    );
                    path = actual.clone();
                } else {
                    // #parameter-style (2016May): prepend first segment of path
                    let first_dot = path.find('.').map(|x| x + 1).unwrap_or(0);
                    let joined = format!("{}{}.{}", &path[..first_dot], &ref_[1..], tail_after_p);
                    actual = format!(
                        "{}{}",
                        self.base,
                        &joined[self.prefix_length.min(joined.len())..]
                    );
                    path = actual.clone();
                }
                i = 0; // loop's i++ makes the restart begin at index 1 (Java parity)
                lc += 1;
                if lc > MAX_RECURSION_LIMIT {
                    anyhow::bail!(
                        "Internal recursion detection: find() loop path recursion > {MAX_RECURSION_LIMIT} - check paths are valid (for path {path}/{op})"
                    );
                }
            }
            i += 1;
        }
        Ok(0)
    }
}

/// PU:3844 sortDifferential. Sorts `diff` in place; returns the collected sort
/// errors ("out of order", size-changed, counts-differ).
pub(crate) fn sort_differential(
    ctx: &mut WalkContext,
    base_sd: &Value,
    diff: &mut Vec<Value>,
    _name: &str,
) -> anyhow::Result<Vec<String>> {
    let mut errors: Vec<String> = Vec::new();
    if diff.is_empty() {
        return Ok(errors);
    }
    let original_paths: Vec<String> = diff.iter().map(|e| path_of(e).to_string()).collect();
    let original_ids: Vec<Option<String>> = diff
        .iter()
        .map(|e| e.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let last_count = diff.len();

    // Build the tree.
    let mut root;
    let mut i = 0usize;
    let first_path = path_of(&diff[0]).to_string();
    if first_path.contains('.') {
        let new_path = first_path.split('.').next().unwrap_or("").to_string();
        root = Holder::placeholder(new_path);
    } else {
        root = Holder::real(0, first_path);
        i = 1;
    }
    process_elements_into_tree(&mut root, i, diff);

    // Sort siblings throughout the tree.
    let base_snapshot: Rc<Vec<Value>> = Rc::new(
        base_sd
            .get("snapshot")
            .and_then(|s| s.get("element"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    );
    let base_url = base_sd
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let cmp = Comparer::new(base_url, base_snapshot, "", 0);
    sort_elements(ctx, &mut root, &cmp, diff)?;

    // Serialize back.
    let mut order: Vec<usize> = Vec::new();
    write_elements(&root, &mut order);
    let new_diff: Vec<Value> = order.iter().map(|&idx| diff[idx].clone()).collect();

    // compareDiffs (errorIfChanges=true under oracle).
    if original_paths.len() != new_diff.len() {
        errors.push(format!(
            "The diff list size changed when sorting - was {} is now {}",
            original_paths.len(),
            new_diff.len()
        ));
    } else {
        for (idx, n) in new_diff.iter().enumerate() {
            if path_of(n) != original_paths[idx] {
                let e_desc = original_ids[idx]
                    .clone()
                    .unwrap_or_else(|| original_paths[idx].clone());
                errors.push(format!(
                    "The element {e_desc} @diff[{idx}] is out of order (and maybe others after it)"
                ));
                break;
            }
        }
    }

    *diff = new_diff;
    if last_count != diff.len() {
        errors.push(
            "Sort failed: counts differ; at least one of the paths in the differential is illegal"
                .to_string(),
        );
    }
    Ok(errors)
}

/// PU:3917 processElementsIntoTree.
fn process_elements_into_tree(edh: &mut Holder, mut i: usize, list: &[Value]) -> usize {
    let prefix = format!("{}.", edh.path);
    while i < list.len() && path_of(&list[i]).starts_with(&prefix) {
        let lp = path_of(&list[i]);
        // Java: list.get(i).getPath().substring(prefix.length()+1).contains(".")
        let deeper = lp.len() > prefix.len() + 1 && lp[prefix.len() + 1..].contains('.');
        if deeper {
            let seg = lp[prefix.len()..].split('.').next().unwrap_or("");
            let new_path = format!("{prefix}{seg}");
            let mut child = Holder::placeholder(new_path);
            i = process_elements_into_tree(&mut child, i, list);
            edh.children.push(child);
        } else {
            let mut child = Holder::real(i, lp.to_string());
            i = process_elements_into_tree(&mut child, i + 1, list);
            edh.children.push(child);
        }
    }
    i
}

/// PU:3939 sortElements (recursive).
fn sort_elements(
    ctx: &mut WalkContext,
    edh: &mut Holder,
    cmp: &Comparer,
    diff: &[Value],
) -> anyhow::Result<()> {
    if edh.children.len() == 1 {
        let idx = cmp.find(&edh.children[0].path)?;
        edh.children[0].base_index = idx;
        edh.children[0].base_index_set = true;
    } else {
        // Lazy find(path, true) in Java's compare; precompute for all (stable sort).
        for child in edh.children.iter_mut() {
            if !child.base_index_set {
                child.base_index = cmp.find(&child.path)?;
                child.base_index_set = true;
            }
        }
        edh.children.sort_by_key(|c| c.base_index);
    }

    // Recurse into children that themselves have children.
    let n = edh.children.len();
    for ci in 0..n {
        if edh.children[ci].children.is_empty() {
            continue;
        }
        let ccmp = get_comparer(ctx, cmp, &edh.children[ci], diff)?;
        if let Some(ccmp) = ccmp {
            sort_elements(ctx, &mut edh.children[ci], &ccmp, diff)?;
        }
    }
    Ok(())
}

fn is_abstract(code: &str) -> bool {
    matches!(code, "Element" | "BackboneElement" | "Resource" | "DomainResource")
}

fn child_self_types<'a>(child: &Holder, diff: &'a [Value]) -> Vec<&'a Value> {
    match child.self_idx {
        Some(idx) => diff[idx]
            .get("type")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

fn first_profile_of(tr: &Value) -> Option<String> {
    tr.get("profile")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn sd_ns(type_: &str) -> String {
    if type_.contains("://") {
        type_.to_string()
    } else {
        format!("http://hl7.org/fhir/StructureDefinition/{type_}")
    }
}

fn snapshot_of(sd: &Value) -> Rc<Vec<Value>> {
    Rc::new(
        sd.get("snapshot")
            .and_then(|s| s.get("element"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    )
}

fn resolve_type_name(ctx: &mut WalkContext, code: &str) -> String {
    if code.contains("://") {
        if let Ok(Some(sd)) = resolve_with_snapshot(ctx, code) {
            if let Some(t) = sd.get("type").and_then(Value::as_str) {
                return t.to_string();
            }
        }
    }
    code.to_string()
}

/// PU:3959 getComparer — pick the comparer for a child's own children.
fn get_comparer(
    ctx: &mut WalkContext,
    cmp: &Comparer,
    child: &Holder,
    diff: &[Value],
) -> anyhow::Result<Option<Comparer>> {
    let ed = cmp
        .snapshot
        .get(child.base_index)
        .cloned()
        .unwrap_or(Value::Null);
    let ed_types: Vec<Value> = ed
        .get("type")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let ed_code = ed_types.first().and_then(working_code).unwrap_or_default();
    let self_types = child_self_types(child, diff);
    let self_code = self_types.first().and_then(|t| working_code(t)).unwrap_or_default();

    if ed_types.is_empty() || is_abstract(&ed_code) || ed_code == path_of(&ed) {
        // running within the same structure (backbone) — or Resource profiled.
        if !ed_types.is_empty()
            && ed_code == "Resource"
            && !self_types.is_empty()
            && first_profile_of(self_types[0]).is_some()
        {
            let profiles = self_types[0]
                .get("profile")
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0);
            if profiles > 1 {
                anyhow::bail!("UNHANDLED_SITUATION_RESOURCE_IS_PROFILED_TO_MORE_THAN_ONE_OPTION");
            }
            let purl = first_profile_of(self_types[0]).unwrap();
            let mut profile = resolve_with_snapshot(ctx, &purl)?;
            // walk up CONSTRAINT chain to the specialization base
            let mut lc = 0;
            while let Some(p) = &profile {
                if p.get("derivation").and_then(Value::as_str) != Some("constraint") {
                    break;
                }
                let base_def = p
                    .get("baseDefinition")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                profile = match base_def {
                    Some(b) => resolve_with_snapshot(ctx, &b)?,
                    None => None,
                };
                lc += 1;
                if lc > 50 {
                    break;
                }
            }
            match profile {
                None => Ok(None),
                Some(p) => {
                    let ptype = p.get("type").and_then(Value::as_str).unwrap_or("").to_string();
                    let purl2 = p.get("url").and_then(Value::as_str).unwrap_or("").to_string();
                    Ok(Some(Comparer::new(
                        purl2,
                        snapshot_of(&p),
                        &ptype,
                        child.path.len(),
                    )))
                }
            }
        } else {
            Ok(Some(Comparer::new(
                cmp.src_url.clone(),
                cmp.snapshot.clone(),
                &cmp.base,
                cmp.prefix_length,
            )))
        }
    } else if ed_code == "Extension"
        && self_types.len() == 1
        && first_profile_of(self_types[0]).is_some()
    {
        let purl = first_profile_of(self_types[0]).unwrap();
        match resolve_with_snapshot(ctx, &purl)? {
            None => Ok(None),
            Some(p) => {
                let base = resolve_type_name(ctx, &ed_code);
                let purl2 = p.get("url").and_then(Value::as_str).unwrap_or("").to_string();
                Ok(Some(Comparer::new(
                    purl2,
                    snapshot_of(&p),
                    &base,
                    child.path.len(),
                )))
            }
        }
    } else if ed_types.len() == 1 && ed_code != "*" {
        let ns = sd_ns(&ed_code);
        let Some(p) = resolve_with_snapshot(ctx, &ns)? else {
            anyhow::bail!("UNABLE_TO_RESOLVE_PROFILE {ns} in element {}", path_of(&ed));
        };
        let base = resolve_type_name(ctx, &ed_code);
        let purl2 = p.get("url").and_then(Value::as_str).unwrap_or("").to_string();
        Ok(Some(Comparer::new(purl2, snapshot_of(&p), &base, child.path.len())))
    } else if self_types.len() == 1 {
        let ns = sd_ns(&self_code);
        let Some(p) = resolve_with_snapshot(ctx, &ns)? else {
            anyhow::bail!("UNABLE_TO_RESOLVE_PROFILE {ns} in element {}", path_of(&ed));
        };
        let purl2 = p.get("url").and_then(Value::as_str).unwrap_or("").to_string();
        Ok(Some(Comparer::new(
            purl2,
            snapshot_of(&p),
            &self_code,
            child.path.len(),
        )))
    } else if path_of(&ed).ends_with("[x]") && !child.path.ends_with("[x]") {
        let ed_last = path_of(&ed).rsplit('.').next().unwrap_or("");
        let child_last = child.path.rsplit('.').next().unwrap_or("");
        let mut p_name = child_last[ed_last.len().saturating_sub(3).min(child_last.len())..].to_string();
        let uncap = {
            let mut c = p_name.chars();
            match c.next() {
                Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        };
        if super::types_pred::is_primitive_str(ctx, &uncap) {
            p_name = uncap;
        }
        let ns = sd_ns(&p_name);
        let Some(sd) = resolve_with_snapshot(ctx, &ns)? else {
            anyhow::bail!("UNABLE_TO_FIND_PROFILE {p_name} at {}", path_of(&ed));
        };
        let purl2 = sd.get("url").and_then(Value::as_str).unwrap_or("").to_string();
        Ok(Some(Comparer::new(
            purl2,
            snapshot_of(&sd),
            &p_name,
            child.path.len(),
        )))
    } else if !self_types.is_empty() && self_code == "Reference" {
        for t in &self_types {
            if working_code(t).as_deref() != Some("Reference") {
                anyhow::bail!(
                    "CANT_HAVE_CHILDREN_ON_AN_ELEMENT_WITH_A_POLYMORPHIC_TYPE at {}",
                    path_of(&ed)
                );
            }
        }
        let ns = sd_ns(&ed_code);
        let Some(p) = resolve_with_snapshot(ctx, &ns)? else {
            return Ok(None);
        };
        let purl2 = p.get("url").and_then(Value::as_str).unwrap_or("").to_string();
        Ok(Some(Comparer::new(purl2, snapshot_of(&p), &ed_code, child.path.len())))
    } else if self_types.is_empty() && ed_code == "Reference" {
        for t in &ed_types {
            if working_code(t).as_deref() != Some("Reference") {
                anyhow::bail!("NOT_HANDLED_YET_SORTELEMENTS at {}", path_of(&ed));
            }
        }
        let ns = sd_ns(&ed_code);
        let Some(p) = resolve_with_snapshot(ctx, &ns)? else {
            return Ok(None);
        };
        let purl2 = p.get("url").and_then(Value::as_str).unwrap_or("").to_string();
        Ok(Some(Comparer::new(purl2, snapshot_of(&p), &ed_code, child.path.len())))
    } else {
        // only profiling extensions — sort against Element
        let ns = sd_ns("Element");
        let Some(p) = resolve_with_snapshot(ctx, &ns)? else {
            anyhow::bail!("UNABLE_TO_RESOLVE_PROFILE Element");
        };
        let purl2 = p.get("url").and_then(Value::as_str).unwrap_or("").to_string();
        Ok(Some(Comparer::new(purl2, snapshot_of(&p), "Element", child.path.len())))
    }
}

/// PU: writeElements — collect original-diff indices in sorted order, skipping
/// placeholders.
fn write_elements(edh: &Holder, out: &mut Vec<usize>) {
    if let Some(idx) = edh.self_idx {
        out.push(idx);
    }
    for child in &edh.children {
        write_elements(child, out);
    }
}

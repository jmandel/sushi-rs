//! FSH path parsing + soft-index resolution (port of `utils/PathUtils.ts`).
//! Used at export time to resolve `[+]`/`[=]` into concrete numeric indices on
//! both rule `path` and `caretPath`, exactly as `resolveSoftIndexing`.

use fsh_model::Rule;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct PathPart {
    pub base: String,
    pub brackets: Vec<String>,
    pub slices: Vec<String>,
    pub prefix: String,
}

/// `splitOnPathPeriods`: split on '.' not inside `[...]`.
pub fn split_on_path_periods(path: &str) -> Vec<String> {
    split_on_path_periods_borrowed(path).iter().map(|s| s.to_string()).collect()
}

/// Allocation-free `splitOnPathPeriods`: yields `&str` slices borrowing `path`.
/// Each segment is an exact substring (only the depth-0 '.' separators are
/// removed), so no per-segment `String` is allocated. `[`/`]`/`.` are all ASCII,
/// so the slice boundaries always land on char boundaries.
pub fn split_on_path_periods_borrowed(path: &str) -> smallvec::SmallVec<[&str; 8]> {
    let mut parts = smallvec::SmallVec::new();
    let bytes = path.as_bytes();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'[' => depth += 1,
            b']' => depth -= 1,
            b'.' if depth == 0 => {
                parts.push(&path[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&path[start..]);
    parts
}

/// Last depth-0 '.'-separated segment of `path`, borrowed (no allocation). Equal
/// to `split_on_path_periods(path).pop().unwrap()` but without building the Vec.
pub fn last_path_period_segment(path: &str) -> &str {
    let bytes = path.as_bytes();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'[' => depth += 1,
            b']' => depth -= 1,
            b'.' if depth == 0 => start = i + 1,
            _ => {}
        }
    }
    &path[start..]
}

fn is_numeric(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// `parseFSHPath`.
pub fn parse_fsh_path(fsh_path: &str) -> Vec<PathPart> {
    let mut parts = Vec::new();
    let mut seen_slices: Vec<String> = Vec::new();
    let split: smallvec::SmallVec<[&str; 8]> = if fsh_path == "." {
        smallvec::smallvec!["."]
    } else {
        split_on_path_periods_borrowed(fsh_path)
    };
    for part in split {
        // base = leading non-'[' run, plus a trailing literal "[x]" if present.
        let nb_end = part.find('[').unwrap_or(part.len());
        let mut base = part[..nb_end].to_string();
        let mut rest = &part[nb_end..];
        if rest.starts_with("[x]") {
            base.push_str("[x]");
            rest = &rest[3..];
        }
        // brackets: outermost [...] pairs in `rest`.
        let mut brackets = Vec::new();
        let bytes: Vec<char> = rest.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == '[' {
                let mut depth = 1;
                let mut j = i + 1;
                let mut inner = String::new();
                while j < bytes.len() && depth > 0 {
                    match bytes[j] {
                        '[' => {
                            depth += 1;
                            inner.push('[');
                        }
                        ']' => {
                            depth -= 1;
                            if depth > 0 {
                                inner.push(']');
                            }
                        }
                        c => inner.push(c),
                    }
                    j += 1;
                }
                brackets.push(inner);
                i = j;
            } else {
                i += 1;
            }
        }
        for b in &brackets {
            if !is_numeric(b) && b != "+" && b != "=" {
                seen_slices.push(b.clone());
            }
        }
        let slices = seen_slices.clone();
        parts.push(PathPart {
            base,
            brackets,
            slices,
            prefix: String::new(),
        });
    }
    parts
}

pub fn assemble_fsh_path(parts: &[PathPart]) -> String {
    let mut path = String::new();
    for (i, p) in parts.iter().enumerate() {
        path.push_str(&p.base);
        for b in &p.brackets {
            path.push('[');
            path.push_str(b);
            path.push(']');
        }
        if i < parts.len() - 1 {
            path.push('.');
        }
    }
    path
}

/// `convertSoftIndices` (non-strict).
fn convert_soft_indices(element: &mut PathPart, path_map: &mut HashMap<String, i64>) {
    let map_name = format!("{}.{}|{}", element.prefix, element.base, element.slices.join("|"));
    if !path_map.contains_key(&map_name) {
        if let Some(num) = element.brackets.iter().find(|b| is_numeric(b)) {
            path_map.insert(map_name, num.parse().unwrap_or(0));
        } else {
            path_map.insert(map_name.clone(), 0);
            if let Some(pos) = element.brackets.iter().position(|b| b == "+") {
                element.brackets[pos] = "0".to_string();
            } else if let Some(pos) = element.brackets.iter().position(|b| b == "=") {
                element.brackets[pos] = "0".to_string();
            }
        }
    } else {
        let mut cur = *path_map.get(&map_name).unwrap();
        for idx in 0..element.brackets.len() {
            let b = element.brackets[idx].clone();
            if b == "+" {
                cur += 1;
                element.brackets[idx] = cur.to_string();
                path_map.insert(map_name.clone(), cur);
            } else if b == "=" {
                element.brackets[idx] = cur.to_string();
            } else if is_numeric(&b) {
                cur = b.parse().unwrap_or(0);
                path_map.insert(map_name.clone(), cur);
            }
        }
    }
}

/// `convertSoftIndicesStrict` — like `convert_soft_indices`, but maintains a
/// `max_path_map` and propagates increments up to the less-sliced elements so a
/// generic soft-indexed entry following a named slice lands in a NEW slot rather
/// than the named slice's slot. Used only when `manualSliceOrdering` is enabled.
fn convert_soft_indices_strict(
    element: &mut PathPart,
    path_map: &mut HashMap<String, i64>,
    max_path_map: &mut HashMap<String, i64>,
) {
    let map_name = format!("{}.{}|{}", element.prefix, element.base, element.slices.join("|"));
    // Track how many indices we need to add to the base (less-sliced) element.
    let mut add_to_base_element: Option<i64> = None;
    if !path_map.contains_key(&map_name) {
        if let Some(num) = element.brackets.iter().find(|b| is_numeric(b)) {
            let index_used: i64 = num.parse().unwrap_or(0);
            path_map.insert(map_name.clone(), index_used);
            max_path_map.insert(map_name.clone(), index_used);
            add_to_base_element = Some(index_used + 1);
        } else {
            path_map.insert(map_name.clone(), 0);
            max_path_map.insert(map_name.clone(), 0);
            add_to_base_element = Some(1);
            if let Some(pos) = element.brackets.iter().position(|b| b == "+") {
                element.brackets[pos] = "0".to_string();
            } else if let Some(pos) = element.brackets.iter().position(|b| b == "=") {
                // Stock throws (first index must be '+'); we assume 0 like the
                // non-strict path does, to stay robust.
                element.brackets[pos] = "0".to_string();
            }
        }
    } else {
        for idx in 0..element.brackets.len() {
            let b = element.brackets[idx].clone();
            if b == "+" {
                let new_index = *path_map.get(&map_name).unwrap() + 1;
                element.brackets[idx] = new_index.to_string();
                path_map.insert(map_name.clone(), new_index);
                let max = *max_path_map.get(&map_name).unwrap();
                if new_index > max {
                    add_to_base_element = Some(new_index - max);
                    max_path_map.insert(map_name.clone(), new_index);
                }
            } else if b == "=" {
                let current_index = *path_map.get(&map_name).unwrap();
                element.brackets[idx] = current_index.to_string();
            } else if is_numeric(&b) {
                let new_index: i64 = b.parse().unwrap_or(0);
                path_map.insert(map_name.clone(), new_index);
                let max = *max_path_map.get(&map_name).unwrap();
                if new_index > max {
                    add_to_base_element = Some(new_index - max);
                    max_path_map.insert(map_name.clone(), new_index);
                }
            }
        }
    }
    // If the element has slices, increment the less-sliced elements.
    if !element.slices.is_empty() {
        if let Some(add) = add_to_base_element {
            for take_slices in (0..element.slices.len()).rev() {
                let less_sliced_map_name = format!(
                    "{}.{}|{}",
                    element.prefix,
                    element.base,
                    element.slices[..take_slices].join("|")
                );
                if !path_map.contains_key(&less_sliced_map_name) {
                    // New entry: subtract 1 since tracked values start at 0.
                    path_map.insert(less_sliced_map_name.clone(), add - 1);
                    max_path_map.insert(less_sliced_map_name.clone(), add - 1);
                } else {
                    let old_max = *max_path_map.get(&less_sliced_map_name).unwrap();
                    let new_index = *path_map.get(&less_sliced_map_name).unwrap() + add;
                    path_map.insert(less_sliced_map_name.clone(), new_index);
                    if new_index > old_max {
                        max_path_map.insert(less_sliced_map_name.clone(), new_index);
                    }
                }
            }
        }
    }
}

/// `resolveSoftIndexing(rules, strict)` — mutate rule `path` and `caretPath` in
/// place. When `strict` is true (InstanceExporter with `manualSliceOrdering`),
/// uses `convert_soft_indices_strict`; otherwise the non-strict variant.
pub fn resolve_soft_indexing(rules: &mut [Rule], strict: bool) {
    let mut path_map: HashMap<String, i64> = HashMap::new();
    let mut max_path_map: HashMap<String, i64> = HashMap::new();
    let mut caret_maps: HashMap<String, HashMap<String, i64>> = HashMap::new();
    let mut caret_max_maps: HashMap<String, HashMap<String, i64>> = HashMap::new();

    for rule in rules.iter_mut() {
        let mut parsed = parse_fsh_path(rule.path());
        for i in 0..parsed.len() {
            parsed[i].prefix = assemble_fsh_path(&parsed[..i]);
            if strict {
                convert_soft_indices_strict(&mut parsed[i], &mut path_map, &mut max_path_map);
            } else {
                convert_soft_indices(&mut parsed[i], &mut path_map);
            }
        }
        let new_path = assemble_fsh_path(&parsed);
        rule.set_path(new_path.clone());

        if let Rule::CaretValue { caret_path, path_array, .. } = rule {
            if let Some(cp) = caret_path.clone() {
                // Key the caret soft-index map by the rule path AND the concept
                // path-array. `path_array` is non-empty only for CodeSystem
                // concept-level carets (`* #code ^property[+]`), so this isolates
                // each concept's `[+]`/`[=]` counters (they reset per concept)
                // without changing SD/instance/VS carets (empty path_array).
                let map_key = format!("{}\u{1}{}", new_path, path_array.join("\u{1}"));
                let mut cparsed = parse_fsh_path(&cp);
                let cm = caret_maps.entry(map_key.clone()).or_default();
                let cmm = caret_max_maps.entry(map_key).or_default();
                for i in 0..cparsed.len() {
                    cparsed[i].prefix = assemble_fsh_path(&cparsed[..i]);
                    if strict {
                        convert_soft_indices_strict(&mut cparsed[i], cm, cmm);
                    } else {
                        convert_soft_indices(&mut cparsed[i], cm);
                    }
                }
                let assembled = assemble_fsh_path(&cparsed);
                if let Rule::CaretValue { caret_path, .. } = rule {
                    *caret_path = Some(assembled);
                }
            }
        }
    }
}

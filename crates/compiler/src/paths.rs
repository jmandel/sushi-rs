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
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in path.chars() {
        match c {
            '[' => {
                depth += 1;
                cur.push(c);
            }
            ']' => {
                depth -= 1;
                cur.push(c);
            }
            '.' if depth == 0 => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    parts
}

fn is_numeric(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// `parseFSHPath`.
pub fn parse_fsh_path(fsh_path: &str) -> Vec<PathPart> {
    let mut parts = Vec::new();
    let mut seen_slices: Vec<String> = Vec::new();
    let split: Vec<String> = if fsh_path == "." {
        vec![".".to_string()]
    } else {
        split_on_path_periods(fsh_path)
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

/// `resolveSoftIndexing(rules)` — mutate rule `path` and `caretPath` in place.
pub fn resolve_soft_indexing(rules: &mut [Rule]) {
    let mut path_map: HashMap<String, i64> = HashMap::new();
    let mut caret_maps: HashMap<String, HashMap<String, i64>> = HashMap::new();

    for rule in rules.iter_mut() {
        let mut parsed = parse_fsh_path(rule.path());
        for i in 0..parsed.len() {
            parsed[i].prefix = assemble_fsh_path(&parsed[..i]);
            convert_soft_indices(&mut parsed[i], &mut path_map);
        }
        let new_path = assemble_fsh_path(&parsed);
        rule.set_path(new_path.clone());

        if let Rule::CaretValue { caret_path, .. } = rule {
            if let Some(cp) = caret_path.clone() {
                let mut cparsed = parse_fsh_path(&cp);
                let cm = caret_maps.entry(new_path.clone()).or_default();
                for i in 0..cparsed.len() {
                    cparsed[i].prefix = assemble_fsh_path(&cparsed[..i]);
                    convert_soft_indices(&mut cparsed[i], cm);
                }
                let assembled = assemble_fsh_path(&cparsed);
                if let Rule::CaretValue { caret_path, .. } = rule {
                    *caret_path = Some(assembled);
                }
            }
        }
    }
}

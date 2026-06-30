//! Instance export (Phase 7). Ports `sushi-ts/src/export/InstanceExporter.ts`
//! + the `setImpliedPropertiesOnInstance` / `setPropertyOnInstance` /
//! `validateValueAtPath` / `cleanResource` / `replaceReferences` machinery from
//! `common.ts` and `StructureDefinition.ts`, producing byte-identical
//! `<ResourceType>-<id>.json` output vs stock SUSHI.

use crate::config::Config;
use crate::sd_export::SdContext;
use fhir_model::{Fisher, StructureDefinition};
use fsh_model::{FshCode, FshDocument, FshQuantity, FshReference, Instance, Rule, Value as FshValue};
use rustc_hash::FxHashMap;
use serde_json::{Map, Value as J};
use std::collections::HashMap;
use std::rc::Rc;

// ---------------------------------------------------------------------------
// Path parts (instance engine; richer than fhir_model::PathPart).
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct IPathPart {
    base: String,
    brackets: Vec<String>,
    primitive: bool,
}

fn parse_ipath(path: &str) -> Vec<IPathPart> {
    fhir_model::parse_fsh_path(path)
        .into_iter()
        .map(|p| IPathPart {
            base: p.base,
            brackets: p.brackets,
            primitive: false,
        })
        .collect()
}

/// `getArrayIndex` — last bracket parsed as a non-negative int.
fn get_array_index(p: &IPathPart) -> Option<i64> {
    let last = p.brackets.last()?;
    if last.chars().all(|c| c.is_ascii_digit() || c == '-' || c == '+') && !last.is_empty() {
        if let Ok(n) = last.parse::<i64>() {
            if n >= 0 {
                return Some(n);
            }
        }
    }
    None
}

/// `getSliceName(pathPart)` — non-numeric brackets joined with `/`.
fn get_slice_name(p: &IPathPart) -> String {
    let has_index = get_array_index(p).is_some();
    let nb: &[String] = if has_index {
        &p.brackets[..p.brackets.len() - 1]
    } else {
        &p.brackets[..]
    };
    nb.join("/")
}

fn assemble_fsh_path(parts: &[IPathPart]) -> String {
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

fn is_extension_base(base: &str) -> bool {
    base == "extension" || base == "modifierExtension"
}

/// Type-code regex test from validateValueAtPath: `^[a-z][a-zA-Z0-9]*$`.
fn is_primitive_code(code: &str) -> bool {
    let mut chars = code.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    code.chars().all(|c| c.is_ascii_alphanumeric())
}

// ---------------------------------------------------------------------------
// Element-tree helpers over a fhir_model::StructureDefinition.
// ---------------------------------------------------------------------------

fn el_min(sd: &StructureDefinition, idx: usize) -> i64 {
    sd.elements[idx].get("min").and_then(|v| v.as_i64()).unwrap_or(0)
}
fn el_max<'a>(sd: &'a StructureDefinition, idx: usize) -> Option<&'a str> {
    sd.elements[idx].get("max").and_then(|v| v.as_str())
}
fn el_slice_name(sd: &StructureDefinition, idx: usize) -> Option<String> {
    sd.elements[idx].slice_name().map(|s| s.to_string())
}
fn el_id(sd: &StructureDefinition, idx: usize) -> String {
    sd.elements[idx].id().to_string()
}
/// Borrowing variant of `el_id` (no String allocation) for read-only callers.
fn el_id_ref(sd: &StructureDefinition, idx: usize) -> &str {
    sd.elements[idx].id()
}
fn el_path(sd: &StructureDefinition, idx: usize) -> String {
    sd.elements[idx].path().to_string()
}
fn el_type_codes(sd: &StructureDefinition, idx: usize) -> Vec<String> {
    sd.elements[idx].type_codes()
}

/// Per-traversal structural index over `sd.elements`, replacing the O(n) linear
/// scans in `children_direct`/`get_slices`. Those are hit many times per BFS node
/// (`find_connected_elements` alone walks the entire ancestor chain calling
/// `get_slices` at every level, for every dequeued node), so the scans were
/// effectively O(n^2). Self-heals when the element count changes — an `unfold`
/// splices in new elements — mirroring `index_of_id`'s length-keyed cache.
/// LOOKUP ONLY: results stay index-ascending, byte-identical to the scans they
/// replace, and never drive emission order.
#[derive(Default)]
struct StructIndexInner {
    /// Element count both maps were built for (`usize::MAX` = unbuilt).
    len: usize,
    /// Direct children per element index (ascending) — TS `children(true)` shape.
    children: Vec<Vec<usize>>,
    /// Element indices grouped by exact `path` (ascending) — slice candidates.
    by_path: FxHashMap<String, Vec<usize>>,
}

struct StructIndex {
    inner: std::cell::RefCell<StructIndexInner>,
}

impl StructIndex {
    fn new() -> Self {
        let inner = StructIndexInner {
            len: usize::MAX,
            ..Default::default()
        };
        Self {
            inner: std::cell::RefCell::new(inner),
        }
    }

    /// (Re)build both maps in a single pass iff the element count changed. Built
    /// lazily on first lookup; mcode traversals need both maps (children for the
    /// BFS, by_path for `get_slices`/`find_connected_elements`), so one combined
    /// pass beats two. The `by_path` key alloc is negligible whole-program.
    fn ensure(&self, sd: &StructureDefinition) {
        let n = sd.elements.len();
        if self.inner.borrow().len == n {
            return;
        }
        let mut inner = self.inner.borrow_mut();
        inner.children.clear();
        inner.children.resize_with(n, Vec::new);
        inner.by_path.clear();
        // id -> first index (matches `index_of_id` first-wins semantics).
        let mut id_to_idx: FxHashMap<&str, usize> =
            FxHashMap::with_capacity_and_hasher(n, Default::default());
        for (i, e) in sd.elements.iter().enumerate() {
            id_to_idx.entry(e.id()).or_insert(i);
        }
        for (j, e) in sd.elements.iter().enumerate() {
            let id = e.id();
            // `children_direct(p)` == { j : id_j starts with "{id_p}." at depth+1 }.
            // Since id segments are '.'-separated (slice ':'/'/' never add a '.'),
            // that is exactly { j : id_j without its last '.'-segment == id_p }.
            if let Some(cut) = id.rfind('.') {
                if let Some(&p) = id_to_idx.get(&id[..cut]) {
                    inner.children[p].push(j);
                }
            }
            inner.by_path.entry(e.path().to_string()).or_default().push(j);
        }
        inner.len = n;
    }

    fn children(&self, sd: &StructureDefinition, idx: usize) -> Vec<usize> {
        self.ensure(sd);
        self.inner.borrow().children.get(idx).cloned().unwrap_or_default()
    }

    fn slices(&self, sd: &StructureDefinition, idx: usize) -> Vec<usize> {
        self.ensure(sd);
        let id = sd.elements[idx].id();
        let idl = id.len();
        let sep = if sd.elements[idx].slice_name().is_some() {
            b'/'
        } else {
            b':'
        };
        let path = sd.elements[idx].path();
        let inner = self.inner.borrow();
        let Some(cands) = inner.by_path.get(path) else {
            return Vec::new();
        };
        cands
            .iter()
            .copied()
            .filter(|&j| {
                if j == idx {
                    return false;
                }
                let jid = sd.elements[j].id();
                let jb = jid.as_bytes();
                jb.len() > idl && jb[idl] == sep && jid.starts_with(id)
            })
            .collect()
    }
}

/// Direct children: ids starting `{id}.` with path-depth == this+1.
fn children_direct(sd: &StructureDefinition, idx: usize, ix: &StructIndex) -> Vec<usize> {
    ix.children(sd, idx)
}

/// `getSlices()` — siblings that are slices/reslices of element `idx`.
fn get_slices(sd: &StructureDefinition, idx: usize, ix: &StructIndex) -> Vec<usize> {
    ix.slices(sd, idx)
}

/// `parent()` — element at id without the trailing `.segment`.
fn parent_idx(sd: &StructureDefinition, idx: usize) -> Option<usize> {
    let id = el_id_ref(sd, idx);
    let cut = id.rfind('.')?;
    let pid = &id[..cut];
    sd.find_element(pid)
}

// ---------------------------------------------------------------------------
// Value coercion (port of ElementDefinition.assignValue, output side only).
// ---------------------------------------------------------------------------

fn coding_from(fc: &FshCode) -> J {
    crate::export::coding_from(fc)
}

fn quantity_from_code(fc: &FshCode) -> Map<String, J> {
    let mut m = Map::new();
    if !fc.code.is_empty() {
        m.insert("code".into(), J::String(fc.code.clone()));
    }
    if let Some(sys) = &fc.system {
        m.insert("system".into(), J::String(sys.clone()));
    }
    if let Some(d) = &fc.display {
        m.insert("unit".into(), J::String(d.clone()));
    }
    m
}

fn quantity_from(q: &FshQuantity) -> Map<String, J> {
    let mut m = Map::new();
    if let Some(v) = q.value {
        m.insert("value".into(), num_json(v));
    }
    if let Some(u) = &q.unit {
        if !u.code.is_empty() {
            m.insert("code".into(), J::String(u.code.clone()));
        }
        if let Some(sys) = &u.system {
            m.insert("system".into(), J::String(sys.clone()));
        }
        if let Some(d) = &u.display {
            m.insert("unit".into(), J::String(d.clone()));
        }
    }
    m
}

fn reference_from(r: &FshReference) -> Map<String, J> {
    let mut m = Map::new();
    m.insert("reference".into(), J::String(r.reference.clone()));
    if let Some(d) = &r.display {
        m.insert("display".into(), J::String(d.clone()));
    }
    m
}

/// Port of SUSHI's xhtml handling: `minify(value, {collapseWhitespace:true,
/// html5:false, keepClosingSlash:true})` (ElementDefinition.assignString). Two
/// transforms: normalize attribute value quotes to `"`, and collapse whitespace
/// with block-element awareness.
fn minify_xhtml(input: &str) -> String {
    #[derive(Clone)]
    enum Tok {
        Tag(String, String), // (rendered tag, lowercase element name)
        Text(String),
    }
    // Tokenize into tags vs text (respecting quotes inside tags).
    let chars: Vec<char> = input.chars().collect();
    let mut toks: Vec<Tok> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            // read until matching '>' (skip over quoted attr values)
            let mut j = i + 1;
            let mut quote: Option<char> = None;
            while j < chars.len() {
                let c = chars[j];
                match quote {
                    Some(q) => {
                        if c == q {
                            quote = None;
                        }
                    }
                    None => {
                        if c == '\'' || c == '"' {
                            quote = Some(c);
                        } else if c == '>' {
                            break;
                        }
                    }
                }
                j += 1;
            }
            let raw: String = chars[i..=j.min(chars.len() - 1)].iter().collect();
            let name = tag_element_name(&raw);
            toks.push(Tok::Tag(rewrite_tag_quotes(&raw), name));
            i = j + 1;
        } else {
            let mut j = i;
            while j < chars.len() && chars[j] != '<' {
                j += 1;
            }
            let raw: String = chars[i..j].iter().collect();
            toks.push(Tok::Text(raw));
            i = j;
        }
    }
    // Collapse whitespace on text tokens with block-awareness.
    let mut out = String::new();
    for idx in 0..toks.len() {
        match &toks[idx] {
            Tok::Tag(t, _) => out.push_str(t),
            Tok::Text(s) => {
                let collapsed = collapse_ws(s);
                let trim_left = match idx.checked_sub(1).and_then(|p| toks.get(p)) {
                    None => true,
                    Some(Tok::Tag(_, name)) => !is_inline_element(name),
                    Some(Tok::Text(_)) => false,
                };
                let trim_right = match toks.get(idx + 1) {
                    None => true,
                    Some(Tok::Tag(_, name)) => !is_inline_element(name),
                    Some(Tok::Text(_)) => false,
                };
                let mut v = collapsed.as_str();
                if trim_left {
                    v = v.trim_start_matches(' ');
                }
                if trim_right {
                    v = v.trim_end_matches(' ');
                }
                out.push_str(v);
            }
        }
    }
    out
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::new();
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

fn tag_element_name(tag: &str) -> String {
    let t = tag.trim_start_matches('<').trim_start_matches('/');
    t.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == ':')
        .collect::<String>()
        .to_ascii_lowercase()
}

fn is_inline_element(name: &str) -> bool {
    matches!(
        name,
        "a" | "abbr" | "acronym" | "b" | "bdo" | "big" | "br" | "button" | "cite" | "code"
            | "dfn" | "em" | "i" | "img" | "input" | "kbd" | "label" | "map" | "object" | "q"
            | "samp" | "select" | "small" | "span" | "strong" | "sub" | "sup" | "textarea"
            | "time" | "tt" | "u" | "var"
    )
}

/// Within a tag, convert single-quoted attribute values to double-quoted
/// (matching html-minifier's default `"` quoting), when the value has no `"`.
fn rewrite_tag_quotes(tag: &str) -> String {
    let chars: Vec<char> = tag.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\'' {
            // find closing single quote
            if let Some(close) = (i + 1..chars.len()).find(|&k| chars[k] == '\'') {
                let inner: String = chars[i + 1..close].iter().collect();
                if !inner.contains('"') {
                    out.push('"');
                    out.push_str(&inner);
                    out.push('"');
                    i = close + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    // Collapse whitespace before a trailing self-closing `/>`.
    if out.ends_with("/>") {
        let body = &out[..out.len() - 2];
        let trimmed = body.trim_end();
        out = format!("{trimmed}/>");
    }
    out
}

fn num_json(f: f64) -> J {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        J::Number((f as i64).into())
    } else {
        serde_json::Number::from_f64(f).map(J::Number).unwrap_or(J::Null)
    }
}

fn bigint_json(s: &str) -> J {
    if let Ok(i) = s.parse::<i64>() {
        J::Number(i.into())
    } else if let Ok(u) = s.parse::<u64>() {
        J::Number(u.into())
    } else {
        J::String(s.to_string())
    }
}

/// Coerce a FSH value to the assignedValue JSON for an element with the given
/// single type `code`. Returns (value, optional childPath for CodeableReference).
fn coerce_value(
    type_code: &str,
    value: &FshValue,
    fisher: &dyn Fisher,
) -> Option<(J, Option<&'static str>)> {
    let v = match value {
        FshValue::Bool(b) => J::Bool(*b),
        FshValue::BigInt(s) => bigint_json(s),
        FshValue::Float(f) => num_json(*f),
        FshValue::Str(s) if type_code == "xhtml" => J::String(minify_xhtml(s)),
        FshValue::Str(s) => J::String(s.clone()),
        FshValue::Code(fc) => match type_code {
            "code" | "string" | "uri" => J::String(fc.code.clone()),
            "CodeableConcept" => {
                let mut m = Map::new();
                m.insert("coding".into(), J::Array(vec![coding_from(fc)]));
                J::Object(m)
            }
            "Coding" => coding_from(fc),
            "CodeableReference" => {
                let mut m = Map::new();
                let mut cc = Map::new();
                cc.insert("coding".into(), J::Array(vec![coding_from(fc)]));
                m.insert("concept".into(), J::Object(cc));
                return Some((J::Object(m), Some("concept")));
            }
            _ => {
                // Quantity (and specializations) take the code mapping.
                J::Object(quantity_from_code(fc))
            }
        },
        FshValue::Quantity(q) => J::Object(quantity_from(q)),
        FshValue::Ratio(r) => {
            let mut m = Map::new();
            let num = quantity_from(&r.numerator);
            if !num.is_empty() {
                m.insert("numerator".into(), J::Object(num));
            }
            let den = quantity_from(&r.denominator);
            if !den.is_empty() {
                m.insert("denominator".into(), J::Object(den));
            }
            J::Object(m)
        }
        FshValue::Reference(r) => {
            if type_code == "CodeableReference" {
                let mut m = Map::new();
                m.insert("reference".into(), J::Object(reference_from(r)));
                return Some((J::Object(m), Some("reference")));
            }
            J::Object(reference_from(r))
        }
        FshValue::Canonical(c) => {
            // Resolve canonical url from the entity metadata.
            let mut url = c
                .entity_name
                .clone();
            if let Some(meta) = fisher.fish_for_metadata(&c.entity_name) {
                if let Some(u) = meta.url {
                    url = u;
                }
            }
            if let Some(v) = &c.version {
                url = format!("{url}|{v}");
            }
            J::String(url)
        }
    };
    Some((v, None))
}

// ---------------------------------------------------------------------------
// validateValueAtPath (port of StructureDefinition.ts:600).
// ---------------------------------------------------------------------------

struct Validated {
    assigned_value: Option<J>,
    path_parts: Vec<IPathPart>,
    child_path: Option<&'static str>,
}

/// Walk the path, find/unfold the element at each step, set primitive flags and
/// add `0` brackets for array elements, and coerce the leaf value.
fn validate_value_at_path(
    sd: &mut StructureDefinition,
    path: &str,
    value: Option<&FshValue>,
    fisher: &dyn Fisher,
) -> Option<Validated> {
    let mut path_parts = parse_ipath(path);
    let mut current_path = String::new();
    let mut current_idx: Option<usize> = None;
    let n = path_parts.len();

    for i in 0..n {
        let previous_path = current_path.clone();
        {
            let pp = &path_parts[i];
            if !current_path.is_empty() {
                current_path.push('.');
            }
            current_path.push_str(&pp.base);
        }
        let array_index = get_array_index(&path_parts[i]);
        let _slice_name = if array_index.is_some() {
            Some(get_slice_name(&path_parts[i]))
        } else {
            None
        };
        // Reconstruct currentPath with brackets.
        {
            let pp = &path_parts[i];
            if array_index.is_some() {
                for b in &pp.brackets[..pp.brackets.len() - 1] {
                    current_path.push('[');
                    current_path.push_str(b);
                    current_path.push(']');
                }
            } else {
                for b in &pp.brackets {
                    current_path.push('[');
                    current_path.push_str(b);
                    current_path.push(']');
                }
            }
        }

        current_idx = sd.find_element_by_path(&current_path, fisher);

        // Allow adding extension slices that are not yet on the SD.
        if current_idx.is_none() && is_extension_base(&path_parts[i].base) {
            let ext_path = if previous_path.is_empty() {
                path_parts[i].base.clone()
            } else {
                format!("{}.{}", previous_path, path_parts[i].base)
            };
            let ext_el = sd.find_element_by_path(&ext_path, fisher);
            let bracket0 = path_parts[i].brackets.first().cloned();
            if let (Some(ext_idx), Some(b0)) = (ext_el, bracket0) {
                if let Some(meta) = fisher.fish_for_metadata(&b0) {
                    if let Some(url) = &meta.url {
                        // First, try to match an EXISTING slice on the SD by its
                        // type profile url (`findMatchingSlice` fishForFHIR branch,
                        // StructureDefinition.ts:907-913). The bracket may be an
                        // alias/url/id that doesn't equal the slice's sliceName
                        // (e.g. `extension[USCoreRace]` -> inherited `race` slice).
                        if let Some(existing_sn) = find_ext_slice_by_profile(sd, ext_idx, url) {
                            // Replace ONLY the matched (first) bracket with the slice
                            // name, preserving any following brackets such as a
                            // numeric index (`extension[Name][1]`). The original code
                            // overwrote the whole bracket list, dropping the trailing
                            // index — which made a soft-indexed slice rule look like a
                            // scalar assignment and corrupted the `extension` array
                            // (G1 crash). StructureDefinition.ts:671-681 rewrites only
                            // the matched bracket and leaves the rest of the path.
                            let mut new_brackets: Vec<String> =
                                existing_sn.split('/').map(|s| s.to_string()).collect();
                            new_brackets.extend(path_parts[i].brackets.iter().skip(1).cloned());
                            path_parts[i].brackets = new_brackets;
                            // Rebuild currentPath, excluding the trailing numeric index
                            // when this part is an array element (mirrors the step-2
                            // reconstruction above that drops the last bracket when
                            // `array_index` is present).
                            current_path = previous_path.clone();
                            if !current_path.is_empty() {
                                current_path.push('.');
                            }
                            current_path.push_str(&path_parts[i].base);
                            let bk = &path_parts[i].brackets;
                            let upto = if array_index.is_some() {
                                bk.len().saturating_sub(1)
                            } else {
                                bk.len()
                            };
                            for b in &bk[..upto] {
                                current_path.push('[');
                                current_path.push_str(b);
                                current_path.push(']');
                            }
                            current_idx = sd.find_element_by_path(&current_path, fisher);
                        } else {
                            // Ensure slicing exists.
                            if sd.elements[ext_idx].get("slicing").is_none() {
                                slice_it_value_url(sd, ext_idx);
                            }
                            let slice_name_for = if b0.starts_with("http") {
                                meta.id.clone()
                            } else {
                                b0.clone()
                            };
                            add_extension_slice(sd, ext_idx, &slice_name_for, url);
                            current_idx = sd.find_element_by_path(&current_path, fisher);
                        }
                    }
                }
            }
        }

        // resourceType pseudo-element on an inline resource.
        if current_idx.is_none() && path_parts[i].base == "resourceType" {
            if let Some(value) = value {
                if let FshValue::Str(_) = value {
                    return Some(Validated {
                        assigned_value: None,
                        path_parts,
                        child_path: None,
                    });
                }
            }
        }

        let cur = current_idx?;
        let max = el_max(sd, cur).map(|s| s.to_string());
        // Cannot resolve if max==0 or array index out of bounds.
        if max.as_deref() == Some("0") {
            return None;
        }
        if let Some(ai) = array_index {
            if let Some(m) = &max {
                if m != "*" && (m == "1" || ai >= m.parse::<i64>().unwrap_or(i64::MAX)) {
                    return None;
                }
            }
        }

        // base is array? add '0' bracket if needed.
        let base_max = sd.elements[cur]
            .get("base")
            .and_then(|b| b.get("max"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let base_is_array =
            base_max.as_deref().map(|m| m != "0" && m != "1").unwrap_or(false);
        let current_is_array =
            max.as_deref().map(|m| m != "0" && m != "1").unwrap_or(false);
        if base_is_array && (array_index.is_none() || !current_is_array) {
            path_parts[i].brackets.push("0".to_string());
        }

        // primitive?
        let types = el_type_codes(sd, cur);
        if types.len() == 1 && is_primitive_code(&types[0]) {
            path_parts[i].primitive = true;
        }

    }

    let cur = current_idx?;
    // Coerce the leaf value (assignValue with exactly=true → fixed[x]).
    let mut assigned_value = None;
    let mut child_path = None;
    if let Some(value) = value {
        let types = el_type_codes(sd, cur);
        if types.len() == 1 {
            if let Some((v, cp)) = coerce_value(&types[0], value, fisher) {
                assigned_value = Some(v);
                child_path = cp;
            }
        }
    }

    Some(Validated {
        assigned_value,
        path_parts,
        child_path,
    })
}

/// Find an existing extension slice on the SD by its type profile url. Mirrors
/// `findMatchingSlice`'s `fishForFHIR(...,Type.Extension)` branch
/// (StructureDefinition.ts:908-913): among siblings sharing the extension
/// element's path, return the sliceName of the one whose `type[0].profile[0]`
/// equals `url` and which has a sliceName.
fn find_ext_slice_by_profile(
    sd: &StructureDefinition,
    ext_idx: usize,
    url: &str,
) -> Option<String> {
    let ext_path = el_path(sd, ext_idx);
    for el in &sd.elements {
        if el.path() != ext_path {
            continue;
        }
        let Some(sn) = el.slice_name() else { continue };
        let profile0 = el
            .get("type")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|t| t.get("profile"))
            .and_then(|p| p.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str());
        if profile0 == Some(url) {
            return Some(sn.to_string());
        }
    }
    None
}

/// `sliceIt('value','url')` — set a value/url discriminator on element `idx`.
fn slice_it_value_url(sd: &mut StructureDefinition, idx: usize) {
    let slicing = serde_json::json!({
        "discriminator": [{ "type": "value", "path": "url" }],
        "ordered": false,
        "rules": "open"
    });
    sd.elements[idx].set("slicing", slicing);
}

/// Add an extension slice (`addSlice`) carrying `type[0].profile=[url]`.
fn add_extension_slice(sd: &mut StructureDefinition, parent_idx: usize, name: &str, url: &str) {
    // `add_slice` takes a SINGLE ElementDefinitionType object and wraps it in the
    // `type` array itself. Passing an array here produced a doubly-nested
    // `type:[[{...}]]`, so the slice's type code resolved to "" and `unfold`
    // couldn't fish the extension profile to expose its sub-extension slices
    // (e.g. ndh `extension[qualification].extension[code]` was dropped).
    let ty = serde_json::json!({ "code": "Extension", "profile": [url] });
    sd.add_slice(parent_idx, name, Some(ty));
}

// ---------------------------------------------------------------------------
// setPropertyOnInstance (port of common.ts:631).
// ---------------------------------------------------------------------------

fn set_property_on_instance(instance: &mut J, parts: &[IPathPart], assigned_value: &J) {
    if assigned_value.is_null() {
        // null assigned values are skipped (TS: assignedValue != null).
        // (But PathRules with no implied value pass null and do nothing.)
    }
    if matches!(assigned_value, J::Null) {
        return;
    }
    set_prop_rec(instance, parts, 0, assigned_value.clone());
}

fn ensure_obj(v: &mut J) -> &mut Map<String, J> {
    if !v.is_object() {
        *v = J::Object(Map::new());
    }
    v.as_object_mut().unwrap()
}

fn set_prop_rec(current: &mut J, parts: &[IPathPart], i: usize, assigned_value: J) {
    let pp = &parts[i];
    let last = i == parts.len() - 1;
    let key = if pp.primitive && !last {
        format!("_{}", pp.base)
    } else {
        pp.base.clone()
    };
    let index = get_array_index(pp);
    let slice_name_s = get_slice_name(pp);
    let slice_name = if slice_name_s.is_empty() {
        None
    } else {
        Some(slice_name_s)
    };

    let obj = ensure_obj(current);

    if let Some(mut index) = index {
        // Array handling.
        if pp.primitive {
            // ensure both base and _base arrays exist
            if obj.get(&pp.base).map(|v| v.is_null()).unwrap_or(true) {
                let mirror = obj
                    .get(&format!("_{}", pp.base))
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .map(|x| sliceish(x))
                            .collect::<Vec<J>>()
                    })
                    .unwrap_or_default();
                obj.insert(pp.base.clone(), J::Array(mirror));
            }
            if obj
                .get(&format!("_{}", pp.base))
                .map(|v| v.is_null())
                .unwrap_or(true)
            {
                let base_arr = obj.get(&pp.base).and_then(|v| v.as_array()).unwrap();
                let mirror: Vec<J> = base_arr.iter().map(|x| sliceish(x)).collect();
                obj.insert(format!("_{}", pp.base), J::Array(mirror));
            }
        } else if obj.get(&key).map(|v| v.is_null()).unwrap_or(true) {
            obj.insert(key.clone(), J::Array(vec![]));
        }

        // Resolve slice index → absolute index.
        if let Some(sn) = &slice_name {
            let base_arr_key = if pp.primitive { pp.base.clone() } else { key.clone() };
            let mut slice_indices: Vec<usize> = Vec::new();
            if let Some(arr) = obj.get(&base_arr_key).and_then(|v| v.as_array()) {
                for (ii, el) in arr.iter().enumerate() {
                    let matches_name =
                        el.get("_sliceName").and_then(|v| v.as_str()) == Some(sn.as_str());
                    if matches_name {
                        slice_indices.push(ii);
                    }
                }
            }
            let arr_len = obj.get(&key).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
            if (index as usize) >= slice_indices.len() {
                index = index - slice_indices.len() as i64 + arr_len as i64;
            } else {
                index = slice_indices[index as usize] as i64;
            }
        }

        let index = index as usize;
        // Grow arrays to index.
        grow_array(obj, pp, &key, index, slice_name.as_deref());

        if !last {
            // descend
            if pp.primitive {
                let arr = obj.get_mut(&pp.base).unwrap().as_array_mut().unwrap();
                if let Some(sn) = &slice_name {
                    if let Some(o) = arr[index].as_object_mut() {
                        o.insert("_sliceName".into(), J::String(sn.clone()));
                    }
                }
            }
            let arr = obj.get_mut(&key).unwrap().as_array_mut().unwrap();
            if let Some(sn) = &slice_name {
                if let Some(o) = arr[index].as_object_mut() {
                    o.insert("_sliceName".into(), J::String(sn.clone()));
                }
            }
            set_prop_rec(&mut arr[index], parts, i + 1, assigned_value);
        } else {
            // assign at leaf
            let mut av = assigned_value;
            if slice_name.is_some() && !av.is_object() {
                let mut m = Map::new();
                m.insert("assignedValue".into(), av);
                m.insert("_primitive".into(), J::Bool(true));
                av = J::Object(m);
            }
            let arr = obj.get_mut(&key).unwrap().as_array_mut().unwrap();
            if av.is_object() {
                let target = ensure_obj(&mut arr[index]);
                if let J::Object(src) = av {
                    for (k, v) in src {
                        target.insert(k, v);
                    }
                }
            } else {
                arr[index] = av;
            }
        }
    } else if !last {
        let child = obj.entry(key.clone()).or_insert_with(|| J::Object(Map::new()));
        set_prop_rec(child, parts, i + 1, assigned_value);
    } else {
        // scalar leaf
        match obj.get_mut(&key) {
            Some(existing) if existing.is_object() => {
                assign_complex_value(existing, &assigned_value);
            }
            _ => {
                if pp.primitive && assigned_value.is_object() {
                    let mut av = assigned_value.clone();
                    let avo = av.as_object_mut().unwrap();
                    if let Some(val) = avo.shift_remove("value") {
                        obj.insert(key.clone(), val);
                    }
                    if !avo.is_empty() {
                        obj.insert(format!("_{}", key), J::Object(avo.clone()));
                    }
                } else {
                    obj.insert(key.clone(), assigned_value);
                }
            }
        }
    }
}

fn sliceish(x: &J) -> J {
    if let Some(sn) = x.get("_sliceName") {
        let mut m = Map::new();
        m.insert("_sliceName".into(), sn.clone());
        J::Object(m)
    } else {
        J::Null
    }
}

fn grow_array(obj: &mut Map<String, J>, pp: &IPathPart, key: &str, index: usize, slice_name: Option<&str>) {
    let cur_len = obj.get(key).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    for j in 0..=index {
        if j < cur_len && j == index {
            // already exists; ensure non-null object if currently null
            let is_null = obj.get(key).and_then(|v| v.as_array()).map(|a| a[j].is_null()).unwrap_or(true);
            if is_null {
                if pp.primitive {
                    set_arr_if_null(obj, &pp.base, j, J::Object(Map::new()));
                    set_arr_if_null(obj, &format!("_{}", pp.base), j, J::Object(Map::new()));
                } else {
                    set_arr(obj, key, j, J::Object(Map::new()));
                }
            }
        } else if j >= cur_len {
            if let Some(sn) = slice_name {
                let mut m = Map::new();
                m.insert("_sliceName".into(), J::String(sn.to_string()));
                if pp.primitive {
                    push_arr(obj, &pp.base, J::Object(m.clone()));
                    push_arr(obj, &format!("_{}", pp.base), J::Object(m));
                } else {
                    push_arr(obj, key, J::Object(m));
                }
            } else if j == index {
                if pp.primitive {
                    push_arr(obj, &pp.base, J::Object(Map::new()));
                    push_arr(obj, &format!("_{}", pp.base), J::Object(Map::new()));
                } else {
                    push_arr(obj, key, J::Object(Map::new()));
                }
            } else {
                if pp.primitive {
                    push_arr(obj, &pp.base, J::Null);
                    push_arr(obj, &format!("_{}", pp.base), J::Null);
                } else {
                    push_arr(obj, key, J::Null);
                }
            }
        }
    }
}

fn push_arr(obj: &mut Map<String, J>, key: &str, v: J) {
    if let Some(a) = obj.get_mut(key).and_then(|x| x.as_array_mut()) {
        a.push(v);
    }
}
fn set_arr(obj: &mut Map<String, J>, key: &str, i: usize, v: J) {
    if let Some(a) = obj.get_mut(key).and_then(|x| x.as_array_mut()) {
        if i < a.len() {
            a[i] = v;
        }
    }
}
fn set_arr_if_null(obj: &mut Map<String, J>, key: &str, i: usize, v: J) {
    if let Some(a) = obj.get_mut(key).and_then(|x| x.as_array_mut()) {
        if i < a.len() && a[i].is_null() {
            a[i] = v;
        }
    }
}

/// `assignComplexValue` (common.ts:783) — merge an object/array assigned value
/// into an existing object/array, used for Quantity/Reference partial sets.
fn assign_complex_value(current: &mut J, assigned: &J) {
    match (current, assigned) {
        (J::Array(cur), J::Array(av)) => {
            for ae in av {
                if !ae.is_object() {
                    let exists = cur.iter().any(|ce| ce == ae);
                    if !exists {
                        cur.push(ae.clone());
                    }
                } else {
                    let ao = ae.as_object().unwrap();
                    let perfect = cur.iter().any(|ce| {
                        !ce.is_null()
                            && ao.iter().all(|(k, v)| ce.get(k) == Some(v))
                    });
                    if !perfect {
                        // partial match: a (possibly null) element where every
                        // assigned key is null or equal.
                        let partial = cur.iter().position(|ce| {
                            ce.is_null()
                                || ao.iter().all(|(k, v)| {
                                    ce.get(k).map(|c| c == v).unwrap_or(true)
                                })
                        });
                        if let Some(p) = partial {
                            if cur[p].is_null() {
                                cur[p] = J::Object(Map::new());
                            }
                            assign_complex_value(&mut cur[p], ae);
                        } else {
                            cur.push(ae.clone());
                        }
                    }
                }
            }
        }
        (J::Object(cur), J::Object(av)) => {
            for (k, v) in av {
                if v.is_object() || v.is_array() {
                    let child = cur.entry(k.clone()).or_insert_with(|| {
                        if v.is_array() {
                            J::Array(vec![])
                        } else {
                            J::Object(Map::new())
                        }
                    });
                    assign_complex_value(child, v);
                } else {
                    cur.insert(k.clone(), v.clone());
                }
            }
        }
        (cur, av) => {
            *cur = av.clone();
        }
    }
}

// ---------------------------------------------------------------------------
// setImpliedPropertiesOnInstance (port of common.ts:336).
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ElementTrace {
    /// Stable element id (indices shift when unfold inserts elements).
    id: String,
    /// '.'-joined path of ancestor segments (replaces the old `Vec<String>`;
    /// it was only ever joined, never indexed except `.last()` == `next_trace`).
    history: String,
    ghost: bool,
    requirement_root: String,
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn split_periods(s: &str) -> Vec<String> {
    crate::paths::split_on_path_periods(s)
}

/// Last '.'-segment of an element id, borrowed (no Vec/String allocation).
fn last_seg(s: &str) -> &str {
    crate::paths::last_path_period_segment(s)
}

/// Join a '.'-separated base path with one more segment (`base.seg`), or just
/// `seg` when `base` is empty. Pre-sized, single allocation.
fn join_seg(base: &str, seg: &str) -> String {
    if base.is_empty() {
        seg.to_string()
    } else {
        let mut s = String::with_capacity(base.len() + 1 + seg.len());
        s.push_str(base);
        s.push('.');
        s.push_str(seg);
        s
    }
}

/// `p == base || p.starts_with(&format!("{base}."))` with no allocation: `p`
/// equals `base` or is a deeper path (`base` immediately followed by `.`).
fn path_eq_or_under(p: &str, base: &str) -> bool {
    p == base
        || (p.len() > base.len() && p.as_bytes()[base.len()] == b'.' && p.starts_with(base))
}

/// Build slice tree counts feeding `effective_mins` (sliceTree.ts).
struct SliceNode {
    idx: usize,
    children: Vec<SliceNode>,
    count: i64,
}

fn build_slice_tree(sd: &StructureDefinition, idx: usize, ix: &StructIndex) -> SliceNode {
    let mut root = SliceNode {
        idx,
        children: vec![],
        count: 0,
    };
    for s in get_slices(sd, idx, ix) {
        insert_slice_tree(sd, &mut root, s);
    }
    root
}

fn insert_slice_tree(sd: &StructureDefinition, parent: &mut SliceNode, add: usize) {
    let add_name = el_slice_name(sd, add).unwrap_or_default();
    if let Some(child) = parent.children.iter_mut().find(|c| {
        // add_name.starts_with(&format!("{cn}/")) without allocating cn or the key.
        let cn = sd.elements[c.idx].slice_name().unwrap_or("");
        add_name.len() > cn.len()
            && add_name.as_bytes()[cn.len()] == b'/'
            && add_name.starts_with(cn)
    }) {
        insert_slice_tree(sd, child, add);
    } else {
        parent.children.push(SliceNode {
            idx: add,
            children: vec![],
            count: 0,
        });
    }
}

fn slice_tree_sum(node: &SliceNode) -> i64 {
    node.count + node.children.iter().map(slice_tree_sum).sum::<i64>()
}

fn calc_slice_tree(sd: &StructureDefinition, node: &mut SliceNode, known: &HashMap<String, i64>, key_start: &str) {
    for c in &mut node.children {
        calc_slice_tree(sd, c, known, key_start);
    }
    let elem_min = el_min(sd, node.idx) - node.children.iter().map(slice_tree_sum).sum::<i64>();
    let seg = reslice_brackets(last_seg(el_id_ref(sd, node.idx)));
    let slice_path = format!("{key_start}{seg}");
    let slice_min = known.get(&slice_path).copied().unwrap_or(0);
    node.count = elem_min.max(slice_min);
}

/// `replace(/:(.*)$/, '[$1]').replace(/\//g, '][')` on a single id segment.
fn reslice_brackets(seg: &str) -> String {
    if let Some(pos) = seg.find(':') {
        let (head, tail) = seg.split_at(pos);
        let tail = &tail[1..];
        format!("{head}[{}]", tail.replace('/', "]["))
    } else {
        seg.to_string()
    }
}

fn collect_effective_mins(sd: &StructureDefinition, node: &SliceNode, trace_path: &str, out: &mut HashMap<String, i64>) {
    let mut trace_key = trace_path.to_string();
    let sn = el_slice_name(sd, node.idx);
    let base_path = sd.elements[node.idx]
        .get("base")
        .and_then(|b| b.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if let Some(sn) = sn {
        if !base_path.ends_with("[x]") {
            trace_key = format!("{trace_path}[{}]", sn.replace('/', "]["));
        }
    }
    out.insert(trace_key, node.count);
    for c in &node.children {
        collect_effective_mins(sd, c, trace_path, out);
    }
}

#[allow(clippy::too_many_arguments)]
fn set_implied_properties_on_instance(
    instance: &mut J,
    sd: &mut StructureDefinition,
    paths: &[String],
    assigned_resource_paths: &[String],
    fisher: &dyn Fisher,
    known_slices: &HashMap<String, i64>,
    manual_slice_ordering: bool,
) {
    // normalize reslice style
    let paths: Vec<String> = paths.iter().map(|p| p.replace('/', "][")).collect();

    // Index `paths` so the per-node "does any rule path lie at-or-under this
    // trace_path" test is an O(1) lookup instead of a linear scan with
    // path_eq_or_under (this scan was the dominant self-cost: the BFS visits
    // many nodes and `paths` can be sizeable). For a rule path P, the set of
    // base/trace_paths T for which path_eq_or_under(P, T) holds is exactly P
    // itself plus each of its component-prefixes (the slices of P ending right
    // before a literal '.'). Map every such key to the FIRST (paths-order)
    // matching index, preserving `find`'s first-match semantics so we can still
    // recover the matched path for the assigned_resource_paths check.
    let mut first_match_path: HashMap<&str, usize> = HashMap::with_capacity(paths.len() * 2);
    for (i, p) in paths.iter().enumerate() {
        first_match_path.entry(p.as_str()).or_insert(i);
        for (j, &b) in p.as_bytes().iter().enumerate() {
            if b == b'.' {
                first_match_path.entry(&p[..j]).or_insert(i);
            }
        }
    }

    let mut sd_rule_map: Vec<(String, J)> = Vec::new();
    let mut requirement_roots: HashMap<String, String> = HashMap::new();
    let mut assigned_value_storage: HashMap<String, J> = HashMap::new();

    // Structural index over the (mutating) element tree; self-heals on unfold.
    let ix = StructIndex::new();

    // topLevelElements = elements[0].children(true)
    let mut queue: std::collections::VecDeque<ElementTrace> = std::collections::VecDeque::new();
    for c in children_direct(sd, 0, &ix) {
        let rr = compute_requirement_root(sd, c);
        queue.push_back(ElementTrace {
            id: el_id(sd, c),
            history: String::new(),
            ghost: false,
            requirement_root: rr,
        });
    }

    let mut effective_mins: HashMap<String, i64> = HashMap::new();

    while let Some(current) = queue.pop_front() {
        // Re-resolve index each iteration (unfold shifts the elements vec).
        let cur_idx = match sd.find_element(&current.id) {
            Some(i) => i,
            None => continue,
        };
        let mut next_trace = last_seg(el_id_ref(sd, cur_idx)).to_string();
        let types = el_type_codes(sd, cur_idx);
        if next_trace.contains("[x]") && types.len() == 1 {
            let has_slices = !get_slices(sd, cur_idx, &ix).is_empty();
            if el_slice_name(sd, cur_idx).is_some() || !has_slices {
                next_trace = replace_x(&next_trace, &upper_first(&types[0]));
            }
        }
        next_trace = reslice_brackets(&next_trace);
        let trace_path = join_seg(&current.history, &next_trace);

        if !effective_mins.contains_key(&trace_path) {
            let mut tree = build_slice_tree(sd, cur_idx, &ix);
            let mut key_start = current.history.clone();
            if !key_start.is_empty() {
                key_start.push('.');
            }
            calc_slice_tree(sd, &mut tree, known_slices, &key_start);
            collect_effective_mins(sd, &tree, &trace_path, &mut effective_mins);
        }
        let final_min = *effective_mins.get(&trace_path).unwrap_or(&0);

        let matching_idx = first_match_path.get(trace_path.as_str()).copied();

        // assigned value (fixed*/pattern*)
        let assigned_value_key = sd.elements[cur_idx]
            .map
            .keys()
            .find(|k| k.starts_with("fixed") || k.starts_with("pattern"))
            .cloned();
        let mut found_assigned = assigned_value_key
            .as_ref()
            .and_then(|k| sd.elements[cur_idx].get(k).cloned());
        let had_assigned = found_assigned.is_some();

        // `connected_ids` (an ancestor-chain slice walk) is only consumed when this
        // element carries an assigned value or `min > 0`; skip the walk otherwise.
        let cur_min = el_min(sd, cur_idx);
        let connected_ids: Vec<String> = if had_assigned || cur_min > 0 {
            find_connected_elements(sd, cur_idx, &ix)
                .iter()
                .map(|&i| el_id(sd, i))
                .collect()
        } else {
            Vec::new()
        };
        if !had_assigned {
            found_assigned = assigned_value_storage.get(&current.id).cloned();
        } else {
            for ce in &connected_ids {
                assigned_value_storage.insert(ce.clone(), found_assigned.clone().unwrap());
            }
        }

        // propagate min to connected
        if cur_min > 0 {
            for ce_id in &connected_ids {
                if let Some(ce) = sd.find_element(ce_id) {
                    if el_min(sd, ce) < cur_min && !ce_id.starts_with(&current.id) {
                        sd.elements[ce].set("min", J::Number(cur_min.into()));
                        if children_direct(sd, ce, &ix).is_empty() {
                            sd.unfold_by_id(ce_id, fisher);
                        }
                    }
                }
            }
        }

        // re-resolve after possible unfold
        let cur_idx = match sd.find_element(&current.id) {
            Some(i) => i,
            None => continue,
        };

        if final_min > 0 {
            if let Some(fa) = &found_assigned {
                if !current.ghost {
                    let mut ip = trace_path.clone();
                    if ip.contains("[x]") {
                        ip = fix_choice_path(sd, &ip, fisher).unwrap_or_default();
                    }
                    if !ip.is_empty() {
                        for idx in 0..final_min {
                            let numeric = if idx > 0 {
                                format!("{ip}[{idx}]")
                            } else {
                                ip.clone()
                            };
                            sd_rule_map.push((numeric.clone(), fa.clone()));
                            requirement_roots.insert(numeric, current.requirement_root.clone());
                        }
                    }
                }
            }
            // children
            let children: Vec<String> =
                children_direct(sd, cur_idx, &ix).iter().map(|&i| el_id(sd, i)).collect();
            let mut existing_slice_count = 0i64;
            if final_min < cur_min && el_slice_name(sd, cur_idx).is_none() {
                for s in get_slices(sd, cur_idx, &ix) {
                    let sp = format!("{trace_path}[{}]", el_slice_name(sd, s).unwrap_or_default());
                    if let Some(c) = known_slices.get(&sp) {
                        existing_slice_count += c;
                    }
                }
            }
            for idx in 0..final_min {
                let eff_idx = idx + existing_slice_count;
                let new_hist = if eff_idx > 0 {
                    format!("{next_trace}[{eff_idx}]")
                } else {
                    next_trace.clone()
                };
                let child_hist = join_seg(&current.history, &new_hist);
                let rr_inherit = cur_min > idx;
                for child in &children {
                    let rr = if rr_inherit {
                        current.requirement_root.clone()
                    } else {
                        child_hist.clone()
                    };
                    queue.push_back(ElementTrace {
                        id: child.clone(),
                        history: child_hist.clone(),
                        ghost: current.ghost,
                        requirement_root: rr,
                    });
                }
            }
        } else if matching_idx.is_some() || cur_min > 0 {
            if let (Some(_), Some(fa)) = (matching_idx, &found_assigned) {
                if !current.ghost {
                    sd_rule_map.push((trace_path.clone(), fa.clone()));
                    requirement_roots.insert(trace_path.clone(), current.requirement_root.clone());
                }
            }
            let mut children: Vec<String> =
                children_direct(sd, cur_idx, &ix).iter().map(|&i| el_id(sd, i)).collect();
            let is_assigned_resource = matching_idx
                .map(|m| assigned_resource_paths.contains(&paths[m]))
                .unwrap_or(false);
            if children.is_empty() && !is_assigned_resource {
                sd.unfold_by_id(&current.id, fisher);
                let cur_idx = sd.find_element(&current.id).unwrap();
                children = children_direct(sd, cur_idx, &ix).iter().map(|&i| el_id(sd, i)).collect();
            }
            let child_hist = join_seg(&current.history, &next_trace);
            let ghost = matching_idx.is_none();
            for child in &children {
                let child_min = sd.find_element(child).map(|i| el_min(sd, i)).unwrap_or(0);
                let rr = if child_min > 0 {
                    current.requirement_root.clone()
                } else {
                    child_hist.clone()
                };
                queue.push_back(ElementTrace {
                    id: child.clone(),
                    history: child_hist.clone(),
                    ghost,
                    requirement_root: rr,
                });
            }
        }
    }

    // Order: a path must come before its ancestors (tree postfix), then a
    // requirement-root/rule-order stable sort.
    let original_keys: Vec<String> = sd_rule_map.iter().map(|(k, _)| k.clone()).collect();
    let mut sorted = traverse_rule_path_tree(&build_path_tree(&original_keys));
    if !manual_slice_ordering {
        stable_sort_rule_paths(&mut sorted, &requirement_roots, &paths);
    }

    let value_for = |k: &str| -> Option<J> {
        sd_rule_map.iter().rev().find(|(p, _)| p == k).map(|(_, v)| v.clone())
    };

    for path in sorted {
        if let Some(validated) = validate_value_at_path(sd, &path, None, fisher) {
            if let Some(v) = value_for(&path) {
                set_property_on_instance(instance, &validated.path_parts, &v);
            }
        }
    }
}

fn compute_requirement_root(sd: &StructureDefinition, idx: usize) -> String {
    if el_min(sd, idx) > 0 {
        return String::new();
    }
    let mut rr = last_seg(el_id_ref(sd, idx)).to_string();
    let types = el_type_codes(sd, idx);
    if rr.contains("[x]") && types.len() == 1 {
        rr = replace_x(&rr, &upper_first(&types[0]));
    }
    reslice_brackets(&rr)
}

fn replace_x(s: &str, repl: &str) -> String {
    // replace /\[x].*/ with repl
    if let Some(pos) = s.find("[x]") {
        format!("{}{}", &s[..pos], repl)
    } else {
        s.to_string()
    }
}

fn fix_choice_path(sd: &mut StructureDefinition, ip: &str, fisher: &dyn Fisher) -> Option<String> {
    let parts = split_periods(ip);
    let mut out: Vec<String> = parts.clone();
    for (i, p) in parts.iter().enumerate() {
        if p.ends_with("[x]") {
            let sub = parts[..=i].join(".");
            if let Some(ei) = sd.find_element_by_path(&sub, fisher) {
                let types = el_type_codes(sd, ei);
                if types.len() == 1 {
                    out[i] = p.replace("[x]", &upper_first(&types[0]));
                }
            }
        }
    }
    let res = out.join(".");
    if res.contains("[x]") {
        None
    } else {
        Some(res)
    }
}

/// findConnectedElements (port). Returns indices.
fn find_connected_elements(sd: &StructureDefinition, idx: usize, ix: &StructIndex) -> Vec<usize> {
    find_connected_elements_post(sd, idx, "", ix)
}

fn find_connected_elements_post(
    sd: &StructureDefinition,
    idx: usize,
    post: &str,
    ix: &StructIndex,
) -> Vec<usize> {
    let mut out = Vec::new();
    for s in get_slices(sd, idx, ix) {
        if el_max(sd, s) == Some("0") {
            continue;
        }
        let target = format!("{}{}", el_id_ref(sd, s), post);
        if let Some(e) = sd.find_element(&target) {
            out.push(e);
        }
    }
    if let Some(p) = parent_idx(sd, idx) {
        let parent_path = last_seg(el_id_ref(sd, idx)).to_string();
        let mut more = find_connected_elements_post(sd, p, &format!(".{parent_path}{post}"), ix);
        out.append(&mut more);
    }
    out
}

// path tree (buildPathTree / traverseRulePathTree)
struct PNode {
    path: String,
    children: Vec<PNode>,
}

fn build_path_tree(paths: &[String]) -> Vec<PNode> {
    let mut top: Vec<PNode> = Vec::new();
    for p in paths {
        insert_into_tree(&mut top, p.clone());
    }
    top
}

fn insert_into_tree(current: &mut Vec<PNode>, path: String) {
    if let Some(parent) = current.iter_mut().find(|c| path.starts_with(&c.path)) {
        insert_into_tree(&mut parent.children, path);
    } else {
        let mut children = Vec::new();
        let mut i = 0;
        while i < current.len() {
            if current[i].path.starts_with(&path) {
                children.push(current.remove(i));
            } else {
                i += 1;
            }
        }
        current.push(PNode { path, children });
    }
}

fn traverse_rule_path_tree(nodes: &[PNode]) -> Vec<String> {
    let mut out = Vec::new();
    for n in nodes {
        out.extend(traverse_rule_path_tree(&n.children));
        out.push(n.path.clone());
    }
    out
}

fn stable_sort_rule_paths(
    paths: &mut [String],
    roots: &HashMap<String, String>,
    rule_paths: &[String],
) {
    // stable sort by comparator (mergesort = stable).
    let cmp = |a: &String, b: &String| -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let a_root = roots.get(a).cloned().unwrap_or_default();
        let b_root = roots.get(b).cloned().unwrap_or_default();
        if a_root == b_root {
            let first_rule = rule_paths
                .iter()
                .find(|p| path_eq_or_under(p, &a_root));
            if let Some(fr) = first_rule {
                let fr_split = crate::paths::split_on_path_periods_borrowed(fr);
                let a_split = crate::paths::split_on_path_periods_borrowed(a);
                let b_split = crate::paths::split_on_path_periods_borrowed(b);
                let maxlen = fr_split.len().max(a_split.len()).max(b_split.len());
                for i in 0..maxlen {
                    let fp = fr_split.get(i);
                    let ap = a_split.get(i);
                    let bp = b_split.get(i);
                    match fp {
                        None => return Ordering::Equal,
                        Some(fp) => {
                            let a_eq = ap == Some(fp);
                            let b_eq = bp == Some(fp);
                            if a_eq && !b_eq {
                                return Ordering::Less;
                            }
                            if !a_eq && b_eq {
                                return Ordering::Greater;
                            }
                            if !a_eq && !b_eq {
                                return Ordering::Equal;
                            }
                        }
                    }
                }
            }
            return Ordering::Equal;
        }
        let first_a = rule_paths
            .iter()
            .position(|p| path_eq_or_under(p, &a_root));
        let first_b = rule_paths
            .iter()
            .position(|p| path_eq_or_under(p, &b_root));
        if first_a == first_b {
            return b_root.len().cmp(&a_root.len());
        }
        let fa = first_a.map(|x| x as i64).unwrap_or(-1);
        let fb = first_b.map(|x| x as i64).unwrap_or(-1);
        fa.cmp(&fb)
    };
    // stable sort
    let mut indexed: Vec<(usize, String)> = paths.iter().cloned().enumerate().collect();
    indexed.sort_by(|x, y| cmp(&x.1, &y.1).then(x.0.cmp(&y.0)));
    for (i, (_, p)) in indexed.into_iter().enumerate() {
        paths[i] = p;
    }
}

// ---------------------------------------------------------------------------
// determineKnownSlices (port of common.ts:281).
// ---------------------------------------------------------------------------

fn determine_known_slices(
    sd: &mut StructureDefinition,
    rule_map: &[(String, Vec<IPathPart>)],
    fisher: &dyn Fisher,
) -> HashMap<String, i64> {
    let mut known = HashMap::new();
    for (_path, parts) in rule_map {
        // Gate on the REWRITTEN parts (which carry the resolved sliceName), not the
        // raw rule path. When a bracket was an extension name/url/alias that differs
        // from the slice's sliceName (e.g. `extension[RecommendedAction]` -> the
        // inherited `recommended-action` slice), the raw path won't resolve via
        // find_element_by_path and the slice would be miscounted — dropping implied
        // urls and corrupting slice order. Mirrors stock rewriting `rule.path`.
        let non_numeric = strip_numeric_brackets(&assemble_fsh_path(parts));
        if sd.find_element_by_path(&non_numeric, fisher).is_some() {
            let mut current_path = String::new();
            for pp in parts {
                if !current_path.is_empty() {
                    current_path.push('.');
                }
                current_path.push_str(&pp.base);
                if let Some(ri) = get_array_index(pp) {
                    let sn = if !pp.brackets.is_empty() {
                        get_slice_name(pp)
                    } else {
                        String::new()
                    };
                    if !sn.is_empty() {
                        let slice_path = format!("{current_path}[{}]", sn.replace('/', "]["));
                        let e = known.entry(slice_path).or_insert(0);
                        *e = (*e).max(ri + 1);
                    } else {
                        let e = known.entry(current_path.clone()).or_insert(0);
                        *e = (*e).max(ri + 1);
                    }
                    for b in pp.brackets.iter().filter(|b| *b != "0") {
                        current_path.push('[');
                        current_path.push_str(b);
                        current_path.push(']');
                    }
                }
            }
        }
    }
    known
}

/// `createUsefulSlices` (common.ts:132). Mutates `instance` to add placeholder
/// elements (establishing key order) AND returns knownSlices. Used when
/// manualSliceOrdering is enabled.
fn create_useful_slices(
    instance: &mut J,
    sd: &mut StructureDefinition,
    rule_map: &[(String, Vec<IPathPart>)],
    fisher: &dyn Fisher,
) -> HashMap<String, i64> {
    let mut known = HashMap::new();
    for (path, parts) in rule_map {
        let non_numeric = strip_numeric_brackets(path);
        if sd.find_element_by_path(&non_numeric, fisher).is_none() {
            continue;
        }
        let mut current: &mut J = instance;
        let mut current_path = String::new();
        let n = parts.len();
        for (i, pp) in parts.iter().enumerate() {
            if !current_path.is_empty() {
                current_path.push('.');
            }
            current_path.push_str(&pp.base);
            let key = if pp.primitive && i < n - 1 {
                format!("_{}", pp.base)
            } else {
                pp.base.clone()
            };
            let rule_index = get_array_index(pp);
            if let Some(rule_index) = rule_index {
                let obj = ensure_obj(current);
                // ensure arrays exist
                if pp.primitive {
                    obj.entry(pp.base.clone()).or_insert_with(|| J::Array(vec![]));
                    obj.entry(format!("_{}", pp.base)).or_insert_with(|| J::Array(vec![]));
                } else {
                    obj.entry(key.clone()).or_insert_with(|| J::Array(vec![]));
                }
                let slice_name_s = get_slice_name(pp);
                let slice_name = if slice_name_s.is_empty() { None } else { Some(slice_name_s) };
                let mut effective_index = rule_index;
                if let Some(sn) = &slice_name {
                    let slice_path = format!("{current_path}[{}]", sn.replace('/', "]["));
                    let e = known.entry(slice_path).or_insert(0);
                    *e = (*e).max(rule_index + 1);
                    // slice indices
                    let ext_url = fisher.fish_for_metadata(sn).and_then(|m| m.url);
                    let mut slice_indices: Vec<i64> = Vec::new();
                    if let Some(arr) = obj.get(&pp.base).and_then(|v| v.as_array()) {
                        for (ii, el) in arr.iter().enumerate() {
                            let by_name = el.get("_sliceName").and_then(|v| v.as_str()) == Some(sn.as_str());
                            let by_url = is_extension_base(&pp.base)
                                && el.get("url").is_some()
                                && el.get("url").and_then(|v| v.as_str()) == ext_url.as_deref();
                            if by_name || by_url {
                                slice_indices.push(ii as i64);
                            }
                        }
                    }
                    let base_len = obj.get(&pp.base).and_then(|v| v.as_array()).map(|a| a.len() as i64).unwrap_or(0);
                    if rule_index >= slice_indices.len() as i64 {
                        effective_index = rule_index - slice_indices.len() as i64 + base_len;
                    } else {
                        effective_index = slice_indices[rule_index as usize];
                    }
                } else {
                    let e = known.entry(current_path.clone()).or_insert(0);
                    *e = (*e).max(effective_index + 1);
                }
                // update current_path with brackets (non-zero)
                for b in pp.brackets.iter().filter(|b| *b != "0") {
                    current_path.push('[');
                    current_path.push_str(b);
                    current_path.push(']');
                }
                grow_array(obj, pp, &key, effective_index as usize, slice_name.as_deref());
                if i == n - 1 {
                    break;
                }
                let arr = obj.get_mut(&key).unwrap().as_array_mut().unwrap();
                current = &mut arr[effective_index as usize];
            } else if i < n - 1 {
                let obj = ensure_obj(current);
                current = obj.entry(key.clone()).or_insert_with(|| J::Object(Map::new()));
            } else {
                break;
            }
        }
    }
    known
}

fn strip_numeric_brackets(path: &str) -> String {
    // remove [<+/-digits>] segments
    let mut out = String::new();
    let chars: Vec<char> = path.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            // find closing
            let mut j = i + 1;
            let mut inner = String::new();
            while j < chars.len() && chars[j] != ']' {
                inner.push(chars[j]);
                j += 1;
            }
            let numeric = !inner.is_empty()
                && inner.chars().all(|c| c.is_ascii_digit() || c == '-' || c == '+');
            if numeric {
                i = j + 1;
                continue;
            } else {
                out.push('[');
                out.push_str(&inner);
                if j < chars.len() {
                    out.push(']');
                }
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// cleanResource (port of common.ts:1192).
// ---------------------------------------------------------------------------

fn clean_resource(v: &mut J) {
    remove_slice_names(v);
    empty_to_null(v);
    unprimitive(v);
    delete_all_null_arrays(v);
    rewrite_contained_references(v);
}

/// Rewrite `reference` fields pointing at a contained resource to `#id`
/// (common.ts:1219).
fn rewrite_contained_references(v: &mut J) {
    let contained: Vec<(String, String)> = v
        .get("contained")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let rt = r.get("resourceType").and_then(|x| x.as_str())?;
                    let id = r.get("id").and_then(|x| x.as_str())?;
                    Some((format!("{rt}/{id}"), id.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    let urls: Vec<(String, String)> = v
        .get("contained")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let url = r.get("url").and_then(|x| x.as_str())?;
                    let id = r.get("id").and_then(|x| x.as_str())?;
                    Some((url.to_string(), id.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    for (refstr, id) in contained.iter().chain(urls.iter()) {
        replace_reference_field(v, refstr, id);
    }
}

fn replace_reference_field(v: &mut J, refstr: &str, id: &str) {
    match v {
        J::Object(m) => {
            for (k, val) in m.iter_mut() {
                if k == "reference" && val.as_str() == Some(refstr) {
                    *val = J::String(format!("#{id}"));
                } else {
                    replace_reference_field(val, refstr, id);
                }
            }
        }
        J::Array(a) => {
            for x in a.iter_mut() {
                replace_reference_field(x, refstr, id);
            }
        }
        _ => {}
    }
}

fn remove_slice_names(v: &mut J) {
    match v {
        J::Object(m) => {
            m.shift_remove("_sliceName");
            for (_, val) in m.iter_mut() {
                remove_slice_names(val);
            }
        }
        J::Array(a) => {
            for x in a.iter_mut() {
                remove_slice_names(x);
            }
        }
        _ => {}
    }
}

fn empty_to_null(v: &mut J) {
    match v {
        J::Object(m) => {
            for (_, val) in m.iter_mut() {
                if is_empty_obj(val) {
                    *val = J::Null;
                } else {
                    empty_to_null(val);
                    if is_empty_obj(val) {
                        *val = J::Null;
                    }
                }
            }
        }
        J::Array(a) => {
            for x in a.iter_mut() {
                if is_empty_obj(x) {
                    *x = J::Null;
                } else {
                    empty_to_null(x);
                    if is_empty_obj(x) {
                        *x = J::Null;
                    }
                }
            }
        }
        _ => {}
    }
}

fn is_empty_obj(v: &J) -> bool {
    match v {
        J::Object(m) => m.is_empty(),
        _ => false,
    }
}

fn unprimitive(v: &mut J) {
    match v {
        J::Object(m) => {
            let keys: Vec<String> = m.keys().cloned().collect();
            for k in keys {
                if let Some(val) = m.get(&k) {
                    if val.get("_primitive").and_then(|x| x.as_bool()) == Some(true) {
                        let av = val.get("assignedValue").cloned().unwrap_or(J::Null);
                        m.insert(k.clone(), av);
                        continue;
                    }
                }
                if let Some(val) = m.get_mut(&k) {
                    unprimitive(val);
                }
            }
        }
        J::Array(a) => {
            for x in a.iter_mut() {
                if x.get("_primitive").and_then(|p| p.as_bool()) == Some(true) {
                    *x = x.get("assignedValue").cloned().unwrap_or(J::Null);
                } else {
                    unprimitive(x);
                }
            }
        }
        _ => {}
    }
}

fn delete_all_null_arrays(v: &mut J) {
    match v {
        J::Object(m) => {
            let keys: Vec<String> = m.keys().cloned().collect();
            for k in keys {
                let del = m
                    .get(&k)
                    .and_then(|val| val.as_array())
                    .map(|a| !a.is_empty() && a.iter().all(|x| x.is_null()))
                    .unwrap_or(false);
                if del {
                    m.shift_remove(&k);
                } else if let Some(val) = m.get_mut(&k) {
                    delete_all_null_arrays(val);
                }
            }
        }
        J::Array(a) => {
            for x in a.iter_mut() {
                delete_all_null_arrays(x);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// merge (lodash merge semantics).
// ---------------------------------------------------------------------------

fn merge(target: &mut J, source: &J) {
    match (target, source) {
        (J::Object(t), J::Object(s)) => {
            for (k, v) in s {
                if v.is_null() {
                    continue;
                }
                match t.get_mut(k) {
                    Some(tv) if tv.is_object() || tv.is_array() => merge(tv, v),
                    _ => {
                        t.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (J::Array(t), J::Array(s)) => {
            for (i, v) in s.iter().enumerate() {
                if i < t.len() {
                    if t[i].is_object() || t[i].is_array() {
                        merge(&mut t[i], v);
                    } else if !v.is_null() {
                        t[i] = v.clone();
                    }
                } else {
                    t.push(v.clone());
                }
            }
        }
        (t, s) => {
            if !s.is_null() {
                *t = s.clone();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Reference / code resolution index (replaceReferences support).
// ---------------------------------------------------------------------------

pub struct InstanceIndex {
    /// name|id -> (instanceOf, id)
    by_ref: HashMap<String, (String, String)>,
    /// name|id -> url (only for instances carrying a `* url = "..."` rule), for
    /// resolving `Canonical(instanceName)` to a local instance's canonical url.
    inst_url: HashMap<String, String>,
}

impl InstanceIndex {
    fn build(docs: &[FshDocument]) -> InstanceIndex {
        let mut by_ref = HashMap::new();
        let mut inst_url = HashMap::new();
        for doc in docs {
            for (_k, inst) in &doc.instances {
                if let Some(io) = &inst.instance_of {
                    let id = effective_instance_id(inst);
                    by_ref
                        .entry(inst.name.clone())
                        .or_insert((io.clone(), id.clone()));
                    by_ref
                        .entry(id.clone())
                        .or_insert((io.clone(), id.clone()));
                    if let Some(url) = effective_instance_url(inst) {
                        inst_url.entry(inst.name.clone()).or_insert(url.clone());
                        inst_url.entry(id).or_insert(url);
                    }
                }
            }
        }
        InstanceIndex { by_ref, inst_url }
    }
}

/// The effective canonical url of an instance: the last `* url = "..."` rule.
fn effective_instance_url(inst: &Instance) -> Option<String> {
    for r in inst.rules.iter().rev() {
        if let Rule::Assignment { path, value: Some(FshValue::Str(s)), .. } = r {
            if path == "url" {
                return Some(s.clone());
            }
        }
    }
    None
}

/// The effective instance id: the last `* id = "..."` AssignmentRule's value,
/// else the declared id (which defaults to the instance name).
fn effective_instance_id(inst: &Instance) -> String {
    for r in inst.rules.iter().rev() {
        if let Rule::Assignment { path, value: Some(FshValue::Str(s)), .. } = r {
            if path == "id" {
                return s.clone();
            }
        }
    }
    inst.id.clone()
}

/// Local definition index for reference/canonical resolution.
struct DefIndex {
    /// name|id -> (kind_type, id)  where kind_type is "StructureDefinition" | "CodeSystem" | "ValueSet"
    by_ref: HashMap<String, (String, String)>,
    /// code-system name|id -> url
    cs_url: HashMap<String, String>,
    /// value-set name|id -> url
    vs_url: HashMap<String, String>,
}

impl DefIndex {
    fn build(docs: &[FshDocument], cfg: &Config) -> DefIndex {
        let mut by_ref = HashMap::new();
        let mut cs_url = HashMap::new();
        let mut vs_url = HashMap::new();
        let mut add_sd = |name: &str, id: &str| {
            by_ref
                .entry(name.to_string())
                .or_insert(("StructureDefinition".to_string(), id.to_string()));
            by_ref
                .entry(id.to_string())
                .or_insert(("StructureDefinition".to_string(), id.to_string()));
        };
        for doc in docs {
            for (_k, d) in doc.profiles.iter().chain(&doc.extensions).chain(&doc.logicals).chain(&doc.resources) {
                add_sd(&d.name, &d.id);
            }
        }
        for doc in docs {
            for (_k, cs) in &doc.code_systems {
                let id = crate::export::effective_id(&cs.rules, &cs.id);
                let url = format!("{}/CodeSystem/{}", cfg.canonical, id);
                by_ref
                    .entry(cs.name.clone())
                    .or_insert(("CodeSystem".to_string(), id.clone()));
                by_ref
                    .entry(id.clone())
                    .or_insert(("CodeSystem".to_string(), id.clone()));
                cs_url.insert(cs.name.clone(), url.clone());
                cs_url.insert(id.clone(), url);
            }
            for (_k, vs) in &doc.value_sets {
                let id = crate::export::effective_id(&vs.rules, &vs.id);
                let url = format!("{}/ValueSet/{}", cfg.canonical, id);
                by_ref
                    .entry(vs.name.clone())
                    .or_insert(("ValueSet".to_string(), id.clone()));
                by_ref
                    .entry(id.clone())
                    .or_insert(("ValueSet".to_string(), id.clone()));
                vs_url.insert(vs.name.clone(), url.clone());
                vs_url.insert(id.clone(), url);
            }
        }
        DefIndex { by_ref, cs_url, vs_url }
    }
}

/// `replaceReferences` for a single assignment value.
fn replace_references(
    value: &mut FshValue,
    inst_idx: &InstanceIndex,
    def_idx: &DefIndex,
    fisher: &dyn Fisher,
) {
    match value {
        FshValue::Reference(r) => {
            let base = r.reference.split('|').next().unwrap_or(&r.reference).to_string();
            let resolved: Option<(String, String)> = if let Some((io, id)) = inst_idx.by_ref.get(&base) {
                fisher.fish_for_metadata(io).and_then(|m| m.sd_type).map(|t| (t, id.clone()))
            } else if let Some((t, id)) = def_idx.by_ref.get(&base) {
                Some((t.clone(), id.clone()))
            } else {
                fisher.fish_for_fhir(&base).and_then(|j| {
                    let rt = j.get("resourceType").and_then(|v| v.as_str());
                    let id = j.get("id").and_then(|v| v.as_str());
                    match (rt, id) {
                        (Some(rt), Some(id)) => Some((rt.to_string(), id.to_string())),
                        _ => None,
                    }
                })
            };
            if let Some((t, id)) = resolved {
                if !r.reference.contains('/') {
                    r.reference = format!("{t}/{id}");
                }
            }
        }
        FshValue::Code(fc) => {
            if let Some(sys) = &fc.system {
                // `replaceReferences` FshCode branch: fish the system name as a
                // CodeSystem (local FSH CodeSystems first, then dependency packages)
                // and substitute its canonical url, preserving any |version.
                let resolve_cs = |base: &str| {
                    def_idx
                        .cs_url
                        .get(base)
                        .cloned()
                        .or_else(|| fisher.fish_for_metadata_cs(base).and_then(|m| m.url))
                };
                if let Some(new_sys) = crate::export::replace_code_system(sys, resolve_cs) {
                    fc.system = Some(new_sys);
                }
            }
        }
        FshValue::Canonical(c) => {
            // Resolve a local ValueSet/CodeSystem/Instance name to its canonical
            // url. (Local SDs + package resources are resolved later in
            // coerce_value via the fisher; per ElementDefinition.ts:2006 stock
            // fishes SD types BEFORE ValueSet/CodeSystem/Instance, so only fall
            // back to these locals when the fisher can't resolve an SD url.)
            if fisher.fish_for_metadata(&c.entity_name).and_then(|m| m.url).is_none() {
                if let Some(url) = def_idx
                    .vs_url
                    .get(&c.entity_name)
                    .or_else(|| def_idx.cs_url.get(&c.entity_name))
                    .or_else(|| inst_idx.inst_url.get(&c.entity_name))
                {
                    c.entity_name = url.clone();
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Main export.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ExportedInst {
    body: J,
    filename: String,
    write: bool,
    ig: IgInstanceMeta,
}

/// Metadata an instance contributes to the generated ImplementationGuide
/// `definition.resource` entry (mirrors `IGExporter.addPackageResource` for
/// `InstanceDefinition`s).
#[derive(Clone)]
pub struct IgInstanceMeta {
    /// `${sdKind==='logical'?'Binary':resourceType}/${id}`
    pub reference_key: String,
    pub name: Option<String>,
    pub description: Option<String>,
    /// Effective usage after datatype→Inline forcing.
    pub usage: String,
    /// `_instanceMeta.sdKind === 'logical'`.
    pub logical: bool,
    /// The InstanceOf StructureDefinition's url (`_instanceMeta.instanceOfUrl`).
    pub instance_of_url: Option<String>,
    /// `meta.profile` urls present on the exported instance body.
    pub meta_profile: Vec<String>,
}

/// One written instance plus its IG metadata.
pub struct InstanceExport {
    pub exported: crate::export::Exported,
    pub ig: IgInstanceMeta,
}

struct Exporter<'a> {
    cfg: &'a Config,
    ctx: &'a SdContext<'a>,
    inst_idx: InstanceIndex,
    def_idx: DefIndex,
    by_name: HashMap<String, &'a Instance>,
    /// effective instance id -> name (for fishing inline instances by id).
    id_to_name: HashMap<String, String>,
    /// FSH alias name -> value (first-wins), resolved on every fish like FSHTank.fish.
    aliases: HashMap<String, String>,
    memo: std::cell::RefCell<HashMap<String, Option<ExportedInst>>>,
    /// Cache of freshly-parsed (unmutated) InstanceOf snapshots keyed by base
    /// type name. Many instances share a profile (mcode: 209 instances → 64
    /// distinct InstanceOf), and fishing+parsing the snapshot per instance is
    /// redundant. The template is never mutated; each export gets a copy-on-write
    /// clone (cheap: element maps are shared `Rc`s until a write forks them, and
    /// the template's lazy id-index stays `None` so clones don't copy it).
    sd_cache: std::cell::RefCell<HashMap<String, Option<Rc<StructureDefinition>>>>,
}

/// Wraps a fisher so that the queried name is alias-resolved first, mirroring
/// `FSHTank.fish` (`item = this.resolveAlias(item) ?? item`). Stock's MasterFisher
/// delegates to the tank, which resolves aliases before every fish.
struct AliasFisher<'a> {
    inner: &'a dyn Fisher,
    aliases: &'a HashMap<String, String>,
}

impl AliasFisher<'_> {
    fn resolve<'n>(&self, name: &'n str) -> std::borrow::Cow<'n, str> {
        match self.aliases.get(name) {
            Some(v) => std::borrow::Cow::Owned(v.clone()),
            None => std::borrow::Cow::Borrowed(name),
        }
    }
}

impl Fisher for AliasFisher<'_> {
    fn fish_for_fhir(&self, name: &str) -> Option<std::rc::Rc<J>> {
        self.inner.fish_for_fhir(&self.resolve(name))
    }
    fn fish_for_metadata(&self, name: &str) -> Option<fhir_model::Metadata> {
        self.inner.fish_for_metadata(&self.resolve(name))
    }
    fn fish_for_metadata_cs(&self, name: &str) -> Option<fhir_model::Metadata> {
        self.inner.fish_for_metadata_cs(&self.resolve(name))
    }
}

pub fn export_instances(
    docs: &[FshDocument],
    cfg: &Config,
    ctx: &SdContext,
) -> Vec<InstanceExport> {
    let mut by_name: HashMap<String, &Instance> = HashMap::new();
    let mut id_to_name: HashMap<String, String> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for doc in docs {
        for (_k, inst) in &doc.instances {
            if !by_name.contains_key(&inst.name) {
                by_name.insert(inst.name.clone(), inst);
                id_to_name
                    .entry(effective_instance_id(inst))
                    .or_insert_with(|| inst.name.clone());
                order.push(inst.name.clone());
            }
        }
    }
    let mut aliases: HashMap<String, String> = HashMap::new();
    for doc in docs {
        for (k, v) in &doc.aliases {
            aliases.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    let exporter = Exporter {
        cfg,
        ctx,
        inst_idx: InstanceIndex::build(docs),
        def_idx: DefIndex::build(docs, cfg),
        by_name,
        id_to_name,
        aliases,
        memo: std::cell::RefCell::new(HashMap::new()),
        sd_cache: std::cell::RefCell::new(HashMap::new()),
    };

    let mut out = Vec::new();
    for name in &order {
        if let Some(e) = exporter.export(name) {
            if e.write {
                out.push(InstanceExport {
                    exported: crate::export::Exported {
                        filename: e.filename,
                        body: e.body,
                    },
                    ig: e.ig,
                });
            }
        }
    }
    out
}

impl<'a> Exporter<'a> {
    /// Fish an exported InstanceDefinition body by FSH name (for inline-instance
    /// assignment). Returns the ordered JSON to embed.
    fn fish_instance(&self, name: &str) -> Option<J> {
        let base = name.split('|').next().unwrap_or(name);
        if self.by_name.contains_key(base) {
            return self.export(base).map(|e| e.body);
        }
        if let Some(real) = self.id_to_name.get(base) {
            return self.export(real).map(|e| e.body);
        }
        // External instance: fish raw FHIR JSON from packages. This body is owned
        // by the AssignRule and mutated downstream, so clone out of the shared Rc.
        self.ctx.fisher().fish_for_fhir(base).map(|rc| (*rc).clone())
    }

    fn export(&self, name: &str) -> Option<ExportedInst> {
        if let Some(cached) = self.memo.borrow().get(name) {
            return cached.clone();
        }
        // In-progress guard against circular inline instances.
        self.memo.borrow_mut().insert(name.to_string(), None);
        let result = self.export_compute(name);
        self.memo.borrow_mut().insert(name.to_string(), result.clone());
        result
    }

    /// Fish + parse the InstanceOf snapshot for `base`, memoizing the unmutated
    /// template and returning a copy-on-write clone for this export to mutate.
    fn fish_sd_template(&self, base: &str) -> Option<StructureDefinition> {
        if let Some(cached) = self.sd_cache.borrow().get(base) {
            return cached.as_ref().map(|rc| (**rc).clone());
        }
        let parsed = self
            .ctx
            .fish_sd_json(base)
            .map(|json| Rc::new(StructureDefinition::from_json(&json, false)));
        let out = parsed.as_ref().map(|rc| (**rc).clone());
        self.sd_cache.borrow_mut().insert(base.to_string(), parsed);
        out
    }

    fn export_compute(&self, name: &str) -> Option<ExportedInst> {
        let inst = *self.by_name.get(name)?;
        let inner_fisher = self.ctx.fisher();
        let fisher = AliasFisher { inner: &inner_fisher, aliases: &self.aliases };
        let instance_of = inst.instance_of.as_ref()?;
        let base = instance_of.split('|').next().unwrap_or(instance_of);
        let mut sd = self.fish_sd_template(base)?;
        if sd.elements.is_empty() {
            return None;
        }
        let kind = sd.kind().to_string();
        let is_resource = kind == "resource" || kind == "logical";

        // Usage: non-resource (datatype) instances are forced to Inline.
        let mut usage = inst.usage.clone();
        if !is_resource && usage != "Inline" {
            usage = "Inline".to_string();
        }
        let is_inline = usage == "Inline";

        let mut instance: J = J::Object(Map::new());
        let obj = instance.as_object_mut().unwrap();
        if is_resource {
            obj.insert("resourceType".into(), J::String(sd.type_().to_string()));
            if self.should_set_id(&sd, is_inline) {
                obj.insert("id".into(), J::String(inst.id.clone()));
            }
        }
        // Usage: #definition sets url/title/description from metadata.
        if usage == "Definition" {
            self.set_definition_metadata(inst, obj, &sd);
        }

        // Apply assigned values.
        self.set_assigned_values(inst, &mut instance, &mut sd, &fisher);

        // meta.profile (respecting instanceOptions.setMetaProfile).
        if self.should_set_meta_profile(&sd, is_inline) {
            apply_meta_profile(&mut instance, &sd, instance_of);
        }

        clean_resource(&mut instance);
        let body = order_instance(&instance);

        let type_name = if kind == "logical" {
            "Binary".to_string()
        } else {
            sd.type_().to_string()
        };
        let id = body
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or(&inst.id)
            .to_string();
        let filename = sanitize(&format!("{type_name}-{id}.json"));

        // IG resource metadata (IGExporter.addPackageResource for instances).
        let body_title = body.get("title").and_then(|v| v.as_str()).map(str::to_string);
        let body_description = body
            .get("description")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let ig_name = inst
            .title
            .clone()
            .or(body_title)
            .or_else(|| Some(inst.name.clone()));
        let ig_description = inst.description.clone().or(body_description);
        let meta_profile = body
            .get("meta")
            .and_then(|m| m.get("profile"))
            .and_then(|p| p.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let ig = IgInstanceMeta {
            reference_key: format!("{type_name}/{id}"),
            name: ig_name,
            description: ig_description,
            usage: usage.clone(),
            logical: kind == "logical",
            instance_of_url: sd.get_str("url").map(str::to_string),
            meta_profile,
        };

        Some(ExportedInst {
            body,
            filename,
            write: !is_inline,
            ig,
        })
    }

    fn should_set_id(&self, sd: &StructureDefinition, is_inline: bool) -> bool {
        let set_id = self
            .cfg
            .instance_options
            .as_ref()
            .and_then(|o| o.set_id.as_deref());
        if set_id == Some("standalone-only") && is_inline {
            return false;
        }
        should_set_id(sd)
    }

    fn should_set_meta_profile(&self, sd: &StructureDefinition, is_inline: bool) -> bool {
        let cfg_meta = self
            .cfg
            .instance_options
            .as_ref()
            .and_then(|o| o.set_meta_profile.as_deref());
        match cfg_meta {
            Some("never") => return false,
            Some("inline-only") if !is_inline => return false,
            Some("standalone-only") if is_inline => return false,
            _ => {}
        }
        should_set_meta_profile(sd)
    }

    /// Usage: #definition — set url/title/description if the SD has those elements.
    fn set_definition_metadata(&self, inst: &Instance, obj: &mut Map<String, J>, sd: &StructureDefinition) {
        let pt = sd.path_type();
        let has = |suffix: &str| sd.elements.iter().any(|e| e.id() == format!("{pt}.{suffix}"));
        if has("url") {
            obj.insert(
                "url".into(),
                J::String(format!("{}/{}/{}", self.cfg.canonical, pt, inst.id)),
            );
        }
        if let Some(t) = &inst.title {
            if has("title") {
                obj.insert("title".into(), J::String(t.clone()));
            }
        }
        if let Some(d) = &inst.description {
            if has("description") {
                obj.insert("description".into(), J::String(d.clone()));
            }
        }
    }
}

fn should_set_id(sd: &StructureDefinition) -> bool {
    let pt = sd.path_type();
    sd.elements.iter().any(|e| {
        e.path() == format!("{pt}.id")
            && e.type_codes().first().map(|t| t == "string" || t == "id").unwrap_or(false)
            && e.get("max").and_then(|v| v.as_str()) == Some("1")
            && e.get("base")
                .and_then(|b| b.get("max"))
                .and_then(|v| v.as_str())
                .map(|m| m == "1")
                .unwrap_or(true)
    })
}

fn should_set_meta_profile(sd: &StructureDefinition) -> bool {
    if sd.derivation() != "constraint" {
        return false;
    }
    let pt = sd.path_type();
    sd.elements.iter().any(|e| {
        e.path() == format!("{pt}.meta")
            && e.type_codes().first().map(|t| t == "Meta").unwrap_or(false)
            && e.get("max").and_then(|v| v.as_str()) == Some("1")
            && e.get("base")
                .and_then(|b| b.get("max"))
                .and_then(|v| v.as_str())
                .map(|m| m == "1")
                .unwrap_or(true)
    })
}

fn apply_meta_profile(instance: &mut J, sd: &StructureDefinition, instance_of: &str) {
    let url = sd.url().to_string();
    let version_parts: Vec<&str> = instance_of.split('|').skip(1).collect();
    let meta_url = if version_parts.is_empty() {
        url.clone()
    } else {
        format!("{}|{}", url, version_parts.join("|"))
    };
    let obj = instance.as_object_mut().unwrap();
    // already present?
    let already = obj
        .get("meta")
        .and_then(|m| m.get("profile"))
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter().any(|p| {
                let pu = p.as_str().unwrap_or("");
                pu == url || pu.starts_with(&format!("{url}|"))
            })
        })
        .unwrap_or(false);
    if already {
        return;
    }
    let meta = obj.entry("meta".to_string()).or_insert_with(|| J::Object(Map::new()));
    let meta_obj = meta.as_object_mut().unwrap();
    match meta_obj.get_mut("profile") {
        None => {
            meta_obj.insert("profile".into(), J::Array(vec![J::String(meta_url)]));
        }
        Some(J::Array(arr)) => {
            if arr.first().map(|p| is_empty_str(p)).unwrap_or(false) {
                arr[0] = J::String(meta_url);
            } else {
                arr.insert(0, J::String(meta_url));
            }
        }
        Some(other) => {
            *other = J::Array(vec![J::String(meta_url)]);
        }
    }
}

fn is_empty_str(v: &J) -> bool {
    match v {
        J::String(s) => s.is_empty(),
        J::Null => true,
        _ => false,
    }
}

#[allow(dead_code)]
struct AssignRule {
    path: String,
    value: Option<FshValue>,
    is_path: bool,
    inline: Option<J>,
}

impl Exporter<'_> {
fn set_assigned_values(
    &self,
    inst: &Instance,
    instance: &mut J,
    sd: &mut StructureDefinition,
    fisher: &dyn Fisher,
) {
    let inst_idx = &self.inst_idx;
    let def_idx = &self.def_idx;
    let manual_slice_ordering = self.cfg.manual_slice_ordering();
    // 1. resolve soft indexing on a clone of the rules.
    let mut rules: Vec<Rule> = inst.rules.clone();
    crate::paths::resolve_soft_indexing(&mut rules);

    // 2. normalize [0] indices away; replaceReferences.
    let mut assign_rules: Vec<AssignRule> = Vec::new();
    for r in &rules {
        match r {
            Rule::Assignment { path, value, is_instance, raw_value, .. } => {
                let mut path = path.clone();
                path = strip_zero_indices(&path);
                let mut value = value.clone();
                if *is_instance {
                    // Inline instance assignment: fish and embed the instance JSON.
                    // The instance name may be a non-string token (e.g. a numeric
                    // id), so fall back to the raw value text.
                    let name = match &value {
                        Some(FshValue::Str(s)) => s.clone(),
                        _ => match raw_value {
                            Some(rv) => rv.clone(),
                            None => continue,
                        },
                    };
                    if let Some(body) = self.fish_instance(&name) {
                        assign_rules.push(AssignRule { path, value: None, is_path: false, inline: Some(body) });
                    }
                    continue;
                }
                // A numeric/boolean value assigned to a Resource-typed element is
                // actually an inline instance referenced by its (numeric) id —
                // mirror InstanceExporter's MismatchedTypeError recovery.
                let numericish = matches!(
                    value,
                    Some(FshValue::BigInt(_)) | Some(FshValue::Bool(_)) | Some(FshValue::Float(_))
                );
                if numericish {
                    if let Some(rv) = raw_value {
                        if leaf_is_resource(sd, &path, fisher) {
                            if let Some(body) = self.fish_instance(rv) {
                                assign_rules.push(AssignRule {
                                    path,
                                    value: None,
                                    is_path: false,
                                    inline: Some(body),
                                });
                                continue;
                            }
                        }
                    }
                }
                if let Some(v) = &mut value {
                    replace_references(v, inst_idx, def_idx, fisher);
                }
                assign_rules.push(AssignRule { path, value, is_path: false, inline: None });
            }
            Rule::Path { path, .. } => {
                let path = strip_zero_indices(path);
                assign_rules.push(AssignRule { path, value: None, is_path: true, inline: None });
            }
            _ => {}
        }
    }

    // 3. Build ruleMap (validate each rule), keyed by rule path (last-wins replaced).
    let mut rule_map: Vec<(String, Vec<IPathPart>, J)> = Vec::new();
    for ar in &assign_rules {
        let path = &ar.path;
        if let Some(validated) = validate_value_at_path(sd, path, ar.value.as_ref(), fisher) {
            // skip choice [x] unresolved
            let av = if let Some(body) = &ar.inline {
                body.clone()
            } else {
                validated.assigned_value.clone().unwrap_or(J::Null)
            };
            let final_path = if let Some(cp) = validated.child_path {
                format!("{path}.{cp}")
            } else {
                path.clone()
            };
            // record (replace existing same key, keep position)
            if av.is_null() && rule_map.iter().any(|(k, _, _)| k == &final_path) {
                continue;
            }
            if let Some(existing) = rule_map.iter_mut().find(|(k, _, _)| k == &final_path) {
                existing.1 = validated.path_parts;
                existing.2 = av;
            } else {
                rule_map.push((final_path, validated.path_parts, av));
            }
        }
    }

    // 4. paths array for implied properties. Also collect the paths where an
    // inline resource was assigned (`inlineResourcePaths`) so setImpliedProperties
    // does NOT unfold/inject implied values into the embedded resource
    // (InstanceExporter.ts:140-160 + common.ts:518).
    let mut paths: Vec<String> = vec![String::new()];
    let mut assigned_resource_paths: Vec<String> = Vec::new();
    for ar in &assign_rules {
        let path = &ar.path;
        // find validated pathParts for this rule path (first match)
        let path_dot = format!("{path}.");
        let assembled = if let Some((_, parts, _)) =
            rule_map.iter().find(|(k, _, _)| k == path || k.starts_with(&path_dot))
        {
            strip_zero_only(&assemble_fsh_path(parts))
        } else {
            path.clone()
        };
        if ar.inline.is_some() {
            assigned_resource_paths.push(assembled.clone());
        }
        paths.push(assembled);
    }

    // 5. knownSlices (+ createUsefulSlices mutation under manualSliceOrdering).
    let rm_for_known: Vec<(String, Vec<IPathPart>)> = rule_map
        .iter()
        .map(|(k, p, _)| (k.clone(), p.clone()))
        .collect();
    let known = if manual_slice_ordering {
        create_useful_slices(instance, sd, &rm_for_known, fisher)
    } else {
        determine_known_slices(sd, &rm_for_known, fisher)
    };

    // 6. setImpliedProperties on instance.
    set_implied_properties_on_instance(
        instance,
        sd,
        &paths,
        &assigned_resource_paths,
        fisher,
        &known,
        manual_slice_ordering,
    );

    // 7. rule assignment on a clone, then merge.
    let mut rule_instance = instance.clone();
    for (_k, parts, av) in &rule_map {
        set_property_on_instance(&mut rule_instance, parts, av);
    }
    merge(instance, &rule_instance);
}
}

/// Does the element at `path` have a single `Resource` type (so a primitive
/// value can't be assigned there — it must be an inline instance)?
fn leaf_is_resource(sd: &mut StructureDefinition, path: &str, fisher: &dyn Fisher) -> bool {
    let non_numeric = strip_numeric_brackets(path);
    if let Some(idx) = sd.find_element_by_path(&non_numeric, fisher) {
        let types = el_type_codes(sd, idx);
        return types.len() == 1 && types[0] == "Resource";
    }
    false
}

fn strip_zero_indices(path: &str) -> String {
    // replace /\[0+\]/g with ''
    let mut out = String::new();
    let chars: Vec<char> = path.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            let mut j = i + 1;
            let mut inner = String::new();
            while j < chars.len() && chars[j] != ']' {
                inner.push(chars[j]);
                j += 1;
            }
            if !inner.is_empty() && inner.chars().all(|c| c == '0') {
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn strip_zero_only(path: &str) -> String {
    // replace /\[0\]/g with ''
    path.replace("[0]", "")
}

fn order_instance(instance: &J) -> J {
    let obj = instance.as_object().unwrap();
    let prefix = ["resourceType", "_resourceType", "id", "_id", "meta", "_meta"];
    let mut ordered = Map::new();
    for k in prefix {
        if let Some(v) = obj.get(k) {
            if !v.is_null() {
                ordered.insert(k.to_string(), v.clone());
            }
        }
    }
    for (k, v) in obj {
        if prefix.contains(&k.as_str()) || k == "_instanceMeta" {
            continue;
        }
        ordered.insert(k.clone(), v.clone());
    }
    json_emit::ordered_clone_deep(&J::Object(ordered))
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | '?' | '%' | '*' | ':' | '|' | '"' | '<' | '>' => '-',
            _ => c,
        })
        .collect()
}

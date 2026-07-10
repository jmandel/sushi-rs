//! Unified, SD-driven element-type resolver for caret-value application.
//!
//! Given a starting resource/datatype type name and a parsed caret path, this
//! resolves each path segment's `(type_code, is_array, slice/index)` by fishing
//! the REAL FHIR StructureDefinition from `package_store` and **descending across
//! datatype / extension boundaries** by fishing the next type's SD. It replaces
//! the hand-maintained datatype/`value[x]` tables previously embedded in
//! `export.rs` and `caret_schema.rs`, so it generalizes to ANY IG/datatype.
//!
//! Key facts driving the algorithm (all read from the snapshot, nothing hardcoded):
//! - A complex element's children are NOT in the parent SD beyond one level — to
//!   type `X.valueAnnotation.extension...` we fish the `Annotation` SD, then
//!   `Extension`, etc.
//! - A BackboneElement child's children ARE in the same SD (deeper paths), so we
//!   keep navigating the same snapshot rather than fishing.
//! - `value[x]` (and `fixed[x]`/`pattern[x]`/…) choices come from the element's
//!   `type` array in the SD; a key like `valueContactDetail` resolves by matching
//!   the suffix after the choice stem to a type code (`ContactDetail`;
//!   `valueString`→`string`).
//! - `extension[url]` fishes the extension's SD BY URL → its `value[x]` choices /
//!   nested `extension` slices.
//! - primitive vs complex is derived from the type SD's `kind`.

use crate::export::Seg;
use serde_json::Value as J;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Indexed view of one StructureDefinition snapshot, built once per fished type.
struct SdInfo {
    /// `pathType` — the SD `type` (last URL segment if it is a URL).
    path_type: String,
    /// `kind` (`primitive-type` / `complex-type` / `resource` / …).
    kind: String,
    /// Full FHIR path -> element info, for non-sliced snapshot elements.
    by_path: HashMap<String, ElemInfo>,
    /// Parent FHIR path -> choice groups directly under it (`value[x]`, `fixed[x]`…).
    choices: HashMap<String, Vec<ChoiceGroup>>,
}

#[derive(Clone)]
struct ElemInfo {
    type_code: String,
    array: bool,
    backbone: bool,
    /// `contentReference` target (FHIR path, leading `#` stripped) for recursive
    /// elements such as `CodeSystem.concept.concept` → `CodeSystem.concept`.
    content_ref: Option<String>,
}

struct ChoiceGroup {
    /// Stem before `[x]` (e.g. `value`, `fixed`, `pattern`).
    stem: String,
    array: bool,
    /// Candidate type codes from the element's `type` array (FHIR order).
    options: Vec<String>,
}

/// `ElementDefinitionType.code` getter: prefer a `structuredefinition-fhir-type`
/// extension's `valueUrl`/`valueUri` (R4/R5 primitives carry the System.* code on
/// `type[0].code` with the real FHIR type in the extension), else the raw `code`.
fn type_code(t: &J) -> Option<String> {
    let code = t.get("code").and_then(|v| v.as_str())?;
    for src in [t.get("extension"), t.pointer("/_code/extension")] {
        if let Some(arr) = src.and_then(|v| v.as_array()) {
            for ext in arr {
                if ext.get("url").and_then(|v| v.as_str())
                    == Some("http://hl7.org/fhir/StructureDefinition/structuredefinition-fhir-type")
                {
                    if let Some(u) = ext
                        .get("valueUrl")
                        .or_else(|| ext.get("valueUri"))
                        .and_then(|v| v.as_str())
                    {
                        return Some(u.to_string());
                    }
                }
            }
        }
    }
    Some(code.to_string())
}

fn is_array_max(el: &J) -> bool {
    match el.get("max").and_then(|v| v.as_str()) {
        Some("0") | Some("1") => false,
        Some(_) => true,
        None => false,
    }
}

fn path_type_of(sd: &J) -> Option<String> {
    let t = sd.get("type").and_then(|v| v.as_str())?;
    if t.starts_with("http") {
        Some(t.rsplit('/').next().unwrap_or(t).to_string())
    } else {
        Some(t.to_string())
    }
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Does `suffix` (the part after a choice stem, e.g. `String`/`ContactDetail`)
/// name the type `code`? Complex types match verbatim; primitive types match
/// their capitalized form (`valueString`→`string`).
fn choice_matches(suffix: &str, code: &str) -> bool {
    suffix == code || suffix == upper_first(code)
}

impl SdInfo {
    fn build(sd: &J) -> Option<SdInfo> {
        let path_type = path_type_of(sd)?;
        let kind = sd
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let mut by_path = HashMap::new();
        let mut choices: HashMap<String, Vec<ChoiceGroup>> = HashMap::new();
        let elements = sd
            .pointer("/snapshot/element")
            .and_then(|v| v.as_array())
            .map(|a| a.as_slice())
            .unwrap_or(&[]);
        for el in elements {
            // Skip slices: caret type resolution is over the unsliced element tree.
            if el.get("sliceName").is_some() {
                continue;
            }
            let Some(p) = el.get("path").and_then(|v| v.as_str()) else {
                continue;
            };
            let types = el.get("type").and_then(|v| v.as_array());
            if let Some(stem_path) = p.strip_suffix("[x]") {
                // Choice element: index under its parent path by the stem name.
                let (parent, stem) = match stem_path.rfind('.') {
                    Some(i) => (stem_path[..i].to_string(), stem_path[i + 1..].to_string()),
                    None => (String::new(), stem_path.to_string()),
                };
                let options: Vec<String> = types
                    .map(|a| a.iter().filter_map(type_code).collect())
                    .unwrap_or_default();
                choices.entry(parent).or_default().push(ChoiceGroup {
                    stem,
                    array: is_array_max(el),
                    options,
                });
            } else {
                let tc = types
                    .and_then(|a| a.first())
                    .and_then(type_code)
                    .unwrap_or_default();
                let content_ref = el
                    .get("contentReference")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim_start_matches('#').to_string());
                let backbone = tc == "BackboneElement" || tc == "Element" || content_ref.is_some();
                by_path.insert(
                    p.to_string(),
                    ElemInfo {
                        type_code: tc,
                        array: is_array_max(el),
                        backbone,
                        content_ref,
                    },
                );
            }
        }
        Some(SdInfo {
            path_type,
            kind,
            by_path,
            choices,
        })
    }

    /// Resolve a direct child `base` under `prefix` (a FHIR path within this SD).
    fn child(&self, prefix: &str, base: &str) -> Option<ElemInfo> {
        let exact = format!("{prefix}.{base}");
        if let Some(ei) = self.by_path.get(&exact) {
            return Some(ei.clone());
        }
        if let Some(groups) = self.choices.get(prefix) {
            for g in groups {
                if let Some(suffix) = base.strip_prefix(&g.stem) {
                    if suffix.is_empty() {
                        continue;
                    }
                    for code in &g.options {
                        if choice_matches(suffix, code) {
                            return Some(ElemInfo {
                                type_code: code.clone(),
                                array: g.array,
                                backbone: false,
                                content_ref: None,
                            });
                        }
                    }
                }
            }
        }
        None
    }
}

/// SD-driven type resolver. Holds a fishing closure plus a per-type SdInfo cache
/// so repeated rules over the same datatypes reuse the indexed snapshot.
pub struct TypeResolver<'a> {
    fish: &'a dyn Fn(&str) -> Option<Rc<J>>,
    cache: RefCell<HashMap<String, Option<Rc<SdInfo>>>>,
}

impl<'a> TypeResolver<'a> {
    pub fn new(fish: &'a dyn Fn(&str) -> Option<Rc<J>>) -> Self {
        TypeResolver {
            fish,
            cache: RefCell::new(HashMap::new()),
        }
    }

    fn info(&self, name: &str) -> Option<Rc<SdInfo>> {
        if let Some(v) = self.cache.borrow().get(name) {
            return v.clone();
        }
        let built = (self.fish)(name)
            .and_then(|sd| SdInfo::build(&sd))
            .map(Rc::new);
        self.cache
            .borrow_mut()
            .insert(name.to_string(), built.clone());
        built
    }

    /// `kind == primitive-type` for a fished type name.
    pub fn is_primitive(&self, ty: &str) -> bool {
        self.info(ty)
            .map(|i| i.kind == "primitive-type")
            .unwrap_or(false)
    }

    /// `kind == complex-type` for a fished type name.
    pub fn is_complex(&self, ty: &str) -> bool {
        self.info(ty)
            .map(|i| i.kind == "complex-type")
            .unwrap_or(false)
    }

    /// Resolve a caret path on a starting resource/datatype type into segments +
    /// the leaf element's FHIR type code. Returns None if any segment can't be
    /// typed (the rule is then dropped — matching stock's CannotResolvePath).
    pub fn resolve(&self, root_type: &str, caret_path: &str) -> Option<(Vec<Seg>, String)> {
        let parts = crate::export::split_caret_path(caret_path);
        if parts.is_empty() {
            return None;
        }
        let mut cur = self.info(root_type)?;
        let mut prefix = cur.path_type.clone();
        let mut segs: Vec<Seg> = Vec::with_capacity(parts.len());
        let mut leaf_ty = String::new();
        let n = parts.len();

        for (i, part) in parts.iter().enumerate() {
            let (base, brackets) = parse_part(part);

            // Resolve this segment's element info against the current SD context.
            let ei = cur.child(&prefix, &base).or_else(|| {
                // Every BackboneElement/datatype can carry extension/modifierExtension
                // even when a constrained snapshot omits the element; and `id` is the
                // string id of any element.
                if base == "extension" || base == "modifierExtension" {
                    Some(ElemInfo {
                        type_code: "Extension".into(),
                        array: true,
                        backbone: false,
                        content_ref: None,
                    })
                } else if base == "id" {
                    Some(ElemInfo {
                        type_code: "string".into(),
                        array: false,
                        backbone: false,
                        content_ref: None,
                    })
                } else {
                    None
                }
            })?;

            // Determine numeric index + extension slice url from the brackets.
            let mut index = None;
            let mut slice_url = None;
            for b in &brackets {
                if !b.is_empty() && b.chars().all(|c| c.is_ascii_digit()) {
                    index = b.parse::<usize>().ok();
                } else if base == "extension" || base == "modifierExtension" {
                    slice_url = Some(b.clone());
                }
            }

            let is_last = i == n - 1;
            // Primitive-sibling redirect: navigating deeper than a primitive (e.g.
            // `targetProfile[0].extension`) targets the `_`-sibling array.
            let descend_primitive = !is_last && self.is_primitive(&ei.type_code);
            let key = if descend_primitive {
                format!("_{base}")
            } else {
                base.clone()
            };
            segs.push(Seg {
                key,
                array: ei.array,
                slice_url: slice_url.clone(),
                index,
                // Default: implied url stays first. Callers that have the original
                // (pre-alias-resolution) FSH path set this for non-URI slice tokens.
                defer_url: false,
            });

            if is_last {
                leaf_ty = ei.type_code.clone();
                break;
            }

            // Descend to the next SD context.
            if base == "extension" || base == "modifierExtension" {
                // Always descend through the generic Extension SD. Its `value[x]`
                // lists every datatype (so `valueContactDetail`/`valueCode`/… all
                // type correctly) and its recursive `.extension` covers nested
                // sub-extension slices. We deliberately do NOT fish the bracket as
                // a type: the bracket is a slice selector (sliceName/url/alias) and
                // a sliceName can collide with a real type name (e.g. a sub-extension
                // sliced `[code]` would otherwise resolve against the `code`
                // primitive and drop the rule).
                cur = self.info("Extension")?;
                prefix = cur.path_type.clone();
            } else if let Some(cr) = &ei.content_ref {
                // contentReference: stay in the same SD but continue from the
                // referenced element's path (recursive hierarchies, e.g.
                // `CodeSystem.concept.concept` -> `CodeSystem.concept`).
                prefix = cr.clone();
            } else if ei.backbone {
                // Stay in the same SD; navigate deeper.
                prefix = format!("{prefix}.{base}");
            } else {
                cur = self.info(&ei.type_code)?;
                prefix = cur.path_type.clone();
            }
        }

        Some((segs, leaf_ty))
    }
}

/// Parse one path segment into `(base, brackets)` using the shared FSH-path parser
/// (handles multiple bracket groups: `extension[url][1]`).
fn parse_part(part: &str) -> (String, Vec<String>) {
    match fhir_model::parse_fsh_path(part).into_iter().next() {
        Some(p) => (p.base, p.brackets),
        None => (part.to_string(), Vec::new()),
    }
}

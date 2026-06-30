//! FHIR model (StructureDefinition / ElementDefinition). Phase 5/6.
//!
//! Pragmatic port of `sushi-ts/src/fhirtypes/{StructureDefinition,ElementDefinition}.ts`.
//! Elements are stored as ordered JSON maps (the FHIR ElementDefinition JSON),
//! with a captured `_original` map for diffing. `path` is derived from `id`.
//! The StructureDefinition keeps its top-level props in an ordered `body` map and
//! a flat `elements` vector; snapshot+differential are both derived from it.

use rustc_hash::FxHashMap;
use serde_json::{Map, Value};
use std::cell::RefCell;
use std::rc::Rc;

pub mod props;
pub use props::{ED_PROPS, SD_PROPS};

/// Metadata returned by a `Fisher` (mirrors `utils/Fishable.ts` `Metadata`).
#[derive(Clone, Debug, Default)]
pub struct Metadata {
    pub id: String,
    pub name: String,
    pub sd_type: Option<String>,
    pub url: Option<String>,
    pub parent: Option<String>,
    pub abstract_: Option<bool>,
    pub version: Option<String>,
    pub kind: Option<String>,
    pub can_bind: bool,
    pub can_be_target: bool,
    pub instance_usage: Option<String>,
}

/// The fishing interface used by `unfold` / type resolution. Implemented by the
/// compiler's combined tank + package_store fisher.
pub trait Fisher {
    /// Full SD JSON (with snapshot) for a type/profile/etc. name|id|url.
    ///
    /// Returns a shared `Rc<Value>`: the package-store path hands back its memoized
    /// parse with no deep clone, and callers here only read it (build a
    /// `StructureDefinition`, inspect fields) via deref coercion.
    fn fish_for_fhir(&self, name: &str) -> Option<std::rc::Rc<Value>>;
    /// Metadata for a name|id|url.
    fn fish_for_metadata(&self, name: &str) -> Option<Metadata>;
    /// Metadata restricted to ValueSet definitions (`fishForMetadata(_, Type.ValueSet)`).
    /// Default falls back to the untyped fish.
    fn fish_for_metadata_vs(&self, name: &str) -> Option<Metadata> {
        self.fish_for_metadata(name)
    }
}

// ---------------------------------------------------------------------------
// id -> path derivation
// ---------------------------------------------------------------------------

/// `id.replace(/(\.[^.:]+):[^.]+/g, '$1')` — strip one `:sliceName` per non-root segment.
pub fn id_to_path(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    let mut first = true;
    for seg in id.split('.') {
        if !first {
            out.push('.');
        }
        // On non-root segments, drop a trailing `:...` (slice name).
        if !first {
            match seg.find(':') {
                Some(idx) => out.push_str(&seg[..idx]),
                None => out.push_str(seg),
            }
        } else {
            out.push_str(seg);
        }
        first = false;
    }
    out
}

// ---------------------------------------------------------------------------
// ElementDefinition
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct ElementDefinition {
    // Copy-on-write element JSON. Shared (`Rc`) between an element and its
    // captured `original`, and across cloned elements (unfold/cloneChildren/
    // addSlice), so cloning an element or capturing its original is a refcount
    // bump — the inner `Map` is deep-cloned lazily, only when a write actually
    // needs a private copy (`map_mut` / `original_mut` -> `Rc::make_mut`). This
    // exactly models `captureOriginal`: capture shares the Rc, the first
    // subsequent mutation forks `map` away while `original` keeps the snapshot.
    pub map: Rc<Map<String, Value>>,
    pub original: Option<Rc<Map<String, Value>>>,
    // Cached mirrors of `map["id"]` / `map["path"]` to avoid IndexMap+SipHash
    // lookups on the hottest accessors (`id()`/`path()` run inside every linear
    // element scan). Kept in sync by `from_json`/`new`/`set_id` (the only writers
    // of those map keys). Never written via `set()`/`map.insert` elsewhere.
    id: String,
    path: String,
}

impl ElementDefinition {
    /// `ElementDefinition.fromJSON` — copy known PROPS (drops unknown keys),
    /// then (optionally) captureOriginal.
    pub fn from_json(json: &Value, capture: bool) -> ElementDefinition {
        let mut map = Map::new();
        if let Some(obj) = json.as_object() {
            let mut uk = String::new();
            for prop in ED_PROPS {
                if let Some(key) = resolve_choice_key(prop, obj) {
                    if let Some(v) = obj.get(&key) {
                        map.insert(key.clone(), v.clone());
                    }
                    uk.clear();
                    uk.push('_');
                    uk.push_str(&key);
                    if let Some(v) = obj.get(uk.as_str()) {
                        map.insert(uk.clone(), v.clone());
                    }
                }
            }
        }
        let id = map.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let path = map.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let mut ed = ElementDefinition { map: Rc::new(map), original: None, id, path };
        if capture {
            ed.capture_original();
        }
        ed
    }

    pub fn new(id: &str) -> ElementDefinition {
        let path = id_to_path(id);
        let mut map = Map::new();
        map.insert("id".into(), Value::String(id.to_string()));
        map.insert("path".into(), Value::String(path.clone()));
        ElementDefinition {
            map: Rc::new(map),
            original: None,
            id: id.to_string(),
            path,
        }
    }

    /// Mutable access to the element map, copy-on-write: forks a private copy of
    /// the inner `Map` only if it is currently shared (with `original` or another
    /// cloned element). The sole write path for `map`.
    #[inline]
    pub fn map_mut(&mut self) -> &mut Map<String, Value> {
        Rc::make_mut(&mut self.map)
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn slice_name(&self) -> Option<&str> {
        self.map.get("sliceName").and_then(|v| v.as_str())
    }

    pub fn set_id(&mut self, id: String) {
        let path = id_to_path(&id);
        let m = self.map_mut();
        m.insert("id".into(), Value::String(id.clone()));
        m.insert("path".into(), Value::String(path.clone()));
        self.id = id;
        self.path = path;
    }

    pub fn capture_original(&mut self) {
        // Zero-copy: share the current map Rc as the captured original. The first
        // later write to `map` forks it (make_mut), leaving `original` intact.
        self.original = Some(Rc::clone(&self.map));
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.map.get(key)
    }
    pub fn set(&mut self, key: &str, v: Value) {
        self.map_mut().insert(key.to_string(), v);
    }
    pub fn remove(&mut self, key: &str) {
        self.map_mut().remove(key);
    }

    /// Type codes (raw `code` or fhir-type extension valueUrl/valueUri).
    pub fn type_codes(&self) -> Vec<String> {
        self.map
            .get("type")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().map(|t| type_code(t).to_string()).collect())
            .unwrap_or_default()
    }

    pub fn has_own_diff(&self) -> bool {
        let blank = Map::new();
        let original = self.original.as_deref().unwrap_or(&blank);
        let mut uk = String::new();
        for prop in ED_PROPS {
            let key = match resolve_choice_key_either(prop, &self.map, original) {
                Some(k) => k,
                None => continue,
            };
            if self.map.get(&key) != original.get(&key) {
                return true;
            }
            uk.clear();
            uk.push('_');
            uk.push_str(&key);
            if self.map.get(uk.as_str()) != original.get(uk.as_str()) {
                return true;
            }
        }
        false
    }

    /// `calculateDiff().toJSON()` collapsed.
    pub fn calculate_diff_json(&self) -> Value {
        let blank = Map::new();
        let original = self.original.as_deref().unwrap_or(&blank);
        let mut diff = Map::new();
        let id = self.id().to_string();
        diff.insert("id".into(), Value::String(id.clone()));
        diff.insert("path".into(), Value::String(id_to_path(&id)));

        let is_choice_slice = self.slice_name().is_some() && self.path().ends_with("[x]");

        let mut uk = String::new();
        for prop in ED_PROPS {
            let key = match resolve_choice_key_either(prop, &self.map, original) {
                Some(k) => k,
                None => continue,
            };
            uk.clear();
            uk.push('_');
            uk.push_str(&key);
            let changed = self.map.get(&key) != original.get(&key);
            let uchanged = self.map.get(uk.as_str()) != original.get(uk.as_str());

            if changed {
                if key == "mapping" || key == "constraint" {
                    let cur = self.map.get(&key).and_then(|v| v.as_array());
                    let orig = original.get(&key).and_then(|v| v.as_array());
                    if let Some(cur) = cur {
                        let diff_arr: Vec<Value> = cur
                            .iter()
                            .filter(|item| !orig.map(|o| o.contains(item)).unwrap_or(false))
                            .cloned()
                            .collect();
                        if !diff_arr.is_empty() {
                            diff.insert(key.clone(), Value::Array(diff_arr));
                        }
                    }
                } else if let Some(v) = self.map.get(&key) {
                    diff.insert(key.clone(), v.clone());
                }
            } else if key == "type" && is_choice_slice {
                if let Some(v) = self.map.get(&key) {
                    diff.insert(key.clone(), v.clone());
                }
            }
            if uchanged {
                if let Some(v) = self.map.get(uk.as_str()) {
                    diff.insert(uk.clone(), v.clone());
                }
            }
        }
        if let Some(sn) = original.get("sliceName") {
            diff.entry("sliceName".to_string()).or_insert(sn.clone());
        }
        order_element_json(&diff)
    }

    pub fn to_json(&self) -> Value {
        order_element_json(&self.map)
    }
}

/// `ElementDefinitionType.toJSON` key order: code first, then the fromJSON
/// assignment order, extension last.
const TYPE_PROPS: &[&str] = &[
    "id",
    "code",
    "_code",
    "profile",
    "_profile",
    "targetProfile",
    "_targetProfile",
    "aggregation",
    "_aggregation",
    "versioning",
    "_versioning",
    "extension",
    "modifierExtension",
];

fn order_type_obj(t: &Value) -> Value {
    let Some(obj) = t.as_object() else {
        return t.clone();
    };
    let mut out = Map::new();
    for k in TYPE_PROPS {
        if let Some(v) = obj.get(*k) {
            out.insert((*k).to_string(), v.clone());
        }
    }
    for (k, v) in obj {
        out.entry(k.clone()).or_insert_with(|| v.clone());
    }
    Value::Object(out)
}

/// Order an element JSON map per ED PROPS (with `[x]` resolution + `_` siblings).
fn order_element_json(map: &Map<String, Value>) -> Value {
    let mut out = Map::new();
    let mut uk = String::new();
    for prop in ED_PROPS {
        if let Some(key) = resolve_choice_key(prop, map) {
            if let Some(v) = map.get(&key) {
                let v = if key == "type" {
                    match v.as_array() {
                        Some(arr) => Value::Array(arr.iter().map(order_type_obj).collect()),
                        None => v.clone(),
                    }
                } else {
                    v.clone()
                };
                out.insert(key.clone(), v);
            }
            uk.clear();
            uk.push('_');
            uk.push_str(&key);
            if let Some(v) = map.get(uk.as_str()) {
                out.insert(uk.clone(), v.clone());
            }
        }
    }
    Value::Object(out)
}

fn resolve_choice_key(prop: &str, map: &Map<String, Value>) -> Option<String> {
    if let Some(base) = prop.strip_suffix("[x]") {
        for k in map.keys() {
            if let Some(rest) = k.strip_prefix(base) {
                if rest.chars().next().map(|c| c.is_ascii_uppercase()) == Some(true) {
                    return Some(k.clone());
                }
            }
        }
        None
    } else {
        Some(prop.to_string())
    }
}

fn resolve_choice_key_either(
    prop: &str,
    a: &Map<String, Value>,
    b: &Map<String, Value>,
) -> Option<String> {
    if prop.ends_with("[x]") {
        resolve_choice_key(prop, a).or_else(|| resolve_choice_key(prop, b))
    } else {
        Some(prop.to_string())
    }
}

/// The "actual" type code: fhir-type extension valueUrl/valueUri else `code`.
pub fn type_code(t: &Value) -> &str {
    if let Some(exts) = t.get("extension").and_then(|v| v.as_array()) {
        for e in exts {
            if e.get("url").and_then(|v| v.as_str())
                == Some("http://hl7.org/fhir/StructureDefinition/structuredefinition-fhir-type")
            {
                if let Some(u) = e
                    .get("valueUrl")
                    .or_else(|| e.get("valueUri"))
                    .and_then(|v| v.as_str())
                {
                    return u;
                }
            }
        }
    }
    t.get("code").and_then(|v| v.as_str()).unwrap_or("")
}

// ---------------------------------------------------------------------------
// StructureDefinition
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct StructureDefinition {
    pub body: Map<String, Value>,
    pub elements: Vec<ElementDefinition>,
    pub original_mapping: Vec<Value>,
    pub in_progress: bool,
    /// Lazy id -> element-index cache (cheap FxHash) backing `index_of_id`/
    /// `path_of_id`, which otherwise linear-scan the elements vec (O(n²) inside
    /// `find_element_by_path`). Stores `(elements.len() at build time, map)`.
    /// Rebuilt automatically when the element count changes (covers every
    /// add/splice in this module). Callers that rename element ids in place
    /// WITHOUT changing the count (e.g. `reset_parent_elements`'s `set_id` loop)
    /// MUST call `invalidate_id_index()` afterwards. A per-lookup verification
    /// (`elements[i].id() == id`) self-heals any shifted-position staleness.
    id_index: RefCell<Option<(usize, FxHashMap<String, usize>)>>,
}

impl StructureDefinition {
    pub fn from_json(json: &Value, capture: bool) -> StructureDefinition {
        let mut body = Map::new();
        let obj = json.as_object().cloned().unwrap_or_default();
        let mut uk = String::new();
        for prop in SD_PROPS {
            if let Some(v) = obj.get(*prop) {
                body.insert((*prop).to_string(), v.clone());
            }
            uk.clear();
            uk.push('_');
            uk.push_str(prop);
            if let Some(v) = obj.get(uk.as_str()) {
                body.insert(uk.clone(), v.clone());
            }
        }
        let mut elements = Vec::new();
        if let Some(snap) = obj
            .get("snapshot")
            .and_then(|s| s.get("element"))
            .and_then(|e| e.as_array())
        {
            for el in snap {
                elements.push(ElementDefinition::from_json(el, capture));
            }
        }
        let original_mapping = body
            .get("mapping")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        StructureDefinition {
            body,
            elements,
            original_mapping,
            in_progress: false,
            id_index: RefCell::new(None),
        }
    }

    /// Drop the cached id->index map. Call after renaming element ids in place
    /// without changing the element count (e.g. an external `set_id` loop).
    pub fn invalidate_id_index(&self) {
        *self.id_index.borrow_mut() = None;
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.body.get(key).and_then(|v| v.as_str())
    }
    pub fn url(&self) -> &str {
        self.get_str("url").unwrap_or("")
    }
    pub fn type_(&self) -> &str {
        self.get_str("type").unwrap_or("")
    }
    pub fn kind(&self) -> &str {
        self.get_str("kind").unwrap_or("")
    }
    pub fn derivation(&self) -> &str {
        self.get_str("derivation").unwrap_or("")
    }
    pub fn name(&self) -> &str {
        self.get_str("name").unwrap_or("")
    }

    pub fn path_type(&self) -> String {
        let t = self.type_();
        if t.starts_with("http") {
            t.rsplit('/').next().unwrap_or(t).to_string()
        } else {
            t.to_string()
        }
    }

    pub fn capture_original_elements(&mut self) {
        for e in &mut self.elements {
            e.capture_original();
        }
        self.original_mapping = self
            .body
            .get("mapping")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
    }

    pub fn find_element(&self, id: &str) -> Option<usize> {
        self.index_of_id(id)
    }

    pub fn index_of_id(&self, id: &str) -> Option<usize> {
        {
            let cache = self.id_index.borrow();
            if let Some((len, map)) = cache.as_ref() {
                if *len == self.elements.len() {
                    return match map.get(id) {
                        // Verify the cached slot still holds this id (self-heals a
                        // position shift that didn't change the count). `position`
                        // semantics: first match wins (the map keeps first).
                        Some(&i) if self.elements.get(i).map(|e| e.id()) == Some(id) => Some(i),
                        Some(_) => self.elements.iter().position(|e| e.id() == id),
                        None => None,
                    };
                }
            }
        }
        // (Re)build the index for the current element vec.
        let mut map: FxHashMap<String, usize> =
            FxHashMap::with_capacity_and_hasher(self.elements.len(), Default::default());
        for (i, e) in self.elements.iter().enumerate() {
            map.entry(e.id().to_string()).or_insert(i);
        }
        let result = map.get(id).copied();
        *self.id_index.borrow_mut() = Some((self.elements.len(), map));
        result
    }
    pub fn path_of_id(&self, id: &str) -> Option<&str> {
        self.index_of_id(id).map(|i| self.elements[i].path())
    }

    /// All descendant element ids of `id` (TS `children(false)`).
    fn children_ids(&self, id: &str) -> Vec<String> {
        let pdepth = self.path_of_id(id).map(path_depth).unwrap_or(0);
        // direct children first, recursively — but flat filter by prefix suffices
        let prefix = format!("{id}.");
        let mut out: Vec<String> = Vec::new();
        for e in &self.elements {
            if e.id().starts_with(&prefix) {
                out.push(e.id().to_string());
            }
        }
        let _ = pdepth;
        out
    }

    /// `slicedElement`: element this slice is a slice of (strip trailing `:x`/`/x`).
    fn sliced_element_id(&self, id: &str) -> Option<String> {
        let seg_start = id.rfind('.').map(|i| i + 1).unwrap_or(0);
        let seg = &id[seg_start..];
        let cut = seg.rfind([':', '/'])?;
        Some(format!("{}{}", &id[..seg_start], &seg[..cut]))
    }

    /// `findElementByPath(path, fisher)`. Port of `StructureDefinition.ts:255`.
    pub fn find_element_by_path(&mut self, path: &str, fisher: &dyn Fisher) -> Option<usize> {
        let pt = self.path_type();
        let full = if !path.is_empty() && path != "." {
            format!("{pt}.{path}")
        } else {
            pt.clone()
        };
        if let Some(i) = self
            .elements
            .iter()
            .position(|e| e.path() == full && !e.id().contains(':'))
        {
            return Some(i);
        }

        let parsed = crate::parse_fsh_path(path);
        let mut fhir_path = pt.clone();
        let mut previous_part = String::new();
        // matching set as ids (stable across splices)
        let mut matching: Vec<String> = self.elements.iter().map(|e| e.id().to_string()).collect();
        for part in &parsed {
            fhir_path = format!("{fhir_path}.{}", part.base);
            let fhir_path_dot = format!("{fhir_path}.");
            let fhir_path_colon = format!("{fhir_path}:");
            let mut new_matching: Vec<String> = matching
                .iter()
                .filter(|id| {
                    let p = self.path_of_id(id).unwrap_or("");
                    p.starts_with(&fhir_path_dot)
                        || p.starts_with(&fhir_path_colon)
                        || p == fhir_path
                })
                .cloned()
                .collect();

            let mut unfolded: Vec<String> = vec![];
            if new_matching.is_empty() && matching.len() == 1 {
                let single = matching[0].clone();
                unfolded = self.unfold_by_id(&single, fisher);
                new_matching = unfolded
                    .iter()
                    .filter(|id| self.path_of_id(id).unwrap_or("").starts_with(&fhir_path))
                    .cloned()
                    .collect();
            }
            let _ = &previous_part;

            if new_matching.is_empty() {
                // sliceMatchingValueX: resolve e.g. valueCodeableConcept -> value[x].
                let mut cands = matching.clone();
                cands.extend(unfolded.clone());
                if let Some(slice_id) = self.slice_matching_value_x(&fhir_path, &cands) {
                    new_matching = vec![slice_id.clone()];
                    new_matching.extend(self.children_ids(&slice_id));
                    fhir_path = self.path_of_id(&slice_id).unwrap_or("").to_string();
                }
            }

            if new_matching.is_empty() {
                return None;
            }
            matching = new_matching;

            if !part.brackets.is_empty() {
                if let Some(slice_id) = self.find_matching_slice(&fhir_path, part, &matching, fisher) {
                    let mut narrowed = vec![slice_id.clone()];
                    narrowed.extend(self.children_ids(&slice_id));
                    matching = narrowed;
                } else {
                    // ref/canonical bracket — narrow to current single match's children
                    return None;
                }
            } else {
                // remove slices that don't match exactly
                let pdepth = path_depth(&fhir_path);
                let path_end = fhir_path.split('.').nth(pdepth).unwrap_or("").to_string();
                let path_end_colon = format!("{path_end}:");
                let path_end_base = format!("{path_end}:{}", part.base);
                let differs = path_end != part.base;
                matching.retain(|id| {
                    let id_end = id.split('.').nth(pdepth).unwrap_or("");
                    !id_end.contains(&path_end_colon)
                        || (id_end == path_end_base && differs)
                });
            }
            previous_part = part.base.clone();
        }
        let finals: Vec<String> = matching
            .into_iter()
            .filter(|id| self.path_of_id(id).unwrap_or("") == fhir_path)
            .collect();
        if finals.len() == 1 {
            self.index_of_id(&finals[0])
        } else {
            None
        }
    }

    /// `sliceMatchingValueX` — resolve `valueCodeableConcept` to a constrained
    /// `value[x]` element, creating a type slice if necessary. Returns the id.
    fn slice_matching_value_x(&mut self, fhir_path: &str, elements: &[String]) -> Option<String> {
        // x-elements among candidates
        let x_ids: Vec<String> = elements
            .iter()
            .filter(|id| self.path_of_id(id).map(|p| p.ends_with("[x]")).unwrap_or(false))
            .cloned()
            .collect();
        // matching x-elements + the matching type for each
        let mut matching: Vec<(String, Value)> = Vec::new();
        for id in &x_ids {
            let i = self.index_of_id(id).unwrap();
            let path = self.elements[i].path().to_string();
            let stem = &path[..path.len() - 3]; // strip [x]
            if let Some(types) = self.elements[i].get("type").and_then(|v| v.as_array()) {
                for t in types {
                    let code = type_code(t);
                    if format!("{stem}{}", upper_first(code)) == fhir_path {
                        matching.push((id.clone(), t.clone()));
                        break;
                    }
                }
            }
        }
        if matching.is_empty() {
            return None;
        }
        let (first_id, matching_type) = matching[0].clone();
        let fi = self.index_of_id(&first_id).unwrap();
        let first_path = self.elements[fi].path().to_string();
        let same_path_count = x_ids
            .iter()
            .filter(|id| self.path_of_id(id).map(|p| p == first_path).unwrap_or(false))
            .count();
        let single_type = self.elements[fi]
            .get("type")
            .and_then(|v| v.as_array())
            .map(|a| a.len() == 1)
            .unwrap_or(false);
        if matching.len() == 1
            && self.elements[fi].slice_name().is_none()
            && single_type
            && same_path_count == 1
        {
            return Some(first_id);
        }
        // create a type slice
        let slice_name = fhir_path.rsplit('.').next().unwrap_or("").to_string();
        // existing matching slice?
        if let Some((id, _)) = matching
            .iter()
            .find(|(id, _)| self.index_of_id(id).map(|i| self.elements[i].slice_name() == Some(slice_name.as_str())).unwrap_or(false))
        {
            return Some(id.clone());
        }
        // sliceIt(type,$this) on the matching x-element then addSlice
        self.slice_it(fi, "type", "$this");
        let fi = self.index_of_id(&first_id).unwrap();
        self.add_slice(fi, &slice_name, Some(matching_type))
    }

    fn slice_it(&mut self, idx: usize, disc_type: &str, disc_path: &str) {
        let existing = self.elements[idx].get("slicing").cloned();
        match existing {
            None => {
                self.elements[idx].set(
                    "slicing",
                    serde_json::json!({
                        "discriminator": [{ "type": disc_type, "path": disc_path }],
                        "ordered": false,
                        "rules": "open"
                    }),
                );
            }
            Some(mut s) => {
                let has = s
                    .get("discriminator")
                    .and_then(|d| d.as_array())
                    .map(|a| {
                        a.iter().any(|d| {
                            d.get("type").and_then(|v| v.as_str()) == Some(disc_type)
                                && d.get("path").and_then(|v| v.as_str()) == Some(disc_path)
                        })
                    })
                    .unwrap_or(false);
                if !has {
                    if let Some(arr) = s.get_mut("discriminator").and_then(|d| d.as_array_mut()) {
                        arr.push(serde_json::json!({ "type": disc_type, "path": disc_path }));
                    }
                }
                self.elements[idx].set("slicing", s);
            }
        }
    }

    /// `findMatchingSlice` (simplified: direct + url-encode retry).
    fn find_matching_slice(
        &mut self,
        fhir_path: &str,
        part: &crate::PathPart,
        elements: &[String],
        _fisher: &dyn Fisher,
    ) -> Option<String> {
        let slice_name = part.brackets.join("/");
        elements
            .iter()
            .find(|id| {
                self.path_of_id(id).unwrap_or("") == fhir_path
                    && self
                        .index_of_id(id)
                        .map(|i| self.elements[i].slice_name() == Some(slice_name.as_str()))
                        .unwrap_or(false)
            })
            .cloned()
    }

    /// `findMatchingSlice` fishForFHIR branch (StructureDefinition.ts:907-913):
    /// match an existing slice whose `type[0].profile[0]` equals `url`. Used by
    /// callers that resolve an extension bracket which is not a sliceName.
    pub fn find_slice_by_profile_url(&self, url: &str) -> Option<String> {
        self.elements
            .iter()
            .find(|ed| {
                ed.slice_name().is_some()
                    && ed
                        .get("type")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                        .and_then(|t| t.get("profile"))
                        .and_then(|p| p.as_array())
                        .and_then(|a| a.first())
                        .and_then(|v| v.as_str())
                        == Some(url)
            })
            .and_then(|ed| ed.slice_name().map(String::from))
    }

    /// `unfold` by id. Returns the ids of newly added children.
    pub fn unfold_by_id(&mut self, id: &str, fisher: &dyn Fisher) -> Vec<String> {
        let Some(idx) = self.index_of_id(id) else {
            return vec![];
        };
        // contentReference branch: clone children of the referenced element.
        if let Some(cr) = self.elements[idx]
            .get("contentReference")
            .and_then(|v| v.as_str())
            .map(String::from)
        {
            // getContentReferenceId: everything after the '#'.
            let ref_id = match cr.find('#') {
                Some(p) => cr[p + 1..].to_string(),
                None => cr.clone(),
            };
            let parent_id = self.elements[idx].id().to_string();
            let sd_type = self.type_().to_string();
            let sd_id = self.get_str("id").unwrap_or("").to_string();
            let sd_url = self.get_str("url").unwrap_or("").to_string();

            // SUSHI clones from the *constrained* snapshot element ONLY when the
            // referenced element carries the elementdefinition-profile-element
            // extension in this profile's differential (rare, SDC-style). Otherwise
            // it clones from the *unconstrained base resource*, so diffs are taken
            // relative to base cardinalities, not the already-constrained parent
            // (`ElementDefinition.ts:2706-2735`).
            let use_constrained = fisher
                .fish_for_fhir(&sd_id)
                .map(|pj| has_profile_element_extension(&pj, &ref_id, &sd_url))
                .unwrap_or(false);

            if !use_constrained {
                if let Some(base_json) = fisher.fish_for_fhir(&sd_type) {
                    let base_def = StructureDefinition::from_json(&base_json, true);
                    if let Some(rbi) = base_def.index_of_id(&ref_id) {
                        let ref_type = base_def.elements[rbi].get("type").cloned();
                        let new_ids =
                            self.clone_children_from_def(&base_def, &ref_id, &parent_id, true);
                        if !new_ids.is_empty() {
                            if let Some(t) = ref_type {
                                self.elements[idx].set("type", t);
                            }
                            self.elements[idx].remove("contentReference");
                        }
                        return new_ids;
                    }
                }
            }
            // Constrained-snapshot branch (profile-element extension) + fallback.
            if let Some(ri) = self.index_of_id(&ref_id) {
                let ref_type = self.elements[ri].get("type").cloned();
                let new_ids = self.clone_children_from(&ref_id, &parent_id, idx, true);
                if !new_ids.is_empty() {
                    if let Some(t) = ref_type {
                        self.elements[idx].set("type", t);
                    }
                    self.elements[idx].remove("contentReference");
                }
                return new_ids;
            }
            return vec![];
        }
        let codes = self.elements[idx].type_codes();
        let is_choice = id.ends_with("[x]");
        let profiles: Vec<String> = self.elements[idx]
            .get("type")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|t| t.get("profile"))
            .and_then(|p| p.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let proceed = codes.len() == 1 && (!is_choice || profiles.len() <= 1);
        if !proceed {
            return vec![];
        }
        let profile_to_use = if profiles.len() == 1 {
            Some(profiles[0].clone())
        } else {
            None
        };

        // sliceName branch: clone children from the sliced element (if it has any).
        let parent_id = self.elements[idx].id().to_string();
        if self.elements[idx].slice_name().is_some() {
            if let Some(sliced_id) = self.sliced_element_id(&parent_id) {
                let child_ids = self.children_ids(&sliced_id);
                if !child_ids.is_empty() {
                    // sliceName unfold uses cloneChildren(slicedElement, false):
                    // slice extensions keep their inherited original so they still
                    // appear as diffs in the slice (ElementDefinition.ts:2742).
                    return self.clone_children_from(&sliced_id, &parent_id, idx, false);
                }
            }
        }

        // type-fishing fallback
        let fish_name = profile_to_use.clone().unwrap_or_else(|| codes[0].clone());
        let json = match fisher
            .fish_for_fhir(&fish_name)
            .or_else(|| fisher.fish_for_fhir(&codes[0]))
        {
            Some(j) => j,
            None => return vec![],
        };
        let def = StructureDefinition::from_json(&json, true);
        if def.elements.len() <= 1 {
            return vec![];
        }
        let def_pt = def.path_type();
        let mut new_children = Vec::new();
        for child in def.elements.iter().skip(1) {
            let mut ed = child.clone();
            let old_id = ed.id().to_string();
            let new_id = old_id.replacen(&def_pt, &parent_id, 1);
            ed.set_id(new_id);
            ed.capture_original();
            new_children.push(ed);
        }
        self.add_elements(new_children)
    }

    /// Clone children of `from_id` to be children of `to_id`. Port of
    /// `ElementDefinition.cloneChildren`: each child captures a fresh original
    /// UNLESS it is a slice extension (sliceName set + path ends in `.extension`)
    /// and `recapture_slice_extensions` is false — in that case the child keeps its
    /// inherited original (only `_original.id` is re-pointed) so the slice still
    /// shows as a diff against base. `recaptureSliceExtensions` (ElementDefinition.ts:2814).
    fn clone_children_from(
        &mut self,
        from_id: &str,
        to_id: &str,
        _parent_idx: usize,
        recapture_slice_extensions: bool,
    ) -> Vec<String> {
        let child_ids = self.children_ids(from_id);
        let mut clones = Vec::new();
        for cid in &child_ids {
            let i = self.index_of_id(cid).unwrap();
            let mut ed = self.elements[i].clone();
            let new_id = cid.replacen(from_id, to_id, 1);
            ed.set_id(new_id);
            remove_uninherited(&mut ed);
            reclone_capture(&mut ed, recapture_slice_extensions);
            clones.push(ed);
        }
        self.add_elements(clones)
    }

    /// Like `clone_children_from`, but the source children come from a *separate*
    /// StructureDefinition (`src`) — used to unfold a contentReference against the
    /// unconstrained base resource rather than this profile's constrained snapshot.
    fn clone_children_from_def(
        &mut self,
        src: &StructureDefinition,
        from_id: &str,
        to_id: &str,
        recapture_slice_extensions: bool,
    ) -> Vec<String> {
        let child_ids = src.children_ids(from_id);
        let mut clones = Vec::new();
        for cid in &child_ids {
            let i = src.index_of_id(cid).unwrap();
            let mut ed = src.elements[i].clone();
            let new_id = cid.replacen(from_id, to_id, 1);
            ed.set_id(new_id);
            remove_uninherited(&mut ed);
            reclone_capture(&mut ed, recapture_slice_extensions);
            clones.push(ed);
        }
        self.add_elements(clones)
    }

    /// `addElements` — insert each via `add_element` (ordering). Returns new ids.
    pub fn add_elements(&mut self, els: Vec<ElementDefinition>) -> Vec<String> {
        let mut ids = Vec::new();
        for e in els {
            ids.push(e.id().to_string());
            self.add_element(e);
        }
        ids
    }

    /// `addElement` — splice into the proper snapshot position. Port of
    /// `StructureDefinition.ts:163`.
    pub fn add_element(&mut self, element: ElementDefinition) {
        let id = element.id().to_string();
        let parent_id = id.rfind('.').map(|i| id[..i].to_string());
        let Some(parent_id) = parent_id else {
            self.elements.push(element);
            return;
        };
        if self.index_of_id(&parent_id).is_none() {
            self.elements.push(element);
            return;
        }
        if element.slice_name().is_some() {
            // start at sliced element, walk forward while ids stay under lastMatch
            let sliced = self.sliced_element_id(&id);
            let mut i = sliced
                .as_deref()
                .and_then(|s| self.index_of_id(s))
                .unwrap_or(0);
            let mut last_match = self.elements[i].id().to_string();
            while i < self.elements.len() {
                let cur = self.elements[i].id().to_string();
                if starts_with_boundary(&id, &cur) {
                    last_match = cur;
                } else {
                    let under = starts_with_any_boundary(&cur, &last_match);
                    let elem_dot = starts_with_dot(&id, &last_match);
                    let cur_slice = starts_with_slice(&cur, &last_match);
                    if !under || (elem_dot && cur_slice) {
                        break;
                    }
                }
                i += 1;
            }
            self.elements.insert(i, element);
        } else {
            // plain child: insert after older sibling's deepest child, or after parent.
            let siblings: Vec<usize> = (0..self.elements.len())
                .filter(|&j| {
                    let cid = self.elements[j].id();
                    cid != id
                        && cid.starts_with(&format!("{parent_id}."))
                        && path_depth(self.elements[j].path())
                            == path_depth(self.path_of_id(&parent_id).unwrap_or("")) + 1
                })
                .collect();
            if siblings.is_empty() {
                let pidx = self.index_of_id(&parent_id).unwrap();
                self.elements.insert(pidx + 1, element);
            } else {
                let older = *siblings.last().unwrap();
                let older_id = self.elements[older].id().to_string();
                // deepest descendant of older sibling
                let mut insert_at = older;
                for j in older..self.elements.len() {
                    if self.elements[j].id() == older_id
                        || self.elements[j].id().starts_with(&format!("{older_id}."))
                        || self.elements[j].id().starts_with(&format!("{older_id}:"))
                    {
                        insert_at = j;
                    } else {
                        break;
                    }
                }
                self.elements.insert(insert_at + 1, element);
            }
        }
    }

    /// `addSlice(parent_idx, name, type)` — create a slice element, returns its id.
    pub fn add_slice(&mut self, parent_idx: usize, name: &str, type_: Option<Value>) -> Option<String> {
        let parent = &self.elements[parent_idx];
        if parent.get("slicing").is_none() && parent.slice_name().is_none() {
            return None;
        }
        let parent_id = parent.id().to_string();
        let parent_max = parent.get("max").cloned();
        let parent_min = parent.get("min").cloned();
        let parent_is_slice = parent.slice_name().is_some();
        let slice_id = if parent_is_slice {
            format!("{parent_id}/{name}")
        } else {
            format!("{parent_id}:{name}")
        };
        if self.index_of_id(&slice_id).is_some() {
            return None;
        }
        let mut slice = parent.clone();
        {
            let m = slice.map_mut();
            m.remove("slicing");
        }
        slice.set_id(slice_id.clone());
        {
            let m = slice.map_mut();
            m.remove("min");
            m.remove("max");
            m.remove("mustSupport");
        }
        slice.capture_original();
        let slice_name = if parent_is_slice {
            format!("{}/{name}", parent.slice_name().unwrap())
        } else {
            name.to_string()
        };
        slice.set("sliceName", Value::String(slice_name));
        // min: 0 unless single-type choice discriminated by type/$this
        let keep_min = parent.path().ends_with("[x]")
            && parent.type_codes().len() == 1
            && parent
                .get("slicing")
                .and_then(|s| s.get("discriminator"))
                .and_then(|d| d.as_array())
                .and_then(|a| a.first())
                .map(|d| {
                    d.get("type").and_then(|v| v.as_str()) == Some("type")
                        && d.get("path").and_then(|v| v.as_str()) == Some("$this")
                })
                .unwrap_or(false);
        if keep_min {
            if let Some(m) = parent_min {
                slice.set("min", m);
            }
        } else {
            slice.set("min", Value::Number(0.into()));
        }
        if let Some(m) = parent_max {
            slice.set("max", m);
        }
        if let Some(t) = type_ {
            slice.set("type", Value::Array(vec![t]));
        }
        self.add_element(slice);
        Some(slice_id)
    }

    pub fn differential_elements(&self) -> Vec<Value> {
        let mut out = Vec::new();
        let specialization = self.derivation() == "specialization";
        for (idx, e) in self.elements.iter().enumerate() {
            if self.element_has_diff(idx) || (specialization && idx == 0) {
                out.push(e.calculate_diff_json());
            }
        }
        if out.is_empty() {
            if let Some(root) = self.elements.first() {
                let mut m = Map::new();
                m.insert("id".into(), Value::String(root.id().to_string()));
                m.insert("path".into(), Value::String(root.path().to_string()));
                out.push(Value::Object(m));
            }
        }
        out
    }

    fn element_has_diff(&self, idx: usize) -> bool {
        let e = &self.elements[idx];
        if e.has_own_diff() {
            return true;
        }
        let is_slice = e.slice_name().is_some();
        let has_slices = self.get_slices(idx).next().is_some();
        if is_slice || has_slices {
            // TS `children()` matches only `.`-descendants (slices excluded).
            let prefix = format!("{}.", e.id());
            for (j, c) in self.elements.iter().enumerate() {
                if j != idx && c.id().starts_with(&prefix) && c.has_own_diff() {
                    return true;
                }
            }
        }
        false
    }

    fn get_slices(&self, idx: usize) -> impl Iterator<Item = usize> + '_ {
        let e = &self.elements[idx];
        let path = e.path().to_string();
        let is_slice = e.slice_name().is_some();
        // Boundary-prefixed id: `id/` for a slice's reslices, `id:` for slices.
        let prefix = format!("{}{}", e.id(), if is_slice { '/' } else { ':' });
        (0..self.elements.len()).filter(move |&j| {
            if j == idx {
                return false;
            }
            let c = &self.elements[j];
            if c.path() != path {
                return false;
            }
            c.id().starts_with(&prefix)
        })
    }

    /// `toJSON(snapshot=true)` — includes the full snapshot. Used when a child
    /// SD fishes this (local) parent to load its elements.
    pub fn to_json_snapshot(&self) -> Value {
        let mut v = self.to_json_differential();
        if let Value::Object(ref mut m) = v {
            let snap: Vec<Value> = self.elements.iter().map(|e| e.to_json()).collect();
            let mut so = Map::new();
            so.insert("element".into(), Value::Array(snap));
            // insert snapshot before differential
            let diff = m.remove("differential");
            m.insert("snapshot".into(), Value::Object(so));
            if let Some(d) = diff {
                m.insert("differential".into(), d);
            }
            // Restore full mapping array (snapshot mode keeps inherited mappings).
            if let Some(mapping) = self.body.get("mapping") {
                m.insert("mapping".into(), mapping.clone());
            }
        }
        v
    }

    pub fn to_json_differential(&self) -> Value {
        let mut j = Map::new();
        j.insert(
            "resourceType".into(),
            Value::String("StructureDefinition".into()),
        );
        for prop in SD_PROPS {
            if *prop == "mapping" {
                if let Some(cur) = self.body.get("mapping").and_then(|v| v.as_array()) {
                    let new: Vec<Value> = cur
                        .iter()
                        .filter(|m| !self.original_mapping.contains(*m))
                        .cloned()
                        .collect();
                    if !new.is_empty() {
                        j.insert("mapping".into(), Value::Array(new));
                    }
                }
                continue;
            }
            if let Some(v) = self.body.get(*prop) {
                j.insert((*prop).to_string(), v.clone());
            }
            let uk = format!("_{prop}");
            if let Some(v) = self.body.get(&uk) {
                j.insert(uk, v.clone());
            }
        }
        let mut diff_obj = Map::new();
        diff_obj.insert("element".into(), Value::Array(self.differential_elements()));
        j.insert("differential".into(), Value::Object(diff_obj));
        if self.in_progress {
            j.insert("inProgress".into(), Value::Bool(true));
        }
        Value::Object(j)
    }
}

// --- path helpers ---

/// Minimal FSH path part (base + bracket contents).
pub struct PathPart {
    pub base: String,
    pub brackets: Vec<String>,
}

/// Parse a dotted FSH path into base+brackets parts (split on '.' outside `[]`).
pub fn parse_fsh_path(path: &str) -> Vec<PathPart> {
    if path.is_empty() {
        return vec![];
    }
    // split on '.' not inside brackets
    let mut segs: Vec<String> = Vec::new();
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
            '.' if depth == 0 => segs.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    segs.push(cur);
    segs.into_iter()
        .map(|seg| {
            let nb = seg.find('[').unwrap_or(seg.len());
            let mut base = seg[..nb].to_string();
            let mut rest = &seg[nb..];
            if rest.starts_with("[x]") {
                base.push_str("[x]");
                rest = &rest[3..];
            }
            let mut brackets = Vec::new();
            let chars: Vec<char> = rest.chars().collect();
            let mut i = 0;
            while i < chars.len() {
                if chars[i] == '[' {
                    let mut depth = 1;
                    let mut j = i + 1;
                    let mut inner = String::new();
                    while j < chars.len() && depth > 0 {
                        match chars[j] {
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
            PathPart { base, brackets }
        })
        .collect()
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn path_depth(path: &str) -> usize {
    path.split('.').count().saturating_sub(1)
}

/// Port of `ElementDefinition.hasProfileElementExtension`: returns true when the
/// referenced element (`element_name`) in this profile's differential carries the
/// elementdefinition-profile-element extension pointing back at itself. When true,
/// a contentReference unfolds from the constrained snapshot rather than base.
fn has_profile_element_extension(profile_json: &Value, element_name: &str, sd_url: &str) -> bool {
    const PROFILE_ELEMENT_EXTENSION: &str =
        "http://hl7.org/fhir/StructureDefinition/elementdefinition-profile-element";
    let Some(diff) = profile_json
        .get("differential")
        .and_then(|d| d.get("element"))
        .and_then(|e| e.as_array())
    else {
        return false;
    };
    let Some(elem) = diff
        .iter()
        .find(|e| e.get("id").and_then(|v| v.as_str()) == Some(element_name))
    else {
        return false;
    };
    let Some(etype) = elem
        .get("type")
        .and_then(|t| t.as_array())
        .and_then(|a| a.first())
    else {
        return false;
    };
    let (Some(profiles), Some(uprofiles)) = (
        etype.get("profile").and_then(|p| p.as_array()),
        etype.get("_profile").and_then(|p| p.as_array()),
    ) else {
        return false;
    };
    let has_ext = |prof: &Value| -> bool {
        prof.get("extension")
            .and_then(|e| e.as_array())
            .map(|exts| {
                exts.iter().any(|ext| {
                    ext.get("url").and_then(|u| u.as_str()) == Some(PROFILE_ELEMENT_EXTENSION)
                        && ext.get("valueString").is_some()
                })
            })
            .unwrap_or(false)
    };
    let Some(pi) = uprofiles.iter().position(|p| p.is_object() && has_ext(p)) else {
        return false;
    };
    let exts = uprofiles[pi].get("extension").and_then(|e| e.as_array());
    let target_element = exts
        .and_then(|e| {
            e.iter().find(|x| {
                x.get("url").and_then(|u| u.as_str()) == Some(PROFILE_ELEMENT_EXTENSION)
                    && x.get("valueString").is_some()
            })
        })
        .and_then(|x| x.get("valueString"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let profile_canonical = profiles.get(pi).and_then(|v| v.as_str()).unwrap_or("");
    profile_canonical == sd_url && target_element == element_name
}

/// `^{escapePath(prefix)}[.:/]` test: id starts with prefix then a boundary char.
fn starts_with_boundary(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix)
        .map(|r| matches!(r.chars().next(), Some('.') | Some(':') | Some('/')))
        .unwrap_or(false)
}
fn starts_with_any_boundary(id: &str, prefix: &str) -> bool {
    starts_with_boundary(id, prefix)
}
fn starts_with_dot(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix)
        .map(|r| r.starts_with('.'))
        .unwrap_or(false)
}
fn starts_with_slice(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix)
        .map(|r| matches!(r.chars().next(), Some(':') | Some('/')))
        .unwrap_or(false)
}

/// Port of the per-child capture logic in `ElementDefinition.cloneChildren`:
/// `shouldCaptureOriginal = recaptureSliceExtensions || sliceName == null ||
/// !path.endsWith('.extension')`. When capturing, snapshot the current state as the
/// new original; otherwise keep the inherited original and only re-point its `id`.
fn reclone_capture(ed: &mut ElementDefinition, recapture_slice_extensions: bool) {
    let should_capture = recapture_slice_extensions
        || ed.slice_name().is_none()
        || !ed.path().ends_with(".extension");
    if should_capture {
        ed.capture_original();
    } else {
        let new_id = ed.id().to_string();
        if let Some(orig) = ed.original.as_mut() {
            Rc::make_mut(orig).insert("id".into(), Value::String(new_id));
        }
    }
}

fn remove_uninherited(ed: &mut ElementDefinition) {
    const UNINHERITED: &[&str] = &[
        "http://hl7.org/fhir/tools/StructureDefinition/binding-definition",
        "http://hl7.org/fhir/tools/StructureDefinition/no-binding",
        "http://hl7.org/fhir/StructureDefinition/elementdefinition-isCommonBinding",
        "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status",
        "http://hl7.org/fhir/StructureDefinition/structuredefinition-category",
        "http://hl7.org/fhir/StructureDefinition/structuredefinition-fmm",
        "http://hl7.org/fhir/StructureDefinition/structuredefinition-implements",
        "http://hl7.org/fhir/StructureDefinition/structuredefinition-explicit-type-name",
        "http://hl7.org/fhir/StructureDefinition/structuredefinition-security-category",
        "http://hl7.org/fhir/StructureDefinition/structuredefinition-wg",
        "http://hl7.org/fhir/StructureDefinition/structuredefinition-normative-version",
        "http://hl7.org/fhir/tools/StructureDefinition/obligation-profile",
        "http://hl7.org/fhir/StructureDefinition/obligation-profile",
        "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status-reason",
        "http://hl7.org/fhir/StructureDefinition/structuredefinition-summary",
    ];
    // Avoid forking the COW map if there is nothing to strip.
    let has_uninherited = ed
        .map
        .get("extension")
        .and_then(|v| v.as_array())
        .map(|exts| {
            exts.iter().any(|e| {
                let u = e.get("url").and_then(|v| v.as_str()).unwrap_or("");
                UNINHERITED.contains(&u)
            })
        })
        .unwrap_or(false);
    if !has_uninherited {
        return;
    }
    let m = ed.map_mut();
    let mut became_empty = false;
    if let Some(Value::Array(exts)) = m.get_mut("extension") {
        exts.retain(|e| {
            let u = e.get("url").and_then(|v| v.as_str()).unwrap_or("");
            !UNINHERITED.contains(&u)
        });
        became_empty = exts.is_empty();
    }
    if became_empty {
        m.remove("extension");
    }
}



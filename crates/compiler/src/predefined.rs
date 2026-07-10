//! Predefined resources (`input/resources`, `input/profiles`, etc.).
//!
//! Stock SUSHI loads these through `DiskBasedVirtualPackage` as
//! `sushi-local#LOCAL` (`sushi-ts/src/ig/predefinedResources.ts`) and the
//! `MasterFisher` checks that package before the local package, tank, or external
//! FHIR definitions. This module mirrors that read-side behavior for compiler
//! consumers that need full predefined resources, not just ValueSet metadata.

use fhir_model::Metadata;
use package_store::{FishType, PackageStore};
use serde_json::{Map, Number, Value as J};
use serde_yaml::Value as Y;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Clone)]
pub struct PredefinedResource {
    pub body: Rc<J>,
    pub resource_type: String,
    pub id: String,
    pub title: Option<String>,
    pub name: Option<String>,
    pub url: Option<String>,
    pub description: Option<String>,
    pub folder: String,
    pub file_stem: String,
    pub meta_profile: Vec<String>,
    pub path: PathBuf,
    fish_type: FishType,
    metadata_fishable: bool,
    seq: usize,
}

#[derive(Default)]
pub struct PredefinedPackage {
    resources: Vec<PredefinedResource>,
    by_id: HashMap<String, Vec<usize>>,
    by_name: HashMap<String, Vec<usize>>,
    by_url: HashMap<String, Vec<usize>>,
}

impl PredefinedPackage {
    pub fn load(ig_dir: &str, cfg_yaml: &Y, store: &PackageStore) -> PredefinedPackage {
        let fish = |name: &str| store.fish_for_fhir(name, package_store::ALL_FISH_TYPES);
        let reader = FhirXmlReader::new(&fish);
        let mut pkg = PredefinedPackage::default();
        for path in collect_predefined_paths(ig_dir, cfg_yaml) {
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            let body = match ext.as_str() {
                "json" => std::fs::read(&path)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<J>(&b).ok()),
                "xml" => std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| reader.parse(&text)),
                _ => None,
            };
            let Some(body) = body else { continue };
            pkg.push(path, body);
        }
        pkg
    }

    /// In-memory sibling of [`PredefinedPackage::load`] (no `std::fs`): take the
    /// already-parsed `(path, body)` predefined resources — the browser feeds
    /// `input/resources/**` JSON bodies decoded JS-side — and index them exactly as
    /// the disk path does. `entries` MUST be in the same order the disk path would
    /// visit them (`collect_predefined_paths` order: the fixed sub-dir list, then
    /// `path-resource` params, each dir's files sorted). XML parsing is disk-only;
    /// the browser hands JSON bodies straight through, so there is no reader here.
    pub fn load_from(entries: Vec<(PathBuf, J)>) -> PredefinedPackage {
        let mut pkg = PredefinedPackage::default();
        for (path, body) in entries {
            pkg.push(path, body);
        }
        pkg
    }

    pub fn resources(&self) -> &[PredefinedResource] {
        &self.resources
    }

    pub fn value_set_url_map(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for r in &self.resources {
            if r.resource_type != "ValueSet" {
                continue;
            }
            let Some(url) = r.url.clone() else { continue };
            if let Some(n) = &r.name {
                map.entry(n.clone()).or_insert_with(|| url.clone());
            }
            if !r.id.is_empty() {
                map.entry(r.id.clone()).or_insert_with(|| url.clone());
            }
            map.entry(url.clone()).or_insert(url);
        }
        map
    }

    pub fn fish_for_fhir(&self, item: &str, types: &[FishType]) -> Option<Rc<J>> {
        let idx = self.resolve(item, types)?;
        Some(self.resources[idx].body.clone())
    }

    pub fn fish_for_metadata(&self, item: &str, types: &[FishType]) -> Option<Metadata> {
        let idx = self.resolve_metadata(item, types)?;
        Some(metadata_from_predefined(&self.resources[idx]))
    }

    fn push(&mut self, path: PathBuf, body: J) {
        let Some(rt) = body
            .get("resourceType")
            .and_then(|v| v.as_str())
            .map(str::to_string)
        else {
            return;
        };
        let id = body
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let name = body
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let url = body.get("url").and_then(|v| v.as_str()).map(str::to_string);
        let title = body
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let description = body
            .get("description")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let fish_type = classify_predefined(&body);
        let metadata_fishable = matches!(
            rt.as_str(),
            "StructureDefinition" | "ValueSet" | "CodeSystem"
        );
        let folder = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file_stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
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
        let seq = self.resources.len();
        let entry = PredefinedResource {
            body: Rc::new(body),
            resource_type: rt,
            id: id.clone(),
            title,
            name: name.clone(),
            url: url.clone(),
            description,
            folder,
            file_stem,
            meta_profile,
            path,
            fish_type,
            metadata_fishable,
            seq,
        };
        let idx = self.resources.len();
        self.resources.push(entry);
        if !id.is_empty() {
            self.by_id.entry(id).or_default().push(idx);
        }
        if let Some(n) = name {
            if !n.is_empty() {
                self.by_name.entry(n).or_default().push(idx);
            }
        }
        if let Some(u) = url {
            if !u.is_empty() {
                self.by_url.entry(u).or_default().push(idx);
            }
        }
    }

    fn resolve(&self, item: &str, types: &[FishType]) -> Option<usize> {
        let (base, version) = match item.split_once('|') {
            Some((b, v)) => (b, Some(v)),
            None => (item, None),
        };
        let wildcard = types.iter().any(|t| *t == FishType::Instance);
        let mut candidates = Vec::new();
        for map in [&self.by_id, &self.by_name, &self.by_url] {
            if let Some(v) = map.get(base) {
                candidates.extend_from_slice(v);
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        candidates.retain(|&i| {
            let r = &self.resources[i];
            if let Some(ver) = version {
                if r.body.get("version").and_then(|v| v.as_str()) != Some(ver) {
                    return false;
                }
            }
            wildcard || types.contains(&r.fish_type)
        });
        candidates.into_iter().min_by(|&a, &b| {
            let ea = &self.resources[a];
            let eb = &self.resources[b];
            fish_rank(ea.fish_type)
                .cmp(&fish_rank(eb.fish_type))
                .then_with(|| eb.seq.cmp(&ea.seq))
        })
    }
}

impl PredefinedPackage {
    fn resolve_metadata(&self, item: &str, types: &[FishType]) -> Option<usize> {
        let (base, version) = match item.split_once('|') {
            Some((b, v)) => (b, Some(v)),
            None => (item, None),
        };
        let wildcard = types.iter().any(|t| *t == FishType::Instance);
        let mut candidates = Vec::new();
        for map in [&self.by_id, &self.by_name, &self.by_url] {
            if let Some(v) = map.get(base) {
                candidates.extend_from_slice(v);
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        candidates.retain(|&i| {
            let r = &self.resources[i];
            if !r.metadata_fishable {
                return false;
            }
            if let Some(ver) = version {
                if r.body.get("version").and_then(|v| v.as_str()) != Some(ver) {
                    return false;
                }
            }
            wildcard || types.contains(&r.fish_type)
        });
        candidates.into_iter().min_by(|&a, &b| {
            let ea = &self.resources[a];
            let eb = &self.resources[b];
            fish_rank(ea.fish_type)
                .cmp(&fish_rank(eb.fish_type))
                .then_with(|| eb.seq.cmp(&ea.seq))
        })
    }
}

fn classify_predefined(body: &J) -> FishType {
    match body.get("resourceType").and_then(|v| v.as_str()) {
        Some("ValueSet") => FishType::ValueSet,
        Some("CodeSystem") => FishType::CodeSystem,
        Some("StructureDefinition") => {
            if body.get("type").and_then(|v| v.as_str()) == Some("Extension") {
                FishType::Extension
            } else if body.get("derivation").and_then(|v| v.as_str()) == Some("constraint") {
                FishType::Profile
            } else {
                match body.get("kind").and_then(|v| v.as_str()) {
                    Some("logical") => FishType::Logical,
                    Some("resource") => FishType::Resource,
                    _ => FishType::Type,
                }
            }
        }
        // FPL loads arbitrary predefined FHIR resources when allowNonResources is
        // true. Treat them as Resource so untyped `fishForFHIR` can embed them.
        _ => FishType::Resource,
    }
}

fn fish_rank(t: FishType) -> u8 {
    match t {
        FishType::Resource => 0,
        FishType::Logical => 1,
        FishType::Type => 2,
        FishType::Profile => 3,
        FishType::Extension => 4,
        FishType::ValueSet => 5,
        FishType::CodeSystem => 6,
        FishType::Instance => 7,
    }
}

fn metadata_from_predefined(r: &PredefinedResource) -> Metadata {
    let body = &r.body;
    let mut out = Metadata {
        id: r.id.clone(),
        name: r.name.clone().unwrap_or_default(),
        sd_type: body
            .get("type")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        url: r.url.clone(),
        parent: body
            .get("baseDefinition")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        abstract_: body.get("abstract").and_then(|v| v.as_bool()),
        version: body
            .get("version")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        kind: body
            .get("kind")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        can_bind: false,
        can_be_target: false,
        instance_usage: None,
    };
    if out.kind.as_deref() == Some("logical") {
        let chars = sd_characteristics(body);
        out.can_be_target = chars.iter().any(|c| c == "can-be-target");
        out.can_bind = chars.iter().any(|c| c == "can-bind");
    }
    out
}

fn sd_characteristics(sd: &J) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(arr) = sd.get("characteristics").and_then(|c| c.as_array()) {
        for c in arr {
            if let Some(s) = c.as_str() {
                out.push(s.to_string());
            }
        }
    }
    if let Some(exts) = sd.get("extension").and_then(|e| e.as_array()) {
        for ext in exts {
            if ext.get("url").and_then(|u| u.as_str())
                == Some("http://hl7.org/fhir/StructureDefinition/structuredefinition-type-characteristics")
            {
                if let Some(c) = ext.get("valueCode").and_then(|v| v.as_str()) {
                    out.push(c.to_string());
                }
            }
        }
    }
    out
}

fn collect_predefined_paths(ig_dir: &str, cfg_yaml: &Y) -> Vec<PathBuf> {
    let input = Path::new(ig_dir).join("input");
    let mut dirs = Vec::<PathBuf>::new();
    for end in [
        "capabilities",
        "extensions",
        "models",
        "operations",
        "profiles",
        "resources",
        "vocabulary",
        "examples",
    ] {
        let p = input.join(end);
        if p.is_dir() {
            dirs.push(p);
        }
    }
    if let Some(Y::Mapping(pm)) = yget(cfg_yaml, "parameters") {
        for (k, v) in pm {
            if ystr(k).as_deref() == Some("path-resource") {
                for val in norm_array(v) {
                    if let Some(s) = ystr(&val) {
                        let recursive = s.ends_with("/*");
                        let rel = s.trim_end_matches("/*");
                        let full =
                            Path::new(ig_dir).join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
                        if full.is_dir() {
                            push_unique(&mut dirs, full.clone());
                            if recursive {
                                add_child_dirs(&mut dirs, &full);
                            }
                        }
                    }
                }
            }
        }
    }

    let mut files = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut local: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .filter(|p| {
                matches!(
                    p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()),
                    Some(ref e) if e == "json" || e == "xml"
                )
            })
            .collect();
        local.sort();
        files.extend(local);
    }
    files
}

fn push_unique(v: &mut Vec<PathBuf>, p: PathBuf) {
    if !v.iter().any(|x| x == &p) {
        v.push(p);
    }
}

fn add_child_dirs(v: &mut Vec<PathBuf>, dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    children.sort();
    for child in children {
        push_unique(v, child.clone());
        add_child_dirs(v, &child);
    }
}

fn yget<'a>(v: &'a Y, key: &str) -> Option<&'a Y> {
    v.as_mapping()?.get(Y::String(key.to_string()))
}

fn ystr(v: &Y) -> Option<String> {
    match v {
        Y::String(s) => Some(s.clone()),
        Y::Bool(b) => Some(b.to_string()),
        Y::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn norm_array(v: &Y) -> Vec<Y> {
    match v {
        Y::Sequence(s) => s.clone(),
        Y::Null => vec![],
        other => vec![other.clone()],
    }
}

// ---------------------------------------------------------------------------
// SD-guided FHIR XML -> JSON.
// ---------------------------------------------------------------------------

struct FhirXmlReader<'a> {
    fish: &'a dyn Fn(&str) -> Option<Rc<J>>,
    cache: RefCell<HashMap<String, Option<Rc<XmlSdInfo>>>>,
}

#[derive(Clone)]
struct XmlChildDef {
    name: String,
    key: String,
    type_code: String,
    array: bool,
    primitive: bool,
    backbone: bool,
    content_ref: Option<String>,
    xml_attr: bool,
}

struct XmlChoice {
    stem: String,
    array: bool,
    options: Vec<String>,
}

struct XmlSdInfo {
    path_type: String,
    children: HashMap<String, Vec<XmlChildDef>>,
    choices: HashMap<String, Vec<XmlChoice>>,
}

#[derive(Clone, Debug)]
struct XmlNode {
    tag: String,
    attrs: Vec<(String, String)>,
    children: Vec<XmlNode>,
}

impl<'a> FhirXmlReader<'a> {
    fn new(fish: &'a dyn Fn(&str) -> Option<Rc<J>>) -> Self {
        FhirXmlReader {
            fish,
            cache: RefCell::new(HashMap::new()),
        }
    }

    fn parse(&self, text: &str) -> Option<J> {
        let root = parse_xml_tree(text)?;
        self.parse_resource_node(&root)
    }

    fn parse_resource_node(&self, node: &XmlNode) -> Option<J> {
        let info = self.info(&node.tag)?;
        let mut obj = Map::new();
        obj.insert("resourceType".into(), J::String(node.tag.clone()));
        self.parse_complex_into(&mut obj, node, &info, &info.path_type);
        Some(J::Object(obj))
    }

    fn parse_complex_node(&self, node: &XmlNode, info: Rc<XmlSdInfo>, prefix: &str) -> J {
        let mut obj = Map::new();
        self.parse_complex_into(&mut obj, node, &info, prefix);
        J::Object(obj)
    }

    fn parse_complex_into(
        &self,
        obj: &mut Map<String, J>,
        node: &XmlNode,
        info: &Rc<XmlSdInfo>,
        prefix: &str,
    ) {
        let defer_extension_url = node.tag == "extension"
            && node.children.iter().any(|c| c.tag == "extension")
            && !node.children.iter().any(|c| c.tag.starts_with("value"));
        if !defer_extension_url {
            self.apply_xml_attrs(obj, node, info, prefix);
        }
        for child in &node.children {
            if let Some(def) = info.child(prefix, &child.tag) {
                let value = self.parse_child_value(child, &def, info, prefix);
                self.insert_child(obj, &def, value);
            }
        }
        if defer_extension_url {
            self.apply_xml_attrs(obj, node, info, prefix);
        }
    }

    fn apply_xml_attrs(
        &self,
        obj: &mut Map<String, J>,
        node: &XmlNode,
        info: &Rc<XmlSdInfo>,
        prefix: &str,
    ) {
        let Some(children) = info.children.get(prefix) else {
            return;
        };
        for def in children.iter().filter(|d| d.xml_attr) {
            if let Some(raw) = attr(&node.attrs, &def.name) {
                let value = primitive_json(&def.type_code, raw);
                self.insert_child(obj, def, value);
            }
        }
    }

    fn parse_child_value(
        &self,
        node: &XmlNode,
        def: &XmlChildDef,
        current: &Rc<XmlSdInfo>,
        current_prefix: &str,
    ) -> J {
        if def.type_code == "Resource" {
            if let Some(resource_child) = node.children.iter().find(|c| self.info(&c.tag).is_some())
            {
                return self.parse_resource_node(resource_child).unwrap_or(J::Null);
            }
        }
        if def.primitive {
            return self.parse_primitive_node(node, def, current);
        }
        let (next, prefix) = self.descend(def, current, current_prefix);
        match next {
            Some(info) => self.parse_complex_node(node, info, &prefix),
            None => {
                let mut obj = Map::new();
                for child in &node.children {
                    obj.insert(child.tag.clone(), self.fallback_node(child));
                }
                J::Object(obj)
            }
        }
    }

    fn parse_primitive_node(
        &self,
        node: &XmlNode,
        def: &XmlChildDef,
        _current: &Rc<XmlSdInfo>,
    ) -> J {
        let main = attr(&node.attrs, "value")
            .map(|v| primitive_json(&def.type_code, v))
            .unwrap_or(J::Null);
        if node.children.is_empty() && attr(&node.attrs, "id").is_none() {
            return main;
        }
        // Primitive children/attributes are represented by the caller as
        // `_name`; stash the sidecar object in a marker object so insertion can
        // add both siblings in order.
        let mut side = Map::new();
        let element = self
            .info("Element")
            .or_else(|| self.info("BackboneElement"));
        if let Some(element) = element {
            self.apply_xml_attrs(&mut side, node, &element, "Element");
        }
        for child in &node.children {
            let child_def = self
                .info("Element")
                .and_then(|el| el.child("Element", &child.tag).map(|d| (el, d)));
            if let Some((el, cd)) = child_def {
                let value = self.parse_child_value(child, &cd, &el, "Element");
                self.insert_child(&mut side, &cd, value);
            }
        }
        let mut wrapper = Map::new();
        wrapper.insert("$value".into(), main);
        wrapper.insert("$sidecar".into(), J::Object(side));
        J::Object(wrapper)
    }

    fn insert_child(&self, obj: &mut Map<String, J>, def: &XmlChildDef, value: J) {
        if let Some(marker) = value.as_object().filter(|m| m.contains_key("$value")) {
            let main = marker.get("$value").cloned().unwrap_or(J::Null);
            if !main.is_null() {
                self.insert_value(obj, &def.key, def.array, main);
            }
            if let Some(side) = marker.get("$sidecar").and_then(|v| v.as_object()) {
                if !side.is_empty() {
                    self.insert_value(
                        obj,
                        &format!("_{}", def.key),
                        def.array,
                        J::Object(side.clone()),
                    );
                }
            }
        } else {
            self.insert_value(obj, &def.key, def.array, value);
        }
    }

    fn insert_value(&self, obj: &mut Map<String, J>, key: &str, array: bool, value: J) {
        if array {
            match obj.get_mut(key) {
                Some(J::Array(a)) => a.push(value),
                Some(existing) => {
                    let old = std::mem::replace(existing, J::Array(Vec::new()));
                    if let J::Array(a) = existing {
                        a.push(old);
                        a.push(value);
                    }
                }
                None => {
                    obj.insert(key.to_string(), J::Array(vec![value]));
                }
            }
        } else {
            obj.insert(key.to_string(), value);
        }
    }

    fn descend(
        &self,
        def: &XmlChildDef,
        current: &Rc<XmlSdInfo>,
        current_prefix: &str,
    ) -> (Option<Rc<XmlSdInfo>>, String) {
        if let Some(cr) = &def.content_ref {
            return (Some(current.clone()), cr.clone());
        }
        let prefix = format!("{current_prefix}.{}", def.name);
        if def.backbone && current.children.contains_key(&prefix) {
            return (Some(current.clone()), prefix);
        }
        let info = self.info(&def.type_code);
        let prefix = info
            .as_ref()
            .map(|i| i.path_type.clone())
            .unwrap_or_else(|| def.type_code.clone());
        (info, prefix)
    }

    fn fallback_node(&self, node: &XmlNode) -> J {
        if let Some(v) = attr(&node.attrs, "value") {
            return J::String(v.to_string());
        }
        let mut obj = Map::new();
        for (k, v) in &node.attrs {
            if k != "xmlns" {
                obj.insert(k.clone(), J::String(v.clone()));
            }
        }
        for child in &node.children {
            obj.insert(child.tag.clone(), self.fallback_node(child));
        }
        J::Object(obj)
    }

    fn info(&self, name: &str) -> Option<Rc<XmlSdInfo>> {
        let key = name.rsplit('/').next().unwrap_or(name).to_string();
        if let Some(v) = self.cache.borrow().get(&key) {
            return v.clone();
        }
        let built = (self.fish)(&key)
            .or_else(|| (self.fish)(name))
            .and_then(|sd| XmlSdInfo::build(&sd))
            .map(Rc::new);
        self.cache.borrow_mut().insert(key, built.clone());
        built
    }
}

impl XmlSdInfo {
    fn build(sd: &J) -> Option<XmlSdInfo> {
        let path_type = path_type_of(sd)?;
        let mut children: HashMap<String, Vec<XmlChildDef>> = HashMap::new();
        let mut choices: HashMap<String, Vec<XmlChoice>> = HashMap::new();
        let elements = sd
            .pointer("/snapshot/element")
            .and_then(|v| v.as_array())
            .map(|a| a.as_slice())
            .unwrap_or(&[]);
        for el in elements {
            if el.get("sliceName").is_some() {
                continue;
            }
            let Some(path) = el.get("path").and_then(|v| v.as_str()) else {
                continue;
            };
            if path == path_type {
                continue;
            }
            let Some((parent, raw_name)) = path.rsplit_once('.') else {
                continue;
            };
            let array = is_array_max(el);
            let types = el.get("type").and_then(|v| v.as_array());
            if let Some(stem) = raw_name.strip_suffix("[x]") {
                let options = types
                    .map(|a| {
                        a.iter()
                            .map(|t| fhir_model::type_code(t).to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                choices
                    .entry(parent.to_string())
                    .or_default()
                    .push(XmlChoice {
                        stem: stem.to_string(),
                        array,
                        options,
                    });
                continue;
            }
            let type_code = types
                .and_then(|a| a.first())
                .map(|t| fhir_model::type_code(t).to_string())
                .unwrap_or_default();
            let content_ref = el
                .get("contentReference")
                .and_then(|v| v.as_str())
                .map(|s| s.trim_start_matches('#').to_string());
            let xml_attr = el
                .get("representation")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().any(|v| v.as_str() == Some("xmlAttr")))
                .unwrap_or(false);
            let primitive = is_primitive_type_code(&type_code);
            let backbone =
                type_code == "BackboneElement" || type_code == "Element" || content_ref.is_some();
            children
                .entry(parent.to_string())
                .or_default()
                .push(XmlChildDef {
                    name: raw_name.to_string(),
                    key: raw_name.to_string(),
                    type_code,
                    array,
                    primitive,
                    backbone,
                    content_ref,
                    xml_attr,
                });
        }
        Some(XmlSdInfo {
            path_type,
            children,
            choices,
        })
    }

    fn child(&self, prefix: &str, name: &str) -> Option<XmlChildDef> {
        if let Some(children) = self.children.get(prefix) {
            for child in children {
                if child.name == name {
                    return Some(child.clone());
                }
            }
        }
        if let Some(choices) = self.choices.get(prefix) {
            for choice in choices {
                if let Some(suffix) = name.strip_prefix(&choice.stem) {
                    if suffix.is_empty() {
                        continue;
                    }
                    for code in &choice.options {
                        if choice_matches(suffix, code) {
                            return Some(XmlChildDef {
                                name: choice.stem.clone(),
                                key: name.to_string(),
                                type_code: code.clone(),
                                array: choice.array,
                                primitive: is_primitive_type_code(code),
                                backbone: false,
                                content_ref: None,
                                xml_attr: false,
                            });
                        }
                    }
                }
            }
        }
        if name == "extension" || name == "modifierExtension" {
            return Some(XmlChildDef {
                name: name.to_string(),
                key: name.to_string(),
                type_code: "Extension".into(),
                array: true,
                primitive: false,
                backbone: false,
                content_ref: None,
                xml_attr: false,
            });
        }
        if name == "id" {
            return Some(XmlChildDef {
                name: name.to_string(),
                key: name.to_string(),
                type_code: "string".into(),
                array: false,
                primitive: true,
                backbone: false,
                content_ref: None,
                xml_attr: false,
            });
        }
        None
    }
}

fn path_type_of(sd: &J) -> Option<String> {
    let t = sd.get("type").and_then(|v| v.as_str())?;
    Some(t.rsplit('/').next().unwrap_or(t).to_string())
}

fn is_array_max(el: &J) -> bool {
    match el.get("max").and_then(|v| v.as_str()) {
        Some("0") | Some("1") => false,
        Some(_) => true,
        None => false,
    }
}

fn is_primitive_type_code(code: &str) -> bool {
    matches!(
        code,
        "base64Binary"
            | "boolean"
            | "canonical"
            | "code"
            | "date"
            | "dateTime"
            | "decimal"
            | "id"
            | "instant"
            | "integer"
            | "integer64"
            | "markdown"
            | "oid"
            | "positiveInt"
            | "string"
            | "time"
            | "unsignedInt"
            | "uri"
            | "url"
            | "uuid"
            | "xhtml"
    )
}

fn choice_matches(suffix: &str, code: &str) -> bool {
    suffix == code || suffix == upper_first(code)
}

fn upper_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn primitive_json(type_code: &str, raw: &str) -> J {
    match type_code {
        "boolean" => J::Bool(raw == "true"),
        "integer" | "unsignedInt" | "positiveInt" => raw
            .parse::<i64>()
            .ok()
            .map(|i| J::Number(i.into()))
            .unwrap_or_else(|| J::String(raw.to_string())),
        "integer64" => raw
            .parse::<i64>()
            .ok()
            .map(|i| J::Number(i.into()))
            .unwrap_or_else(|| J::String(raw.to_string())),
        "decimal" => raw
            .parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map(J::Number)
            .unwrap_or_else(|| J::String(raw.to_string())),
        _ => J::String(raw.to_string()),
    }
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn parse_xml_tree(text: &str) -> Option<XmlNode> {
    let mut stack: Vec<XmlNode> = Vec::new();
    let mut root: Option<XmlNode> = None;
    let mut i = 0usize;
    while let Some(rel) = text[i..].find('<') {
        i += rel;
        if text[i..].starts_with("<!--") {
            let end = text[i..].find("-->")?;
            i += end + 3;
            continue;
        }
        if text[i..].starts_with("<![CDATA[") {
            let end = text[i..].find("]]>")?;
            i += end + 3;
            continue;
        }
        if text[i..].starts_with("<?") || text[i..].starts_with("<!") {
            let end = text[i..].find('>')?;
            i += end + 1;
            continue;
        }
        let end = text[i..].find('>')?;
        let tag_text = &text[i + 1..i + end];
        i += end + 1;
        if let Some(rest) = tag_text.strip_prefix('/') {
            let end_tag = local_name(rest.trim());
            let node = stack.pop()?;
            if node.tag != end_tag {
                return None;
            }
            if let Some(parent) = stack.last_mut() {
                parent.children.push(node);
            } else {
                root = Some(node);
            }
            continue;
        }
        let self_closing = tag_text.trim_end().ends_with('/');
        let inner = tag_text.trim_end().trim_end_matches('/').trim();
        let mut parts = inner.splitn(2, |c: char| c.is_whitespace());
        let tag = local_name(parts.next().unwrap_or("")).to_string();
        let attrs = parse_xml_attrs(parts.next().unwrap_or(""));
        let node = XmlNode {
            tag,
            attrs,
            children: Vec::new(),
        };
        if self_closing {
            if let Some(parent) = stack.last_mut() {
                parent.children.push(node);
            } else {
                root = Some(node);
            }
        } else {
            stack.push(node);
        }
    }
    while let Some(node) = stack.pop() {
        if let Some(parent) = stack.last_mut() {
            parent.children.push(node);
        } else {
            root = Some(node);
        }
    }
    root
}

fn local_name(s: &str) -> &str {
    s.rsplit_once(':').map(|(_, l)| l).unwrap_or(s)
}

fn parse_xml_attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i] as char).is_whitespace() {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && bytes[i] != b'=' && !(bytes[i] as char).is_whitespace() {
            i += 1;
        }
        if start == i {
            break;
        }
        let key = local_name(&s[start..i]).to_string();
        while i < bytes.len() && (bytes[i] as char).is_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            continue;
        }
        i += 1;
        while i < bytes.len() && (bytes[i] as char).is_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || (bytes[i] != b'"' && bytes[i] != b'\'') {
            continue;
        }
        let quote = bytes[i];
        i += 1;
        let vstart = i;
        while i < bytes.len() && bytes[i] != quote {
            i += 1;
        }
        let value = xml_unescape(&s[vstart..i]);
        out.push((key, value));
        if i < bytes.len() {
            i += 1;
        }
    }
    out
}

fn xml_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while let Some(rel) = s[i..].find('&') {
        out.push_str(&s[i..i + rel]);
        i += rel;
        let Some(end) = s[i..].find(';') else {
            out.push('&');
            i += 1;
            continue;
        };
        let ent = &s[i + 1..i + end];
        match ent {
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "amp" => out.push('&'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                if let Ok(cp) = u32::from_str_radix(&ent[2..], 16) {
                    if let Some(ch) = char::from_u32(cp) {
                        out.push(ch);
                    }
                }
            }
            _ if ent.starts_with('#') => {
                if let Ok(cp) = ent[1..].parse::<u32>() {
                    if let Some(ch) = char::from_u32(cp) {
                        out.push(ch);
                    }
                }
            }
            _ => {
                out.push('&');
                out.push_str(ent);
                out.push(';');
            }
        }
        i += end + 1;
    }
    out.push_str(&s[i..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_resource_sd() -> Rc<J> {
        Rc::new(json!({
            "resourceType": "StructureDefinition",
            "type": "TestResource",
            "kind": "resource",
            "snapshot": {
                "element": [
                    {"path": "TestResource", "max": "1"},
                    {"path": "TestResource.id", "max": "1", "type": [{"code": "id"}]},
                    {"path": "TestResource.name", "max": "1", "type": [{"code": "string"}]},
                    {"path": "TestResource.alias", "max": "*", "type": [{"code": "string"}]},
                    {"path": "TestResource.extension", "max": "*", "type": [{"code": "Extension"}]},
                    {"path": "TestResource.tag", "max": "*", "type": [{"code": "BackboneElement"}]},
                    {"path": "TestResource.tag.code", "max": "1", "type": [{"code": "code"}]}
                ]
            }
        }))
    }

    fn extension_sd() -> Rc<J> {
        Rc::new(json!({
            "resourceType": "StructureDefinition",
            "type": "Extension",
            "kind": "complex-type",
            "snapshot": {
                "element": [
                    {"path": "Extension", "max": "1"},
                    {"path": "Extension.id", "max": "1", "representation": ["xmlAttr"], "type": [{"code": "string"}]},
                    {"path": "Extension.extension", "max": "*", "type": [{"code": "Extension"}]},
                    {"path": "Extension.url", "max": "1", "representation": ["xmlAttr"], "type": [{"code": "uri"}]},
                    {
                        "path": "Extension.value[x]",
                        "max": "1",
                        "type": [{"code": "string"}, {"code": "integer"}, {"code": "Coding"}]
                    }
                ]
            }
        }))
    }

    #[test]
    fn xml_reader_uses_sd_cardinality_and_decodes_entities() {
        let mut defs = HashMap::new();
        defs.insert("TestResource", test_resource_sd());
        defs.insert("Extension", extension_sd());
        let fish = |name: &str| defs.get(name).cloned();
        let reader = FhirXmlReader::new(&fish);

        let actual = reader
            .parse(
                r#"<TestResource xmlns="http://hl7.org/fhir">
                 <id value="abc"/>
                 <name value="A&#xA;B"/>
                 <alias value="one"/>
                 <tag><code value="x"/></tag>
               </TestResource>"#,
            )
            .unwrap();

        assert_eq!(
            actual,
            json!({
                "resourceType": "TestResource",
                "id": "abc",
                "name": "A\nB",
                "alias": ["one"],
                "tag": [{"code": "x"}]
            })
        );
    }

    #[test]
    fn xml_reader_matches_nested_extension_url_order() {
        let mut defs = HashMap::new();
        defs.insert("TestResource", test_resource_sd());
        defs.insert("Extension", extension_sd());
        let fish = |name: &str| defs.get(name).cloned();
        let reader = FhirXmlReader::new(&fish);

        let actual = reader
            .parse(
                r#"<TestResource xmlns="http://hl7.org/fhir">
                 <extension url="outer">
                   <extension url="inner">
                     <valueString value="v"/>
                   </extension>
                 </extension>
                 <extension url="direct">
                   <valueString value="x"/>
                 </extension>
               </TestResource>"#,
            )
            .unwrap();

        let rendered = serde_json::to_string(&actual).unwrap();
        assert!(rendered.contains(
            r#""extension":[{"extension":[{"url":"inner","valueString":"v"}],"url":"outer"},{"url":"direct","valueString":"x"}]"#
        ));
    }

    #[test]
    fn non_conformance_predefined_resources_do_not_shadow_metadata() {
        let mut pkg = PredefinedPackage::default();
        pkg.push(
            PathBuf::from("input/resources/SearchParameter-practitioner-period.json"),
            json!({
                "resourceType": "SearchParameter",
                "id": "practitioner-period",
                "name": "Plannet_sp_practitioner_period",
                "url": "http://example.org/SearchParameter/practitioner-period"
            }),
        );

        assert!(pkg
            .fish_for_fhir("practitioner-period", package_store::ALL_FISH_TYPES)
            .is_some());
        assert!(pkg
            .fish_for_metadata("practitioner-period", package_store::ALL_FISH_TYPES)
            .is_none());
        assert!(pkg
            .fish_for_metadata("practitioner-period", &[FishType::Extension])
            .is_none());
    }
}

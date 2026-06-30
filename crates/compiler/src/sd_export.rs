//! StructureDefinition export (Phase 5/6). Ports
//! `sushi-ts/src/export/StructureDefinitionExporter.ts` + the ElementDefinition
//! mutation methods, producing byte-identical differential `StructureDefinition-*.json`.

use crate::config::Config;
use crate::paths::resolve_soft_indexing;
use fhir_model::{type_code, ElementDefinition, Fisher, Metadata, StructureDefinition};
use fsh_model::{
    ExtensionContext, FshDocument, OnlyRuleType, Rule, StructureDef, StructureKind,
    Value as FshValue,
};
use serde_json::{json, Map, Value as J};

const UNINHERITED_SD_EXTENSIONS: &[&str] = &[
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-fmm",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-fmm-no-warnings",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-interface",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-normative-version",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-applicable-version",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-category",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-security-category",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-summary",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-wg",
    "http://hl7.org/fhir/StructureDefinition/replaces",
    "http://hl7.org/fhir/StructureDefinition/resource-approvalDate",
    "http://hl7.org/fhir/StructureDefinition/resource-effectivePeriod",
    "http://hl7.org/fhir/StructureDefinition/resource-lastReviewDate",
];

const UNINHERITED_ED_EXTENSIONS: &[&str] = &[
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

// ---------------------------------------------------------------------------
// Tank SD entry + context
// ---------------------------------------------------------------------------

struct TankSd {
    kind: StructureKind,
    def: StructureDef,
}

pub struct ExportedSd {
    pub name: String,
    pub sd: StructureDefinition,
    pub kind: StructureKind,
}

pub struct SdContext<'a> {
    pub store: &'a package_store::PackageStore,
    cfg: &'a Config,
    tank: Vec<TankSd>,
    /// name -> tank index
    by_name: std::collections::HashMap<String, usize>,
    by_id: std::collections::HashMap<String, usize>,
    pub exported: Vec<ExportedSd>,
    in_progress: std::collections::HashSet<String>,
    /// Early-push metadata for in-progress SDs (so circular fishes resolve url).
    in_progress_meta: std::collections::HashMap<String, Metadata>,
    /// name/id -> url-bearing metadata for every local SD in the tank, computed
    /// up front. Lets the fisher resolve a local SD's canonical url before it is
    /// exported (or even if its export fails), mirroring stock's
    /// `FSHTank.fishForMetadata` for StructureDefinitions.
    tank_sd_meta: std::collections::HashMap<String, Metadata>,
    pub diag: Vec<String>,
    vs_url: &'a dyn Fn(&str) -> Option<String>,
    cs_url: &'a dyn Fn(&str) -> Option<String>,
    /// name/id/url -> canonical url for predefined ValueSets (input/resources).
    predefined_vs: std::collections::HashMap<String, String>,
}

/// A read-only fisher over package + already-exported local SDs.
pub struct FisherView<'a> {
    store: &'a package_store::PackageStore,
    exported: &'a [ExportedSd],
    in_progress_meta: &'a std::collections::HashMap<String, Metadata>,
    tank_sd_meta: &'a std::collections::HashMap<String, Metadata>,
    predefined_vs: &'a std::collections::HashMap<String, String>,
}

impl Fisher for FisherView<'_> {
    fn fish_for_fhir(&self, name: &str) -> Option<std::rc::Rc<J>> {
        let name = &resolve_alias_tl(name);
        let base = name.split('|').next().unwrap_or(name);
        // local exported SDs (by name, id, or url)
        for e in self.exported {
            let url = e.sd.url();
            let id = e.sd.get_str("id").unwrap_or("");
            if e.name == base || id == base || url == base {
                return Some(std::rc::Rc::new(e.sd.to_json_snapshot()));
            }
        }
        self.store.fish_for_fhir(name, package_store::ALL_FISH_TYPES)
    }

    fn fish_for_metadata(&self, name: &str) -> Option<Metadata> {
        let name = &resolve_alias_tl(name);
        let base = name.split('|').next().unwrap_or(name);
        for e in self.exported {
            let url = e.sd.url();
            let id = e.sd.get_str("id").unwrap_or("");
            if e.name == base || id == base || url == base {
                return Some(metadata_from_sd(&e.sd));
            }
        }
        // in-progress SDs (early-push): match by name, id, or url.
        for (n, m) in self.in_progress_meta {
            if n == base || m.id == base || m.url.as_deref() == Some(base) {
                return Some(m.clone());
            }
        }
        // Tank SDs not yet exported: resolve a local SD's url before/without its
        // export (FSHTank.fishForMetadata for StructureDefinitions). Checked after
        // the package store would normally run in stock, but a tank SD shadows the
        // external definition of the same name, so it must win over `store`.
        if let Some(m) = self.tank_sd_meta.get(base) {
            return Some(m.clone());
        }
        let m = self.store.fish_for_metadata(name, package_store::ALL_FISH_TYPES)?;
        Some(metadata_from_json(&m))
    }

    fn fish_for_metadata_vs(&self, name: &str) -> Option<Metadata> {
        // Local ValueSets are resolved by the exporter's `vs_url` closure; here
        // we only restrict the package fish to ValueSet definitions.
        // Predefined ValueSets (input/resources/*.{xml,json}) win over packages,
        // mirroring SUSHI's FHIRDefinitions precedence (predefined before packages):
        // a `* path from <Name>` binding resolves to the local canonical url before
        // a wrong same-named THO/core ValueSet (or no match).
        let base = name.split('|').next().unwrap_or(name);
        if let Some(url) = self.predefined_vs.get(base) {
            return Some(Metadata {
                id: String::new(),
                name: base.to_string(),
                sd_type: None,
                url: Some(url.clone()),
                parent: None,
                abstract_: None,
                version: None,
                kind: None,
                can_bind: false,
                can_be_target: false,
                instance_usage: None,
            });
        }
        let m = self
            .store
            .fish_for_metadata(name, &[package_store::FishType::ValueSet])?;
        Some(metadata_from_json(&m))
    }

    fn fish_for_metadata_cs(&self, name: &str) -> Option<Metadata> {
        // Restrict the package fish to CodeSystem definitions, mirroring
        // `fishForMetadata(_, Type.CodeSystem)`. Local FSH CodeSystems are not
        // StructureDefinitions, so they are resolved by callers via the tank's
        // `cs_url`; here we only cover dependency-package CodeSystems.
        let m = self
            .store
            .fish_for_metadata(name, &[package_store::FishType::CodeSystem])?;
        Some(metadata_from_json(&m))
    }
}

fn metadata_from_sd(sd: &StructureDefinition) -> Metadata {
    Metadata {
        id: sd.get_str("id").unwrap_or("").to_string(),
        name: sd.name().to_string(),
        sd_type: Some(sd.type_().to_string()),
        url: Some(sd.url().to_string()),
        parent: sd.get_str("baseDefinition").map(String::from),
        abstract_: sd.body.get("abstract").and_then(|v| v.as_bool()),
        version: sd.get_str("version").map(String::from),
        kind: Some(sd.kind().to_string()),
        can_bind: false,
        can_be_target: false,
        instance_usage: None,
    }
}

fn metadata_from_json(m: &J) -> Metadata {
    Metadata {
        id: m.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        name: m.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        sd_type: m.get("sdType").and_then(|v| v.as_str()).map(String::from),
        url: m.get("url").and_then(|v| v.as_str()).map(String::from),
        parent: m.get("parent").and_then(|v| v.as_str()).map(String::from),
        abstract_: m.get("abstract").and_then(|v| v.as_bool()),
        version: m.get("version").and_then(|v| v.as_str()).map(String::from),
        kind: m.get("kind").and_then(|v| v.as_str()).map(String::from),
        can_bind: m.get("canBind").and_then(|v| v.as_bool()).unwrap_or(false),
        can_be_target: m
            .get("canBeTarget")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        instance_usage: None,
    }
}

impl<'a> SdContext<'a> {
    pub fn new(
        store: &'a package_store::PackageStore,
        cfg: &'a Config,
        docs: &[FshDocument],
        vs_url: &'a dyn Fn(&str) -> Option<String>,
        cs_url: &'a dyn Fn(&str) -> Option<String>,
    ) -> SdContext<'a> {
        // Collect SDs in FHIRExporter order: profiles, extensions, logicals, resources.
        let mut tank = Vec::new();
        let mut by_name = std::collections::HashMap::new();
        let mut by_id = std::collections::HashMap::new();
        let collect = |tank: &mut Vec<TankSd>,
                       by_name: &mut std::collections::HashMap<String, usize>,
                       by_id: &mut std::collections::HashMap<String, usize>,
                       items: &[(String, StructureDef)],
                       kind: StructureKind| {
            for (_k, def) in items {
                let idx = tank.len();
                by_name.entry(def.name.clone()).or_insert(idx);
                by_id.entry(def.id.clone()).or_insert(idx);
                tank.push(TankSd {
                    kind,
                    def: def.clone(),
                });
            }
        };
        for doc in docs {
            collect(&mut tank, &mut by_name, &mut by_id, &doc.profiles, StructureKind::Profile);
        }
        for doc in docs {
            collect(&mut tank, &mut by_name, &mut by_id, &doc.extensions, StructureKind::Extension);
        }
        for doc in docs {
            collect(&mut tank, &mut by_name, &mut by_id, &doc.logicals, StructureKind::Logical);
        }
        for doc in docs {
            collect(&mut tank, &mut by_name, &mut by_id, &doc.resources, StructureKind::Resource);
        }
        let mut tank_sd_meta: std::collections::HashMap<String, Metadata> =
            std::collections::HashMap::new();
        for ts in &tank {
            let id = effective_sd_id(&ts.def);
            let url = sd_url_from_def(&ts.def, &id, &cfg.canonical);
            let md = Metadata {
                id: id.clone(),
                name: ts.def.name.clone(),
                sd_type: None,
                url: Some(url),
                parent: ts.def.parent.clone(),
                abstract_: None,
                version: None,
                kind: None,
                can_bind: false,
                can_be_target: false,
                instance_usage: None,
            };
            tank_sd_meta.entry(ts.def.name.clone()).or_insert_with(|| md.clone());
            tank_sd_meta.entry(id).or_insert(md);
        }
        SdContext {
            store,
            cfg,
            tank,
            by_name,
            by_id,
            exported: Vec::new(),
            in_progress: std::collections::HashSet::new(),
            in_progress_meta: std::collections::HashMap::new(),
            tank_sd_meta,
            diag: Vec::new(),
            vs_url,
            cs_url,
            predefined_vs: std::collections::HashMap::new(),
        }
    }

    /// Install the predefined ValueSet name/id/url -> url map used by the binding
    /// fisher (see `FisherView::fish_for_metadata_vs`).
    pub fn set_predefined_vs(&mut self, map: std::collections::HashMap<String, String>) {
        self.predefined_vs = map;
    }

    pub fn fisher(&self) -> FisherView<'_> {
        FisherView {
            store: self.store,
            exported: &self.exported,
            in_progress_meta: &self.in_progress_meta,
            tank_sd_meta: &self.tank_sd_meta,
            predefined_vs: &self.predefined_vs,
        }
    }

    /// Fish the InstanceOf SD JSON (snapshot) for an instance export, mirroring
    /// `fisher.fishForFHIR(instanceOf, Resource, Profile, Extension, Type, Logical)`.
    pub fn fish_sd_json(&self, name: &str) -> Option<std::rc::Rc<J>> {
        self.fisher().fish_for_fhir(name)
    }

    fn tank_index(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).or_else(|| self.by_id.get(name)).copied()
    }

    /// Export every local SD (in tank order) on demand.
    pub fn export_all(&mut self) {
        for i in 0..self.tank.len() {
            let name = self.tank[i].def.name.clone();
            self.export_sd(&name);
        }
    }

    fn already_exported(&self, name: &str) -> bool {
        self.exported.iter().any(|e| e.name == name)
    }

    /// Port of `MappingExporter` — applied last, mutating already-exported SDs.
    pub fn export_mappings(&mut self, docs: &[FshDocument]) {
        let mappings: Vec<fsh_model::Mapping> =
            docs.iter().flat_map(|d| d.mappings.iter().map(|(_, m)| m.clone())).collect();
        for mapping in &mappings {
            self.export_mapping(mapping);
        }
    }

    fn export_mapping(&mut self, mapping: &fsh_model::Mapping) {
        let Some(source) = &mapping.source else { return };
        let Some(si) = self
            .exported
            .iter()
            .position(|e| &e.name == source || e.sd.get_str("id") == Some(source.as_str()))
        else {
            self.diag.push(format!("Unable to find mapping source {source}"));
            return;
        };
        // Determine whether the parent already carries a mapping with this identity.
        let base_def = self.exported[si].sd.get_str("baseDefinition").map(String::from);
        let parent_match: Option<(Option<String>, Option<String>)> = base_def.and_then(|bd| {
            let parent = self.fisher().fish_for_fhir(&bd)?;
            let maps = parent.get("mapping")?.as_array()?;
            maps.iter()
                .find(|m| m.get("identity").and_then(|v| v.as_str()) == Some(mapping.id.as_str()))
                .map(|m| {
                    (
                        m.get("name").and_then(|v| v.as_str()).map(String::from),
                        m.get("uri").and_then(|v| v.as_str()).map(String::from),
                    )
                })
        });

        let sd = &mut self.exported[si].sd;
        if let Some((p_name, p_uri)) = parent_match {
            let title_ok = mapping.title.as_ref().map(|t| Some(t) == p_name.as_ref()).unwrap_or(true);
            let target_ok = mapping.target.as_ref().map(|t| Some(t) == p_uri.as_ref()).unwrap_or(true);
            if !title_ok || !target_ok {
                self.diag.push(format!(
                    "Unable to add Mapping {} because it conflicts with one already on the parent of {source}.",
                    mapping.name
                ));
                return;
            }
            // Update inherited mapping's comment (only mergeable metadata).
            if let Some(desc) = &mapping.description {
                if let Some(maps) = sd.body.get_mut("mapping").and_then(|v| v.as_array_mut()) {
                    if let Some(m) = maps.iter_mut().find(|m| {
                        m.get("identity").and_then(|v| v.as_str()) == Some(mapping.id.as_str())
                    }) {
                        if let Some(o) = m.as_object_mut() {
                            o.insert("comment".into(), J::String(desc.clone()));
                        }
                    }
                }
            }
        } else {
            // setMetadata: push a new SD-level mapping entry.
            let mut entry = Map::new();
            entry.insert("identity".into(), J::String(mapping.id.clone()));
            if let Some(t) = &mapping.title {
                entry.insert("name".into(), J::String(t.clone()));
            }
            if let Some(t) = &mapping.target {
                entry.insert("uri".into(), J::String(t.clone()));
            }
            if let Some(d) = &mapping.description {
                entry.insert("comment".into(), J::String(d.clone()));
            }
            let arr = sd
                .body
                .entry("mapping".to_string())
                .or_insert_with(|| J::Array(vec![]));
            if let Some(a) = arr.as_array_mut() {
                a.push(J::Object(entry));
            }
        }

        // setMappingRules
        let mut rules = mapping.rules.clone();
        resolve_soft_indexing(&mut rules);
        // Take the source SD out so we can mutate it while the fisher borrows the
        // rest of `self.exported` (needed for the findMatchingSlice fishForFHIR
        // fallback, which resolves an extension bracket — e.g. a locally-defined
        // extension referenced by name — to the matching inherited slice).
        let mut sd = std::mem::take(&mut self.exported[si].sd);
        let fisher = FisherView {
            store: self.store,
            exported: &self.exported,
            in_progress_meta: &self.in_progress_meta,
            tank_sd_meta: &self.tank_sd_meta,
            predefined_vs: &self.predefined_vs,
        };
        for rule in &rules {
            let Rule::Mapping { path, map, comment, language, .. } = rule else { continue };
            let Some(ei) = find_element_with_ext_fallback(&mut sd, path, &fisher) else {
                self.diag.push(format!(
                    "No element found at path {path} for {}, skipping rule",
                    mapping.name
                ));
                continue;
            };
            let mut entry = Map::new();
            entry.insert("identity".into(), J::String(mapping.id.clone()));
            entry.insert("map".into(), J::String(map.clone()));
            if let Some(c) = comment {
                entry.insert("comment".into(), J::String(c.clone()));
            }
            if let Some(l) = language {
                entry.insert("language".into(), J::String(l.code.clone()));
            }
            let arr = sd.elements[ei]
                .map_mut()
                .entry("mapping".to_string())
                .or_insert_with(|| J::Array(vec![]));
            if let Some(a) = arr.as_array_mut() {
                a.push(J::Object(entry));
            }
        }
        drop(fisher);
        self.exported[si].sd = sd;
    }

    fn export_sd(&mut self, name: &str) {
        if self.already_exported(name) || self.in_progress.contains(name) {
            return;
        }
        let Some(ti) = self.tank_index(name) else {
            return;
        };
        self.in_progress.insert(name.to_string());
        let kind = self.tank[ti].kind;
        let mut def = self.tank[ti].def.clone();
        // Build the SD from its parent.
        let Some(mut sd) = self.get_structure_definition(&def, kind) else {
            self.in_progress.remove(name);
            return;
        };
        // setMetadata
        self.set_metadata(&mut sd, &def, kind);
        // Early-push metadata so circular fishes (during dep export) resolve our url.
        self.in_progress_meta
            .insert(name.to_string(), metadata_from_sd(&sd));
        // preprocess (extension cardinality inference) — pushes inferred rules
        preprocess_structure_definition(&mut def, kind == StructureKind::Extension);
        // pre-export local dependencies referenced by rules so the fisher finds them
        self.export_local_deps(&def);

        // apply rules + context with an immutable fisher view
        {
            let fisher = FisherView {
                store: self.store,
                exported: &self.exported,
                in_progress_meta: &self.in_progress_meta,
                tank_sd_meta: &self.tank_sd_meta,
                predefined_vs: &self.predefined_vs,
            };
            let mut local_diag = Vec::new();
            set_rules(&mut sd, &mut def, kind, &fisher, self.cfg, self.vs_url, self.cs_url, &mut local_diag);
            if kind == StructureKind::Extension {
                set_context(&mut sd, &def, &fisher);
            }
            self.diag.extend(local_diag);
        }
        self.in_progress.remove(name);
        self.in_progress_meta.remove(name);
        self.exported.push(ExportedSd {
            name: name.to_string(),
            sd,
            kind,
        });
    }

    fn export_local_deps(&mut self, def: &StructureDef) {
        let mut deps: Vec<String> = Vec::new();
        for r in &def.rules {
            match r {
                Rule::Only { types, .. } => {
                    for t in types {
                        let base = t.type_.split('|').next().unwrap_or(&t.type_).to_string();
                        deps.push(base);
                    }
                }
                Rule::Contains { items, .. } => {
                    for it in items {
                        if let Some(t) = &it.type_ {
                            deps.push(t.split('|').next().unwrap_or(t).to_string());
                        }
                    }
                }
                _ => {}
            }
        }
        for d in deps {
            if !self.already_exported(&d) && self.tank_index(&d).is_some() {
                self.export_sd(&d);
            }
        }
    }

    /// `getStructureDefinition`: fish parent, build SD, set baseDefinition/url/type.
    fn get_structure_definition(
        &mut self,
        def: &StructureDef,
        kind: StructureKind,
    ) -> Option<StructureDefinition> {
        let parent = def.parent.clone()?;
        // If parent is a local SD, ensure it's exported first.
        if !self.already_exported(&parent) && self.tank_index(&parent).is_some() {
            self.export_sd(&parent);
        }
        let parent_json = {
            let fisher = self.fisher();
            fisher.fish_for_fhir(&parent)?
        };
        let mut sd = StructureDefinition::from_json(&parent_json, true);
        let parent_url = sd.url().to_string();
        let version_parts: Vec<&str> = parent.split('|').skip(1).collect();
        let base_def = if version_parts.is_empty() {
            parent_url.clone()
        } else {
            format!("{}|{}", parent_url, version_parts.join("|"))
        };
        sd.body
            .insert("baseDefinition".into(), J::String(base_def));
        let id = effective_sd_id(def);
        let url = self.url_from_def(def, &id);
        sd.body.insert("url".into(), J::String(url.clone()));
        let type_ = type_from_def_or_parent(def, kind, &sd);
        sd.body.insert("type".into(), J::String(type_));
        // Fix fhirVersion for R4/R4B logicals whose parent is the time-traveling
        // Base (R5-in-R4 bundle). The bundled Base carries fhirVersion "5.0.0";
        // SUSHI overrides it with the configured/default FHIR version.
        if parent_url == "http://hl7.org/fhir/StructureDefinition/Base" {
            if let Some(default_ver) = self.cfg.fhir_version() {
                let cur = sd.body.get("fhirVersion").and_then(|v| v.as_str());
                if cur != Some(default_ver.as_str()) {
                    sd.body
                        .insert("fhirVersion".into(), J::String(default_ver));
                }
            }
        }
        // resetParentElements
        reset_parent_elements(&mut sd, def, kind);
        Some(sd)
    }

    fn url_from_def(&self, def: &StructureDef, id: &str) -> String {
        sd_url_from_def(def, id, &self.cfg.canonical)
    }

    fn set_metadata(&mut self, sd: &mut StructureDefinition, def: &StructureDef, kind: StructureKind) {
        let id = effective_sd_id(def);
        sd.body.insert("id".into(), J::String(id));
        for k in ["meta", "implicitRules", "language", "text", "contained"] {
            sd.body.remove(k);
        }
        remove_matching_extensions(&mut sd.body, UNINHERITED_SD_EXTENSIONS);
        sd.body.remove("identifier");
        sd.body.insert("name".into(), J::String(def.name.clone()));

        let apply_to_root = self.cfg.apply_extension_metadata_to_root;
        if let Some(t) = &def.title {
            if !t.is_empty() {
                sd.body.insert("title".into(), J::String(t.clone()));
                if kind == StructureKind::Extension && apply_to_root {
                    if let Some(root) = sd.elements.first_mut() {
                        root.set("short", J::String(t.clone()));
                    }
                }
            } else {
                sd.body.remove("title");
            }
        } else {
            sd.body.remove("title");
        }
        sd.body.insert("status".into(), J::String(self.cfg.status().into()));
        if self.cfg.fsh_only {
            if let Some(v) = &self.cfg.version {
                sd.body.insert("version".into(), J::String(v.clone()));
            }
        } else {
            sd.body.remove("version");
        }
        for k in ["experimental", "date", "publisher", "contact"] {
            sd.body.remove(k);
        }
        if let Some(d) = &def.description {
            if !d.is_empty() {
                sd.body.insert("description".into(), J::String(d.clone()));
                if kind == StructureKind::Extension && apply_to_root {
                    if let Some(root) = sd.elements.first_mut() {
                        root.set("definition", J::String(d.clone()));
                    }
                }
            } else {
                sd.body.remove("description");
            }
        } else {
            sd.body.remove("description");
        }
        for k in ["useContext", "jurisdiction", "purpose", "copyright", "keyword"] {
            sd.body.remove(k);
        }
        if kind == StructureKind::Logical {
            sd.body.insert("kind".into(), J::String("logical".into()));
            // characteristics → type-characteristics extension (handled if present)
            if !def.characteristics.is_empty() {
                set_characteristics(sd, def);
            }
        }
        sd.body.insert("abstract".into(), J::Bool(false));
        let derivation = match kind {
            StructureKind::Logical | StructureKind::Resource => "specialization",
            _ => "constraint",
        };
        sd.body.insert("derivation".into(), J::String(derivation.into()));

        if kind == StructureKind::Extension {
            let url = sd.url().to_string();
            if let Some(i) = sd.find_element("Extension.url") {
                if sd.elements[i].get("fixedUri").is_none() {
                    sd.elements[i].set("fixedUri", J::String(url));
                }
            }
        } else {
            sd.body.remove("context");
            sd.body.remove("contextInvariant");
        }
        // remove top-level _ props
        let underscores: Vec<String> = sd
            .body
            .keys()
            .filter(|k| k.starts_with('_'))
            .cloned()
            .collect();
        for k in underscores {
            sd.body.remove(&k);
        }
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

/// `getUrlFromFshDefinition`: a `* ^url = ...` caret wins, else the default
/// `{canonical}/StructureDefinition/{id}`.
fn sd_url_from_def(def: &StructureDef, id: &str, canonical: &str) -> String {
    for r in def.rules.iter().rev() {
        if let Rule::CaretValue {
            path,
            caret_path,
            value: Some(FshValue::Str(s)),
            is_instance: false,
            ..
        } = r
        {
            if path.is_empty() && caret_path.as_deref() == Some("url") {
                return s.clone();
            }
        }
    }
    format!("{canonical}/StructureDefinition/{id}")
}

fn effective_sd_id(def: &StructureDef) -> String {
    for r in def.rules.iter().rev() {
        if let Rule::CaretValue {
            path,
            caret_path,
            value: Some(FshValue::Str(s)),
            is_instance: false,
            ..
        } = r
        {
            if path.is_empty() && caret_path.as_deref() == Some("id") {
                return s.clone();
            }
        }
    }
    def.id.clone()
}

fn type_from_def_or_parent(def: &StructureDef, kind: StructureKind, parent: &StructureDefinition) -> String {
    if kind == StructureKind::Profile || kind == StructureKind::Extension {
        return parent.type_().to_string();
    }
    // last ^type caret rule
    let mut found: Option<String> = None;
    for r in &def.rules {
        if let Rule::CaretValue {
            path,
            caret_path,
            value: Some(v),
            ..
        } = r
        {
            if path.is_empty() && caret_path.as_deref() == Some("type") {
                if let FshValue::Str(s) = v {
                    found = Some(s.clone());
                }
            }
        }
    }
    if let Some(t) = found {
        return t;
    }
    if kind == StructureKind::Logical {
        parent.url().to_string()
    } else {
        def.id.clone()
    }
}

fn remove_matching_extensions(body: &mut Map<String, J>, urls: &[&str]) {
    if let Some(J::Array(exts)) = body.get_mut("extension") {
        exts.retain(|e| {
            let u = e.get("url").and_then(|v| v.as_str()).unwrap_or("");
            !urls.contains(&u)
        });
        if exts.is_empty() {
            body.remove("extension");
        }
    }
}

fn remove_uninherited_ed_extensions(ed: &mut ElementDefinition) {
    // Skip forking the COW map when there is nothing to strip.
    let has_uninherited = ed
        .map
        .get("extension")
        .and_then(|v| v.as_array())
        .map(|exts| {
            exts.iter().any(|e| {
                let u = e.get("url").and_then(|v| v.as_str()).unwrap_or("");
                UNINHERITED_ED_EXTENSIONS.contains(&u)
            })
        })
        .unwrap_or(false);
    if !has_uninherited {
        return;
    }
    let m = ed.map_mut();
    let mut became_empty = false;
    if let Some(J::Array(exts)) = m.get_mut("extension") {
        exts.retain(|e| {
            let u = e.get("url").and_then(|v| v.as_str()).unwrap_or("");
            !UNINHERITED_ED_EXTENSIONS.contains(&u)
        });
        became_empty = exts.is_empty();
    }
    if became_empty {
        m.remove("extension");
    }
}

/// `resetParentElements`.
fn reset_parent_elements(sd: &mut StructureDefinition, def: &StructureDef, kind: StructureKind) {
    for e in &mut sd.elements {
        remove_uninherited_ed_extensions(e);
    }
    sd.capture_original_elements();
    if kind == StructureKind::Profile || kind == StructureKind::Extension {
        return;
    }
    // logical / resource: re-base element ids to the new pathType
    let pt = sd.path_type();
    for e in &mut sd.elements {
        let id = e.id().to_string();
        let new_id = if let Some(pos) = id.find('.') {
            format!("{}{}", pt, &id[pos..])
        } else {
            pt.clone()
        };
        e.set_id(new_id);
    }
    // ids were renamed in place (count unchanged) — drop the stale id->index cache.
    sd.invalidate_id_index();
    // root base.path = root path
    if let Some(root) = sd.elements.first_mut() {
        let rp = root.path().to_string();
        if root.map.get("base").is_some() {
            if let Some(base) = root.map_mut().get_mut("base") {
                if let Some(bo) = base.as_object_mut() {
                    bo.insert("path".into(), J::String(rp));
                }
            }
        }
    }
    let parent = def.parent.as_deref().unwrap_or("");
    if parent == "Element" || parent == "http://hl7.org/fhir/StructureDefinition/Element" {
        if let Some(root) = sd.elements.first_mut() {
            root.map_mut().remove("extension");
        }
    }
    sd.capture_original_elements();
    // root short/definition
    let short = def.title.clone().unwrap_or_else(|| def.name.clone());
    let definition = def.description.clone().unwrap_or_else(|| short.clone());
    if let Some(root) = sd.elements.first_mut() {
        root.set("short", J::String(short));
        root.set("definition", J::String(definition));
    }
}

fn set_characteristics(sd: &mut StructureDefinition, def: &StructureDef) {
    // Add structuredefinition-type-characteristics extension entries.
    let mut exts: Vec<J> = sd
        .body
        .get("extension")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for c in &def.characteristics {
        exts.push(json!({
            "url": "http://hl7.org/fhir/StructureDefinition/structuredefinition-type-characteristics",
            "valueCode": c
        }));
    }
    sd.body.insert("extension".into(), J::Array(exts));
}

/// `preprocessStructureDefinition` — infer `extension`/`value[x]` 0..0.
fn preprocess_structure_definition(def: &mut StructureDef, is_extension: bool) {
    use crate::paths::split_on_path_periods;
    // key -> should-set (true) / contradiction (false)
    let mut inferred: Vec<(String, bool)> = Vec::new();
    let set = |inferred: &mut Vec<(String, bool)>, key: &str, val: bool| {
        if let Some(e) = inferred.iter_mut().find(|(k, _)| k == key) {
            e.1 = val;
        } else {
            inferred.push((key.to_string(), val));
        }
    };
    let get = |inferred: &[(String, bool)], key: &str| -> Option<bool> {
        inferred.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
    };
    for rule in &def.rules {
        let parts = split_on_path_periods(rule.path());
        for (i, part) in parts.iter().enumerate() {
            let prev = if i > 0 { Some(parts[i - 1].as_str()) } else { None };
            let is_on_extension = (is_extension && parts.len() == 1)
                || prev.map(|p| p.starts_with("extension")).unwrap_or(false);
            if !is_on_extension {
                continue;
            }
            let initial = parts[..i].join(".");
            let base_path = if initial.is_empty() {
                String::new()
            } else {
                format!("{initial}.")
            };
            let is_max0 = matches!(rule, Rule::Card { max, .. } if max == "0");
            if part.starts_with("extension") {
                let contra = format!("{base_path}extension");
                if !is_max0 {
                    if get(&inferred, &contra).is_some() {
                        set(&mut inferred, &contra, false);
                    } else {
                        set(&mut inferred, &format!("{base_path}value[x]"), true);
                    }
                } else if get(&inferred, &contra).is_some() {
                    set(&mut inferred, &contra, false);
                }
            } else if part.starts_with("value") {
                let contra = format!("{base_path}value[x]");
                if !is_max0 {
                    if get(&inferred, &contra).is_some() {
                        set(&mut inferred, &contra, false);
                    } else {
                        set(&mut inferred, &format!("{base_path}extension"), true);
                    }
                } else if get(&inferred, &contra).is_some() {
                    set(&mut inferred, &contra, false);
                }
            }
        }
    }
    for (key, should) in inferred {
        if should {
            def.rules.push(Rule::Card {
                source_info: Default::default(),
                path: key,
                min: Some(0),
                max: "0".to_string(),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// setRules
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
/// Resolve a rule path, with the `findMatchingSlice` fishForFHIR fallback for
/// extension brackets that name an extension by alias/url/id rather than by its
/// inherited `sliceName` (e.g. `extension[us-core-birthsex]` -> `birthsex`).
/// Rewrites such brackets to the matching inherited slice's sliceName and
/// retries. (Stock does this inside findElementByPath; our generic
/// find_element_by_path leaves it to callers to avoid instance-path churn.)
fn find_element_with_ext_fallback(
    sd: &mut StructureDefinition,
    path: &str,
    fisher: &dyn Fisher,
) -> Option<usize> {
    if let Some(ei) = sd.find_element_by_path(path, fisher) {
        return Some(ei);
    }
    let mut parts = crate::paths::parse_fsh_path(path);
    let mut changed = false;
    for i in 0..parts.len() {
        if parts[i].base != "extension" && parts[i].base != "modifierExtension" {
            continue;
        }
        if parts[i].brackets.len() != 1 {
            continue;
        }
        let b0 = parts[i].brackets[0].clone();
        if b0.is_empty() || b0.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let cum = crate::paths::assemble_fsh_path(&parts[..=i]);
        if sd.find_element_by_path(&cum, fisher).is_some() {
            continue;
        }
        let mut parent_parts = parts[..=i].to_vec();
        parent_parts[i].brackets.clear();
        let parent_path = crate::paths::assemble_fsh_path(&parent_parts);
        if sd.find_element_by_path(&parent_path, fisher).is_none() {
            continue;
        }
        if let Some(url) = fisher.fish_for_metadata(&b0).and_then(|m| m.url) {
            if let Some(sn) = sd.find_slice_by_profile_url(&url) {
                parts[i].brackets = vec![sn];
                changed = true;
            }
        }
    }
    if changed {
        let new_path = crate::paths::assemble_fsh_path(&parts);
        return sd.find_element_by_path(&new_path, fisher);
    }
    None
}

fn set_rules(
    sd: &mut StructureDefinition,
    def: &mut StructureDef,
    kind: StructureKind,
    fisher: &dyn Fisher,
    cfg: &Config,
    vs_url: &dyn Fn(&str) -> Option<String>,
    cs_url: &dyn Fn(&str) -> Option<String>,
    diag: &mut Vec<String>,
) {
    resolve_soft_indexing(&mut def.rules);
    let rules = def.rules.clone();
    // One SD-driven type resolver for all caret rules of this SD (cache reused
    // across segments/rules). Fishes StructureDefinition/ElementDefinition + every
    // datatype/extension SD on demand instead of a hardcoded element-type table.
    let fish = |name: &str| fisher.fish_for_fhir(name);
    let resolver = crate::type_resolver::TypeResolver::new(&fish);
    let mut i = 0;
    while i < rules.len() {
        let rule = &rules[i];
        i += 1;
        // AddElementRule handled separately (logicals/resources)
        if let Rule::AddElement { .. } = rule {
            apply_add_element(sd, rule, fisher, diag);
            continue;
        }
        let path = rule.path();
        let Some(ei) = find_element_with_ext_fallback(sd, path, fisher) else {
            diag.push(format!(
                "No element found at path {path} for {} in {}, skipping rule",
                rule.constructor_name(),
                def.name
            ));
            continue;
        };
        match rule {
            Rule::Card { min, max, .. } => {
                constrain_cardinality_sd(sd, ei, *min, max);
            }
            Rule::Assignment {
                value: Some(value),
                exactly,
                is_instance: false,
                ..
            } => {
                // `replaceReferences` resolves a FshCode's bare CodeSystem-name
                // system to its canonical url before the value is assigned
                // (pattern/fixed). Applies to local and dependency-package
                // CodeSystems alike.
                let resolved = resolve_code_system_in_value(value, fisher, cs_url);
                assign_value(sd, ei, resolved.as_ref().unwrap_or(value), *exactly, fisher);
            }
            Rule::Flag { flags, .. } => {
                apply_flags(&mut sd.elements[ei], flags, sd_derivation_specialization(kind), diag);
            }
            Rule::Only { types, .. } => {
                let target = get_reference_or_canonical_name(path);
                constrain_type(sd, ei, types, target.as_deref(), fisher, diag);
            }
            Rule::Binding { value_set, strength, .. } => {
                let base = value_set.split('|').next().unwrap_or(value_set);
                let vs_meta_url = (vs_url)(value_set).or_else(|| {
                    let m = fisher.fish_for_metadata_vs(base)?;
                    // For a URL-valued binding, only accept an exact-url match
                    // (SUSHI replaces only the name part with the fished url).
                    match &m.url {
                        Some(u) if base.contains("://") && u != base => None,
                        other => other.clone(),
                    }
                });
                let vs_uri = match &vs_meta_url {
                    Some(u) => {
                        let ver: String = value_set.split('|').skip(1).collect::<Vec<_>>().join("|");
                        if ver.is_empty() {
                            u.clone()
                        } else {
                            format!("{u}|{ver}")
                        }
                    }
                    None => value_set.clone(),
                };
                let _cs = (cs_url)(value_set);
                bind_to_vs(&mut sd.elements[ei], &vs_uri, strength);
            }
            Rule::Obeys { invariant, .. } => {
                // handled by full obeys port below
                let url = sd.url().to_string();
                apply_obeys(sd, ei, invariant, &url, def, diag, &resolver);
            }
            Rule::CaretValue {
                path: rpath,
                caret_path: Some(cp),
                value: Some(value),
                is_instance,
                ..
            } => {
                // Resolve `Canonical(localName)` against the fisher (SD) then
                // local ValueSet/CodeSystem urls, mirroring replaceReferences.
                let value = &resolve_canonical_caret(value, fisher, vs_url, cs_url);
                if rpath.is_empty() {
                    // SD-body instance carets (e.g. ^contained) are deferred — skip.
                    if !is_instance {
                        apply_caret_fhir(&mut sd.body, "StructureDefinition", cp, value, cfg, &resolver);
                    }
                } else {
                    // Element carets: apply literal value (bare-name `valueId`/`valueCode`
                    // assignments resolve to the name string).
                    apply_caret_element(&mut sd.elements[ei], cp, value, cfg, &resolver);
                }
            }
            Rule::Contains { items, .. } => {
                handle_contains(sd, ei, items, kind, fisher, diag);
            }
            _ => {}
        }
    }
    // ensure SD body url stays after any ^url caret (caret already set it)
    let _ = cfg;
}

fn sd_derivation_specialization(kind: StructureKind) -> bool {
    matches!(kind, StructureKind::Logical | StructureKind::Resource)
}

// ---------------------------------------------------------------------------
// Element mutation: cardinality / flags / binding / constraint
// ---------------------------------------------------------------------------

/// constrainCardinality with slice→parent min propagation.
fn constrain_cardinality_sd(sd: &mut StructureDefinition, ei: usize, min: Option<i64>, max: &str) {
    constrain_cardinality(&mut sd.elements[ei], min, max);
    // If this is a slice, bump the sliced element's min to the sum of slice mins.
    if sd.elements[ei].slice_name().is_none() {
        return;
    }
    let id = sd.elements[ei].id().to_string();
    let path = sd.elements[ei].path().to_string();
    // slicedElement id (strip last slice marker)
    let seg_start = id.rfind('.').map(|i| i + 1).unwrap_or(0);
    let seg = &id[seg_start..];
    let Some(cut) = seg.rfind([':', '/']) else { return };
    let sliced_id = format!("{}{}", &id[..seg_start], &seg[..cut]);
    let Some(si) = sd.index_of_id(&sliced_id) else { return };
    // sum mins of all sibling slices (same path, slices of the same sliced element)
    let mut sum = 0i64;
    for e in &sd.elements {
        if e.path() == path && e.slice_name().is_some() {
            let eid = e.id();
            let es = eid.rfind('.').map(|i| i + 1).unwrap_or(0);
            let eseg = &eid[es..];
            if let Some(ecut) = eseg.rfind([':', '/']) {
                let esliced = format!("{}{}", &eid[..es], &eseg[..ecut]);
                if esliced == sliced_id {
                    sum += e.get("min").and_then(|v| v.as_i64()).unwrap_or(0);
                }
            }
        }
    }
    let parent_min = sd.elements[si].get("min").and_then(|v| v.as_i64()).unwrap_or(0);
    if sum > parent_min {
        constrain_cardinality(&mut sd.elements[si], Some(sum), "");
    }
}

fn constrain_cardinality(ed: &mut ElementDefinition, min: Option<i64>, max: &str) {
    let cur_min = ed.get("min").and_then(|v| v.as_i64());
    let cur_max = ed.get("max").and_then(|v| v.as_str()).map(String::from);
    let new_min = min.or(cur_min);
    let new_max = if max.is_empty() {
        cur_max
    } else {
        Some(max.to_string())
    };
    if let Some(m) = new_min {
        ed.set("min", J::Number(m.into()));
    }
    if let Some(m) = new_max {
        ed.set("max", J::String(m));
    }
}

fn apply_flags(ed: &mut ElementDefinition, flags: &fsh_model::Flags, specialization: bool, diag: &mut Vec<String>) {
    if flags.must_support {
        if specialization {
            diag.push("must support on specialization".to_string());
        } else {
            ed.set("mustSupport", J::Bool(true));
        }
    }
    if flags.summary {
        ed.set("isSummary", J::Bool(true));
    }
    if flags.modifier {
        ed.set("isModifier", J::Bool(true));
    }
    let status = if flags.trial_use {
        Some("trial-use")
    } else if flags.normative {
        Some("normative")
    } else if flags.draft {
        Some("draft")
    } else {
        None
    };
    if let Some(s) = status {
        let new_ext = json!({
            "url": "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status",
            "valueCode": s
        });
        let mut exts: Vec<J> = ed.get("extension").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        if let Some(pos) = exts.iter().position(|e| {
            e.get("url").and_then(|v| v.as_str())
                == Some("http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status")
        }) {
            exts[pos] = new_ext;
        } else {
            exts.push(new_ext);
        }
        ed.set("extension", J::Array(exts));
    }
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// `assignValue` (common scalar/code/reference cases).
fn assign_value(
    sd: &mut StructureDefinition,
    ei: usize,
    value: &FshValue,
    exactly: bool,
    fisher: &dyn Fisher,
) {
    assign_value_inner(&mut sd.elements[ei], value, exactly, fisher);
    enforce_discriminator_min(sd, ei);
}

/// After assignment, if this element is a value/pattern discriminator path of a
/// parent slice and min==0, force min to 1 (`ElementDefinition.ts:2076-2095`).
fn enforce_discriminator_min(sd: &mut StructureDefinition, ei: usize) {
    let this_path = sd.elements[ei].path().to_string();
    let this_id = sd.elements[ei].id().to_string();
    // chain: this + ancestors (by stripping trailing `.seg`)
    let mut chain = vec![this_id.clone()];
    let mut cur = this_id.clone();
    while let Some(pos) = cur.rfind('.') {
        cur = cur[..pos].to_string();
        chain.push(cur.clone());
    }
    let mut should = false;
    for pid in &chain {
        let Some(pi) = sd.index_of_id(pid) else { continue };
        if sd.elements[pi].slice_name().is_none() {
            continue;
        }
        // slicedElement id = strip trailing slice from pid
        let seg_start = pid.rfind('.').map(|i| i + 1).unwrap_or(0);
        let seg = &pid[seg_start..];
        let Some(cutpos) = seg.rfind([':', '/']) else { continue };
        let sliced_id = format!("{}{}", &pid[..seg_start], &seg[..cutpos]);
        let Some(si) = sd.index_of_id(&sliced_id) else { continue };
        let sliced_path = sd.elements[si].path().to_string();
        let discs = sd.elements[si]
            .get("slicing")
            .and_then(|s| s.get("discriminator"))
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();
        for d in &discs {
            let dpath = d.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let dtype = d.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if dpath != "$this"
                && format!("{sliced_path}.{dpath}") == this_path
                && (dtype == "value" || dtype == "pattern")
            {
                should = true;
            }
        }
    }
    if should {
        let cur_min = sd.elements[ei].get("min").and_then(|v| v.as_i64()).unwrap_or(0);
        if cur_min == 0 {
            constrain_cardinality(&mut sd.elements[ei], Some(1), "");
        }
    }
}

/// `ElementDefinition.isQuantityType` — Quantity or any FHIR type derived from
/// it (baseDefinition == .../Quantity).
fn is_quantity_type(t: &str) -> bool {
    matches!(
        t,
        "Quantity" | "Age" | "Count" | "Distance" | "Duration" | "MoneyQuantity" | "SimpleQuantity"
    )
}

fn assign_value_inner(ed: &mut ElementDefinition, value: &FshValue, exactly: bool, fisher: &dyn Fisher) {
    let types = ed.type_codes();
    if types.len() != 1 {
        return;
    }
    let etype = types[0].clone();
    // (fhir_type_name, json_value)
    let assigned: Option<(String, J)> = match value {
        FshValue::Bool(b) => Some(("boolean".to_string(), J::Bool(*b))),
        FshValue::BigInt(s) => {
            let n: i64 = s.parse().unwrap_or(0);
            Some((etype.clone(), J::Number(n.into())))
        }
        FshValue::Float(f) => serde_json::Number::from_f64(*f).map(|n| (etype.clone(), J::Number(n))),
        FshValue::Str(s) => Some((etype.clone(), J::String(s.clone()))),
        FshValue::Code(fc) => match etype.as_str() {
            "code" | "string" | "uri" => Some((etype.clone(), J::String(fc.code.clone()))),
            "CodeableConcept" => {
                let mut m = Map::new();
                m.insert("coding".into(), J::Array(vec![crate::export::coding_from(fc)]));
                Some(("CodeableConcept".to_string(), J::Object(m)))
            }
            "Coding" => Some(("Coding".to_string(), crate::export::coding_from(fc))),
            // A FshCode assigned to a Quantity-typed element maps to the code +
            // system parts of the Quantity (`FshCode.toFHIRQuantity`).
            t if is_quantity_type(t) => {
                let mut m = Map::new();
                m.insert("code".into(), J::String(fc.code.clone()));
                if let Some(sys) = &fc.system {
                    m.insert("system".into(), J::String(sys.clone()));
                }
                if let Some(d) = &fc.display {
                    m.insert("unit".into(), J::String(d.clone()));
                }
                Some((etype.clone(), J::Object(m)))
            }
            _ => None,
        },
        FshValue::Reference(r) => {
            // resolve referenced entity to a url/id if local
            let mut m = Map::new();
            let resolved = resolve_reference(&r.reference, fisher);
            m.insert("reference".into(), J::String(resolved));
            if let Some(d) = &r.display {
                m.insert("display".into(), J::String(d.clone()));
            }
            let code = if etype == "CodeableReference" {
                let mut cr = Map::new();
                cr.insert("reference".into(), J::Object(m));
                return set_pattern(ed, "CodeableReference", J::Object(cr), exactly);
            } else {
                "Reference".to_string()
            };
            Some((code, J::Object(m)))
        }
        FshValue::Canonical(c) => {
            let url = fisher
                .fish_for_metadata(&c.entity_name)
                .and_then(|m| m.url)
                .unwrap_or_else(|| c.entity_name.clone());
            let url = match &c.version {
                Some(v) => format!("{url}|{v}"),
                None => url,
            };
            Some(("canonical".to_string(), J::String(url)))
        }
        FshValue::Quantity(q) => {
            // Mirror `FshQuantity.toFHIRQuantity` for key order: value, code, system, unit.
            // Like TS, support compatible Quantity specializations (Age, Distance, ...) by
            // keeping the element's actual type as the assigned type when it derives from
            // Quantity; otherwise fall back to plain "Quantity".
            let provided_type = if etype != "Quantity" && is_quantity_type(&etype) {
                etype.clone()
            } else {
                "Quantity".to_string()
            };
            Some((provided_type, J::Object(quantity_to_json(q))))
        }
        FshValue::Ratio(r) => {
            // `FshRatio.toFHIRRatio`: numerator then denominator, each a Quantity.
            let mut m = Map::new();
            m.insert("numerator".into(), J::Object(quantity_to_json(&r.numerator)));
            m.insert("denominator".into(), J::Object(quantity_to_json(&r.denominator)));
            Some(("Ratio".to_string(), J::Object(m)))
        }
        _ => None,
    };
    if let Some((type_name, jv)) = assigned {
        set_pattern(ed, &type_name, jv, exactly);
    }
}

/// Build a FHIR Quantity JSON object from an `FshQuantity`, mirroring
/// `FshQuantity.toFHIRQuantity` exactly (key order: value, code, system, unit).
/// Each field is only emitted when truthy (non-empty), matching the TS guards.
fn quantity_to_json(q: &fsh_model::FshQuantity) -> Map<String, J> {
    let mut m = Map::new();
    if let Some(v) = q.value {
        // FshQuantity.value is a plain JS number, so it serializes the JS way:
        // whole numbers drop the trailing ".0" (155.0 -> 155, but 1.5 -> 1.5).
        let jv = if v.fract() == 0.0 && v.abs() < 1e15 {
            J::Number((v as i64).into())
        } else if let Some(n) = serde_json::Number::from_f64(v) {
            J::Number(n)
        } else {
            J::Null
        };
        if !jv.is_null() {
            m.insert("value".into(), jv);
        }
    }
    if let Some(u) = &q.unit {
        if !u.code.is_empty() {
            m.insert("code".into(), J::String(u.code.clone()));
        }
        if let Some(sys) = &u.system {
            if !sys.is_empty() {
                m.insert("system".into(), J::String(sys.clone()));
            }
        }
        if let Some(d) = &u.display {
            if !d.is_empty() {
                m.insert("unit".into(), J::String(d.clone()));
            }
        }
    }
    m
}

fn resolve_reference(reference: &str, _fisher: &dyn Fisher) -> String {
    reference.to_string()
}

fn set_pattern(ed: &mut ElementDefinition, type_name: &str, value: J, exactly: bool) {
    let key = if exactly {
        format!("fixed{}", upper_first(type_name))
    } else {
        format!("pattern{}", upper_first(type_name))
    };
    ed.set(&key, value);
}

fn bind_to_vs(ed: &mut ElementDefinition, vs_uri: &str, strength: &str) {
    // bindToVS REPLACES the binding entirely (`ElementDefinition.ts:1083`).
    let mut binding = Map::new();
    binding.insert("strength".into(), J::String(strength.to_string()));
    binding.insert("valueSet".into(), J::String(vs_uri.to_string()));
    ed.set("binding", J::Object(binding));
}

fn apply_obeys(
    sd: &mut StructureDefinition,
    ei: usize,
    invariant_name: &str,
    sd_url: &str,
    def: &StructureDef,
    diag: &mut Vec<String>,
    resolver: &crate::type_resolver::TypeResolver,
) {
    // find invariant in tank-collected list (passed via def? we need access)
    // We look up via the global invariant registry stored on the context's tank.
    // For simplicity, search def's own rules is wrong; invariants are separate.
    // This is wired through INVARIANTS thread-local set before export.
    let inv = INVARIANTS.with(|m| m.borrow().get(invariant_name).cloned());
    let Some(inv) = inv else {
        diag.push(format!(
            "Cannot apply {invariant_name} constraint on {} because it was never defined.",
            sd.get_str("id").unwrap_or("")
        ));
        return;
    };
    // applyConstraint: build from keyword fields (insertion order).
    let mut constraint = Map::new();
    if !inv.name.is_empty() {
        constraint.insert("key".into(), J::String(inv.name.clone()));
    }
    if let Some(sev) = &inv.severity {
        constraint.insert("severity".into(), J::String(sev.code.clone()));
    }
    if let Some(d) = &inv.description {
        constraint.insert("human".into(), J::String(d.clone()));
    }
    if let Some(e) = &inv.expression {
        constraint.insert("expression".into(), J::String(e.clone()));
    }
    if let Some(x) = &inv.xpath {
        constraint.insert("xpath".into(), J::String(x.clone()));
    }
    constraint.insert("source".into(), J::String(sd_url.to_string()));
    let ed = &mut sd.elements[ei];
    let mut arr: Vec<J> = ed
        .get("constraint")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let cst_idx = arr.len();
    arr.push(J::Object(constraint));
    ed.set("constraint", J::Array(arr));

    // Invariant's own AssignmentRules become `constraint[idx].{path}` carets.
    for r in &inv.rules {
        if let Rule::Assignment {
            path,
            value: Some(value),
            is_instance: false,
            ..
        } = r
        {
            let caret_path = format!("constraint[{cst_idx}].{path}");
            apply_caret_element(&mut sd.elements[ei], &caret_path, value, &Config::default(), resolver);
        }
    }
    let _ = def;
}

thread_local! {
    static INVARIANTS: std::cell::RefCell<std::collections::HashMap<String, fsh_model::Invariant>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    static ALIASES: std::cell::RefCell<std::collections::HashMap<String, String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// `tank.resolveAlias(item)` — the MasterFisher resolves an alias token to its
/// target before fishing. Returns the input unchanged when it isn't an alias.
pub(crate) fn resolve_alias_tl(name: &str) -> String {
    ALIASES.with(|m| m.borrow().get(name).cloned().unwrap_or_else(|| name.to_string()))
}

pub(crate) fn set_aliases(docs: &[FshDocument]) {
    ALIASES.with(|m| {
        let mut map = m.borrow_mut();
        map.clear();
        for doc in docs {
            for (k, v) in &doc.aliases {
                map.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
    });
}

/// Resolve alias bracket tokens in a caret path (`extension[$fmm]` -> url).
pub(crate) fn resolve_caret_aliases(caret_path: &str) -> String {
    if !caret_path.contains('[') {
        return caret_path.to_string();
    }
    ALIASES.with(|m| {
        let map = m.borrow();
        if map.is_empty() {
            return caret_path.to_string();
        }
        let mut out = String::with_capacity(caret_path.len());
        let mut chars = caret_path.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c == '[' {
                // capture until matching ']'
                let rest = &caret_path[i + 1..];
                if let Some(end) = rest.find(']') {
                    let inner = &rest[..end];
                    let replaced = map.get(inner).cloned().unwrap_or_else(|| inner.to_string());
                    out.push('[');
                    out.push_str(&replaced);
                    out.push(']');
                    // advance iterator past the bracket
                    for _ in 0..(end + 1) {
                        chars.next();
                    }
                    continue;
                }
            }
            out.push(c);
        }
        out
    })
}

pub fn set_invariants(docs: &[FshDocument]) {
    INVARIANTS.with(|m| {
        let mut map = m.borrow_mut();
        map.clear();
        for doc in docs {
            for (_k, inv) in &doc.invariants {
                map.entry(inv.name.clone()).or_insert_with(|| inv.clone());
            }
        }
    });
}

// ---------------------------------------------------------------------------
// OnlyRule: constrainType (common cases)
// ---------------------------------------------------------------------------

fn get_reference_or_canonical_name(_path: &str) -> Option<String> {
    None
}

fn get_type_lineage(type_name: &str, fisher: &dyn Fisher) -> Vec<Metadata> {
    let mut results = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut current = Some(type_name.to_string());
    while let Some(ct) = current {
        if seen.contains(&ct) {
            break;
        }
        let md = fisher
            .fish_for_metadata(&ct)
            .or_else(|| fisher.fish_for_metadata(ct.split('|').next().unwrap_or(&ct)));
        let Some(md) = md else { break };
        if let Some(u) = &md.url {
            if seen.contains(u) {
                break;
            }
            seen.push(u.clone());
        }
        let parent = md.parent.clone();
        results.push(md);
        current = parent;
    }
    results
}

struct Match {
    metadata: Metadata,
    /// the matched element-type code (e.g. "Reference", "canonical", "Observation")
    code: String,
}

fn is_reference_type(code: &str) -> bool {
    code == "Reference" || code == "CodeableReference"
}

fn target_profile_ok(et: &J, md_url: &str, md: &Metadata) -> bool {
    match et.get("targetProfile").and_then(|v| v.as_array()) {
        None => true,
        Some(tps) => {
            let versioned = format!("{md_url}|{}", md.version.clone().unwrap_or_default());
            tps.iter()
                .any(|t| t.as_str() == Some(md_url) || t.as_str() == Some(versioned.as_str()))
        }
    }
}

fn constrain_type(
    sd: &mut StructureDefinition,
    ei: usize,
    types: &[OnlyRuleType],
    _target: Option<&str>,
    fisher: &dyn Fisher,
    diag: &mut Vec<String>,
) {
    let kind = sd.kind().to_string();
    let element_types: Vec<J> = sd.elements[ei]
        .get("type")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if element_types.is_empty() {
        return;
    }
    // matches grouped by the element-type code they matched against
    let mut matches_by_code: std::collections::HashMap<String, Vec<Match>> =
        std::collections::HashMap::new();
    for et in &element_types {
        matches_by_code.entry(type_code(et).to_string()).or_default();
    }
    for t in types {
        let lineage = get_type_lineage(&t.type_, fisher);
        if lineage.is_empty() {
            diag.push(format!("Type not found: {}", t.type_));
            return;
        }
        let mut matched: Option<String> = None;
        'outer: for md in &lineage {
            let md_url = md.url.clone().unwrap_or_default();
            if t.is_reference || t.is_codeable_reference {
                // Prefer Reference (only when the keyword is Reference), then CodeableReference.
                if t.is_reference {
                    if element_types.iter().any(|et| {
                        type_code(et) == "Reference" && target_profile_ok(et, &md_url, md)
                    }) {
                        matched = Some("Reference".to_string());
                        break 'outer;
                    }
                }
                if element_types.iter().any(|et| {
                    type_code(et) == "CodeableReference" && target_profile_ok(et, &md_url, md)
                }) {
                    matched = Some("CodeableReference".to_string());
                    break 'outer;
                }
            } else if t.is_canonical {
                for et in &element_types {
                    if type_code(et) == "canonical" && target_profile_ok(et, &md_url, md) {
                        matched = Some("canonical".to_string());
                        break 'outer;
                    }
                }
            } else {
                for et in &element_types {
                    let c = type_code(et);
                    let profiles: Vec<String> = et
                        .get("profile")
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                    let matches_unprofiled = c == md.id && profiles.is_empty();
                    let anc = get_type_lineage(md.sd_type.as_deref().unwrap_or(""), fisher);
                    let versioned = format!("{md_url}|{}", md.version.clone().unwrap_or_default());
                    let matches_profile = anc.iter().any(|a| a.sd_type.as_deref() == Some(c))
                        && (profiles.contains(&md_url) || profiles.contains(&versioned));
                    let matches_logical =
                        kind == "logical" && !c.is_empty() && Some(c) == md.sd_type.as_deref();
                    if matches_unprofiled || matches_profile || matches_logical {
                        matched = Some(c.to_string());
                        break 'outer;
                    }
                }
            }
        }
        let Some(code) = matched else {
            diag.push(format!("Invalid type {} for element", t.type_));
            return;
        };
        let mut meta = lineage[0].clone();
        if (t.is_canonical || t.is_reference || t.is_codeable_reference) && t.type_.contains('|') {
            let ver = t.type_.split('|').nth(1).unwrap_or("");
            meta.url = Some(format!("{}|{ver}", meta.url.clone().unwrap_or_default()));
        }
        matches_by_code
            .entry(code.clone())
            .or_default()
            .push(Match { metadata: meta, code });
    }

    // build new types
    let mut new_types: Vec<J> = Vec::new();
    for et in &element_types {
        let c = type_code(et);
        let Some(matches) = matches_by_code.get(c) else {
            new_types.push(et.clone());
            continue;
        };
        if matches.is_empty() {
            continue; // filtered out (target specified, not matched)
        }
        // group matches by grouping code (applyTypeIntersection)
        let mut groups: Vec<(String, Vec<&Match>)> = Vec::new();
        for m in matches {
            let group_code = if is_reference_type(&m.code) || m.code == "canonical" {
                m.code.clone()
            } else {
                m.metadata.sd_type.clone().unwrap_or_else(|| m.code.clone())
            };
            if let Some(g) = groups.iter_mut().find(|(k, _)| *k == group_code) {
                g.1.push(m);
            } else {
                groups.push((group_code, vec![m]));
            }
        }
        for (group_code, gmatches) in groups {
            let mut new_type = et.clone();
            // `getActualCode()` is the raw `code` field (NOT the fhir-type-ext
            // resolved code); FHIRPath System primitives keep their raw code.
            let actual = et.get("code").and_then(|v| v.as_str()).unwrap_or("");
            let fhirpath_primitive = actual.starts_with("http://hl7.org/fhirpath/System.");
            if !fhirpath_primitive {
                if let Some(o) = new_type.as_object_mut() {
                    o.insert("code".into(), J::String(group_code.clone()));
                }
            }
            apply_profiles(&mut new_type, &gmatches, &kind);
            new_types.push(new_type);
        }
    }
    // Rebuild _profile / _targetProfile underscore siblings against the originals.
    for new_type in new_types.iter_mut() {
        let code = type_code(new_type).to_string();
        let original = element_types.iter().find(|t| type_code(t) == code).cloned();
        for key in ["profile", "targetProfile"] {
            let ukey = format!("_{key}");
            if let Some(o) = new_type.as_object_mut() {
                o.remove(&ukey);
            }
            let orig_u = original
                .as_ref()
                .and_then(|o| o.get(&ukey))
                .and_then(|v| v.as_array())
                .cloned();
            let orig_p = original
                .as_ref()
                .and_then(|o| o.get(key))
                .and_then(|v| v.as_array())
                .cloned();
            if let (Some(orig_u), Some(orig_p)) = (orig_u, orig_p) {
                if orig_u.is_empty() {
                    continue;
                }
                let new_p: Vec<String> = new_type
                    .get(key)
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                let rebuilt: Vec<J> = new_p
                    .iter()
                    .map(|np| {
                        match orig_p.iter().position(|op| op.as_str() == Some(np.as_str())) {
                            Some(idx) => orig_u.get(idx).cloned().unwrap_or(J::Null),
                            None => J::Null,
                        }
                    })
                    .collect();
                if !rebuilt.is_empty() && !rebuilt.iter().all(|e| e.is_null()) {
                    if let Some(o) = new_type.as_object_mut() {
                        o.insert(ukey, J::Array(rebuilt));
                    }
                }
            }
        }
    }
    // Propagate the type constraint to connected slice elements
    // (`findConnectedElements`): each slice's type becomes the intersection of
    // the new types with its current types. Stock only applies the changes when
    // *every* connected slice yields a non-empty intersection.
    let connected = connected_slice_ids(sd, ei);
    if !connected.is_empty() {
        let mut changes: Vec<(usize, Vec<J>)> = Vec::new();
        for cid in &connected {
            let Some(ci) = sd.index_of_id(cid) else { continue };
            let ce_types: Vec<J> = sd.elements[ci]
                .get("type")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let inter = find_type_intersection(&new_types, &ce_types, &kind, fisher);
            if inter.is_empty() {
                changes.clear();
                break;
            }
            changes.push((ci, inter));
        }
        if changes.len() == connected.len() {
            for (ci, t) in changes {
                sd.elements[ci].set("type", J::Array(t));
            }
        }
    }
    sd.elements[ei].set("type", J::Array(new_types));
}

/// Slice elements connected to element `ei` (`findConnectedElements`, direct
/// slices only): same path, id prefixed with `{ei.id}:`, max != "0".
fn connected_slice_ids(sd: &StructureDefinition, ei: usize) -> Vec<String> {
    let base_id = sd.elements[ei].id().to_string();
    let base_path = sd.elements[ei].path().to_string();
    let prefix = format!("{base_id}:");
    sd.elements
        .iter()
        .filter(|e| {
            let id = e.id();
            id != base_id
                && e.path() == base_path
                && id.starts_with(&prefix)
                && e.get("max").and_then(|v| v.as_str()) != Some("0")
        })
        .map(|e| e.id().to_string())
        .collect()
}

/// `findTypeMatch` for a plain type name (no Reference/canonical keyword) against
/// a set of existing element types. Returns the candidate's own metadata
/// (`lineage[0]`) paired with the matched element-type code, mirroring stock.
fn find_type_match_plain(
    type_name: &str,
    right_types: &[J],
    kind: &str,
    fisher: &dyn Fisher,
) -> Option<Match> {
    let lineage = get_type_lineage(type_name, fisher);
    if lineage.is_empty() {
        return None;
    }
    for md in &lineage {
        let md_url = md.url.clone().unwrap_or_default();
        let md_sdtype = md.sd_type.clone().unwrap_or_default();
        let versioned = format!("{md_url}|{}", md.version.clone().unwrap_or_default());
        for rt in right_types {
            let c = type_code(rt).to_string();
            let profiles: Vec<String> = rt
                .get("profile")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let matches_unprofiled = c == md.id && profiles.is_empty();
            let anc = get_type_lineage(&md_sdtype, fisher);
            let matches_profile = anc.iter().any(|a| a.sd_type.as_deref() == Some(c.as_str()))
                && (profiles.contains(&md_url) || profiles.contains(&versioned));
            let matches_logical = kind == "logical" && !c.is_empty() && c == md_sdtype;
            if matches_unprofiled || matches_profile || matches_logical {
                return Some(Match { metadata: lineage[0].clone(), code: c });
            }
        }
    }
    None
}

/// `applyTypeIntersection` (targetType == None): group matches by their grouping
/// code, then build one constrained type per group.
fn apply_type_intersection(left: &J, matches: &[Match], kind: &str) -> Vec<J> {
    let mut groups: Vec<(String, Vec<&Match>)> = Vec::new();
    for m in matches {
        let group_code = if is_reference_type(&m.code) || m.code == "canonical" {
            m.code.clone()
        } else {
            m.metadata.sd_type.clone().unwrap_or_else(|| m.code.clone())
        };
        if let Some(g) = groups.iter_mut().find(|(k, _)| *k == group_code) {
            g.1.push(m);
        } else {
            groups.push((group_code, vec![m]));
        }
    }
    let mut out = Vec::new();
    for (group_code, gmatches) in groups {
        let mut nt = left.clone();
        let actual = left.get("code").and_then(|v| v.as_str()).unwrap_or("");
        if !actual.starts_with("http://hl7.org/fhirpath/System.") {
            if let Some(o) = nt.as_object_mut() {
                o.insert("code".into(), J::String(group_code.clone()));
            }
        }
        apply_profiles(&mut nt, &gmatches, kind);
        out.push(nt);
    }
    out
}

/// `findTypeIntersection` — intersect `left_types` (the newly-constrained parent
/// types) with `right_types` (a connected slice's current types).
fn find_type_intersection(
    left_types: &[J],
    right_types: &[J],
    kind: &str,
    fisher: &dyn Fisher,
) -> Vec<J> {
    let mut intersection: Vec<J> = Vec::new();
    for left in left_types {
        let lcode = type_code(left).to_string();
        let lprofiles: Vec<String> = left
            .get("profile")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let types_to_try: Vec<String> =
            if lprofiles.is_empty() { vec![lcode] } else { lprofiles };
        let mut matches: Vec<Match> = Vec::new();
        for tt in &types_to_try {
            if let Some(m) = find_type_match_plain(tt, right_types, kind, fisher) {
                matches.push(m);
            }
        }
        intersection.extend(apply_type_intersection(left, &matches, kind));
    }
    intersection
}

/// `applyProfiles` (targetType == None branch).
fn apply_profiles(new_type: &mut J, matches: &[&Match], kind: &str) {
    let mut matched_profiles: Vec<String> = Vec::new();
    let mut matched_target_profiles: Vec<String> = Vec::new();
    let new_code = type_code(new_type).to_string();
    for m in matches {
        let md_url = m.metadata.url.clone().unwrap_or_default();
        let md_sdtype = m.metadata.sd_type.clone().unwrap_or_default();
        if m.metadata.id == new_code && matches.len() == 1 {
            continue;
        } else if kind == "logical" && new_code == md_sdtype && md_sdtype == md_url {
            // A logical model's newType code is a URL; do not add it as a
            // profile/targetProfile (ElementDefinition.ts:1582-1589).
            continue;
        } else if is_reference_type(&m.code) && !is_reference_type(&md_sdtype) {
            matched_target_profiles.push(md_url);
        } else if m.code == "canonical" && md_sdtype != "canonical" {
            matched_target_profiles.push(md_url);
        } else {
            matched_profiles.push(md_url);
        }
    }
    if let Some(o) = new_type.as_object_mut() {
        if !matched_target_profiles.is_empty() {
            o.insert(
                "targetProfile".into(),
                J::Array(matched_target_profiles.into_iter().map(J::String).collect()),
            );
        }
        if !matched_profiles.is_empty() {
            o.insert(
                "profile".into(),
                J::Array(matched_profiles.into_iter().map(J::String).collect()),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// AddElementRule / ContainsRule (minimal)
// ---------------------------------------------------------------------------

/// `ElementDefinition.initializeElementType` — build the initial type array
/// from the AddElementRule's declared types.
fn initialize_element_type(types: &[OnlyRuleType], fisher: &dyn Fisher) -> Vec<J> {
    let mut ref_cnt = 0;
    let mut can_cnt = 0;
    let mut codeable_ref_cnt = 0;
    let mut initial: Vec<J> = Vec::new();
    for t in types {
        if t.is_reference {
            ref_cnt += 1;
        } else if t.is_canonical {
            can_cnt += 1;
        } else if t.is_codeable_reference {
            codeable_ref_cnt += 1;
        } else {
            let sd_type = fisher
                .fish_for_metadata(&t.type_)
                .and_then(|m| m.sd_type)
                .unwrap_or_else(|| t.type_.clone());
            let mut o = Map::new();
            o.insert("code".into(), J::String(sd_type));
            let cand = J::Object(o);
            if !initial.contains(&cand) {
                initial.push(cand);
            }
        }
    }
    let mk = |code: &str| {
        let mut o = Map::new();
        o.insert("code".into(), J::String(code.to_string()));
        J::Object(o)
    };
    if ref_cnt > 0 {
        initial.push(mk("Reference"));
    }
    if can_cnt > 0 {
        initial.push(mk("canonical"));
    }
    if codeable_ref_cnt > 0 {
        initial.push(mk("CodeableReference"));
    }
    initial
}

/// `StructureDefinition.newElement` + `ElementDefinition.applyAddElementRule`.
fn apply_add_element(sd: &mut StructureDefinition, rule: &Rule, fisher: &dyn Fisher, diag: &mut Vec<String>) {
    let Rule::AddElement {
        path,
        min,
        max,
        flags,
        types,
        content_reference,
        short,
        definition,
        ..
    } = rule
    else {
        return;
    };
    let path_type = sd.path_type();
    let id = format!("{path_type}.{path}");
    // newElement: error if an ancestor already defines this element.
    if sd.find_element(&id).is_some() {
        diag.push(format!("Element already defined: {id}"));
        return;
    }
    let mut ed = ElementDefinition::new(&id);
    // base + Element root constraints are set before captureOriginal in stock and
    // thus excluded from the differential; we skip them (no observable effect on
    // the differential, and they are not referenced downstream for these models).
    // type / contentReference, then card / flags / short / definition all appear
    // in the differential because there is no captured original for a new element.
    if !types.is_empty() {
        ed.set("type", J::Array(initialize_element_type(types, fisher)));
    } else if let Some(cr) = content_reference {
        ed.set("contentReference", J::String(cr.clone()));
    }
    sd.add_element(ed);
    let Some(ei) = sd.index_of_id(&id) else {
        return;
    };
    if !types.is_empty() {
        constrain_type(sd, ei, types, None, fisher, diag);
    }
    constrain_cardinality(&mut sd.elements[ei], *min, max);
    apply_flags(&mut sd.elements[ei], flags, false, diag);
    let short = short.clone().filter(|s| !s.is_empty());
    let definition = definition.clone().filter(|s| !s.is_empty());
    if let Some(s) = &short {
        sd.elements[ei].set("short", J::String(s.clone()));
    }
    match &definition {
        Some(d) => sd.elements[ei].set("definition", J::String(d.clone())),
        None => {
            // sdf-3: default definition to short when definition is empty.
            if let Some(s) = &short {
                sd.elements[ei].set("definition", J::String(s.clone()));
            }
        }
    }
}

fn handle_contains(
    sd: &mut StructureDefinition,
    ei: usize,
    items: &[fsh_model::ContainsRuleItem],
    _kind: StructureKind,
    fisher: &dyn Fisher,
    diag: &mut Vec<String>,
) {
    let is_extension = sd.elements[ei].type_codes() == ["Extension"]
        && sd.elements[ei].slice_name().is_none();
    if is_extension {
        handle_extension_contains(sd, ei, items, fisher, diag);
        return;
    }
    let path = sd.elements[ei].path().to_string();
    for item in items {
        if item.type_.is_some() {
            diag.push(format!(
                "Cannot specify type on {} slice since {path} is not an extension path.",
                item.name
            ));
        }
        if sd.add_slice(ei, &item.name, None).is_none() {
            diag.push(format!("Could not add slice {} at {path}", item.name));
        }
    }
}

/// `handleExtensionContainsRule` (simplified): ensure slicing on `extension`,
/// add a slice per item, set url (inline) or profile + url (named extension).
fn handle_extension_contains(
    sd: &mut StructureDefinition,
    ei: usize,
    items: &[fsh_model::ContainsRuleItem],
    fisher: &dyn Fisher,
    diag: &mut Vec<String>,
) {
    // ensure slicing by url on the extension element
    if sd.elements[ei].get("slicing").is_none() {
        sd.elements[ei].set(
            "slicing",
            json!({
                "discriminator": [{ "type": "value", "path": "url" }],
                "ordered": false,
                "rules": "open"
            }),
        );
    }
    for item in items {
        let Some(slice_id) = sd.add_slice(ei, &item.name, None) else {
            diag.push(format!("Could not add extension slice {}", item.name));
            continue;
        };
        let si = sd.index_of_id(&slice_id).unwrap();
        if let Some(type_name) = &item.type_ {
            // named extension: type = [{code:Extension, profile:[url]}], url fixed.
            // Stock computes `item.type.replace(/^[^|]+/, extension.url)`, i.e. it
            // keeps any `|version` suffix that was on the (possibly aliased) type
            // and only swaps the canonical part for the fished extension url.
            let url = match fisher.fish_for_metadata(type_name).and_then(|m| m.url) {
                Some(ext_url) => match type_name.find('|') {
                    Some(idx) => format!("{ext_url}{}", &type_name[idx..]),
                    None => ext_url,
                },
                None => type_name.clone(),
            };
            sd.elements[si].set(
                "type",
                json!([{ "code": "Extension", "profile": [url] }]),
            );
        } else {
            // inline extension: auto-fix the sub-extension's url to the slice name.
            sd.unfold_by_id(&slice_id, fisher);
            let url_id = format!("{slice_id}.url");
            if let Some(ui) = sd.index_of_id(&url_id) {
                sd.elements[ui].set("fixedUri", json!(item.name));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// setContext (default + simple)
// ---------------------------------------------------------------------------

fn set_context(sd: &mut StructureDefinition, def: &StructureDef, fisher: &dyn Fisher) {
    if !def.contexts.is_empty() {
        let mut contexts: Vec<J> = Vec::new();
        for ec in &def.contexts {
            if let Some(c) = build_context(ec, fisher) {
                contexts.push(c);
            }
        }
        sd.body.insert("context".into(), J::Array(contexts));
    } else if sd.body.get("context").is_none() {
        sd.body.insert(
            "context".into(),
            json!([{ "type": "element", "expression": "Element" }]),
        );
    }
}

fn build_context(ec: &ExtensionContext, _fisher: &dyn Fisher) -> Option<J> {
    if ec.is_quoted {
        Some(json!({ "expression": ec.value, "type": "fhirpath" }))
    } else {
        // G4: the `Context:` keyword emits source order `expression` then `type`
        // (stock SUSHI). The default-context literal and `^context` caret rules
        // are separate paths and keep their own (type-first) ordering.
        Some(json!({ "expression": ec.value, "type": "element" }))
    }
}

// ---------------------------------------------------------------------------
// Caret application (FHIR schema)
// ---------------------------------------------------------------------------

/// Resolve a `Canonical(localName)` caret value to its url. Stock fishes SD
/// types first (`ElementDefinition.ts:2006`); fall back to local ValueSet /
/// CodeSystem names when the fisher can't resolve an SD url.
fn resolve_canonical_caret(
    value: &FshValue,
    fisher: &dyn Fisher,
    vs_url: &dyn Fn(&str) -> Option<String>,
    cs_url: &dyn Fn(&str) -> Option<String>,
) -> FshValue {
    if let FshValue::Canonical(c) = value {
        // Resolve the entity to its canonical url. Stock fishes SD types (Profile,
        // Extension, Logical, Resource, ...) then ValueSet/CodeSystem; the version
        // (if any) is folded onto the url. Fall back to the bare name when nothing
        // resolves.
        let url = fisher
            .fish_for_metadata(&c.entity_name)
            .and_then(|m| m.url)
            .or_else(|| vs_url(&c.entity_name))
            .or_else(|| cs_url(&c.entity_name));
        if let Some(mut url) = url {
            if let Some(v) = &c.version {
                url = format!("{url}|{v}");
            }
            let mut c2 = c.clone();
            c2.entity_name = url;
            c2.version = None;
            return FshValue::Canonical(c2);
        }
    }
    // `replaceReferences` FshCode branch: resolve a bare CodeSystem name (local or
    // package) to its canonical url.
    if let Some(resolved) = resolve_code_system_in_value(value, fisher, cs_url) {
        return resolved;
    }
    value.clone()
}

/// `replaceReferences` FshCode-system branch as a standalone resolver: if `value`
/// is a FshCode whose system fishes to a CodeSystem (local FSH CodeSystems via
/// `cs_url` first, then dependency-package CodeSystems), return a clone with the
/// system rewritten to that canonical url (preserving any `|version`). Returns
/// `None` when nothing changed, so callers can keep using the original value.
fn resolve_code_system_in_value(
    value: &FshValue,
    fisher: &dyn Fisher,
    cs_url: &dyn Fn(&str) -> Option<String>,
) -> Option<FshValue> {
    let FshValue::Code(fc) = value else { return None };
    let sys = fc.system.as_ref()?;
    let resolve_cs =
        |base: &str| cs_url(base).or_else(|| fisher.fish_for_metadata_cs(base).and_then(|m| m.url));
    let new_sys = crate::export::replace_code_system(sys, resolve_cs)?;
    let mut fc2 = fc.clone();
    fc2.system = Some(new_sys);
    Some(FshValue::Code(fc2))
}

fn apply_caret_fhir(
    obj: &mut Map<String, J>,
    resource_type: &str,
    caret_path: &str,
    value: &FshValue,
    _cfg: &Config,
    resolver: &crate::type_resolver::TypeResolver,
) {
    let caret_path = resolve_caret_aliases(caret_path);
    if let Some((segs, leaf_ty)) = resolver.resolve(resource_type, &caret_path) {
        if let Some(leaf) = crate::export::coerce(value, &leaf_ty, resolver) {
            crate::export::apply(obj, &segs, leaf);
        }
    }
}

fn apply_caret_element(
    ed: &mut ElementDefinition,
    caret_path: &str,
    value: &FshValue,
    _cfg: &Config,
    resolver: &crate::type_resolver::TypeResolver,
) {
    let caret_path = resolve_caret_aliases(caret_path);
    if let Some((segs, leaf_ty)) = resolver.resolve("ElementDefinition", &caret_path) {
        if let Some(leaf) = crate::export::coerce(value, &leaf_ty, resolver) {
            crate::export::apply(ed.map_mut(), &segs, leaf);
        }
    }
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// Build the SD export context and export every local SD (in tank order).
/// Returns the context so the InstanceExporter can fish InstanceOf snapshots.
pub fn build_sd_context<'a>(
    docs: &[FshDocument],
    cfg: &'a Config,
    store: &'a package_store::PackageStore,
    vs_url: &'a dyn Fn(&str) -> Option<String>,
    cs_url: &'a dyn Fn(&str) -> Option<String>,
    predefined_vs: std::collections::HashMap<String, String>,
) -> SdContext<'a> {
    set_invariants(docs);
    set_aliases(docs);
    let mut ctx = SdContext::new(store, cfg, docs, vs_url, cs_url);
    ctx.set_predefined_vs(predefined_vs);
    ctx.export_all();
    ctx.export_mappings(docs);
    ctx
}

/// The exported SD JSON files (differential), in export order.
pub fn exported_files(ctx: &SdContext) -> Vec<crate::export::Exported> {
    let mut out = Vec::new();
    for e in &ctx.exported {
        let id = e.sd.get_str("id").unwrap_or("").to_string();
        out.push(crate::export::Exported {
            filename: format!("StructureDefinition-{}.json", id),
            body: e.sd.to_json_differential(),
        });
    }
    out
}

pub fn export_structure_definitions(
    docs: &[FshDocument],
    cfg: &Config,
    store: &package_store::PackageStore,
    vs_url: &dyn Fn(&str) -> Option<String>,
    cs_url: &dyn Fn(&str) -> Option<String>,
) -> Vec<crate::export::Exported> {
    let ctx = build_sd_context(docs, cfg, store, vs_url, cs_url, std::collections::HashMap::new());
    exported_files(&ctx)
}

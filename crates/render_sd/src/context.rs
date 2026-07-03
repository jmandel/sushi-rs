//! Per-IG render context: canonical URL -> (webPath, name/title) resolution,
//! reproducing the publisher's SpecMapManager + IGKnowledgeProvider link logic
//! from the same inputs the publisher had (the F0 build's package cache +
//! the IG's own output resources).
//!
//! Data sources (all real publisher inputs, no synthesized behavior):
//! - The IG's own resources: `<build>/output/*.json` — webPath is the local
//!   page `{ResourceType}-{id}.html` (relative; corePath="" for fragments).
//! - Each loaded dependency package `<home>/.fhir/packages/<id>#<ver>/package`:
//!   base URL = package.json `url` (e.g. hl7.fhir.r4.core -> http://hl7.org/fhir/R4,
//!   hl7.fhir.us.core#7.0.0 -> http://hl7.org/fhir/us/core/STU7); page names
//!   from `other/spec.internals` `paths` when present (the core spec), else the
//!   standard `{ResourceType}-{id}.html` convention.
//! - The IGKnowledgeProvider `getOverride` table sits on top for a fixed set of
//!   core datatypes (Extension -> extensibility.html#Extension etc.).
//!
//! Version resolution: when several loaded packages define the same canonical,
//! the highest package version wins (verified against the plan-net golden:
//! us-core 3.1.1/6.1.0/7.0.0 all loaded, links go to STU7).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// A resolved canonical.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// Absolute URL (dependency package) or local page (own IG resource).
    pub web_path: String,
    /// resource `name` (Java getName()).
    pub name: Option<String>,
    /// resource `title` (present() prefers title, falls back to name).
    pub title: Option<String>,
    pub rtype: String,
    /// package version this came from (for version-conflict resolution).
    pub version: String,
    /// StructureDefinition.kind ("primitive-type"/"complex-type"/"resource"/
    /// "logical"); None for non-SD resources.
    pub kind: Option<String>,
    /// StructureDefinition.derivation ("specialization"/"constraint").
    pub derivation: Option<String>,
    /// The file to load the full resource from (package path), if a dependency.
    pub file: Option<PathBuf>,
    /// True when the resource was fetched from a terminology server
    /// (`render_external_link` userdata in the publisher) -> external.png flag.
    pub external: bool,
}

impl Resolved {
    /// Java `present()`: title if set, else name, else id-ish.
    pub fn present(&self) -> String {
        self.title
            .clone()
            .or_else(|| self.name.clone())
            .unwrap_or_default()
    }
}

struct PkgEntry {
    base_url: String,
    version: String,
    /// The IG's fhirVersion-matched core package (the "master" package —
    /// PackageInformation.isMaster; its CodeSystem/ValueSet/base-SD copies own
    /// `masterDefinitions`, CanonicalResourceManager.java:394-400).
    is_master: bool,
    /// canonical -> (filename, resourceType, resource version).
    files: HashMap<String, (String, String, String)>,
    dir: PathBuf,
    /// canonical -> page from spec.internals (core spec only).
    spec_paths: Option<HashMap<String, String>>,
}

pub struct IgContext {
    /// own IG resources: canonical -> Resolved (local page).
    own: HashMap<String, Resolved>,
    packages: Vec<PkgEntry>,
    /// lazy cache of dependency lookups.
    cache: RefCell<HashMap<String, Option<Resolved>>>,
    /// lazy cache of full-resource loads.
    res_cache: RefCell<HashMap<String, Option<std::rc::Rc<Value>>>>,
    /// tx-server-fetched resources (input-cache/txcache/vs-externals.json):
    /// canonical -> (server, file). Last-resort resolution, external=true.
    tx_externals: HashMap<String, (String, PathBuf)>,
}

impl IgContext {
    /// Build from an F0-style build dir: `<build>/output` + `<build>/.home/.fhir/packages`,
    /// loading the IG's dependsOn closure. `own_dir` may instead be any dir of
    /// the IG's resource JSONs (e.g. fsh-generated for cycle).
    pub fn load(own_dir: &Path, packages_dir: &Path) -> IgContext {
        Self::load_with_txcache(own_dir, packages_dir, None)
    }

    /// `txcache_dir` = the build's `input-cache/txcache` (holds
    /// vs-externals.json + the tx-fetched VS bodies) — the same cache the
    /// publisher's BaseWorkerContext used (BaseWorkerContext.java:3499-3511).
    pub fn load_with_txcache(
        own_dir: &Path,
        packages_dir: &Path,
        txcache_dir: Option<&Path>,
    ) -> IgContext {
        let mut own = HashMap::new();
        let mut deps: Vec<(String, String)> = Vec::new(); // (pkgId, version)
        let mut ig_fhir_version: Option<String> = None;
        if let Ok(rd) = std::fs::read_dir(own_dir) {
            for e in rd.flatten() {
                let p = e.path();
                let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !fname.ends_with(".json") {
                    continue;
                }
                // Only resource-shaped files: Type-id.json
                let Ok(text) = std::fs::read_to_string(&p) else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                let rtype = v.get("resourceType").and_then(|x| x.as_str()).unwrap_or("");
                if rtype.is_empty() {
                    continue;
                }
                let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                if rtype == "ImplementationGuide" {
                    if let Some(fv) = v
                        .get("fhirVersion")
                        .and_then(|x| x.as_array())
                        .and_then(|a| a.first())
                        .and_then(|x| x.as_str())
                    {
                        ig_fhir_version = Some(fv.to_string());
                    }
                    for d in v
                        .get("dependsOn")
                        .and_then(|x| x.as_array())
                        .map(|a| a.as_slice())
                        .unwrap_or(&[])
                    {
                        if let (Some(pid), Some(ver)) = (
                            d.get("packageId").and_then(|x| x.as_str()),
                            d.get("version").and_then(|x| x.as_str()),
                        ) {
                            deps.push((pid.to_string(), ver.to_string()));
                        }
                    }
                }
                let Some(url) = v.get("url").and_then(|x| x.as_str()) else { continue };
                own.insert(
                    url.to_string(),
                    Resolved {
                        web_path: format!("{}-{}.html", rtype, id),
                        name: v.get("name").and_then(|x| x.as_str()).map(String::from),
                        title: v.get("title").and_then(|x| x.as_str()).map(String::from),
                        rtype: rtype.to_string(),
                        version: String::new(),
                        kind: v.get("kind").and_then(|x| x.as_str()).map(String::from),
                        derivation: v.get("derivation").and_then(|x| x.as_str()).map(String::from),
                        file: Some(p.clone()),
                        external: false,
                    },
                );
            }
        }

        // Transitive dependency closure (breadth-first over package.json deps).
        let mut to_visit = deps.clone();
        let mut seen: Vec<(String, String)> = Vec::new();
        while let Some((pid, ver)) = to_visit.pop() {
            if seen.iter().any(|(p, v)| *p == pid && *v == ver) {
                continue;
            }
            // Examples packages are never resolution sources (SpecMapManager
            // SpecialPackageType.Examples; TypeManager excludes examples-
            // sourced definitions).
            if pid.ends_with(".examples") {
                continue;
            }
            // The us-core "vNNN" facade packages are empty wrappers the
            // publisher resolves to the REAL us-core package of that version
            // (SimpleWorkerContext.java:695; SpecMapManager FACADE).
            if pid.starts_with("hl7.fhir.us.core.v") {
                to_visit.push(("hl7.fhir.us.core".to_string(), ver.clone()));
                continue;
            }
            seen.push((pid.clone(), ver.clone()));
            let pdir = packages_dir.join(format!("{}#{}", pid, ver)).join("package");
            let pj = pdir.join("package.json");
            if let Ok(text) = std::fs::read_to_string(&pj) {
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    if let Some(d) = v.get("dependencies").and_then(|x| x.as_object()) {
                        for (dp, dv) in d {
                            if let Some(dvs) = dv.as_str() {
                                to_visit.push((dp.to_string(), dvs.to_string()));
                            }
                        }
                    }
                }
            }
        }
        // The core package is always loaded (hl7.fhir.r4.core etc. comes in via
        // the dependency closure of every IG package; if absent, scan for it).
        if !seen.iter().any(|(p, _)| p.contains(".core")) {
            if let Ok(rd) = std::fs::read_dir(packages_dir) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().to_string();
                    if n.starts_with("hl7.fhir.r4.core#") || n.starts_with("hl7.fhir.r4b.core#") || n.starts_with("hl7.fhir.r5.core#") {
                        let parts: Vec<&str> = n.splitn(2, '#').collect();
                        seen.push((parts[0].to_string(), parts[1].to_string()));
                    }
                }
            }
        }

        // Core packages: only the one matching the IG's fhirVersion joins the
        // resolution pool (the publisher loads other-version cores for tooling
        // only; golden evidence: an R4 IG's base-type links are R4, never R5).
        let want_core = match ig_fhir_version.as_deref() {
            Some(v) if v.starts_with("4.0") => "hl7.fhir.r4.core",
            Some(v) if v.starts_with("4.3") => "hl7.fhir.r4b.core",
            Some(v) if v.starts_with("5.0") => "hl7.fhir.r5.core",
            Some(v) if v.starts_with("3.0") => "hl7.fhir.r3.core",
            _ => "hl7.fhir.r4.core",
        };
        seen.retain(|(pid, _)| {
            !(pid.starts_with("hl7.fhir.r") && pid.ends_with(".core")) || pid == want_core
        });

        let mut packages = Vec::new();
        for (pid, ver) in &seen {
            let pdir = packages_dir.join(format!("{}#{}", pid, ver)).join("package");
            if !pdir.exists() {
                continue;
            }
            let Some(mut entry) = load_package(&pdir, ver) else { continue };
            entry.is_master = pid == want_core;
            packages.push(entry);
        }

        let mut tx_externals = HashMap::new();
        if let Some(txd) = txcache_dir {
            if let Ok(text) = std::fs::read_to_string(txd.join("vs-externals.json")) {
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    if let Some(obj) = v.as_object() {
                        for (canonical, entry) in obj {
                            if let (Some(server), Some(fname)) = (
                                entry.get("server").and_then(|x| x.as_str()),
                                entry.get("filename").and_then(|x| x.as_str()),
                            ) {
                                tx_externals.insert(
                                    canonical.clone(),
                                    (server.to_string(), txd.join(fname)),
                                );
                            }
                        }
                    }
                }
            }
        }

        IgContext {
            own,
            packages,
            cache: RefCell::new(HashMap::new()),
            res_cache: RefCell::new(HashMap::new()),
            tx_externals,
        }
    }

    /// Resolve a canonical URL (versionless or `url|version`).
    pub fn resolve(&self, canonical: &str) -> Option<Resolved> {
        let (url, want_ver) = match canonical.split_once('|') {
            Some((u, v)) => (u, Some(v)),
            None => (canonical, None),
        };
        // Own IG resources win.
        if let Some(r) = self.own.get(url) {
            return Some(r.clone());
        }
        let key = canonical.to_string();
        if let Some(hit) = self.cache.borrow().get(&key) {
            return hit.clone();
        }
        // masterDefinitions rule (CanonicalResourceManager.java:394-400 +
        // get():713-719): the MASTER (core) package's CodeSystem/ValueSet/
        // specializing-SD copy wins outright for urls NOT under
        // terminology.hl7.org; THO urls are excluded from master, so the THO
        // package copies win there (golden-verified: core's v2-/v3- valueset
        // duplicates never shadow hl7.terminology's).
        let is_tho = url.starts_with("http://terminology.hl7.org");
        if !is_tho {
            for pkg in &self.packages {
                if !pkg.is_master {
                    continue;
                }
                if let Some((fname, rtype, rver)) = pkg.files.get(url) {
                    if matches!(rtype.as_str(), "CodeSystem" | "ValueSet" | "StructureDefinition") {
                        if let Some(v) = want_ver {
                            if rver != v && pkg.version != v {
                                continue;
                            }
                        }
                        let page = pkg
                            .spec_paths
                            .as_ref()
                            .and_then(|m| m.get(url))
                            .cloned()
                            .or_else(|| ig_override_page(url))
                            .unwrap_or_else(|| {
                                fname.trim_end_matches(".json").to_string() + ".html"
                            });
                        let page = ig_override_page(url).unwrap_or(page);
                        let web_path = if page.starts_with("http://") || page.starts_with("https://") {
                            page.clone()
                        } else {
                            join_url(&pkg.base_url, &page)
                        };
                        let fpath = pkg.dir.join(fname);
                        let (name, title, kind, derivation) = read_meta(&fpath);
                        // masterDefinitions applies to SDs only when they
                        // SPECIALIZE (base types/resources).
                        if rtype != "StructureDefinition"
                            || derivation.as_deref() != Some("constraint")
                        {
                            let out = Some(Resolved {
                                web_path,
                                name,
                                title,
                                rtype: rtype.clone(),
                                version: if rver.is_empty() { pkg.version.clone() } else { rver.clone() },
                                kind,
                                derivation,
                                file: Some(fpath),
                                external: false,
                            });
                            self.cache.borrow_mut().insert(key, out.clone());
                            return out;
                        }
                    }
                }
            }
        }

        let mut best: Option<Resolved> = None;
        let mut best_pkg = String::new();
        for pkg in &self.packages {
            // THO urls: the core package's v2-/v3- duplicates are never
            // fetchable masters; skip them when any THO package has the url.
            if is_tho
                && pkg.is_master
                && self
                    .packages
                    .iter()
                    .any(|p| !p.is_master && p.files.contains_key(url))
            {
                continue;
            }
            if let Some((fname, rtype, rver)) = pkg.files.get(url) {
                if let Some(v) = want_ver {
                    // `url|version` pins the RESOURCE version (business
                    // version); package version is the fallback comparator.
                    if rver != v && pkg.version != v {
                        continue;
                    }
                }
                let fpath = pkg.dir.join(fname);
                // page: spec.internals paths, else (for special packages
                // without spec.internals, e.g. us.cdc.phinvads) the resource's
                // meta.source (publisher SpecMapManager.getPath: paths ->
                // special -> def=meta.source), else filename convention.
                let mut page = pkg.spec_paths.as_ref().and_then(|m| m.get(url)).cloned();
                if page.is_none() && pkg.spec_paths.is_none() {
                    page = read_meta_source(&fpath);
                }
                let page = page
                    .or_else(|| ig_override_page(url))
                    .unwrap_or_else(|| fname.trim_end_matches(".json").to_string() + ".html");
                // getOverride sits on top of spec paths for the core set.
                let page = ig_override_page(url).unwrap_or(page);
                let web_path = if page.starts_with("http://") || page.starts_with("https://") {
                    // absolute paths bypass the base (PublisherLoader:119-122).
                    page.clone()
                } else {
                    join_url(&pkg.base_url, &page)
                };
                // lazy name/title/kind from the resource file.
                let (name, title, kind, derivation) = read_meta(&fpath);
                let cand = Resolved {
                    web_path,
                    name,
                    title,
                    rtype: rtype.clone(),
                    version: if rver.is_empty() {
                        pkg.version.clone()
                    } else {
                        rver.clone()
                    },
                    kind,
                    derivation,
                    file: Some(fpath),
                    external: false,
                };
                // Highest RESOURCE version wins (CanonicalResourceManager
                // sort); tie -> highest PACKAGE version (the golden picks
                // hl7.terminology 7.2.0's copy over 7.1.0's identical-version
                // resource).
                let cand_pkg = pkg.version.clone();
                best = match best {
                    None => {
                        best_pkg = cand_pkg;
                        Some(cand)
                    }
                    Some(prev) => {
                        if version_gt(&cand.version, &prev.version)
                            || (cand.version == prev.version
                                && version_gt(&cand_pkg, &best_pkg))
                        {
                            best_pkg = cand_pkg;
                            Some(cand)
                        } else {
                            Some(prev)
                        }
                    }
                };
            }
        }
        // Last resort: the tx-server cache (BaseWorkerContext.java:3499-3511):
        // webPath = pathURL(server, "ValueSet", vs.getIdBase()); external flag.
        if best.is_none() {
            if let Some((server, file)) = self.tx_externals.get(url) {
                if let Ok(text) = std::fs::read_to_string(file) {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                        best = Some(Resolved {
                            web_path: format!(
                                "{}/ValueSet/{}",
                                server.trim_end_matches('/'),
                                id
                            ),
                            name: v.get("name").and_then(|x| x.as_str()).map(String::from),
                            title: v.get("title").and_then(|x| x.as_str()).map(String::from),
                            rtype: v
                                .get("resourceType")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            version: v
                                .get("version")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            kind: None,
                            derivation: None,
                            file: Some(file.clone()),
                            external: true,
                        });
                    }
                }
            }
        }
        self.cache.borrow_mut().insert(key, best.clone());
        best
    }

    /// `fetchTypeDefinition(code)` equivalent: resolve the core/loaded SD for a
    /// bare type code.
    pub fn resolve_type(&self, code: &str) -> Option<Resolved> {
        if code.is_empty() {
            return None;
        }
        if code.starts_with("http://") || code.starts_with("https://") {
            return self.resolve(code);
        }
        self.resolve(&format!("http://hl7.org/fhir/StructureDefinition/{}", code))
    }

    /// `isPrimitiveType` (TypeManager.java:113): kind == primitive-type
    /// (+ xhtml). The Java fast-path name lists are consistent with kind.
    pub fn is_primitive_type(&self, code: &str) -> bool {
        if code == "xhtml" {
            return true;
        }
        self.resolve_type(code)
            .and_then(|r| r.kind)
            .map(|k| k == "primitive-type")
            .unwrap_or(false)
    }

    /// `isDataType` (TypeManager.java:123): kind == complex-type.
    pub fn is_data_type(&self, code: &str) -> bool {
        self.resolve_type(code)
            .and_then(|r| r.kind)
            .map(|k| k == "complex-type")
            .unwrap_or(false)
    }

    /// `hasMultipleVersions(fetchResourceVersions(url))` (SDR:2517-2523): the
    /// number of DISTINCT versions of a canonical across loaded packages
    /// (+ the IG's own resources).
    pub fn version_count(&self, canonical: &str) -> usize {
        let url = canonical.split('|').next().unwrap_or(canonical);
        let mut versions: Vec<&str> = Vec::new();
        if self.own.contains_key(url) {
            versions.push("");
        }
        for pkg in &self.packages {
            if pkg.files.contains_key(url) && !versions.contains(&pkg.version.as_str()) {
                versions.push(&pkg.version);
            }
        }
        versions.len()
    }

    /// IGKnowledgeProvider.hasLinkFor (IGKnowledgeProvider.java:568-570):
    /// the type resolves AND kind is a type kind AND derivation==specialization
    /// (base abstract types like Resource/Element have no derivation -> false).
    pub fn has_link_for(&self, code: &str) -> bool {
        self.resolve_type(code)
            .map(|r| {
                r.kind.is_some() && r.derivation.as_deref() == Some("specialization")
            })
            .unwrap_or(false)
    }

    /// The IG's own StructureDefinitions whose baseDefinition equals `url`
    /// (for the abstract-root child list). Deterministic (sorted by web_path);
    /// the publisher's own order is identity-hash-unstable (see caller).
    pub fn own_sds_derived_from(&self, url: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for r in self.own.values() {
            if r.rtype != "StructureDefinition" {
                continue;
            }
            let Some(f) = &r.file else { continue };
            let Ok(text) = std::fs::read_to_string(f) else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
            let base = v
                .get("baseDefinition")
                .and_then(|x| x.as_str())
                .map(|b| b.split('|').next().unwrap_or(b));
            if base == Some(url) {
                out.push((r.web_path.clone(), r.name.clone().unwrap_or_default()));
            }
        }
        out.sort();
        out
    }

    /// Load the full resource JSON for a canonical (for locateExtension etc.).
    /// Cached per canonical.
    pub fn load_resource(&self, canonical: &str) -> Option<std::rc::Rc<Value>> {
        if let Some(hit) = self.res_cache.borrow().get(canonical) {
            return hit.clone();
        }
        let out = self.resolve(canonical).and_then(|r| {
            let f = r.file?;
            let text = std::fs::read_to_string(f).ok()?;
            serde_json::from_str::<Value>(&text).ok().map(std::rc::Rc::new)
        });
        self.res_cache
            .borrow_mut()
            .insert(canonical.to_string(), out.clone());
        out
    }

    /// `IGKnowledgeProvider.resolveBinding` (the `context.getPkp().resolveBinding`
    /// call at SDR:2005/3150/5280 etc). Given a ValueSet canonical reference,
    /// return the display, webPath link, and URI shown in the binding piece.
    /// Ported from IGKnowledgeProvider.java:640-690 (the v3/v2 specials, the
    /// core-VS branch, LOINC, VSAC, and the general resolve). Shared by the
    /// snapshot table path and the grid path.
    pub fn resolve_binding(&self, vs_ref: &str) -> BindingRes {
        if vs_ref.is_empty() {
            return BindingRes {
                url: Some("terminologies.html#unbound".into()),
                display: "(unbound)".into(),
                uri: None,
                external: false,
            };
        }
        if let Some(rest) = vs_ref.strip_prefix("http://hl7.org/fhir/ValueSet/v3-") {
            return BindingRes {
                url: Some(format!("http://hl7.org/fhir/R4/v3/{}/vs.html", rest)),
                display: rest.to_string(),
                uri: Some(vs_ref.to_string()),
                external: false,
            };
        }
        if let Some(rest) = vs_ref.strip_prefix("http://hl7.org/fhir/ValueSet/v2-") {
            return BindingRes {
                url: Some(format!("http://hl7.org/fhir/R4/v2/{}/index.html", rest)),
                display: rest.to_string(),
                uri: Some(vs_ref.to_string()),
                external: false,
            };
        }
        if vs_ref.starts_with("http://hl7.org/fhir/ValueSet/") {
            if let Some(vs) = self.resolve(vs_ref) {
                return BindingRes {
                    url: Some(vs.web_path.clone()),
                    display: vs.name.clone().unwrap_or_default(),
                    uri: Some(strip_version(vs_ref)),
                    external: false,
                };
            }
            let rest = &vs_ref[29..];
            return BindingRes {
                url: None,
                display: format!("{} (??)", rest),
                uri: None,
                external: false,
            };
        }
        if vs_ref.starts_with("http://loinc.org/vs/") {
            let code = &vs_ref[20..];
            let display = if code.starts_with("LL") {
                format!("LOINC Answer List {}", code)
            } else {
                format!("LOINC {}", code)
            };
            return BindingRes {
                url: Some(format!("https://loinc.org/{}/", code)),
                display,
                uri: Some(vs_ref.to_string()),
                external: false,
            };
        }
        if let Some(vs) = self.resolve(vs_ref) {
            let display = if vs_ref.contains('|') {
                format!("{} ({})", vs.name.clone().unwrap_or_default(), vs.version)
            } else {
                vs.present()
            };
            return BindingRes {
                url: Some(vs.web_path.clone()),
                display,
                uri: Some(strip_version(vs_ref)),
                external: vs.external,
            };
        }
        if vs_ref.contains("cts.nlm.nih.gov") {
            let oid = vs_ref.rsplit('/').next().unwrap_or("");
            return BindingRes {
                url: Some(format!("https://vsac.nlm.nih.gov/valueset/{}/expansion", oid)),
                display: format!("VSAC {}", oid),
                uri: Some(vs_ref.to_string()),
                external: true,
            };
        }
        if vs_ref.starts_with("http://") || vs_ref.starts_with("https://") {
            return BindingRes {
                url: Some(vs_ref.to_string()),
                display: vs_ref.to_string(),
                uri: None,
                external: false,
            };
        }
        BindingRes {
            url: None,
            display: vs_ref.to_string(),
            uri: None,
            external: false,
        }
    }
}

/// The result of `resolveBinding`: the ValueSet piece's link/display/uri.
#[derive(Debug, Clone)]
pub struct BindingRes {
    pub url: Option<String>,
    pub display: String,
    pub uri: Option<String>,
    pub external: bool,
}

/// Strip a trailing `|version` from a canonical URL.
pub fn strip_version(url: &str) -> String {
    match url.split_once('|') {
        Some((u, _)) => u.to_string(),
        None => url.to_string(),
    }
}

fn load_package(pdir: &Path, ver: &str) -> Option<PkgEntry> {
    let pj: Value =
        serde_json::from_str(&std::fs::read_to_string(pdir.join("package.json")).ok()?).ok()?;
    let base_url = fix_package_url(pj.get("url").and_then(|x| x.as_str())?);
    let mut files = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(pdir.join(".index.json")) {
        if let Ok(idx) = serde_json::from_str::<Value>(&text) {
            for f in idx
                .get("files")
                .and_then(|x| x.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[])
            {
                if let (Some(url), Some(fname), Some(rt)) = (
                    f.get("url").and_then(|x| x.as_str()),
                    f.get("filename").and_then(|x| x.as_str()),
                    f.get("resourceType").and_then(|x| x.as_str()),
                ) {
                    let rv = f.get("version").and_then(|x| x.as_str()).unwrap_or("");
                    files.insert(
                        url.to_string(),
                        (fname.to_string(), rt.to_string(), rv.to_string()),
                    );
                }
            }
        }
    }
    // spec.internals (core spec only) — has a UTF-8 BOM.
    let mut spec_paths = None;
    let si_path = pdir.join("other").join("spec.internals");
    if let Ok(bytes) = std::fs::read(&si_path) {
        let text = String::from_utf8_lossy(&bytes);
        let text = text.trim_start_matches('\u{feff}');
        if let Ok(si) = serde_json::from_str::<Value>(text) {
            if let Some(paths) = si.get("paths").and_then(|x| x.as_object()) {
                let mut m = HashMap::new();
                for (k, v) in paths {
                    if let Some(vs) = v.as_str() {
                        m.insert(k.clone(), vs.to_string());
                    }
                }
                spec_paths = Some(m);
            }
        }
    }
    Some(PkgEntry {
        base_url,
        version: ver.to_string(),
        is_master: false,
        files,
        dir: pdir.to_path_buf(),
        spec_paths,
    })
}

fn read_meta_source(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("meta")
        .and_then(|m| m.get("source"))
        .and_then(|x| x.as_str())
        .map(String::from)
}

fn read_meta(path: &Path) -> (Option<String>, Option<String>, Option<String>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (None, None, None, None);
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return (None, None, None, None);
    };
    (
        v.get("name").and_then(|x| x.as_str()).map(String::from),
        v.get("title").and_then(|x| x.as_str()).map(String::from),
        v.get("kind").and_then(|x| x.as_str()).map(String::from),
        v.get("derivation").and_then(|x| x.as_str()).map(String::from),
    )
}

/// `PackageHacker.fixPackageUrl` (PackageHacker.java): workarounds for past
/// publishing problems. The corpus-relevant rules; the file://C:\ legacy table
/// is omitted (none of those packages are in our dependency closures — any hit
/// would surface as a divergence and be added with citation).
fn fix_package_url(webref: &str) -> String {
    if webref.contains("hl7.org/fhir/us/core/STU4.0.0") {
        return webref.replace("hl7.org/fhir/us/core/STU4.0.0", "hl7.org/fhir/us/core/STU4");
    }
    if webref == "http://hl7.org/fhir/us/core/v311" {
        return "https://hl7.org/fhir/us/core/STU3.1.1".to_string();
    }
    if webref.contains("hl7.org/fhir/uv/hl7.fhir.uv.xver") {
        return webref.replace("hl7.org/fhir/uv/hl7.fhir.uv.xver", "hl7.org/fhir/uv/xver");
    }
    webref.to_string()
}

/// `Utilities.pathURL`-style join for base + page.
pub fn join_url(base: &str, page: &str) -> String {
    if base.is_empty() {
        return page.to_string();
    }
    if base.ends_with('/') || page.starts_with('/') {
        format!("{}{}", base, page)
    } else {
        format!("{}/{}", base, page)
    }
}

/// The IGKnowledgeProvider `getOverride` table (core datatype page overrides).
/// Keys are the full canonical URLs.
fn ig_override_page(url: &str) -> Option<String> {
    let code = url.strip_prefix("http://hl7.org/fhir/StructureDefinition/")?;
    let page = match code {
        "Reference" => "references.html#Reference",
        "Extension" => "extensibility.html#Extension",
        "DataRequirement" => "metadatatypes.html#DataRequirement",
        "ContactDetail" => "metadatatypes.html#ContactDetail",
        "Contributor" => "metadatatypes.html#Contributor",
        "ParameterDefinition" => "metadatatypes.html#ParameterDefinition",
        "RelatedArtifact" => "metadatatypes.html#RelatedArtifact",
        "TriggerDefinition" => "metadatatypes.html#TriggerDefinition",
        "UsageContext" => "metadatatypes.html#UsageContext",
        _ => return None,
    };
    Some(page.to_string())
}

/// Compare two package versions, semver-style: numeric dot segments, and a
/// prerelease suffix ("-ballot-tc1") LOWERS precedence (5.3.0 > 5.3.0-ballot).
fn version_gt(a: &str, b: &str) -> bool {
    version_key(a) > version_key(b)
}

fn version_key(v: &str) -> (Vec<i64>, bool, String) {
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let nums: Vec<i64> = core.split('.').map(|s| s.parse().unwrap_or(-1)).collect();
    // release (no prerelease) sorts above prerelease: bool true > false.
    (nums, pre.is_none(), pre.unwrap_or("").to_string())
}

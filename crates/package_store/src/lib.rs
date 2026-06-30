//! package_store: the READ side of fhir-package-loader (FPL) that SUSHI's
//! `FHIRDefinitions` exposes. Resolves a project's FHIR dependency graph from a
//! local package cache (exactly as `sushi-ts/src/utils/Processing.ts`
//! `loadExternalDependencies` does) and fishes resources by canonical/id/name/type
//! with SUSHI's resolution order (`FHIRDefinitions.ts` `FISHING_ORDER` +
//! `DEFAULT_SORT` = byType then reverse-load-order / LIFO).
//!
//! HARD RULE: the cache dir is ALWAYS explicit (never default to ~/.fhir).
//! See docs/specs/{06-package-fhirdefs.md,package-store-notes.md}.
//! Gate: `harness/package-oracle.cjs` (run under the isolated cache).

use rustc_hash::FxHashMap;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// Fishing type (mirrors `sushi-ts/src/utils/Fishable.ts` `Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FishType {
    Resource,
    Type,
    Profile,
    Extension,
    ValueSet,
    CodeSystem,
    Logical,
    Instance,
}

/// The default search order used by SUSHI's untyped `fishForFHIR`.
pub const ALL_FISH_TYPES: &[FishType] = &[
    FishType::Resource,
    FishType::Type,
    FishType::Profile,
    FishType::Extension,
    FishType::ValueSet,
    FishType::CodeSystem,
    FishType::Logical,
];

impl FishType {
    /// Rank in `FISHING_ORDER`
    /// (`Resource, Logical, Type, Profile, Extension, ValueSet, CodeSystem`)
    /// used as the primary `DEFAULT_SORT` key (`FHIRDefinitions.ts:22-32`).
    fn fishing_rank(self) -> u8 {
        match self {
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
}

/// One fishable resource (StructureDefinition / ValueSet / CodeSystem) discovered
/// in a loaded package's `.index.json`.
#[derive(Debug, Clone)]
struct ResEntry {
    /// Global load sequence: packages in load order, files in index order. The
    /// LIFO secondary sort uses `Reverse(seq)`.
    seq: usize,
    resource_type: String,
    id: String,
    url: Option<String>,
    version: Option<String>,
    /// `.index.json` `type` (the SD's `type`; FPL `sdType`).
    sd_type: Option<String>,
    /// `.index.json` `kind` (resource / complex-type / logical / ...).
    kind: Option<String>,
    /// Resource `name` (read eagerly from the file; not in `.index.json`).
    name: Option<String>,
    fish_type: FishType,
    /// Absolute path to the resource JSON on disk.
    path: PathBuf,
}

/// Reads package `.index.json` files and resolves canonical/id/name → resource.
pub struct PackageStore {
    entries: Vec<ResEntry>,
    by_id: FxHashMap<String, Vec<usize>>,
    by_url: FxHashMap<String, Vec<usize>>,
    by_name: FxHashMap<String, Vec<usize>>,
}

// ---------------------------------------------------------------------------
// Config parsing (the subset Processing.ts reads: fhirVersion + dependencies)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct DepEntry {
    package_id: String,
    version: Option<String>,
}

struct ProjectConfig {
    fhir_version: String,
    dependencies: Vec<DepEntry>,
}

fn parse_config(ig_dir: &str) -> anyhow::Result<ProjectConfig> {
    let cfg_path = Path::new(ig_dir).join("sushi-config.yaml");
    let text = std::fs::read_to_string(&cfg_path)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", cfg_path.display()))?;
    let root: Value = serde_yaml::from_str(&text)?;

    // fhirVersion: string or sequence; take the first.
    let fhir_version = match root.get("fhirVersion") {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Array(a)) => a
            .first()
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .ok_or_else(|| anyhow::anyhow!("empty fhirVersion array"))?,
        Some(Value::Number(n)) => n.to_string(),
        _ => anyhow::bail!("sushi-config.yaml missing fhirVersion"),
    };

    // dependencies: map<packageId, version|{version,...}> in insertion order.
    let mut dependencies = Vec::new();
    if let Some(Value::Object(map)) = root.get("dependencies") {
        for (id, val) in map {
            let version = match val {
                Value::String(s) => Some(s.trim().to_string()),
                Value::Number(n) => Some(n.to_string()),
                Value::Object(m) => m.get("version").and_then(|v| match v {
                    Value::String(s) => Some(s.trim().to_string()),
                    Value::Number(n) => Some(n.to_string()),
                    _ => None,
                }),
                _ => None,
            };
            // Honor npm-alias syntax `alias@npm:realId` (Processing.ts:362-371).
            let package_id = match id.split_once("@npm:") {
                Some((_alias, real)) => real.to_string(),
                None => id.clone(),
            };
            dependencies.push(DepEntry { package_id, version });
        }
    }

    Ok(ProjectConfig {
        fhir_version,
        dependencies,
    })
}

// ---------------------------------------------------------------------------
// FHIR version + automatic-dependency resolution (Processing.ts)
// ---------------------------------------------------------------------------

/// Returns `(corePackageId, fhirVersionName)` for a fhirVersion string
/// (port of the supported rows of `FHIRVersionUtils.ts` `VERSIONS`).
fn fhir_version_info(version: &str) -> (String, &'static str) {
    // Strip a pre-release suffix for the numeric checks.
    let core = version.split('-').next().unwrap_or(version);
    let parts: Vec<&str> = core.split('.').collect();
    let major = parts.first().copied().unwrap_or("");
    let minor = parts.get(1).copied().unwrap_or("");
    match (major, minor) {
        ("4", "0") => ("hl7.fhir.r4.core".into(), "R4"),
        ("4", "3") => ("hl7.fhir.r4b.core".into(), "R4B"),
        ("4", "1") => ("hl7.fhir.r4b.core".into(), "R4B"),
        ("5", _) => ("hl7.fhir.r5.core".into(), "R5"),
        ("6", _) => ("hl7.fhir.r6.core".into(), "R6"),
        _ if version == "current" || version == "dev" => ("hl7.fhir.r5.core".into(), "R5"),
        // catch-all (unsupported) — keep loading attempt graceful.
        _ => (format!("hl7.fhir.{major}.core"), "??"),
    }
}

struct AutoDep {
    package_id: &'static str,
    fhir_versions: &'static [&'static str],
    high: bool,
}

// `AUTOMATIC_DEPENDENCIES` (Processing.ts:61-98).
const AUTOMATIC_DEPENDENCIES: &[AutoDep] = &[
    AutoDep { package_id: "hl7.fhir.uv.tools.r4", fhir_versions: &["R4", "R4B"], high: false },
    AutoDep { package_id: "hl7.fhir.uv.tools.r5", fhir_versions: &["R5", "R6"], high: false },
    AutoDep { package_id: "hl7.terminology.r4", fhir_versions: &["R4", "R4B"], high: false },
    AutoDep { package_id: "hl7.terminology.r5", fhir_versions: &["R5", "R6"], high: false },
    AutoDep { package_id: "hl7.fhir.uv.extensions.r4", fhir_versions: &["R4", "R4B"], high: true },
    AutoDep { package_id: "hl7.fhir.uv.extensions.r5", fhir_versions: &["R5", "R6"], high: true },
];

/// Strip a trailing `.r4`..`.r9` from a package id
/// (`configuredDependencyMatchesAutomaticDependency`, Processing.ts:100-111).
fn root_id(id: &str) -> &str {
    let bytes = id.as_bytes();
    if bytes.len() >= 3 {
        let tail = &id[id.len() - 3..];
        if tail.starts_with(".r") {
            let d = tail.as_bytes()[2];
            if (b'4'..=b'9').contains(&d) {
                return &id[..id.len() - 3];
            }
        }
    }
    id
}

fn config_matches_auto(cd: &str, ad: &str) -> bool {
    root_id(cd) == root_id(ad)
}

// ---------------------------------------------------------------------------
// version comparison (semver compareLoose-ish, enough for `latest` selection)
// ---------------------------------------------------------------------------

fn parse_num_ver(v: &str) -> Option<(u64, u64, u64, Option<String>)> {
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p.to_string())),
        None => (v, None),
    };
    let mut it = core.split('.');
    let maj = it.next()?.parse::<u64>().ok()?;
    let min = it.next().unwrap_or("0").parse::<u64>().ok()?;
    let pat = it.next().unwrap_or("0").parse::<u64>().ok()?;
    Some((maj, min, pat, pre))
}

fn version_cmp(a: &str, b: &str) -> Ordering {
    match (parse_num_ver(a), parse_num_ver(b)) {
        (Some((a1, a2, a3, ap)), Some((b1, b2, b3, bp))) => {
            (a1, a2, a3).cmp(&(b1, b2, b3)).then_with(|| match (ap, bp) {
                // a release outranks a pre-release of the same core version.
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(x), Some(y)) => x.cmp(&y),
            })
        }
        // Non-numeric versions (e.g. dates) fall back to lexical compare.
        _ => a.cmp(b),
    }
}

/// Resolve `latest` for a package id = the highest-version cached dir.
fn resolve_latest(cache_dir: &Path, package_id: &str) -> Option<String> {
    let prefix = format!("{package_id}#");
    let mut best: Option<String> = None;
    let rd = std::fs::read_dir(cache_dir).ok()?;
    for ent in rd.flatten() {
        let name = ent.file_name();
        let name = name.to_string_lossy();
        if let Some(ver) = name.strip_prefix(&prefix) {
            // Exclude nested `#` (none expected) and ensure it's a real package dir.
            match &best {
                Some(b) if version_cmp(ver, b) != Ordering::Greater => {}
                _ => best = Some(ver.to_string()),
            }
        }
    }
    best
}

// ---------------------------------------------------------------------------
// .index.json
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct IndexFile {
    files: Vec<IndexEntry>,
}

#[derive(Deserialize)]
struct IndexEntry {
    filename: String,
    #[serde(rename = "resourceType")]
    resource_type: Option<String>,
    id: Option<String>,
    url: Option<String>,
    version: Option<String>,
    kind: Option<String>,
    #[serde(rename = "type")]
    sd_type: Option<String>,
    derivation: Option<String>,
}

#[derive(Deserialize)]
struct NameProbe {
    #[serde(default)]
    name: Option<String>,
}

/// Classify an index entry into a `FishType`, mirroring how FPL/FHIRDefinitions
/// derive the searchable type for SD/VS/CS. Returns `None` for resources that are
/// not fishable as one of the conformance types (instances, examples, etc.).
fn classify(e: &IndexEntry) -> Option<FishType> {
    match e.resource_type.as_deref() {
        Some("ValueSet") => Some(FishType::ValueSet),
        Some("CodeSystem") => Some(FishType::CodeSystem),
        Some("StructureDefinition") => {
            if e.derivation.as_deref() == Some("constraint") {
                if e.sd_type.as_deref() == Some("Extension") {
                    Some(FishType::Extension)
                } else {
                    Some(FishType::Profile)
                }
            } else {
                // specialization (or absent, e.g. base `Resource`)
                match e.kind.as_deref() {
                    Some("logical") => Some(FishType::Logical),
                    Some("resource") => Some(FishType::Resource),
                    _ => Some(FishType::Type), // primitive-type / complex-type
                }
            }
        }
        _ => None,
    }
}

impl PackageStore {
    /// Resolve the project's dependency graph and index every resolved package.
    /// `cache_dir` MUST be explicit (the `<cache>/<name>#<version>/package` root).
    pub fn for_project(ig_dir: &str, cache_dir: &str) -> anyhow::Result<Self> {
        let cache = Path::new(cache_dir);
        if !cache.is_dir() {
            anyhow::bail!(
                "package_store: cache dir does not exist or is not a directory: {cache_dir}"
            );
        }
        let cfg = parse_config(ig_dir)?;
        let load_list = resolve_load_order(&cfg, cache);

        let mut store = PackageStore {
            entries: Vec::new(),
            by_id: FxHashMap::default(),
            by_url: FxHashMap::default(),
            by_name: FxHashMap::default(),
        };
        let mut seq = 0usize;
        for (id, version) in &load_list {
            let pkg_dir = cache.join(format!("{id}#{version}")).join("package");
            let index_path = pkg_dir.join(".index.json");
            let Ok(bytes) = std::fs::read(&index_path) else {
                // Package not present / unreadable — FPL would fail to load it.
                continue;
            };
            let Ok(index): Result<IndexFile, _> = serde_json::from_slice(&bytes) else {
                continue;
            };
            for e in &index.files {
                let Some(fish_type) = classify(e) else {
                    seq += 1;
                    continue;
                };
                let path = pkg_dir.join(&e.filename);
                // name is not in .index.json — read it eagerly (cheap probe).
                let name = std::fs::read(&path)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<NameProbe>(&b).ok())
                    .and_then(|p| p.name);
                let idx = store.entries.len();
                let entry = ResEntry {
                    seq,
                    resource_type: e.resource_type.clone().unwrap_or_default(),
                    id: e.id.clone().unwrap_or_default(),
                    url: e.url.clone(),
                    version: e.version.clone(),
                    sd_type: e.sd_type.clone(),
                    kind: e.kind.clone(),
                    name: name.clone(),
                    fish_type,
                    path,
                };
                if !entry.id.is_empty() {
                    store.by_id.entry(entry.id.clone()).or_default().push(idx);
                }
                if let Some(u) = &entry.url {
                    store.by_url.entry(u.clone()).or_default().push(idx);
                }
                if let Some(n) = &name {
                    store.by_name.entry(n.clone()).or_default().push(idx);
                }
                store.entries.push(entry);
                seq += 1;
            }
        }
        Ok(store)
    }

    /// Resolve a query (`item` or `item|version`) + requested types to the winning
    /// entry index, applying `normalizeTypes` (Instance ⇒ wildcard) + `DEFAULT_SORT`
    /// (byType asc, then reverse load order / LIFO).
    fn resolve(&self, item: &str, types: &[FishType]) -> Option<usize> {
        let (base, version) = match item.split_once('|') {
            Some((b, v)) => (b, Some(v)),
            None => (item, None),
        };
        // normalizeTypes: any Instance ⇒ no type filter (wildcard).
        let wildcard = types.iter().any(|t| *t == FishType::Instance);

        // Gather candidates matching by id OR name OR url.
        let mut cands: Vec<usize> = Vec::new();
        for map in [&self.by_id, &self.by_name, &self.by_url] {
            if let Some(v) = map.get(base) {
                cands.extend_from_slice(v);
            }
        }
        cands.sort_unstable();
        cands.dedup();

        cands.retain(|&i| {
            let e = &self.entries[i];
            if let Some(v) = version {
                if e.version.as_deref() != Some(v) {
                    return false;
                }
            }
            wildcard || types.contains(&e.fish_type)
        });

        cands.into_iter().min_by(|&a, &b| {
            let ea = &self.entries[a];
            let eb = &self.entries[b];
            ea.fish_type
                .fishing_rank()
                .cmp(&eb.fish_type.fishing_rank())
                // LIFO: later-loaded (higher seq) wins.
                .then_with(|| eb.seq.cmp(&ea.seq))
        })
    }

    /// `fishForFHIR(item, ...types)` — returns the full resource JSON.
    pub fn fish_for_fhir(&self, item: &str, types: &[FishType]) -> Option<Value> {
        let idx = self.resolve(item, types)?;
        let bytes = std::fs::read(&self.entries[idx].path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// `fishForMetadata(item, ...types)` — the `Metadata` object SUSHI emits
    /// (`convertInfoToMetadata`, FHIRDefinitions.ts:233-251). Key order and
    /// falsy-omission match the oracle.
    pub fn fish_for_metadata(&self, item: &str, types: &[FishType]) -> Option<Value> {
        let idx = self.resolve(item, types)?;
        let entry = &self.entries[idx];
        let value: Option<Value> = std::fs::read(&entry.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());

        let mut out = Map::new();
        // id
        if !entry.id.is_empty() {
            out.insert("id".into(), Value::String(entry.id.clone()));
        }
        // name
        if let Some(n) = &entry.name {
            if !n.is_empty() {
                out.insert("name".into(), Value::String(n.clone()));
            }
        }
        // sdType (= SD `type`)
        if let Some(t) = &entry.sd_type {
            if !t.is_empty() {
                out.insert("sdType".into(), Value::String(t.clone()));
            }
        }
        // url
        if let Some(u) = &entry.url {
            if !u.is_empty() {
                out.insert("url".into(), Value::String(u.clone()));
            }
        }
        // parent (= baseDefinition)
        if let Some(v) = &value {
            if let Some(p) = v.get("baseDefinition").and_then(|x| x.as_str()) {
                if !p.is_empty() {
                    out.insert("parent".into(), Value::String(p.to_string()));
                }
            }
            // imposeProfiles (extension structuredefinition-imposeProfile)
            let impose = impose_profiles(v);
            if !impose.is_empty() {
                out.insert(
                    "imposeProfiles".into(),
                    Value::Array(impose.into_iter().map(Value::String).collect()),
                );
            }
            // abstract (only when present / not null)
            if let Some(a) = v.get("abstract") {
                if a.is_boolean() {
                    out.insert("abstract".into(), a.clone());
                }
            }
        }
        // version
        if let Some(ver) = &entry.version {
            if !ver.is_empty() {
                out.insert("version".into(), Value::String(ver.clone()));
            }
        }
        // resourceType
        if !entry.resource_type.is_empty() {
            out.insert(
                "resourceType".into(),
                Value::String(entry.resource_type.clone()),
            );
        }
        // canBeTarget / canBind — only for logical models.
        if entry.kind.as_deref() == Some("logical") {
            let chars = sd_characteristics(value.as_ref());
            out.insert(
                "canBeTarget".into(),
                Value::Bool(chars.iter().any(|c| c == "can-be-target")),
            );
            out.insert(
                "canBind".into(),
                Value::Bool(chars.iter().any(|c| c == "can-bind")),
            );
        }
        // resourcePath
        out.insert(
            "resourcePath".into(),
            Value::String(entry.path.to_string_lossy().to_string()),
        );

        Some(Value::Object(out))
    }
}

fn impose_profiles(sd: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(exts) = sd.get("extension").and_then(|e| e.as_array()) {
        for ext in exts {
            if ext.get("url").and_then(|u| u.as_str())
                == Some("http://hl7.org/fhir/StructureDefinition/structuredefinition-imposeProfile")
            {
                if let Some(c) = ext.get("valueCanonical").and_then(|v| v.as_str()) {
                    out.push(c.to_string());
                }
            }
        }
    }
    out
}

fn sd_characteristics(sd: Option<&Value>) -> Vec<String> {
    let mut out = Vec::new();
    let Some(sd) = sd else { return out };
    // R5-style `characteristics: [code]`.
    if let Some(arr) = sd.get("characteristics").and_then(|c| c.as_array()) {
        for c in arr {
            if let Some(s) = c.as_str() {
                out.push(s.to_string());
            }
        }
    }
    // R4-style type-characteristics extension (valueCode).
    if let Some(exts) = sd.get("extension").and_then(|e| e.as_array()) {
        for ext in exts {
            if ext.get("url").and_then(|u| u.as_str())
                == Some(
                    "http://hl7.org/fhir/StructureDefinition/structuredefinition-type-characteristics",
                )
            {
                if let Some(c) = ext.get("valueCode").and_then(|v| v.as_str()) {
                    out.push(c.to_string());
                }
            }
        }
    }
    out
}

/// Build the ordered package load list, replicating `loadExternalDependencies`
/// (+ the two `loadAutomaticDependencies` passes). Returns `(id, version)` in load
/// order with `(id,version)` duplicates removed (first occurrence kept, like FPL
/// skip-if-already-loaded).
///
/// NOTE: the bundled R5-in-R4 virtual package (`sushi-r5forR4#1.0.0`, the lowest
/// priority loaded first for R4/R4B) is NOT included — its 7 defs are bundled
/// inside SUSHI, not in the cache. See report / KNOWN GAP.
fn resolve_load_order(cfg: &ProjectConfig, cache: &Path) -> Vec<(String, String)> {
    let (core_id, fhir_name) = fhir_version_info(&cfg.fhir_version);

    // Group configured deps by package id (insertion order), sort same-id by
    // version ascending so the latest loads last (Processing.ts:359-383).
    let mut grouped: Vec<(String, Vec<DepEntry>)> = Vec::new();
    for dep in &cfg.dependencies {
        if let Some((_, v)) = grouped.iter_mut().find(|(id, _)| *id == dep.package_id) {
            v.push(dep.clone());
        } else {
            grouped.push((dep.package_id.clone(), vec![dep.clone()]));
        }
    }
    let mut configured: Vec<DepEntry> = Vec::new();
    for (_, mut v) in grouped {
        v.sort_by(|a, b| {
            version_cmp(
                a.version.as_deref().unwrap_or(""),
                b.version.as_deref().unwrap_or(""),
            )
        });
        configured.extend(v);
    }
    // Append FHIR core (loaded last in the configured pass).
    configured.push(DepEntry {
        package_id: core_id.clone(),
        version: Some(cfg.fhir_version.clone()),
    });

    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |id: &str, ver: &str, out: &mut Vec<(String, String)>| {
        if !out.iter().any(|(i, v)| i == id && v == ver) {
            out.push((id.to_string(), ver.to_string()));
        }
    };

    // resolve a version string ('latest'/'current' ⇒ highest cached).
    let resolve_ver = |id: &str, ver: Option<&str>| -> Option<String> {
        match ver {
            None | Some("latest") | Some("current") => resolve_latest(cache, id),
            Some(v) => Some(v.to_string()),
        }
    };

    // -- Low automatic dependencies (before configured + core) ----------------
    auto_pass(
        false,
        fhir_name,
        &configured,
        &resolve_ver,
        &mut out,
        &mut push,
    );

    // -- Configured dependencies + FHIR core ----------------------------------
    for dep in &configured {
        let Some(ver) = &dep.version else { continue }; // null version ⇒ error+skip
        // Skip configured deps that match an automatic dep (loaded in High pass).
        if AUTOMATIC_DEPENDENCIES
            .iter()
            .any(|ad| config_matches_auto(&dep.package_id, ad.package_id))
        {
            continue;
        }
        if let Some(v) = resolve_ver(&dep.package_id, Some(ver)) {
            push(&dep.package_id, &v, &mut out);
        }
    }

    // -- High automatic dependencies (after core; e.g. extensions) ------------
    auto_pass(
        true,
        fhir_name,
        &configured,
        &resolve_ver,
        &mut out,
        &mut push,
    );

    out
}

#[allow(clippy::too_many_arguments)]
fn auto_pass(
    high: bool,
    fhir_name: &str,
    configured: &[DepEntry],
    resolve_ver: &dyn Fn(&str, Option<&str>) -> Option<String>,
    out: &mut Vec<(String, String)>,
    push: &mut dyn FnMut(&str, &str, &mut Vec<(String, String)>),
) {
    for ad in AUTOMATIC_DEPENDENCIES.iter().filter(|ad| ad.high == high) {
        // Prefer configured deps that match this automatic dep.
        let matches: Vec<&DepEntry> = configured
            .iter()
            .filter(|cd| config_matches_auto(&cd.package_id, ad.package_id))
            .collect();
        if !matches.is_empty() {
            for cd in matches {
                if let Some(v) = resolve_ver(&cd.package_id, cd.version.as_deref()) {
                    push(&cd.package_id, &v, out);
                }
            }
        } else if ad.fhir_versions.contains(&fhir_name) {
            if let Some(v) = resolve_ver(ad.package_id, Some("latest")) {
                push(ad.package_id, &v, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ordering() {
        assert_eq!(version_cmp("7.2.0", "7.1.0"), Ordering::Greater);
        assert_eq!(version_cmp("5.3.0", "5.3.0-ballot-tc1"), Ordering::Greater);
        assert_eq!(version_cmp("1.1.2", "1.1.2"), Ordering::Equal);
    }

    #[test]
    fn root_id_strip() {
        assert_eq!(root_id("hl7.fhir.uv.extensions.r4"), "hl7.fhir.uv.extensions");
        assert_eq!(root_id("hl7.terminology.r5"), "hl7.terminology");
        assert_eq!(root_id("hl7.fhir.uv.ipa"), "hl7.fhir.uv.ipa");
    }

    #[test]
    fn classify_types() {
        let sd = |kind: &str, ty: &str, der: Option<&str>| IndexEntry {
            filename: "f".into(),
            resource_type: Some("StructureDefinition".into()),
            id: Some("x".into()),
            url: None,
            version: None,
            kind: Some(String::from(kind)),
            sd_type: Some(String::from(ty)),
            derivation: der.map(String::from),
        };
        assert_eq!(classify(&sd("resource", "Observation", Some("specialization"))), Some(FishType::Resource));
        assert_eq!(classify(&sd("complex-type", "Quantity", Some("specialization"))), Some(FishType::Type));
        assert_eq!(classify(&sd("complex-type", "Extension", Some("constraint"))), Some(FishType::Extension));
        assert_eq!(classify(&sd("resource", "Patient", Some("constraint"))), Some(FishType::Profile));
        assert_eq!(classify(&sd("logical", "Foo", Some("specialization"))), Some(FishType::Logical));
    }
}

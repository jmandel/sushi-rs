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

pub mod bundle;
pub mod derived_index;
pub mod source;

pub use bundle::{BundleManifest, BundleManifestEntry, BundleSource, BUNDLE_FORMAT_VERSION};
pub use source::{DirEntry, DiskSource, PackageSource};

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
    /// For the bundled R5-in-R4 virtual package: the embedded JSON (no disk path).
    embedded: Option<&'static str>,
}

/// The `sushi-r5forR4#1.0.0` virtual package: 7 R5 defs needed in R4
/// (`dist/fhirdefs/R5DefsForR4`). Vendored so the port doesn't depend on the
/// fsh-sushi install. Loaded FIRST (lowest priority) so real packages shadow
/// them, but they're the only source for ActorDefinition / Base / etc.
const R5_FOR_R4_DEFS: &[&str] = &[
    include_str!("../vendor/r5-for-r4/StructureDefinition-Base.json"),
    include_str!("../vendor/r5-for-r4/StructureDefinition-ActorDefinition.json"),
    include_str!("../vendor/r5-for-r4/StructureDefinition-Requirements.json"),
    include_str!("../vendor/r5-for-r4/StructureDefinition-SubscriptionTopic.json"),
    include_str!("../vendor/r5-for-r4/StructureDefinition-TestPlan.json"),
    include_str!("../vendor/r5-for-r4/StructureDefinition-CodeableReference.json"),
    include_str!("../vendor/r5-for-r4/StructureDefinition-DataType.json"),
];

/// Reads package `.index.json` files and resolves canonical/id/name → resource.
pub struct PackageStore {
    entries: Vec<ResEntry>,
    by_id: FxHashMap<String, Vec<usize>>,
    by_url: FxHashMap<String, Vec<usize>>,
    by_name: FxHashMap<String, Vec<usize>>,
    /// Parse cache: a resource file is read+parsed once, then shared. SUSHI's
    /// FHIRDefinitions holds all defs in memory; we lazily memoize. Single-threaded
    /// build, so RefCell is fine. (Avoids re-parsing core SDs hundreds of times.)
    cache: std::cell::RefCell<FxHashMap<usize, std::rc::Rc<Value>>>,
    /// The storage backing this store, held for the lazy per-resource `read_value`.
    /// Native callers get a `DiskSource` (unchanged behavior); a browser/test caller
    /// can supply a read-only in-memory source.
    source: Box<dyn PackageSource>,
}

// ---------------------------------------------------------------------------
// Config parsing (the subset Processing.ts reads: fhirVersion + dependencies)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct DepEntry {
    package_id: String,
    version: Option<String>,
}

/// Ordered package request produced from a project's SUSHI dependency rules.
///
/// This is the acquisition-side counterpart to `PackageStore::for_project`: it
/// preserves stock SUSHI's load order, but leaves mutable coordinates such as
/// `latest`, `current`, and `dev` unresolved so the acquisition layer can resolve
/// them against registries/CAS and record a lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRequest {
    pub package_id: String,
    pub version: String,
}

struct ProjectConfig {
    fhir_version: String,
    dependencies: Vec<DepEntry>,
}

/// If `package_id` is a legacy cross-version extensions package
/// (`^hl7\.fhir\.extensions\.r\d+b?$`), return its source FHIR token (e.g. `r5`).
fn legacy_xver_source(package_id: &str) -> Option<String> {
    let rest = package_id.strip_prefix("hl7.fhir.extensions.")?;
    let rest = rest.strip_prefix('r')?;
    let (digits, suffix) = match rest.strip_suffix('b') {
        Some(d) => (d, "b"),
        None => (rest, ""),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("r{digits}{suffix}"))
}

/// Port of `fixCrossVersionDependencies` (Processing.ts:540-568): rewrite a legacy
/// `hl7.fhir.extensions.r{N}` dependency to the official xver package
/// `hl7.fhir.uv.xver-{source}.{target}#latest`, where `target` is derived from the
/// declared FHIR version of the legacy package (e.g. `4.0.1` -> `r4`).
fn fix_cross_version_dep(package_id: &str, version: Option<String>) -> (String, Option<String>) {
    if let (Some(source), Some(ver)) = (legacy_xver_source(package_id), version.as_deref()) {
        // getFHIRVersionInfo(version).name.replace(/D?STU/, 'r').toLowerCase()
        let name = fhir_version_info(ver).1;
        let target = name.replace("DSTU", "r").replace("STU", "r").to_lowercase();
        return (
            format!("hl7.fhir.uv.xver-{source}.{target}"),
            Some("latest".to_string()),
        );
    }
    (package_id.to_string(), version)
}

/// Public form of `fixCrossVersionDependencies` (Processing.ts:540-568) for the
/// IG `dependsOn` exporter. Given a configured dependency's `(package_id, version)`,
/// returns the official xver package `(packageId, "latest", uri)` when `package_id`
/// is a legacy `hl7.fhir.extensions.r{N}` package, else `None`.
pub fn xver_substitution(package_id: &str, version: &str) -> Option<(String, String, String)> {
    let source = legacy_xver_source(package_id)?;
    let name = fhir_version_info(version).1;
    let target = name.replace("DSTU", "r").replace("STU", "r").to_lowercase();
    let id = format!("hl7.fhir.uv.xver-{source}.{target}");
    let uri = format!("http://hl7.org/fhir/uv/xver/ImplementationGuide/{id}");
    Some((id, "latest".to_string(), uri))
}

fn parse_config(ig_dir: &str) -> anyhow::Result<ProjectConfig> {
    let cfg_path = Path::new(ig_dir).join("sushi-config.yaml");
    let text = std::fs::read_to_string(&cfg_path)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", cfg_path.display()))?;
    parse_config_text(&text)
}

/// Same dependency-graph resolution as [`parse_config`], but from the
/// `sushi-config.yaml` TEXT rather than reading it off disk. This is the last
/// read-path `std::fs` site the wasm build needed to shed (see
/// docs/wasm-editor-plan.md §4.3): the browser passes the config text through the
/// API. Native callers keep `parse_config` (identical behavior; it just reads
/// then delegates here).
fn parse_config_text(text: &str) -> anyhow::Result<ProjectConfig> {
    let root: Value = serde_yaml::from_str(text)?;

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
            // Replace an old-style cross-version extensions package
            // (`hl7.fhir.extensions.r5#4.0.1`) with the official xver package
            // (`hl7.fhir.uv.xver-r5.r4#latest`) so its extensions become fishable —
            // port of `fixCrossVersionDependencies` (Processing.ts:540-568).
            let (package_id, version) = fix_cross_version_dep(&package_id, version);
            dependencies.push(DepEntry {
                package_id,
                version,
            });
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
    AutoDep {
        package_id: "hl7.fhir.uv.tools.r4",
        fhir_versions: &["R4", "R4B"],
        high: false,
    },
    AutoDep {
        package_id: "hl7.fhir.uv.tools.r5",
        fhir_versions: &["R5", "R6"],
        high: false,
    },
    AutoDep {
        package_id: "hl7.terminology.r4",
        fhir_versions: &["R4", "R4B"],
        high: false,
    },
    AutoDep {
        package_id: "hl7.terminology.r5",
        fhir_versions: &["R5", "R6"],
        high: false,
    },
    AutoDep {
        package_id: "hl7.fhir.uv.extensions.r4",
        fhir_versions: &["R4", "R4B"],
        high: true,
    },
    AutoDep {
        package_id: "hl7.fhir.uv.extensions.r5",
        fhir_versions: &["R5", "R6"],
        high: true,
    },
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
            (a1, a2, a3)
                .cmp(&(b1, b2, b3))
                .then_with(|| match (ap, bp) {
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
fn resolve_latest(source: &dyn PackageSource, cache_dir: &Path, package_id: &str) -> Option<String> {
    let prefix = format!("{package_id}#");
    let mut best: Option<String> = None;
    let rd = source.read_dir(cache_dir).ok()?;
    for ent in rd {
        let name = ent.file_name;
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

/// Resolve a SUSHI/FPL `M.N.x` dependency against the explicit cache.
fn resolve_minor_wildcard(
    source: &dyn PackageSource,
    cache_dir: &Path,
    package_id: &str,
    requested: &str,
) -> Option<String> {
    let minor = requested.strip_suffix(".x")?;
    if minor.matches('.').count() != 1 {
        return None;
    }
    let dir_prefix = format!("{package_id}#{minor}.");
    let mut best: Option<String> = None;
    let rd = source.read_dir(cache_dir).ok()?;
    for ent in rd {
        let name = ent.file_name;
        if let Some(patch) = name.strip_prefix(&dir_prefix) {
            let ver = format!("{minor}.{patch}");
            match &best {
                Some(b) if version_cmp(&ver, b) != Ordering::Greater => {}
                _ => best = Some(ver.to_string()),
            }
        }
    }
    best
}

fn resolve_cached_version(
    source: &dyn PackageSource,
    cache_dir: &Path,
    package_id: &str,
    requested: Option<&str>,
) -> Option<String> {
    match requested {
        None | Some("latest") | Some("current") => resolve_latest(source, cache_dir, package_id),
        Some(v) if v.ends_with(".x") => Some(
            resolve_minor_wildcard(source, cache_dir, package_id, v)
                .unwrap_or_else(|| v.to_string()),
        ),
        Some(v) => Some(v.to_string()),
    }
}

// ---------------------------------------------------------------------------
// .index.json
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct IndexFile {
    files: Vec<IndexEntry>,
}

type IndexEntry = PackageResourceEntry;

#[derive(Debug, Clone, Deserialize)]
pub struct PackageResourceEntry {
    pub filename: String,
    #[serde(rename = "resourceType")]
    pub resource_type: Option<String>,
    pub id: Option<String>,
    #[serde(rename = "packageId")]
    pub package_id: Option<String>,
    pub url: Option<String>,
    pub version: Option<String>,
    pub kind: Option<String>,
    #[serde(rename = "type")]
    pub sd_type: Option<String>,
    pub derivation: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PackageResourceRecord {
    pub entry: PackageResourceEntry,
    pub path: PathBuf,
    pub name: Option<String>,
    pub ordinal: usize,
}

struct PackageResourceListing {
    records: Vec<PackageResourceRecord>,
    source_count: usize,
}

fn package_resource_listing(
    source: &dyn PackageSource,
    pkg_dir: &Path,
) -> PackageResourceListing {
    if !source.is_dir(pkg_dir) {
        return PackageResourceListing {
            records: Vec::new(),
            source_count: 0,
        };
    }

    let index: Option<IndexFile> = source
        .read(&pkg_dir.join(".index.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<IndexFile>(&b).ok());

    // Derived-columns index: filename -> `name`, content-derived once (CAS
    // sidecar or write-once) instead of the eager per-SD `probe_name_from_path`
    // read this used to do on every run. `name` is the only column the stock
    // `.index.json` lacks that this listing needs; ordering / source_count stay
    // driven by the existing index-vs-scan selection so fishing (LIFO seq)
    // precedence is byte-identical. See docs/package-derived-index.md.
    let derived_name: std::collections::HashMap<String, Option<String>> = derived_index::load(source, pkg_dir)
        .into_iter()
        .map(|e| (e.filename, e.name))
        .collect();
    let name_for = |filename: &str| -> Option<String> {
        derived_name.get(filename).cloned().flatten()
    };

    // HEURISTIC: "index is valid" == `.index.json` exists with a NON-EMPTY
    // `files` array. When valid we trust it as COMPLETE and index straight from
    // it (no directory scan). Only when the index is missing or `files:[]` do we
    // fall back to a full directory scan.
    //
    // Why this holds in practice: across the whole 7.6G cache (154 packages),
    // indexes are all-or-nothing — either complete or `files:[]`. The IG-Publisher
    // either runs its index step or ships an empty placeholder (e.g.
    // hl7.fhir.uv.subscriptions-backport.r4#1.1.0 — verified empty in the
    // published tarball itself). A *partially* populated index was never observed.
    // Stock SUSHI/FPL sidestep the question by ALWAYS directory-scanning; we trade
    // that for a real speedup (skip the readdir + re-reading covered files),
    // knowingly accepting only the empty-index failure mode.
    if let Some(idx) = index.filter(|i| !i.files.is_empty()) {
        let source_count = idx.files.len();
        let records = idx
            .files
            .into_iter()
            .enumerate()
            .map(|(ordinal, entry)| {
                let path = pkg_dir.join(&entry.filename);
                let name = if classify(&entry).is_some() {
                    // name is not in stock `.index.json` — take it from the
                    // content-derived index (computed once) rather than probing
                    // the file header on every run.
                    name_for(&entry.filename)
                } else {
                    None
                };
                PackageResourceRecord {
                    entry,
                    path,
                    name,
                    ordinal,
                }
            })
            .collect();
        return PackageResourceListing {
            records,
            source_count,
        };
    }

    // -- Deep-scan fallback: index missing or `files:[]` (stale/empty). -----
    // Mirror FPL's `getPotentialResourcePaths`: every `^[^.].*\.json$` file
    // (except package.json), SORTED, read from disk and indexed. Recovers
    // resources from packages whose index is broken/empty.
    let Ok(rd) = source.read_dir(pkg_dir) else {
        return PackageResourceListing {
            records: Vec::new(),
            source_count: 0,
        };
    };
    let mut files: Vec<String> = Vec::new();
    for ent in rd {
        if !ent.is_file {
            continue;
        }
        let fname = ent.file_name;
        if fname.starts_with('.') || !fname.to_ascii_lowercase().ends_with(".json") {
            continue; // dotfiles (incl. .index.json) and non-json excluded.
        }
        if fname == "package.json" {
            continue; // carries no resourceType — FPL reads then rejects it.
        }
        files.push(fname);
    }
    files.sort(); // FPL `getPotentialResourcePaths` sorts the paths.

    let source_count = files.len();
    let mut records = Vec::new();
    for (ordinal, fname) in files.into_iter().enumerate() {
        let path = pkg_dir.join(&fname);
        let Some(json) = source
            .read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        else {
            continue;
        };
        let name = json
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let entry = index_entry_from_json(&json, fname);
        records.push(PackageResourceRecord {
            entry,
            path,
            name,
            ordinal,
        });
    }

    PackageResourceListing {
        records,
        source_count,
    }
}

/// List package resource metadata using the same source selection as
/// `PackageStore`: trust a non-empty materialized `.index.json`, otherwise scan
/// top-level package JSON resources in FPL order.
pub fn package_resource_entries(pkg_dir: &Path) -> Vec<PackageResourceRecord> {
    package_resource_listing(&DiskSource, pkg_dir).records
}

/// Same as [`package_resource_entries`] but reading through an explicit
/// [`PackageSource`] (browser bundle, test in-memory source). Native callers use
/// the disk-backed [`package_resource_entries`].
pub fn package_resource_entries_with(
    source: &dyn PackageSource,
    pkg_dir: &Path,
) -> Vec<PackageResourceRecord> {
    package_resource_listing(source, pkg_dir).records
}

/// Build an `IndexEntry` from a parsed resource JSON, extracting exactly the
/// fields `.index.json` would have precomputed. Used by the directory-scan
/// reconcile when a file is not covered by `.index.json` (empty/stale index).
fn index_entry_from_json(json: &Value, filename: String) -> IndexEntry {
    let s = |k: &str| json.get(k).and_then(|v| v.as_str()).map(str::to_string);
    IndexEntry {
        filename,
        resource_type: s("resourceType"),
        id: s("id"),
        package_id: s("packageId"),
        url: s("url"),
        version: s("version"),
        kind: s("kind"),
        sd_type: s("type"),
        derivation: s("derivation"),
    }
}

/// Classify an index entry into a `FishType`, mirroring how FPL/FHIRDefinitions
/// derive the searchable type for SD/VS/CS. Returns `None` for resources that are
/// not fishable as one of the conformance types (instances, examples, etc.).
fn classify(e: &IndexEntry) -> Option<FishType> {
    match e.resource_type.as_deref() {
        Some("ValueSet") => Some(FishType::ValueSet),
        Some("CodeSystem") => Some(FishType::CodeSystem),
        Some("StructureDefinition") => {
            // Mirror fhir-package-loader's `getSDFlavor(resourceJSON)`
            // (BasePackageLoader.ts): the Extension-flavor test comes FIRST and is
            // keyed on `type == "Extension"` — NOT on `derivation`. This matters
            // because the FHIR R4 core `.index.json` omits `derivation`, so an
            // older keying on `derivation == "constraint"` mis-flavored core
            // extensions as `Type`. That tied them (by fishing rank) with same-name
            // datatypes and let LIFO win: e.g. fishing `markdown` returned the
            // `rendering-markdown` Extension (name == "markdown") instead of the
            // `markdown` primitive, so markdown-typed caret leaves were dropped.
            // (The base `Extension` SD derives from `Element`; stock excludes it
            // here, but it has no name/id/url fishing collision, so omitting that
            // sub-check is safe — verified against the 18-IG core.)
            if e.sd_type.as_deref() == Some("Extension") {
                Some(FishType::Extension)
            } else if e.derivation.as_deref() == Some("constraint") {
                Some(FishType::Profile)
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
    /// Resolve the project's dependency graph and index every resolved package,
    /// reading from disk (native behavior; unchanged).
    /// `cache_dir` MUST be explicit (the `<cache>/<name>#<version>/package` root).
    pub fn for_project(ig_dir: &str, cache_dir: &str) -> anyhow::Result<Self> {
        Self::for_project_with(DiskSource, ig_dir, cache_dir)
    }

    /// Same as [`PackageStore::for_project`] but reading every package file through
    /// an explicit [`PackageSource`] (browser bundle, test in-memory source). The
    /// `ig_dir` config is still read via `std::fs` here (the project is native even
    /// when packages are mounted from a bundle); only the package cache flows
    /// through `source`.
    pub fn for_project_with(
        source: impl PackageSource + 'static,
        ig_dir: &str,
        cache_dir: &str,
    ) -> anyhow::Result<Self> {
        let cfg = parse_config(ig_dir)?;
        Self::for_project_from_config(source, cfg, cache_dir)
    }

    /// Same as [`PackageStore::for_project_with`] but the project's dependency
    /// list comes from the `sushi-config.yaml` TEXT, not a disk read. This is the
    /// entry point the wasm build uses: the browser passes config text through the
    /// API, so no `std::fs` touches the IG project. Native behavior is unchanged —
    /// `for_project_with` reads the file then delegates to the same core.
    pub fn for_project_with_config(
        source: impl PackageSource + 'static,
        cfg_text: &str,
        cache_dir: &str,
    ) -> anyhow::Result<Self> {
        let cfg = parse_config_text(cfg_text)?;
        Self::for_project_from_config(source, cfg, cache_dir)
    }

    fn for_project_from_config(
        source: impl PackageSource + 'static,
        cfg: ProjectConfig,
        cache_dir: &str,
    ) -> anyhow::Result<Self> {
        let cache = Path::new(cache_dir);
        if !source.is_dir(cache) {
            anyhow::bail!(
                "package_store: cache dir does not exist or is not a directory: {cache_dir}"
            );
        }
        let load_list = resolve_load_order(&source, &cfg, cache);

        let mut store = PackageStore {
            entries: Vec::new(),
            by_id: FxHashMap::default(),
            by_url: FxHashMap::default(),
            by_name: FxHashMap::default(),
            cache: std::cell::RefCell::new(FxHashMap::default()),
            source: Box::new(source),
        };
        let mut seq = 0usize;

        // Inject the bundled R5-in-R4 virtual package FIRST (lowest priority).
        for content in R5_FOR_R4_DEFS {
            let Ok(json): Result<Value, _> = serde_json::from_str(content) else {
                continue;
            };
            let str_field = |k: &str| json.get(k).and_then(|v| v.as_str()).map(str::to_string);
            let ie = IndexEntry {
                filename: String::new(),
                resource_type: str_field("resourceType"),
                id: str_field("id"),
                package_id: str_field("packageId"),
                url: str_field("url"),
                version: str_field("version"),
                kind: str_field("kind"),
                sd_type: str_field("type"),
                derivation: str_field("derivation"),
            };
            if classify(&ie).is_some() {
                let name = str_field("name");
                store.add_entry(ie, name, PathBuf::new(), Some(content), seq);
            }
            seq += 1;
        }

        for (id, version) in &load_list {
            let pkg_dir = cache.join(format!("{id}#{version}")).join("package");
            store.index_package(&pkg_dir, &mut seq);
        }
        Ok(store)
    }

    /// The storage backing this store (used for the lazy per-resource read).
    fn source(&self) -> &dyn PackageSource {
        self.source.as_ref()
    }

    /// Index a single resolved package directory, mirroring FPL's
    /// `loadResourcesFromCache` / `getPotentialResourcePaths`.
    ///
    /// **Stock behavior (the rule we match):** `fhir-package-loader` v2 (and SUSHI
    /// v3.20.0) **never read `package/.index.json`** — `getPotentialResourcePaths`
    /// scans the package directory for files matching `^[^.].*\.json$`, sorts them,
    /// and reads each one (`BasePackageLoader.loadResourcesFromCache`). The
    /// `.index.json` is a legacy artifact FPL ignores entirely.
    ///
    /// We keep `.index.json` as a metadata CACHE (to avoid re-parsing thousands of
    /// large core SDs) under a HEURISTIC: a NON-EMPTY index is trusted as complete
    /// and used directly (no directory scan); a MISSING or `files:[]` index triggers
    /// the FPL-style directory scan (sorted, read each file) to recover resources.
    /// See the body for why the heuristic is safe here and the TODO to remove the
    /// fallback once we control indexing ourselves. Both paths process files in
    /// sorted order, so load/seq order (LIFO fishing precedence) matches stock.
    fn index_package(&mut self, pkg_dir: &Path, seq: &mut usize) {
        // Compute the listing through the store's own source, then release that
        // borrow before mutating the lookup tables below.
        let listing = package_resource_listing(self.source.as_ref(), pkg_dir);
        for record in listing.records {
            if classify(&record.entry).is_some() {
                self.add_entry(
                    record.entry,
                    record.name,
                    record.path,
                    None,
                    *seq + record.ordinal,
                );
            }
        }
        *seq += listing.source_count;
    }

    /// Push one index entry into the resource table + lookup maps if it classifies
    /// as a fishable conformance type. Shared by the `.index.json` fast path, the
    /// directory-scan reconcile, and the bundled R5-in-R4 injection.
    fn add_entry(
        &mut self,
        ie: IndexEntry,
        name: Option<String>,
        path: PathBuf,
        embedded: Option<&'static str>,
        seq: usize,
    ) {
        let Some(fish_type) = classify(&ie) else {
            return;
        };
        let idx = self.entries.len();
        // Insert lookup keys (cloned — maps own their keys), then MOVE the
        // remaining owned fields into the entry to avoid per-field clones.
        let id = ie.id.unwrap_or_default();
        if !id.is_empty() {
            self.by_id.entry(id.clone()).or_default().push(idx);
        }
        if let Some(u) = &ie.url {
            self.by_url.entry(u.clone()).or_default().push(idx);
        }
        if let Some(n) = &name {
            self.by_name.entry(n.clone()).or_default().push(idx);
        }
        self.entries.push(ResEntry {
            seq,
            resource_type: ie.resource_type.unwrap_or_default(),
            id,
            url: ie.url,
            version: ie.version,
            sd_type: ie.sd_type,
            kind: ie.kind,
            name,
            fish_type,
            path,
            embedded,
        });
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

    /// Read+parse a resource file once, memoized (core SDs are fished hundreds of
    /// times during a build; re-parsing dominated wall time before this cache).
    fn read_value(&self, idx: usize) -> Option<std::rc::Rc<Value>> {
        if let Some(v) = self.cache.borrow().get(&idx) {
            return Some(v.clone());
        }
        let entry = &self.entries[idx];
        let v = if let Some(content) = entry.embedded {
            std::rc::Rc::new(serde_json::from_str::<Value>(content).ok()?)
        } else {
            let bytes = self.source().read(&entry.path).ok()?;
            std::rc::Rc::new(serde_json::from_slice::<Value>(&bytes).ok()?)
        };
        self.cache.borrow_mut().insert(idx, v.clone());
        Some(v)
    }

    /// `fishForFHIR(item, ...types)` — returns the full resource JSON.
    ///
    /// Returns the memoized `Rc<Value>` directly: callers almost always just read
    /// the SD (build a `StructureDefinition`, read fields) and discard, so the old
    /// per-fish deep clone of (large) base StructureDefinitions was pure waste.
    /// The rare caller that needs ownership clones at its own site.
    pub fn fish_for_fhir(&self, item: &str, types: &[FishType]) -> Option<std::rc::Rc<Value>> {
        let idx = self.resolve(item, types)?;
        self.read_value(idx)
    }

    /// `fishForMetadata(item, ...types)` — the `Metadata` object SUSHI emits
    /// (`convertInfoToMetadata`, FHIRDefinitions.ts:233-251). Key order and
    /// falsy-omission match the oracle.
    pub fn fish_for_metadata(&self, item: &str, types: &[FishType]) -> Option<Value> {
        let idx = self.resolve(item, types)?;
        // Read-only here (we only `.get(...)` fields), so hold the Rc, never clone.
        let value: Option<std::rc::Rc<Value>> = self.read_value(idx);
        let entry = &self.entries[idx];

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
            let chars = sd_characteristics(value.as_deref());
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

/// Resolve a project's requested external package load set in stock SUSHI load
/// order, without consulting a package cache.
///
/// The bundled `sushi-r5forR4#1.0.0` virtual package is intentionally omitted:
/// it is embedded in the Rust binary and is not acquired or materialized.
pub fn project_package_requests(ig_dir: &str) -> anyhow::Result<Vec<PackageRequest>> {
    let cfg = parse_config(ig_dir)?;
    Ok(
        resolve_load_order_with(&cfg, &|_id, ver| ver.map(str::to_string))
            .into_iter()
            .map(|(package_id, version)| PackageRequest {
                package_id,
                version,
            })
            .collect(),
    )
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
fn resolve_load_order(
    source: &dyn PackageSource,
    cfg: &ProjectConfig,
    cache: &Path,
) -> Vec<(String, String)> {
    resolve_load_order_with(cfg, &|id, ver| resolve_cached_version(source, cache, id, ver))
}

/// Build the ordered package load list using the supplied version resolver.
///
/// The resolver receives `(package_id, requested_version)` and returns the label
/// that should participate in duplicate suppression and loading.
fn resolve_load_order_with(
    cfg: &ProjectConfig,
    resolve_ver: &dyn Fn(&str, Option<&str>) -> Option<String>,
) -> Vec<(String, String)> {
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

    // -- Low automatic dependencies (before configured + core) ----------------
    auto_pass(
        false,
        fhir_name,
        &configured,
        resolve_ver,
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
        resolve_ver,
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

    fn unique_test_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{name}_{}_{}", std::process::id(), nanos))
    }

    #[test]
    fn version_ordering() {
        assert_eq!(version_cmp("7.2.0", "7.1.0"), Ordering::Greater);
        assert_eq!(version_cmp("5.3.0", "5.3.0-ballot-tc1"), Ordering::Greater);
        assert_eq!(version_cmp("1.1.2", "1.1.2"), Ordering::Equal);
    }

    #[test]
    fn minor_wildcard_resolves_highest_cached_patch() {
        let dir = unique_test_dir("pkgstore_wildcard_versions");
        for version in ["4.0.0", "4.0.2", "4.0.10", "4.1.0"] {
            std::fs::create_dir_all(dir.join(format!("ihe.iti.mcsd#{version}"))).unwrap();
        }

        assert_eq!(
            resolve_cached_version(&DiskSource, &dir, "ihe.iti.mcsd", Some("4.0.x")).as_deref(),
            Some("4.0.10")
        );
        assert_eq!(
            resolve_cached_version(&DiskSource, &dir, "ihe.iti.mcsd", Some("4.2.x")).as_deref(),
            Some("4.2.x")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn root_id_strip() {
        assert_eq!(
            root_id("hl7.fhir.uv.extensions.r4"),
            "hl7.fhir.uv.extensions"
        );
        assert_eq!(root_id("hl7.terminology.r5"), "hl7.terminology");
        assert_eq!(root_id("hl7.fhir.uv.ipa"), "hl7.fhir.uv.ipa");
    }

    #[test]
    fn project_requests_preserve_unresolved_sushi_order() {
        let cfg = ProjectConfig {
            fhir_version: "4.0.1".into(),
            dependencies: Vec::new(),
        };
        let got = resolve_load_order_with(&cfg, &|_id, ver| ver.map(str::to_string));
        assert_eq!(
            got,
            vec![
                ("hl7.fhir.uv.tools.r4".into(), "latest".into()),
                ("hl7.terminology.r4".into(), "latest".into()),
                ("hl7.fhir.r4.core".into(), "4.0.1".into()),
                ("hl7.fhir.uv.extensions.r4".into(), "latest".into()),
            ]
        );
    }

    fn empty_store() -> PackageStore {
        PackageStore {
            entries: Vec::new(),
            by_id: FxHashMap::default(),
            by_url: FxHashMap::default(),
            by_name: FxHashMap::default(),
            cache: std::cell::RefCell::new(FxHashMap::default()),
            source: Box::new(DiskSource),
        }
    }

    #[test]
    fn wildcard_dependency_loads_materialized_concrete_package() {
        let root = unique_test_dir("pkgstore_wildcard_project");
        let ig = root.join("ig");
        let cache = root.join("cache");
        let pkg = cache.join("ihe.iti.mcsd#4.0.0").join("package");
        std::fs::create_dir_all(&ig).unwrap();
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            ig.join("sushi-config.yaml"),
            r#"fhirVersion: 4.0.1
dependencies:
  ihe.iti.mcsd: 4.0.x
"#,
        )
        .unwrap();
        std::fs::write(
            pkg.join(".index.json"),
            r#"{"index-version":2,"files":[{"filename":"StructureDefinition-IHE.mCSD.OrganizationAffiliation.DocShare.json","resourceType":"StructureDefinition","id":"IHE.mCSD.OrganizationAffiliation.DocShare","url":"https://profiles.ihe.net/ITI/mCSD/StructureDefinition/IHE.mCSD.OrganizationAffiliation.DocShare","version":"4.0.0","kind":"resource","type":"OrganizationAffiliation","derivation":"constraint"}]}"#,
        )
        .unwrap();
        std::fs::write(
            pkg.join("StructureDefinition-IHE.mCSD.OrganizationAffiliation.DocShare.json"),
            r#"{"resourceType":"StructureDefinition","id":"IHE.mCSD.OrganizationAffiliation.DocShare","url":"https://profiles.ihe.net/ITI/mCSD/StructureDefinition/IHE.mCSD.OrganizationAffiliation.DocShare","version":"4.0.0","name":"IHE_mCSD_OrganizationAffiliation_DocShare","kind":"resource","type":"OrganizationAffiliation","derivation":"constraint"}"#,
        )
        .unwrap();

        let store = PackageStore::for_project(ig.to_str().unwrap(), cache.to_str().unwrap())
            .expect("wildcard dependency should load the concrete cached package");
        for q in [
            "IHE.mCSD.OrganizationAffiliation.DocShare",
            "IHE_mCSD_OrganizationAffiliation_DocShare",
            "https://profiles.ihe.net/ITI/mCSD/StructureDefinition/IHE.mCSD.OrganizationAffiliation.DocShare",
        ] {
            assert!(
                store.fish_for_fhir(q, &[FishType::Profile]).is_some(),
                "should fish wildcard-loaded profile by {q}"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A package whose `.index.json` is `files:[]` (or missing) must still have
    /// its on-disk resources indexed by the directory-scan reconcile — exactly as
    /// stock SUSHI / fhir-package-loader do (FPL never reads `.index.json`).
    #[test]
    fn empty_index_directory_fallback() {
        let dir = std::env::temp_dir().join(format!("pkgstore_emptyidx_{}", std::process::id()));
        let pkg = dir.join("package");
        std::fs::create_dir_all(&pkg).unwrap();
        // The bug repro: an empty index next to real resources on disk.
        std::fs::write(pkg.join(".index.json"), r#"{"index-version":2,"files":[]}"#).unwrap();
        std::fs::write(
            pkg.join("StructureDefinition-backport-subscription.json"),
            r#"{"resourceType":"StructureDefinition","id":"backport-subscription","url":"http://hl7.org/fhir/uv/subscriptions-backport/StructureDefinition/backport-subscription","name":"BackportSubscription","derivation":"constraint","kind":"resource","type":"Subscription"}"#,
        )
        .unwrap();
        // A non-fishable file (instance) and package.json must be ignored.
        std::fs::write(
            pkg.join("Patient-example.json"),
            r#"{"resourceType":"Patient","id":"example"}"#,
        )
        .unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name":"p","version":"1.0.0"}"#,
        )
        .unwrap();

        let mut store = empty_store();
        let mut seq = 0usize;
        store.index_package(&pkg, &mut seq);

        // The empty index yielded nothing; the scan recovered the profile.
        assert_eq!(
            store.entries.len(),
            1,
            "only the SD profile should be indexed"
        );
        for q in [
            "backport-subscription",
            "BackportSubscription",
            "http://hl7.org/fhir/uv/subscriptions-backport/StructureDefinition/backport-subscription",
        ] {
            let hit = store.fish_for_fhir(q, &[FishType::Profile]);
            assert!(hit.is_some(), "should fish {q} by id/name/url after scan");
        }

        // A completely missing `.index.json` must behave identically.
        std::fs::remove_file(pkg.join(".index.json")).unwrap();
        let mut store2 = empty_store();
        let mut seq2 = 0usize;
        store2.index_package(&pkg, &mut seq2);
        assert!(store2
            .fish_for_fhir("backport-subscription", &[FishType::Profile])
            .is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A complete `.index.json` must take the fast path unchanged: the scan adds
    /// nothing (only package.json is left over, and it is skipped).
    #[test]
    fn complete_index_no_double_index() {
        let dir = std::env::temp_dir().join(format!("pkgstore_fullidx_{}", std::process::id()));
        let pkg = dir.join("package");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join(".index.json"),
            r#"{"index-version":2,"files":[{"filename":"StructureDefinition-foo.json","resourceType":"StructureDefinition","id":"foo","url":"http://x/foo","version":"1.0.0","kind":"resource","type":"Patient","derivation":"constraint"}]}"#,
        )
        .unwrap();
        std::fs::write(
            pkg.join("StructureDefinition-foo.json"),
            r#"{"resourceType":"StructureDefinition","id":"foo","url":"http://x/foo","name":"Foo","derivation":"constraint","kind":"resource","type":"Patient"}"#,
        )
        .unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name":"p","version":"1.0.0"}"#,
        )
        .unwrap();

        let mut store = empty_store();
        let mut seq = 0usize;
        store.index_package(&pkg, &mut seq);
        assert_eq!(
            store.entries.len(),
            1,
            "no double-indexing from the reconcile"
        );
        assert!(store.fish_for_fhir("foo", &[FishType::Profile]).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn classify_types() {
        let sd = |kind: &str, ty: &str, der: Option<&str>| IndexEntry {
            filename: "f".into(),
            resource_type: Some("StructureDefinition".into()),
            id: Some("x".into()),
            package_id: None,
            url: None,
            version: None,
            kind: Some(String::from(kind)),
            sd_type: Some(String::from(ty)),
            derivation: der.map(String::from),
        };
        assert_eq!(
            classify(&sd("resource", "Observation", Some("specialization"))),
            Some(FishType::Resource)
        );
        assert_eq!(
            classify(&sd("complex-type", "Quantity", Some("specialization"))),
            Some(FishType::Type)
        );
        assert_eq!(
            classify(&sd("complex-type", "Extension", Some("constraint"))),
            Some(FishType::Extension)
        );
        assert_eq!(
            classify(&sd("resource", "Patient", Some("constraint"))),
            Some(FishType::Profile)
        );
        assert_eq!(
            classify(&sd("logical", "Foo", Some("specialization"))),
            Some(FishType::Logical)
        );
    }
}

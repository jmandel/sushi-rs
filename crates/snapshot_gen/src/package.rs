//! Package loading + resource index (`PackageContext`) with fetch/snapshot
//! memoization and version comparison helpers. Per-resource metadata (including
//! `name`) comes from the shared content-derived index
//! (`package_store::derived_index`); see docs/package-derived-index.md.

use anyhow::{bail, Context};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use package_store::{derived_index, DiskSource, PackageSource};

const SNAPSHOT_DEPENDENCY_MAX_FACTS: usize = 4_096;
const SNAPSHOT_DEPENDENCY_MAX_RETAINED_BYTES: usize = 1024 * 1024;

/// Exact observable PackageContext read made while generating one snapshot.
///
/// This is deliberately narrower than the package catalog: a successor may
/// reuse a snapshot only when every read the old walk actually made has the
/// same result in the freshly constructed context. Missing results are facts,
/// so a newly introduced local or package resource invalidates a prior miss.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SnapshotDependencyQuery {
    /// Raw query resolution happens before canonical loading and before the
    /// recursive generation cache probe. Preserve that observable ordering
    /// without treating the complete raw resource envelope as snapshot input.
    ResolveStructureDefinition(String),
    /// Result of the one canonical local/package StructureDefinition loader.
    LoadStructureDefinition(String),
    IsLocal(String),
    PackageId(String),
    CanonicalVersion {
        url: String,
        resource_type: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SnapshotDependencyOutcome {
    Resolution(Option<SnapshotResolutionIdentity>),
    LoadedStructureDefinition(SnapshotLoadedIdentity),
    Bool(bool),
    Text(Option<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SnapshotResolutionIdentity {
    source_proof: Option<[u8; 32]>,
    url: String,
    version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SnapshotLoadedIdentity {
    source_proof: Option<[u8; 32]>,
    outcome: SnapshotLoadedOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SnapshotLoadedOutcome {
    Loaded([u8; 32]),
    LoadFailed,
}

/// Private execution proof for one generated StructureDefinition snapshot.
///
/// The fields stay opaque outside snapshot_gen so callers cannot manufacture
/// partial evidence. An incomplete/overflowed manifest becomes an ineligible
/// history entry and can never authorize reuse.
#[derive(Clone, Debug)]
pub(crate) struct SnapshotDependencyManifest {
    reads: BTreeMap<SnapshotDependencyQuery, SnapshotDependencyOutcome>,
    complete: bool,
    retained_approx_bytes: usize,
}

impl SnapshotDependencyManifest {
    pub(crate) fn is_complete(&self) -> bool {
        self.complete
    }

    pub(crate) fn fact_count(&self) -> usize {
        self.reads.len()
    }

    /// Approximate logical bytes retained by the captured fact values. This is
    /// a deterministic admission weight, not allocator or process memory.
    pub(crate) fn retained_approx_bytes(&self) -> usize {
        self.retained_approx_bytes
    }
}

#[derive(Default, Debug)]
struct SnapshotDependencyTrace {
    reads: BTreeMap<SnapshotDependencyQuery, SnapshotDependencyOutcome>,
    complete: bool,
    retained_approx_bytes: usize,
}

impl SnapshotDependencyTrace {
    fn new() -> Self {
        Self {
            complete: true,
            ..Self::default()
        }
    }

    fn finish(self) -> SnapshotDependencyManifest {
        SnapshotDependencyManifest {
            reads: self.reads,
            complete: self.complete,
            retained_approx_bytes: self.retained_approx_bytes,
        }
    }
}

/// True iff the package's stock `.index.json` lists at least one
/// StructureDefinition. This is the exact trigger the old loader used
/// (`loaded == 0` after scanning `.index.json` for SD rows): a package whose
/// stock index is empty/SD-less takes the local full-conversion load path
/// (`local:true`), which several packages (e.g. subscriptions-backport.r4)
/// depend on for oracle parity. Reads only the small stock index, never the
/// resource files.
fn stock_index_lists_structure_definition(source: &dyn PackageSource, package_dir: &Path) -> bool {
    let Ok(bytes) = source.read(&package_dir.join(".index.json")) else {
        return false;
    };
    let Ok(index) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    index
        .get("files")
        .and_then(Value::as_array)
        .map(|files| {
            files.iter().any(|e| {
                e.get("resourceType").and_then(Value::as_str) == Some("StructureDefinition")
            })
        })
        .unwrap_or(false)
}

#[derive(Debug)]
pub struct PackageContext {
    by_url: HashMap<String, ResourceIndexEntry>,
    by_id: HashMap<String, PathBuf>,
    by_name: HashMap<String, PathBuf>,
    // Interior-mutability memoization. Equivalent to reading+parsing the resource
    // file on every call: the on-disk packages are immutable for the lifetime of a
    // run, so caching parsed values cannot change output — only avoid repeated
    // disk reads and JSON parses.
    fetch_cache: RefCell<HashMap<String, Option<Rc<Value>>>>,
    // Exact post-canonical-load structural digests keyed by the resolved resource
    // path. Snapshot manifests commonly read the same base; project the loaded
    // value once per PackageContext rather than once per manifest validation.
    loaded_sd_digests: RefCell<HashMap<PathBuf, [u8; 32]>>,
    // The storage backing package reads, held for the lazy per-resource `fetch`.
    // Native callers get a `DiskSource` (unchanged behavior); a browser/test caller
    // supplies a read-only in-memory source. Local-dir resources are always read
    // via `std::fs` (they are the native IG project, not the mounted cache).
    source: Box<dyn PackageSource>,
    // Parsed bodies of in-memory local resources (`load_local_resources`), keyed by
    // their synthetic path. There is no file behind these paths, so `fetch` reads
    // them here instead of from `source`. Empty for the disk path (which reads
    // local-dir files via `source`), so native behavior is untouched.
    in_memory_bodies: HashMap<PathBuf, Rc<Value>>,
    // The mounted package dirs (`<cache>/<pkg>/package`), in load order. Held ONLY
    // for the opt-in Layer-B canonical-version resolver, which needs to see
    // ValueSet/CodeSystem versions (Layer A indexes StructureDefinitions only).
    package_dirs: Vec<PathBuf>,
    // LAYER B (opt-in) canonical-version index: (resourceType, url) -> version,
    // built lazily on first `resolve_canonical_version` call from the derived
    // index (which lists EVERY resource, incl. VS/CS). Never consulted by Layer A
    // — `by_url` is unchanged, so the OFF path is byte-identical.
    canonical_versions: RefCell<Option<HashMap<(String, String), String>>>,
    // Present only around canonical derivation generation. Observation is
    // non-semantic: overflow or an internal inconsistency makes the resulting
    // manifest incomplete and therefore ineligible, but never fails generation.
    snapshot_dependencies: RefCell<Option<SnapshotDependencyTrace>>,
}

/// Raw, parsed StructureDefinition resolution retained only long enough to
/// preserve the canonical URL-before-load / generation-cache-before-load
/// ordering. Canonical conversion and package fixups happen exclusively in
/// [`PackageContext::load_snapshot_structure_definition`].
pub(crate) struct ResolvedSnapshotStructureDefinition {
    query: String,
    pub(crate) url: String,
    pub(crate) raw: Rc<Value>,
}

#[derive(Clone, Debug)]
pub(crate) struct ResourceIndexEntry {
    path: PathBuf,
    version: Option<String>,
    local: bool,
    /// The owning npm package id (e.g. `hl7.fhir.uv.extensions.r4`), mirroring
    /// Java's `PackageInformation.getId()`. `None` for local-dir resources (Java
    /// loads those outside the package loader, so `PackageHackerR5` never sees a
    /// package id for them). Derived from the cache path
    /// `.../packages/<id>#<ver>/package/<file>`.
    package_id: Option<String>,
}

impl PackageContext {
    /// Load packages from a disk cache (native behavior; unchanged).
    pub fn new(cache_dir: impl AsRef<Path>, packages: &[String]) -> anyhow::Result<Self> {
        Self::new_with(DiskSource, cache_dir, packages)
    }

    /// Same as [`PackageContext::new`] but reading every package file through an
    /// explicit [`PackageSource`] (browser bundle, test in-memory source). Local-dir
    /// resources loaded later via [`PackageContext::load_local_dir`] are still read
    /// from disk (they are the native IG project, not the mounted cache).
    pub fn new_with(
        source: impl PackageSource + 'static,
        cache_dir: impl AsRef<Path>,
        packages: &[String],
    ) -> anyhow::Result<Self> {
        let cache_dir = cache_dir.as_ref();
        if !source.is_dir(cache_dir) {
            bail!(
                "FHIR package cache is not a directory: {}",
                cache_dir.display()
            );
        }
        let mut ctx = Self {
            by_url: HashMap::new(),
            by_id: HashMap::new(),
            by_name: HashMap::new(),
            fetch_cache: RefCell::new(HashMap::new()),
            loaded_sd_digests: RefCell::new(HashMap::new()),
            source: Box::new(source),
            in_memory_bodies: HashMap::new(),
            package_dirs: Vec::new(),
            canonical_versions: RefCell::new(None),
            snapshot_dependencies: RefCell::new(None),
        };
        // SUSHI fishes these embedded definitions before ordinary packages.
        // Snapshot generation needs the same R5-only datatype definitions.
        // Direct inheritance from Base is handled separately by Publisher's
        // minimal, context-versioned synthetic Base (walk::resolve).
        ctx.load_sushi_r5_for_r4()?;
        for package in packages {
            ctx.load_package(cache_dir, package)?;
        }
        Ok(ctx)
    }

    fn load_sushi_r5_for_r4(&mut self) -> anyhow::Result<()> {
        for (index, content) in package_store::sushi_r5_for_r4_definitions()
            .iter()
            .enumerate()
        {
            let body: Value = serde_json::from_str(content)
                .with_context(|| format!("parse bundled sushi-r5forR4 definition {index}"))?;
            let id = body
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("definition");
            let path = PathBuf::from(format!(
                "__embedded/sushi-r5forR4#1.0.0/package/StructureDefinition-{id}.json"
            ));
            self.index_structure_definition(
                path.clone(),
                &body,
                false,
                Some("sushi-r5forR4".to_string()),
            );
            self.in_memory_bodies.insert(path, Rc::new(body));
        }
        Ok(())
    }

    fn load_package(&mut self, cache_dir: &Path, package: &str) -> anyhow::Result<()> {
        // Java's PackageInformation.getId() is the npm package name, i.e. the
        // part of `<id>#<version>` before the `#`.
        let package_id = package.split('#').next().unwrap_or(package).to_string();
        let package_dir = cache_dir.join(package).join("package");
        if !self.source.is_dir(&package_dir) {
            bail!(
                "package directory does not exist: {}",
                package_dir.display()
            );
        }
        // Remember the dir for the opt-in Layer-B canonical-version resolver.
        self.package_dirs.push(package_dir.clone());

        // Derived-columns index: one content-derived row per resource file
        // (filename/resourceType/id/url/version/kind/type/derivation/
        // baseDefinition/NAME), read from the `.derived-index.json` sidecar
        // (materialized once from the CAS) or built+cached once on first need for
        // non-CAS caches. This replaces the eager `.index.json` parse + per-file
        // `probe_name` + the `scan_package_structure_definitions` directory scan;
        // all three derived the same columns from immutable content every run.
        // See docs/package-derived-index.md.
        let rows = derived_index::load(self.source.as_ref(), &package_dir);

        // Preserve the exact legacy `local` semantics. Old behavior: SD rows that
        // the STOCK `.index.json` listed were loaded `local:false` (lenient R5
        // read); a package whose stock index listed ZERO StructureDefinitions
        // (empty/SD-less `.index.json`) fell into the scan fallback, which indexed
        // its SDs `local:true` (full R4->R5 conversion — subscriptions-backport.r4
        // etc. depend on this). The trigger is "did the stock index list any SD?"
        // — derived once here from the stock index, not the derived rows.
        let stock_index_has_sd =
            stock_index_lists_structure_definition(self.source.as_ref(), &package_dir);
        let local = !stock_index_has_sd;

        for row in &rows {
            if row.resource_type.as_deref() != Some("StructureDefinition") {
                continue;
            }
            let path = package_dir.join(&row.filename);
            if let Some(id) = &row.id {
                self.by_id.entry(id.clone()).or_insert_with(|| path.clone());
            }
            if let Some(url) = &row.url {
                let version = row.version.clone();
                self.insert_url(
                    url,
                    path.clone(),
                    version.clone(),
                    local,
                    Some(package_id.clone()),
                );
                if let Some(version) = &row.version {
                    self.by_url.insert(
                        format!("{url}|{version}"),
                        ResourceIndexEntry {
                            path: path.clone(),
                            version: Some(version.clone()),
                            local,
                            package_id: Some(package_id.clone()),
                        },
                    );
                }
            }
            if let Some(name) = &row.name {
                self.by_name
                    .entry(name.clone())
                    .or_insert_with(|| path.clone());
            }
        }
        Ok(())
    }

    pub fn load_local_dir(&mut self, dir: impl AsRef<Path>) -> anyhow::Result<()> {
        let dir = dir.as_ref();
        if !dir.is_dir() {
            bail!(
                "local resource directory is not a directory: {}",
                dir.display()
            );
        }
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("cannot read local resource directory {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                files.push(path);
            }
        }
        files.sort();
        for path in files {
            let Ok(json) = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .ok_or(())
            else {
                continue;
            };
            if json.get("resourceType").and_then(Value::as_str) == Some("StructureDefinition") {
                self.index_structure_definition(path, &json, true, None);
            }
        }
        Ok(())
    }

    /// In-memory sibling of [`PackageContext::load_local_dir`] (no `std::fs`):
    /// index already-parsed local resources — the wasm build feeds the compiled
    /// IG resources (from `compiler::build_project_in_memory`) here so a profile's
    /// sibling bases resolve exactly as `--local-dir` does natively. `entries` is
    /// `(synthetic path, body)`; the caller MUST pass them sorted by path (the
    /// disk path sorts filenames) so `by_url`/`by_id`/`by_name` last-writer-wins
    /// resolution is identical. Only `StructureDefinition`s are indexed (same as
    /// the disk path); each is `local:true`, `package_id:None`.
    pub fn load_local_resources<I>(&mut self, entries: I)
    where
        I: IntoIterator<Item = (PathBuf, Value)>,
    {
        for (path, json) in entries {
            if json.get("resourceType").and_then(Value::as_str) == Some("StructureDefinition") {
                // Index exactly as the disk path does (same `local:true`, same
                // `insert_url` version-precedence), then stash the parsed body under
                // the synthetic path so `fetch_rc` serves it without a `source` read.
                self.index_structure_definition(path.clone(), &json, true, None);
                self.in_memory_bodies.insert(path, Rc::new(json));
            }
        }
    }

    fn index_structure_definition(
        &mut self,
        path: PathBuf,
        json: &Value,
        local: bool,
        package_id: Option<String>,
    ) {
        if let Some(id) = json.get("id").and_then(Value::as_str) {
            self.by_id.insert(id.to_string(), path.clone());
        }
        if let Some(url) = json.get("url").and_then(Value::as_str) {
            let version = json
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string);
            self.insert_url(
                url,
                path.clone(),
                version.clone(),
                local,
                package_id.clone(),
            );
            if let Some(version) = version {
                self.by_url.insert(
                    format!("{url}|{version}"),
                    ResourceIndexEntry {
                        path: path.clone(),
                        version: Some(version),
                        local,
                        package_id: package_id.clone(),
                    },
                );
            }
        }
        if let Some(name) = json.get("name").and_then(Value::as_str) {
            self.by_name.insert(name.to_string(), path);
        }
    }

    fn insert_url(
        &mut self,
        url: &str,
        path: PathBuf,
        version: Option<String>,
        local: bool,
        package_id: Option<String>,
    ) {
        let replace = match self.by_url.get(url) {
            Some(existing) => match (&version, &existing.version) {
                (Some(new), Some(old)) if new != old => later_version(new, old),
                _ => local || !existing.local,
            },
            None => true,
        };
        if replace {
            self.by_url.insert(
                url.to_string(),
                ResourceIndexEntry {
                    path,
                    version,
                    local,
                    package_id,
                },
            );
        }
    }

    pub(crate) fn is_local(&self, query: &str) -> bool {
        let outcome = self.is_local_unobserved(query);
        if self.snapshot_dependency_capture_active() {
            self.observe_snapshot_dependency(
                SnapshotDependencyQuery::IsLocal(query.to_string()),
                SnapshotDependencyOutcome::Bool(outcome),
            );
        }
        outcome
    }

    fn is_local_unobserved(&self, query: &str) -> bool {
        self.by_url
            .get(query)
            .map(|entry| entry.local)
            .unwrap_or(false)
    }

    /// The owning npm package id for the resource resolved by `query`, mirroring
    /// Java's `PackageInformation.getId()`. Resolves by url first, then falls back
    /// to matching the resolved path (id/name lookups) to a `by_url` entry.
    /// `None` for local-dir resources or unresolved queries.
    pub(crate) fn package_id_for(&self, query: &str) -> Option<String> {
        let outcome = self.package_id_for_unobserved(query);
        if self.snapshot_dependency_capture_active() {
            self.observe_snapshot_dependency(
                SnapshotDependencyQuery::PackageId(query.to_string()),
                SnapshotDependencyOutcome::Text(outcome.clone()),
            );
        }
        outcome
    }

    fn package_id_for_unobserved(&self, query: &str) -> Option<String> {
        if let Some(entry) = self.by_url.get(query) {
            entry.package_id.clone()
        } else {
            self.resource_path(query).and_then(|path| {
                self.by_url
                    .values()
                    .find(|entry| &entry.path == path)
                    .and_then(|entry| entry.package_id.clone())
            })
        }
    }

    /// Fetch the memoized raw parsed resource for non-walk callers. Snapshot
    /// generation uses the typed resolve/load methods below so its manifest can
    /// distinguish pre-load URL resolution from canonical loaded semantics.
    pub fn fetch(&self, query: &str) -> Option<Rc<Value>> {
        self.fetch_rc(query)
    }

    /// Resolve a raw StructureDefinition and record only the pre-load facts the
    /// canonical walk observes before conversion: missing/present selection,
    /// canonical URL, and version. The returned raw value is intentionally
    /// opaque outside this crate and cannot authorize reuse by itself.
    pub(crate) fn resolve_snapshot_structure_definition(
        &self,
        query: &str,
    ) -> Option<ResolvedSnapshotStructureDefinition> {
        self.resolve_snapshot_structure_definition_impl(query, true)
    }

    fn resolve_snapshot_structure_definition_impl(
        &self,
        query: &str,
        observe: bool,
    ) -> Option<ResolvedSnapshotStructureDefinition> {
        let raw = self.fetch_rc(query);
        if observe {
            self.observe_snapshot_resolution(query, raw.as_deref());
        }
        let raw = raw?;
        let url = raw
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or(query)
            .to_string();
        Some(ResolvedSnapshotStructureDefinition {
            query: query.to_string(),
            url,
            raw,
        })
    }

    /// Run the one canonical StructureDefinition load path. Local definitions
    /// receive full current conversion; package definitions receive the
    /// Publisher-compatible lenient read; successful package loads then receive
    /// the same package fixups. Capture records only the post-load structural
    /// projection (or a distinct load failure), never a raw-field blacklist.
    pub(crate) fn load_snapshot_structure_definition(
        &self,
        resolved: &ResolvedSnapshotStructureDefinition,
    ) -> anyhow::Result<Value> {
        self.load_snapshot_structure_definition_impl(resolved, true)
    }

    fn load_snapshot_structure_definition_impl(
        &self,
        resolved: &ResolvedSnapshotStructureDefinition,
        observe: bool,
    ) -> anyhow::Result<Value> {
        let local = if observe {
            self.is_local(&resolved.url) || self.is_local(&resolved.query)
        } else {
            self.is_local_unobserved(&resolved.url) || self.is_local_unobserved(&resolved.query)
        };
        let converted = if local {
            crate::sd_load::to_r5_internal(resolved.raw.as_ref())
        } else {
            Ok(crate::sd_load::lenient_r5_read_r4(resolved.raw.as_ref()))
        };
        let mut loaded = match converted {
            Ok(loaded) => loaded,
            Err(error) => {
                if observe {
                    self.observe_snapshot_loaded(
                        &resolved.query,
                        SnapshotLoadedOutcome::LoadFailed,
                    );
                }
                return Err(error);
            }
        };
        let package_id = if observe {
            self.package_id_for(&resolved.url)
                .or_else(|| self.package_id_for(&resolved.query))
        } else {
            self.package_id_for_unobserved(&resolved.url)
                .or_else(|| self.package_id_for_unobserved(&resolved.query))
        };
        crate::sd_load::fix_loaded_resource(&mut loaded, package_id.as_deref());
        if observe && self.snapshot_dependency_capture_active() {
            let digest = self.loaded_sd_digest(&resolved.query, &loaded);
            self.observe_snapshot_loaded(&resolved.query, SnapshotLoadedOutcome::Loaded(digest));
        }
        Ok(loaded)
    }

    /// LAYER B (opt-in): resolve the `version` of the canonical `url` when it
    /// resolves to a resource of type `resource_type` in the loaded context —
    /// mirroring Java's type-scoped `context.fetchResource(X.class, url)` used by
    /// `CoreVersionPinner`. Unlike [`fetch`], this sees ValueSets/CodeSystems too
    /// (Layer A indexes only StructureDefinitions). Returns `None` when the target
    /// is absent, is a different resource type, or has no non-empty `version`.
    ///
    /// Built lazily + memoized from the derived index the first time it is called,
    /// so a build that never opts into Layer B pays nothing. Local-dir resources
    /// (loaded after packages) are consulted via `by_url` first so a local VS/SD
    /// overrides a package one, matching load precedence.
    pub fn resolve_canonical_version(&self, url: &str, resource_type: &str) -> Option<String> {
        // Fast path: a locally-loaded resource indexed in by_url whose file we can
        // parse (covers local-dir SDs; VS/CS locals fall through to the index).
        if let Some(entry) = self.by_url.get(url) {
            if let Some(v) = entry.version.as_deref().filter(|s| !s.is_empty()) {
                // Confirm the resource type matches (by_url only holds SDs today, so
                // this guards against a future VS being added there).
                if resource_type == "StructureDefinition" {
                    let outcome = Some(v.to_string());
                    if self.snapshot_dependency_capture_active() {
                        self.observe_snapshot_dependency(
                            SnapshotDependencyQuery::CanonicalVersion {
                                url: url.to_string(),
                                resource_type: resource_type.to_string(),
                            },
                            SnapshotDependencyOutcome::Text(outcome.clone()),
                        );
                    }
                    return outcome;
                }
            }
        }
        self.ensure_canonical_index();
        let outcome = self
            .canonical_versions
            .borrow()
            .as_ref()
            .and_then(|m| {
                m.get(&(resource_type.to_string(), url.to_string()))
                    .cloned()
            })
            .filter(|v| !v.is_empty());
        if self.snapshot_dependency_capture_active() {
            self.observe_snapshot_dependency(
                SnapshotDependencyQuery::CanonicalVersion {
                    url: url.to_string(),
                    resource_type: resource_type.to_string(),
                },
                SnapshotDependencyOutcome::Text(outcome.clone()),
            );
        }
        outcome
    }

    /// Build the (resourceType, url) -> version index from every loaded package's
    /// derived index (all resource types). Idempotent; memoized.
    fn ensure_canonical_index(&self) {
        if self.canonical_versions.borrow().is_some() {
            return;
        }
        let mut map: HashMap<(String, String), String> = HashMap::new();
        for package_dir in &self.package_dirs {
            let rows = derived_index::load(self.source.as_ref(), package_dir);
            for row in &rows {
                let (Some(rt), Some(url), Some(version)) =
                    (&row.resource_type, &row.url, &row.version)
                else {
                    continue;
                };
                if version.is_empty() {
                    continue;
                }
                // First loaded wins (package load order), matching by_url's
                // insert precedence for the common single-version core case.
                map.entry((rt.clone(), url.clone()))
                    .or_insert_with(|| version.clone());
            }
        }
        *self.canonical_versions.borrow_mut() = Some(map);
    }

    // Memoized parse of the resource file for `query`. Returns the shared parsed
    // value (or `None` if unresolved / unreadable), caching both outcomes so
    // repeated lookups do not re-read or re-parse the immutable package files.
    fn fetch_rc(&self, query: &str) -> Option<Rc<Value>> {
        if let Some(cached) = self.fetch_cache.borrow().get(query) {
            return cached.clone();
        }
        let path = self.resource_path(query).cloned();
        // In-memory local resources have no file behind their synthetic path; serve
        // the stashed parsed body. Everything else reads through `source` (disk or
        // bundle), byte-for-byte as before.
        let parsed = path
            .as_deref()
            .and_then(|p| self.in_memory_bodies.get(p))
            .cloned()
            .or_else(|| {
                path.as_deref()
                    .and_then(|p| self.source.read(p).ok())
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    .map(Rc::new)
            });
        self.fetch_cache
            .borrow_mut()
            .insert(query.to_string(), parsed.clone());
        parsed
    }

    fn resource_path(&self, query: &str) -> Option<&PathBuf> {
        self.by_url
            .get(query)
            .map(|e| &e.path)
            .or_else(|| self.by_id.get(query))
            .or_else(|| self.by_name.get(query))
    }

    pub(crate) fn begin_snapshot_dependency_capture(&self) {
        *self.snapshot_dependencies.borrow_mut() = Some(SnapshotDependencyTrace::new());
    }

    pub(crate) fn finish_snapshot_dependency_capture(&self) -> SnapshotDependencyManifest {
        self.snapshot_dependencies
            .borrow_mut()
            .take()
            .unwrap_or_else(|| {
                let mut trace = SnapshotDependencyTrace::new();
                trace.complete = false;
                trace
            })
            .finish()
    }

    /// Revalidate a prior manifest against this newly constructed context.
    /// No reads are added to an active capture while validating.
    pub(crate) fn matches_snapshot_dependencies(
        &self,
        manifest: &SnapshotDependencyManifest,
    ) -> bool {
        manifest.complete
            && manifest
                .reads
                .iter()
                .all(|(query, expected)| self.matches_snapshot_dependency(query, expected))
    }

    fn matches_snapshot_dependency(
        &self,
        query: &SnapshotDependencyQuery,
        expected: &SnapshotDependencyOutcome,
    ) -> bool {
        match (query, expected) {
            (
                SnapshotDependencyQuery::ResolveStructureDefinition(query),
                SnapshotDependencyOutcome::Resolution(expected),
            ) => return self.matches_snapshot_resolution(query, expected.as_ref()),
            (
                SnapshotDependencyQuery::LoadStructureDefinition(query),
                SnapshotDependencyOutcome::LoadedStructureDefinition(expected),
            ) => return self.matches_snapshot_loaded(query, expected),
            _ => {}
        }
        self.snapshot_dependency_outcome(query)
            .as_ref()
            .is_some_and(|actual| actual == expected)
    }

    fn matches_snapshot_resolution(
        &self,
        query: &str,
        expected: Option<&SnapshotResolutionIdentity>,
    ) -> bool {
        let current_proof = self.snapshot_source_proof(query);
        if let Some(expected) = expected {
            if expected.source_proof.is_some() && current_proof.is_none() {
                return false;
            }
            if expected.source_proof.is_some() && expected.source_proof == current_proof {
                return true;
            }
            return self.fetch_rc(query).as_deref().is_some_and(|raw| {
                let actual = self.snapshot_resolution_identity(query, raw);
                actual.url == expected.url && actual.version == expected.version
            });
        }
        self.fetch_rc(query).is_none()
    }

    fn matches_snapshot_loaded(&self, query: &str, expected: &SnapshotLoadedIdentity) -> bool {
        let current_proof = self.snapshot_source_proof(query);
        if expected.source_proof.is_some() && current_proof.is_none() {
            return false;
        }
        if expected.source_proof.is_some() && expected.source_proof == current_proof {
            return true;
        }
        let Some(resolved) = self.resolve_snapshot_structure_definition_impl(query, false) else {
            return false;
        };
        let actual = match self.load_snapshot_structure_definition_impl(&resolved, false) {
            Ok(loaded) => SnapshotLoadedOutcome::Loaded(self.loaded_sd_digest(query, &loaded)),
            Err(_) => SnapshotLoadedOutcome::LoadFailed,
        };
        actual == expected.outcome
    }

    fn snapshot_dependency_outcome(
        &self,
        query: &SnapshotDependencyQuery,
    ) -> Option<SnapshotDependencyOutcome> {
        Some(match query {
            SnapshotDependencyQuery::ResolveStructureDefinition(_)
            | SnapshotDependencyQuery::LoadStructureDefinition(_) => return None,
            SnapshotDependencyQuery::IsLocal(query) => SnapshotDependencyOutcome::Bool(
                self.by_url
                    .get(query)
                    .map(|entry| entry.local)
                    .unwrap_or(false),
            ),
            SnapshotDependencyQuery::PackageId(query) => {
                let package_id = if let Some(entry) = self.by_url.get(query) {
                    entry.package_id.clone()
                } else {
                    self.resource_path(query).and_then(|path| {
                        self.by_url
                            .values()
                            .find(|entry| &entry.path == path)
                            .and_then(|entry| entry.package_id.clone())
                    })
                };
                SnapshotDependencyOutcome::Text(package_id)
            }
            SnapshotDependencyQuery::CanonicalVersion { url, resource_type } => {
                let version = if resource_type == "StructureDefinition" {
                    self.by_url
                        .get(url)
                        .and_then(|entry| entry.version.as_deref())
                        .filter(|version| !version.is_empty())
                        .map(str::to_string)
                } else {
                    None
                };
                let version = version.or_else(|| {
                    self.ensure_canonical_index();
                    self.canonical_versions.borrow().as_ref().and_then(|index| {
                        index
                            .get(&(resource_type.clone(), url.clone()))
                            .cloned()
                            .filter(|version| !version.is_empty())
                    })
                });
                SnapshotDependencyOutcome::Text(version)
            }
        })
    }

    fn observe_snapshot_dependency(
        &self,
        query: SnapshotDependencyQuery,
        outcome: SnapshotDependencyOutcome,
    ) {
        let mut capture = self.snapshot_dependencies.borrow_mut();
        let Some(trace) = capture.as_mut() else {
            return;
        };
        if !trace.complete {
            return;
        }
        if let Some(existing) = trace.reads.get(&query) {
            if existing != &outcome {
                trace.complete = false;
                trace.reads.clear();
                trace.retained_approx_bytes = 0;
            }
            return;
        }
        let retained_approx_bytes = snapshot_dependency_retained_approx_bytes(&query, &outcome);
        if trace.reads.len() >= SNAPSHOT_DEPENDENCY_MAX_FACTS
            || trace
                .retained_approx_bytes
                .saturating_add(retained_approx_bytes)
                > SNAPSHOT_DEPENDENCY_MAX_RETAINED_BYTES
        {
            trace.complete = false;
            trace.reads.clear();
            trace.retained_approx_bytes = 0;
            return;
        }
        trace.retained_approx_bytes += retained_approx_bytes;
        trace.reads.insert(query, outcome);
    }

    fn observe_snapshot_resolution(&self, query: &str, outcome: Option<&Value>) {
        let dependency_query =
            SnapshotDependencyQuery::ResolveStructureDefinition(query.to_string());
        let needs_identity = self
            .snapshot_dependencies
            .borrow()
            .as_ref()
            .is_some_and(|trace| trace.complete && !trace.reads.contains_key(&dependency_query));
        if !needs_identity {
            return;
        }
        let outcome = SnapshotDependencyOutcome::Resolution(
            outcome.map(|value| self.snapshot_resolution_identity(query, value)),
        );
        self.observe_snapshot_dependency(dependency_query, outcome);
    }

    fn observe_snapshot_loaded(&self, query: &str, outcome: SnapshotLoadedOutcome) {
        self.observe_snapshot_dependency(
            SnapshotDependencyQuery::LoadStructureDefinition(query.to_string()),
            SnapshotDependencyOutcome::LoadedStructureDefinition(SnapshotLoadedIdentity {
                source_proof: self.snapshot_source_proof(query),
                outcome,
            }),
        );
    }

    fn snapshot_dependency_capture_active(&self) -> bool {
        self.snapshot_dependencies
            .borrow()
            .as_ref()
            .is_some_and(|trace| trace.complete)
    }

    fn loaded_sd_digest(&self, query: &str, value: &Value) -> [u8; 32] {
        let Some(path) = self.resource_path(query) else {
            return crate::sd_load::snapshot_dependency_digest(value);
        };
        if let Some(digest) = self.loaded_sd_digests.borrow().get(path) {
            return *digest;
        }
        let digest = crate::sd_load::snapshot_dependency_digest(value);
        self.loaded_sd_digests
            .borrow_mut()
            .insert(path.clone(), digest);
        digest
    }

    fn snapshot_source_proof(&self, query: &str) -> Option<[u8; 32]> {
        self.resource_path(query)
            .and_then(|path| self.source.immutable_content_identity(path))
    }

    fn snapshot_resolution_identity(
        &self,
        query: &str,
        value: &Value,
    ) -> SnapshotResolutionIdentity {
        SnapshotResolutionIdentity {
            source_proof: self.snapshot_source_proof(query),
            url: value
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or(query)
                .to_string(),
            version: value
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string),
        }
    }
}

fn snapshot_dependency_retained_approx_bytes(
    query: &SnapshotDependencyQuery,
    outcome: &SnapshotDependencyOutcome,
) -> usize {
    let query_bytes = match query {
        SnapshotDependencyQuery::ResolveStructureDefinition(query)
        | SnapshotDependencyQuery::LoadStructureDefinition(query)
        | SnapshotDependencyQuery::IsLocal(query)
        | SnapshotDependencyQuery::PackageId(query) => query.len(),
        SnapshotDependencyQuery::CanonicalVersion { url, resource_type } => {
            url.len().saturating_add(resource_type.len())
        }
    };
    let outcome_bytes = match outcome {
        SnapshotDependencyOutcome::Resolution(Some(identity)) => 34usize
            .saturating_add(identity.url.len())
            .saturating_add(identity.version.as_ref().map_or(0, String::len)),
        SnapshotDependencyOutcome::Resolution(None) | SnapshotDependencyOutcome::Bool(_) => 1,
        SnapshotDependencyOutcome::LoadedStructureDefinition(_) => 34,
        SnapshotDependencyOutcome::Text(value) => value
            .as_ref()
            .map_or(1, |value| value.len().saturating_add(1)),
    };
    std::mem::size_of::<SnapshotDependencyQuery>()
        .saturating_add(std::mem::size_of::<SnapshotDependencyOutcome>())
        .saturating_add(query_bytes)
        .saturating_add(outcome_bytes)
}

pub(crate) fn later_version(new: &str, old: &str) -> bool {
    let new_parts = version_parts(new);
    let old_parts = version_parts(old);
    let max = new_parts.len().max(old_parts.len());
    for i in 0..max {
        let n = new_parts.get(i);
        let o = old_parts.get(i);
        match (n, o) {
            (Some(VersionPart::Number(n)), Some(VersionPart::Number(o))) if n != o => return n > o,
            (Some(VersionPart::Text(n)), Some(VersionPart::Text(o))) if n != o => return n > o,
            (Some(VersionPart::Number(_)), Some(VersionPart::Text(_))) => return true,
            (Some(VersionPart::Text(_)), Some(VersionPart::Number(_))) => return false,
            (Some(VersionPart::Number(n)), None) => return *n > 0,
            (Some(VersionPart::Text(_)), None) => return false,
            (None, Some(VersionPart::Number(o))) => return *o == 0,
            (None, Some(VersionPart::Text(_))) => return true,
            _ => {}
        }
    }
    false
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum VersionPart {
    Number(u64),
    Text(String),
}

pub(crate) fn version_parts(version: &str) -> Vec<VersionPart> {
    version
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|part| {
            part.parse::<u64>()
                .map(VersionPart::Number)
                .unwrap_or_else(|_| VersionPart::Text(part.to_ascii_lowercase()))
        })
        .collect()
}

#[cfg(test)]
mod dependency_manifest_tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct ImmutableTestSource {
        inner: package_store::BundleSource,
        identities: BTreeMap<PathBuf, [u8; 32]>,
    }

    impl package_store::PackageSource for ImmutableTestSource {
        fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
            self.inner.read(path)
        }

        fn immutable_content_identity(&self, path: &Path) -> Option<[u8; 32]> {
            self.identities.get(path).copied()
        }

        fn read_dir(&self, path: &Path) -> std::io::Result<Vec<package_store::DirEntry>> {
            self.inner.read_dir(path)
        }

        fn exists(&self, path: &Path) -> bool {
            self.inner.exists(path)
        }

        fn is_file(&self, path: &Path) -> bool {
            self.inner.is_file(path)
        }

        fn is_dir(&self, path: &Path) -> bool {
            self.inner.is_dir(path)
        }

        fn fork_read_cache(&self) -> std::io::Result<Box<dyn package_store::PackageSource>> {
            Ok(Box::new(Self {
                inner: self.inner.fork_read_cache(),
                identities: self.identities.clone(),
            }))
        }
    }

    fn prepared_package_context(base_min: u64) -> (ImmutableTestSource, PackageContext) {
        prepared_package_context_with_base(base(base_min))
    }

    fn prepared_package_context_with_base(base: Value) -> (ImmutableTestSource, PackageContext) {
        use sha2::{Digest, Sha256};

        let package_label = "demo.base#1.0.0";
        let base_bytes = serde_json::to_vec(&base).unwrap();
        let prepared = package_store::PreparedPackage::prepare(
            package_label,
            BTreeMap::from([
                (
                    "package.json".to_string(),
                    br#"{"name":"demo.base","version":"1.0.0"}"#.to_vec(),
                ),
                (
                    ".index.json".to_string(),
                    serde_json::to_vec(&serde_json::json!({
                        "index-version": 2,
                        "files": [{
                            "filename":"StructureDefinition-LocalBase.json",
                            "resourceType":"StructureDefinition",
                            "id":"LocalBase",
                            "url":"https://example.org/StructureDefinition/LocalBase",
                            "version":"1.0.0",
                            "kind":"resource",
                            "type":"Patient",
                            "derivation":"constraint"
                        }]
                    }))
                    .unwrap(),
                ),
                (
                    "StructureDefinition-LocalBase.json".to_string(),
                    base_bytes.clone(),
                ),
            ]),
        )
        .unwrap();
        let encoded = prepared.encode();
        let decoded =
            package_store::PreparedPackage::decode_expected(&encoded, &prepared.key).unwrap();
        let mut source = package_store::BundleSource::new();
        decoded.mount_into(&mut source);
        let root = source.cache_root().to_path_buf();
        let base_path = root
            .join(package_label)
            .join("package/StructureDefinition-LocalBase.json");
        let source = ImmutableTestSource {
            inner: source,
            identities: BTreeMap::from([(base_path, Sha256::digest(&base_bytes).into())]),
        };
        let observer = source.clone();
        let context = PackageContext::new_with(source, root, &[package_label.to_string()]).unwrap();
        (observer, context)
    }

    fn context(resources: Vec<(PathBuf, Value)>) -> PackageContext {
        let cache = tempfile::tempdir().unwrap();
        let mut context = PackageContext::new(cache.path(), &[]).unwrap();
        context.load_local_resources(resources);
        context
    }

    fn generate_with_manifest(
        input: Value,
        context: &PackageContext,
    ) -> anyhow::Result<(Value, SnapshotDependencyManifest)> {
        let prepared = crate::prepare_snapshot_derivation(input, Default::default())?;
        context.begin_snapshot_dependency_capture();
        let generated = crate::walk::generate_prepared_snapshot(prepared, context);
        let manifest = context.finish_snapshot_dependency_capture();
        generated.map(|generated| (generated, manifest))
    }

    fn base(min: u64) -> Value {
        serde_json::json!({
            "resourceType": "StructureDefinition",
            "id": "LocalBase",
            "url": "https://example.org/StructureDefinition/LocalBase",
            "version": "1.0.0",
            "name": "LocalBase",
            "status": "draft",
            "fhirVersion": "5.0.0",
            "kind": "resource",
            "abstract": false,
            "type": "Patient",
            "derivation": "constraint",
            "snapshot": { "element": [{
                "id": "Patient",
                "path": "Patient",
                "min": min,
                "max": "*",
                "base": { "path": "Patient", "min": 0, "max": "*" }
            }] },
            "differential": { "element": [{
                "id": "Patient",
                "path": "Patient",
                "min": min,
                "max": "*"
            }] }
        })
    }

    fn derived() -> Value {
        serde_json::json!({
            "resourceType": "StructureDefinition",
            "id": "Derived",
            "url": "https://example.org/StructureDefinition/Derived",
            "version": "1.0.0",
            "name": "Derived",
            "status": "draft",
            "fhirVersion": "5.0.0",
            "kind": "resource",
            "abstract": false,
            "type": "Patient",
            "baseDefinition": "https://example.org/StructureDefinition/LocalBase",
            "derivation": "constraint",
            "differential": { "element": [{
                "id": "Patient",
                "path": "Patient"
            }] }
        })
    }

    #[test]
    fn generated_snapshot_manifest_revalidates_and_tracks_transitive_base_content() {
        let input = derived();
        let original = context(vec![
            (PathBuf::from("a-base.json"), base(0)),
            (PathBuf::from("b-derived.json"), input.clone()),
        ]);
        let (_, manifest) = generate_with_manifest(input.clone(), &original).unwrap();
        assert!(manifest.is_complete());
        assert!(manifest.fact_count() > 0);
        assert!(original.matches_snapshot_dependencies(&manifest));

        let identical = context(vec![
            (PathBuf::from("a-base.json"), base(0)),
            (PathBuf::from("b-derived.json"), input.clone()),
        ]);
        assert!(identical.matches_snapshot_dependencies(&manifest));

        let changed_base = context(vec![
            (PathBuf::from("a-base.json"), base(1)),
            (PathBuf::from("b-derived.json"), input.clone()),
        ]);
        assert!(!changed_base.matches_snapshot_dependencies(&manifest));

        let mut metadata_only_base = base(0);
        metadata_only_base["title"] = Value::String("Current title".into());
        metadata_only_base["status"] = Value::String("active".into());
        metadata_only_base["description"] = Value::String("Current description".into());
        metadata_only_base["publisher"] = Value::String("Current publisher".into());
        let metadata_only = context(vec![
            (PathBuf::from("a-base.json"), metadata_only_base),
            (PathBuf::from("b-derived.json"), input.clone()),
        ]);
        assert!(
            metadata_only.matches_snapshot_dependencies(&manifest),
            "top-level envelope metadata is not a snapshot input"
        );

        let mut contact_changed_base = base(0);
        contact_changed_base["contact"] = serde_json::json!([{
            "name":"Changed contact",
            "extension":[{"url":"https://example.org/unsupported","valueFoo":true}]
        }]);
        let contact_changed = context(vec![
            (PathBuf::from("a-base.json"), contact_changed_base),
            (PathBuf::from("b-derived.json"), input.clone()),
        ]);
        assert!(
            contact_changed.matches_snapshot_dependencies(&manifest),
            "successfully loaded metadata outside the structural projection is not a snapshot input"
        );

        let mut malformed_r4_base = base(0);
        malformed_r4_base["fhirVersion"] = Value::String("4.0.1".into());
        malformed_r4_base["contact"] = serde_json::json!([{
            "name":"Malformed contact",
            "extension":[{"url":"https://example.org/unsupported","valueFoo":true}]
        }]);
        let mut r4_input = input.clone();
        r4_input["fhirVersion"] = Value::String("4.0.1".into());
        let mut valid_r4_base = base(0);
        valid_r4_base["fhirVersion"] = Value::String("4.0.1".into());
        let valid_r4_context = context(vec![
            (PathBuf::from("a-base.json"), valid_r4_base),
            (PathBuf::from("b-derived.json"), r4_input.clone()),
        ]);
        let (_, valid_r4_manifest) = generate_with_manifest(r4_input.clone(), &valid_r4_context)
            .expect("valid local R4 base loads canonically");
        let malformed_context = context(vec![
            (PathBuf::from("a-base.json"), malformed_r4_base),
            (PathBuf::from("b-derived.json"), r4_input.clone()),
        ]);
        assert!(
            !malformed_context.matches_snapshot_dependencies(&valid_r4_manifest),
            "a prior successful load cannot be reused across a current conversion failure"
        );
        let error = crate::generate_snapshot(r4_input, &malformed_context, Default::default())
            .expect_err("canonical fallback must still execute dependency conversion");
        assert!(
            error
                .to_string()
                .contains("unimplemented value[x] datatype converter: Foo"),
            "{error:#}"
        );

        let unrelated = context(vec![
            (PathBuf::from("a-base.json"), base(0)),
            (PathBuf::from("b-derived.json"), input),
            (
                PathBuf::from("z-unrelated.json"),
                serde_json::json!({
                    "resourceType":"StructureDefinition",
                    "id":"Unrelated",
                    "url":"https://example.org/StructureDefinition/Unrelated"
                }),
            ),
        ]);
        assert!(unrelated.matches_snapshot_dependencies(&manifest));
    }

    #[test]
    fn authenticated_member_identity_revalidates_without_reading_package_body() {
        let input = derived();
        let (source, original) = prepared_package_context(0);
        let (_, manifest) = generate_with_manifest(input, &original).unwrap();
        let base_query = SnapshotDependencyQuery::LoadStructureDefinition(
            "https://example.org/StructureDefinition/LocalBase".into(),
        );
        assert!(matches!(
            manifest.reads.get(&base_query),
            Some(SnapshotDependencyOutcome::LoadedStructureDefinition(
                SnapshotLoadedIdentity {
                    source_proof: Some(_),
                    outcome: SnapshotLoadedOutcome::Loaded(_)
                }
            ))
        ));

        let identical_source = ImmutableTestSource {
            inner: source.inner.fork_read_cache(),
            identities: source.identities.clone(),
        };
        let observer = identical_source.inner.clone();
        let root = identical_source.inner.cache_root().to_path_buf();
        let identical =
            PackageContext::new_with(identical_source, root, &["demo.base#1.0.0".into()]).unwrap();
        let before = observer.compression_metrics();
        assert!(identical.matches_snapshot_dependencies(&manifest));
        let after = observer.compression_metrics();
        assert_eq!(after.chunks_inflated, before.chunks_inflated);
        assert_eq!(after.raw_inflated_bytes, before.raw_inflated_bytes);

        // Losing the closed-source proof never falls through to a comparison
        // against the differently-domained parsed-value digest.
        let unauthenticated = source.inner.fork_read_cache();
        let root = unauthenticated.cache_root().to_path_buf();
        let unauthenticated =
            PackageContext::new_with(unauthenticated, root, &["demo.base#1.0.0".into()]).unwrap();
        assert!(!unauthenticated.matches_snapshot_dependencies(&manifest));

        let mut metadata_only_base = base(0);
        metadata_only_base["title"] = Value::String("Current title".into());
        metadata_only_base["status"] = Value::String("active".into());
        metadata_only_base["description"] = Value::String("Current description".into());
        metadata_only_base["publisher"] = Value::String("Current publisher".into());
        let (_, changed_metadata) = prepared_package_context_with_base(metadata_only_base);
        assert!(
            changed_metadata.matches_snapshot_dependencies(&manifest),
            "a changed authenticated carrier must run the loader and may prove equal loaded input"
        );

        let (_, changed_structure) = prepared_package_context(1);
        assert!(!changed_structure.matches_snapshot_dependencies(&manifest));
    }

    #[test]
    fn negative_and_precedence_results_are_exact_manifest_facts() {
        let empty = context(Vec::new());
        empty.begin_snapshot_dependency_capture();
        assert!(empty
            .resolve_snapshot_structure_definition("FutureProfile")
            .is_none());
        let missing = empty.finish_snapshot_dependency_capture();
        assert!(empty.matches_snapshot_dependencies(&missing));

        let future = serde_json::json!({
            "resourceType":"StructureDefinition",
            "id":"FutureProfile",
            "url":"https://example.org/StructureDefinition/FutureProfile",
            "name":"FutureProfile"
        });
        let now_present = context(vec![(PathBuf::from("future.json"), future)]);
        assert!(!now_present.matches_snapshot_dependencies(&missing));

        let first = base(0);
        let mut second = base(1);
        second["url"] = Value::String("https://example.org/StructureDefinition/Other".into());
        let original = context(vec![
            (PathBuf::from("a.json"), first.clone()),
            (PathBuf::from("b.json"), second.clone()),
        ]);
        original.begin_snapshot_dependency_capture();
        assert_eq!(
            original
                .resolve_snapshot_structure_definition("LocalBase")
                .and_then(|resolved| resolved.raw["snapshot"]["element"][0]["min"].as_u64()),
            Some(1)
        );
        let winner = original.finish_snapshot_dependency_capture();

        let reversed = context(vec![
            (PathBuf::from("b.json"), second),
            (PathBuf::from("a.json"), first),
        ]);
        assert!(!reversed.matches_snapshot_dependencies(&winner));
    }

    #[test]
    fn overflowed_manifest_is_incomplete_and_never_revalidates() {
        let context = context(Vec::new());
        context.begin_snapshot_dependency_capture();
        for index in 0..=SNAPSHOT_DEPENDENCY_MAX_FACTS {
            assert!(context
                .resolve_snapshot_structure_definition(&format!("missing-{index}"))
                .is_none());
        }
        let manifest = context.finish_snapshot_dependency_capture();
        assert!(!manifest.is_complete());
        assert_eq!(manifest.fact_count(), 0);
        assert!(!context.matches_snapshot_dependencies(&manifest));
    }

    #[test]
    fn ordinary_snapshot_generation_leaves_observation_and_digest_caches_empty() {
        let input = derived();
        let context = context(vec![
            (PathBuf::from("a-base.json"), base(0)),
            (PathBuf::from("b-derived.json"), input.clone()),
        ]);
        crate::generate_snapshot(input, &context, Default::default()).unwrap();
        assert!(context.snapshot_dependencies.borrow().is_none());
        assert!(context.loaded_sd_digests.borrow().is_empty());
    }

    fn package_context(base_min: u64) -> (tempfile::TempDir, PackageContext) {
        let cache = tempfile::tempdir().unwrap();
        let package = cache.path().join("demo.base#1.0.0/package");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("package.json"),
            br#"{"name":"demo.base","version":"1.0.0"}"#,
        )
        .unwrap();
        let body = base(base_min);
        std::fs::write(
            package.join("StructureDefinition-LocalBase.json"),
            serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();
        std::fs::write(
            package.join(".index.json"),
            serde_json::to_vec(&serde_json::json!({
                "index-version": 2,
                "files": [{
                    "filename":"StructureDefinition-LocalBase.json",
                    "resourceType":"StructureDefinition",
                    "id":"LocalBase",
                    "url":"https://example.org/StructureDefinition/LocalBase",
                    "version":"1.0.0",
                    "kind":"resource",
                    "type":"Patient",
                    "derivation":"constraint"
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let context = PackageContext::new(cache.path(), &["demo.base#1.0.0".into()]).unwrap();
        (cache, context)
    }

    #[test]
    fn package_backed_base_origin_and_body_are_revalidated() {
        let input = derived();
        let (_original_cache, original) = package_context(0);
        let (_, manifest) = generate_with_manifest(input.clone(), &original).unwrap();
        let url = "https://example.org/StructureDefinition/LocalBase";
        assert_eq!(
            manifest
                .reads
                .get(&SnapshotDependencyQuery::IsLocal(url.into())),
            Some(&SnapshotDependencyOutcome::Bool(false))
        );
        assert_eq!(
            manifest
                .reads
                .get(&SnapshotDependencyQuery::PackageId(url.into())),
            Some(&SnapshotDependencyOutcome::Text(Some("demo.base".into())))
        );

        let (_identical_cache, identical) = package_context(0);
        assert!(identical.matches_snapshot_dependencies(&manifest));
        let (_changed_cache, changed) = package_context(1);
        assert!(!changed.matches_snapshot_dependencies(&manifest));
    }

    #[test]
    fn derivation_recomposition_cannot_bypass_package_revalidation() {
        let input = derived();
        let (_original_cache, original) = package_context(0);
        let prepared = crate::prepare_snapshot_derivation(input.clone(), Default::default())
            .expect("prepare original derivation");
        let (_, derivation) = crate::generate_prepared_snapshot_derivation(prepared, &original)
            .expect("generate original derivation");

        let (_changed_cache, changed) = package_context(1);
        let mut current = crate::prepare_snapshot_derivation(input.clone(), Default::default())
            .expect("prepare current input");
        assert!(derivation
            .try_recompose(&mut current, &changed)
            .expect("dependency mismatch is a cache miss")
            .is_none());

        // A failed admission attempt leaves the opaque current envelope intact,
        // so canonical retry or a matching bounded generation remains possible.
        assert!(derivation
            .try_recompose(&mut current, &original)
            .expect("matching dependencies may recompose")
            .is_some());
    }
}

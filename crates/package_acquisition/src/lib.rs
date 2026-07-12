//! Content-addressed acquisition and materialization for FHIR packages.
//!
//! This crate owns the write side: resolve or ingest package artifacts into an
//! immutable CAS, then materialize a `.fhir/packages`-shaped cache root for the
//! existing `package_store` read side.

use anyhow::{anyhow, bail, Context};
use flate2::read::GzDecoder;
use flate2::{Compression, GzBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tar::{Archive, Builder, Header};
use walkdir::WalkDir;

use package_store::derived_index;

const RESOLUTION_CONFIG_JSON: &str = include_str!("../resolution-config.json");
const DERIVED_DIR: &str = "derived";
const MATERIALIZED_INDEX_V2: &str = "materialized-index-v2.json";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolutionConfig {
    build_server_base: String,
    default_registries: Vec<ConfiguredRegistry>,
    custom_registry: RegistryTemplate,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfiguredRegistry {
    url: String,
    metadata_url: String,
    fallback_tarball_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryTemplate {
    metadata_url: String,
    fallback_tarball_url: String,
}

struct RegistryTemplates<'a> {
    metadata_url: &'a str,
    fallback_tarball_url: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coordinate {
    pub name: String,
    pub version: String,
}

impl Coordinate {
    pub fn parse(input: &str) -> anyhow::Result<Self> {
        let (name, version) = input
            .split_once('#')
            .or_else(|| input.split_once('@'))
            .ok_or_else(|| anyhow::anyhow!("package coordinate must be <name>#<version>"))?;
        let name = name.trim();
        let version = version.trim();
        if name.is_empty() || version.is_empty() {
            bail!("package coordinate must be <name>#<version>");
        }
        Ok(Self {
            name: name.to_string(),
            version: version.to_string(),
        })
    }

    pub fn label(&self) -> String {
        format!("{}#{}", self.name, self.version)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageRef {
    pub name: String,
    pub requested: String,
    pub effective_version: String,
    pub materialized_version: String,
    pub sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shasum: Option<String>,
    pub source: SourceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tarball_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_date: Option<String>,
    pub mutable: bool,
    pub fallback: bool,
    pub fetched_at_unix: u64,
}

impl PackageRef {
    pub fn requested_label(&self) -> String {
        format!("{}#{}", self.name, self.requested)
    }

    pub fn materialized_label(&self) -> String {
        format!("{}#{}", self.name, self.materialized_version)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Registry,
    BuildServer,
    LocalTarball,
    LocalDirectory,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageLock {
    pub lockfile_version: u32,
    pub generated_at_unix: u64,
    pub packages: Vec<PackageRef>,
}

impl PackageLock {
    pub fn read(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn write(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("lock.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PackageManifest {
    sha256: String,
    artifact_size: u64,
    files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestFile {
    path: String,
    size: u64,
    sha256: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub name: String,
    pub requested: String,
    pub effective_version: String,
    pub materialized_version: String,
    pub source: SourceKind,
    pub registry: Option<String>,
    pub tarball_url: String,
    pub shasum: Option<String>,
    pub build_url: Option<String>,
    pub build_date: Option<String>,
    pub mutable: bool,
    pub fallback: bool,
}

pub struct PackageCas {
    root: PathBuf,
}

impl PackageCas {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn default_root() -> anyhow::Result<PathBuf> {
        if let Ok(path) = std::env::var("FHIR_CAS") {
            return Ok(PathBuf::from(path));
        }
        if let Ok(path) = std::env::var("XDG_CACHE_HOME") {
            return Ok(PathBuf::from(path).join("fhir-rs").join("cas"));
        }
        let home = std::env::var("HOME").context("FHIR_CAS not set and HOME not available")?;
        Ok(PathBuf::from(home)
            .join(".cache")
            .join("fhir-rs")
            .join("cas"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn acquire_remote(
        &self,
        coord: &Coordinate,
        registries: &[String],
    ) -> anyhow::Result<PackageRef> {
        if !is_build_server_version(&coord.version) {
            return self.acquire_registry(coord, registries);
        }
        let resolved = resolve_remote(coord, self, registries)?;
        let bytes = download_bytes(&resolved.tarball_url)
            .with_context(|| format!("download {}", resolved.tarball_url))?;
        self.ingest_artifact_bytes(&resolved, &bytes)
    }

    fn acquire_registry(
        &self,
        coord: &Coordinate,
        registries: &[String],
    ) -> anyhow::Result<PackageRef> {
        let mut errors = Vec::new();
        for registry in registries {
            let registry = registry.trim_end_matches('/');
            match resolve_registry_one(coord, registry) {
                Ok(resolved) => match download_bytes(&resolved.tarball_url) {
                    Ok(bytes) => return self.ingest_artifact_bytes(&resolved, &bytes),
                    Err(e) => errors.push(format!(
                        "{}: download {} failed: {e}",
                        registry, resolved.tarball_url
                    )),
                },
                Err(e) => errors.push(format!("{registry}: {e}")),
            }
        }
        bail!(
            "failed to acquire {} from registries: {}",
            coord.label(),
            errors.join("; ")
        )
    }

    pub fn ingest_local_source(
        &self,
        coord: &Coordinate,
        source: impl AsRef<Path>,
    ) -> anyhow::Result<PackageRef> {
        let source = source.as_ref();
        reject_real_fhir_path(source)?;
        let (bytes, kind) = if source.is_dir() {
            (
                canonicalize_package_dir(source)?,
                SourceKind::LocalDirectory,
            )
        } else {
            (
                fs::read(source).with_context(|| format!("read {}", source.display()))?,
                SourceKind::LocalTarball,
            )
        };
        let resolved = ResolvedPackage {
            name: coord.name.clone(),
            requested: coord.version.clone(),
            effective_version: coord.version.clone(),
            materialized_version: coord.version.clone(),
            source: kind,
            registry: None,
            tarball_url: source.to_string_lossy().to_string(),
            shasum: None,
            build_url: None,
            build_date: None,
            mutable: is_mutable_version(&coord.version),
            fallback: false,
        };
        self.ingest_artifact_bytes(&resolved, &bytes)
    }

    pub fn read_ref(&self, coord: &Coordinate) -> anyhow::Result<PackageRef> {
        let path = self.ref_path(&coord.label());
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn materialize_package(
        &self,
        coord: &Coordinate,
        out_cache: impl AsRef<Path>,
    ) -> anyhow::Result<PackageRef> {
        let package_ref = self.read_ref(coord)?;
        self.materialize_ref(&package_ref, out_cache)?;
        Ok(package_ref)
    }

    pub fn materialize_package_resolving(
        &self,
        coord: &Coordinate,
        out_cache: impl AsRef<Path>,
        registries: &[String],
        offline: bool,
    ) -> anyhow::Result<PackageRef> {
        let package_ref = self.acquire_or_read_ref(coord, registries, offline)?;
        self.materialize_ref(&package_ref, out_cache)?;
        Ok(package_ref)
    }

    pub fn lock_project(
        &self,
        ig_dir: impl AsRef<Path>,
        lock_path: impl AsRef<Path>,
        registries: &[String],
        update_all_mutable: bool,
    ) -> anyhow::Result<PackageLock> {
        self.lock_project_with_options(
            ig_dir,
            lock_path,
            registries,
            update_all_mutable,
            &[],
            false,
        )
    }

    pub fn lock_project_with_options(
        &self,
        ig_dir: impl AsRef<Path>,
        lock_path: impl AsRef<Path>,
        registries: &[String],
        update_all_mutable: bool,
        update_packages: &[String],
        offline: bool,
    ) -> anyhow::Result<PackageLock> {
        let ig_dir = ig_dir.as_ref();
        let lock_path = lock_path.as_ref();
        let existing = if lock_path.is_file() {
            Some(PackageLock::read(lock_path)?)
        } else {
            None
        };
        let mut existing_by_request = BTreeMap::new();
        if let Some(lock) = &existing {
            for package in &lock.packages {
                existing_by_request.insert(package.requested_label(), package.clone());
            }
        }
        let update_packages: BTreeSet<&str> = update_packages.iter().map(String::as_str).collect();

        let requests = package_store::project_package_requests(&ig_dir.to_string_lossy())
            .with_context(|| format!("resolve package requests for {}", ig_dir.display()))?;

        let mut packages = Vec::new();
        let mut loaded = std::collections::BTreeSet::new();
        for request in requests {
            let coord = Coordinate {
                name: request.package_id,
                version: request.version,
            };
            let requested_label = coord.label();
            let acquired = if let Some(existing) = existing_by_request.get(&requested_label) {
                let selected_for_update = update_packages.contains(existing.name.as_str())
                    || update_packages.contains(requested_label.as_str());
                let should_update = existing.mutable && (update_all_mutable || selected_for_update);
                if !should_update {
                    self.ensure_locked_ref_in_cas(existing, offline)
                        .map(|()| existing.clone())
                } else {
                    self.acquire_or_read_ref(&coord, registries, offline)
                }
            } else {
                self.acquire_or_read_ref(&coord, registries, offline)
            };

            let package_ref = match acquired {
                Ok(package_ref) => package_ref,
                Err(error) => {
                    // Mirror stock SUSHI's per-dependency leniency: an unresolvable
                    // *non-core* dependency is logged and skipped rather than aborting
                    // the whole build. See `loadConfiguredDependencies` (the per-dep
                    // `.catch(... logger.error ...)`) and `loadAutomaticDependencies`
                    // (the `logger.warn("Failed to load ...")`) in
                    // `sushi-ts/src/utils/Processing.ts`. FHIR core stays fatal — stock
                    // cannot build without it.
                    //
                    // This also covers `current`/`dev` build-server coordinates: stock's
                    // BasePackageLoader.loadPackage downloads current builds via
                    // build.fhir.org's `qas.json` and, when no matching entry exists,
                    // returns FAILED (logging "Failed to load ...") rather than throwing,
                    // so the build continues. `temp/top20/subscriptions` requests
                    // `hl7.fhir.uv.tools.r4#current`, but qas.json only publishes the CI
                    // build under package-id `hl7.fhir.uv.tools` (no `.r4` variant), so
                    // neither stock nor we can resolve it — we skip it like stock.
                    if is_core_package(&coord.name) {
                        return Err(error);
                    }
                    eprintln!("warn  Failed to load {requested_label}: {error:#}");
                    continue;
                }
            };

            let materialized = package_ref.materialized_label();
            if loaded.insert(materialized) {
                packages.push(package_ref);
            }
        }

        let lock = PackageLock {
            lockfile_version: 1,
            generated_at_unix: now_unix(),
            packages,
        };
        lock.write(lock_path)?;
        Ok(lock)
    }

    pub fn materialize_lock(
        &self,
        lock_path: impl AsRef<Path>,
        out_cache: impl AsRef<Path>,
    ) -> anyhow::Result<PackageLock> {
        self.materialize_lock_with_options(lock_path, out_cache, false)
    }

    pub fn materialize_lock_with_options(
        &self,
        lock_path: impl AsRef<Path>,
        out_cache: impl AsRef<Path>,
        offline: bool,
    ) -> anyhow::Result<PackageLock> {
        let lock = PackageLock::read(lock_path)?;
        for package_ref in &lock.packages {
            self.ensure_locked_ref_in_cas(package_ref, offline)?;
            self.materialize_ref(package_ref, out_cache.as_ref())?;
            if package_ref.fallback && package_ref.requested == "dev" {
                self.materialize_ref_as(
                    package_ref,
                    &package_ref.requested_label(),
                    out_cache.as_ref(),
                )?;
            }
        }
        Ok(lock)
    }

    pub fn materialize_ref(
        &self,
        package_ref: &PackageRef,
        out_cache: impl AsRef<Path>,
    ) -> anyhow::Result<()> {
        let pkg_root = self.package_root(&package_ref.sha256);
        if verify_cas_on_materialize() {
            verify_manifest(&pkg_root)?;
        }

        let source = pkg_root.join("package");
        if !source.is_dir() {
            bail!(
                "CAS package {} has no package/ directory",
                package_ref.sha256
            );
        }
        let out_cache = out_cache.as_ref();
        reject_real_fhir_path(out_cache)?;
        let target = out_cache
            .join(package_ref.materialized_label())
            .join("package");
        materialize_package_view(&pkg_root, &source, &target)?;
        Ok(())
    }

    fn materialize_ref_as(
        &self,
        package_ref: &PackageRef,
        label: &str,
        out_cache: &Path,
    ) -> anyhow::Result<()> {
        let pkg_root = self.package_root(&package_ref.sha256);
        if verify_cas_on_materialize() {
            verify_manifest(&pkg_root)?;
        }
        let source = pkg_root.join("package");
        reject_real_fhir_path(out_cache)?;
        let target = out_cache.join(label).join("package");
        materialize_package_view(&pkg_root, &source, &target)?;
        Ok(())
    }

    fn acquire_or_read_ref(
        &self,
        coord: &Coordinate,
        registries: &[String],
        offline: bool,
    ) -> anyhow::Result<PackageRef> {
        match self.read_ref(coord) {
            Ok(package_ref) if self.package_root(&package_ref.sha256).is_dir() => Ok(package_ref),
            _ if offline => bail!(
                "{} is not present in CAS and --offline is set",
                coord.label()
            ),
            _ => self.acquire_remote(coord, registries),
        }
    }

    fn ensure_locked_ref_in_cas(
        &self,
        package_ref: &PackageRef,
        offline: bool,
    ) -> anyhow::Result<()> {
        if self.package_root(&package_ref.sha256).is_dir() {
            return Ok(());
        }
        if offline {
            bail!(
                "lock entry {} points to missing CAS digest {} and --offline is set",
                package_ref.requested_label(),
                package_ref.sha256
            );
        }
        let Some(source) = &package_ref.tarball_url else {
            bail!(
                "lock entry {} points to missing CAS digest {} and has no recoverable source",
                package_ref.requested_label(),
                package_ref.sha256
            );
        };
        let bytes = match package_ref.source {
            SourceKind::Registry | SourceKind::BuildServer => {
                download_bytes(source).with_context(|| format!("download locked {source}"))?
            }
            SourceKind::LocalTarball => {
                let path = Path::new(source);
                reject_real_fhir_path(path)?;
                fs::read(path).with_context(|| format!("read locked {}", path.display()))?
            }
            SourceKind::LocalDirectory => {
                let path = Path::new(source);
                reject_real_fhir_path(path)?;
                canonicalize_package_dir(path)
                    .with_context(|| format!("canonicalize locked {}", path.display()))?
            }
        };
        let actual = sha256_hex(&bytes);
        if actual != package_ref.sha256 {
            bail!(
                "locked source for {} resolved to sha256 {}, expected {}",
                package_ref.requested_label(),
                actual,
                package_ref.sha256
            );
        }
        let resolved = ResolvedPackage {
            name: package_ref.name.clone(),
            requested: package_ref.requested.clone(),
            effective_version: package_ref.effective_version.clone(),
            materialized_version: package_ref.materialized_version.clone(),
            source: package_ref.source,
            registry: package_ref.registry.clone(),
            tarball_url: source.clone(),
            shasum: package_ref.shasum.clone(),
            build_url: package_ref.build_url.clone(),
            build_date: package_ref.build_date.clone(),
            mutable: package_ref.mutable,
            fallback: package_ref.fallback,
        };
        self.ingest_artifact_bytes(&resolved, &bytes)?;
        Ok(())
    }

    fn ingest_artifact_bytes(
        &self,
        resolved: &ResolvedPackage,
        bytes: &[u8],
    ) -> anyhow::Result<PackageRef> {
        self.ensure_layout()?;
        if let Some(expected) = &resolved.shasum {
            let actual = sha1_hex(bytes);
            if !expected.eq_ignore_ascii_case(&actual) {
                bail!(
                    "sha1 mismatch for {}#{}: expected {}, got {}",
                    resolved.name,
                    resolved.effective_version,
                    expected,
                    actual
                );
            }
        }

        let sha256 = sha256_hex(bytes);
        let pkg_root = self.package_root(&sha256);
        if !pkg_root.is_dir() {
            let tmp_parent = self.root.join("tmp");
            fs::create_dir_all(&tmp_parent)?;
            let temp = tempfile::Builder::new()
                .prefix("pkg-")
                .tempdir_in(&tmp_parent)?;
            let extract_root = temp.path().join("extract");
            fs::create_dir_all(&extract_root)?;
            extract_package_artifact(bytes, &extract_root)?;
            normalize_extracted_package(&extract_root)?;

            let manifest =
                build_manifest(&sha256, bytes.len() as u64, &extract_root.join("package"))?;
            let staged_root = temp.path().join("cas-entry");
            fs::create_dir_all(&staged_root)?;
            copy_tree(&extract_root.join("package"), &staged_root.join("package"))?;
            write_derived_materialized_index(
                &staged_root.join("package"),
                &derived_materialized_index_path(&staged_root),
            )?;
            // Derived-columns index (adds `name` + `baseDefinition`, folds in the
            // scan-fallback) — computed ONCE here, keyed by content hash +
            // format version, hardlinked out at materialize. See
            // `docs/package-derived-index.md`.
            write_derived_columns_index(
                &staged_root.join("package"),
                &derived_columns_index_path(&staged_root),
            )?;
            fs::write(
                staged_root.join("manifest.json"),
                serde_json::to_vec_pretty(&manifest)?,
            )?;
            fs::rename(&staged_root, &pkg_root)
                .or_else(|_| {
                    if pkg_root.is_dir() {
                        Ok(())
                    } else {
                        fs::rename(&staged_root, &pkg_root)
                    }
                })
                .with_context(|| format!("install CAS package {}", pkg_root.display()))?;
            make_read_only(&pkg_root)?;
        }

        let tarball = self.tarball_path(&sha256);
        if !tarball.exists() {
            let tmp = self.root.join("tarballs").join(format!("{sha256}.tmp"));
            fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
            fs::rename(&tmp, &tarball).with_context(|| format!("write {}", tarball.display()))?;
        }

        let package_ref = PackageRef {
            name: resolved.name.clone(),
            requested: resolved.requested.clone(),
            effective_version: resolved.effective_version.clone(),
            materialized_version: resolved.materialized_version.clone(),
            sha256,
            shasum: resolved.shasum.clone(),
            source: resolved.source,
            registry: resolved.registry.clone(),
            tarball_url: Some(resolved.tarball_url.clone()),
            build_url: resolved.build_url.clone(),
            build_date: resolved.build_date.clone(),
            mutable: resolved.mutable,
            fallback: resolved.fallback,
            fetched_at_unix: now_unix(),
        };
        self.write_ref(&package_ref.requested_label(), &package_ref)?;
        self.write_ref(&package_ref.materialized_label(), &package_ref)?;
        Ok(package_ref)
    }

    fn ensure_layout(&self) -> anyhow::Result<()> {
        // Hard rule: the CAS must never be created under the real `~/.fhir`
        // tree, even if a caller passes `--cas ~/.fhir` explicitly. This is the
        // single choke point before any CAS write, so guarding here covers all
        // ingest/materialize-into-CAS paths (materialize TARGETS and local
        // SOURCES are guarded separately).
        reject_real_fhir_path(&self.root)?;
        fs::create_dir_all(self.root.join("packages"))?;
        fs::create_dir_all(self.root.join("tarballs"))?;
        fs::create_dir_all(self.root.join("refs"))?;
        fs::create_dir_all(self.root.join("tmp"))?;
        Ok(())
    }

    fn package_root(&self, sha256: &str) -> PathBuf {
        self.root.join("packages").join(sha256)
    }

    fn tarball_path(&self, sha256: &str) -> PathBuf {
        self.root.join("tarballs").join(format!("{sha256}.tgz"))
    }

    fn ref_path(&self, label: &str) -> PathBuf {
        self.root
            .join("refs")
            .join(format!("{}.json", encode_ref(label)))
    }

    fn write_ref(&self, label: &str, package_ref: &PackageRef) -> anyhow::Result<()> {
        let path = self.ref_path(label);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(package_ref)?)?;
        fs::rename(&tmp, &path).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
}

pub fn default_registries() -> Vec<String> {
    if let Ok(registry) = std::env::var("FHIR_REGISTRY").or_else(|_| std::env::var("FPL_REGISTRY"))
    {
        vec![registry]
    } else {
        resolution_config()
            .default_registries
            .iter()
            .map(|r| r.url.trim_end_matches('/').to_string())
            .collect()
    }
}

fn resolve_remote(
    coord: &Coordinate,
    cas: &PackageCas,
    registries: &[String],
) -> anyhow::Result<ResolvedPackage> {
    if coord.version == "dev" {
        if cas.read_ref(coord).is_ok() {
            bail!(
                "{} already exists in CAS; use materialize instead",
                coord.label()
            );
        }
        let current = Coordinate {
            name: coord.name.clone(),
            version: "current".to_string(),
        };
        let mut resolved = resolve_current(&current.name, None)?;
        resolved.requested = "dev".to_string();
        resolved.fallback = true;
        return Ok(resolved);
    }
    if let Some(branch) = coord.version.strip_prefix("current$") {
        return resolve_current(&coord.name, Some(branch));
    }
    if coord.version == "current" {
        return resolve_current(&coord.name, None);
    }
    resolve_registry(coord, registries)
}

fn resolve_registry(coord: &Coordinate, registries: &[String]) -> anyhow::Result<ResolvedPackage> {
    let mut errors = Vec::new();
    for registry in registries {
        match resolve_registry_one(coord, registry.trim_end_matches('/')) {
            Ok(r) => return Ok(r),
            Err(e) => errors.push(format!("{}: {e}", registry)),
        }
    }
    bail!(
        "failed to resolve {} from registries: {}",
        coord.label(),
        errors.join("; ")
    )
}

fn resolve_registry_one(coord: &Coordinate, registry: &str) -> anyhow::Result<ResolvedPackage> {
    let manifest_url = registry_manifest_url(registry, &coord.name);
    let manifest: Option<RegistryManifest> = match get_json(&manifest_url) {
        Ok(m) => Some(m),
        Err(_) if is_exact_version(&coord.version) => None,
        Err(e) => return Err(e),
    };

    let effective = match &manifest {
        Some(m) => resolve_version_from_manifest(m, &coord.version)?,
        None => coord.version.clone(),
    };
    let (tarball_url, shasum) = manifest
        .as_ref()
        .and_then(|m| m.versions.get(&effective))
        .and_then(|v| v.dist.as_ref())
        .map(|d| {
            (
                d.tarball
                    .clone()
                    .unwrap_or_else(|| fallback_tarball_url(registry, &coord.name, &effective)),
                d.shasum.clone(),
            )
        })
        .unwrap_or_else(|| {
            (
                fallback_tarball_url(registry, &coord.name, &effective),
                None,
            )
        });

    Ok(ResolvedPackage {
        name: coord.name.clone(),
        requested: coord.version.clone(),
        effective_version: effective.clone(),
        materialized_version: effective,
        source: SourceKind::Registry,
        registry: Some(registry.to_string()),
        tarball_url,
        shasum,
        build_url: None,
        build_date: None,
        mutable: is_mutable_version(&coord.version),
        fallback: false,
    })
}

fn resolve_current(name: &str, branch: Option<&str>) -> anyhow::Result<ResolvedPackage> {
    let build_base = build_server_base();
    let qas: Vec<Value> = get_json(&format!("{build_base}/qas.json"))?;
    let mut matches: Vec<&Value> = qas
        .iter()
        .filter(|v| v.get("package-id").and_then(Value::as_str) == Some(name))
        .filter(|v| {
            let Some(repo) = v.get("repo").and_then(Value::as_str) else {
                return false;
            };
            match branch {
                Some(b) => repo.ends_with(&format!("/{b}/qa.json")),
                None => repo.contains("/main/qa.json") || repo.contains("/master/qa.json"),
            }
        })
        .collect();
    matches.sort_by(|a, b| {
        let ad = a.get("date").and_then(Value::as_str).unwrap_or("");
        let bd = b.get("date").and_then(Value::as_str).unwrap_or("");
        bd.cmp(ad)
    });
    let newest = matches
        .first()
        .ok_or_else(|| anyhow::anyhow!("no current build found for {name}"))?;
    let repo = newest
        .get("repo")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("current build for {name} has no repo"))?;
    let package_path = repo
        .strip_suffix("/qa.json")
        .ok_or_else(|| anyhow::anyhow!("current build repo does not end in /qa.json: {repo}"))?;
    let build_url = format!("{build_base}/{package_path}");
    let requested = branch
        .map(|b| format!("current${b}"))
        .unwrap_or_else(|| "current".to_string());
    Ok(ResolvedPackage {
        name: name.to_string(),
        requested: requested.clone(),
        effective_version: requested.clone(),
        materialized_version: requested,
        source: SourceKind::BuildServer,
        registry: None,
        tarball_url: format!("{build_url}/package.tgz"),
        shasum: None,
        build_url: Some(build_url),
        build_date: newest
            .get("date")
            .and_then(Value::as_str)
            .map(str::to_string),
        mutable: true,
        fallback: false,
    })
}

#[derive(Debug, Deserialize)]
struct RegistryManifest {
    #[serde(rename = "dist-tags", default)]
    dist_tags: BTreeMap<String, String>,
    #[serde(default)]
    versions: BTreeMap<String, RegistryVersion>,
}

#[derive(Debug, Deserialize)]
struct RegistryVersion {
    dist: Option<RegistryDist>,
}

#[derive(Debug, Deserialize)]
struct RegistryDist {
    shasum: Option<String>,
    tarball: Option<String>,
}

fn resolve_version_from_manifest(
    manifest: &RegistryManifest,
    requested: &str,
) -> anyhow::Result<String> {
    if requested == "latest" {
        return manifest
            .dist_tags
            .get("latest")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("registry metadata has no dist-tags.latest"));
    }
    if let Some(prefix) = requested.strip_suffix(".x") {
        if prefix.matches('.').count() != 1 {
            bail!("unsupported wildcard version {requested}; expected M.N.x");
        }
        let prefix = format!("{prefix}.");
        return manifest
            .versions
            .keys()
            .filter(|v| v.starts_with(&prefix))
            .max_by(|a, b| version_cmp(a, b))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no version satisfies {requested}"));
    }
    if manifest.versions.contains_key(requested) {
        Ok(requested.to_string())
    } else if is_exact_version(requested) {
        Ok(requested.to_string())
    } else {
        bail!("registry metadata has no version {requested}")
    }
}

fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> anyhow::Result<T> {
    let mut req = ureq::get(url);
    if let (Ok(registry), Ok(token)) = (
        std::env::var("FPL_REGISTRY"),
        std::env::var("FPL_REGISTRY_TOKEN"),
    ) {
        if url.starts_with(&registry) {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
    }
    let response = req.call().map_err(|e| anyhow::anyhow!("{e}"))?;
    response.into_json().map_err(|e| anyhow::anyhow!("{e}"))
}

fn resolution_config() -> &'static ResolutionConfig {
    static CONFIG: OnceLock<ResolutionConfig> = OnceLock::new();
    CONFIG.get_or_init(|| {
        serde_json::from_str(RESOLUTION_CONFIG_JSON)
            .expect("package acquisition resolution-config.json must be valid")
    })
}

fn build_server_base() -> &'static str {
    resolution_config().build_server_base.trim_end_matches('/')
}

fn registry_manifest_url(registry: &str, name: &str) -> String {
    let templates = registry_templates(registry);
    expand_registry_template(templates.metadata_url, registry, name, "")
}

fn fallback_tarball_url(registry: &str, name: &str, version: &str) -> String {
    let templates = registry_templates(registry);
    expand_registry_template(templates.fallback_tarball_url, registry, name, version)
}

fn registry_templates(registry: &str) -> RegistryTemplates<'static> {
    let registry = registry.trim_end_matches('/');
    let config = resolution_config();
    if let Some(configured) = config
        .default_registries
        .iter()
        .find(|r| r.url.trim_end_matches('/') == registry)
    {
        RegistryTemplates {
            metadata_url: &configured.metadata_url,
            fallback_tarball_url: &configured.fallback_tarball_url,
        }
    } else {
        RegistryTemplates {
            metadata_url: &config.custom_registry.metadata_url,
            fallback_tarball_url: &config.custom_registry.fallback_tarball_url,
        }
    }
}

fn expand_registry_template(template: &str, registry: &str, name: &str, version: &str) -> String {
    template
        .replace("{registry}", registry.trim_end_matches('/'))
        .replace("{name}", name)
        .replace("{version}", version)
}

fn download_bytes(url: &str) -> anyhow::Result<Vec<u8>> {
    let mut req = ureq::get(url);
    if let (Ok(registry), Ok(token)) = (
        std::env::var("FPL_REGISTRY"),
        std::env::var("FPL_REGISTRY_TOKEN"),
    ) {
        if url.starts_with(&registry) {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
    }
    let response = req.call().map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut reader = response.into_reader();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn extract_package_artifact(bytes: &[u8], dest: &Path) -> anyhow::Result<()> {
    let gz = GzDecoder::new(Cursor::new(bytes));
    let mut archive = Archive::new(gz);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if !entry.unpack_in(dest)? {
            bail!(
                "package artifact contains path outside extraction root: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn normalize_extracted_package(root: &Path) -> anyhow::Result<()> {
    let package_dir = root.join("package");
    if !package_dir.exists() {
        fs::create_dir(&package_dir)?;
    }
    let mut entries = Vec::new();
    for ent in fs::read_dir(root)? {
        let ent = ent?;
        if ent.file_name() != "package" {
            entries.push(ent.path());
        }
    }
    for path in entries {
        let target = package_dir.join(
            path.file_name()
                .ok_or_else(|| anyhow::anyhow!("invalid path {}", path.display()))?,
        );
        fs::rename(&path, &target)?;
    }
    if !package_dir.join("package.json").is_file() {
        bail!("package artifact does not contain package/package.json");
    }
    Ok(())
}

fn canonicalize_package_dir(source: &Path) -> anyhow::Result<Vec<u8>> {
    let package_root = if source.file_name().and_then(|s| s.to_str()) == Some("package") {
        source.to_path_buf()
    } else if source.join("package").is_dir() {
        source.join("package")
    } else {
        source.to_path_buf()
    };
    if !package_root.join("package.json").is_file() {
        bail!(
            "local package directory must contain package.json or package/package.json: {}",
            source.display()
        );
    }

    let mut files = sorted_files(&package_root)?;
    let mut out = Vec::new();
    {
        let gz = GzBuilder::new()
            .mtime(0)
            .write(&mut out, Compression::default());
        let mut builder = Builder::new(gz);
        for path in files.drain(..) {
            let rel = path.strip_prefix(&package_root)?;
            let tar_path = Path::new("package").join(rel);
            let bytes = fs::read(&path)?;
            let mut header = Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_cksum();
            builder.append_data(&mut header, tar_path, Cursor::new(bytes))?;
        }
        builder.finish()?;
        let gz = builder.into_inner()?;
        gz.finish()?;
    }
    Ok(out)
}

fn build_manifest(
    sha256: &str,
    artifact_size: u64,
    package_dir: &Path,
) -> anyhow::Result<PackageManifest> {
    let mut files = Vec::new();
    for path in sorted_files(package_dir)? {
        let bytes = fs::read(&path)?;
        let rel = path
            .strip_prefix(package_dir)?
            .to_string_lossy()
            .replace('\\', "/");
        files.push(ManifestFile {
            path: rel,
            size: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
        });
    }
    Ok(PackageManifest {
        sha256: sha256.to_string(),
        artifact_size,
        files,
    })
}

fn verify_manifest(pkg_root: &Path) -> anyhow::Result<()> {
    let bytes = fs::read(pkg_root.join("manifest.json"))
        .with_context(|| format!("read {}", pkg_root.join("manifest.json").display()))?;
    let manifest: PackageManifest = serde_json::from_slice(&bytes)?;
    let sample = sample_indices(manifest.files.len());
    for (idx, file) in manifest.files.into_iter().enumerate() {
        let path = pkg_root.join("package").join(&file.path);
        let meta = fs::metadata(&path).with_context(|| format!("stat {}", path.display()))?;
        if meta.len() != file.size {
            bail!("CAS manifest verification failed for {}", path.display());
        }
        if sample.contains(&idx) {
            let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            if sha256_hex(&bytes) != file.sha256 {
                bail!("CAS manifest verification failed for {}", path.display());
            }
        }
    }
    Ok(())
}

fn verify_cas_on_materialize() -> bool {
    std::env::var("RUST_SUSHI_VERIFY_CAS")
        .map(|v| {
            matches!(
                v.as_str(),
                "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
            )
        })
        .unwrap_or(false)
}

fn sample_indices(len: usize) -> Vec<usize> {
    if len == 0 {
        Vec::new()
    } else {
        let mut out = vec![0, len / 2, len - 1];
        out.sort_unstable();
        out.dedup();
        out
    }
}

fn sorted_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for ent in WalkDir::new(root).follow_links(false) {
        let ent = ent?;
        if ent.file_type().is_file() {
            files.push(ent.path().to_path_buf());
        }
    }
    files.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
    Ok(files)
}

fn copy_tree(source: &Path, target: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(target)?;
    for ent in WalkDir::new(source).follow_links(false) {
        let ent = ent?;
        let rel = ent.path().strip_prefix(source)?;
        let dst = target.join(rel);
        if ent.file_type().is_dir() {
            fs::create_dir_all(&dst)?;
        } else if ent.file_type().is_file() {
            fs::copy(ent.path(), &dst)?;
        }
    }
    Ok(())
}

fn link_tree(source: &Path, target: &Path) -> anyhow::Result<()> {
    for ent in WalkDir::new(source).follow_links(false) {
        let ent = ent?;
        let rel = ent.path().strip_prefix(source)?;
        let dst = target.join(rel);
        if ent.file_type().is_dir() {
            fs::create_dir_all(&dst)?;
        } else if ent.file_type().is_file() {
            link_or_copy_file(ent.path(), &dst)?;
        }
    }
    Ok(())
}

fn materialize_package_view(pkg_root: &Path, source: &Path, target: &Path) -> anyhow::Result<()> {
    remove_materialized_target(target)?;
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("package target has no parent: {}", target.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;

    if package_index_is_trustworthy(source) {
        match symlink_dir(source, target) {
            Ok(()) => return Ok(()),
            Err(_) => {
                // Symlinks can be unavailable on some filesystems/platforms. Fall
                // back to the existing hardlink/copy materialization.
            }
        }
    }

    fs::create_dir_all(target).with_context(|| format!("create {}", target.display()))?;
    link_tree(source, target)?;
    install_materialized_index(pkg_root, target)?;
    install_derived_columns_sidecar(pkg_root, target)?;
    Ok(())
}

fn remove_materialized_target(target: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(target) {
        Ok(meta) => {
            if meta.file_type().is_dir() && !meta.file_type().is_symlink() {
                fs::remove_dir_all(target)
                    .with_context(|| format!("remove {}", target.display()))?;
            } else {
                fs::remove_file(target).with_context(|| format!("remove {}", target.display()))?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("stat {}", target.display())),
    }
    Ok(())
}

fn package_index_is_trustworthy(package_dir: &Path) -> bool {
    fs::read(package_dir.join(".index.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|index| {
            index
                .get("files")
                .and_then(Value::as_array)
                .map(|a| !a.is_empty())
        })
        .unwrap_or(false)
}

#[cfg(unix)]
fn symlink_dir(source: &Path, target: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, target)
}

#[cfg(not(unix))]
fn symlink_dir(_source: &Path, _target: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "directory symlinks are not supported on this platform",
    ))
}

fn link_or_copy_file(source: &Path, target: &Path) -> anyhow::Result<()> {
    match fs::hard_link(source, target) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(source, target)?;
            Ok(())
        }
    }
}

fn derived_materialized_index_path(pkg_root: &Path) -> PathBuf {
    pkg_root.join(DERIVED_DIR).join(MATERIALIZED_INDEX_V2)
}

/// Path of the derived-columns index CAS artifact (keyed by content hash +
/// format version via the versioned filename). See `docs/package-derived-index.md`.
fn derived_columns_index_path(pkg_root: &Path) -> PathBuf {
    pkg_root
        .join(DERIVED_DIR)
        .join(derived_index::cas_artifact_name())
}

fn write_derived_columns_index(package_dir: &Path, out_path: &Path) -> anyhow::Result<()> {
    let index = derived_index::build(&package_store::DiskSource, package_dir);
    write_bytes_atomically(out_path, &derived_index::to_bytes(&index))
}

/// Hardlink the CAS derived-columns artifact out to the materialized package
/// directory as the `.derived-index.json` sidecar (next to `.index.json`).
/// Consumers read the sidecar; if it is absent (e.g. a symlinked package dir over
/// read-only CAS content), they fall back to deriving it once themselves.
fn install_derived_columns_sidecar(
    pkg_root: &Path,
    target_package_dir: &Path,
) -> anyhow::Result<()> {
    let artifact = derived_columns_index_path(pkg_root);
    if !artifact.is_file() {
        return Ok(());
    }
    let sidecar = target_package_dir.join(derived_index::SIDECAR_NAME);
    if sidecar.exists() {
        fs::remove_file(&sidecar)?;
    }
    link_or_copy_file(&artifact, &sidecar)?;
    Ok(())
}

fn install_materialized_index(pkg_root: &Path, target_package_dir: &Path) -> anyhow::Result<()> {
    let target_index = target_package_dir.join(".index.json");
    if target_index.exists() {
        fs::remove_file(&target_index)?;
    }
    let derived_index = derived_materialized_index_path(pkg_root);
    if derived_index.is_file() {
        link_or_copy_file(&derived_index, &target_index)?;
    } else {
        write_materialized_index(target_package_dir)?;
    }
    Ok(())
}

fn build_materialized_index(package_dir: &Path) -> anyhow::Result<Value> {
    let mut filenames = Vec::new();
    for ent in fs::read_dir(package_dir)? {
        let ent = ent?;
        if !ent.file_type()?.is_file() {
            continue;
        }
        let name = ent.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !name.to_ascii_lowercase().ends_with(".json") {
            continue;
        }
        if name == "package.json" {
            continue;
        }
        filenames.push(name);
    }
    filenames.sort();

    let mut files = Vec::new();
    for filename in filenames {
        let path = package_dir.join(&filename);
        let Some(json) = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        else {
            continue;
        };
        let mut entry = Map::new();
        entry.insert("filename".into(), Value::String(filename));
        for key in [
            "resourceType",
            "id",
            "url",
            "version",
            "kind",
            "type",
            "derivation",
        ] {
            if let Some(value) = json.get(key).and_then(Value::as_str) {
                entry.insert(key.into(), Value::String(value.to_string()));
            }
        }
        files.push(Value::Object(entry));
    }

    let mut index = Map::new();
    index.insert("index-version".into(), Value::Number(2.into()));
    index.insert("files".into(), Value::Array(files));
    Ok(Value::Object(index))
}

fn write_derived_materialized_index(package_dir: &Path, index_path: &Path) -> anyhow::Result<()> {
    let index = build_materialized_index(package_dir)?;
    write_json_atomically(index_path, &index)
}

fn write_materialized_index(package_dir: &Path) -> anyhow::Result<()> {
    let index = build_materialized_index(package_dir)?;
    write_json_atomically(&package_dir.join(".index.json"), &index)
}

fn write_json_atomically(path: &Path, value: &Value) -> anyhow::Result<()> {
    write_bytes_atomically(path, &serde_json::to_vec(value)?)
}

fn write_bytes_atomically(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| format!("{e}.tmp"))
            .unwrap_or_else(|| "tmp".to_string()),
    );
    if tmp.exists() {
        fs::remove_file(&tmp)?;
    }
    fs::write(&tmp, bytes)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn make_read_only(path: &Path) -> anyhow::Result<()> {
    for ent in WalkDir::new(path).contents_first(true) {
        let ent = ent?;
        let mut perms = ent.metadata()?.permissions();
        perms.set_readonly(true);
        fs::set_permissions(ent.path(), perms)?;
    }
    Ok(())
}

fn reject_real_fhir_path(path: &Path) -> anyhow::Result<()> {
    let Ok(home) = std::env::var("HOME") else {
        return Ok(());
    };
    let real_fhir = PathBuf::from(home).join(".fhir");
    let real_fhir = real_fhir.canonicalize().unwrap_or(real_fhir);
    let candidate = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if candidate.starts_with(&real_fhir) {
        bail!(
            "refusing to use path under real ~/.fhir for acquisition/materialize: {}",
            path.display()
        );
    }
    Ok(())
}

fn encode_ref(label: &str) -> String {
    let mut out = String::new();
    for b in label.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

fn is_mutable_version(v: &str) -> bool {
    v == "latest" || v == "dev" || v == "current" || v.starts_with("current$") || v.ends_with(".x")
}

fn is_build_server_version(v: &str) -> bool {
    v == "dev" || v == "current" || v.starts_with("current$")
}

/// True for the FHIR core definitional package (e.g. `hl7.fhir.r4.core`,
/// `hl7.fhir.r4b.core`, `hl7.fhir.r5.core`, `hl7.fhir.r6.core`).
///
/// Stock SUSHI tolerates a failed load of any *configured* or *automatic*
/// dependency (it logs and continues — see `loadConfiguredDependencies` /
/// `loadAutomaticDependencies` in `sushi-ts/src/utils/Processing.ts`), but it
/// cannot produce any output without FHIR core. We mirror that by keeping a
/// failure to acquire core fatal while skipping other unresolvable deps.
fn is_core_package(name: &str) -> bool {
    let Some(mid) = name
        .strip_prefix("hl7.fhir.")
        .and_then(|s| s.strip_suffix(".core"))
    else {
        return name == "hl7.fhir.core";
    };
    // `mid` is the FHIR release token, e.g. "r4", "r4b", "r5", "r6".
    let digits = mid
        .strip_prefix('r')
        .map(|d| d.strip_suffix('b').unwrap_or(d))
        .unwrap_or("");
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

fn is_exact_version(v: &str) -> bool {
    !is_mutable_version(v)
}

fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (a_maj, a_min, a_pat, a_pre) = parse_num_ver(a);
    let (b_maj, b_min, b_pat, b_pre) = parse_num_ver(b);
    (a_maj, a_min, a_pat)
        .cmp(&(b_maj, b_min, b_pat))
        .then_with(|| match (a_pre.is_empty(), b_pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => a_pre.cmp(&b_pre),
        })
        .then_with(|| a.cmp(b))
}

fn parse_num_ver(v: &str) -> (u64, u64, u64, String) {
    let (core, pre) = v.split_once('-').unwrap_or((v, ""));
    let mut it = core.split('.');
    let maj = it.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let min = it.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let pat = it.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (maj, min, pat, pre.to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

fn sha1_hex(bytes: &[u8]) -> String {
    hex::encode(sha1::Sha1::digest(bytes))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// Editor package bundles (P1) — authenticated cold/compatibility transport.
// ---------------------------------------------------------------------------
//
// A *bundle* is one blob per package: the gzipped tar of the materialized
// `package/` directory's top-level files (resource JSONs + `.index.json` +
// `.derived-index.json`), tar entries named by the package-relative filename. A
// set of bundles is pinned by a `BundleManifest` lockfile. On a cache miss the
// browser fetches one blob per package, authenticates/inflates it in the host,
// and passes the entries through shared Rust normalization into a compact
// PreparedPackage v3-backed `package_store::BundleSource`; direct owned-file
// mounting remains a supported compatibility path. See
// `docs/package-derived-index.md` (Bundle format section) and
// `package_store::bundle`.

use package_store::{BundleManifest, BundleManifestEntry};

/// Version of the small manifest emitted beside a prepared-package set.
pub const PREPARED_PACKAGE_MANIFEST_VERSION: u32 = 3;

/// One exact prepared package artifact in a [`PreparedPackageManifest`].
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreparedPackageManifestEntry {
    pub id: String,
    pub version: String,
    pub artifact: String,
    pub key: package_store::PreparedPackageKey,
    pub cache_key: String,
    pub artifact_sha256: String,
    pub bytes: u64,
}

/// Native/browser-neutral inventory for a set of `.fpp` artifacts.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreparedPackageManifest {
    pub manifest_version: u32,
    pub packages: Vec<PreparedPackageManifestEntry>,
}

impl PreparedPackageManifest {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("PreparedPackageManifest serializes")
    }
}

/// Canonical argument contract shared by `fig packages prepare` and
/// `rust_sushi packages prepare`. The two binaries deliberately keep their
/// own output skins (`fig --json` uses an API envelope), but option parsing,
/// validation, and package preparation must not drift.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedPackageSetRequest {
    pub cache_dir: PathBuf,
    pub out_dir: PathBuf,
    pub labels: Vec<String>,
}

impl PreparedPackageSetRequest {
    /// Parse arguments following the `prepare` token.
    ///
    /// Accepted form:
    /// `--cache <dir> (--out <dir> | -o <dir>) [--json] <id#version>...`
    /// Unknown options, duplicate singleton options, missing values, and
    /// unsafe/non-exact labels fail before any filesystem access.
    pub fn parse_cli(args: &[String]) -> anyhow::Result<Self> {
        let mut cache_dir = None;
        let mut out_dir = None;
        let mut labels = Vec::new();
        let mut positional_only = false;
        let mut index = 0usize;
        while index < args.len() {
            let arg = args[index].as_str();
            if positional_only {
                package_store::parse_exact_package_label(arg)?;
                labels.push(arg.to_string());
                index += 1;
                continue;
            }
            match arg {
                "--" => {
                    positional_only = true;
                    index += 1;
                }
                "--cache" => {
                    let value = args
                        .get(index + 1)
                        .filter(|value| !value.starts_with('-'))
                        .ok_or_else(|| anyhow!("packages prepare needs --cache <dir>"))?;
                    if cache_dir.replace(PathBuf::from(value)).is_some() {
                        bail!("packages prepare accepts --cache exactly once");
                    }
                    index += 2;
                }
                "--out" | "-o" => {
                    let value = args
                        .get(index + 1)
                        .filter(|value| !value.starts_with('-'))
                        .ok_or_else(|| anyhow!("packages prepare needs --out <dir>"))?;
                    if out_dir.replace(PathBuf::from(value)).is_some() {
                        bail!("packages prepare accepts --out/-o exactly once");
                    }
                    index += 2;
                }
                // `--json` belongs to the outer CLI skin, but accepting it here
                // keeps both binaries on the identical preparation grammar.
                "--json" => index += 1,
                value if value.starts_with('-') => {
                    bail!("packages prepare does not recognize option {value}")
                }
                value => {
                    package_store::parse_exact_package_label(value)?;
                    labels.push(value.to_string());
                    index += 1;
                }
            }
        }
        let cache_dir = cache_dir.ok_or_else(|| anyhow!("packages prepare needs --cache <dir>"))?;
        let out_dir = out_dir.ok_or_else(|| anyhow!("packages prepare needs --out <dir>"))?;
        if labels.is_empty() {
            bail!("packages prepare needs at least one exact <id#version>");
        }
        let mut unique = BTreeSet::new();
        for label in &labels {
            if !unique.insert(label.clone()) {
                bail!("duplicate prepared package label {label}");
            }
        }
        Ok(Self {
            cache_dir,
            out_dir,
            labels,
        })
    }

    pub fn execute(&self) -> anyhow::Result<PreparedPackageManifest> {
        build_prepared_package_set(&self.cache_dir, &self.labels, &self.out_dir)
    }
}

/// Prepare one exact package from a materialized package directory. Safe nested
/// files are retained for templates; nested symlinks and non-files fail closed.
pub fn build_prepared_package(
    label: &str,
    package_dir: &Path,
) -> anyhow::Result<package_store::PreparedPackage> {
    if !package_dir.is_dir() {
        bail!(
            "prepare package: package dir does not exist: {}",
            package_dir.display()
        );
    }
    // A materialized package root is commonly a symlink into the immutable CAS.
    // Resolve that one explicit root, but never follow links inside it.
    let root = package_dir
        .canonicalize()
        .with_context(|| format!("resolve package directory {}", package_dir.display()))?;
    let mut entries = BTreeMap::new();
    for entry in walkdir::WalkDir::new(&root).follow_links(false) {
        let entry = entry.with_context(|| format!("walk package directory {}", root.display()))?;
        if entry.path() == root {
            continue;
        }
        if entry.file_type().is_symlink() {
            bail!(
                "prepare package rejects nested symlink {}",
                entry.path().display()
            );
        }
        if entry.file_type().is_dir() {
            continue;
        }
        if !entry.file_type().is_file() {
            bail!(
                "prepare package rejects non-file member {}",
                entry.path().display()
            );
        }
        let relative = entry
            .path()
            .strip_prefix(&root)
            .expect("walked member is below package root");
        let name = relative
            .components()
            .map(|component| component.as_os_str().to_str())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow!("package member path is not UTF-8: {}", relative.display()))?
            .join("/");
        if entries
            .insert(name.clone(), fs::read(entry.path())?)
            .is_some()
        {
            bail!("duplicate package member {name:?}");
        }
    }
    package_store::PreparedPackage::prepare(label, entries)
        .with_context(|| format!("prepare package {label}"))
}

/// Prepare one exact package from its complete native cache coordinate.
/// Compiler metadata lives below `package/`; Publisher templates and core
/// runtime assets may be siblings such as `config.json`, `content/`, or
/// `other/`. All safe files are folded into the one canonical carrier.
pub fn build_prepared_package_coordinate(
    label: &str,
    coordinate_dir: &Path,
) -> anyhow::Result<package_store::PreparedPackage> {
    if !coordinate_dir.is_dir() {
        bail!(
            "prepare package: coordinate dir does not exist: {}",
            coordinate_dir.display()
        );
    }
    let mut package = build_prepared_package(label, &coordinate_dir.join("package"))?;
    let mut files = package.files.materialize_all()?;
    let root = coordinate_dir
        .canonicalize()
        .with_context(|| format!("resolve package coordinate {}", coordinate_dir.display()))?;
    for entry in walkdir::WalkDir::new(&root)
        .min_depth(1)
        .follow_links(false)
    {
        let entry = entry.with_context(|| format!("walk package coordinate {}", root.display()))?;
        let relative = entry
            .path()
            .strip_prefix(&root)
            .expect("walked member is below package coordinate");
        if relative.starts_with("package") {
            continue;
        }
        if entry.file_type().is_symlink() {
            bail!(
                "prepare package rejects nested symlink {}",
                entry.path().display()
            );
        }
        if entry.file_type().is_dir() {
            continue;
        }
        if !entry.file_type().is_file() {
            bail!(
                "prepare package rejects non-file member {}",
                entry.path().display()
            );
        }
        let name = relative
            .components()
            .map(|component| component.as_os_str().to_str())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow!("package member path is not UTF-8: {}", relative.display()))?
            .join("/");
        if files
            .insert(name.clone(), fs::read(entry.path())?)
            .is_some()
        {
            bail!("duplicate package member {name:?}");
        }
    }
    package = package_store::PreparedPackage::prepare(label, files)
        .with_context(|| format!("prepare complete package coordinate {label}"))?;
    Ok(package)
}

/// Decode a prepared artifact and require the exact cache key selected by its
/// caller. This is the shared API used by native verification and WASM mounting.
pub fn read_prepared_package(
    bytes: &[u8],
    expected: &package_store::PreparedPackageKey,
) -> anyhow::Result<package_store::PreparedPackage> {
    package_store::PreparedPackage::decode_expected(bytes, expected)
}

/// Build one `.fpp` per exact label plus `prepared-package-manifest.json`.
pub fn build_prepared_package_set(
    cache_dir: &Path,
    labels: &[String],
    out_dir: &Path,
) -> anyhow::Result<PreparedPackageManifest> {
    fs::create_dir_all(out_dir)?;
    let mut manifest = PreparedPackageManifest {
        manifest_version: PREPARED_PACKAGE_MANIFEST_VERSION,
        packages: Vec::with_capacity(labels.len()),
    };
    let mut seen = BTreeSet::new();
    for label in labels {
        // Validate before either cache or output path construction. Without
        // this boundary a malicious label could escape those roots before the
        // package.json identity check ran.
        let (id, version) = package_store::parse_exact_package_label(label)?;
        if !seen.insert(label.clone()) {
            bail!("duplicate prepared package label {label}");
        }
        let prepared = build_prepared_package_coordinate(label, &cache_dir.join(label))?;
        let bytes = prepared.encode();
        let artifact = format!("{label}.fpp");
        write_bytes_atomically(&out_dir.join(&artifact), &bytes)?;
        let cache_key = prepared.key.cache_key();
        manifest.packages.push(PreparedPackageManifestEntry {
            id: id.to_string(),
            version: version.to_string(),
            artifact,
            key: prepared.key,
            cache_key,
            artifact_sha256: sha256_hex(&bytes),
            bytes: bytes.len() as u64,
        });
    }
    write_bytes_atomically(
        &out_dir.join("prepared-package-manifest.json"),
        &manifest.to_bytes(),
    )?;
    Ok(manifest)
}

/// Build one package bundle: the gzipped tar of every top-level file in a
/// materialized `package/` directory, tar entries named by the file's name (no
/// `package/` prefix). The `.derived-index.json` sidecar is guaranteed present —
/// if the materialized dir lacks it, it is derived from content (the same shared
/// builder the CAS/write-once paths use) and included, so a `BundleSource` reader
/// is fully read-only (never needs to write a sidecar).
///
/// Nested subdirectories are ignored: FHIR packages keep conformance resources at
/// the `package/` top level (FPL `getPotentialResourcePaths`), which is all the
/// read path and `BundleSource` consult.
pub fn build_bundle(package_dir: &Path) -> anyhow::Result<Vec<u8>> {
    if !package_dir.is_dir() {
        bail!(
            "bundle: package dir does not exist: {}",
            package_dir.display()
        );
    }

    // Collect top-level files (name -> bytes), sorted for determinism.
    let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for ent in fs::read_dir(package_dir)? {
        let ent = ent?;
        if !ent.file_type()?.is_file() {
            continue;
        }
        let name = ent.file_name().to_string_lossy().into_owned();
        entries.insert(name, fs::read(ent.path())?);
    }

    // Guarantee the derived-index sidecar is in the bundle (derive if absent).
    if !entries.contains_key(derived_index::SIDECAR_NAME) {
        let index = derived_index::build(&package_store::DiskSource, package_dir);
        entries.insert(
            derived_index::SIDECAR_NAME.to_string(),
            derived_index::to_bytes(&index),
        );
    }

    let mut out = Vec::new();
    {
        let gz = GzBuilder::new()
            .mtime(0)
            .write(&mut out, Compression::default());
        let mut builder = Builder::new(gz);
        for (name, bytes) in &entries {
            let mut header = Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_cksum();
            builder.append_data(&mut header, name, Cursor::new(bytes.clone()))?;
        }
        builder.finish()?;
        let gz = builder.into_inner()?;
        gz.finish()?;
    }
    Ok(out)
}

/// Inflate a bundle blob into its untrusted package-relative container entries.
/// Call [`read_normalized_bundle`] before mounting; this low-level parser alone
/// does not validate package identity, dependency metadata, or path semantics.
pub fn read_bundle(bytes: &[u8]) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    let gz = GzDecoder::new(Cursor::new(bytes));
    let mut archive = Archive::new(gz);
    let mut out = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        // Mount regular files only. Symlink/hardlink/device entries are not
        // package bytes and the browser transport applies the same rule.
        if !entry.header().entry_type().is_file() {
            continue;
        }
        // Normalize the member name to the package-relative filename the
        // BundleSource mounts: strip a leading `./`, then a leading `package/`.
        // Our repacked bundles store bare filenames (no `package/`); RAW REGISTRY
        // npm tarballs root every file under `package/`. Stripping it here makes
        // `read_bundle` mount a raw tgz identically to a repacked bundle — matching
        // the browser inflate (`app/src/worker/inflate.ts` strips the same prefixes).
        let path = entry.path()?;
        let raw_name = path
            .to_str()
            .ok_or_else(|| anyhow!("package bundle member name is not UTF-8"))?
            .trim_start_matches("./")
            .to_string();
        // Strip the npm package root exactly once. Repeated stripping would
        // promote `package/package/foo` to top-level `foo`, disagreeing with the
        // browser and with ordinary extraction semantics.
        let name = raw_name
            .strip_prefix("package/")
            .unwrap_or(&raw_name)
            .to_string();
        if name.is_empty() {
            continue;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        out.push((name, data));
    }
    Ok(out)
}

/// Inflate and pass one exact package through the authoritative shared
/// identity/path/dependency/derived-index boundary. Hosts should mount
/// `material.files` and use `material.payload` for semantic content addressing.
pub fn read_normalized_bundle(
    label: &str,
    bytes: &[u8],
) -> anyhow::Result<package_store::NormalizedPackageMaterial> {
    let mut entries = BTreeMap::new();
    for (name, body) in read_bundle(bytes)? {
        if entries.insert(name.clone(), body).is_some() {
            bail!("duplicate package bundle member after root normalization: {name}");
        }
    }
    package_store::normalize_package_material(label, entries)
        .with_context(|| format!("normalize package bundle {label}"))
}

/// Build a set of package bundles from an explicit cache, writing each package's
/// `<id>#<ver>.tgz` blob into `out_dir` and returning the pinned
/// [`BundleManifest`] (the editor's lockfile). `labels` are `<id>#<ver>` cache
/// directory names.
pub fn build_bundle_set(
    cache_dir: &Path,
    labels: &[String],
    out_dir: &Path,
) -> anyhow::Result<BundleManifest> {
    fs::create_dir_all(out_dir)?;
    let mut manifest = BundleManifest::new();
    for label in labels {
        let (id, version) = label
            .split_once('#')
            .ok_or_else(|| anyhow::anyhow!("bundle label must be <id>#<version>: {label}"))?;
        let package_dir = cache_dir.join(label).join("package");
        let blob =
            build_bundle(&package_dir).with_context(|| format!("building bundle for {label}"))?;
        let bundle_name = format!("{label}.tgz");
        write_bytes_atomically(&out_dir.join(&bundle_name), &blob)?;
        manifest.packages.push(BundleManifestEntry {
            id: id.to_string(),
            version: version.to_string(),
            bundle: bundle_name,
            sha256: Some(sha256_hex(&blob)),
        });
    }
    let manifest_bytes = manifest.to_bytes();
    write_bytes_atomically(&out_dir.join("bundle-manifest.json"), &manifest_bytes)?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[test]
    fn prepared_package_set_uses_shared_decoder_and_keeps_nested_files() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let package = cache.join("example.pkg#1.0.0/package");
        fs::create_dir_all(package.join("template")).unwrap();
        fs::write(
            package.join("package.json"),
            br#"{"name":"example.pkg","version":"1.0.0"}"#,
        )
        .unwrap();
        fs::write(
            package.join("Patient-p.json"),
            br#"{"resourceType":"Patient","id":"p"}"#,
        )
        .unwrap();
        fs::write(
            cache.join("example.pkg#1.0.0/config.json"),
            br#"{"name":"x"}"#,
        )
        .unwrap();
        fs::create_dir_all(cache.join("example.pkg#1.0.0/content")).unwrap();
        fs::write(
            cache.join("example.pkg#1.0.0/content/page.html"),
            b"publisher sibling",
        )
        .unwrap();

        let out = temp.path().join("prepared");
        let manifest =
            build_prepared_package_set(&cache, &["example.pkg#1.0.0".into()], &out).unwrap();
        let entry = &manifest.packages[0];
        let bytes = fs::read(out.join(&entry.artifact)).unwrap();
        let decoded = read_prepared_package(&bytes, &entry.key).unwrap();
        assert!(decoded.files.contains_key("config.json"));
        assert!(decoded.files.contains_key("content/page.html"));
        assert!(decoded.files.contains_key(derived_index::SIDECAR_NAME));
        assert_eq!(entry.artifact_sha256, sha256_hex(&bytes));
        assert!(out.join("prepared-package-manifest.json").is_file());
    }

    #[test]
    fn prepared_package_cli_contract_is_shared_strict_and_path_safe() {
        let request = PreparedPackageSetRequest::parse_cli(&[
            "--cache".into(),
            "cache".into(),
            "example.pkg#1.0.0".into(),
            "-o".into(),
            "prepared".into(),
            "--json".into(),
        ])
        .unwrap();
        assert_eq!(request.cache_dir, PathBuf::from("cache"));
        assert_eq!(request.out_dir, PathBuf::from("prepared"));
        assert_eq!(request.labels, ["example.pkg#1.0.0"]);

        for args in [
            vec!["--cache", "cache", "--out", "prepared", "../x#1"],
            vec!["--cache", "cache", "--out", "prepared", "x@1"],
            vec!["--cache", "cache", "--out", "prepared", "x#1#2"],
            vec!["--cache", "cache", "--out", "prepared", "--wat"],
            vec!["--cache", "a", "--cache", "b", "--out", "prepared", "x#1"],
            vec!["--cache", "cache", "--out", "a", "-o", "b", "x#1"],
        ] {
            let args = args.into_iter().map(str::to_string).collect::<Vec<_>>();
            assert!(
                PreparedPackageSetRequest::parse_cli(&args).is_err(),
                "{args:?}"
            );
        }

        let temp = tempfile::tempdir().unwrap();
        let err = build_prepared_package_set(
            temp.path(),
            &["../escape#1".into()],
            &temp.path().join("out"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("unsafe package label"));
        assert!(!temp.path().join("escape#1.fpp").exists());
    }

    #[test]
    fn coordinate_parse_accepts_hash_and_at() {
        assert_eq!(
            Coordinate::parse("hl7.fhir.r4.core#4.0.1").unwrap(),
            Coordinate {
                name: "hl7.fhir.r4.core".into(),
                version: "4.0.1".into()
            }
        );
        assert_eq!(Coordinate::parse("a@b").unwrap().label(), "a#b");
    }

    #[test]
    fn ref_encoding_is_filesystem_safe() {
        assert_eq!(encode_ref("a.b#current$main"), "a.b%23current%24main");
    }

    #[test]
    fn version_ordering_prefers_release_over_prerelease() {
        assert_eq!(
            version_cmp("5.3.0", "5.3.0-ballot-tc1"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(version_cmp("1.2.4", "1.2.3"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn core_package_detection_matches_fhir_core_only() {
        for core in [
            "hl7.fhir.r4.core",
            "hl7.fhir.r4b.core",
            "hl7.fhir.r5.core",
            "hl7.fhir.r6.core",
            "hl7.fhir.core",
        ] {
            assert!(is_core_package(core), "{core} should be core");
        }
        for non_core in [
            "hl7.fhir.us.core",
            "hl7.fhir.uv.tools.r4",
            "hl7.fhir.extensions.r5",
            "hl7.fhir.uv.extensions.r4",
            "hl7.terminology.r4",
        ] {
            assert!(!is_core_package(non_core), "{non_core} should not be core");
        }
    }

    #[test]
    fn manifest_sampling_is_deterministic() {
        assert_eq!(sample_indices(0), Vec::<usize>::new());
        assert_eq!(sample_indices(1), vec![0]);
        assert_eq!(sample_indices(5), vec![0, 2, 4]);
    }

    #[test]
    fn resolution_config_preserves_sushi_registry_order_and_url_shapes() {
        let config = resolution_config();
        let default_urls: Vec<&str> = config
            .default_registries
            .iter()
            .map(|r| r.url.as_str())
            .collect();
        assert_eq!(
            default_urls,
            vec![
                "https://packages.fhir.org",
                "https://packages2.fhir.org/packages"
            ]
        );
        assert_eq!(build_server_base(), "https://build.fhir.org/ig");
        assert_eq!(
            registry_manifest_url("https://packages2.fhir.org/packages/", "us.nlm.vsac"),
            "https://packages2.fhir.org/packages/us.nlm.vsac"
        );
        assert_eq!(
            fallback_tarball_url(
                "https://packages2.fhir.org/packages/",
                "us.nlm.vsac",
                "0.19.0"
            ),
            "https://packages2.fhir.org/packages/us.nlm.vsac/0.19.0"
        );
        assert_eq!(
            fallback_tarball_url("https://example.test/fhir", "example.fhir.pkg", "1.0.0"),
            "https://example.test/fhir/example.fhir.pkg/-/example.fhir.pkg-1.0.0.tgz"
        );
    }

    #[test]
    fn extraction_rejects_paths_outside_destination() {
        let bytes = raw_tgz_with_path("../evil.json", b"evil");
        let temp = tempfile::tempdir().unwrap();
        assert!(extract_package_artifact(&bytes, temp.path()).is_err());
    }

    #[test]
    fn exact_version_download_failure_falls_through_to_next_registry() {
        let package = package_tgz("example.fhir.pkg", "1.0.0");
        let shasum = sha1_hex(&package);
        let first = TestServer::new([
            ("/example.fhir.pkg", 404, "not found".as_bytes().to_vec()),
            (
                "/example.fhir.pkg/-/example.fhir.pkg-1.0.0.tgz",
                404,
                "not found".as_bytes().to_vec(),
            ),
        ]);
        let second = TestServer::new([
            (
                "/example.fhir.pkg",
                200,
                manifest_json(&second_placeholder(), "example.fhir.pkg", "1.0.0", &shasum),
            ),
            ("/example.fhir.pkg-1.0.0.tgz", 200, package),
        ]);
        second.replace_route(
            "/example.fhir.pkg",
            200,
            manifest_json(&second.base, "example.fhir.pkg", "1.0.0", &shasum),
        );

        let temp = tempfile::tempdir().unwrap();
        let cas = PackageCas::new(temp.path().join("cas"));
        let coord = Coordinate::parse("example.fhir.pkg#1.0.0").unwrap();
        let package_ref = cas
            .acquire_remote(&coord, &[first.base.clone(), second.base.clone()])
            .unwrap();

        assert_eq!(package_ref.registry.as_deref(), Some(second.base.as_str()));
        assert_eq!(package_ref.effective_version, "1.0.0");
        assert_eq!(package_ref.shasum.as_deref(), Some(shasum.as_str()));
        assert!(first.hit("/example.fhir.pkg/-/example.fhir.pkg-1.0.0.tgz"));
        assert!(second.hit("/example.fhir.pkg"));
        assert!(second.hit("/example.fhir.pkg-1.0.0.tgz"));
    }

    #[test]
    fn custom_registry_uses_npm_tarball_fallback_when_manifest_has_no_dist_tarball() {
        let package = package_tgz("example.fhir.pkg", "1.0.0");
        let server = TestServer::new([
            (
                "/example.fhir.pkg",
                200,
                br#"{"versions":{"1.0.0":{}}}"#.to_vec(),
            ),
            (
                "/example.fhir.pkg/-/example.fhir.pkg-1.0.0.tgz",
                200,
                package,
            ),
        ]);

        let temp = tempfile::tempdir().unwrap();
        let cas = PackageCas::new(temp.path().join("cas"));
        let package_ref = cas
            .acquire_remote(
                &Coordinate::parse("example.fhir.pkg#1.0.0").unwrap(),
                std::slice::from_ref(&server.base),
            )
            .unwrap();

        assert_eq!(package_ref.registry.as_deref(), Some(server.base.as_str()));
        assert!(server.hit("/example.fhir.pkg/-/example.fhir.pkg-1.0.0.tgz"));
    }

    #[test]
    fn materialize_installs_generated_index_from_cas_derived_artifact() {
        let package = package_tgz_with_bad_index("example.fhir.pkg", "1.0.0");
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("package.tgz");
        fs::write(&source, package).unwrap();
        let cas = PackageCas::new(temp.path().join("cas"));
        let coord = Coordinate::parse("example.fhir.pkg#1.0.0").unwrap();

        let package_ref = cas.ingest_local_source(&coord, &source).unwrap();
        let pkg_root = cas.package_root(&package_ref.sha256);
        assert!(derived_materialized_index_path(&pkg_root).is_file());
        // The derived-columns CAS artifact is computed once at ingest, keyed by
        // content hash + format version.
        assert!(derived_columns_index_path(&pkg_root).is_file());

        let out = temp.path().join("cache");
        cas.materialize_ref(&package_ref, &out).unwrap();
        #[cfg(unix)]
        assert!(
            !fs::symlink_metadata(out.join("example.fhir.pkg#1.0.0/package"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        // The derived-columns sidecar is hardlinked out next to `.index.json`,
        // and carries the `name` column the stock index omits.
        let sidecar = out.join(format!(
            "example.fhir.pkg#1.0.0/package/{}",
            derived_index::SIDECAR_NAME
        ));
        let rows = derived_index::parse(&fs::read(&sidecar).unwrap()).expect("current sidecar");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].filename, "ValueSet-Test.json");
        assert_eq!(rows[0].resource_type.as_deref(), Some("ValueSet"));
        assert_eq!(rows[0].id.as_deref(), Some("Test"));

        let index_path = out.join("example.fhir.pkg#1.0.0/package/.index.json");
        let index: Value = serde_json::from_slice(&fs::read(index_path).unwrap()).unwrap();
        let files = index
            .get("files")
            .and_then(Value::as_array)
            .expect("index files array");

        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].get("filename").and_then(Value::as_str),
            Some("ValueSet-Test.json")
        );
        assert_eq!(
            files[0].get("resourceType").and_then(Value::as_str),
            Some("ValueSet")
        );
        assert_eq!(files[0].get("id").and_then(Value::as_str), Some("Test"));
    }

    #[test]
    fn materialize_symlinks_package_directory_when_source_index_is_good() {
        let package = package_tgz_with_good_index("example.fhir.pkg", "1.0.0");
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("package.tgz");
        fs::write(&source, package).unwrap();
        let cas = PackageCas::new(temp.path().join("cas"));
        let coord = Coordinate::parse("example.fhir.pkg#1.0.0").unwrap();

        let package_ref = cas.ingest_local_source(&coord, &source).unwrap();
        let out = temp.path().join("cache");
        cas.materialize_ref(&package_ref, &out).unwrap();
        cas.materialize_ref(&package_ref, &out).unwrap();

        let package_dir = out.join("example.fhir.pkg#1.0.0/package");
        assert!(package_dir.join("ValueSet-Test.json").is_file());
        assert!(cas
            .package_root(&package_ref.sha256)
            .join("package/ValueSet-Test.json")
            .is_file());
        #[cfg(unix)]
        assert!(fs::symlink_metadata(&package_dir)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn latest_and_wildcard_versions_resolve_from_registry_metadata() {
        let pkg_120 = package_tgz("example.fhir.pkg", "1.2.0");
        let pkg_123 = package_tgz("example.fhir.pkg", "1.2.3");
        let pkg_130 = package_tgz("example.fhir.pkg", "1.3.0");
        let server = TestServer::empty();
        server.replace_route(
            "/example.fhir.pkg",
            200,
            version_manifest_json(
                &server.base,
                "example.fhir.pkg",
                "1.3.0",
                &[
                    ("1.2.0", &sha1_hex(&pkg_120)),
                    ("1.2.3", &sha1_hex(&pkg_123)),
                    ("1.3.0", &sha1_hex(&pkg_130)),
                ],
            ),
        );
        server.replace_route("/example.fhir.pkg-1.2.0.tgz", 200, pkg_120);
        server.replace_route("/example.fhir.pkg-1.2.3.tgz", 200, pkg_123);
        server.replace_route("/example.fhir.pkg-1.3.0.tgz", 200, pkg_130);

        let temp = tempfile::tempdir().unwrap();
        let cas = PackageCas::new(temp.path().join("cas"));
        let latest = cas
            .acquire_remote(
                &Coordinate::parse("example.fhir.pkg#latest").unwrap(),
                std::slice::from_ref(&server.base),
            )
            .unwrap();
        let wildcard = cas
            .acquire_remote(
                &Coordinate::parse("example.fhir.pkg#1.2.x").unwrap(),
                std::slice::from_ref(&server.base),
            )
            .unwrap();

        assert_eq!(latest.effective_version, "1.3.0");
        assert_eq!(latest.materialized_version, "1.3.0");
        assert!(latest.mutable);
        assert_eq!(wildcard.effective_version, "1.2.3");
        assert_eq!(wildcard.materialized_version, "1.2.3");
        assert!(wildcard.mutable);
    }

    fn raw_tgz_with_path(path: &str, data: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 512];
        header[..path.len()].copy_from_slice(path.as_bytes());
        write_octal(&mut header[100..108], 0o644);
        write_octal(&mut header[108..116], 0);
        write_octal(&mut header[116..124], 0);
        write_octal(&mut header[124..136], data.len() as u64);
        write_octal(&mut header[136..148], 0);
        for b in &mut header[148..156] {
            *b = b' ';
        }
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum: u32 = header.iter().map(|b| *b as u32).sum();
        let checksum = format!("{checksum:06o}\0 ");
        header[148..156].copy_from_slice(checksum.as_bytes());

        let mut tar = Vec::new();
        tar.extend_from_slice(&header);
        tar.extend_from_slice(data);
        let pad = (512 - (data.len() % 512)) % 512;
        tar.extend(std::iter::repeat(0).take(pad));
        tar.extend(std::iter::repeat(0).take(1024));

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&tar).unwrap();
        gz.finish().unwrap()
    }

    fn write_octal(field: &mut [u8], value: u64) {
        let text = format!("{value:0width$o}\0", width = field.len() - 1);
        field.copy_from_slice(text.as_bytes());
    }

    fn package_tgz(name: &str, version: &str) -> Vec<u8> {
        package_tgz_with_files(
            name,
            version,
            &[(
                "package/ValueSet-Test.json",
                br#"{"resourceType":"ValueSet","id":"Test","url":"http://example.org/ValueSet/Test","status":"draft"}"#,
            )],
        )
    }

    fn package_tgz_with_bad_index(name: &str, version: &str) -> Vec<u8> {
        package_tgz_with_files(
            name,
            version,
            &[
                ("package/.index.json", br#"{"index-version":2,"files":[]}"#),
                (
                    "package/ValueSet-Test.json",
                    br#"{"resourceType":"ValueSet","id":"Test","url":"http://example.org/ValueSet/Test","status":"draft"}"#,
                ),
            ],
        )
    }

    fn package_tgz_with_good_index(name: &str, version: &str) -> Vec<u8> {
        package_tgz_with_files(
            name,
            version,
            &[
                (
                    "package/.index.json",
                    br#"{"index-version":2,"files":[{"filename":"ValueSet-Test.json","resourceType":"ValueSet","id":"Test","url":"http://example.org/ValueSet/Test"}]}"#,
                ),
                (
                    "package/ValueSet-Test.json",
                    br#"{"resourceType":"ValueSet","id":"Test","url":"http://example.org/ValueSet/Test","status":"draft"}"#,
                ),
            ],
        )
    }

    fn package_tgz_with_files(name: &str, version: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = Builder::new(&mut gz);
            let package_json = format!(r#"{{"name":"{name}","version":"{version}"}}"#);
            append_tar_file(
                &mut builder,
                "package/package.json",
                package_json.as_bytes(),
            );
            for (path, data) in files {
                append_tar_file(&mut builder, path, data);
            }
            builder.finish().unwrap();
        }
        gz.finish().unwrap()
    }

    fn append_tar_file<W: Write>(builder: &mut Builder<W>, path: &str, data: &[u8]) {
        let mut header = Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, data).unwrap();
    }

    #[test]
    fn bundle_reader_strips_the_registry_package_root_only_once() {
        let raw = package_tgz_with_files(
            "example.pkg",
            "1.0.0",
            &[("package/package/nested.json", br#"{}"#)],
        );
        let entries = read_bundle(&raw).unwrap();
        assert!(entries
            .iter()
            .any(|(name, _)| name == "package/nested.json"));
        assert!(!entries.iter().any(|(name, _)| name == "nested.json"));
    }

    /// Raw-tgz mount parity (task #32 gate iii): a RAW REGISTRY npm tarball
    /// (files under `package/`, NO `.derived-index.json`) mounts into the engine
    /// IDENTICALLY to a repacked bundle (bare filenames + a `.derived-index.json`
    /// sidecar). "Identically" means: same package_store fishing results AND the
    /// same derived index (derived in-memory when the sidecar is absent).
    #[test]
    fn raw_registry_tgz_mounts_identically_to_repacked_bundle() {
        use package_store::{derived_index, BundleSource, DiskSource, FishType, PackageStore};

        let label = "example.pkg#1.0.0";
        let sd = br#"{"resourceType":"StructureDefinition","id":"Foo","url":"http://ex/Foo","name":"Foo","kind":"resource","type":"Patient","derivation":"constraint","baseDefinition":"http://hl7.org/fhir/StructureDefinition/Patient"}"#;

        // (A) RAW registry tarball: files under `package/`, no derived index.
        let raw = package_tgz_with_files(
            "example.pkg",
            "1.0.0",
            &[("package/StructureDefinition-Foo.json", sd)],
        );
        let raw_entries = read_bundle(&raw).unwrap();
        // read_bundle stripped `package/` — the mounted names are bare.
        assert!(raw_entries
            .iter()
            .any(|(n, _)| n == "StructureDefinition-Foo.json"));
        assert!(!raw_entries.iter().any(|(n, _)| n.contains("package/")));

        // (B) REPACKED bundle: materialize the raw tgz to a real package dir, then
        // build_bundle (which injects `.derived-index.json`).
        let tmp = std::env::temp_dir().join(format!("rawparity_{}", std::process::id()));
        let pkg_dir = tmp.join(label).join("package");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        for (name, bytes) in read_bundle(&raw).unwrap() {
            std::fs::write(pkg_dir.join(&name), &bytes).unwrap();
        }
        let repacked = build_bundle(&pkg_dir).unwrap();
        let repacked_entries = read_bundle(&repacked).unwrap();
        assert!(
            repacked_entries
                .iter()
                .any(|(n, _)| n == derived_index::SIDECAR_NAME),
            "repacked bundle carries the derived-index sidecar"
        );

        // Mount both into separate BundleSources.
        let material_a = read_normalized_bundle(label, &raw).unwrap();
        let material_b = read_normalized_bundle(label, &repacked).unwrap();
        assert_eq!(material_a.payload, material_b.payload);
        let mut src_a = BundleSource::new();
        src_a.mount_package(label, material_a.files);
        let mut src_b = BundleSource::new();
        src_b.mount_package(label, material_b.files);

        // Derived index parity: A derives it in-memory (no sidecar); B reads the
        // shipped sidecar. The rows must be identical.
        let dir_a = src_a.cache_root().join(label).join("package");
        let dir_b = src_b.cache_root().join(label).join("package");
        let rows_a = derived_index::load(&src_a, &dir_a);
        let rows_b = derived_index::load(&src_b, &dir_b);
        assert_eq!(rows_a, rows_b, "derived index rows differ raw vs repacked");
        assert_eq!(rows_a.len(), 1);
        assert_eq!(rows_a[0].name.as_deref(), Some("Foo"));

        // Engine-read parity: a PackageStore over each mounted source fishes the SD
        // identically by id / name / url.
        // Declare example.pkg as a configured dep so the store indexes it (the
        // auto-deps/core aren't mounted — index_package skips missing dirs).
        let cfg = "fhirVersion: 4.0.1\ndependencies:\n  example.pkg: 1.0.0\n";
        // Use for_project_with_config over the mounted source.
        let store_a = PackageStore::for_project_with_config(
            src_a.clone(),
            cfg,
            &src_a.cache_root().to_string_lossy(),
        )
        .unwrap();
        let store_b = PackageStore::for_project_with_config(
            src_b.clone(),
            cfg,
            &src_b.cache_root().to_string_lossy(),
        )
        .unwrap();
        for q in ["Foo", "http://ex/Foo"] {
            let a = store_a.fish_for_fhir(q, &[FishType::Profile]);
            let b = store_b.fish_for_fhir(q, &[FishType::Profile]);
            assert_eq!(
                a.as_deref(),
                b.as_deref(),
                "raw vs repacked fish mismatch for {q}"
            );
            assert!(a.is_some(), "should fish {q} from the raw-tgz mount");
        }
        let _ = DiskSource; // keep the import meaningful across refactors
        std::fs::remove_dir_all(&tmp).ok();
    }

    fn manifest_json(base: &str, name: &str, version: &str, shasum: &str) -> Vec<u8> {
        version_manifest_json(base, name, version, &[(version, shasum)])
    }

    fn version_manifest_json(
        base: &str,
        name: &str,
        latest: &str,
        versions: &[(&str, &str)],
    ) -> Vec<u8> {
        let mut version_entries = Vec::new();
        for (version, shasum) in versions {
            version_entries.push(format!(
                r#""{version}":{{"dist":{{"shasum":"{shasum}","tarball":"{base}/{name}-{version}.tgz"}}}}"#
            ));
        }
        format!(
            r#"{{"dist-tags":{{"latest":"{latest}"}},"versions":{{{}}}}}"#,
            version_entries.join(",")
        )
        .into_bytes()
    }

    fn second_placeholder() -> String {
        "http://127.0.0.1:9".to_string()
    }

    struct TestServer {
        base: String,
        routes: Arc<Mutex<HashMap<String, (u16, Vec<u8>)>>>,
        hits: Arc<Mutex<Vec<String>>>,
    }

    impl TestServer {
        fn empty() -> Self {
            Self::new([])
        }

        fn new<const N: usize>(routes: [(&str, u16, Vec<u8>); N]) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let route_map = Arc::new(Mutex::new(HashMap::new()));
            for (path, status, body) in routes {
                route_map
                    .lock()
                    .unwrap()
                    .insert(path.to_string(), (status, body));
            }
            let hits = Arc::new(Mutex::new(Vec::new()));
            let thread_routes = Arc::clone(&route_map);
            let thread_hits = Arc::clone(&hits);
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    handle_test_request(stream, &thread_routes, &thread_hits);
                }
            });
            Self {
                base,
                routes: route_map,
                hits,
            }
        }

        fn replace_route(&self, path: &str, status: u16, body: Vec<u8>) {
            self.routes
                .lock()
                .unwrap()
                .insert(path.to_string(), (status, body));
        }

        fn hit(&self, path: &str) -> bool {
            self.hits.lock().unwrap().iter().any(|p| p == path)
        }
    }

    fn handle_test_request(
        mut stream: std::net::TcpStream,
        routes: &Arc<Mutex<HashMap<String, (u16, Vec<u8>)>>>,
        hits: &Arc<Mutex<Vec<String>>>,
    ) {
        let mut buf = [0u8; 4096];
        let Ok(n) = std::io::Read::read(&mut stream, &mut buf) else {
            return;
        };
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string();
        hits.lock().unwrap().push(path.clone());
        let (status, body) = routes
            .lock()
            .unwrap()
            .get(&path)
            .cloned()
            .unwrap_or_else(|| (404, b"not found".to_vec()));
        let reason = if status == 200 { "OK" } else { "Not Found" };
        let header = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(&body);
    }
}

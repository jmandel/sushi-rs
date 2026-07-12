//! Native production of a callback-free closed SiteBuild bundle.
//!
//! This module is deliberately library composition rather than CLI business
//! logic. It resolves one exact package closure from an explicit cache, runs the
//! native renderer-neutral preparation pipeline once, derives the project
//! identity from its PreparedGuide, closes the selected SiteBuild projection,
//! and atomically emits a filesystem CAS bundle.
//!
//! The bundle layout is intentionally small and transport-neutral:
//!
//! ```text
//! <out>/site-build.json
//! <out>/objects/sha256/<lowercase digest>
//! ```
//!
//! `site-build.json` is the canonical `ClosedSiteBuild` value. Every content
//! reference it contains is present in `objects/sha256/` and is verified before
//! publication. Compilation and target preparation consume only the captured
//! project/package values. Postchecks additionally diagnose concurrent source or
//! package mutation before publication.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use content_store::{ContentStore, FileContentStore};
use serde::Serialize;
#[cfg(test)]
use serde_json::Value;
use site_build::{
    cycle_semantic, ArtifactState, ClosedSiteBuild, ContentRef, LockedPackage, PackageCoordinate,
    PackageLock, Sha256Digest, SourceEntry, SourceKind, SourceManifest, SourcePath,
};
use walkdir::WalkDir;

/// Supported closed SiteBuild contracts.
pub const CYCLE_SITE_TARGET: &str = cycle_semantic::TARGET;
pub const PUBLISHER_SITE_TARGET: &str = "publisher-site/v1";

/// All host inputs for one closed bundle. Every path is explicit; this API never
/// consults a default package cache and never performs package acquisition.
#[derive(Clone, Debug)]
pub struct PrepareConfig {
    pub ig_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub out_dir: PathBuf,
    pub target: String,
    pub template_coordinate: Option<String>,
    pub active_tables: bool,
    pub run_uuid: Option<String>,
    pub build_epoch_secs: i64,
}

fn site_engine_inputs(
    sources: &SourceSnapshot,
    packages: &PackageSnapshot,
    closure: &ResolvedClosure,
) -> Result<(site_engine::ProjectInputs, site_engine::PackageEnvironment)> {
    let config_path = SourcePath::parse("sushi-config.yaml").expect("static path");
    let config_ref = sources
        .manifest
        .get(&config_path)
        .ok_or_else(|| anyhow!("authored source manifest has no sushi-config.yaml"))?;
    let config_bytes = addressed_bytes(&sources.objects, &config_ref.content, "source")?;
    let config = std::str::from_utf8(config_bytes)
        .context("sushi-config.yaml must be UTF-8")?
        .to_string();

    let scoped_labels = closure
        .resolved
        .labels
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let (package_source, package_root) = captured_package_source(packages)?;
    let compile_view = site_engine::PackageView::new(
        package_source.clone(),
        package_root.clone(),
        Some(scoped_labels),
    );
    let store = package_store::PackageStore::for_project_with_config(
        compile_view,
        &config,
        package_root.to_string_lossy().as_ref(),
    )?;
    let config_yaml: serde_yaml::Value = serde_yaml::from_str(&config)?;
    let captured = sources
        .manifest
        .iter()
        .map(|(path, entry)| {
            addressed_bytes(&sources.objects, &entry.content, "source")
                .map(|bytes| (PathBuf::from(path.as_str()), bytes.to_vec()))
        })
        .collect::<Result<Vec<_>>>()?;
    let predefined = compiler::predefined::PredefinedPackage::load_from_project_bytes(
        &config_yaml,
        &captured,
        &store,
    );
    let predefined = predefined
        .resources()
        .iter()
        .map(|resource| {
            (
                resource.path.to_string_lossy().replace('\\', "/"),
                resource.body.as_ref().clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut fsh = BTreeMap::new();
    let mut site_files = BTreeMap::new();
    for (path, entry) in sources.manifest.iter() {
        if path.as_str() == "sushi-config.yaml" {
            continue;
        }
        let bytes = addressed_bytes(&sources.objects, &entry.content, "source")?;
        if matches!(entry.kind, SourceKind::Fsh) {
            let text = std::str::from_utf8(bytes)
                .with_context(|| format!("FSH source {path} must be UTF-8"))?;
            fsh.insert(path.as_str().to_string(), text.to_string());
        } else {
            site_files.insert(path.as_str().to_string(), bytes.to_vec());
        }
    }
    let materials = packages
        .lock
        .iter()
        .map(|(coordinate, locked)| {
            let label = coordinate.to_string();
            let bytes = packages
                .objects
                .get(&locked.content.sha256)
                .ok_or_else(|| anyhow!("package object for {label} is absent"))?;
            let material = site_engine::PackageMaterial::new(
                locked.content.clone(),
                packages
                    .declared_dependencies
                    .get(&label)
                    .cloned()
                    .unwrap_or_default(),
                std::rc::Rc::new(bytes.clone()),
            )
            .map_err(anyhow::Error::msg)?;
            Ok((label, material))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let labels = materials.keys().cloned().collect::<Vec<_>>();
    let environment = site_engine::PackageEnvironment::new(
        site_engine::PackageView::new(package_source, package_root, None),
        labels,
        materials,
    )
    .map_err(anyhow::Error::msg)?;
    Ok((
        site_engine::ProjectInputs {
            config,
            fsh,
            predefined,
            site_files,
        },
        environment,
    ))
}

/// Stable summary returned by the library and the CLI JSON envelope.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareOutcome {
    pub target: String,
    pub out: String,
    pub build_id: String,
    pub project_id: String,
    pub fhir_version: String,
    pub sources: usize,
    pub packages: usize,
    pub objects: usize,
    pub object_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceSnapshot {
    manifest: SourceManifest,
    objects: BTreeMap<Sha256Digest, Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedClosure {
    /// Every exact coordinate selected by the resolver. Different versions of
    /// one package id are legitimate when an automatic compile dependency and
    /// an exact transitive dependency select different releases.
    coordinates: BTreeSet<PackageCoordinate>,
    core: PackageCoordinate,
    resolved: site_engine::ResolvedPackageClosure,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PackageSnapshot {
    lock: PackageLock,
    objects: BTreeMap<Sha256Digest, Vec<u8>>,
    declared_dependencies: BTreeMap<String, BTreeMap<String, String>>,
    /// Complete validated files mounted for compilation/template preparation.
    /// The package lock addresses the canonical semantic payload separately;
    /// nested template/runtime inputs become explicit target artifacts.
    mounted_files: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
}

fn captured_package_source(
    packages: &PackageSnapshot,
) -> Result<(std::rc::Rc<dyn package_store::PackageSource>, PathBuf)> {
    let mut source = package_store::BundleSource::new();
    for (label, files) in &packages.mounted_files {
        if packages
            .lock
            .get(&PackageCoordinate::parse(label)?)
            .is_none()
        {
            bail!("captured mounted package {label} is absent from its package lock");
        }
        source.mount_package(label, files.clone());
    }
    if packages.mounted_files.len() != packages.lock.iter().count() {
        bail!("captured mounted package inventory differs from its package lock");
    }
    let root = source.cache_root().to_path_buf();
    Ok((std::rc::Rc::new(source), root))
}

/// Compile and project an IG exactly once, then publish its sealed target bundle.
///
/// Authored and package bytes are captured once and mounted into private
/// in-memory project/package views. Live source/cache comparisons after
/// compilation remain useful mutation diagnostics, but are not relied on for
/// correctness: even an A→B→A live mutation cannot influence the result while
/// retaining A's manifest.
pub fn prepare(config: &PrepareConfig) -> Result<PrepareOutcome> {
    if config.target != CYCLE_SITE_TARGET && config.target != PUBLISHER_SITE_TARGET {
        bail!(
            "unsupported prepare target {:?}; supported targets are {CYCLE_SITE_TARGET} and {PUBLISHER_SITE_TARGET}",
            config.target
        );
    }

    let ig_dir = canonical_existing_dir(&config.ig_dir, "IG directory")?;
    let cache_dir = canonical_existing_dir(&config.cache_dir, "package cache")?;
    reject_ambient_liquid_asset_dirs()?;
    // Resolve and reject an authored-tree destination before creating output
    // parents; otherwise a symlinked parent could mutate input/ on an error path.
    let intended_output = resolved_destination(&config.out_dir)?;
    ensure_no_source_output_overlap(&ig_dir, &intended_output)?;
    let output = crate::publication::new_directory_destination(&config.out_dir)?;
    // Recheck the actual canonical parent after creation to close the race as
    // far as the portable filesystem API permits.
    ensure_no_source_output_overlap(&ig_dir, &output)?;

    let source_before = collect_authored_sources(&ig_dir)?;
    let config_path = SourcePath::parse("sushi-config.yaml").expect("static path");
    let config_ref = source_before
        .manifest
        .get(&config_path)
        .ok_or_else(|| anyhow!("authored source manifest has no sushi-config.yaml"))?;
    let config_bytes = source_before
        .objects
        .get(&config_ref.content.sha256)
        .ok_or_else(|| anyhow!("sushi-config.yaml content object is absent"))?;
    let config_text =
        std::str::from_utf8(config_bytes).context("sushi-config.yaml must be UTF-8")?;

    let closure_before = resolve_prepare_closure(config, config_text, &cache_dir)?;
    let packages_before = collect_package_snapshot(&cache_dir, &closure_before)?;
    let (inputs, environment) =
        site_engine_inputs(&source_before, &packages_before, &closure_before)?;
    let mut engine = site_engine::SiteEngine::default();
    let generator = match config.target.as_str() {
        CYCLE_SITE_TARGET => site_engine::GeneratorSpec::Cycle {
            build_epoch_secs: config.build_epoch_secs,
            liquid_asset_dirs: vec!["input/includes".into()],
            branch: None,
            revision: None,
        },
        PUBLISHER_SITE_TARGET => site_engine::GeneratorSpec::Publisher {
            template_coordinate: config
                .template_coordinate
                .clone()
                .expect("Publisher template was checked"),
            build_epoch_secs: config.build_epoch_secs,
            active_tables: config.active_tables,
            run_uuid: config.run_uuid.clone(),
        },
        _ => unreachable!("target validated"),
    };
    let prepared = engine
        .prepare_project(
            inputs,
            closure_before.resolved.clone(),
            generator,
            environment,
        )
        .map_err(anyhow::Error::msg)?;
    let handle = prepared.site.handle.clone();
    let site_build = prepared.site.site_build;

    let source_after = collect_authored_sources(&ig_dir)?;
    if source_before != source_after {
        bail!("authored inputs changed while fig prepare was compiling; no bundle was emitted");
    }
    let closure_after = resolve_prepare_closure(config, config_text, &cache_dir)?;
    if closure_before != closure_after {
        bail!("resolved package closure changed while fig prepare was compiling; no bundle was emitted");
    }
    let packages_after = collect_package_snapshot(&cache_dir, &closure_after)?;
    if packages_before != packages_after {
        bail!("package bytes changed while fig prepare was compiling; no bundle was emitted");
    }
    drop(packages_after);

    let project_id = site_build.site_build().project().project_id.clone();
    let fhir_version = site_build.site_build().render_target().fhir_version.clone();
    validate_core_for_fhir_version(&closure_before.core, &fhir_version)?;
    let mut objects = BTreeMap::new();
    for content in bundle_content_refs(&site_build) {
        let bytes = engine
            .read_content(&handle, content.sha256.as_str())
            .map_err(anyhow::Error::msg)?;
        insert_object(&mut objects, content.clone(), bytes)?;
    }
    verify_bundle_objects(&site_build, &objects)?;
    let site_build_bytes = site_build.site_build().canonical_bytes()?;
    emit_bundle(
        &output,
        &site_build_bytes,
        &objects,
        &bundle_content_refs(&site_build),
    )?;

    Ok(PrepareOutcome {
        target: config.target.clone(),
        out: output.display().to_string(),
        build_id: site_build.site_build().build_id().to_string(),
        project_id,
        fhir_version,
        sources: site_build.site_build().project().sources.iter().count(),
        packages: site_build.site_build().package_lock().iter().count(),
        objects: objects.len(),
        object_bytes: objects.values().map(|bytes| bytes.len() as u64).sum(),
    })
}

fn resolve_prepare_closure(
    config: &PrepareConfig,
    config_text: &str,
    cache_dir: &Path,
) -> Result<ResolvedClosure> {
    let mut closure = resolve_exact_closure(config_text, cache_dir)?;
    if config.target != PUBLISHER_SITE_TARGET {
        return Ok(closure);
    }
    let template = config
        .template_coordinate
        .as_deref()
        .ok_or_else(|| anyhow!("Publisher prepare requires --template <id#version>"))?;
    let resolution = package_store::resolve_template_base_chain(
        &package_store::DiskSource,
        &package_store::TemplatePaths::new(cache_dir),
        template,
    )?;
    if let Some(missing) = resolution.missing {
        bail!("explicit package cache does not contain template dependency {missing}");
    }
    for label in resolution.chain {
        closure
            .coordinates
            .insert(PackageCoordinate::parse(&label)?);
    }
    Ok(closure)
}

fn canonical_existing_dir(path: &Path, description: &str) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect {description} {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "{description} must be a real directory, not a symlink: {}",
            path.display()
        );
    }
    path.canonicalize()
        .with_context(|| format!("canonicalize {description} {}", path.display()))
}

fn reject_ambient_liquid_asset_dirs() -> Result<()> {
    if std::env::var_os("SITE_LIQUID_ASSET_DIRS").is_some() {
        bail!(
            "SITE_LIQUID_ASSET_DIRS is not permitted for deterministic fig prepare; the target uses [\"input/includes\"]"
        );
    }
    Ok(())
}

fn ensure_no_source_output_overlap(ig_dir: &Path, bundle_out: &Path) -> Result<()> {
    let input = ig_dir.join("input");
    if bundle_out.starts_with(&input) {
        bail!(
            "bundle output may not be inside authored input/: {}",
            bundle_out.display()
        );
    }
    Ok(())
}

/// Resolve every existing prefix (including symlinks) and append only safe,
/// missing path components. This models where a future create/write will land;
/// lexical `starts_with` alone is insufficient when a parent is a symlink.
fn resolved_destination(path: &Path) -> Result<PathBuf> {
    let mut candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(&candidate) {
            Ok(_) => {
                let mut resolved = candidate.canonicalize().with_context(|| {
                    format!("canonicalize destination prefix {}", candidate.display())
                })?;
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = candidate
                    .file_name()
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| {
                        anyhow!("destination has no existing ancestor: {}", path.display())
                    })?;
                if !matches!(
                    Path::new(name).components().next(),
                    Some(Component::Normal(_))
                ) {
                    bail!("unsafe missing destination component: {}", path.display());
                }
                missing.push(name.to_os_string());
                candidate = candidate
                    .parent()
                    .ok_or_else(|| anyhow!("destination has no parent: {}", path.display()))?
                    .to_path_buf();
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect destination prefix {}", candidate.display()))
            }
        }
    }
}

fn collect_authored_sources(ig_dir: &Path) -> Result<SourceSnapshot> {
    let mut entries = Vec::new();
    let mut objects = BTreeMap::new();

    collect_authored_file(
        ig_dir,
        &ig_dir.join("sushi-config.yaml"),
        SourceKind::Config,
        &mut entries,
        &mut objects,
    )?;

    let input = ig_dir.join("input");
    match fs::symlink_metadata(&input) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!(
                    "authored input must be a real directory: {}",
                    input.display()
                );
            }
            for entry in WalkDir::new(&input).follow_links(false).sort_by_file_name() {
                let entry =
                    entry.with_context(|| format!("walk authored input {}", input.display()))?;
                if entry.path() == input {
                    continue;
                }
                let file_type = entry.file_type();
                if file_type.is_symlink() {
                    bail!(
                        "authored input symlinks are not allowed: {}",
                        entry.path().display()
                    );
                }
                if file_type.is_dir() {
                    continue;
                }
                if !file_type.is_file() {
                    bail!(
                        "authored input contains a non-regular file: {}",
                        entry.path().display()
                    );
                }
                let rel = normalized_relative_path(ig_dir, entry.path())?;
                let (kind, _) = source_kind_and_media_type(&rel);
                collect_authored_file(ig_dir, entry.path(), kind, &mut entries, &mut objects)?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("inspect {}", input.display())),
    }

    Ok(SourceSnapshot {
        manifest: SourceManifest::from_entries(entries)?,
        objects,
    })
}

fn collect_authored_file(
    ig_dir: &Path,
    path: &Path,
    kind: SourceKind,
    entries: &mut Vec<(SourcePath, SourceEntry)>,
    objects: &mut BTreeMap<Sha256Digest, Vec<u8>>,
) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect authored file {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "authored source must be a regular, non-symlink file: {}",
            path.display()
        );
    }
    let canonical = path
        .canonicalize()
        .with_context(|| format!("canonicalize authored file {}", path.display()))?;
    if !canonical.starts_with(ig_dir) {
        bail!(
            "authored source escapes the IG directory: {}",
            path.display()
        );
    }
    let rel = normalized_relative_path(ig_dir, &canonical)?;
    let source_path = SourcePath::parse(rel.clone())?;
    let bytes = fs::read(&canonical).with_context(|| format!("read authored file {rel}"))?;
    let (_, media_type) = source_kind_and_media_type(&rel);
    let content = ContentRef::of_bytes(&bytes, Some(media_type));
    insert_object(objects, content.clone(), bytes)?;
    entries.push((source_path, SourceEntry { kind, content }));
    Ok(())
}

fn normalized_relative_path(root: &Path, path: &Path) -> Result<String> {
    let rel = path
        .strip_prefix(root)
        .with_context(|| format!("{} is outside {}", path.display(), root.display()))?;
    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| anyhow!("path is not UTF-8: {}", path.display()))?,
            ),
            _ => bail!(
                "path is not normalized beneath its root: {}",
                path.display()
            ),
        }
    }
    if parts.is_empty() {
        bail!("path has no relative filename: {}", path.display());
    }
    Ok(parts.join("/"))
}

fn resolve_exact_closure(config_text: &str, cache_dir: &Path) -> Result<ResolvedClosure> {
    let source = package_store::DiskSource;
    let index = package_store::version_index_from_cache(&source, cache_dir);
    let step = package_store::resolve_project(config_text, &source, cache_dir, Some(&index))
        .context("resolve project package closure from explicit cache")?;
    if !step.satisfied {
        bail!(
            "explicit package cache does not satisfy the project closure: {}",
            serde_json::to_string(&step.missing)?
        );
    }

    closure_from_step(config_text, &step)
}

fn closure_from_step(
    config_text: &str,
    step: &package_store::ResolutionStep,
) -> Result<ResolvedClosure> {
    let resolved = site_engine::ResolvedPackageClosure::from_resolution_step(config_text, step)
        .map_err(anyhow::Error::msg)?;
    let mut coordinates = BTreeSet::new();
    for label in resolved
        .labels
        .iter()
        .chain(resolved.resolution_support.iter())
    {
        coordinates.insert(PackageCoordinate::parse(label)?);
    }
    if coordinates.is_empty() {
        bail!("resolved package closure is empty");
    }
    let declared_fhir_version = config_fhir_version(config_text)?;
    let core_id = core_package_id(&declared_fhir_version)?;
    let core_version = canonical_core_version(core_id, &declared_fhir_version);
    let core = PackageCoordinate::new(core_id, core_version)?;
    if !coordinates.contains(&core) {
        bail!("resolved closure has no expected core package {core}");
    }
    Ok(ResolvedClosure {
        coordinates,
        core,
        resolved,
    })
}

fn canonical_core_version<'a>(core_id: &str, fhir_version: &'a str) -> &'a str {
    if core_id == "hl7.fhir.r4.core" && fhir_version == "4.0.0" {
        "4.0.1"
    } else {
        fhir_version
    }
}

fn config_fhir_version(config_text: &str) -> Result<String> {
    let value: serde_yaml::Value = serde_yaml::from_str(config_text)?;
    let version = match value.get("fhirVersion") {
        Some(serde_yaml::Value::String(value)) => value.trim().to_string(),
        Some(serde_yaml::Value::Sequence(values)) => values
            .first()
            .and_then(serde_yaml::Value::as_str)
            .map(str::trim)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("empty fhirVersion sequence"))?,
        Some(serde_yaml::Value::Number(value)) => value.to_string(),
        _ => bail!("sushi-config.yaml has no fhirVersion"),
    };
    if version.is_empty() {
        bail!("sushi-config.yaml has an empty fhirVersion");
    }
    Ok(version)
}

fn core_package_id(fhir_version: &str) -> Result<&'static str> {
    let numeric = fhir_version.split('-').next().unwrap_or(fhir_version);
    let mut parts = numeric.split('.');
    match (parts.next().unwrap_or(""), parts.next().unwrap_or("")) {
        ("4", "0") => Ok("hl7.fhir.r4.core"),
        ("4", "1" | "3") => Ok("hl7.fhir.r4b.core"),
        ("5", _) => Ok("hl7.fhir.r5.core"),
        ("6", _) => Ok("hl7.fhir.r6.core"),
        _ => bail!("unsupported FHIR version for a native SiteBuild: {fhir_version}"),
    }
}

fn validate_package_component(value: &str, description: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
    {
        bail!("unsafe {description}: {value:?}");
    }
    Ok(())
}

fn collect_package_snapshot(
    cache_dir: &Path,
    closure: &ResolvedClosure,
) -> Result<PackageSnapshot> {
    let mut packages = Vec::with_capacity(closure.coordinates.len());
    let mut objects = BTreeMap::new();
    let mut declared_dependencies = BTreeMap::new();
    let mut mounted_files = BTreeMap::new();
    let mut closure_versions = package_store::VersionIndex::new();
    let mut versions_by_id: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for coordinate in &closure.coordinates {
        versions_by_id
            .entry(coordinate.package_id().to_string())
            .or_default()
            .push(coordinate.version().to_string());
    }
    for (id, versions) in versions_by_id {
        closure_versions.insert(id, versions);
    }
    for coordinate in &closure.coordinates {
        let label = coordinate.to_string();
        validate_package_component(&label, "package coordinate")?;
        let label_dir = cache_dir.join(&label);
        let label_metadata = fs::symlink_metadata(&label_dir)
            .with_context(|| format!("inspect resolved package directory {label}"))?;
        if label_metadata.file_type().is_symlink() || !label_metadata.is_dir() {
            bail!(
                "resolved package coordinate must be a real directory: {}",
                label_dir.display()
            );
        }
        let package_path = label_dir.join("package");
        let package_metadata = fs::symlink_metadata(&package_path)
            .with_context(|| format!("inspect package payload {label}"))?;
        if !package_metadata.is_dir() && !package_metadata.file_type().is_symlink() {
            bail!(
                "resolved package payload is not a directory: {}",
                package_path.display()
            );
        }
        let package_root = package_path
            .canonicalize()
            .with_context(|| format!("canonicalize package payload {label}"))?;
        if !package_root.starts_with(cache_dir) {
            bail!(
                "resolved package payload escapes the explicit cache: {} -> {}",
                package_path.display(),
                package_root.display()
            );
        }
        let material = read_normalized_package_material(&package_root, &label)?;
        let mut files = material.files;
        // Native FHIR caches put compiler metadata under `package/`, while
        // template content and core `other/` assets may be siblings of that
        // directory. BundleSource has one normalized `<label>/package/` root, so
        // capture and mount every safe sibling at its same relative name there.
        // This is the complete immutable environment that preparation consumes.
        for entry in WalkDir::new(&label_dir)
            .min_depth(1)
            .follow_links(false)
            .sort_by_file_name()
        {
            let entry =
                entry.with_context(|| format!("walk resolved package coordinate {label}"))?;
            let relative = entry
                .path()
                .strip_prefix(&label_dir)
                .expect("package entry is below coordinate root");
            if relative.starts_with("package") {
                continue;
            }
            let file_type = entry.file_type();
            if file_type.is_symlink() {
                bail!(
                    "package coordinate contains a symlink: {}",
                    entry.path().display()
                );
            }
            if file_type.is_dir() {
                continue;
            }
            if !file_type.is_file() {
                bail!(
                    "package coordinate contains a non-regular file: {}",
                    entry.path().display()
                );
            }
            let canonical = entry.path().canonicalize().with_context(|| {
                format!(
                    "canonicalize package coordinate file {}",
                    entry.path().display()
                )
            })?;
            if !canonical.starts_with(&label_dir) {
                bail!(
                    "package coordinate file escapes its root: {}",
                    entry.path().display()
                );
            }
            let relative = normalized_relative_path(&label_dir, &canonical)?;
            let bytes = fs::read(&canonical)
                .with_context(|| format!("read package coordinate file {relative}"))?;
            if files.contains_key(&relative) {
                // Metadata under native `package/` is the compiler-visible
                // meaning of a colliding name (notably `.index.db`). Top-level
                // cache sidecars are neither template content nor renderer input.
                continue;
            }
            files.insert(relative, bytes);
        }
        declared_dependencies.insert(label.clone(), material.declared_dependencies.clone());
        let content = ContentRef::of_bytes(
            &material.payload,
            Some(package_store::NORMALIZED_PACKAGE_MEDIA_TYPE),
        );
        insert_object(&mut objects, content.clone(), material.payload)?;
        mounted_files.insert(label.clone(), files);
        let dependencies = material
            .declared_dependencies
            .iter()
            .filter_map(|(id, requested)| {
                package_store::resolve_version(Some(&closure_versions), id, requested)
                    .ok()
                    .and_then(|version| PackageCoordinate::new(id, &version).ok())
                    .filter(|dependency| closure.coordinates.contains(dependency))
            })
            .collect();
        packages.push(LockedPackage {
            coordinate: coordinate.clone(),
            content,
            dependencies,
        });
    }
    Ok(PackageSnapshot {
        lock: PackageLock::from_packages(packages)?,
        objects,
        declared_dependencies,
        mounted_files,
    })
}

fn read_normalized_package_material(
    root: &Path,
    label: &str,
) -> Result<package_store::NormalizedPackageMaterial> {
    let mut files = BTreeMap::new();
    for entry in WalkDir::new(root).follow_links(false).sort_by_file_name() {
        let entry = entry.with_context(|| format!("walk package payload {}", root.display()))?;
        if entry.path() == root {
            continue;
        }
        let file_type = entry.file_type();
        if file_type.is_symlink() {
            bail!(
                "nested package symlinks are not allowed: {}",
                entry.path().display()
            );
        }
        if file_type.is_dir() {
            continue;
        }
        if !file_type.is_file() {
            bail!(
                "package contains a non-regular file: {}",
                entry.path().display()
            );
        }
        let canonical = entry
            .path()
            .canonicalize()
            .with_context(|| format!("canonicalize package file {}", entry.path().display()))?;
        if !canonical.starts_with(root) {
            bail!(
                "package file escapes its payload root: {}",
                entry.path().display()
            );
        }
        let relative = normalized_relative_path(root, &canonical)?;
        let bytes = fs::read(&canonical)
            .with_context(|| format!("read package file {}", canonical.display()))?;
        if files.insert(relative.clone(), bytes).is_some() {
            bail!("package repeats normalized path {relative:?}");
        }
    }
    package_store::normalize_package_material(label, files)
        .with_context(|| format!("normalize captured package material {label}"))
}

fn addressed_bytes<'a>(
    objects: &'a BTreeMap<Sha256Digest, Vec<u8>>,
    content: &ContentRef,
    description: &str,
) -> Result<&'a [u8]> {
    let bytes = objects
        .get(&content.sha256)
        .ok_or_else(|| anyhow!("captured {description} object {} is absent", content.sha256))?;
    if bytes.len() as u64 != content.byte_length || Sha256Digest::of_bytes(bytes) != content.sha256
    {
        bail!(
            "captured {description} object {} fails digest/length verification",
            content.sha256
        );
    }
    Ok(bytes)
}

fn validate_core_for_fhir_version(core: &PackageCoordinate, fhir_version: &str) -> Result<()> {
    let expected_id = core_package_id(fhir_version)?;
    let expected_version = if expected_id == "hl7.fhir.r4.core" && fhir_version == "4.0.0" {
        "4.0.1"
    } else {
        fhir_version
    };
    if core.package_id() != expected_id || core.version() != expected_version {
        bail!(
            "produced IG FHIR version {fhir_version} requires {expected_id}#{expected_version}, but prepare used {core}"
        );
    }
    Ok(())
}

fn source_kind_and_media_type(path: &str) -> (SourceKind, &'static str) {
    let lower = path.to_ascii_lowercase();
    let media_type = if lower.ends_with(".fsh") {
        "text/fhir-shorthand"
    } else if lower.ends_with(".json") {
        "application/json"
    } else if lower.ends_with(".yaml") || lower.ends_with(".yml") {
        "application/yaml"
    } else if lower.ends_with(".md") {
        "text/markdown"
    } else if lower.ends_with(".html") || lower.ends_with(".xhtml") {
        "text/html"
    } else if lower.ends_with(".css") {
        "text/css"
    } else if lower.ends_with(".js") {
        "text/javascript"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else {
        "application/octet-stream"
    };
    let kind = if path == "sushi-config.yaml" {
        SourceKind::Config
    } else if path.starts_with("input/fsh/") || lower.ends_with(".fsh") {
        SourceKind::Fsh
    } else if path.starts_with("input/resources/") {
        SourceKind::PredefinedResource
    } else if ["input/pagecontent/", "input/pages/", "input/resource-docs/"]
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        SourceKind::Page
    } else if path.starts_with("input/images/")
        || matches!(
            path.rsplit_once('.')
                .map(|(_, extension)| extension.to_ascii_lowercase())
                .as_deref(),
            Some(
                "css"
                    | "js"
                    | "png"
                    | "gif"
                    | "jpg"
                    | "jpeg"
                    | "svg"
                    | "ico"
                    | "woff"
                    | "woff2"
                    | "ttf"
            )
        )
    {
        SourceKind::Asset
    } else {
        SourceKind::Other {
            name: "authored_input".into(),
        }
    };
    (kind, media_type)
}

fn insert_object(
    objects: &mut BTreeMap<Sha256Digest, Vec<u8>>,
    content: ContentRef,
    bytes: Vec<u8>,
) -> Result<()> {
    if content.byte_length != bytes.len() as u64 || content.sha256 != Sha256Digest::of_bytes(&bytes)
    {
        bail!("content object does not match its declared digest/length");
    }
    if let Some(existing) = objects.insert(content.sha256.clone(), bytes.clone()) {
        if existing != bytes {
            bail!(
                "SHA-256 collision while assembling bundle object {}",
                content.sha256
            );
        }
    }
    Ok(())
}

fn verify_bundle_objects(
    closed: &ClosedSiteBuild,
    objects: &BTreeMap<Sha256Digest, Vec<u8>>,
) -> Result<()> {
    let build = closed.site_build();
    let mut refs = Vec::new();
    refs.extend(
        build
            .project()
            .sources
            .iter()
            .map(|(_, source)| &source.content),
    );
    refs.extend(
        build
            .package_lock()
            .iter()
            .map(|(_, package)| &package.content),
    );
    for (_, artifact) in build.artifacts().iter() {
        if let ArtifactState::Ready { content } = &artifact.state {
            refs.push(content);
        }
    }
    for content in refs {
        let bytes = objects
            .get(&content.sha256)
            .ok_or_else(|| anyhow!("bundle is missing addressed object {}", content.sha256))?;
        if bytes.len() as u64 != content.byte_length
            || Sha256Digest::of_bytes(bytes) != content.sha256
        {
            bail!(
                "bundle object {} fails digest/length verification",
                content.sha256
            );
        }
    }
    build.verify()?;
    Ok(())
}

fn bundle_content_refs(closed: &ClosedSiteBuild) -> Vec<ContentRef> {
    let build = closed.site_build();
    let mut refs: Vec<_> = build
        .project()
        .sources
        .iter()
        .map(|(_, source)| source.content.clone())
        .collect();
    refs.extend(
        build
            .package_lock()
            .iter()
            .map(|(_, package)| package.content.clone()),
    );
    refs.extend(build.artifacts().iter().filter_map(|(_, artifact)| {
        if let ArtifactState::Ready { content } = &artifact.state {
            Some(content.clone())
        } else {
            None
        }
    }));
    refs
}

fn emit_bundle(
    output: &Path,
    site_build_bytes: &[u8],
    objects: &BTreeMap<Sha256Digest, Vec<u8>>,
    expected_refs: &[ContentRef],
) -> Result<()> {
    if fs::symlink_metadata(output).is_ok() {
        bail!("output already exists: {}", output.display());
    }
    let parent = output.parent().expect("output_destination supplies parent");
    let temp = tempfile::Builder::new()
        .prefix(".fig-prepare-")
        .tempdir_in(parent)
        .with_context(|| format!("create temporary bundle beside {}", output.display()))?;
    let store = FileContentStore::create(temp.path().join("objects/sha256"))
        .context("create bundle content store")?;
    fs::write(temp.path().join("site-build.json"), site_build_bytes)?;
    for (digest, bytes) in objects {
        let content = ContentRef {
            sha256: digest.clone(),
            byte_length: bytes.len() as u64,
            media_type: None,
        };
        store
            .put(&content, bytes)
            .with_context(|| format!("publish bundle object {digest}"))?;
    }
    // Re-read every reference carried by the closed handoff. This verifies the
    // filesystem view consumers will observe, including each producer-declared
    // media type, rather than trusting only the in-memory assembly above.
    for content in expected_refs {
        store
            .read(content)
            .with_context(|| format!("verify closed-build object {}", content.sha256))?;
    }
    if fs::symlink_metadata(output).is_ok() {
        bail!(
            "output appeared while bundle was being built: {}",
            output.display()
        );
    }
    crate::publication::rename_no_replace(temp.path(), output)
        .with_context(|| format!("publish bundle atomically at {}", output.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use package_store::{PackageRequest, ResolutionStep};

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn package(cache: &Path, id: &str, version: &str, dependencies: Value) {
        let body = serde_json::json!({
            "name": id,
            "version": version,
            "fhirVersions": ["4.0.1"],
            "dependencies": dependencies,
        });
        write(
            &cache.join(format!("{id}#{version}/package/package.json")),
            serde_json::to_string(&body).unwrap().as_bytes(),
        );
    }

    fn closure(
        coordinates: BTreeSet<PackageCoordinate>,
        core: PackageCoordinate,
    ) -> ResolvedClosure {
        let labels = coordinates.iter().map(ToString::to_string).collect();
        ResolvedClosure {
            coordinates,
            core,
            resolved: site_engine::ResolvedPackageClosure {
                config_sha256: Sha256Digest::of_bytes(b""),
                resolution_support: BTreeSet::new(),
                labels,
            },
        }
    }

    #[test]
    fn source_manifest_captures_config_and_every_regular_input_file() {
        let temp = tempfile::tempdir().unwrap();
        write(
            &temp.path().join("sushi-config.yaml"),
            b"id: demo\nfhirVersion: 4.0.1\n",
        );
        write(&temp.path().join("input/fsh/demo.fsh"), b"Profile: Demo\n");
        write(&temp.path().join("input/images/pixel.bin"), &[0, 1, 2]);
        let root = temp.path().canonicalize().unwrap();
        let snapshot = collect_authored_sources(&root).unwrap();
        let paths: Vec<String> = snapshot
            .manifest
            .iter()
            .map(|(path, _)| path.to_string())
            .collect();
        assert_eq!(
            paths,
            [
                "input/fsh/demo.fsh",
                "input/images/pixel.bin",
                "sushi-config.yaml"
            ]
        );
        assert_eq!(snapshot.objects.len(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn source_manifest_rejects_authored_symlinks() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        write(
            &temp.path().join("sushi-config.yaml"),
            b"fhirVersion: 4.0.1\n",
        );
        write(&temp.path().join("outside.fsh"), b"Profile: Outside\n");
        fs::create_dir_all(temp.path().join("input/fsh")).unwrap();
        symlink(
            "../../outside.fsh",
            temp.path().join("input/fsh/escape.fsh"),
        )
        .unwrap();
        let error = collect_authored_sources(&temp.path().canonicalize().unwrap()).unwrap_err();
        assert!(error.to_string().contains("symlinks are not allowed"));
    }

    #[test]
    fn package_payload_separates_semantic_lock_from_complete_mounted_files() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().canonicalize().unwrap();
        package(&cache, "example", "1.0.0", serde_json::json!({}));
        write(&cache.join("example#1.0.0/package/nested/a.txt"), b"alpha");
        write(
            &cache.join("example#1.0.0/content/layouts/page.html"),
            b"template layout",
        );
        write(
            &cache.join("example#1.0.0/other/spec.internals"),
            b"renderer facts",
        );
        let coordinate = PackageCoordinate::new("example", "1.0.0").unwrap();
        let closure = closure(BTreeSet::from([coordinate.clone()]), coordinate);
        let snapshot = collect_package_snapshot(&cache, &closure).unwrap();
        let package_root = cache.join("example#1.0.0/package");
        let material = read_normalized_package_material(&package_root, "example#1.0.0").unwrap();
        let files = material.files;
        assert!(files.contains_key(package_store::derived_index::SIDECAR_NAME));
        assert_eq!(files["nested/a.txt"], b"alpha");
        let locked = snapshot.lock.iter().next().unwrap().1;
        assert_eq!(
            locked.content.sha256,
            Sha256Digest::of_bytes(&material.payload)
        );
        assert_eq!(
            snapshot.objects.get(&locked.content.sha256),
            Some(&material.payload)
        );
        for (path, bytes) in &files {
            assert_eq!(&snapshot.mounted_files["example#1.0.0"][path], bytes);
        }
        assert_eq!(
            snapshot.mounted_files["example#1.0.0"]["content/layouts/page.html"],
            b"template layout"
        );
        assert_eq!(
            snapshot.mounted_files["example#1.0.0"]["other/spec.internals"],
            b"renderer facts"
        );
        assert!(!package_store::decode_normalized_package(&material.payload)
            .unwrap()
            .contains_key("nested/a.txt"));
    }

    #[test]
    fn captured_package_view_is_detached_from_live_cache_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().canonicalize().unwrap();
        package(&cache, "example", "1.0.0", serde_json::json!({}));
        let resource_path = cache.join("example#1.0.0/package/StructureDefinition-demo.json");
        let resource = |name: &str| {
            serde_json::to_vec(&serde_json::json!({
                "resourceType": "StructureDefinition",
                "id": "demo",
                "url": "https://example.org/StructureDefinition/demo",
                "name": name,
                "kind": "resource",
                "type": "Patient",
                "derivation": "constraint"
            }))
            .unwrap()
        };
        write(&resource_path, &resource("CapturedDefinition"));
        write(
            &cache.join("example#1.0.0/content/layouts/page.html"),
            b"captured layout",
        );
        let coordinate = PackageCoordinate::new("example", "1.0.0").unwrap();
        let closure = closure(BTreeSet::from([coordinate.clone()]), coordinate);
        let snapshot = collect_package_snapshot(&cache, &closure).unwrap();

        // The live package changes after capture. Both the semantic compiler
        // view and nested template view must remain bound to captured A.
        write(&resource_path, &resource("LiveMutation"));
        write(
            &cache.join("example#1.0.0/content/layouts/page.html"),
            b"live mutation",
        );

        let (source, root) = captured_package_source(&snapshot).unwrap();
        let view = site_engine::PackageView::new(
            source.clone(),
            root.clone(),
            Some(BTreeSet::from(["example#1.0.0".into()])),
        );
        let config = "fhirVersion: 4.0.1\ndependencies:\n  example: 1.0.0\n";
        let store = package_store::PackageStore::for_project_with_config(
            view,
            config,
            root.to_str().unwrap(),
        )
        .unwrap();
        let found = store
            .fish_for_fhir(
                "https://example.org/StructureDefinition/demo",
                package_store::ALL_FISH_TYPES,
            )
            .unwrap();
        assert_eq!(found["name"], "CapturedDefinition");
        assert_eq!(
            source
                .read(&root.join("example#1.0.0/package/content/layouts/page.html"))
                .unwrap(),
            b"captured layout"
        );
    }

    #[test]
    fn package_lock_keeps_exact_dependency_edges_across_multiple_versions() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().canonicalize().unwrap();
        package(
            &cache,
            "legacy-root",
            "1.0.0",
            serde_json::json!({"hl7.terminology.r4": "7.1.0"}),
        );
        package(
            &cache,
            "current-root",
            "1.0.0",
            serde_json::json!({"hl7.terminology.r4": "latest"}),
        );
        package(&cache, "hl7.terminology.r4", "7.1.0", serde_json::json!({}));
        package(&cache, "hl7.terminology.r4", "7.2.0", serde_json::json!({}));
        let legacy = PackageCoordinate::new("legacy-root", "1.0.0").unwrap();
        let current = PackageCoordinate::new("current-root", "1.0.0").unwrap();
        let terminology_71 = PackageCoordinate::new("hl7.terminology.r4", "7.1.0").unwrap();
        let terminology_72 = PackageCoordinate::new("hl7.terminology.r4", "7.2.0").unwrap();
        let closure = closure(
            BTreeSet::from([
                legacy.clone(),
                current.clone(),
                terminology_71.clone(),
                terminology_72.clone(),
            ]),
            legacy.clone(),
        );

        let snapshot = collect_package_snapshot(&cache, &closure).unwrap();
        assert_eq!(
            snapshot.lock.get(&legacy).unwrap().dependencies,
            BTreeSet::from([terminology_71])
        );
        assert_eq!(
            snapshot.lock.get(&current).unwrap().dependencies,
            BTreeSet::from([terminology_72])
        );
        assert_eq!(snapshot.lock.iter().count(), 4);
    }

    #[test]
    fn exact_closure_retains_multiple_versions_of_one_package_id() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().canonicalize().unwrap();
        package(&cache, "hl7.fhir.r4.core", "4.0.1", serde_json::json!({}));
        package(
            &cache,
            "hl7.fhir.uv.tools.r4",
            "1.0.0",
            serde_json::json!({}),
        );
        package(&cache, "hl7.terminology.r4", "1.0.0", serde_json::json!({}));
        package(
            &cache,
            "hl7.fhir.uv.extensions.r4",
            "1.0.0",
            serde_json::json!({}),
        );
        let closure = resolve_exact_closure("fhirVersion: 4.0.1\n", &cache).unwrap();
        assert_eq!(closure.coordinates.len(), 4);
        assert_eq!(closure.core.to_string(), "hl7.fhir.r4.core#4.0.1");

        let multi_version = ResolutionStep {
            resolver_schema: package_store::RESOLVER_SCHEMA,
            compile_set: vec![
                PackageRequest {
                    package_id: "hl7.fhir.r4.core".into(),
                    version: "4.0.1".into(),
                },
                PackageRequest {
                    package_id: "hl7.terminology.r4".into(),
                    version: "7.2.0".into(),
                },
            ],
            context_closure: vec![
                PackageRequest {
                    package_id: "hl7.fhir.r4.core".into(),
                    version: "4.0.0".into(),
                },
                PackageRequest {
                    package_id: "hl7.fhir.r4.core".into(),
                    version: "4.0.1".into(),
                },
                PackageRequest {
                    package_id: "hl7.terminology.r4".into(),
                    version: "7.1.0".into(),
                },
            ],
            resolution_support: Vec::new(),
            missing: Vec::new(),
            satisfied: true,
            mutable_requests: Vec::new(),
        };
        let closure = closure_from_step("fhirVersion: 4.0.1\n", &multi_version).unwrap();
        let labels: Vec<String> = closure
            .coordinates
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            labels,
            [
                "hl7.fhir.r4.core#4.0.0",
                "hl7.fhir.r4.core#4.0.1",
                "hl7.terminology.r4#7.1.0",
                "hl7.terminology.r4#7.2.0",
            ]
        );
        assert_eq!(closure.core.to_string(), "hl7.fhir.r4.core#4.0.1");
    }

    #[cfg(unix)]
    #[test]
    fn package_root_symlink_may_not_escape_the_explicit_cache() {
        use std::os::unix::fs::symlink;

        let cache_temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        package(outside.path(), "example", "1.0.0", serde_json::json!({}));
        let label_dir = cache_temp.path().join("example#1.0.0");
        fs::create_dir_all(&label_dir).unwrap();
        symlink(
            outside.path().join("example#1.0.0/package"),
            label_dir.join("package"),
        )
        .unwrap();
        let coordinate = PackageCoordinate::new("example", "1.0.0").unwrap();
        let closure = closure(BTreeSet::from([coordinate.clone()]), coordinate);
        let error = collect_package_snapshot(&cache_temp.path().canonicalize().unwrap(), &closure)
            .unwrap_err();
        assert!(error.to_string().contains("escapes the explicit cache"));
    }

    #[test]
    fn bundle_directory_is_published_once_with_addressed_objects() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("closed-build");
        let body = b"object".to_vec();
        let digest = Sha256Digest::of_bytes(&body);
        let objects = BTreeMap::from([(digest.clone(), body.clone())]);
        let expected = ContentRef {
            sha256: digest.clone(),
            byte_length: body.len() as u64,
            media_type: Some("application/octet-stream".into()),
        };
        emit_bundle(
            &output,
            b"{\"schemaVersion\":\"site-build/v1\"}",
            &objects,
            &[expected],
        )
        .unwrap();
        assert_eq!(
            fs::read(output.join("objects/sha256").join(digest.as_str())).unwrap(),
            body
        );
        assert!(output.join("site-build.json").is_file());
        let error = emit_bundle(&output, b"different", &objects, &[]).unwrap_err();
        assert!(error.to_string().contains("output already exists"));
    }

    #[test]
    fn atomic_publication_never_replaces_a_destination_that_appeared() {
        let temp = tempfile::tempdir().unwrap();
        let staging = temp.path().join("staging");
        let destination = temp.path().join("destination");
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("new"), b"new").unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("prior"), b"prior").unwrap();

        let error = crate::publication::rename_no_replace(&staging, &destination).unwrap_err();
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::AlreadyExists
                | std::io::ErrorKind::DirectoryNotEmpty
                | std::io::ErrorKind::PermissionDenied
                | std::io::ErrorKind::Other
        ));
        assert_eq!(fs::read(destination.join("prior")).unwrap(), b"prior");
        assert_eq!(fs::read(staging.join("new")).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn bundle_output_cannot_reach_authored_input_through_a_symlink_parent() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let ig = temp.path().join("ig");
        fs::create_dir_all(ig.join("input")).unwrap();
        symlink(ig.join("input"), temp.path().join("redirect")).unwrap();
        let ig = ig.canonicalize().unwrap();
        let output = resolved_destination(&temp.path().join("redirect/generated")).unwrap();
        let error = ensure_no_source_output_overlap(&ig, &output).unwrap_err();
        assert!(error.to_string().contains("inside authored input"));
    }
}

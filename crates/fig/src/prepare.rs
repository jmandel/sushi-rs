//! Native production of a callback-free external-builder bundle.
//!
//! This module is deliberately library composition rather than CLI business
//! logic. It resolves one exact package closure from an explicit cache, runs the
//! native `site_db` pipeline once, derives the project identity from that
//! pipeline's produced IG/rows, closes the shared SiteBuild compatibility
//! projection, and atomically emits a filesystem CAS bundle.
//!
//! The bundle layout is intentionally small and transport-neutral:
//!
//! ```text
//! <out>/site-build.json
//! <out>/objects/sha256/<lowercase digest>
//! ```
//!
//! `site-build.json` is the canonical `ClosedSiteBuild` value. Every content
//! reference it contains (authored source, normalized package payload, and the
//! Cycle row projection) is present in `objects/sha256/` and is verified before
//! publication. Compilation never re-reads the live project/cache after capture:
//! both are reconstructed in a private staged filesystem from those exact CAS
//! bytes, closing the A→B→A/TOCTOU gap between identity and execution.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::Value;
use site_build::{
    cycle_semantic, site_db_compat, ArtifactState, ClosedSiteBuild, ContentRef, LockedPackage,
    PackageCoordinate, PackageLock, ProducerRef, ProjectRevision, RenderMode, RenderTarget,
    Sha256Digest, SourceEntry, SourceKind, SourceManifest, SourcePath,
};
use walkdir::WalkDir;

/// Preferred typed external-builder contract. V1 remains available only as a
/// migration producer/reader and can still be selected explicitly.
pub const CYCLE_SITE_TARGET: &str = cycle_semantic::TARGET;
pub const CYCLE_SITE_TARGET_V1: &str = "cycle-site/v1";

/// All host inputs for one closed bundle. Every path is explicit; this API never
/// consults a default package cache and never performs package acquisition.
#[derive(Clone, Debug)]
pub struct PrepareConfig {
    pub ig_dir: PathBuf,
    pub sushi_out: PathBuf,
    pub cache_dir: PathBuf,
    pub out_dir: PathBuf,
    pub target: String,
    pub build_epoch_secs: i64,
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
    /// Exactly one coordinate per package id, sorted by package id.
    by_id: BTreeMap<String, PackageCoordinate>,
    core: PackageCoordinate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PackageSnapshot {
    lock: PackageLock,
    objects: BTreeMap<Sha256Digest, Vec<u8>>,
}

/// Private filesystem view reconstructed only from captured content. Keeping
/// the `TempDir` owned here guarantees the paths remain valid for the build and
/// are removed afterward.
struct StagedBuildInputs {
    _root: tempfile::TempDir,
    ig_dir: PathBuf,
    cache_dir: PathBuf,
}

impl Drop for StagedBuildInputs {
    fn drop(&mut self) {
        // Restore owner-write permission so TempDir can remove the private tree.
        // Failure here is intentionally best-effort during cleanup only.
        let _ = set_tree_read_only(&self.ig_dir, false);
        let _ = set_tree_read_only(&self.cache_dir, false);
    }
}

/// Compile and project an IG exactly once, then publish a sealed Cycle bundle.
///
/// Authored and package bytes are captured once, reconstructed into a private
/// staged filesystem, and only that staged view is compiled. Live source/cache
/// comparisons after compilation remain useful mutation diagnostics, but are
/// not relied on for correctness: even an A→B→A live mutation cannot influence
/// the rows while retaining A's manifest.
pub fn prepare(config: &PrepareConfig) -> Result<PrepareOutcome> {
    if !matches!(
        config.target.as_str(),
        CYCLE_SITE_TARGET | CYCLE_SITE_TARGET_V1
    ) {
        bail!(
            "unsupported prepare target {:?}; supported targets are {CYCLE_SITE_TARGET} (preferred) and {CYCLE_SITE_TARGET_V1}",
            config.target
        );
    }

    let ig_dir = canonical_existing_dir(&config.ig_dir, "IG directory")?;
    let cache_dir = canonical_existing_dir(&config.cache_dir, "package cache")?;
    reject_ambient_liquid_asset_dirs()?;
    require_new_sushi_output(&config.sushi_out)?;
    ensure_output_trees_are_disjoint(&config.sushi_out, &config.out_dir)?;
    // Resolve and reject an authored-tree destination before creating output
    // parents; otherwise a symlinked parent could mutate input/ on an error path.
    let intended_output = resolved_destination(&config.out_dir)?;
    ensure_no_source_output_overlap(&ig_dir, &config.sushi_out, &intended_output)?;
    let output = output_destination(&config.out_dir)?;
    // Recheck the actual canonical parent after creation to close the race as
    // far as the portable filesystem API permits.
    ensure_no_source_output_overlap(&ig_dir, &config.sushi_out, &output)?;

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

    let closure_before = resolve_exact_closure(config_text, &cache_dir)?;
    let packages_before = collect_package_snapshot(&cache_dir, &closure_before)?;
    let staged = stage_build_inputs(&source_before, &packages_before)?;
    verify_staged_inputs(
        &staged,
        config_text,
        &source_before,
        &closure_before,
        &packages_before,
    )?;
    // Recheck after package/source preparation so a concurrently-created output
    // cannot introduce stale generated resources.
    require_new_sushi_output(&config.sushi_out)?;

    let build_config = site_db::BuildConfig {
        ig_dir: staged.ig_dir.clone(),
        sushi_out: config.sushi_out.clone(),
        cache_dir: staged.cache_dir.clone(),
        // `site_db::build` returns the row model and never writes this path.
        out_db: output.with_extension("unused-site-db"),
        build_epoch_secs: config.build_epoch_secs,
        branch: None,
        revision: None,
        run_sushi: true,
        core_package: closure_before.core.to_string(),
        layer_b: snapshot_gen::LayerBOptions::default(),
    };
    let built = site_db::build(&build_config, None).context("native site_db build")?;

    // Detect any unexpected mutation by the pipeline itself. This is not a live
    // ABA defense (the staged paths already provide that); it asserts the staged
    // filesystem remained the exact value reconstructed from the manifest/lock.
    verify_staged_inputs(
        &staged,
        config_text,
        &source_before,
        &closure_before,
        &packages_before,
    )?;

    let source_after = collect_authored_sources(&ig_dir)?;
    if source_before != source_after {
        bail!("authored inputs changed while fig prepare was compiling; no bundle was emitted");
    }
    let closure_after = resolve_exact_closure(config_text, &cache_dir)?;
    if closure_before != closure_after {
        bail!("resolved package closure changed while fig prepare was compiling; no bundle was emitted");
    }
    let packages_after = collect_package_snapshot(&cache_dir, &closure_after)?;
    if packages_before != packages_after {
        bail!("package bytes changed while fig prepare was compiling; no bundle was emitted");
    }
    drop(packages_after);

    let (project_id, fhir_version) = produced_identity(&built.db)?;
    validate_core_for_fhir_version(&closure_before.core, &fhir_version)?;
    let revision = format!(
        "sources-sha256:{}",
        site_build::sha256_canonical(&source_before.manifest)?
    );
    let project = ProjectRevision {
        project_id: project_id.clone(),
        revision,
        sources: source_before.manifest.clone(),
    };
    let render_target = RenderTarget {
        renderer: ProducerRef::new(
            "cycle-site",
            if config.target == CYCLE_SITE_TARGET {
                "2"
            } else {
                "1"
            },
        ),
        mode: RenderMode::ExternalBuilder,
        fhir_version: fhir_version.clone(),
        template: None,
        parameters: BTreeMap::from([
            ("buildEpochSecs".into(), config.build_epoch_secs.to_string()),
            ("contract".into(), config.target.clone()),
            ("liquidAssetDirs".into(), "input/includes".into()),
        ]),
    };
    let (site_build, projected_objects) = if config.target == CYCLE_SITE_TARGET {
        let projection = cycle_semantic::close_projection(
            &built.db,
            cycle_semantic::CycleProjectionInput {
                project,
                package_lock: packages_before.lock.clone(),
                render_target,
                diagnostics: BTreeSet::new(),
            },
        )
        .context("close typed Cycle semantic projection")?;
        (projection.site_build, projection.objects)
    } else {
        let projection = site_db_compat::close_projection(
            &built.db,
            site_db_compat::CloseProjectionInput {
                project,
                package_lock: packages_before.lock.clone(),
                render_target,
                diagnostics: BTreeSet::new(),
            },
        )
        .context("close legacy Cycle site.db projection")?;
        let content = ContentRef::of_bytes(&projection.bytes, Some("application/json"));
        (
            projection.site_build,
            BTreeMap::from([(content.sha256, projection.bytes)]),
        )
    };

    let mut objects = source_before.objects;
    merge_objects(&mut objects, packages_before.objects)?;
    for (digest, bytes) in projected_objects {
        insert_object(
            &mut objects,
            ContentRef {
                sha256: digest,
                byte_length: bytes.len() as u64,
                media_type: None,
            },
            bytes,
        )?;
    }
    verify_bundle_objects(&site_build, &objects)?;
    let site_build_bytes = site_build.site_build().canonical_bytes()?;
    emit_bundle(&output, &site_build_bytes, &objects)?;

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

fn require_new_sushi_output(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect --sushi-out {}", path.display())),
        Ok(_) => bail!(
            "--sushi-out must name a new, nonexistent directory so stale generated resources cannot enter the build: {}",
            path.display()
        ),
    }
}

fn ensure_output_trees_are_disjoint(sushi_out: &Path, bundle_out: &Path) -> Result<()> {
    let sushi = resolved_destination(sushi_out)?;
    let bundle = resolved_destination(bundle_out)?;
    if sushi.starts_with(&bundle) || bundle.starts_with(&sushi) {
        bail!(
            "--sushi-out and --out must be disjoint directories: {} and {}",
            sushi.display(),
            bundle.display()
        );
    }
    Ok(())
}

/// Resolve the destination's parent now, reject any existing entry (including a
/// broken symlink), and return the actual path the final atomic rename will use.
fn output_destination(path: &Path) -> Result<PathBuf> {
    if fs::symlink_metadata(path).is_ok() {
        bail!("output already exists: {}", path.display());
    }
    let name = path
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow!("output must name a new directory: {}", path.display()))?;
    if Path::new(name)
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("unsafe output directory name: {}", path.display());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("create output parent {}", parent.display()))?;
    let parent = parent
        .canonicalize()
        .with_context(|| format!("canonicalize output parent {}", parent.display()))?;
    let output = parent.join(name);
    if fs::symlink_metadata(&output).is_ok() {
        bail!("output already exists: {}", output.display());
    }
    Ok(output)
}

fn ensure_no_source_output_overlap(
    ig_dir: &Path,
    sushi_out: &Path,
    bundle_out: &Path,
) -> Result<()> {
    let input = ig_dir.join("input");
    for destination in [
        resolved_destination(sushi_out)?,
        resolved_destination(&sushi_out.join("fsh-generated"))?,
    ] {
        if destination.starts_with(&input) {
            bail!(
                "--sushi-out resolves inside authored input/: {} -> {}",
                sushi_out.display(),
                destination.display()
            );
        }
    }
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
    let mut by_id = BTreeMap::new();
    for request in step.compile_set.iter().chain(&step.context_closure) {
        validate_package_component(&request.package_id, "package id")?;
        validate_package_component(&request.version, "package version")?;
        let coordinate = PackageCoordinate::new(&request.package_id, &request.version)?;
        if let Some(prior) = by_id.insert(request.package_id.clone(), coordinate.clone()) {
            if prior != coordinate {
                bail!(
                    "resolved closure contains conflicting versions for {}: {} and {}",
                    request.package_id,
                    prior,
                    coordinate
                );
            }
        }
    }
    if by_id.is_empty() {
        bail!("resolved package closure is empty");
    }
    let declared_fhir_version = config_fhir_version(config_text)?;
    let core_id = core_package_id(&declared_fhir_version)?;
    let core = by_id
        .get(core_id)
        .cloned()
        .ok_or_else(|| anyhow!("resolved closure has no expected core package {core_id}"))?;
    Ok(ResolvedClosure { by_id, core })
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
    let mut packages = Vec::with_capacity(closure.by_id.len());
    let mut objects = BTreeMap::new();
    for coordinate in closure.by_id.values() {
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
        let content = ContentRef::of_bytes(
            &material.payload,
            Some(package_store::NORMALIZED_PACKAGE_MEDIA_TYPE),
        );
        insert_object(&mut objects, content.clone(), material.payload)?;
        let dependencies = material
            .declared_dependencies
            .keys()
            .filter_map(|id| closure.by_id.get(id).cloned())
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
    })
}

fn read_normalized_package_material(
    root: &Path,
    label: &str,
) -> Result<package_store::NormalizedPackageMaterial> {
    // Security scan the entire tree even though the shared browser bundle shape
    // intentionally consumes only package/ top-level files.
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
        normalized_relative_path(root, &canonical)?;
    }

    // Fig's current Cycle lock intentionally captures the compiler-visible
    // top-level package projection. The browser transport may additionally
    // retain validated nested template content for the native-template path;
    // that content is not an implicit input to this Cycle target.
    let bundle = package_acquisition::build_bundle(root)
        .with_context(|| format!("build browser-equivalent package bundle {}", root.display()))?;
    package_acquisition::read_normalized_bundle(label, &bundle)
        .with_context(|| format!("normalize browser-equivalent package bundle {label}"))
}

/// Inverse of [`package_store::encode_normalized_package`]. The decoder rejects non-canonical
/// order and duplicate/unsafe names so the bytes cannot stage a different tree
/// from the one whose digest appears in the package lock.
fn decode_package_payload(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    fn take_len(bytes: &[u8], cursor: &mut usize, label: &str) -> Result<usize> {
        let end = cursor
            .checked_add(8)
            .ok_or_else(|| anyhow!("normalized package {label} length overflow"))?;
        let raw: [u8; 8] = bytes
            .get(*cursor..end)
            .ok_or_else(|| anyhow!("truncated normalized package {label} length"))?
            .try_into()
            .expect("eight-byte slice");
        *cursor = end;
        usize::try_from(u64::from_be_bytes(raw))
            .map_err(|_| anyhow!("normalized package {label} length exceeds this host"))
    }

    fn take<'a>(bytes: &'a [u8], cursor: &mut usize, len: usize, label: &str) -> Result<&'a [u8]> {
        let end = cursor
            .checked_add(len)
            .ok_or_else(|| anyhow!("normalized package {label} length overflow"))?;
        let value = bytes
            .get(*cursor..end)
            .ok_or_else(|| anyhow!("truncated normalized package {label}"))?;
        *cursor = end;
        Ok(value)
    }

    let mut cursor = 0usize;
    let mut files = BTreeMap::new();
    let mut prior: Option<String> = None;
    while cursor < bytes.len() {
        let name_len = take_len(bytes, &mut cursor, "name")?;
        let name = std::str::from_utf8(take(bytes, &mut cursor, name_len, "name")?)
            .context("normalized package filename is not UTF-8")?
            .to_string();
        validate_package_component(&name, "normalized package filename")?;
        if prior.as_ref().is_some_and(|prior| prior >= &name) {
            bail!("normalized package filenames are not in strict sorted order");
        }
        let body_len = take_len(bytes, &mut cursor, "body")?;
        let body = take(bytes, &mut cursor, body_len, "body")?.to_vec();
        if files.insert(name.clone(), body).is_some() {
            bail!("duplicate normalized package filename: {name}");
        }
        prior = Some(name);
    }
    if package_store::encode_normalized_package(&files) != bytes {
        bail!("normalized package payload is not canonical");
    }
    Ok(files)
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

fn write_staged_file(root: &Path, relative: &str, bytes: &[u8]) -> Result<()> {
    let relative = SourcePath::parse(relative.to_string())?;
    let path = root.join(relative.as_str());
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("staged file has no parent: {relative}"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create staged directory {}", parent.display()))?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("create staged file {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write staged file {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("sync staged file {}", path.display()))?;
    let mut permissions = file.metadata()?.permissions();
    permissions.set_readonly(true);
    file.set_permissions(permissions)
        .with_context(|| format!("make staged file read-only {}", path.display()))?;
    Ok(())
}

fn set_tree_read_only(root: &Path, read_only: bool) -> Result<()> {
    for entry in WalkDir::new(root).contents_first(read_only) {
        let entry = entry.with_context(|| format!("walk staged tree {}", root.display()))?;
        let metadata = entry
            .metadata()
            .with_context(|| format!("inspect staged path {}", entry.path().display()))?;
        let mut permissions = metadata.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = permissions.mode();
            permissions.set_mode(if read_only {
                mode & !0o222
            } else {
                mode | 0o200
            });
        }
        #[cfg(not(unix))]
        permissions.set_readonly(read_only);
        fs::set_permissions(entry.path(), permissions).with_context(|| {
            format!(
                "make staged path {}read-only {}",
                if read_only { "" } else { "not " },
                entry.path().display()
            )
        })?;
    }
    Ok(())
}

/// Reconstruct the exact captured source and package values as a private disk
/// view for native crates that still take filesystem paths. No live input path
/// is carried into the returned value.
fn stage_build_inputs(
    sources: &SourceSnapshot,
    packages: &PackageSnapshot,
) -> Result<StagedBuildInputs> {
    let root = tempfile::Builder::new()
        .prefix("fig-prepare-inputs-")
        .tempdir()
        .context("create private staged build inputs")?;
    let ig_dir = root.path().join("ig");
    let cache_dir = root.path().join("cache");
    fs::create_dir_all(&ig_dir)?;
    fs::create_dir_all(&cache_dir)?;

    for (path, entry) in sources.manifest.iter() {
        let bytes = addressed_bytes(&sources.objects, &entry.content, "source")?;
        write_staged_file(&ig_dir, path.as_str(), bytes)?;
    }

    for (coordinate, package) in packages.lock.iter() {
        validate_package_component(&coordinate.to_string(), "package coordinate")?;
        let normalized = addressed_bytes(&packages.objects, &package.content, "package")?;
        let files = decode_package_payload(normalized)
            .with_context(|| format!("decode captured package {coordinate}"))?;
        let package_dir = cache_dir.join(coordinate.to_string()).join("package");
        fs::create_dir_all(&package_dir)?;
        for (name, bytes) in files {
            // Browser bundle material is top-level by contract. Keeping the
            // staged representation equally narrow prevents a host-dependent
            // directory interpretation.
            validate_package_component(&name, "staged package filename")?;
            write_staged_file(&package_dir, &name, &bytes)?;
        }
    }

    // The pipeline receives a filesystem-shaped compatibility view, but it is
    // immutable in the OS as well as detached from live paths. Every derived
    // package sidecar is already present in the captured payload.
    set_tree_read_only(&ig_dir, true)?;
    set_tree_read_only(&cache_dir, true)?;

    Ok(StagedBuildInputs {
        _root: root,
        ig_dir,
        cache_dir,
    })
}

fn verify_staged_inputs(
    staged: &StagedBuildInputs,
    config_text: &str,
    expected_sources: &SourceSnapshot,
    expected_closure: &ResolvedClosure,
    expected_packages: &PackageSnapshot,
) -> Result<()> {
    let sources =
        collect_authored_sources(&staged.ig_dir).context("verify reconstructed staged project")?;
    if &sources != expected_sources {
        bail!("reconstructed staged project differs from captured source objects");
    }
    let closure = resolve_exact_closure(config_text, &staged.cache_dir)
        .context("verify reconstructed staged package closure")?;
    if &closure != expected_closure {
        bail!("reconstructed staged package closure differs from captured closure");
    }
    let packages = collect_package_snapshot(&staged.cache_dir, &closure)
        .context("verify reconstructed staged package payloads")?;
    if &packages != expected_packages {
        bail!("reconstructed staged package payloads differ from captured package objects");
    }
    Ok(())
}

fn produced_identity(db: &site_db::SiteDb) -> Result<(String, String)> {
    let primary = db
        .primary_implementation_guide
        .as_ref()
        .ok_or_else(|| anyhow!("site_db build did not identify its primary ImplementationGuide"))?;
    let ig_row = db
        .resources
        .iter()
        .find(|row| {
            if row.type_ != primary.resource_type {
                return false;
            }
            serde_json::from_str::<Value>(&row.json)
                .ok()
                .and_then(|resource| {
                    resource
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .as_deref()
                == Some(primary.id.as_str())
        })
        .ok_or_else(|| {
            anyhow!(
                "site_db primary ImplementationGuide {}/{} is absent from rows",
                primary.resource_type,
                primary.id
            )
        })?;
    let ig: Value =
        serde_json::from_str(&ig_row.json).context("parse produced ImplementationGuide row")?;
    let project_id = ig
        .get("packageId")
        .or_else(|| ig.get("id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("produced ImplementationGuide has no packageId/id"))?
        .to_string();
    let fhir_version = match ig.get("fhirVersion") {
        Some(Value::String(value)) => Some(value.as_str()),
        Some(Value::Array(values)) => values.first().and_then(Value::as_str),
        _ => None,
    }
    .filter(|value| !value.trim().is_empty())
    .ok_or_else(|| anyhow!("produced ImplementationGuide has no fhirVersion"))?
    .to_string();

    for (name, expected) in [
        ("packageId", project_id.as_str()),
        ("version", fhir_version.as_str()),
    ] {
        let row_value = db
            .metadata
            .iter()
            .find(|row| row.name == name)
            .map(|row| row.value.as_str())
            .ok_or_else(|| anyhow!("site_db build produced no {name} metadata row"))?;
        if row_value != expected {
            bail!(
                "produced ImplementationGuide {name} {expected:?} disagrees with site_db metadata {row_value:?}"
            );
        }
    }
    Ok((project_id, fhir_version))
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

fn merge_objects(
    target: &mut BTreeMap<Sha256Digest, Vec<u8>>,
    additions: BTreeMap<Sha256Digest, Vec<u8>>,
) -> Result<()> {
    for (digest, bytes) in additions {
        insert_object(
            target,
            ContentRef {
                sha256: digest,
                byte_length: bytes.len() as u64,
                media_type: None,
            },
            bytes,
        )?;
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

fn emit_bundle(
    output: &Path,
    site_build_bytes: &[u8],
    objects: &BTreeMap<Sha256Digest, Vec<u8>>,
) -> Result<()> {
    if fs::symlink_metadata(output).is_ok() {
        bail!("output already exists: {}", output.display());
    }
    let parent = output.parent().expect("output_destination supplies parent");
    let temp = tempfile::Builder::new()
        .prefix(".fig-prepare-")
        .tempdir_in(parent)
        .with_context(|| format!("create temporary bundle beside {}", output.display()))?;
    let object_dir = temp.path().join("objects/sha256");
    fs::create_dir_all(&object_dir)?;
    fs::write(temp.path().join("site-build.json"), site_build_bytes)?;
    for (digest, bytes) in objects {
        fs::write(object_dir.join(digest.as_str()), bytes)
            .with_context(|| format!("write bundle object {digest}"))?;
    }
    if fs::symlink_metadata(output).is_ok() {
        bail!(
            "output appeared while bundle was being built: {}",
            output.display()
        );
    }
    rename_no_replace(temp.path(), output)
        .with_context(|| format!("publish bundle atomically at {}", output.display()))?;
    Ok(())
}

/// Atomically publish without replacing a destination that appeared after the
/// last userspace check. Linux and macOS use their exclusive rename primitives;
/// Windows rename already refuses an existing destination. Unknown platforms
/// fail closed instead of weakening the no-clobber guarantee.
#[cfg(target_os = "linux")]
fn rename_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let destination = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    // SAFETY: both pointers are live NUL-terminated path strings for the call;
    // AT_FDCWD makes them process-relative exactly like std::fs::rename.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn rename_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let destination = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    // SAFETY: both pointers remain live NUL-terminated path strings throughout
    // the call; RENAME_EXCL makes destination creation atomic and no-clobber.
    let result =
        unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "windows")]
fn rename_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    if fs::symlink_metadata(destination).is_ok() {
        return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
    }
    fs::rename(source, destination)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn rename_no_replace(_source: &Path, _destination: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-replace directory publication is unsupported on this platform",
    ))
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
    fn package_payload_hash_matches_the_wasm_normalization() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().canonicalize().unwrap();
        package(&cache, "example", "1.0.0", serde_json::json!({}));
        write(&cache.join("example#1.0.0/package/nested/a.txt"), b"alpha");
        let coordinate = PackageCoordinate::new("example", "1.0.0").unwrap();
        let closure = ResolvedClosure {
            by_id: BTreeMap::from([("example".into(), coordinate.clone())]),
            core: coordinate,
        };
        let snapshot = collect_package_snapshot(&cache, &closure).unwrap();
        let package_root = cache.join("example#1.0.0/package");
        let material = read_normalized_package_material(&package_root, "example#1.0.0").unwrap();
        let files = material.files;
        let shared_bundle = package_acquisition::build_bundle(&package_root).unwrap();
        let shared_files: BTreeMap<String, Vec<u8>> =
            package_acquisition::read_bundle(&shared_bundle)
                .unwrap()
                .into_iter()
                .collect();
        assert_eq!(files, shared_files);
        assert!(files.contains_key(package_store::derived_index::SIDECAR_NAME));
        assert!(!files.contains_key("nested/a.txt"));
        let expected = package_store::encode_normalized_package(&files);
        let locked = snapshot.lock.iter().next().unwrap().1;
        assert_eq!(locked.content.sha256, Sha256Digest::of_bytes(&expected));
        assert_eq!(
            snapshot.objects.get(&locked.content.sha256),
            Some(&expected)
        );
    }

    #[test]
    fn normalized_package_payload_round_trips_to_the_exact_file_map() {
        let files = BTreeMap::from([
            (
                ".derived-index.json".to_string(),
                b"{\"files\":[]}".to_vec(),
            ),
            ("package.json".to_string(), b"{\"name\":\"demo\"}".to_vec()),
            (
                "StructureDefinition-Demo.json".to_string(),
                vec![0, 1, 2, 255],
            ),
        ]);
        let encoded = package_store::encode_normalized_package(&files);
        assert_eq!(decode_package_payload(&encoded).unwrap(), files);

        let mut truncated = encoded;
        truncated.pop();
        assert!(decode_package_payload(&truncated)
            .unwrap_err()
            .to_string()
            .contains("truncated"));
    }

    #[test]
    fn staged_inputs_are_detached_from_post_capture_live_mutations() {
        let live = tempfile::tempdir().unwrap();
        let ig = live.path().join("ig");
        let cache = live.path().join("cache");
        write(
            &ig.join("sushi-config.yaml"),
            b"id: staged.demo\nfhirVersion: 4.0.1\n",
        );
        write(&ig.join("input/fsh/demo.fsh"), b"Profile: Captured\n");
        package(&cache, "example", "1.0.0", serde_json::json!({}));

        let ig = ig.canonicalize().unwrap();
        let cache = cache.canonicalize().unwrap();
        let sources = collect_authored_sources(&ig).unwrap();
        let coordinate = PackageCoordinate::new("example", "1.0.0").unwrap();
        let closure = ResolvedClosure {
            by_id: BTreeMap::from([("example".into(), coordinate.clone())]),
            core: coordinate,
        };
        let packages = collect_package_snapshot(&cache, &closure).unwrap();
        let staged = stage_build_inputs(&sources, &packages).unwrap();

        // Mutate both live inputs after capture. A native build is handed only
        // staged.ig_dir/staged.cache_dir, so these B values cannot affect it.
        fs::write(ig.join("input/fsh/demo.fsh"), b"Profile: LiveMutation\n").unwrap();
        write(
            &cache.join("example#1.0.0/package/live-only.txt"),
            b"live mutation",
        );

        let staged_sources = collect_authored_sources(&staged.ig_dir).unwrap();
        let staged_packages = collect_package_snapshot(&staged.cache_dir, &closure).unwrap();
        assert_eq!(staged_sources, sources);
        assert_eq!(staged_packages, packages);
        assert_ne!(collect_authored_sources(&ig).unwrap(), sources);
        assert_ne!(
            collect_package_snapshot(&cache, &closure).unwrap(),
            packages
        );
        assert_eq!(
            fs::read_to_string(staged.ig_dir.join("input/fsh/demo.fsh")).unwrap(),
            "Profile: Captured\n"
        );
        assert!(fs::metadata(staged.ig_dir.join("input/fsh/demo.fsh"))
            .unwrap()
            .permissions()
            .readonly());
    }

    #[test]
    fn exact_closure_is_satisfied_and_conflicting_versions_are_rejected() {
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
        assert_eq!(closure.by_id.len(), 4);
        assert_eq!(closure.core.to_string(), "hl7.fhir.r4.core#4.0.1");

        let conflict = ResolutionStep {
            compile_set: vec![PackageRequest {
                package_id: "same".into(),
                version: "1.0.0".into(),
            }],
            context_closure: vec![PackageRequest {
                package_id: "same".into(),
                version: "2.0.0".into(),
            }],
            missing: Vec::new(),
            satisfied: true,
        };
        let error = closure_from_step("fhirVersion: 4.0.1\n", &conflict).unwrap_err();
        assert!(error.to_string().contains("conflicting versions"));
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
        let closure = ResolvedClosure {
            by_id: BTreeMap::from([("example".into(), coordinate.clone())]),
            core: coordinate,
        };
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
        emit_bundle(&output, b"{\"schemaVersion\":\"site-build/v1\"}", &objects).unwrap();
        assert_eq!(
            fs::read(output.join("objects/sha256").join(digest.as_str())).unwrap(),
            body
        );
        assert!(output.join("site-build.json").is_file());
        let error = emit_bundle(&output, b"different", &objects).unwrap_err();
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

        let error = rename_no_replace(&staging, &destination).unwrap_err();
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
    fn sushi_output_cannot_reach_authored_input_through_a_symlink_parent() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let ig = temp.path().join("ig");
        fs::create_dir_all(ig.join("input")).unwrap();
        symlink(ig.join("input"), temp.path().join("redirect")).unwrap();
        let ig = ig.canonicalize().unwrap();
        let error = ensure_no_source_output_overlap(
            &ig,
            &temp.path().join("redirect/generated"),
            &temp.path().join("bundle"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("resolves inside authored input"));
    }

    #[test]
    fn sushi_output_must_be_new_and_disjoint_from_the_bundle() {
        let temp = tempfile::tempdir().unwrap();
        let existing = temp.path().join("old-sushi-output");
        fs::create_dir(&existing).unwrap();
        let error = require_new_sushi_output(&existing).unwrap_err();
        assert!(error.to_string().contains("new, nonexistent directory"));

        let new_sushi = temp.path().join("new-sushi-output");
        require_new_sushi_output(&new_sushi).unwrap();
        let nested_bundle = new_sushi.join("closed-build");
        let error = ensure_output_trees_are_disjoint(&new_sushi, &nested_bundle).unwrap_err();
        assert!(error.to_string().contains("must be disjoint"));
    }
}

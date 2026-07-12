use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime::{
    ObjectMap, OutputDescriptor, OutputResourceSubject, OutputSubjectPage, PreparedOutput,
    PublisherRuntime,
};
use crate::{
    build_render_semantics, build_render_state_from_semantics, PackageView, SiteEngine, SiteOptions,
};

const ENGINE_API: u32 = 1;
const PREPARED_GUIDE_CACHE_SCHEMA: &str = "prepared-guide-cache-key/v1";
const PREPARED_GUIDE_RECIPE: &str = "sushi.snapshot+site-semantics/v1";
const SNAPSHOT_COMPLETED_LOCAL_CACHE_SCHEMA: &str = "snapshot-completed-local-cache-key/v1";
const SNAPSHOT_COMPLETED_LOCAL_RECIPE: &str = "snapshot-gen.walk+local-precedence/v1";
const CLOSED_SITE_BUILD_CACHE_SCHEMA: &str = "closed-site-build-cache-key/v1";
const CYCLE_PROJECTION_RECIPE: &str = "site-build.cycle-projection/v2";
const PUBLISHER_RUNTIME_PREPARATION_SCHEMA: &str = "publisher-runtime-preparation-key/v1";
const PUBLISHER_RUNTIME_PREPARATION_RECIPE: &str =
    "publisher-template-rust.runtime+model+render+catalog/v1";

const PUBLISHER_AUTHORED_NAMESPACE_PREFIX: &str = "publisher.authored";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "generator", rename_all = "camelCase", deny_unknown_fields)]
pub enum GeneratorSpec {
    Cycle {
        #[serde(rename = "buildEpochSecs")]
        build_epoch_secs: i64,
        #[serde(default, rename = "liquidAssetDirs")]
        liquid_asset_dirs: Vec<String>,
        #[serde(default)]
        branch: Option<String>,
        #[serde(default)]
        revision: Option<String>,
    },
    Publisher {
        #[serde(rename = "templateCoordinate")]
        template_coordinate: String,
        #[serde(rename = "buildEpochSecs")]
        build_epoch_secs: i64,
        #[serde(default, rename = "activeTables")]
        active_tables: bool,
        #[serde(default, rename = "runUuid")]
        run_uuid: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct PackageMaterial {
    content: site_build::ContentRef,
    declared_dependencies: BTreeMap<String, String>,
    content_bytes: Rc<Vec<u8>>,
}

impl PackageMaterial {
    pub fn new(
        content: site_build::ContentRef,
        declared_dependencies: BTreeMap<String, String>,
        content_bytes: Rc<Vec<u8>>,
    ) -> Result<Self, String> {
        content
            .verify(content_bytes.as_ref())
            .map_err(|error| format!("package material is not authenticated: {error}"))?;
        Ok(Self {
            content,
            declared_dependencies,
            content_bytes,
        })
    }

    pub fn content(&self) -> &site_build::ContentRef {
        &self.content
    }

    pub fn declared_dependencies(&self) -> &BTreeMap<String, String> {
        &self.declared_dependencies
    }
}

#[derive(Clone)]
pub struct PackageEnvironment {
    packages: PackageView,
    mounted_labels: Vec<String>,
    materials: BTreeMap<String, PackageMaterial>,
}

impl PackageEnvironment {
    pub fn new(
        packages: PackageView,
        mounted_labels: Vec<String>,
        materials: BTreeMap<String, PackageMaterial>,
    ) -> Result<Self, String> {
        let distinct = mounted_labels.iter().collect::<BTreeSet<_>>();
        if distinct.len() != mounted_labels.len() {
            return Err("package environment repeats a mounted label".into());
        }
        for label in &mounted_labels {
            if !materials.contains_key(label) {
                return Err(format!(
                    "mounted package {label} has no authenticated material"
                ));
            }
        }
        Ok(Self {
            packages,
            mounted_labels,
            materials,
        })
    }

    pub fn resolve_template(&self, coordinate: &str) -> Result<TemplateResolution, String> {
        let resolution = package_store::resolve_template_base_chain(
            &self.packages,
            &package_store::TemplatePaths::new(self.packages.root()),
            coordinate,
        )
        .map_err(|error| format!("resolveTemplate: {error}"))?;
        Ok(TemplateResolution {
            satisfied: resolution.satisfied(),
            chain: resolution.chain,
            missing: resolution.missing,
        })
    }

    fn scoped(&self, labels: &[String], operation: &str) -> Result<PackageView, String> {
        let mut allowed = BTreeSet::new();
        for label in labels {
            if !self.mounted_labels.contains(label) || !self.materials.contains_key(label) {
                return Err(format!(
                    "{operation}: resolved package {label} is not present in the mounted package store"
                ));
            }
            if !allowed.insert(label.clone()) {
                return Err(format!(
                    "{operation}: resolved package closure repeats {label}"
                ));
            }
        }
        Ok(self.packages.scoped(allowed))
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateResolution {
    pub satisfied: bool,
    pub chain: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareResult {
    pub handle: String,
    pub build_id: String,
    pub generator: String,
    pub site_build: site_build::ClosedSiteBuild,
    pub metrics: PrepareMetrics,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareMetrics {
    pub total_ms: f64,
    pub project_revision_ms: f64,
    pub package_lock_ms: f64,
    pub prepared_guide_key_ms: f64,
    pub prepared_guide_ms: f64,
    pub snapshot_completed_local_cache_hit: bool,
    pub prepared_guide_cache_hit: bool,
    pub site_build_cache_hit: bool,
    pub template_materialize_ms: f64,
    pub publisher_runtime_ms: f64,
    pub publisher_model_ms: f64,
    pub render_model_ms: f64,
    pub catalog_ms: f64,
}

#[derive(Default)]
pub(crate) struct PreparationState {
    prepared_guide: Option<PreparedGuideCacheEntry>,
    snapshot_completed_local: Option<SnapshotCompletedLocalCacheEntry>,
    closed_cycle: Option<ClosedSiteBuildCacheEntry>,
    #[cfg(test)]
    cache_hits: DerivedCacheHits,
}

#[derive(Clone)]
struct PreparedGuideCacheEntry {
    key: site_build::Sha256Digest,
    guide: site_build::PreparedGuide,
}

struct SnapshotCompletedLocalCacheEntry {
    key: site_build::Sha256Digest,
    generated: Vec<Value>,
    primary_implementation_guide: Value,
}

struct PreparationCacheKeys {
    prepared_guide: site_build::Sha256Digest,
    snapshot_completed_local: site_build::Sha256Digest,
}

#[derive(Clone)]
struct ClosedSiteBuildCacheEntry {
    key: site_build::Sha256Digest,
    projection: site_build::cycle_semantic::ClosedCycleProjection,
}

#[cfg(test)]
#[derive(Default)]
struct DerivedCacheHits {
    snapshot_completed_local: u64,
    prepared_guide: u64,
    closed_site_build: u64,
    retained_publisher_runtime: u64,
}

#[derive(Clone)]
struct PrepareGuideOptions {
    build_epoch_secs: i64,
    liquid_asset_dirs: Vec<String>,
    branch: Option<String>,
    revision: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreparedGuideCachePayload<'a> {
    schema: &'static str,
    recipe: &'static str,
    engine_api: u32,
    project: &'a site_build::ProjectRevision,
    package_lock: &'a site_build::PackageLock,
    compiled_sha256: &'a site_build::Sha256Digest,
    build_epoch_secs: i64,
    liquid_asset_dirs: &'a [String],
    branch: Option<&'a str>,
    revision: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotCompletedLocalCachePayload<'a> {
    schema: &'static str,
    recipe: &'static str,
    engine_api: u32,
    compiled_sha256: &'a site_build::Sha256Digest,
    package_lock: &'a site_build::PackageLock,
    resolved_config_sha256: &'a site_build::Sha256Digest,
    resolution_support: &'a BTreeSet<String>,
    resolved_labels: &'a [String],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClosedSiteBuildCachePayload<'a> {
    schema: &'static str,
    recipe: &'static str,
    prepared_guide_key: &'a site_build::Sha256Digest,
    render_target: &'a site_build::RenderTarget,
    diagnostics: &'a BTreeSet<site_build::BuildDiagnostic>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublisherRuntimePreparationKeyPayload<'a> {
    schema: &'static str,
    recipe: &'a str,
    engine_api: u32,
    project: &'a site_build::ProjectRevision,
    package_lock: &'a site_build::PackageLock,
    prepared_guide_key: &'a site_build::Sha256Digest,
    diagnostics: &'a BTreeSet<site_build::BuildDiagnostic>,
    template: &'a site_build::PackageCoordinate,
    template_chain: &'a [String],
    renderer: &'a site_build::ProducerRef,
    active_tables: bool,
    run_uuid: Option<&'a str>,
}

impl SiteEngine {
    pub fn clear_preparation(&mut self) {
        self.preparation = PreparationState::default();
    }

    pub fn resolve_template(
        &self,
        environment: &PackageEnvironment,
        coordinate: &str,
    ) -> Result<TemplateResolution, String> {
        environment.resolve_template(coordinate)
    }

    pub fn prepare(
        &mut self,
        spec: GeneratorSpec,
        environment: PackageEnvironment,
    ) -> Result<PrepareResult, String> {
        match spec {
            GeneratorSpec::Cycle {
                build_epoch_secs,
                liquid_asset_dirs,
                branch,
                revision,
            } => self.prepare_cycle(
                &PrepareGuideOptions {
                    build_epoch_secs,
                    liquid_asset_dirs,
                    branch,
                    revision,
                },
                &environment,
            ),
            GeneratorSpec::Publisher {
                template_coordinate,
                build_epoch_secs,
                active_tables,
                run_uuid,
            } => self.prepare_publisher(
                &template_coordinate,
                PrepareGuideOptions {
                    build_epoch_secs,
                    liquid_asset_dirs: vec!["input/includes".into()],
                    branch: None,
                    revision: None,
                },
                active_tables,
                run_uuid,
                &environment,
            ),
        }
    }

    fn prepare_cycle(
        &mut self,
        options: &PrepareGuideOptions,
        environment: &PackageEnvironment,
    ) -> Result<PrepareResult, String> {
        let total_started = self.timer();
        let operation = "prepare(cycle)";
        let mut metrics = PrepareMetrics::default();
        let project = self.project_revision().cloned().ok_or_else(|| {
            format!("{operation}: compileProject has not established a complete source revision")
        })?;
        let (project_id, fhir_version) = compiled_ig_identity(self.compiled_resources())?;
        let started = self.timer();
        let project_revision = site_build_project_revision(&project, &project_id)?;
        metrics.project_revision_ms = elapsed_ms(started);
        let started = self.timer();
        let package_lock = site_build_package_lock(&project, environment)?;
        metrics.package_lock_ms = elapsed_ms(started);
        let started = self.timer();
        let keys = self.preparation_cache_keys(
            options,
            &project_revision,
            &package_lock,
            project.resolved_packages(),
            operation,
        )?;
        metrics.snapshot_completed_local_cache_hit = self
            .preparation
            .snapshot_completed_local
            .as_ref()
            .is_some_and(|entry| entry.key == keys.snapshot_completed_local);
        metrics.prepared_guide_key_ms = elapsed_ms(started);
        let diagnostics = site_build_diagnostics(self.compile_diagnostics());
        let mut parameters = BTreeMap::from([
            ("contract".into(), site_build::cycle_semantic::TARGET.into()),
            (
                "buildEpochSecs".into(),
                options.build_epoch_secs.to_string(),
            ),
            (
                "liquidAssetDirs".into(),
                if options.liquid_asset_dirs.is_empty() {
                    "input/includes".into()
                } else {
                    options.liquid_asset_dirs.join(",")
                },
            ),
        ]);
        if let Some(branch) = &options.branch {
            parameters.insert("branch".into(), branch.clone());
        }
        if let Some(revision) = &options.revision {
            parameters.insert("revision".into(), revision.clone());
        }
        let render_target = site_build::RenderTarget {
            renderer: site_build::ProducerRef::new("cycle-site", "2"),
            mode: site_build::RenderMode::ExternalBuilder,
            fhir_version,
            template: None,
            parameters,
        };
        let closed_key =
            closed_site_build_cache_key(&keys.prepared_guide, &render_target, &diagnostics)
                .map_err(|error| format!("{operation}: cache key: {error}"))?;
        if let Some(entry) = &self.preparation.closed_cycle {
            if entry.key == closed_key {
                metrics.site_build_cache_hit = true;
                metrics.prepared_guide_cache_hit = self
                    .preparation
                    .prepared_guide
                    .as_ref()
                    .is_some_and(|prepared| prepared.key == keys.prepared_guide);
                metrics.total_ms = elapsed_ms(total_started);
                #[cfg(test)]
                {
                    self.preparation.cache_hits.closed_site_build += 1;
                }
                let projection = entry.projection.clone();
                let handle = projection.site_build.site_build().build_id().to_string();
                let mut objects = environment_objects(
                    &project,
                    projection.site_build.site_build().package_lock(),
                    environment,
                    operation,
                )?;
                merge_objects(&mut objects, projection.objects.clone())?;
                self.install_cycle(projection.site_build.clone(), objects);
                return Ok(PrepareResult {
                    handle: handle.clone(),
                    build_id: handle,
                    generator: "cycle".into(),
                    site_build: projection.site_build,
                    metrics,
                });
            }
        }
        metrics.prepared_guide_cache_hit = self
            .preparation
            .prepared_guide
            .as_ref()
            .is_some_and(|prepared| prepared.key == keys.prepared_guide);
        let started = self.timer();
        let prepared = self.site_model_from_compile(
            options,
            environment,
            operation,
            keys.prepared_guide.clone(),
            keys.snapshot_completed_local,
        )?;
        metrics.prepared_guide_ms = elapsed_ms(started);
        let started = self.timer();
        let projection = site_build::cycle_semantic::close_prepared(
            &prepared,
            site_build::cycle_semantic::CycleProjectionInput {
                project: project_revision,
                package_lock,
                render_target,
                diagnostics,
            },
        )
        .map_err(|error| format!("{operation}: {error}"))?;
        metrics.catalog_ms = elapsed_ms(started);
        self.preparation.closed_cycle = Some(ClosedSiteBuildCacheEntry {
            key: closed_key,
            projection: projection.clone(),
        });
        let handle = projection.site_build.site_build().build_id().to_string();
        let mut objects = environment_objects(
            &project,
            projection.site_build.site_build().package_lock(),
            environment,
            operation,
        )?;
        merge_objects(&mut objects, projection.objects.clone())?;
        self.install_cycle(projection.site_build.clone(), objects);
        metrics.total_ms = elapsed_ms(total_started);
        Ok(PrepareResult {
            handle: handle.clone(),
            build_id: handle,
            generator: "cycle".into(),
            site_build: projection.site_build,
            metrics,
        })
    }
}

const PAGE_MD_SHIM: &str = "---\r\n---\r\n{% include template-page-md.html %}";
const FRAGMENT_SUFFIXES: &[&str] = &[
    "intro",
    "introduction",
    "notes",
    "search",
    "summary",
    "examples",
];

fn is_fragment_include(name: &str) -> bool {
    FRAGMENT_SUFFIXES
        .iter()
        .any(|suffix| name.ends_with(&format!("-{suffix}")))
}

fn is_static_asset(path: &str) -> bool {
    matches!(
        path.rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .as_deref(),
        Some(
            "css"
                | "js"
                | "png"
                | "svg"
                | "jpg"
                | "jpeg"
                | "gif"
                | "webp"
                | "ico"
                | "woff"
                | "woff2"
                | "ttf"
                | "otf"
                | "eot"
        )
    )
}

#[derive(Clone, Copy)]
enum PreparedOutputCollision {
    Reject,
    AuthoredOverridesRenderer,
}

#[allow(clippy::too_many_arguments)]
fn insert_prepared_output(
    outputs: &mut BTreeMap<site_build::OutputPath, PreparedOutput>,
    objects: &mut ObjectMap,
    path: &str,
    bytes: Vec<u8>,
    media_type: &str,
    producer: site_build::OutputProducer,
    source: Option<String>,
    collision: PreparedOutputCollision,
) -> Result<(), String> {
    let path = site_build::OutputPath::parse(path.to_string())
        .map_err(|error| format!("invalid output path {path}: {error}"))?;
    if let Some(existing) = outputs.get(&path) {
        if matches!(collision, PreparedOutputCollision::Reject) {
            return Err(format!(
                "prepared output collision at {path}: producer {} and {}",
                existing.producer.id, producer.id
            ));
        }
    }
    let content = site_build::ContentRef::of_bytes(&bytes, Some(media_type));
    insert_object(objects, &content, bytes)?;
    outputs.insert(
        path,
        PreparedOutput {
            content,
            producer,
            source,
            owner: None,
        },
    );
    Ok(())
}

fn insert_object(
    objects: &mut ObjectMap,
    content: &site_build::ContentRef,
    bytes: impl Into<Rc<Vec<u8>>>,
) -> Result<(), String> {
    let bytes = bytes.into();
    content
        .verify(bytes.as_ref())
        .map_err(|error| format!("content {} failed verification: {error}", content.sha256))?;
    insert_authenticated_object(objects, content, bytes)
}

fn insert_authenticated_object(
    objects: &mut ObjectMap,
    content: &site_build::ContentRef,
    bytes: Rc<Vec<u8>>,
) -> Result<(), String> {
    if content.byte_length != bytes.len() as u64 {
        return Err(format!(
            "content {} length {} differs from authenticated bytes {}",
            content.sha256,
            content.byte_length,
            bytes.len()
        ));
    }
    if let Some(existing) = objects.get(&content.sha256) {
        if existing.as_ref() != bytes.as_ref() {
            return Err(format!("conflicting bytes for digest {}", content.sha256));
        }
    } else {
        objects.insert(content.sha256.clone(), bytes);
    }
    Ok(())
}

fn merge_objects<T: Into<Rc<Vec<u8>>>>(
    target: &mut ObjectMap,
    source: BTreeMap<site_build::Sha256Digest, T>,
) -> Result<(), String> {
    for (digest, bytes) in source {
        let bytes = bytes.into();
        let content = site_build::ContentRef {
            sha256: digest,
            byte_length: bytes.len() as u64,
            media_type: None,
        };
        insert_object(target, &content, bytes)?;
    }
    Ok(())
}

fn add_page_relative_output_aliases(
    outputs: &mut BTreeMap<site_build::OutputPath, PreparedOutput>,
    pages: &[String],
) -> Result<(), String> {
    let directories = pages
        .iter()
        .filter_map(|page| page.rsplit_once('/').map(|(directory, _)| directory))
        .filter(|directory| !directory.is_empty())
        .collect::<BTreeSet<_>>();
    let originals = outputs
        .iter()
        .filter(|(path, _)| is_static_asset(path.as_str()))
        .map(|(path, output)| (path.clone(), output.clone()))
        .collect::<Vec<_>>();
    for directory in directories {
        for (path, output) in &originals {
            if path.as_str().starts_with(&format!("{directory}/")) {
                continue;
            }
            let alias = site_build::OutputPath::parse(format!("{directory}/{path}"))
                .map_err(|error| format!("prepare(publisher): invalid asset alias: {error}"))?;
            let mut alias_output = output.clone();
            alias_output.source = alias_output
                .source
                .as_ref()
                .map(|source| format!("{source}; alias={alias}"));
            outputs.entry(alias).or_insert(alias_output);
        }
    }
    Ok(())
}

fn authored_projection_targets(file: &site_build::AuthoredFile) -> Result<Vec<String>, String> {
    let path = file.path.as_str();
    let mut targets = Vec::new();
    let tree = |path: String| format!("tree:/site/{path}");
    let output = |path: &str| -> Result<String, String> {
        site_build::OutputPath::parse(path.to_string())
            .map(|path| format!("output:{path}"))
            .map_err(|error| {
                format!(
                    "prepare(publisher): authored {:?} path {path} is not safe: {error}",
                    file.role
                )
            })
    };
    match file.role {
        site_build::AuthoredFileRole::Image => targets.push(output(path)?),
        site_build::AuthoredFileRole::Data => targets.push(tree(format!("_data/{path}"))),
        site_build::AuthoredFileRole::Include | site_build::AuthoredFileRole::ResourceContent => {
            targets.push(tree(format!("_includes/{path}")));
        }
        site_build::AuthoredFileRole::PageContent => {
            if let Some(name) = path.strip_suffix(".md") {
                targets.push(tree(format!("_includes/en/{path}")));
                targets.push(tree(format!("_includes/{path}")));
                if !is_fragment_include(name) {
                    let page = format!("en/{name}.html");
                    targets.push(tree(page.clone()));
                    targets.push(output(&page)?);
                }
            } else if path.ends_with(".html") {
                let page = format!("en/{path}");
                targets.push(tree(page.clone()));
                targets.push(output(&page)?);
            }
        }
        site_build::AuthoredFileRole::ImageSource => {}
    }
    Ok(targets)
}

fn validate_authored_projections(files: &[site_build::AuthoredFile]) -> Result<(), String> {
    let mut claims = BTreeMap::new();
    for file in files {
        let owner = format!("{:?} {}", file.role, file.path);
        for target in authored_projection_targets(file)? {
            if let Some(existing) = claims.insert(target.clone(), owner.clone()) {
                return Err(format!(
                    "prepare(publisher): authored projection collision at {target}: {existing} and {owner}"
                ));
            }
        }
    }
    Ok(())
}

fn stage_prepared_authored_files(
    prepared: &site_build::PreparedGuide,
    site_files: &mut HashMap<PathBuf, Vec<u8>>,
    outputs: &mut BTreeMap<site_build::OutputPath, PreparedOutput>,
    objects: &mut ObjectMap,
    project_id: &str,
) -> Result<(), String> {
    validate_authored_projections(&prepared.authored_files)?;
    for file in &prepared.authored_files {
        let path = file.path.as_str();
        let source = file
            .source_reads
            .iter()
            .map(|source| source.as_str())
            .collect::<Vec<_>>()
            .join(",");
        match file.role {
            site_build::AuthoredFileRole::Image => insert_prepared_output(
                outputs,
                objects,
                path,
                file.content.clone(),
                &file.mime,
                site_build::OutputProducer {
                    id: "publisher-authored-asset".into(),
                    version: project_id.into(),
                },
                Some(source),
                PreparedOutputCollision::AuthoredOverridesRenderer,
            )?,
            site_build::AuthoredFileRole::Data => {
                site_files.insert(
                    PathBuf::from(format!("/site/_data/{path}")),
                    file.content.clone(),
                );
            }
            site_build::AuthoredFileRole::Include
            | site_build::AuthoredFileRole::ResourceContent => {
                site_files.insert(
                    PathBuf::from(format!("/site/_includes/{path}")),
                    file.content.clone(),
                );
            }
            site_build::AuthoredFileRole::PageContent => {
                let Some(name) = path.strip_suffix(".md") else {
                    if path.ends_with(".html") {
                        site_files.insert(
                            PathBuf::from(format!("/site/en/{path}")),
                            file.content.clone(),
                        );
                    }
                    continue;
                };
                site_files.insert(
                    PathBuf::from(format!("/site/_includes/en/{path}")),
                    file.content.clone(),
                );
                site_files.insert(
                    PathBuf::from(format!("/site/_includes/{path}")),
                    file.content.clone(),
                );
                if !is_fragment_include(name) {
                    site_files.insert(
                        PathBuf::from(format!("/site/en/{name}.html")),
                        PAGE_MD_SHIM.as_bytes().to_vec(),
                    );
                }
            }
            site_build::AuthoredFileRole::ImageSource => {}
        }
    }
    Ok(())
}

fn prepared_render_set(
    prepared: &site_build::PreparedGuide,
) -> Result<Vec<(PathBuf, Value)>, String> {
    let mut seen = BTreeSet::new();
    prepared
        .resources
        .iter()
        .map(|resource| {
            if !seen.insert(resource.key.clone()) {
                return Err(format!(
                    "prepare(publisher): PreparedGuide repeats resource {}/{}",
                    resource.key.resource_type, resource.key.id
                ));
            }
            if resource
                .resource
                .get("resourceType")
                .and_then(Value::as_str)
                != Some(resource.key.resource_type.as_str())
                || resource.resource.get("id").and_then(Value::as_str)
                    != Some(resource.key.id.as_str())
            {
                return Err(format!(
                    "prepare(publisher): PreparedGuide resource {}/{} disagrees with JSON identity",
                    resource.key.resource_type, resource.key.id
                ));
            }
            let channel = if resource.key == prepared.guide.implementation_guide {
                "__ig__"
            } else {
                "__prepared__"
            };
            Ok((
                PathBuf::from(format!(
                    "/{channel}/{}-{}.json",
                    resource.key.resource_type, resource.key.id
                )),
                resource.resource.clone(),
            ))
        })
        .collect()
}

fn output_catalog(
    ready: &BTreeMap<site_build::OutputPath, PreparedOutput>,
    pages: Vec<String>,
    resource_pages: &BTreeMap<String, site_producer::ResourcePageMetadata>,
    operation: &str,
) -> Result<Vec<OutputDescriptor>, String> {
    let mut catalog = ready
        .iter()
        .map(|(path, output)| OutputDescriptor {
            path: path.clone(),
            kind: if is_static_asset(path.as_str()) {
                "asset"
            } else {
                "auxiliary"
            },
            media_type: output
                .content
                .media_type
                .clone()
                .expect("prepared output media type"),
            content: Some(output.content.clone()),
            title: None,
            subject: None,
            subject_page: None,
        })
        .collect::<Vec<_>>();
    for page in pages {
        let metadata = resource_pages.get(&page);
        let path = site_build::OutputPath::parse(page.clone())
            .map_err(|error| format!("{operation}: invalid page {page}: {error}"))?;
        catalog.push(OutputDescriptor {
            path,
            kind: "page",
            media_type: "text/html".into(),
            content: None,
            title: metadata.map(|metadata| metadata.title.clone()),
            subject: metadata.map(|metadata| OutputResourceSubject {
                resource_type: metadata.resource_type.clone(),
                id: metadata.id.clone(),
            }),
            subject_page: metadata.map(|metadata| match metadata.role {
                site_producer::ResourcePageRole::Primary => OutputSubjectPage::Primary,
                site_producer::ResourcePageRole::Companion => OutputSubjectPage::Companion,
            }),
        });
    }
    catalog.sort_by(|left, right| left.path.cmp(&right.path));
    for pair in catalog.windows(2) {
        if pair[0].path == pair[1].path {
            return Err(format!(
                "{operation}: duplicate output path {}",
                pair[0].path
            ));
        }
    }
    Ok(catalog)
}

fn publisher_artifacts(
    prepared: &site_build::PreparedGuide,
    project: &site_build::ProjectRevision,
    package_lock: &site_build::PackageLock,
    template_chain: &[String],
    core: &site_build::PackageCoordinate,
    template_files: &BTreeMap<String, Vec<u8>>,
    runtime: &site_producer::publisher_runtime::PublisherRuntime,
) -> Result<
    (
        site_build::ArtifactCatalog,
        site_build::RenderPlan,
        ObjectMap,
    ),
    String,
> {
    let resources = site_build::cycle_semantic::ResourcesDocument {
        schema: site_build::cycle_semantic::RESOURCES_SCHEMA.into(),
        guide: prepared.guide.clone(),
        resources: prepared.resources.clone(),
        publisher_compatibility: prepared.publisher_compatibility.clone(),
    };
    let terminology = site_build::cycle_semantic::TerminologyDocument {
        schema: site_build::cycle_semantic::TERMINOLOGY_SCHEMA.into(),
        expansions: prepared.expansions.clone(),
    };
    let navigation = site_build::cycle_semantic::NavigationDocument {
        schema: site_build::cycle_semantic::NAVIGATION_SCHEMA.into(),
        pages: prepared.pages.clone(),
        menu: prepared.menu.clone(),
    };
    let config = site_build::cycle_semantic::ConfigDocument {
        schema: site_build::cycle_semantic::CONFIG_SCHEMA.into(),
        sushi_config: prepared.sushi_config.clone(),
    };
    let all_reads = project
        .sources
        .iter()
        .map(|(path, _)| site_build::ReadDependency::Source { path: path.clone() })
        .chain(
            package_lock
                .iter()
                .map(|(coordinate, _)| site_build::ReadDependency::Package {
                    coordinate: coordinate.clone(),
                }),
        )
        .collect::<BTreeSet<_>>();
    let semantic = [
        (
            site_build::cycle_semantic::resources_key(),
            serde_json::to_vec(&resources).map_err(|error| error.to_string())?,
            "site_semantics.resources",
            site_build::cycle_semantic::RESOURCES_SCHEMA,
        ),
        (
            site_build::cycle_semantic::terminology_key(),
            serde_json::to_vec(&terminology).map_err(|error| error.to_string())?,
            "site_semantics.terminology",
            site_build::cycle_semantic::TERMINOLOGY_SCHEMA,
        ),
        (
            site_build::cycle_semantic::navigation_key(),
            serde_json::to_vec(&navigation).map_err(|error| error.to_string())?,
            "site_semantics.navigation",
            site_build::cycle_semantic::NAVIGATION_SCHEMA,
        ),
        (
            site_build::cycle_semantic::config_key(),
            serde_json::to_vec(&config).map_err(|error| error.to_string())?,
            "site_semantics.config",
            site_build::cycle_semantic::CONFIG_SCHEMA,
        ),
    ];
    let mut records = Vec::new();
    let mut roots = BTreeSet::new();
    let mut objects = BTreeMap::new();
    for (key, bytes, recipe, schema) in semantic {
        push_artifact(
            &mut records,
            &mut roots,
            &mut objects,
            key,
            bytes,
            Some("application/json"),
            "site-semantics",
            recipe,
            BTreeMap::from([("schema".into(), schema.into())]),
            all_reads.clone(),
        )?;
    }
    for file in &prepared.authored_files {
        let path = site_build::SourcePath::parse(file.path.as_str().to_string())
            .map_err(|error| format!("prepare(publisher): authored artifact path: {error}"))?;
        let namespace = match file.role {
            site_build::AuthoredFileRole::Image => site_build::AssetNamespace::Authored,
            _ => site_build::AssetNamespace::Other {
                name: format!(
                    "{PUBLISHER_AUTHORED_NAMESPACE_PREFIX}.{}/v1",
                    authored_role_name(file.role.clone())
                ),
            },
        };
        let key = site_build::ArtifactKey::Asset { namespace, path };
        let reads = file
            .source_reads
            .iter()
            .map(|path| {
                site_build::SourcePath::parse(path.as_str().to_string())
                    .map(|path| site_build::ReadDependency::Source { path })
                    .map_err(|error| format!("prepare(publisher): authored source read: {error}"))
            })
            .collect::<Result<_, _>>()?;
        push_artifact(
            &mut records,
            &mut roots,
            &mut objects,
            key,
            file.content.clone(),
            Some(&file.mime),
            "prepared-guide",
            "publisher.authored-file/v1",
            BTreeMap::from([("role".into(), authored_role_name(file.role.clone()).into())]),
            reads,
        )?;
    }
    let template_reads = template_chain
        .iter()
        .map(|label| {
            site_build::PackageCoordinate::parse(label)
                .map(|coordinate| site_build::ReadDependency::Package { coordinate })
                .map_err(|error| format!("prepare(publisher): template chain {label}: {error}"))
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    for (path, bytes) in template_files {
        let key = site_build::ArtifactKey::Asset {
            namespace: site_build::AssetNamespace::Template,
            path: site_build::SourcePath::parse(path.clone()).map_err(|error| {
                format!("prepare(publisher): template artifact {path}: {error}")
            })?,
        };
        push_artifact(
            &mut records,
            &mut roots,
            &mut objects,
            key,
            bytes.clone(),
            None,
            "package-store.template-loader",
            "template.materialize/v1",
            BTreeMap::new(),
            template_reads.clone(),
        )?;
    }
    for file in runtime.files() {
        let key = site_build::ArtifactKey::Asset {
            namespace: site_build::AssetNamespace::PublisherRuntime,
            path: site_build::SourcePath::parse(file.path.clone()).map_err(|error| {
                format!(
                    "prepare(publisher): runtime artifact {}: {error}",
                    file.path
                )
            })?,
        };
        let mut attributes = BTreeMap::from([
            ("source".into(), file.provenance.source.clone()),
            ("license".into(), file.provenance.license.clone()),
            ("sourcePath".into(), file.provenance.source_path.clone()),
        ]);
        if let Some(transformation) = &file.provenance.transformation {
            attributes.insert("transformation".into(), transformation.clone());
        }
        push_artifact(
            &mut records,
            &mut roots,
            &mut objects,
            key,
            file.bytes.clone(),
            Some(&file.media_type),
            "publisher-runtime",
            "publisher-runtime.assemble/v1",
            attributes,
            BTreeSet::from([site_build::ReadDependency::Package {
                coordinate: core.clone(),
            }]),
        )?;
    }
    let catalog = site_build::ArtifactCatalog::from_records(records)
        .map_err(|error| format!("prepare(publisher): artifact catalog: {error}"))?;
    Ok((catalog, site_build::RenderPlan::new(roots), objects))
}

#[allow(clippy::too_many_arguments)]
fn push_artifact(
    records: &mut Vec<site_build::ArtifactRecord>,
    roots: &mut BTreeSet<site_build::ArtifactKey>,
    objects: &mut ObjectMap,
    key: site_build::ArtifactKey,
    bytes: Vec<u8>,
    media_type: Option<&str>,
    producer: &str,
    recipe: &str,
    attributes: BTreeMap<String, String>,
    reads: BTreeSet<site_build::ReadDependency>,
) -> Result<(), String> {
    let content = site_build::ContentRef::of_bytes(&bytes, media_type.map(str::to_string));
    insert_object(objects, &content, bytes)?;
    records.push(site_build::ArtifactRecord {
        key: key.clone(),
        state: site_build::ArtifactState::Ready { content },
        provenance: site_build::ArtifactProvenance {
            producer: site_build::ProducerRef::new(producer, env!("CARGO_PKG_VERSION")),
            recipe: recipe.into(),
            attributes,
        },
        reads,
    });
    roots.insert(key);
    Ok(())
}

fn authored_role_name(role: site_build::AuthoredFileRole) -> &'static str {
    match role {
        site_build::AuthoredFileRole::PageContent => "page-content",
        site_build::AuthoredFileRole::ResourceContent => "resource-content",
        site_build::AuthoredFileRole::Data => "data",
        site_build::AuthoredFileRole::Include => "include",
        site_build::AuthoredFileRole::Image => "image",
        site_build::AuthoredFileRole::ImageSource => "image-source",
    }
}

fn verify_ready_artifacts(
    build: &site_build::ClosedSiteBuild,
    objects: &ObjectMap,
    operation: &str,
) -> Result<(), String> {
    let site = build.site_build();
    if site.render_plan().is_empty() || site.artifacts().is_empty() {
        return Err(format!("{operation}: closed Publisher input is empty"));
    }
    for (path, source) in site.project().sources.iter() {
        verify_object(objects, &source.content)
            .map_err(|error| format!("{operation}: verify source {path}: {error}"))?;
    }
    for (coordinate, package) in site.package_lock().iter() {
        verify_object(objects, &package.content)
            .map_err(|error| format!("{operation}: verify package {coordinate}: {error}"))?;
    }
    for (key, record) in site.artifacts().iter() {
        let site_build::ArtifactState::Ready { content } = &record.state else {
            continue;
        };
        verify_object(objects, content)
            .map_err(|error| format!("{operation}: verify artifact {key:?}: {error}"))?;
    }
    Ok(())
}

fn verify_object(objects: &ObjectMap, content: &site_build::ContentRef) -> Result<(), String> {
    let bytes = objects
        .get(&content.sha256)
        .ok_or_else(|| format!("object {} is absent", content.sha256))?;
    if bytes.len() as u64 != content.byte_length {
        return Err(format!(
            "object {} length {} differs from {}",
            content.sha256,
            bytes.len(),
            content.byte_length
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_material_authenticates_immutable_bytes_once() {
        let content = site_build::ContentRef::of_bytes(b"expected", None::<String>);
        assert!(PackageMaterial::new(
            content.clone(),
            BTreeMap::new(),
            Rc::new(b"different".to_vec()),
        )
        .is_err());
        PackageMaterial::new(content, BTreeMap::new(), Rc::new(b"expected".to_vec())).unwrap();
    }

    #[test]
    fn project_revision_uses_one_exact_raw_source_for_a_parsed_local_resource() {
        let config = "id: overlap.ig\nfhirVersion: 4.0.1\n";
        let path = "input/resources/Patient-example.json";
        let raw = br#"{ "resourceType": "Patient", "id": "example" }"#.to_vec();
        let project = crate::ProjectRevision::new(
            crate::ProjectInputs {
                config: config.into(),
                fsh: BTreeMap::new(),
                predefined: BTreeMap::from([(path.into(), serde_json::from_slice(&raw).unwrap())]),
                site_files: BTreeMap::from([(path.into(), raw.clone())]),
            },
            crate::ResolvedPackageClosure {
                config_sha256: site_build::Sha256Digest::of_bytes(config.as_bytes()),
                resolution_support: BTreeSet::new(),
                labels: Vec::new(),
            },
        )
        .unwrap();

        let revision = site_build_project_revision(&project, "overlap.ig").unwrap();
        let entry = revision
            .sources
            .get(&site_build::SourcePath::parse(path).unwrap())
            .unwrap();
        entry.content.verify(&raw).unwrap();
        assert_eq!(revision.sources.iter().count(), 2);
    }

    fn prepared_with_all_roles() -> site_build::PreparedGuide {
        let key = site_build::SemanticResourceKey {
            resource_type: "ImplementationGuide".into(),
            id: "demo".into(),
        };
        let roles = [
            site_build::AuthoredFileRole::PageContent,
            site_build::AuthoredFileRole::ResourceContent,
            site_build::AuthoredFileRole::Data,
            site_build::AuthoredFileRole::Include,
            site_build::AuthoredFileRole::Image,
            site_build::AuthoredFileRole::ImageSource,
        ];
        let authored_files = roles
            .into_iter()
            .enumerate()
            .map(|(index, role)| {
                let path = format!("role-{index}.txt");
                site_build::AuthoredFile {
                    role,
                    path: site_build::PreparedPath::parse(path.clone()).unwrap(),
                    mime: "text/plain".into(),
                    content: path.as_bytes().to_vec(),
                    source_reads: BTreeSet::from([site_build::PreparedPath::parse(format!(
                        "input/{path}"
                    ))
                    .unwrap()]),
                }
            })
            .collect();
        site_build::PreparedGuide {
            guide: site_build::GuideIdentity {
                implementation_guide: key.clone(),
                package_id: "demo.ig".into(),
                canonical: Some("https://example.org/demo".into()),
                name: Some("Demo".into()),
                version: Some("1.0.0".into()),
                fhir_version: "4.0.1".into(),
                release_label: None,
                fhir_publication_base: "http://hl7.org/fhir/R4/".into(),
                generated: site_build::GeneratedIdentity {
                    epoch_seconds: 1,
                    date: "1970-01-01T00:00:01Z".into(),
                    day: "19700101".into(),
                },
                source_control: None,
            },
            resources: vec![site_build::SemanticResource {
                key,
                resource: serde_json::json!({
                    "resourceType": "ImplementationGuide",
                    "id": "demo"
                }),
                publication: None,
            }],
            publisher_compatibility: None,
            expansions: Vec::new(),
            pages: Vec::new(),
            menu: Vec::new(),
            sushi_config: serde_json::json!({"id":"demo.ig"}),
            authored_files,
        }
    }

    #[test]
    fn publisher_closed_input_roots_and_verifies_every_external_artifact() {
        let core_label = "hl7.fhir.r4.core#4.0.1";
        let template_label = "demo.template#1.0.0";
        let mut source = package_store::BundleSource::new();
        source.mount_package(
            core_label,
            BTreeMap::<String, Vec<u8>>::from([
                (
                    "package.json".into(),
                    br#"{"name":"hl7.fhir.r4.core","version":"4.0.1"}"#.to_vec(),
                ),
                ("other/fhir.css".into(), b"body{}".to_vec()),
                ("other/icon_element.gif".into(), b"icon".to_vec()),
                ("other/tbl_spacer.png".into(), b"spacer".to_vec()),
            ]),
        );
        source.mount_package(
            template_label,
            BTreeMap::<String, Vec<u8>>::from([
                (
                    "package.json".into(),
                    br#"{"name":"demo.template","version":"1.0.0"}"#.to_vec(),
                ),
                ("config.json".into(), br#"{"defaults":{}}"#.to_vec()),
                ("layouts/default.html".into(), b"{{ content }}".to_vec()),
            ]),
        );
        let tree = package_store::template_loader::materialize(
            &source,
            &package_store::TemplatePaths::new(source.cache_root()),
            template_label,
        )
        .unwrap();
        let core = site_build::PackageCoordinate::parse(core_label).unwrap();
        let runtime = site_producer::publisher_runtime::PublisherRuntime::assemble(
            &source,
            source.cache_root(),
            &core,
            &tree,
        )
        .unwrap();
        let prepared = prepared_with_all_roles();
        let source_entries = prepared.authored_files.iter().map(|file| {
            let path = site_build::SourcePath::parse(
                file.source_reads
                    .iter()
                    .next()
                    .unwrap()
                    .as_str()
                    .to_string(),
            )
            .unwrap();
            (
                path,
                site_build::SourceEntry {
                    kind: site_build::SourceKind::Other {
                        name: "test".into(),
                    },
                    content: site_build::ContentRef::of_bytes(
                        &file.content,
                        Some(file.mime.clone()),
                    ),
                },
            )
        });
        let project = site_build::ProjectRevision {
            project_id: "demo.ig".into(),
            revision: "test".into(),
            sources: site_build::SourceManifest::from_entries(source_entries).unwrap(),
        };
        let template = site_build::PackageCoordinate::parse(template_label).unwrap();
        let lock = site_build::PackageLock::from_packages([
            site_build::LockedPackage {
                coordinate: core.clone(),
                content: site_build::ContentRef::of_bytes(b"core", None::<String>),
                dependencies: BTreeSet::new(),
            },
            site_build::LockedPackage {
                coordinate: template.clone(),
                content: site_build::ContentRef::of_bytes(b"template", None::<String>),
                dependencies: BTreeSet::new(),
            },
        ])
        .unwrap();
        let (artifacts, plan, mut objects) = publisher_artifacts(
            &prepared,
            &project,
            &lock,
            &[template_label.into()],
            &core,
            tree.files(),
            &runtime,
        )
        .unwrap();
        for (_, package) in lock.iter() {
            let bytes = if package.coordinate == core {
                b"core".to_vec()
            } else {
                b"template".to_vec()
            };
            insert_object(&mut objects, &package.content, bytes).unwrap();
        }
        assert!(!plan.is_empty());
        assert_eq!(plan.required_artifacts().len(), artifacts.len());
        assert!(artifacts.len() > prepared.authored_files.len() + 4);
        assert!(artifacts.iter().all(|(_, record)| {
            let site_build::ArtifactState::Ready { content } = &record.state else {
                return false;
            };
            objects
                .get(&content.sha256)
                .is_some_and(|bytes| content.verify(bytes).is_ok())
        }));
        let build = site_build::SiteBuild::new(
            project,
            lock,
            site_build::RenderTarget {
                renderer: site_build::ProducerRef::new("publisher-template-rust", "test"),
                mode: site_build::RenderMode::NativeTemplate,
                fhir_version: "4.0.1".into(),
                template: Some(template),
                parameters: BTreeMap::new(),
            },
            plan,
            artifacts,
            BTreeSet::new(),
        )
        .unwrap()
        .close()
        .unwrap();
        verify_ready_artifacts(&build, &objects, "test").unwrap();
        let mut missing = objects;
        let digest = match &build
            .site_build()
            .artifacts()
            .iter()
            .next()
            .unwrap()
            .1
            .state
        {
            site_build::ArtifactState::Ready { content } => content.sha256.clone(),
            _ => unreachable!(),
        };
        missing.remove(&digest);
        assert!(verify_ready_artifacts(&build, &missing, "test").is_err());
    }

    #[test]
    fn typed_prepare_constructs_and_retains_complete_publisher_build() {
        let core_label = "hl7.fhir.r4.core#4.0.1";
        let template_label = "demo.template#1.0.0";
        let core = package_store::normalize_package_material(
            core_label,
            BTreeMap::from([
                (
                    "package.json".into(),
                    br#"{"name":"hl7.fhir.r4.core","version":"4.0.1","fhirVersions":["4.0.1"]}"#
                        .to_vec(),
                ),
                ("other/fhir.css".into(), b"body{}".to_vec()),
                ("other/icon_element.gif".into(), b"icon".to_vec()),
                ("other/tbl_spacer.png".into(), b"spacer".to_vec()),
            ]),
        )
        .unwrap();
        let template = package_store::normalize_package_material(
            template_label,
            BTreeMap::from([
                (
                    "package.json".into(),
                    br#"{"name":"demo.template","version":"1.0.0","type":"fhir.template"}"#.to_vec(),
                ),
                (
                    "config.json".into(),
                    br#"{"defaults":{"Any":{"template-base":"template/layouts/default.html","base":"{{[type]}}-{{[id]}}.html"}}}"#.to_vec(),
                ),
                (
                    "layouts/default.html".into(),
                    b"---\n---\n{{content}}".to_vec(),
                ),
            ]),
        )
        .unwrap();
        let mut source = package_store::BundleSource::new();
        source.mount_package(core_label, core.files.clone());
        source.mount_package(template_label, template.files.clone());
        let root = source.cache_root().to_path_buf();
        let environment = PackageEnvironment::new(
            PackageView::new(Rc::new(source), root, None),
            vec![core_label.into(), template_label.into()],
            BTreeMap::from([
                (
                    core_label.into(),
                    PackageMaterial {
                        content: site_build::ContentRef::of_bytes(
                            &core.payload,
                            Some(package_store::NORMALIZED_PACKAGE_MEDIA_TYPE),
                        ),
                        content_bytes: Rc::new(core.payload.clone()),
                        declared_dependencies: core.declared_dependencies,
                    },
                ),
                (
                    template_label.into(),
                    PackageMaterial {
                        content: site_build::ContentRef::of_bytes(
                            &template.payload,
                            Some(package_store::NORMALIZED_PACKAGE_MEDIA_TYPE),
                        ),
                        content_bytes: Rc::new(template.payload.clone()),
                        declared_dependencies: template.declared_dependencies,
                    },
                ),
            ]),
        )
        .unwrap();
        let config = "id: demo.ig\ncanonical: https://example.org/demo\nname: Demo\nstatus: draft\nversion: 1.0.0\nfhirVersion: 4.0.1\n";
        let resolved = crate::ResolvedPackageClosure {
            config_sha256: site_build::Sha256Digest::of_bytes(config.as_bytes()),
            resolution_support: BTreeSet::new(),
            labels: vec![core_label.into()],
        };
        let project = crate::ProjectRevision::new(
            crate::ProjectInputs {
                config: config.into(),
                fsh: BTreeMap::new(),
                predefined: BTreeMap::new(),
                site_files: BTreeMap::new(),
            },
            resolved,
        )
        .unwrap();
        let ig = serde_json::json!({
            "resourceType":"ImplementationGuide",
            "id":"demo",
            "packageId":"demo.ig",
            "url":"https://example.org/demo/ImplementationGuide/demo",
            "name":"Demo",
            "status":"draft",
            "version":"1.0.0",
            "fhirVersion":["4.0.1"],
            "definition":{}
        });
        let mut engine = SiteEngine::default();
        engine.install_compilation_for_test(
            project,
            vec![(PathBuf::from("/__ig__/ImplementationGuide-demo.json"), ig)],
        );
        let prepared = engine
            .prepare(
                GeneratorSpec::Publisher {
                    template_coordinate: template_label.into(),
                    build_epoch_secs: 1,
                    active_tables: true,
                    run_uuid: None,
                },
                environment,
            )
            .unwrap();
        let build = prepared.site_build.site_build();
        assert!(!build.render_plan().is_empty());
        assert!(!build.artifacts().is_empty());
        assert_eq!(prepared.handle, build.build_id().to_string());
        let outputs = engine.outputs(&prepared.handle).unwrap();
        assert!(!outputs.outputs.is_empty());
        for (_, source) in build.project().sources.iter() {
            source
                .content
                .verify(
                    &engine
                        .read_content(&prepared.handle, source.content.sha256.as_str())
                        .unwrap(),
                )
                .unwrap();
        }
        for (_, package) in build.package_lock().iter() {
            package
                .content
                .verify(
                    &engine
                        .read_content(&prepared.handle, package.content.sha256.as_str())
                        .unwrap(),
                )
                .unwrap();
        }
        for (_, artifact) in build.artifacts().iter() {
            let site_build::ArtifactState::Ready { content } = &artifact.state else {
                panic!("closed Publisher artifact is not ready")
            };
            content
                .verify(
                    &engine
                        .read_content(&prepared.handle, content.sha256.as_str())
                        .unwrap(),
                )
                .unwrap();
        }
    }
}

fn elapsed_ms(started: crate::runtime::PhaseTimer) -> f64 {
    started.elapsed_ms()
}

impl SiteEngine {
    fn preparation_cache_keys(
        &self,
        input: &PrepareGuideOptions,
        project: &site_build::ProjectRevision,
        package_lock: &site_build::PackageLock,
        resolved: &crate::ResolvedPackageClosure,
        operation: &str,
    ) -> Result<PreparationCacheKeys, String> {
        let compiled_sha256 = compiled_revision_sha256(self.compiled_resources(), operation)?;
        let prepared_guide = site_build::sha256_canonical(&PreparedGuideCachePayload {
            schema: PREPARED_GUIDE_CACHE_SCHEMA,
            recipe: PREPARED_GUIDE_RECIPE,
            engine_api: ENGINE_API,
            project,
            package_lock,
            compiled_sha256: &compiled_sha256,
            build_epoch_secs: input.build_epoch_secs,
            liquid_asset_dirs: &input.liquid_asset_dirs,
            branch: input.branch.as_deref(),
            revision: input.revision.as_deref(),
        })
        .map_err(|error| format!("{operation}: hash PreparedGuide cache key: {error}"))?;
        let snapshot_completed_local =
            site_build::sha256_canonical(&SnapshotCompletedLocalCachePayload {
                schema: SNAPSHOT_COMPLETED_LOCAL_CACHE_SCHEMA,
                recipe: SNAPSHOT_COMPLETED_LOCAL_RECIPE,
                engine_api: ENGINE_API,
                compiled_sha256: &compiled_sha256,
                package_lock,
                resolved_config_sha256: &resolved.config_sha256,
                resolution_support: &resolved.resolution_support,
                resolved_labels: &resolved.labels,
            })
            .map_err(|error| {
                format!("{operation}: hash snapshot-completed local cache key: {error}")
            })?;
        Ok(PreparationCacheKeys {
            prepared_guide,
            snapshot_completed_local,
        })
    }

    fn site_model_from_compile(
        &mut self,
        input: &PrepareGuideOptions,
        environment: &PackageEnvironment,
        operation: &str,
        cache_key: site_build::Sha256Digest,
        snapshot_cache_key: site_build::Sha256Digest,
    ) -> Result<site_build::PreparedGuide, String> {
        if self.compiled_resources().is_empty() {
            return Err(format!(
                "{operation}: no compiled revision; call compileProject first"
            ));
        }
        if let Some(entry) = &self.preparation.prepared_guide {
            if entry.key == cache_key {
                #[cfg(test)]
                {
                    self.preparation.cache_hits.prepared_guide += 1;
                }
                return Ok(entry.guide.clone());
            }
        }
        let snapshot_hit = self
            .preparation
            .snapshot_completed_local
            .as_ref()
            .is_some_and(|entry| entry.key == snapshot_cache_key);
        if snapshot_hit {
            #[cfg(test)]
            {
                self.preparation.cache_hits.snapshot_completed_local += 1;
            }
        } else {
            let project = self.project_revision().ok_or_else(|| {
                format!(
                    "{operation}: compileProject has not established a complete source revision"
                )
            })?;
            let resolved = project.resolved_packages();
            let (_, fhir_version) = compiled_ig_identity(self.compiled_resources())?;
            resolved_core_package(
                resolved,
                project.config(),
                &fhir_version,
                environment,
                operation,
            )?;
            let source = environment.scoped(&resolved.labels, operation)?;
            let mut context = snapshot_gen::PackageContext::new_with(
                source,
                environment.packages.root(),
                &resolved.labels,
            )
            .map_err(|error| format!("{operation}: package context: {error:#}"))?;
            let (mut local_resources, primary_implementation_guide) =
                prepared_local_resource_set(self.compiled_resources(), operation)?;
            let mut snapshot_locals = local_resources.clone();
            snapshot_locals.sort_by(|left, right| left.0.cmp(&right.0));
            context.load_local_resources(snapshot_locals);
            for (path, body) in &mut local_resources {
                if body.get("resourceType").and_then(Value::as_str) != Some("StructureDefinition") {
                    continue;
                }
                let label = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("StructureDefinition");
                *body = snapshot_gen::generate_snapshot(body.clone(), &context, Default::default())
                    .map_err(|error| format!("{operation}: snapshot {label}: {error:#}"))?;
            }
            self.preparation.snapshot_completed_local = Some(SnapshotCompletedLocalCacheEntry {
                key: snapshot_cache_key,
                generated: local_resources.into_iter().map(|(_, body)| body).collect(),
                primary_implementation_guide,
            });
        }
        let snapshot = self
            .preparation
            .snapshot_completed_local
            .as_ref()
            .expect("snapshot-completed local cache was hit or installed");
        let project = self.project_revision().expect("compiled project exists");
        let guide = assemble_prepared_model(
            operation,
            &snapshot.generated,
            &snapshot.primary_implementation_guide,
            project.config(),
            project.site_files(),
            input.build_epoch_secs,
            &input.liquid_asset_dirs,
            input.branch.clone(),
            input.revision.clone(),
        )?;
        self.preparation.prepared_guide = Some(PreparedGuideCacheEntry {
            key: cache_key,
            guide: guide.clone(),
        });
        Ok(guide)
    }
}

fn compiled_revision_sha256(
    compiled: &[(PathBuf, Value)],
    operation: &str,
) -> Result<site_build::Sha256Digest, String> {
    let values = compiled
        .iter()
        .map(|(path, body)| {
            path.to_str()
                .map(|path| (path.to_string(), body))
                .ok_or_else(|| format!("{operation}: compiled path is not UTF-8: {path:?}"))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    if values.len() != compiled.len() {
        return Err(format!(
            "{operation}: compiled revision contains duplicate paths"
        ));
    }
    site_build::sha256_canonical(&values)
        .map_err(|error| format!("{operation}: hash compiled revision: {error}"))
}

fn prepared_local_resource_set(
    render_set: &[(PathBuf, Value)],
    operation: &str,
) -> Result<(Vec<(PathBuf, Value)>, Value), String> {
    let explicit = render_set
        .iter()
        .enumerate()
        .filter(|(_, (path, body))| {
            path.starts_with("/__ig__")
                && body.get("resourceType").and_then(Value::as_str) == Some("ImplementationGuide")
        })
        .collect::<Vec<_>>();
    let (primary_index, (primary_path, primary)) = match explicit.as_slice() {
        [(index, entry)] => (*index, *entry),
        [] => return Err(format!("{operation}: compiled revision has no explicit primary ImplementationGuide artifact")),
        _ => return Err(format!("{operation}: compiled revision has multiple explicit primary ImplementationGuide artifacts")),
    };
    let identity = |body: &Value| -> Option<(String, String)> {
        Some((
            body.get("resourceType")?.as_str()?.to_string(),
            body.get("id")?.as_str()?.to_string(),
        ))
    };
    let primary_key = identity(primary).ok_or_else(|| {
        format!("{operation}: explicit primary ImplementationGuide has no resourceType/id")
    })?;
    let mut selected = BTreeMap::new();
    for (index, (path, body)) in render_set.iter().enumerate() {
        if let Some(key) = identity(body) {
            selected.insert(key, (index, path.clone(), body.clone()));
        }
    }
    selected.insert(
        primary_key,
        (primary_index, primary_path.clone(), primary.clone()),
    );
    let mut resources = selected.into_values().collect::<Vec<_>>();
    resources.sort_by_key(|(index, _, _)| *index);
    Ok((
        resources
            .into_iter()
            .map(|(_, path, body)| (path, body))
            .collect(),
        primary.clone(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn assemble_prepared_model(
    operation: &str,
    generated: &[Value],
    primary_implementation_guide: &Value,
    config: &str,
    site_files: &BTreeMap<String, Vec<u8>>,
    build_epoch_secs: i64,
    liquid_asset_dirs: &[String],
    branch: Option<String>,
    revision: Option<String>,
) -> Result<site_build::PreparedGuide, String> {
    let root = PathBuf::from("/ig");
    let vfs = site_files
        .iter()
        .map(|(path, bytes)| (root.join(path), bytes.clone()))
        .collect();
    let asset_dirs = if liquid_asset_dirs.is_empty() {
        vec!["input/includes".to_string()]
    } else {
        liquid_asset_dirs.to_vec()
    };
    let files = prepared_guide::MemFiles::new(vfs);
    prepared_guide::semantics::prepare(&prepared_guide::semantics::PrepareInputs {
        generated,
        primary_implementation_guide,
        examples: &[],
        sushi_config_yaml: config,
        build_epoch_secs,
        branch,
        revision,
        augmentation: prepared_guide::AugmentInputs {
            ig: primary_implementation_guide,
            sushi_config_yaml: config,
            project_root: root.clone(),
            pagecontent_dir: root.join("input/pagecontent"),
            image_dir: root.join("input/images"),
            liquid_asset_dirs: asset_dirs.iter().map(|dir| root.join(dir)).collect(),
            files: &files,
        },
    })
    .map_err(|error| format!("{operation}: prepare guide: {error:#}"))
}

fn site_build_project_revision(
    project: &crate::ProjectRevision,
    project_id: &str,
) -> Result<site_build::ProjectRevision, String> {
    let mut entries = BTreeMap::new();
    let mut insert = |path: &str,
                      kind: site_build::SourceKind,
                      bytes: &[u8],
                      media_type: &str|
     -> Result<(), String> {
        let path = site_build::SourcePath::parse(path.to_string())
            .map_err(|error| format!("prepare: source path {path}: {error}"))?;
        if entries
            .insert(
                path.clone(),
                site_build::SourceEntry {
                    kind,
                    content: site_build::ContentRef::of_bytes(bytes, Some(media_type)),
                },
            )
            .is_some()
        {
            return Err(format!(
                "prepare: source path {path} is declared by more than one input channel"
            ));
        }
        Ok(())
    };
    insert(
        "sushi-config.yaml",
        site_build::SourceKind::Config,
        project.config().as_bytes(),
        "application/yaml",
    )?;
    for (path, body) in project.fsh() {
        insert(
            path,
            site_build::SourceKind::Fsh,
            body.as_bytes(),
            "text/fhir-shorthand",
        )?;
    }
    for (path, body) in project.predefined() {
        // Parsed local resources and their raw authored bytes are one input, not
        // two source channels. Preserve the exact authored stream when present;
        // otherwise capture a deterministic canonical representation.
        if project.site_files().contains_key(path) {
            continue;
        }
        let bytes = site_build::canonical_json_bytes(body)
            .map_err(|error| format!("prepare: canonical predefined {path}: {error}"))?;
        insert(
            path,
            site_build::SourceKind::PredefinedResource,
            &bytes,
            "application/fhir+json",
        )?;
    }
    for (path, bytes) in project.site_files() {
        let (kind, media_type) = source_kind_and_media_type(path);
        insert(path, kind, bytes, media_type)?;
    }
    let sources = site_build::SourceManifest::from_entries(entries)
        .map_err(|error| format!("prepare: source manifest: {error}"))?;
    let revision = format!(
        "sources-sha256:{}",
        site_build::sha256_canonical(&sources)
            .map_err(|error| format!("prepare: source revision: {error}"))?
    );
    Ok(site_build::ProjectRevision {
        project_id: project_id.into(),
        revision,
        sources,
    })
}

fn site_build_package_lock(
    project: &crate::ProjectRevision,
    environment: &PackageEnvironment,
) -> Result<site_build::PackageLock, String> {
    let resolved = project.resolved_packages();
    if resolved.config_sha256 != site_build::Sha256Digest::of_bytes(project.config().as_bytes()) {
        return Err("prepare: compiled package closure belongs to different config bytes".into());
    }
    let mut coordinates_by_id: BTreeMap<String, Vec<site_build::PackageCoordinate>> =
        BTreeMap::new();
    let mut ordered = Vec::new();
    for label in &resolved.labels {
        let coordinate = site_build::PackageCoordinate::parse(label)
            .map_err(|error| format!("prepare: non-exact resolved package {label}: {error}"))?;
        coordinates_by_id
            .entry(coordinate.package_id().to_string())
            .or_default()
            .push(coordinate.clone());
        ordered.push((label, coordinate));
    }
    let mut support_by_id: BTreeMap<String, Vec<site_build::PackageCoordinate>> = BTreeMap::new();
    for label in &resolved.resolution_support {
        if !environment.materials.contains_key(label) {
            return Err(format!(
                "prepare: resolution-support package {label} has no mounted material"
            ));
        }
        let coordinate = site_build::PackageCoordinate::parse(label).map_err(|error| {
            format!("prepare: non-exact resolution-support package {label}: {error}")
        })?;
        support_by_id
            .entry(coordinate.package_id().to_string())
            .or_default()
            .push(coordinate);
    }
    let mut packages = Vec::new();
    for (label, coordinate) in ordered {
        let material = environment
            .materials
            .get(label)
            .ok_or_else(|| format!("prepare: resolved package {label} has no mounted material"))?;
        let dependencies = material
            .declared_dependencies
            .iter()
            .filter_map(|(package_id, requested)| {
                coordinates_by_id.get(package_id).map(|candidates| {
                    (
                        package_id,
                        requested,
                        candidates,
                        support_by_id
                            .get(package_id)
                            .map(Vec::as_slice)
                            .unwrap_or_default(),
                    )
                })
            })
            .map(|(package_id, requested, candidates, support)| {
                select_locked_dependency(package_id, requested, candidates, support)
            })
            .filter_map(Result::transpose)
            .collect::<Result<BTreeSet<_>, _>>()?;
        packages.push(site_build::LockedPackage {
            coordinate,
            content: material.content.clone(),
            dependencies,
        });
    }
    site_build::PackageLock::from_packages(packages)
        .map_err(|error| format!("prepare: package lock: {error}"))
}

fn environment_objects(
    project: &crate::ProjectRevision,
    package_lock: &site_build::PackageLock,
    environment: &PackageEnvironment,
    operation: &str,
) -> Result<ObjectMap, String> {
    let (project_id, _) = compiled_ig_identity_from_project(project)?;
    let revision = site_build_project_revision(project, &project_id)?;
    let mut objects = BTreeMap::new();
    let source_bytes = project_source_bytes(project)?;
    for (path, entry) in revision.sources.iter() {
        let bytes = source_bytes
            .get(path.as_str())
            .ok_or_else(|| format!("{operation}: source bytes for {path} are absent"))?;
        insert_object(&mut objects, &entry.content, bytes.clone())?;
    }
    for (coordinate, _) in package_lock.iter() {
        let label = coordinate.as_str();
        let material = environment
            .materials
            .get(label)
            .ok_or_else(|| format!("{operation}: package material for {label} is absent"))?;
        let bytes = material.content_bytes.clone();
        insert_authenticated_object(&mut objects, &material.content, bytes)?;
    }
    Ok(objects)
}

fn project_source_bytes(
    project: &crate::ProjectRevision,
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut bytes = BTreeMap::from([(
        "sushi-config.yaml".into(),
        project.config().as_bytes().to_vec(),
    )]);
    bytes.extend(
        project
            .fsh()
            .iter()
            .map(|(path, source)| (path.clone(), source.as_bytes().to_vec())),
    );
    for (path, body) in project.predefined() {
        bytes.insert(
            path.clone(),
            serde_json::to_vec(body)
                .map_err(|error| format!("prepare: serialize predefined {path}: {error}"))?,
        );
    }
    bytes.extend(project.site_files().clone());
    Ok(bytes)
}

fn compiled_ig_identity_from_project(
    project: &crate::ProjectRevision,
) -> Result<(String, String), String> {
    let config: Value = serde_yaml::from_str(project.config())
        .map_err(|error| format!("prepare: parse config identity: {error}"))?;
    let project_id = config
        .get("id")
        .or_else(|| config.get("packageId"))
        .and_then(Value::as_str)
        .ok_or("prepare: config has no id/packageId")?;
    let fhir_version = config
        .get("fhirVersion")
        .and_then(Value::as_str)
        .ok_or("prepare: config has no fhirVersion")?;
    Ok((project_id.into(), fhir_version.into()))
}

fn source_kind_and_media_type(path: &str) -> (site_build::SourceKind, &'static str) {
    let lower = path.to_ascii_lowercase();
    let media = if lower.ends_with(".json") {
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
    let kind = if lower.ends_with(".json")
        && (path.starts_with("input/resources/") || path.starts_with("input/examples/"))
    {
        site_build::SourceKind::PredefinedResource
    } else if ["input/pagecontent/", "input/pages/", "input/resource-docs/"]
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        site_build::SourceKind::Page
    } else if path.starts_with("input/images/")
        || matches!(
            path.rsplit_once('.')
                .map(|(_, ext)| ext.to_ascii_lowercase())
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
        site_build::SourceKind::Asset
    } else {
        site_build::SourceKind::Other {
            name: "site_file".into(),
        }
    };
    (kind, media)
}

fn compiled_ig_identity(compiled: &[(PathBuf, Value)]) -> Result<(String, String), String> {
    let ig = compiled
        .iter()
        .find(|(path, body)| {
            path.starts_with("/__ig__")
                && body.get("resourceType").and_then(Value::as_str) == Some("ImplementationGuide")
        })
        .map(|(_, body)| body)
        .ok_or("prepare: compiled revision has no ImplementationGuide")?;
    let project_id = ig
        .get("packageId")
        .or_else(|| ig.get("id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or("prepare: ImplementationGuide has no packageId/id")?
        .to_string();
    let fhir_version = match ig.get("fhirVersion") {
        Some(Value::String(value)) => Some(value.as_str()),
        Some(Value::Array(values)) => values.first().and_then(Value::as_str),
        _ => None,
    }
    .filter(|value| !value.trim().is_empty())
    .ok_or("prepare: ImplementationGuide has no fhirVersion")?
    .to_string();
    Ok((project_id, fhir_version))
}

fn site_build_diagnostics(
    diagnostics: &[crate::CompilationDiagnostic],
) -> BTreeSet<site_build::BuildDiagnostic> {
    diagnostics
        .iter()
        .enumerate()
        .map(|(sequence, diagnostic)| {
            let severity = match diagnostic.severity.to_ascii_lowercase().as_str() {
                "error" => site_build::DiagnosticSeverity::Error,
                "warning" => site_build::DiagnosticSeverity::Warning,
                _ => site_build::DiagnosticSeverity::Information,
            };
            let mut output = site_build::BuildDiagnostic::new(
                severity,
                "sushi.compile",
                diagnostic.message.clone(),
            )
            .with_sequence(sequence as u64);
            if let (Some(file), Some(line)) = (&diagnostic.file, diagnostic.line) {
                if let Ok(path) = site_build::SourcePath::parse(file.clone()) {
                    output.location = Some(site_build::SourceLocation {
                        path,
                        line,
                        column: 0,
                    });
                }
            }
            output
        })
        .collect()
}

fn select_locked_dependency(
    package_id: &str,
    requested: &str,
    candidates: &[site_build::PackageCoordinate],
    support: &[site_build::PackageCoordinate],
) -> Result<Option<site_build::PackageCoordinate>, String> {
    let requested = if package_id.starts_with("hl7.fhir.")
        && package_id.ends_with(".core")
        && requested == "4.0.0"
    {
        "4.0.1"
    } else {
        requested
    };
    let mut matches = candidates
        .iter()
        .filter(|candidate| package_version_matches(candidate.version(), requested))
        .cloned()
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| package_version_cmp(left.version(), right.version()));
    if let Some(selected) = matches.pop() {
        return Ok(Some(selected));
    }
    if support
        .iter()
        .any(|candidate| package_version_matches(candidate.version(), requested))
    {
        return Ok(None);
    }
    Err(format!(
        "prepare: dependency {package_id}#{requested} matches neither the resolved coordinates nor an intentionally excluded resolution-support coordinate"
    ))
}

fn package_version_matches(version: &str, pattern: &str) -> bool {
    if matches!(
        pattern.to_ascii_lowercase().as_str(),
        "latest" | "current" | "dev"
    ) {
        return true;
    }
    let version = version.split('.').collect::<Vec<_>>();
    for (index, part) in pattern.split('.').enumerate() {
        if matches!(part.to_ascii_lowercase().as_str(), "x" | "*") {
            return true;
        }
        if version.get(index) != Some(&part) {
            return false;
        }
    }
    true
}

fn package_version_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    fn parts(value: &str) -> Vec<(u8, String)> {
        value
            .split(['.', '-', '+'])
            .map(|part| {
                part.parse::<u64>()
                    .map(|number| (0, format!("{number:020}")))
                    .unwrap_or_else(|_| (1, part.to_ascii_lowercase()))
            })
            .collect()
    }
    parts(left).cmp(&parts(right)).then_with(|| left.cmp(right))
}

fn core_coordinate_for_fhir_version(fhir_version: &str) -> Result<(&'static str, String), String> {
    let numeric = fhir_version.split('-').next().unwrap_or(fhir_version);
    let mut parts = numeric.split('.');
    let package_id = match (parts.next().unwrap_or(""), parts.next().unwrap_or("")) {
        ("4", "0") => "hl7.fhir.r4.core",
        ("4", "1" | "3") => "hl7.fhir.r4b.core",
        ("5", _) => "hl7.fhir.r5.core",
        ("6", _) => "hl7.fhir.r6.core",
        _ => return Err(format!("unsupported FHIR version {fhir_version}")),
    };
    let version = if package_id == "hl7.fhir.r4.core" && fhir_version == "4.0.0" {
        "4.0.1".into()
    } else {
        fhir_version.into()
    };
    Ok((package_id, version))
}

fn resolved_core_package(
    resolved: &crate::ResolvedPackageClosure,
    config: &str,
    fhir_version: &str,
    environment: &PackageEnvironment,
    operation: &str,
) -> Result<String, String> {
    if resolved.config_sha256 != site_build::Sha256Digest::of_bytes(config.as_bytes()) {
        return Err(format!(
            "{operation}: resolved package closure belongs to different config bytes"
        ));
    }
    let (expected_id, expected_version) = core_coordinate_for_fhir_version(fhir_version)?;
    let expected = format!("{expected_id}#{expected_version}");
    let matches = resolved
        .labels
        .iter()
        .filter(|label| label.split_once('#').map(|(id, _)| id) == Some(expected_id))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [label] if label.as_str() == expected => {
            if !environment.mounted_labels.contains(label)
                || !environment.materials.contains_key(label.as_str())
            {
                Err(format!(
                    "{operation}: resolved core package {label} has no mounted material"
                ))
            } else {
                Ok((*label).clone())
            }
        }
        [label] => Err(format!(
            "{operation}: FHIR version {fhir_version} requires {expected}, resolved {label}"
        )),
        [] => Err(format!("{operation}: no resolved core package {expected}")),
        _ => Err(format!(
            "{operation}: multiple resolved {expected_id} packages"
        )),
    }
}

fn target_core_from_package_lock(
    package_lock: &site_build::PackageLock,
    fhir_version: &str,
    operation: &str,
) -> Result<site_build::PackageCoordinate, String> {
    let (expected_id, expected_version) = core_coordinate_for_fhir_version(fhir_version)?;
    let expected = format!("{expected_id}#{expected_version}");
    let matches = package_lock
        .iter()
        .map(|(coordinate, _)| coordinate)
        .filter(|coordinate| coordinate.package_id() == expected_id)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [coordinate] if coordinate.version() == expected_version => Ok((*coordinate).clone()),
        [coordinate] => Err(format!(
            "{operation}: FHIR version {fhir_version} requires target core {expected}, but lock contains {coordinate}"
        )),
        [] => Err(format!("{operation}: exact package lock has no required target core {expected}")),
        _ => Err(format!("{operation}: exact package lock contains multiple {expected_id} coordinates")),
    }
}

fn with_template_chain(
    compile_lock: &site_build::PackageLock,
    chain: &[String],
    materials: &BTreeMap<String, PackageMaterial>,
    operation: &str,
) -> Result<site_build::PackageLock, String> {
    let mut packages = compile_lock
        .iter()
        .map(|(coordinate, package)| (coordinate.clone(), package.clone()))
        .collect::<BTreeMap<_, _>>();
    let coordinates = chain
        .iter()
        .map(|label| {
            site_build::PackageCoordinate::parse(label)
                .map(|coordinate| (label, coordinate))
                .map_err(|error| format!("{operation}: non-exact template {label}: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut candidates_by_id: BTreeMap<String, Vec<site_build::PackageCoordinate>> =
        BTreeMap::new();
    for coordinate in compile_lock
        .iter()
        .map(|(coordinate, _)| coordinate)
        .chain(coordinates.iter().map(|(_, coordinate)| coordinate))
    {
        candidates_by_id
            .entry(coordinate.package_id().into())
            .or_default()
            .push(coordinate.clone());
    }
    for (index, (label, coordinate)) in coordinates.iter().enumerate() {
        let material = materials.get(*label).ok_or_else(|| {
            format!("{operation}: template package {label} has no authenticated material")
        })?;
        let mut dependencies = material
            .declared_dependencies
            .iter()
            .filter_map(|(id, requested)| {
                candidates_by_id
                    .get(id)
                    .map(|candidates| (id, requested, candidates))
            })
            .map(|(id, requested, candidates)| {
                select_locked_dependency(id, requested, candidates, &[])?.ok_or_else(|| {
                    format!("{operation}: template dependency {id}#{requested} was excluded")
                })
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        if index > 0 {
            dependencies.insert(coordinates[index - 1].1.clone());
        }
        let mut locked = site_build::LockedPackage {
            coordinate: coordinate.clone(),
            content: material.content.clone(),
            dependencies,
        };
        if let Some(existing) = packages.get(coordinate) {
            if existing.content != locked.content {
                return Err(format!(
                    "{operation}: template coordinate {coordinate} disagrees with compile lock"
                ));
            }
            locked
                .dependencies
                .extend(existing.dependencies.iter().cloned());
        }
        packages.insert(coordinate.clone(), locked);
    }
    site_build::PackageLock::from_packages(packages.into_values())
        .map_err(|error| format!("{operation}: extend template package lock: {error}"))
}

fn closed_site_build_cache_key(
    prepared_guide_key: &site_build::Sha256Digest,
    render_target: &site_build::RenderTarget,
    diagnostics: &BTreeSet<site_build::BuildDiagnostic>,
) -> Result<site_build::Sha256Digest, site_build::CanonicalError> {
    site_build::sha256_canonical(&ClosedSiteBuildCachePayload {
        schema: CLOSED_SITE_BUILD_CACHE_SCHEMA,
        recipe: CYCLE_PROJECTION_RECIPE,
        prepared_guide_key,
        render_target,
        diagnostics,
    })
}

impl SiteEngine {
    fn prepare_publisher(
        &mut self,
        template_coordinate: &str,
        guide_options: PrepareGuideOptions,
        active_tables: bool,
        run_uuid: Option<String>,
        environment: &PackageEnvironment,
    ) -> Result<PrepareResult, String> {
        let total_started = self.timer();
        let operation = "prepare(publisher)";
        let mut metrics = PrepareMetrics::default();
        if template_coordinate.trim().is_empty() {
            return Err(format!("{operation}: template coordinate is empty"));
        }
        let template = site_build::PackageCoordinate::parse(template_coordinate)
            .map_err(|error| format!("{operation}: template coordinate must be exact: {error}"))?;
        let project = self.project_revision().cloned().ok_or_else(|| {
            format!("{operation}: compileProject has not established a complete source revision")
        })?;
        let (project_id, fhir_version) = compiled_ig_identity(self.compiled_resources())
            .map_err(|error| format!("{operation}: {error}"))?;
        let started = self.timer();
        let project_revision = site_build_project_revision(&project, &project_id)?;
        metrics.project_revision_ms = elapsed_ms(started);
        let started = self.timer();
        let compile_lock = site_build_package_lock(&project, environment)?;
        metrics.package_lock_ms = elapsed_ms(started);
        let started = self.timer();
        let keys = self.preparation_cache_keys(
            &guide_options,
            &project_revision,
            &compile_lock,
            project.resolved_packages(),
            operation,
        )?;
        metrics.snapshot_completed_local_cache_hit = self
            .preparation
            .snapshot_completed_local
            .as_ref()
            .is_some_and(|entry| entry.key == keys.snapshot_completed_local);
        metrics.prepared_guide_cache_hit = self
            .preparation
            .prepared_guide
            .as_ref()
            .is_some_and(|entry| entry.key == keys.prepared_guide);
        metrics.prepared_guide_key_ms = elapsed_ms(started);

        let resolution = environment.resolve_template(template_coordinate)?;
        if let Some(missing) = &resolution.missing {
            return Err(format!(
                "{operation}: template chain is incomplete; missing {missing}"
            ));
        }
        let started = self.timer();
        let package_lock = with_template_chain(
            &compile_lock,
            &resolution.chain,
            &environment.materials,
            operation,
        )?;
        metrics.package_lock_ms += elapsed_ms(started);
        let diagnostics = site_build_diagnostics(self.compile_diagnostics());
        let renderer =
            site_build::ProducerRef::new("publisher-template-rust", env!("CARGO_PKG_VERSION"));
        let preparation_key =
            site_build::sha256_canonical(&PublisherRuntimePreparationKeyPayload {
                schema: PUBLISHER_RUNTIME_PREPARATION_SCHEMA,
                recipe: PUBLISHER_RUNTIME_PREPARATION_RECIPE,
                engine_api: ENGINE_API,
                project: &project_revision,
                package_lock: &package_lock,
                prepared_guide_key: &keys.prepared_guide,
                diagnostics: &diagnostics,
                template: &template,
                template_chain: &resolution.chain,
                renderer: &renderer,
                active_tables,
                run_uuid: run_uuid.as_deref(),
            })
            .map_err(|error| format!("{operation}: retained runtime key: {error}"))?;
        if let Some((handle, build)) = self.reuse_publisher(&preparation_key) {
            metrics.site_build_cache_hit = true;
            metrics.total_ms = elapsed_ms(total_started);
            #[cfg(test)]
            {
                self.preparation.cache_hits.retained_publisher_runtime += 1;
            }
            return Ok(PrepareResult {
                handle: handle.clone(),
                build_id: handle,
                generator: "publisher".into(),
                site_build: build,
                metrics,
            });
        }

        let started = self.timer();
        let prepared = self.site_model_from_compile(
            &guide_options,
            environment,
            operation,
            keys.prepared_guide.clone(),
            keys.snapshot_completed_local,
        )?;
        metrics.prepared_guide_ms = elapsed_ms(started);
        if prepared.guide.package_id != project_id || prepared.guide.fhir_version != fhir_version {
            return Err(format!(
                "{operation}: PreparedGuide identity disagrees with compiled project"
            ));
        }
        let options = SiteOptions {
            active_tables,
            run_uuid: run_uuid.clone(),
            ..Default::default()
        };
        let started = self.timer();
        let tree = package_store::template_loader::materialize(
            &environment.packages,
            &package_store::TemplatePaths::new(environment.packages.root()),
            template_coordinate,
        )
        .map_err(|error| format!("{operation}: materialize {template_coordinate}: {error}"))?;
        metrics.template_materialize_ms = elapsed_ms(started);
        let core = target_core_from_package_lock(&package_lock, &fhir_version, operation)?;
        let started = self.timer();
        let publisher = site_producer::publisher_runtime::PublisherRuntime::assemble(
            &environment.packages,
            environment.packages.root(),
            &core,
            &tree,
        )
        .map_err(|error| format!("{operation}: Publisher runtime: {error:#}"))?;
        let mut ready = BTreeMap::new();
        let mut objects = environment_objects(&project, &package_lock, environment, operation)?;
        for file in publisher.files() {
            let transformation = file
                .provenance
                .transformation
                .as_deref()
                .map(|value| format!("; transformation={value}"))
                .unwrap_or_default();
            insert_prepared_output(
                &mut ready,
                &mut objects,
                &file.path,
                file.bytes.clone(),
                &file.media_type,
                site_build::OutputProducer {
                    id: "publisher-runtime".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
                Some(format!(
                    "{}; license={}; source={}{}",
                    file.provenance.source,
                    file.provenance.license,
                    file.provenance.source_path,
                    transformation
                )),
                PreparedOutputCollision::Reject,
            )?;
        }
        metrics.publisher_runtime_ms = elapsed_ms(started);

        let started = self.timer();
        let config_json: Value = serde_json::from_slice(tree.get("config.json").ok_or_else(|| {
            format!("{operation}: no template config.json; Publisher template assembly is incomplete")
        })?)
        .map_err(|error| format!("{operation}: bad template config.json: {error}"))?;
        let layouts = tree
            .files()
            .iter()
            .filter_map(|(relative, bytes)| {
                relative
                    .strip_prefix("layouts/")
                    .and_then(|_| String::from_utf8(bytes.clone()).ok())
                    .map(|text| (format!("template/{relative}"), text))
            })
            .collect::<HashMap<_, _>>();
        if layouts.is_empty() {
            return Err(format!(
                "{operation}: no template layouts; Publisher template assembly is incomplete"
            ));
        }
        let producer_inputs =
            site_producer::ProducerInputs::from_prepared(&prepared, &config_json, layouts, "en/")
                .map_err(|error| format!("{operation}: {error:#}"))?;
        let produced = site_producer::produce(&producer_inputs)
            .map_err(|error| format!("{operation}: {error:#}"))?;
        let resource_pages = produced.resource_pages;
        let template_files = tree.files().clone();
        let mut site_files = HashMap::new();
        for (relative, bytes) in tree.into_files() {
            let mounted = match relative.strip_prefix("includes/") {
                Some(name) => format!("_includes/{name}"),
                None => format!("template/{relative}"),
            };
            site_files.insert(PathBuf::from(format!("/site/{mounted}")), bytes);
        }
        for (name, body) in produced.pages {
            site_files.insert(PathBuf::from(format!("/site/{name}")), body.into_bytes());
        }
        for (name, body) in produced.data {
            site_files.insert(
                PathBuf::from(format!("/site/_data/{name}")),
                body.into_bytes(),
            );
        }
        for (name, body) in produced.includes {
            site_files.insert(
                PathBuf::from(format!("/site/_includes/{name}")),
                body.into_bytes(),
            );
        }
        stage_prepared_authored_files(
            &prepared,
            &mut site_files,
            &mut ready,
            &mut objects,
            &project_id,
        )?;
        metrics.publisher_model_ms = elapsed_ms(started);

        let started = self.timer();
        let render_packages = environment.scoped(&project.resolved_packages().labels, operation)?;
        let semantics = build_render_semantics(
            prepared_render_set(&prepared)?,
            Some(render_packages),
            &site_files,
            &options,
        )?;
        let state = Rc::new(build_render_state_from_semantics(
            &semantics,
            &site_files,
            &options,
        )?);
        metrics.render_model_ms = elapsed_ms(started);

        let started = self.timer();
        let pages = state.list_pages();
        add_page_relative_output_aliases(&mut ready, &pages)?;
        let tree_manifest = site_files
            .iter()
            .map(|(path, bytes)| {
                (
                    path.to_string_lossy().into_owned(),
                    site_build::ContentRef::of_bytes(bytes, None::<String>),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let tree_digest = site_build::sha256_canonical(&tree_manifest)
            .map_err(|error| format!("{operation}: hash mounted tree: {error}"))?;
        let mut parameters = BTreeMap::from([
            ("contract".into(), "publisher-site/v1".into()),
            ("mountedTreeSha256".into(), tree_digest.to_string()),
            (
                "preparedGuideSha256".into(),
                keys.prepared_guide.to_string(),
            ),
            (
                "publisherRuntimeRecipeSha256".into(),
                publisher.recipe_sha256().to_string(),
            ),
            (
                "buildEpochSecs".into(),
                guide_options.build_epoch_secs.to_string(),
            ),
            ("activeTables".into(), active_tables.to_string()),
        ]);
        if let Some(run_uuid) = &run_uuid {
            parameters.insert("runUuid".into(), run_uuid.clone());
        }
        let (artifacts, plan, artifact_objects) = publisher_artifacts(
            &prepared,
            &project_revision,
            &package_lock,
            &resolution.chain,
            &core,
            &template_files,
            &publisher,
        )?;
        merge_objects(&mut objects, artifact_objects)?;
        let build = site_build::SiteBuild::new(
            project_revision,
            package_lock,
            site_build::RenderTarget {
                renderer,
                mode: site_build::RenderMode::NativeTemplate,
                fhir_version,
                template: Some(template.clone()),
                parameters: parameters.clone(),
            },
            plan,
            artifacts,
            diagnostics,
        )
        .map_err(|error| format!("{operation}: SiteBuild: {error}"))?
        .close()
        .map_err(|error| format!("{operation}: close SiteBuild: {error}"))?;
        verify_ready_artifacts(&build, &objects, operation)?;
        let catalog = output_catalog(&ready, pages, &resource_pages, operation)?;
        let renderer = site_build::RendererImplementation {
            id: "publisher-template-rust".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            recipe_sha256: site_build::sha256_canonical(&(template, &parameters))
                .map_err(|error| format!("{operation}: renderer recipe: {error}"))?,
        };
        let handle = build.site_build().build_id().to_string();
        let installed = self.install_publisher(PublisherRuntime {
            preparation_key,
            state,
            publisher: Some(publisher),
            build: build.clone(),
            catalog,
            ready,
            objects,
            renderer,
            output_options: parameters,
        });
        debug_assert_eq!(installed, handle);
        metrics.catalog_ms = elapsed_ms(started);
        metrics.total_ms = elapsed_ms(total_started);
        Ok(PrepareResult {
            handle: handle.clone(),
            build_id: handle,
            generator: "publisher".into(),
            site_build: build,
            metrics,
        })
    }
}

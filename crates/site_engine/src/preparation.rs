use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;

use package_store::PackageSource;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::render_surface::RenderSemantics;
use crate::runtime::{
    AuthenticatedObject, ObjectMap, OutputDescriptor, OutputResourceSubject, OutputSubjectPage,
    PreparedOutput, PublisherRecipeAssets, PublisherRuntime,
};
use crate::{
    build_render_semantics, build_render_state_from_semantics, PackageView, RenderState,
    SiteEngine, SiteOptions,
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
const PUBLISHER_RENDER_SEMANTICS_CACHE_SCHEMA: &str = "publisher-render-semantics-cache/v1";
const PUBLISHER_RECIPE_ASSETS_SCHEMA: &str = "publisher-recipe-assets/v1";
const PUBLISHER_RECIPE_ASSETS_RECIPE: &str =
    "package-store.template-loader+site-producer.publisher-runtime/v1";

const PUBLISHER_AUTHORED_NAMESPACE_PREFIX: &str = "publisher.authored";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "wire-contract", ts(optional_fields))]
#[serde(tag = "generator", rename_all = "camelCase", deny_unknown_fields)]
pub enum GeneratorSpec {
    Cycle {
        #[serde(rename = "buildEpochSecs")]
        build_epoch_secs: i64,
        #[serde(rename = "liquidAssetDirs")]
        liquid_asset_dirs: Vec<String>,
        #[serde(default)]
        #[cfg_attr(feature = "wire-contract", ts(optional, type = "string | null"))]
        branch: Option<String>,
        #[serde(default)]
        #[cfg_attr(feature = "wire-contract", ts(optional, type = "string | null"))]
        revision: Option<String>,
    },
    Publisher {
        #[serde(rename = "templateCoordinate")]
        template_coordinate: String,
        #[serde(rename = "buildEpochSecs")]
        build_epoch_secs: i64,
        #[serde(rename = "activeTables")]
        active_tables: bool,
        #[serde(default, rename = "runUuid")]
        #[cfg_attr(feature = "wire-contract", ts(optional, type = "string | null"))]
        run_uuid: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum GeneratorKind {
    Cycle,
    Publisher,
}

#[derive(Clone)]
struct PackageMaterial {
    object: AuthenticatedObject,
    declared_dependencies: BTreeMap<String, String>,
}

impl PackageMaterial {
    /// Admit the exact deterministic prepared-package carrier consumed by the
    /// mounted view. The carrier itself is the package-lock object; members
    /// remain compressed and are verified lazily by `BundleSource` when read.
    fn from_prepared(mount: package_store::PreparedPackageMount) -> Result<Self, String> {
        let artifact = mount.artifact_bytes();
        let content = site_build::ContentRef {
            sha256: site_build::Sha256Digest::parse(mount.artifact_sha256())
                .map_err(|error| format!("prepared package carrier digest: {error}"))?,
            byte_length: artifact.len() as u64,
            media_type: Some(package_store::PREPARED_PACKAGE_MEDIA_TYPE.into()),
        };
        let declared_dependencies = mount.declared_dependencies.clone();
        Ok(Self {
            object: AuthenticatedObject::eager_authenticated(content, artifact)
                .map_err(|error| format!("prepared package carrier: {error}"))?,
            declared_dependencies,
        })
    }

    fn content(&self) -> &site_build::ContentRef {
        self.object.content()
    }

    fn object(&self) -> AuthenticatedObject {
        self.object.clone()
    }
}

impl std::fmt::Debug for PackageMaterial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PackageMaterial")
            .field("content", self.content())
            .field("declared_dependencies", &self.declared_dependencies)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct PackageEnvironment {
    packages: PackageView,
    mounted_labels: Vec<String>,
    materials: BTreeMap<String, PackageMaterial>,
}

impl PackageEnvironment {
    /// Construct the package view and its closed carrier material from the same
    /// typed packages. Callers cannot pair a same-label carrier with unrelated
    /// execution bytes.
    pub fn new(
        prepared: impl IntoIterator<Item = package_store::PreparedPackage>,
    ) -> Result<Self, String> {
        let mut source = package_store::BundleSource::new();
        let mut mounted_labels = Vec::new();
        let mut materials = BTreeMap::new();
        for package in prepared {
            let label = package.label.clone();
            if materials.contains_key(&label) {
                return Err(format!("package environment repeats mounted label {label}"));
            }
            let material = PackageMaterial::from_prepared(package.mount_into(&mut source))?;
            mounted_labels.push(label.clone());
            materials.insert(label, material);
        }
        let root = source.cache_root().to_path_buf();
        let carrier_identities = materials
            .iter()
            .map(|(label, material)| (label.clone(), material.content().clone()))
            .collect();
        Ok(Self {
            packages: PackageView::new(Rc::new(source), root, None)
                .with_carrier_identities(carrier_identities),
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

    pub fn scoped(&self, labels: &[String], operation: &str) -> Result<PackageView, String> {
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
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "wire-contract", ts(optional_fields))]
#[serde(rename_all = "camelCase")]
pub struct TemplateResolution {
    pub satisfied: bool,
    pub chain: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "String"))]
    pub missing: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PrepareResult {
    pub build_id: String,
    pub generator: GeneratorKind,
    pub site_build: site_build::ClosedSiteBuild,
    #[serde(skip)]
    #[cfg_attr(feature = "wire-contract", ts(skip))]
    #[cfg_attr(feature = "wire-contract", schemars(skip))]
    measurements: PrepareMeasurements,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PreparedProjectResult {
    #[serde(rename = "compiled")]
    pub compilation: crate::CompilationOutcome,
    pub site: PrepareResult,
    pub events: Vec<crate::BuildEvent>,
}

/// Platform adapter that captures one immutable authored revision. Filesystem
/// and browser Workspace implementations own path/storage mechanics; the core
/// sees only the canonical ProjectRevision they capture. This adapter also
/// owns the operation lease: cancellation is observed between immutable
/// preparation phases through `cancelled`.
pub trait ProjectSource {
    fn cancelled(&self) -> bool {
        false
    }
    fn config(&mut self) -> Result<String, String>;
    fn capture(
        &mut self,
        packages: &PackageEnvironment,
        resolved: &crate::ResolvedPackageClosure,
    ) -> Result<crate::ProjectRevision, String>;
}

/// Platform adapter that transports the exact package coordinates selected for
/// a project. Resolution policy stays in Rust; native disk and browser-mounted
/// sources differ only in how they provide the authenticated environment.
pub trait PackageProvider {
    fn resolve(
        &mut self,
        config: &str,
        generator: &GeneratorSpec,
    ) -> Result<crate::ResolvedPackageClosure, String>;
    fn environment(
        &mut self,
        resolved: &crate::ResolvedPackageClosure,
    ) -> Result<PackageEnvironment, String>;
}

#[derive(Clone, Debug, Default)]
struct PrepareMeasurements {
    total_ms: f64,
    project_revision_ms: f64,
    package_lock_ms: f64,
    prepared_guide_key_ms: f64,
    prepared_guide_ms: f64,
    snapshot_completed_local_cache_hit: bool,
    prepared_guide_cache_hit: bool,
    site_build_cache_hit: bool,
    publisher_recipe_assets_cache_hit: bool,
    template_materialize_ms: f64,
    publisher_runtime_ms: f64,
    publisher_model_ms: f64,
    render_semantics_cache_hit: bool,
    render_model_ms: f64,
    output_catalog_ms: f64,
    publisher_artifacts_ms: f64,
    site_build_close_ms: f64,
    closure_verify_ms: f64,
    catalog_ms: f64,
}

impl PrepareMeasurements {
    fn event(
        &self,
        build_id: &str,
        compile_ms: f64,
        compilation: &crate::compilation::CompilationMeasurements,
    ) -> crate::BuildEvent {
        let metrics = BTreeMap::from([
            ("compileProjectMs".into(), compile_ms),
            (
                "semanticCompilationCacheHit".into(),
                f64::from(compilation.semantic_compilation_cache_hit),
            ),
            (
                "compilerPackageStoreCacheHit".into(),
                f64::from(compilation.package_store_cache_hit),
            ),
            (
                "compilerPackageStoreUsed".into(),
                f64::from(compilation.package_store_used),
            ),
            (
                "compilerPackageStoreKeyMs".into(),
                compilation.package_store_key_ms,
            ),
            (
                "compilerPackageStoreBuildMs".into(),
                compilation.package_store_build_ms,
            ),
            (
                "compilerPackageStoreRetainedGenerations".into(),
                compilation.retained_package_store_generations as f64,
            ),
            (
                "compilerPackageBodyCacheHits".into(),
                compilation.package_body_cache_hits as f64,
            ),
            (
                "compilerPackageBodyCacheMisses".into(),
                compilation.package_body_cache_misses as f64,
            ),
            (
                "compilerPackageBodyCacheInserts".into(),
                compilation.package_body_cache_inserts as f64,
            ),
            (
                "compilerPackageBodyCacheEvictions".into(),
                compilation.package_body_cache_evictions as f64,
            ),
            (
                "compilerPackageBodyActiveEntries".into(),
                compilation.active_package_body_entries as f64,
            ),
            (
                "compilerPackageBodyActiveApproxSourceBytes".into(),
                compilation.active_package_body_approximate_source_bytes as f64,
            ),
            (
                "compilerPackageCatalogRetainedGenerations".into(),
                compilation.retained_package_catalog_generations as f64,
            ),
            (
                "compilerPackageCatalogRetainedEntries".into(),
                compilation.retained_package_catalog_entries as f64,
            ),
            (
                "compilerPackageBodyRetainedLogicalEntries".into(),
                compilation.retained_package_body_logical_entries as f64,
            ),
            (
                "compilerPackageBodyRetainedLogicalApproxSourceBytes".into(),
                compilation.retained_package_body_logical_approximate_source_bytes as f64,
            ),
            (
                "compilerPackageBodyRetainedUniqueEntries".into(),
                compilation.retained_package_body_unique_entries as f64,
            ),
            (
                "compilerPackageBodyRetainedUniqueApproxSourceBytes".into(),
                compilation.retained_package_body_unique_approximate_source_bytes as f64,
            ),
            ("rustPrepareTotalMs".into(), self.total_ms),
            ("projectRevisionMs".into(), self.project_revision_ms),
            ("packageLockMs".into(), self.package_lock_ms),
            ("preparedGuideKeyMs".into(), self.prepared_guide_key_ms),
            ("preparedGuideMs".into(), self.prepared_guide_ms),
            (
                "snapshotCompletedLocalCacheHit".into(),
                f64::from(self.snapshot_completed_local_cache_hit),
            ),
            (
                "preparedGuideCacheHit".into(),
                f64::from(self.prepared_guide_cache_hit),
            ),
            (
                "siteBuildCacheHit".into(),
                f64::from(self.site_build_cache_hit),
            ),
            (
                "publisherRecipeAssetsCacheHit".into(),
                f64::from(self.publisher_recipe_assets_cache_hit),
            ),
            ("templateMaterializeMs".into(), self.template_materialize_ms),
            ("publisherRuntimeMs".into(), self.publisher_runtime_ms),
            ("publisherModelMs".into(), self.publisher_model_ms),
            (
                "renderSemanticsCacheHit".into(),
                f64::from(self.render_semantics_cache_hit),
            ),
            ("renderModelMs".into(), self.render_model_ms),
            ("outputCatalogMs".into(), self.output_catalog_ms),
            ("publisherArtifactsMs".into(), self.publisher_artifacts_ms),
            ("siteBuildCloseMs".into(), self.site_build_close_ms),
            ("closureVerifyMs".into(), self.closure_verify_ms),
            ("catalogMs".into(), self.catalog_ms),
        ]);
        crate::BuildEvent {
            operation: Some(crate::BuildOperation::Prepare),
            build_id: Some(build_id.into()),
            phase: Some("site.prepare".into()),
            source: Some(crate::BuildEventSource::Rust),
            start_ms: None,
            stage: crate::BuildStage::SiteBuild,
            label: Some(build_id.into()),
            bytes: None,
            total_bytes: None,
            message: format!("Prepared SiteBuild {build_id}."),
            fraction: None,
            from_cache: Some(self.site_build_cache_hit),
            duration_ms: Some(compile_ms + self.total_ms),
            input_bytes: None,
            output_bytes: None,
            file_count: None,
            metrics: Some(metrics),
        }
    }
}

#[derive(Default)]
pub(crate) struct PreparationState {
    prepared_guide: Option<PreparedGuideCacheEntry>,
    snapshot_completed_local: Option<SnapshotCompletedLocalCacheEntry>,
    closed_cycle: Option<ClosedSiteBuildCacheEntry>,
    publisher_render_semantics: Option<PublisherRenderSemanticsCacheEntry>,
    #[cfg(test)]
    cache_hits: DerivedCacheHits,
}

struct PublisherRenderSemanticsCacheEntry {
    key: site_build::Sha256Digest,
    semantics: Rc<RenderSemantics>,
}

#[derive(Clone)]
struct PreparedGuideCacheEntry {
    key: site_build::Sha256Digest,
    guide: Rc<site_build::PreparedGuide>,
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublisherRenderSemanticsCachePayload<'a> {
    schema: &'static str,
    snapshot_completed_local_key: &'a site_build::Sha256Digest,
    active_tables: bool,
    run_uuid: Option<&'a str>,
    txcache: BTreeMap<String, site_build::Sha256Digest>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublisherRecipeAssetsCachePayload<'a> {
    schema: &'static str,
    recipe: &'static str,
    engine_api: u32,
    template: &'a site_build::PackageCoordinate,
    template_chain: Vec<PublisherRecipePackage<'a>>,
    core: PublisherRecipePackage<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublisherRecipePackage<'a> {
    coordinate: &'a site_build::PackageCoordinate,
    content: &'a site_build::ContentRef,
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
    publisher_recipe_artifact_builds: u64,
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
    project: &'a site_build::ProjectIdentity,
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
    project: &'a site_build::ProjectIdentity,
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
    pub(crate) fn clear_preparation(&mut self) {
        self.preparation = PreparationState::default();
    }

    pub(crate) fn prepare(
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

    /// Capture/compile one immutable project revision and prepare its target in
    /// one canonical executor call. Native and WASM transports should prefer
    /// this boundary over composing a host-side compile-then-prepare flow.
    pub fn prepare_project(
        &mut self,
        source: &mut impl ProjectSource,
        packages: &mut impl PackageProvider,
        spec: GeneratorSpec,
    ) -> Result<PreparedProjectResult, crate::BuildError<crate::CompilationOutcome>> {
        let cancelled = |source: &dyn ProjectSource| {
            if source.cancelled() {
                Err(crate::BuildError::new(
                    crate::BuildOperation::Prepare,
                    crate::BuildErrorPhase::Lifecycle,
                    crate::BuildErrorCode::Cancelled,
                    "prepare was cancelled before the next immutable phase",
                ))
            } else {
                Ok(())
            }
        };
        cancelled(source)?;
        let config = source.config().map_err(|message| {
            crate::BuildError::new(
                crate::BuildOperation::Prepare,
                crate::BuildErrorPhase::Input,
                crate::BuildErrorCode::InvalidInput,
                message,
            )
        })?;
        cancelled(source)?;
        let resolved = packages.resolve(&config, &spec).map_err(|message| {
            crate::BuildError::new(
                crate::BuildOperation::Prepare,
                crate::BuildErrorPhase::PackageResolution,
                crate::BuildErrorCode::Unavailable,
                message,
            )
        })?;
        cancelled(source)?;
        let environment = packages.environment(&resolved).map_err(|message| {
            crate::BuildError::new(
                crate::BuildOperation::Prepare,
                crate::BuildErrorPhase::PackageTransport,
                crate::BuildErrorCode::Integrity,
                message,
            )
        })?;
        cancelled(source)?;
        let inputs = source.capture(&environment, &resolved).map_err(|message| {
            crate::BuildError::new(
                crate::BuildOperation::Prepare,
                crate::BuildErrorPhase::Input,
                crate::BuildErrorCode::InvalidInput,
                message,
            )
        })?;
        if inputs.config != config {
            return Err(crate::BuildError::new(
                crate::BuildOperation::Prepare,
                crate::BuildErrorPhase::Input,
                crate::BuildErrorCode::Integrity,
                "project config changed while its immutable revision was captured",
            ));
        }
        cancelled(source)?;
        self.prepare_values(inputs, resolved, spec, environment)
    }

    pub(crate) fn prepare_values(
        &mut self,
        inputs: crate::ProjectRevision,
        resolved: crate::ResolvedPackageClosure,
        spec: GeneratorSpec,
        environment: PackageEnvironment,
    ) -> Result<PreparedProjectResult, crate::BuildError<crate::CompilationOutcome>> {
        let packages = environment
            .scoped(&resolved.labels, "prepare(project)")
            .map_err(|message| {
                crate::BuildError::new(
                    crate::BuildOperation::Prepare,
                    crate::BuildErrorPhase::PackageResolution,
                    crate::BuildErrorCode::InvalidInput,
                    message,
                )
            })?;
        let compile_started = self.timer();
        let transition = self
            .compile_project(inputs, packages, resolved)
            .map_err(|message| {
                crate::BuildError::new(
                    crate::BuildOperation::Prepare,
                    crate::BuildErrorPhase::Compilation,
                    crate::BuildErrorCode::CompileFailed,
                    message,
                )
            })?;
        let crate::compilation::CompilationTransition {
            outcome: compilation,
            measurements: compilation_measurements,
        } = transition;
        let compile_ms = elapsed_ms(compile_started);
        let site = match self.prepare(spec, environment) {
            Ok(site) => site,
            Err(message) => {
                return Err(crate::BuildError::new(
                    crate::BuildOperation::Prepare,
                    crate::BuildErrorPhase::Preparation,
                    crate::BuildErrorCode::RendererFailed,
                    message,
                )
                .with_successful_compilation(compilation));
            }
        };
        let events =
            vec![site
                .measurements
                .event(&site.build_id, compile_ms, &compilation_measurements)];
        Ok(PreparedProjectResult {
            compilation,
            site,
            events,
        })
    }

    fn prepare_cycle(
        &mut self,
        options: &PrepareGuideOptions,
        environment: &PackageEnvironment,
    ) -> Result<PrepareResult, String> {
        let total_started = self.timer();
        let operation = "prepare(cycle)";
        let mut metrics = PrepareMeasurements::default();
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
                    build_id: handle,
                    generator: GeneratorKind::Cycle,
                    site_build: projection.site_build,
                    measurements: metrics,
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
            keys.snapshot_completed_local.clone(),
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
            build_id: handle,
            generator: GeneratorKind::Cycle,
            site_build: projection.site_build,
            measurements: metrics,
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
    let incoming = AuthenticatedObject::eager_authenticated(content.clone(), bytes)?;
    if let Some(existing) = objects.get(&content.sha256) {
        if existing.content().byte_length != content.byte_length {
            return Err(format!("conflicting length for digest {}", content.sha256));
        }
    } else {
        objects.insert(content.sha256.clone(), incoming);
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

fn merge_authenticated_objects(target: &mut ObjectMap, source: &ObjectMap) -> Result<(), String> {
    for (digest, object) in source {
        if let Some(existing) = target.get(digest) {
            if existing.content().byte_length != object.content().byte_length {
                return Err(format!("conflicting length for digest {digest}"));
            }
        } else {
            target.insert(digest.clone(), object.clone());
        }
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
                crate::OutputKind::Asset
            } else {
                crate::OutputKind::Auxiliary
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
            page_kind: None,
        })
        .collect::<Vec<_>>();
    for page in pages {
        let metadata = resource_pages.get(&page);
        let path = site_build::OutputPath::parse(page.clone())
            .map_err(|error| format!("{operation}: invalid page {page}: {error}"))?;
        catalog.push(OutputDescriptor {
            path,
            kind: crate::OutputKind::Page,
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
            page_kind: None,
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

struct PublisherModel {
    site_files: Rc<HashMap<PathBuf, Vec<u8>>>,
    resource_pages: BTreeMap<String, site_producer::ResourcePageMetadata>,
}

fn publisher_runtime_outputs(
    publisher: &site_producer::publisher_runtime::PublisherRuntime,
    objects: &mut ObjectMap,
) -> Result<BTreeMap<site_build::OutputPath, PreparedOutput>, String> {
    let mut ready = BTreeMap::new();
    for file in publisher.files() {
        let transformation = file
            .provenance
            .transformation
            .as_deref()
            .map(|value| format!("; transformation={value}"))
            .unwrap_or_default();
        insert_prepared_output(
            &mut ready,
            objects,
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
    Ok(ready)
}

fn publisher_model(
    prepared: &site_build::PreparedGuide,
    template_files: &BTreeMap<String, Vec<u8>>,
    ready: &mut BTreeMap<site_build::OutputPath, PreparedOutput>,
    objects: &mut ObjectMap,
    project_id: &str,
    operation: &str,
) -> Result<PublisherModel, String> {
    let config_json: Value = serde_json::from_slice(
        template_files
            .get("config.json")
            .ok_or_else(|| format!("{operation}: template artifacts contain no config.json"))?,
    )
    .map_err(|error| format!("{operation}: bad template config.json: {error}"))?;
    let layouts = template_files
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
            "{operation}: template artifacts contain no layouts"
        ));
    }
    let producer_inputs =
        site_producer::ProducerInputs::from_prepared(prepared, &config_json, layouts, "en/")
            .map_err(|error| format!("{operation}: {error:#}"))?;
    let produced = site_producer::produce(&producer_inputs)
        .map_err(|error| format!("{operation}: {error:#}"))?;
    let resource_pages = produced.resource_pages;
    let mut site_files = HashMap::new();
    for (relative, bytes) in template_files {
        let mounted = match relative.strip_prefix("includes/") {
            Some(name) => format!("_includes/{name}"),
            None => format!("template/{relative}"),
        };
        site_files.insert(PathBuf::from(format!("/site/{mounted}")), bytes.clone());
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
    stage_prepared_authored_files(prepared, &mut site_files, ready, objects, project_id)?;
    Ok(PublisherModel {
        site_files: Rc::new(site_files),
        resource_pages,
    })
}

fn publisher_render_semantics(
    prepared: &site_build::PreparedGuide,
    packages: PackageView,
    model: &PublisherModel,
    options: &SiteOptions,
) -> Result<RenderSemantics, String> {
    build_render_semantics(
        prepared_render_set(prepared)?,
        Some(packages),
        &model.site_files,
        options,
    )
}

fn publisher_render_state(
    semantics: &RenderSemantics,
    model: &PublisherModel,
    options: &SiteOptions,
) -> Result<Rc<RenderState>, String> {
    Ok(Rc::new(build_render_state_from_semantics(
        semantics,
        model.site_files.clone(),
        options,
    )?))
}

fn publisher_render_semantics_cache_key(
    snapshot_completed_local_key: &site_build::Sha256Digest,
    model: &PublisherModel,
    options: &SiteOptions,
    operation: &str,
) -> Result<site_build::Sha256Digest, String> {
    let txcache_root = PathBuf::from("/site/txcache");
    let txcache = model
        .site_files
        .iter()
        .filter(|(path, _)| path.starts_with(&txcache_root))
        .map(|(path, bytes)| {
            (
                path.to_string_lossy().into_owned(),
                site_build::Sha256Digest::of_bytes(bytes),
            )
        })
        .collect();
    site_build::sha256_canonical(&PublisherRenderSemanticsCachePayload {
        schema: PUBLISHER_RENDER_SEMANTICS_CACHE_SCHEMA,
        snapshot_completed_local_key,
        active_tables: options.active_tables,
        run_uuid: options.run_uuid.as_deref(),
        txcache,
    })
    .map_err(|error| format!("{operation}: hash Publisher render semantics key: {error}"))
}

fn publisher_catalog(
    state: &RenderState,
    ready: &mut BTreeMap<site_build::OutputPath, PreparedOutput>,
    model: &PublisherModel,
    operation: &str,
) -> Result<(Vec<OutputDescriptor>, site_build::Sha256Digest), String> {
    let pages = state.list_pages();
    add_page_relative_output_aliases(ready, &pages)?;
    let tree_manifest = model
        .site_files
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
    let catalog = output_catalog(ready, pages, &model.resource_pages, operation)?;
    Ok((catalog, tree_digest))
}

fn publisher_artifacts(
    prepared: &site_build::PreparedGuide,
    project: &site_build::ProjectIdentity,
    package_lock: &site_build::PackageLock,
    recipe_assets: &PublisherRecipeAssets,
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
    let mut records = recipe_assets.artifact_records.as_ref().clone();
    let mut roots = recipe_assets.artifact_roots.as_ref().clone();
    // Recipe-asset objects were already admitted into the current build's
    // authenticated object map. Only return objects created for this exact
    // successor so callers never hash the immutable template/runtime bytes
    // again.
    let mut objects = ObjectMap::new();
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
    let catalog = site_build::ArtifactCatalog::from_records(records)
        .map_err(|error| format!("prepare(publisher): artifact catalog: {error}"))?;
    Ok((catalog, site_build::RenderPlan::new(roots), objects))
}

fn publisher_recipe_artifacts(
    template_chain: &[String],
    core: &site_build::PackageCoordinate,
    template_files: &BTreeMap<String, Vec<u8>>,
    runtime: &site_producer::publisher_runtime::PublisherRuntime,
) -> Result<
    (
        Vec<site_build::ArtifactRecord>,
        BTreeSet<site_build::ArtifactKey>,
        ObjectMap,
    ),
    String,
> {
    let mut records = Vec::new();
    let mut roots = BTreeSet::new();
    let mut objects = ObjectMap::new();
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
    Ok((records, roots, objects))
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

fn authored_role_from_name(name: &str) -> Result<site_build::AuthoredFileRole, String> {
    match name {
        "page-content" => Ok(site_build::AuthoredFileRole::PageContent),
        "resource-content" => Ok(site_build::AuthoredFileRole::ResourceContent),
        "data" => Ok(site_build::AuthoredFileRole::Data),
        "include" => Ok(site_build::AuthoredFileRole::Include),
        "image" => Ok(site_build::AuthoredFileRole::Image),
        "image-source" => Ok(site_build::AuthoredFileRole::ImageSource),
        other => Err(format!("unknown closed Publisher authored role {other:?}")),
    }
}

fn ready_artifact_content<'a>(
    build: &'a site_build::ClosedSiteBuild,
    key: &site_build::ArtifactKey,
    operation: &str,
) -> Result<(&'a site_build::ArtifactRecord, &'a site_build::ContentRef), String> {
    let record = build
        .site_build()
        .artifacts()
        .get(key)
        .ok_or_else(|| format!("{operation}: required artifact {key:?} is absent"))?;
    let site_build::ArtifactState::Ready { content } = &record.state else {
        return Err(format!(
            "{operation}: required artifact {key:?} is not ready"
        ));
    };
    Ok((record, content))
}

fn read_store_content(
    store: &dyn content_store::ContentStore,
    content: &site_build::ContentRef,
    operation: &str,
) -> Result<Vec<u8>, String> {
    store
        .read(content)
        .map(content_store::VerifiedContent::into_bytes)
        .map_err(|error| format!("{operation}: read content {}: {error}", content.sha256))
}

fn authenticated_object_bytes(
    objects: &ObjectMap,
    content: &site_build::ContentRef,
    operation: &str,
) -> Result<Rc<Vec<u8>>, String> {
    let object = objects.get(&content.sha256).ok_or_else(|| {
        format!(
            "{operation}: authenticated object {} is absent",
            content.sha256
        )
    })?;
    if object.content().byte_length != content.byte_length {
        return Err(format!(
            "{operation}: authenticated object {} has the wrong length",
            content.sha256
        ));
    }
    object.materialize()
}

fn read_json_artifact<T: serde::de::DeserializeOwned>(
    build: &site_build::ClosedSiteBuild,
    objects: &ObjectMap,
    key: &site_build::ArtifactKey,
    expected_recipe: &str,
    expected_schema: &str,
    operation: &str,
) -> Result<T, String> {
    let (record, content) = ready_artifact_content(build, key, operation)?;
    if record.provenance.producer.id != "site-semantics"
        || record.provenance.recipe != expected_recipe
        || record
            .provenance
            .attributes
            .get("schema")
            .map(String::as_str)
            != Some(expected_schema)
        || content.media_type.as_deref() != Some("application/json")
    {
        return Err(format!(
            "{operation}: semantic artifact {key:?} has incompatible provenance"
        ));
    }
    let bytes = authenticated_object_bytes(objects, content, operation)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("{operation}: decode semantic artifact {key:?}: {error}"))
}

fn prepared_from_closed_publisher(
    build: &site_build::ClosedSiteBuild,
    objects: &ObjectMap,
    operation: &str,
) -> Result<site_build::PreparedGuide, String> {
    let resources: site_build::cycle_semantic::ResourcesDocument = read_json_artifact(
        build,
        objects,
        &site_build::cycle_semantic::resources_key(),
        "site_semantics.resources",
        site_build::cycle_semantic::RESOURCES_SCHEMA,
        operation,
    )?;
    let terminology: site_build::cycle_semantic::TerminologyDocument = read_json_artifact(
        build,
        objects,
        &site_build::cycle_semantic::terminology_key(),
        "site_semantics.terminology",
        site_build::cycle_semantic::TERMINOLOGY_SCHEMA,
        operation,
    )?;
    let navigation: site_build::cycle_semantic::NavigationDocument = read_json_artifact(
        build,
        objects,
        &site_build::cycle_semantic::navigation_key(),
        "site_semantics.navigation",
        site_build::cycle_semantic::NAVIGATION_SCHEMA,
        operation,
    )?;
    let config: site_build::cycle_semantic::ConfigDocument = read_json_artifact(
        build,
        objects,
        &site_build::cycle_semantic::config_key(),
        "site_semantics.config",
        site_build::cycle_semantic::CONFIG_SCHEMA,
        operation,
    )?;
    if resources.schema != site_build::cycle_semantic::RESOURCES_SCHEMA
        || terminology.schema != site_build::cycle_semantic::TERMINOLOGY_SCHEMA
        || navigation.schema != site_build::cycle_semantic::NAVIGATION_SCHEMA
        || config.schema != site_build::cycle_semantic::CONFIG_SCHEMA
    {
        return Err(format!("{operation}: semantic document schema mismatch"));
    }

    let mut authored_files = Vec::new();
    for (key, record) in build.site_build().artifacts().iter() {
        let site_build::ArtifactKey::Asset { namespace, path } = key else {
            continue;
        };
        let role = match namespace {
            site_build::AssetNamespace::Authored => site_build::AuthoredFileRole::Image,
            site_build::AssetNamespace::Other { name } => {
                let Some(role) = name
                    .strip_prefix(&format!("{PUBLISHER_AUTHORED_NAMESPACE_PREFIX}."))
                    .and_then(|name| name.strip_suffix("/v1"))
                else {
                    continue;
                };
                authored_role_from_name(role)?
            }
            _ => continue,
        };
        if record.provenance.producer.id != "prepared-guide"
            || record.provenance.recipe != "publisher.authored-file/v1"
            || record.provenance.attributes.get("role").map(String::as_str)
                != Some(authored_role_name(role.clone()))
        {
            return Err(format!(
                "{operation}: authored artifact {key:?} has incompatible provenance"
            ));
        }
        let site_build::ArtifactState::Ready { content } = &record.state else {
            return Err(format!(
                "{operation}: authored artifact {key:?} is not ready"
            ));
        };
        let mime = content
            .media_type
            .clone()
            .ok_or_else(|| format!("{operation}: authored artifact {key:?} has no media type"))?;
        let source_reads = record
            .reads
            .iter()
            .map(|read| match read {
                site_build::ReadDependency::Source { path } => {
                    site_build::PreparedPath::parse(path.as_str().to_string()).map_err(|error| {
                        format!("{operation}: authored artifact source {path}: {error}")
                    })
                }
                other => Err(format!(
                    "{operation}: authored artifact {key:?} has non-source read {other:?}"
                )),
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        authored_files.push(site_build::AuthoredFile {
            role,
            path: site_build::PreparedPath::parse(path.as_str().to_string())
                .map_err(|error| format!("{operation}: authored artifact path {path}: {error}"))?,
            mime,
            content: authenticated_object_bytes(objects, content, operation)?.to_vec(),
            source_reads,
        });
    }
    authored_files.sort_by(|left, right| {
        left.role
            .cmp(&right.role)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(site_build::PreparedGuide {
        guide: resources.guide,
        resources: resources.resources,
        publisher_compatibility: resources.publisher_compatibility,
        expansions: terminology.expansions,
        pages: navigation.pages,
        menu: navigation.menu,
        sushi_config: config.sushi_config,
        authored_files,
    })
}

fn closed_artifact_files(
    build: &site_build::ClosedSiteBuild,
    objects: &ObjectMap,
    namespace: &site_build::AssetNamespace,
    expected_producer: &str,
    expected_recipe: &str,
    operation: &str,
) -> Result<BTreeMap<String, (site_build::ArtifactProvenance, String, Vec<u8>)>, String> {
    let mut files = BTreeMap::new();
    for (key, record) in build.site_build().artifacts().iter() {
        let site_build::ArtifactKey::Asset {
            namespace: actual,
            path,
        } = key
        else {
            continue;
        };
        if actual != namespace {
            continue;
        }
        if record.provenance.producer.id != expected_producer
            || record.provenance.recipe != expected_recipe
        {
            return Err(format!(
                "{operation}: artifact {key:?} has incompatible provenance"
            ));
        }
        let site_build::ArtifactState::Ready { content } = &record.state else {
            return Err(format!("{operation}: artifact {key:?} is not ready"));
        };
        if namespace == &site_build::AssetNamespace::PublisherRuntime
            && content.media_type.is_none()
        {
            return Err(format!(
                "{operation}: Publisher runtime artifact {key:?} has no media type"
            ));
        }
        let media_type = content
            .media_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".into());
        let value = (
            record.provenance.clone(),
            media_type,
            authenticated_object_bytes(objects, content, operation)?.to_vec(),
        );
        if files.insert(path.as_str().to_string(), value).is_some() {
            return Err(format!("{operation}: duplicate artifact file {path}"));
        }
    }
    if files.is_empty() {
        return Err(format!(
            "{operation}: closed build has no {expected_producer} artifacts"
        ));
    }
    Ok(files)
}

fn package_view_from_closed_build(
    build: &site_build::ClosedSiteBuild,
    objects: &ObjectMap,
    labels: &[String],
    operation: &str,
) -> Result<PackageView, String> {
    let expected = labels.iter().cloned().collect::<BTreeSet<_>>();
    if expected.len() != labels.len() {
        return Err(format!(
            "{operation}: compile package order repeats a label"
        ));
    }
    let mut source = package_store::BundleSource::new();
    for label in labels {
        let coordinate = site_build::PackageCoordinate::parse(label)
            .map_err(|error| format!("{operation}: invalid compile package {label}: {error}"))?;
        let locked = build
            .site_build()
            .package_lock()
            .get(&coordinate)
            .ok_or_else(|| format!("{operation}: compile package {label} is absent from lock"))?;
        if locked.content.media_type.as_deref() != Some(package_store::PREPARED_PACKAGE_MEDIA_TYPE)
        {
            return Err(format!(
                "{operation}: package {label} has unsupported material type {:?}",
                locked.content.media_type
            ));
        }
        let bytes = authenticated_object_bytes(objects, &locked.content, operation)?;
        let package = package_store::PreparedPackage::decode(bytes.as_ref())
            .map_err(|error| format!("{operation}: decode package {label}: {error:#}"))?;
        if package.label != *label {
            return Err(format!(
                "{operation}: package lock {label} selected carrier for {}",
                package.label
            ));
        }
        package.mount_into(&mut source);
    }
    let root = source.cache_root().to_path_buf();
    let packages = PackageView::new(Rc::new(source), root, Some(expected));
    publisher_render_package_view(packages, labels, operation)
}

fn publisher_render_package_view(
    packages: PackageView,
    labels: &[String],
    operation: &str,
) -> Result<PackageView, String> {
    let mut allowed_files = BTreeSet::new();
    for label in labels {
        let package_root = packages.root().join(label);
        let semantic_root = package_root.join("package");
        for entry in packages
            .read_dir(&semantic_root)
            .map_err(|error| format!("{operation}: list semantic package {label}: {error}"))?
        {
            if entry.is_file {
                allowed_files.insert(semantic_root.join(entry.file_name));
            }
        }
        let normalized_path = package_root.join("package/other/spec.internals");
        let native_path = package_root.join("other/spec.internals");
        if packages.exists(&normalized_path) {
            allowed_files.insert(normalized_path.clone());
        } else if packages.exists(&native_path) {
            allowed_files.insert(native_path.clone());
        }
    }
    // Both live and restored execution receive the same exact projection of
    // the rooted carrier: top-level semantic members plus spec.internals.
    packages.restricted_to_files(allowed_files)
}

fn verify_closed_package_materials(
    build: &site_build::ClosedSiteBuild,
    objects: &ObjectMap,
    operation: &str,
) -> Result<(), String> {
    for (coordinate, locked) in build.site_build().package_lock().iter() {
        if locked.content.media_type.as_deref() != Some(package_store::PREPARED_PACKAGE_MEDIA_TYPE)
        {
            return Err(format!(
                "{operation}: package {coordinate} has unsupported material type {:?}",
                locked.content.media_type
            ));
        }
        let bytes = authenticated_object_bytes(objects, &locked.content, operation)?;
        let package = package_store::PreparedPackage::decode(bytes.as_ref())
            .map_err(|error| format!("{operation}: decode package {coordinate}: {error:#}"))?;
        if package.label != coordinate.as_str() {
            return Err(format!(
                "{operation}: package lock {coordinate} selected carrier for {}",
                package.label
            ));
        }
    }
    Ok(())
}

fn validate_closed_publisher_artifact_inventory(
    build: &site_build::ClosedSiteBuild,
    package_order: &[String],
    operation: &str,
) -> Result<(), String> {
    let all_keys = build
        .site_build()
        .artifacts()
        .iter()
        .map(|(key, _)| key.clone())
        .collect::<BTreeSet<_>>();
    if build.site_build().render_plan().required_artifacts() != &all_keys {
        return Err(format!(
            "{operation}: Publisher render plan does not root its exact artifact inventory"
        ));
    }
    let semantic = BTreeSet::from([
        site_build::cycle_semantic::resources_key(),
        site_build::cycle_semantic::terminology_key(),
        site_build::cycle_semantic::navigation_key(),
        site_build::cycle_semantic::config_key(),
    ]);
    for key in &all_keys {
        if semantic.contains(key) {
            continue;
        }
        match key {
            site_build::ArtifactKey::Asset {
                namespace: site_build::AssetNamespace::Authored,
                ..
            }
            | site_build::ArtifactKey::Asset {
                namespace: site_build::AssetNamespace::Template,
                ..
            }
            | site_build::ArtifactKey::Asset {
                namespace: site_build::AssetNamespace::PublisherRuntime,
                ..
            } => {}
            site_build::ArtifactKey::Asset {
                namespace: site_build::AssetNamespace::Other { name },
                ..
            } => {
                let role = name
                    .strip_prefix(&format!("{PUBLISHER_AUTHORED_NAMESPACE_PREFIX}."))
                    .and_then(|name| name.strip_suffix("/v1"))
                    .ok_or_else(|| {
                        format!("{operation}: unknown Publisher artifact namespace {name:?}")
                    })?;
                authored_role_from_name(role)?;
            }
            other => {
                return Err(format!(
                    "{operation}: unsupported rooted Publisher artifact {other:?}"
                ));
            }
        }
    }
    let expected = package_order.iter().collect::<BTreeSet<_>>();
    if expected.len() != package_order.len() {
        return Err(format!(
            "{operation}: compilePackageOrder repeats a package"
        ));
    }
    Ok(())
}

fn cycle_semantic_artifact<T: serde::de::DeserializeOwned>(
    build: &site_build::ClosedSiteBuild,
    objects: &ObjectMap,
    key: &site_build::ArtifactKey,
    producer: &str,
    recipe: &str,
    reads: BTreeSet<site_build::ReadDependency>,
    operation: &str,
) -> Result<T, String> {
    let (record, content) = ready_artifact_content(build, key, operation)?;
    if record.provenance.producer
        != site_build::ProducerRef::new(producer, env!("CARGO_PKG_VERSION"))
        || record.provenance.recipe != recipe
        || !record.provenance.attributes.is_empty()
        || record.reads != reads
        || content.media_type.as_deref() != Some("application/json")
    {
        return Err(format!(
            "{operation}: Cycle semantic artifact {key:?} has incompatible provenance or reads"
        ));
    }
    serde_json::from_slice(&authenticated_object_bytes(objects, content, operation)?)
        .map_err(|error| format!("{operation}: decode Cycle semantic artifact {key:?}: {error}"))
}

fn validate_cycle_page_sources(
    pages: &[site_build::PageNode],
    project: &site_build::ProjectIdentity,
    operation: &str,
) -> Result<(), String> {
    for page in pages {
        match (&page.body, &page.source) {
            (Some(_), Some(source)) => {
                let source = site_build::SourcePath::parse(source.as_str().to_string())
                    .map_err(|error| format!("{operation}: page source {source}: {error}"))?;
                if project.sources.get(&source).is_none() {
                    return Err(format!(
                        "{operation}: page {} reads absent source {source}",
                        page.name_url
                    ));
                }
            }
            (None, None) => {}
            _ => {
                return Err(format!(
                    "{operation}: page {} must have both an authored body and source, or neither",
                    page.name_url
                ));
            }
        }
        validate_cycle_page_sources(&page.children, project, operation)?;
    }
    Ok(())
}

fn validate_closed_cycle(
    build: &site_build::ClosedSiteBuild,
    objects: &ObjectMap,
    operation: &str,
) -> Result<(), String> {
    let site = build.site_build();
    let target = site.render_target();
    if target.mode != site_build::RenderMode::ExternalBuilder
        || target.renderer != site_build::ProducerRef::new("cycle-site", "2")
        || target.template.is_some()
        || target.parameters.get("contract").map(String::as_str)
            != Some(site_build::cycle_semantic::TARGET)
    {
        return Err(format!(
            "{operation}: build targets an incompatible Cycle executor"
        ));
    }
    let required_parameters = ["contract", "buildEpochSecs", "liquidAssetDirs"];
    let allowed_parameters = [
        "contract",
        "buildEpochSecs",
        "liquidAssetDirs",
        "branch",
        "revision",
    ];
    if required_parameters
        .iter()
        .any(|key| !target.parameters.contains_key(*key))
        || target
            .parameters
            .keys()
            .any(|key| !allowed_parameters.contains(&key.as_str()))
    {
        return Err(format!(
            "{operation}: Cycle target parameters do not match cycle-site/v2"
        ));
    }
    let build_epoch = target
        .parameters
        .get("buildEpochSecs")
        .expect("required Cycle parameter")
        .parse::<i64>()
        .map_err(|error| format!("{operation}: invalid buildEpochSecs: {error}"))?;
    const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
    if !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&build_epoch) {
        return Err(format!(
            "{operation}: buildEpochSecs is outside the JavaScript safe-integer range"
        ));
    }

    let all_keys = site
        .artifacts()
        .iter()
        .map(|(key, _)| key.clone())
        .collect::<BTreeSet<_>>();
    if site.render_plan().required_artifacts() != &all_keys {
        return Err(format!(
            "{operation}: Cycle render plan does not root its exact artifact inventory"
        ));
    }

    let resources_key = site_build::cycle_semantic::resources_key();
    let terminology_key = site_build::cycle_semantic::terminology_key();
    let navigation_key = site_build::cycle_semantic::navigation_key();
    let config_key = site_build::cycle_semantic::config_key();
    let semantic_keys = BTreeSet::from([
        resources_key.clone(),
        terminology_key.clone(),
        navigation_key.clone(),
        config_key.clone(),
    ]);
    if !semantic_keys.is_subset(&all_keys) {
        return Err(format!(
            "{operation}: Cycle build is missing a required semantic artifact"
        ));
    }

    let all_inputs = site
        .project()
        .sources
        .iter()
        .map(|(path, _)| site_build::ReadDependency::Source { path: path.clone() })
        .chain(site.package_lock().iter().map(|(coordinate, _)| {
            site_build::ReadDependency::Package {
                coordinate: coordinate.clone(),
            }
        }))
        .collect::<BTreeSet<_>>();
    let resources: site_build::cycle_semantic::ResourcesDocument = cycle_semantic_artifact(
        build,
        objects,
        &resources_key,
        "site_semantics.resources",
        site_build::cycle_semantic::RESOURCES_SCHEMA,
        all_inputs,
        operation,
    )?;
    let terminology_reads = site
        .package_lock()
        .iter()
        .map(|(coordinate, _)| site_build::ReadDependency::Package {
            coordinate: coordinate.clone(),
        })
        .chain([site_build::ReadDependency::Artifact {
            key: resources_key.clone(),
        }])
        .collect();
    let terminology: site_build::cycle_semantic::TerminologyDocument = cycle_semantic_artifact(
        build,
        objects,
        &terminology_key,
        "site_semantics.terminology",
        site_build::cycle_semantic::TERMINOLOGY_SCHEMA,
        terminology_reads,
        operation,
    )?;
    let navigation_reads = site
        .project()
        .sources
        .iter()
        .filter(|(_, entry)| {
            matches!(
                entry.kind,
                site_build::SourceKind::Config | site_build::SourceKind::Page
            )
        })
        .map(|(path, _)| site_build::ReadDependency::Source { path: path.clone() })
        .chain([site_build::ReadDependency::Artifact {
            key: resources_key.clone(),
        }])
        .collect();
    let navigation: site_build::cycle_semantic::NavigationDocument = cycle_semantic_artifact(
        build,
        objects,
        &navigation_key,
        "site_semantics.navigation",
        site_build::cycle_semantic::NAVIGATION_SCHEMA,
        navigation_reads,
        operation,
    )?;
    let config_reads = site
        .project()
        .sources
        .iter()
        .filter(|(_, entry)| matches!(entry.kind, site_build::SourceKind::Config))
        .map(|(path, _)| site_build::ReadDependency::Source { path: path.clone() })
        .collect();
    let config: site_build::cycle_semantic::ConfigDocument = cycle_semantic_artifact(
        build,
        objects,
        &config_key,
        "site_semantics.config",
        site_build::cycle_semantic::CONFIG_SCHEMA,
        config_reads,
        operation,
    )?;
    if resources.schema != site_build::cycle_semantic::RESOURCES_SCHEMA
        || terminology.schema != site_build::cycle_semantic::TERMINOLOGY_SCHEMA
        || navigation.schema != site_build::cycle_semantic::NAVIGATION_SCHEMA
        || config.schema != site_build::cycle_semantic::CONFIG_SCHEMA
        || resources.guide.package_id != site.project().project_id
        || resources.guide.fhir_version != target.fhir_version
        || resources.guide.generated.epoch_seconds != build_epoch
        || !config.sushi_config.is_object()
    {
        return Err(format!(
            "{operation}: Cycle semantic documents disagree with the target or project"
        ));
    }

    let mut resource_keys = BTreeSet::new();
    for resource in &resources.resources {
        if resource
            .resource
            .get("resourceType")
            .and_then(Value::as_str)
            != Some(resource.key.resource_type.as_str())
            || resource.resource.get("id").and_then(Value::as_str) != Some(resource.key.id.as_str())
            || !resource_keys.insert(resource.key.clone())
        {
            return Err(format!(
                "{operation}: Cycle semantic resource identity is invalid or duplicated"
            ));
        }
    }
    if resources.guide.implementation_guide.resource_type != "ImplementationGuide"
        || !resource_keys.contains(&resources.guide.implementation_guide)
        || terminology.expansions.iter().any(|expansion| {
            expansion.value_set.resource_type != "ValueSet"
                || !resource_keys.contains(&expansion.value_set)
        })
    {
        return Err(format!(
            "{operation}: Cycle guide or terminology references an absent resource"
        ));
    }
    validate_cycle_page_sources(&navigation.pages, site.project(), operation)?;

    for key in all_keys.difference(&semantic_keys) {
        let (expected_recipe, path) = match key {
            site_build::ArtifactKey::Asset {
                namespace: site_build::AssetNamespace::Authored,
                path,
            } => ("authored-image/v1", path),
            site_build::ArtifactKey::Asset {
                namespace: site_build::AssetNamespace::Other { name },
                path,
            } if name == site_build::cycle_semantic::AUTHORED_INCLUDE_NAMESPACE => {
                ("authored-include/v1", path)
            }
            other => {
                return Err(format!(
                    "{operation}: unsupported rooted Cycle artifact {other:?}"
                ));
            }
        };
        let (record, content) = ready_artifact_content(build, key, operation)?;
        if record.provenance.producer
            != site_build::ProducerRef::new("site_semantics.asset", env!("CARGO_PKG_VERSION"))
            || record.provenance.recipe != expected_recipe
            || !record.provenance.attributes.is_empty()
            || record
                .reads
                .iter()
                .any(|read| !matches!(read, site_build::ReadDependency::Source { .. }))
            || content.media_type.is_none()
            || record.reads.iter().any(|read| match read {
                site_build::ReadDependency::Source { path } => {
                    site.project().sources.get(path).is_none()
                }
                _ => true,
            })
        {
            return Err(format!(
                "{operation}: authored Cycle artifact {path} has incompatible provenance or reads"
            ));
        }
    }
    Ok(())
}

fn all_closed_objects(
    build: &site_build::ClosedSiteBuild,
    store: &dyn content_store::ContentStore,
    operation: &str,
) -> Result<ObjectMap, String> {
    let mut refs = Vec::new();
    refs.extend(
        build
            .site_build()
            .project()
            .sources
            .iter()
            .map(|(_, entry)| &entry.content),
    );
    refs.extend(
        build
            .site_build()
            .package_lock()
            .iter()
            .map(|(_, package)| &package.content),
    );
    refs.extend(build.site_build().artifacts().iter().filter_map(
        |(_, record)| match &record.state {
            site_build::ArtifactState::Ready { content } => Some(content),
            _ => None,
        },
    ));
    let mut objects = BTreeMap::new();
    for content in refs {
        let bytes = Rc::new(read_store_content(store, content, operation)?);
        insert_authenticated_object(&mut objects, content, bytes)?;
    }
    Ok(objects)
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
    let object = objects
        .get(&content.sha256)
        .ok_or_else(|| format!("object {} is absent", content.sha256))?;
    object.authenticates(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use content_store::ContentStore;

    #[test]
    fn prepare_event_reports_compiler_package_store_reuse_metrics() {
        let compilation = crate::compilation::CompilationMeasurements {
            semantic_compilation_cache_hit: false,
            package_store_cache_hit: true,
            package_store_used: true,
            package_store_key_ms: 1.25,
            package_store_build_ms: 0.0,
            retained_package_store_generations: 2,
            package_body_cache_hits: 7,
            package_body_cache_misses: 3,
            package_body_cache_inserts: 2,
            package_body_cache_evictions: 1,
            active_package_body_entries: 8,
            active_package_body_approximate_source_bytes: 4096,
            retained_package_catalog_generations: 1,
            retained_package_catalog_entries: 200,
            retained_package_body_logical_entries: 12,
            retained_package_body_logical_approximate_source_bytes: 6144,
            retained_package_body_unique_entries: 9,
            retained_package_body_unique_approximate_source_bytes: 4608,
        };
        let event = PrepareMeasurements::default().event("build", 12.0, &compilation);
        let metrics = event.metrics.expect("prepare metrics");
        assert_eq!(metrics["compilerPackageStoreCacheHit"], 1.0);
        assert_eq!(metrics["compilerPackageStoreUsed"], 1.0);
        assert_eq!(metrics["compilerPackageStoreKeyMs"], 1.25);
        assert_eq!(metrics["compilerPackageStoreBuildMs"], 0.0);
        assert_eq!(metrics["compilerPackageStoreRetainedGenerations"], 2.0);
        assert_eq!(metrics["compilerPackageBodyCacheHits"], 7.0);
        assert_eq!(metrics["compilerPackageBodyCacheMisses"], 3.0);
        assert_eq!(metrics["compilerPackageBodyCacheInserts"], 2.0);
        assert_eq!(metrics["compilerPackageBodyCacheEvictions"], 1.0);
        assert_eq!(metrics["compilerPackageBodyActiveEntries"], 8.0);
        assert_eq!(
            metrics["compilerPackageBodyActiveApproxSourceBytes"],
            4096.0
        );
        assert_eq!(metrics["compilerPackageCatalogRetainedGenerations"], 1.0);
        assert_eq!(metrics["compilerPackageCatalogRetainedEntries"], 200.0);
        assert_eq!(metrics["compilerPackageBodyRetainedLogicalEntries"], 12.0);
        assert_eq!(
            metrics["compilerPackageBodyRetainedLogicalApproxSourceBytes"],
            6144.0
        );
        assert_eq!(metrics["compilerPackageBodyRetainedUniqueEntries"], 9.0);
        assert_eq!(
            metrics["compilerPackageBodyRetainedUniqueApproxSourceBytes"],
            4608.0
        );
    }

    #[test]
    fn prepared_package_carrier_closes_without_inflating_members() {
        let label = "demo.package#1.0.0";
        let semantic_files = BTreeMap::from([
            (
                "package.json".into(),
                br#"{"name":"demo.package","version":"1.0.0"}"#.to_vec(),
            ),
            (
                "StructureDefinition-demo.json".into(),
                vec![b'x'; 1024 * 1024],
            ),
        ]);
        let package =
            package_store::PreparedPackage::prepare(label, semantic_files.clone()).unwrap();
        let key = package.key.clone();
        let encoded = package.encode();
        let package = package_store::PreparedPackage::decode_expected(&encoded, &key).unwrap();
        let mut source = package_store::BundleSource::new();
        let mounted = package.mount_into(&mut source);
        let source = Rc::new(source);
        let material = PackageMaterial::from_prepared(mounted).unwrap();
        assert_eq!(source.compression_metrics().chunks_inflated, 0);
        let bytes = material.object().materialize().unwrap();
        material.content().verify(bytes.as_ref()).unwrap();
        assert_eq!(bytes.as_ref(), &encoded);
        assert_eq!(source.compression_metrics().chunks_inflated, 0);
        for (path, expected) in semantic_files {
            assert_eq!(
                source
                    .read(&source.cache_root().join(label).join("package").join(path))
                    .unwrap(),
                expected
            );
        }
        let first = source.compression_metrics();
        assert!(first.chunks_inflated > 0);
        assert_eq!(material.object().materialize().unwrap(), bytes);
        assert_eq!(source.compression_metrics(), first);
    }

    #[test]
    fn package_environment_derives_view_and_lock_material_from_one_carrier() {
        let label = "demo.package#1.0.0";
        let package = |body: &[u8]| {
            package_store::PreparedPackage::prepare(
                label,
                BTreeMap::from([
                    (
                        "package.json".into(),
                        br#"{"name":"demo.package","version":"1.0.0"}"#.to_vec(),
                    ),
                    ("StructureDefinition-demo.json".into(), body.to_vec()),
                ]),
            )
            .unwrap()
        };
        let a = package(b"carrier-a");
        let a_digest = a.artifact_sha256().to_string();
        let environment = PackageEnvironment::new([a.clone()]).unwrap();
        assert_eq!(
            environment
                .packages
                .read(
                    &environment
                        .packages
                        .root()
                        .join(label)
                        .join("package/StructureDefinition-demo.json"),
                )
                .unwrap(),
            b"carrier-a"
        );
        assert_eq!(
            environment.materials[label].content().sha256.as_str(),
            a_digest
        );
        assert!(PackageEnvironment::new([a, package(b"carrier-b")])
            .err()
            .unwrap()
            .contains("repeats mounted label"));
    }

    #[test]
    fn live_publisher_package_view_matches_closed_restoration_authority() {
        let label = "demo.package#1.0.0";
        let package = package_store::PreparedPackage::prepare(
            label,
            BTreeMap::from([
                (
                    "package.json".into(),
                    br#"{"name":"demo.package","version":"1.0.0"}"#.to_vec(),
                ),
                ("StructureDefinition-demo.json".into(), b"semantic".to_vec()),
                ("other/spec.internals".into(), b"spec".to_vec()),
                ("template/private.txt".into(), b"must-not-leak".to_vec()),
            ]),
        )
        .unwrap();
        let environment = PackageEnvironment::new([package]).unwrap();
        let root = environment.packages.root().to_path_buf();

        let view = publisher_render_package_view(
            environment.scoped(&[label.into()], "test").unwrap(),
            &[label.into()],
            "test",
        )
        .unwrap();
        assert_eq!(
            view.read(
                &root
                    .join(label)
                    .join("package/StructureDefinition-demo.json")
            )
            .unwrap(),
            b"semantic"
        );
        assert_eq!(
            view.read(&root.join(label).join("package/other/spec.internals"))
                .unwrap(),
            b"spec"
        );
        assert!(!view.exists(&root.join(label).join("package/template/private.txt")));
        assert!(view
            .read(&root.join(label).join("package/template/private.txt"))
            .is_err());
    }

    #[test]
    fn project_revision_uses_one_exact_raw_source_for_a_parsed_local_resource() {
        let config = "id: overlap.ig\nfhirVersion: 4.0.1\n";
        let path = "input/resources/Patient-example.json";
        let raw = br#"{ "resourceType": "Patient", "id": "example" }"#.to_vec();
        let project = crate::compilation::CompiledProjectRevision::new(
            crate::ProjectRevision {
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

    fn closed_cycle_fixture() -> (
        site_build::ClosedSiteBuild,
        BTreeMap<site_build::Sha256Digest, Vec<u8>>,
    ) {
        let prepared = prepared_with_all_roles();
        let mut object_bytes = BTreeMap::new();
        let sources =
            site_build::SourceManifest::from_entries(prepared.authored_files.iter().map(|file| {
                let path = site_build::SourcePath::parse(
                    file.source_reads
                        .iter()
                        .next()
                        .expect("fixture authored source")
                        .as_str()
                        .to_string(),
                )
                .unwrap();
                let content =
                    site_build::ContentRef::of_bytes(&file.content, Some(file.mime.clone()));
                object_bytes.insert(content.sha256.clone(), file.content.clone());
                (
                    path,
                    site_build::SourceEntry {
                        kind: site_build::SourceKind::Other {
                            name: "fixture".into(),
                        },
                        content,
                    },
                )
            }))
            .unwrap();
        let projection = site_build::cycle_semantic::close_prepared(
            &prepared,
            site_build::cycle_semantic::CycleProjectionInput {
                project: site_build::ProjectIdentity {
                    project_id: prepared.guide.package_id.clone(),
                    revision: "cycle-restore-fixture".into(),
                    sources,
                },
                package_lock: site_build::PackageLock::default(),
                render_target: site_build::RenderTarget {
                    renderer: site_build::ProducerRef::new("cycle-site", "2"),
                    mode: site_build::RenderMode::ExternalBuilder,
                    fhir_version: prepared.guide.fhir_version.clone(),
                    template: None,
                    parameters: BTreeMap::from([
                        ("contract".into(), site_build::cycle_semantic::TARGET.into()),
                        ("buildEpochSecs".into(), "1".into()),
                        ("liquidAssetDirs".into(), "input/includes".into()),
                    ]),
                },
                diagnostics: BTreeSet::new(),
            },
        )
        .unwrap();
        object_bytes.extend(projection.objects);
        (projection.site_build, object_bytes)
    }

    fn populate_store(
        store: &content_store::FileContentStore,
        build: &site_build::ClosedSiteBuild,
        objects: &BTreeMap<site_build::Sha256Digest, Vec<u8>>,
        omitted: Option<&site_build::Sha256Digest>,
    ) {
        let refs =
            build
                .site_build()
                .project()
                .sources
                .iter()
                .map(|(_, source)| &source.content)
                .chain(
                    build
                        .site_build()
                        .package_lock()
                        .iter()
                        .map(|(_, package)| &package.content),
                )
                .chain(
                    build.site_build().artifacts().iter().filter_map(
                        |(_, artifact)| match &artifact.state {
                            site_build::ArtifactState::Ready { content } => Some(content),
                            _ => None,
                        },
                    ),
                );
        for content in refs {
            if omitted == Some(&content.sha256) {
                continue;
            }
            store
                .put(
                    content,
                    objects
                        .get(&content.sha256)
                        .expect("fixture has every addressed object"),
                )
                .unwrap();
        }
    }

    #[test]
    fn generic_restore_admits_a_fresh_closed_cycle_runtime() {
        let (closed, objects) = closed_cycle_fixture();
        let serialized = closed.site_build().canonical_bytes().unwrap();
        let closed: site_build::ClosedSiteBuild = serde_json::from_slice(&serialized).unwrap();
        let expected_handle = closed.site_build().build_id().to_string();
        let temp = tempfile::tempdir().unwrap();
        let store = content_store::FileContentStore::create(temp.path()).unwrap();
        populate_store(&store, &closed, &objects, None);

        let resources = closed
            .site_build()
            .artifacts()
            .get(&site_build::cycle_semantic::resources_key())
            .unwrap();
        let site_build::ArtifactState::Ready { content } = &resources.state else {
            panic!("fixture resources are ready")
        };
        let resources_digest = content.sha256.clone();
        let mut engine = SiteEngine::default();
        let handle = engine.restore(closed, &store).unwrap();
        assert_eq!(handle, expected_handle);
        assert_eq!(
            engine
                .read_content(&handle, resources_digest.as_str())
                .unwrap(),
            objects[&resources_digest]
        );
        let outputs_error = engine.outputs(&handle).unwrap_err();
        assert_eq!(outputs_error.operation, crate::BuildOperation::Outputs);
        assert_eq!(outputs_error.code, crate::BuildErrorCode::RendererFailed);
        assert!(outputs_error.message.contains("external LiquidJS"));
        let finalize_error = engine.finalize(&handle).unwrap_err();
        assert_eq!(finalize_error.operation, crate::BuildOperation::Finalize);
        assert_eq!(finalize_error.code, crate::BuildErrorCode::RendererFailed);
        assert_eq!(
            finalize_error.message,
            "finalize: Cycle renderer is not open"
        );
    }

    #[test]
    fn cycle_finalize_requires_rust_authenticated_content_refs() {
        let (closed, objects) = closed_cycle_fixture();
        let temp = tempfile::tempdir().unwrap();
        let store = content_store::FileContentStore::create(temp.path()).unwrap();
        populate_store(&store, &closed, &objects, None);
        let mut engine = SiteEngine::default();
        let handle = engine.restore(closed, &store).unwrap();
        let path = site_build::OutputPath::parse("index.html").unwrap();
        let bytes = b"<!doctype html>".to_vec();
        let content = site_build::ContentRef::of_bytes(&bytes, Some("text/html"));
        let file = site_build::SiteOutputFile {
            path: path.clone(),
            content: content.clone(),
            producer: site_build::OutputProducer {
                id: "cycle-site".into(),
                version: "1".into(),
            },
            source: Some("page".into()),
            owner: None,
        };
        engine
            .open_renderer(
                &handle,
                site_build::RendererImplementation {
                    id: "cycle-site".into(),
                    version: "1".into(),
                    recipe_sha256: site_build::Sha256Digest::of_bytes(b"cycle recipe"),
                },
                "cycle-static-site/v1".into(),
                BTreeMap::new(),
                vec![path],
            )
            .unwrap();

        let error = engine.finalize(&handle).unwrap_err();
        assert_eq!(error.operation, crate::BuildOperation::Finalize);
        assert!(error.message.contains("1 Cycle outputs are not rendered"));
        assert!(engine
            .admit_output(&handle, file.clone(), b"wrong".to_vec())
            .unwrap_err()
            .contains("verify"));
        engine.admit_output(&handle, file, bytes).unwrap();
        let output = engine.finalize(&handle).unwrap();
        assert_eq!(output.files()[0].content, content);
    }

    #[test]
    fn generic_restore_rejects_a_non_cycle_external_target() {
        let (closed, objects) = closed_cycle_fixture();
        let site = closed.site_build();
        let wrong = site_build::SiteBuild::new(
            site.project().clone(),
            site.package_lock().clone(),
            site_build::RenderTarget {
                renderer: site_build::ProducerRef::new("not-cycle", "2"),
                mode: site_build::RenderMode::ExternalBuilder,
                fhir_version: site.render_target().fhir_version.clone(),
                template: None,
                parameters: site.render_target().parameters.clone(),
            },
            site.render_plan().clone(),
            site.artifacts().clone(),
            site.diagnostics().clone(),
        )
        .unwrap()
        .close()
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let store = content_store::FileContentStore::create(temp.path()).unwrap();
        populate_store(&store, &wrong, &objects, None);

        let error = SiteEngine::default().restore(wrong, &store).unwrap_err();
        assert!(error.contains("incompatible Cycle executor"), "{error}");
    }

    #[test]
    fn generic_restore_rejects_missing_and_corrupt_cycle_objects() {
        let (closed, objects) = closed_cycle_fixture();
        let resources = closed
            .site_build()
            .artifacts()
            .get(&site_build::cycle_semantic::resources_key())
            .unwrap();
        let site_build::ArtifactState::Ready { content } = &resources.state else {
            panic!("fixture resources are ready")
        };
        let digest = content.sha256.clone();

        let missing_temp = tempfile::tempdir().unwrap();
        let missing_store = content_store::FileContentStore::create(missing_temp.path()).unwrap();
        populate_store(&missing_store, &closed, &objects, Some(&digest));
        let missing = SiteEngine::default()
            .restore(closed.clone(), &missing_store)
            .unwrap_err();
        assert!(missing.contains("absent"), "{missing}");

        let corrupt_temp = tempfile::tempdir().unwrap();
        let corrupt_store = content_store::FileContentStore::create(corrupt_temp.path()).unwrap();
        populate_store(&corrupt_store, &closed, &objects, None);
        std::fs::write(corrupt_store.object_path(&digest), b"corrupt").unwrap();
        let corrupt = SiteEngine::default()
            .restore(closed, &corrupt_store)
            .unwrap_err();
        assert!(
            corrupt.contains("length mismatch") || corrupt.contains("digest mismatch"),
            "{corrupt}"
        );
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
        let project = site_build::ProjectIdentity {
            project_id: "demo.ig".into(),
            revision: "test".into(),
            sources: site_build::SourceManifest::from_entries(source_entries).unwrap(),
        };
        let template = site_build::PackageCoordinate::parse(template_label).unwrap();
        let lock = site_build::PackageLock::from_packages([
            site_build::LockedPackage {
                coordinate: core.clone(),
                content: site_build::ContentRef::of_bytes(
                    b"core",
                    Some(package_store::PREPARED_PACKAGE_MEDIA_TYPE),
                ),
                dependencies: BTreeSet::new(),
            },
            site_build::LockedPackage {
                coordinate: template.clone(),
                content: site_build::ContentRef::of_bytes(
                    b"template",
                    Some(package_store::PREPARED_PACKAGE_MEDIA_TYPE),
                ),
                dependencies: BTreeSet::new(),
            },
        ])
        .unwrap();
        let (artifact_records, artifact_roots, recipe_objects) =
            publisher_recipe_artifacts(&[template_label.into()], &core, tree.files(), &runtime)
                .unwrap();
        let recipe_assets = PublisherRecipeAssets {
            key: None,
            template_files: Rc::new(tree.files().clone()),
            publisher: Rc::new(runtime),
            ready: BTreeMap::new(),
            objects: recipe_objects,
            artifact_records: Rc::new(artifact_records),
            artifact_roots: Rc::new(artifact_roots),
        };
        let (artifacts, plan, mut objects) =
            publisher_artifacts(&prepared, &project, &lock, &recipe_assets).unwrap();
        merge_authenticated_objects(&mut objects, &recipe_assets.objects).unwrap();
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
                .and_then(|object| object.materialize().ok())
                .is_some_and(|bytes| content.verify(bytes.as_ref()).is_ok())
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
        let core = package_store::PreparedPackage::prepare(
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
        let template = package_store::PreparedPackage::prepare(
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
        let environment = PackageEnvironment::new([core, template]).unwrap();
        let config = "id: demo.ig\ncanonical: https://example.org/demo\nname: Demo\nstatus: draft\nversion: 1.0.0\nfhirVersion: 4.0.1\n";
        let resolved = crate::ResolvedPackageClosure {
            config_sha256: site_build::Sha256Digest::of_bytes(config.as_bytes()),
            resolution_support: BTreeSet::new(),
            labels: vec![core_label.into()],
        };
        let project = crate::compilation::CompiledProjectRevision::new(
            crate::ProjectRevision {
                config: config.into(),
                fsh: BTreeMap::new(),
                predefined: BTreeMap::new(),
                site_files: BTreeMap::from([(
                    "input/pagecontent/index.md".into(),
                    b"# First narrative".to_vec(),
                )]),
            },
            resolved.clone(),
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
            vec![(
                PathBuf::from("/__ig__/ImplementationGuide-demo.json"),
                ig.clone(),
            )],
        );
        let publisher_spec = GeneratorSpec::Publisher {
            template_coordinate: template_label.into(),
            build_epoch_secs: 1,
            active_tables: true,
            run_uuid: None,
        };
        let prepared = engine
            .prepare(publisher_spec.clone(), environment.clone())
            .unwrap();
        assert_eq!(
            engine
                .preparation
                .cache_hits
                .publisher_recipe_artifact_builds,
            1
        );
        let closed = prepared.site_build.clone();
        let live_catalog = engine.outputs(&prepared.build_id).unwrap();
        assert!(!live_catalog.outputs.is_empty());
        let mut content_refs = Vec::new();
        {
            let build = closed.site_build();
            assert!(!build.render_plan().is_empty());
            assert!(!build.artifacts().is_empty());
            assert_eq!(prepared.build_id, build.build_id().to_string());
            content_refs.extend(
                build
                    .project()
                    .sources
                    .iter()
                    .map(|(_, source)| source.content.clone()),
            );
            content_refs.extend(
                build
                    .package_lock()
                    .iter()
                    .map(|(_, package)| package.content.clone()),
            );
            content_refs.extend(build.artifacts().iter().map(|(_, artifact)| {
                let site_build::ArtifactState::Ready { content } = &artifact.state else {
                    panic!("closed Publisher artifact is not ready")
                };
                content.clone()
            }));
        }
        content_refs.sort_by(|left, right| left.sha256.cmp(&right.sha256));
        content_refs.dedup_by(|left, right| left.sha256 == right.sha256);
        let temp = tempfile::tempdir().unwrap();
        let store = content_store::FileContentStore::create(temp.path()).unwrap();
        for content in &content_refs {
            let bytes = engine
                .read_content(&prepared.build_id, content.sha256.as_str())
                .unwrap();
            content.verify(&bytes).unwrap();
            store.put(content, &bytes).unwrap();
        }

        let mut restored = SiteEngine::default();
        let restored_handle = restored.restore(closed, &store).unwrap();
        assert_eq!(restored_handle, prepared.build_id);
        assert_eq!(restored.outputs(&restored_handle).unwrap(), live_catalog);

        let page_paths = live_catalog
            .outputs
            .iter()
            .filter(|output| output.content.is_none())
            .map(|output| output.path.to_string())
            .collect::<Vec<_>>();
        let mut live_pages = BTreeMap::new();
        for path in &page_paths {
            let output = engine.render(&prepared.build_id, path).unwrap();
            let bytes = engine
                .read_content(&prepared.build_id, output.sha256.as_str())
                .unwrap();
            live_pages.insert(path.clone(), (output, bytes));
        }
        #[cfg(feature = "dependency-observation")]
        for path in &page_paths {
            let path = site_build::OutputPath::parse(path.clone()).unwrap();
            assert!(matches!(
                engine
                    .dependency_decision_for_page(&prepared.build_id, &path)
                    .unwrap(),
                ::dependency_observation::RebuildDecision::FullBuild { .. }
            ));
        }
        for path in page_paths.iter().rev() {
            let output = restored.render(&restored_handle, path).unwrap();
            let bytes = restored
                .read_content(&restored_handle, output.sha256.as_str())
                .unwrap();
            assert_eq!(Some(&(output, bytes)), live_pages.get(path));
        }
        let live_output = engine.finalize(&prepared.build_id).unwrap();
        let restored_output = restored.finalize(&restored_handle).unwrap();
        assert_eq!(
            live_output.canonical_bytes().unwrap(),
            restored_output.canonical_bytes().unwrap()
        );

        // Exact preparation reuses the retained immutable runtime, including
        // already rendered pages, without manufacturing another handle.
        let repeated = engine
            .prepare(publisher_spec.clone(), environment.clone())
            .unwrap();
        assert_eq!(repeated.build_id, prepared.build_id);
        assert!(repeated.measurements.site_build_cache_hit);

        let install_prose_revision = |engine: &mut SiteEngine, narrative: &str| {
            let project = crate::compilation::CompiledProjectRevision::new(
                crate::ProjectRevision {
                    config: config.into(),
                    fsh: BTreeMap::new(),
                    predefined: BTreeMap::new(),
                    site_files: BTreeMap::from([(
                        "input/pagecontent/index.md".into(),
                        narrative.as_bytes().to_vec(),
                    )]),
                },
                resolved.clone(),
            )
            .unwrap();
            engine.install_compilation_for_test(
                project,
                vec![(
                    PathBuf::from("/__ig__/ImplementationGuide-demo.json"),
                    ig.clone(),
                )],
            );
        };

        // A prose-only successor constructs a distinct complete build while
        // reusing only the exact semantic half of Publisher rendering.
        install_prose_revision(&mut engine, "# Second narrative");
        let second = engine
            .prepare(publisher_spec.clone(), environment.clone())
            .unwrap();
        assert_ne!(second.build_id, prepared.build_id);
        assert!(second.measurements.publisher_recipe_assets_cache_hit);
        assert!(second.measurements.render_semantics_cache_hit);
        assert!(!second.measurements.site_build_cache_hit);
        assert_eq!(second.measurements.template_materialize_ms, 0.0);
        assert_eq!(second.measurements.publisher_runtime_ms, 0.0);
        let prose_path = site_build::SourcePath::parse("input/pagecontent/index.md").unwrap();
        let prose_ref = second
            .site_build
            .site_build()
            .project()
            .sources
            .get(&prose_path)
            .unwrap()
            .content
            .clone();
        assert_eq!(
            engine
                .read_content(&second.build_id, prose_ref.sha256.as_str())
                .unwrap(),
            b"# Second narrative"
        );

        // Runtime retention is exactly current + previous. Refreshing an exact
        // predecessor changes recency; the next distinct success evicts the
        // other runtime, not the refreshed one.
        install_prose_revision(&mut engine, "# Third narrative");
        let third = engine
            .prepare(publisher_spec.clone(), environment.clone())
            .unwrap();
        assert!(third.measurements.publisher_recipe_assets_cache_hit);
        assert!(third.measurements.render_semantics_cache_hit);
        assert!(engine.outputs(&prepared.build_id).is_err());
        assert!(engine.outputs(&second.build_id).is_ok());
        assert!(engine.outputs(&third.build_id).is_ok());

        install_prose_revision(&mut engine, "# Second narrative");
        let second_again = engine
            .prepare(publisher_spec.clone(), environment.clone())
            .unwrap();
        assert_eq!(second_again.build_id, second.build_id);
        assert!(second_again.measurements.site_build_cache_hit);

        install_prose_revision(&mut engine, "# Fourth narrative");
        let fourth = engine
            .prepare(publisher_spec.clone(), environment.clone())
            .unwrap();
        assert!(fourth.measurements.publisher_recipe_assets_cache_hit);
        assert!(fourth.measurements.render_semantics_cache_hit);
        assert!(engine.outputs(&third.build_id).is_err());
        assert!(engine.outputs(&second.build_id).is_ok());
        assert!(engine.outputs(&fourth.build_id).is_ok());

        // Renderer-semantic options are identity, so changing one deliberately
        // misses the semantic reuse rather than borrowing incompatible state.
        let option_miss = engine
            .prepare(
                GeneratorSpec::Publisher {
                    template_coordinate: template_label.into(),
                    build_epoch_secs: 1,
                    active_tables: false,
                    run_uuid: None,
                },
                environment,
            )
            .unwrap();
        assert!(option_miss.measurements.publisher_recipe_assets_cache_hit);
        assert!(!option_miss.measurements.render_semantics_cache_hit);
        assert!(!option_miss.measurements.site_build_cache_hit);
        assert_eq!(
            engine
                .preparation
                .cache_hits
                .publisher_recipe_artifact_builds,
            1,
            "prose/options successors must reuse exact template/runtime artifact records"
        );
    }

    #[test]
    fn publisher_recipe_assets_key_binds_only_exact_runtime_inputs() {
        fn locked(label: &str, bytes: &[u8]) -> site_build::LockedPackage {
            site_build::LockedPackage {
                coordinate: site_build::PackageCoordinate::parse(label).unwrap(),
                content: site_build::ContentRef::of_bytes(
                    bytes,
                    Some(package_store::PREPARED_PACKAGE_MEDIA_TYPE),
                ),
                dependencies: BTreeSet::new(),
            }
        }
        fn package_lock(
            template: &[u8],
            base: &[u8],
            core: &[u8],
            unrelated: &[u8],
        ) -> site_build::PackageLock {
            site_build::PackageLock::from_packages([
                locked("demo.template#1.0.0", template),
                locked("demo.base#1.0.0", base),
                locked("hl7.fhir.r4.core#4.0.1", core),
                locked("example.unrelated#1.0.0", unrelated),
            ])
            .unwrap()
        }
        let template = site_build::PackageCoordinate::parse("demo.template#1.0.0").unwrap();
        let core = site_build::PackageCoordinate::parse("hl7.fhir.r4.core#4.0.1").unwrap();
        let chain = vec!["demo.template#1.0.0".into(), "demo.base#1.0.0".into()];
        let base = package_lock(b"template-a", b"base-a", b"core-a", b"unrelated-a");
        let key = publisher_recipe_assets_key(&template, &chain, &core, &base, "test").unwrap();

        let unrelated = package_lock(b"template-a", b"base-a", b"core-a", b"unrelated-b");
        assert_eq!(
            publisher_recipe_assets_key(&template, &chain, &core, &unrelated, "test").unwrap(),
            key
        );
        for changed in [
            package_lock(b"template-b", b"base-a", b"core-a", b"unrelated-a"),
            package_lock(b"template-a", b"base-b", b"core-a", b"unrelated-a"),
            package_lock(b"template-a", b"base-a", b"core-b", b"unrelated-a"),
        ] {
            assert_ne!(
                publisher_recipe_assets_key(&template, &chain, &core, &changed, "test").unwrap(),
                key
            );
        }
        let reversed = vec!["demo.base#1.0.0".into(), "demo.template#1.0.0".into()];
        assert_ne!(
            publisher_recipe_assets_key(&template, &reversed, &core, &base, "test").unwrap(),
            key
        );
    }
}

fn elapsed_ms(started: crate::runtime::PhaseTimer) -> f64 {
    started.elapsed_ms()
}

impl SiteEngine {
    fn preparation_cache_keys(
        &self,
        input: &PrepareGuideOptions,
        project: &site_build::ProjectIdentity,
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
    ) -> Result<Rc<site_build::PreparedGuide>, String> {
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
        let guide = Rc::new(assemble_prepared_model(
            operation,
            &snapshot.generated,
            &snapshot.primary_implementation_guide,
            project.config(),
            project.site_files(),
            input.build_epoch_secs,
            &input.liquid_asset_dirs,
            input.branch.clone(),
            input.revision.clone(),
        )?);
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
    project: &crate::compilation::CompiledProjectRevision,
    project_id: &str,
) -> Result<site_build::ProjectIdentity, String> {
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
    Ok(site_build::ProjectIdentity {
        project_id: project_id.into(),
        revision,
        sources,
    })
}

fn site_build_package_lock(
    project: &crate::compilation::CompiledProjectRevision,
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
            content: material.content().clone(),
            dependencies,
        });
    }
    site_build::PackageLock::from_packages(packages)
        .map_err(|error| format!("prepare: package lock: {error}"))
}

fn environment_objects(
    project: &crate::compilation::CompiledProjectRevision,
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
        let content = material.content();
        let object = material.object();
        if let Some(existing) = objects.get(&content.sha256) {
            if existing.content().byte_length != content.byte_length {
                return Err(format!(
                    "{operation}: conflicting package-object length for {}",
                    content.sha256
                ));
            }
        } else {
            objects.insert(content.sha256.clone(), object);
        }
    }
    Ok(objects)
}

fn project_source_bytes(
    project: &crate::compilation::CompiledProjectRevision,
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
    project: &crate::compilation::CompiledProjectRevision,
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
            let severity = match diagnostic.severity {
                crate::CompilationDiagnosticSeverity::Error => {
                    site_build::DiagnosticSeverity::Error
                }
                crate::CompilationDiagnosticSeverity::Warning => {
                    site_build::DiagnosticSeverity::Warning
                }
                crate::CompilationDiagnosticSeverity::Info => {
                    site_build::DiagnosticSeverity::Information
                }
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
            content: material.content().clone(),
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

fn publisher_recipe_assets_key(
    template: &site_build::PackageCoordinate,
    template_chain: &[String],
    core: &site_build::PackageCoordinate,
    package_lock: &site_build::PackageLock,
    operation: &str,
) -> Result<site_build::Sha256Digest, String> {
    let mut chain = Vec::with_capacity(template_chain.len());
    for label in template_chain {
        let coordinate = site_build::PackageCoordinate::parse(label)
            .map_err(|error| format!("{operation}: invalid template-chain coordinate: {error}"))?;
        let package = package_lock.get(&coordinate).ok_or_else(|| {
            format!("{operation}: template-chain package {coordinate} is absent from lock")
        })?;
        chain.push(PublisherRecipePackage {
            coordinate: &package.coordinate,
            content: &package.content,
        });
    }
    let core_package = package_lock
        .get(core)
        .ok_or_else(|| format!("{operation}: core package {core} is absent from lock"))?;
    site_build::sha256_canonical(&PublisherRecipeAssetsCachePayload {
        schema: PUBLISHER_RECIPE_ASSETS_SCHEMA,
        recipe: PUBLISHER_RECIPE_ASSETS_RECIPE,
        engine_api: ENGINE_API,
        template,
        template_chain: chain,
        core: PublisherRecipePackage {
            coordinate: &core_package.coordinate,
            content: &core_package.content,
        },
    })
    .map_err(|error| format!("{operation}: hash Publisher recipe assets: {error}"))
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
        let mut metrics = PrepareMeasurements::default();
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
                build_id: handle,
                generator: GeneratorKind::Publisher,
                site_build: build,
                measurements: metrics,
            });
        }

        let started = self.timer();
        let prepared = self.site_model_from_compile(
            &guide_options,
            environment,
            operation,
            keys.prepared_guide.clone(),
            keys.snapshot_completed_local.clone(),
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
        let core = target_core_from_package_lock(&package_lock, &fhir_version, operation)?;
        let recipe_assets_key = publisher_recipe_assets_key(
            &template,
            &resolution.chain,
            &core,
            &package_lock,
            operation,
        )?;
        let recipe_assets = match self.reuse_publisher_recipe_assets(&recipe_assets_key) {
            Some(assets) => {
                metrics.publisher_recipe_assets_cache_hit = true;
                assets
            }
            None => {
                let started = self.timer();
                let tree = package_store::template_loader::materialize(
                    &environment.packages,
                    &package_store::TemplatePaths::new(environment.packages.root()),
                    template_coordinate,
                )
                .map_err(|error| {
                    format!("{operation}: materialize {template_coordinate}: {error}")
                })?;
                metrics.template_materialize_ms = elapsed_ms(started);
                let started = self.timer();
                let publisher = Rc::new(
                    site_producer::publisher_runtime::PublisherRuntime::assemble(
                        &environment.packages,
                        environment.packages.root(),
                        &core,
                        &tree,
                    )
                    .map_err(|error| format!("{operation}: Publisher runtime: {error:#}"))?,
                );
                let mut runtime_objects = ObjectMap::new();
                let runtime_ready =
                    publisher_runtime_outputs(publisher.as_ref(), &mut runtime_objects)?;
                let template_files = Rc::new(tree.into_files());
                let (artifact_records, artifact_roots, artifact_objects) =
                    publisher_recipe_artifacts(
                        &resolution.chain,
                        &core,
                        template_files.as_ref(),
                        publisher.as_ref(),
                    )?;
                #[cfg(test)]
                {
                    self.preparation.cache_hits.publisher_recipe_artifact_builds += 1;
                }
                merge_authenticated_objects(&mut runtime_objects, &artifact_objects)?;
                metrics.publisher_runtime_ms = elapsed_ms(started);
                Rc::new(PublisherRecipeAssets {
                    key: Some(recipe_assets_key),
                    template_files,
                    publisher,
                    ready: runtime_ready,
                    objects: runtime_objects,
                    artifact_records: Rc::new(artifact_records),
                    artifact_roots: Rc::new(artifact_roots),
                })
            }
        };
        let mut objects = environment_objects(&project, &package_lock, environment, operation)?;
        merge_authenticated_objects(&mut objects, &recipe_assets.objects)?;
        let mut ready = recipe_assets.ready.clone();

        let started = self.timer();
        let model = publisher_model(
            &prepared,
            recipe_assets.template_files.as_ref(),
            &mut ready,
            &mut objects,
            &project_id,
            operation,
        )?;
        metrics.publisher_model_ms = elapsed_ms(started);

        let started = self.timer();
        let render_semantics_key = publisher_render_semantics_cache_key(
            &keys.snapshot_completed_local,
            &model,
            &options,
            operation,
        )?;
        let render_semantics = match self
            .preparation
            .publisher_render_semantics
            .as_ref()
            .filter(|entry| entry.key == render_semantics_key)
        {
            Some(entry) => {
                metrics.render_semantics_cache_hit = true;
                entry.semantics.clone()
            }
            None => {
                let render_packages = publisher_render_package_view(
                    environment.scoped(&project.resolved_packages().labels, operation)?,
                    &project.resolved_packages().labels,
                    operation,
                )?;
                let semantics = Rc::new(publisher_render_semantics(
                    &prepared,
                    render_packages,
                    &model,
                    &options,
                )?);
                self.preparation.publisher_render_semantics =
                    Some(PublisherRenderSemanticsCacheEntry {
                        key: render_semantics_key,
                        semantics: semantics.clone(),
                    });
                semantics
            }
        };
        let state = publisher_render_state(&render_semantics, &model, &options)?;
        metrics.render_model_ms = elapsed_ms(started);

        let catalog_total_started = self.timer();
        let started = self.timer();
        let (catalog, tree_digest) = publisher_catalog(&state, &mut ready, &model, operation)?;
        metrics.output_catalog_ms = elapsed_ms(started);
        let mut parameters = BTreeMap::from([
            ("contract".into(), "publisher-site/v1".into()),
            (
                "executorRecipe".into(),
                PUBLISHER_RUNTIME_PREPARATION_RECIPE.into(),
            ),
            (
                "compilePackageOrder".into(),
                serde_json::to_string(&project.resolved_packages().labels)
                    .map_err(|error| format!("{operation}: package order: {error}"))?,
            ),
            ("mountedTreeSha256".into(), tree_digest.to_string()),
            (
                "preparedGuideSha256".into(),
                keys.prepared_guide.to_string(),
            ),
            (
                "publisherRuntimeRecipeSha256".into(),
                recipe_assets.publisher.recipe_sha256().to_string(),
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
        let started = self.timer();
        let (artifacts, plan, artifact_objects) = publisher_artifacts(
            &prepared,
            &project_revision,
            &package_lock,
            recipe_assets.as_ref(),
        )?;
        merge_authenticated_objects(&mut objects, &artifact_objects)?;
        metrics.publisher_artifacts_ms = elapsed_ms(started);
        let started = self.timer();
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
        metrics.site_build_close_ms = elapsed_ms(started);
        let started = self.timer();
        verify_ready_artifacts(&build, &objects, operation)?;
        metrics.closure_verify_ms = elapsed_ms(started);
        let renderer = site_build::RendererImplementation {
            id: "publisher-template-rust".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            recipe_sha256: site_build::sha256_canonical(&(template, &parameters))
                .map_err(|error| format!("{operation}: renderer recipe: {error}"))?,
        };
        #[cfg(feature = "dependency-observation")]
        let dependency_observation = match self.dependency_compilation() {
            Some((compilation, package_lookups)) => {
                crate::dependency_observation::BuildDependencyObservation::capture(
                    compilation,
                    package_lookups,
                    &prepared,
                    &build,
                    &catalog,
                )
            }
            None => crate::dependency_observation::BuildDependencyObservation::unavailable(
                "successful compilation has no dependency observation",
            ),
        };
        let handle = build.site_build().build_id().to_string();
        let installed = self.install_publisher(PublisherRuntime {
            preparation_key: Some(preparation_key),
            recipe_assets,
            state,
            build: build.clone(),
            catalog,
            ready,
            objects,
            renderer,
            output_options: parameters,
            #[cfg(feature = "dependency-observation")]
            dependency_observation,
        });
        debug_assert_eq!(installed, handle);
        metrics.catalog_ms = elapsed_ms(catalog_total_started);
        metrics.total_ms = elapsed_ms(total_started);
        Ok(PrepareResult {
            build_id: handle,
            generator: GeneratorKind::Publisher,
            site_build: build,
            measurements: metrics,
        })
    }
}

impl SiteEngine {
    /// Admit a closed build into a fresh executor from only its authenticated
    /// `ContentStore` objects.
    ///
    /// This is lifecycle reconstruction, not a fifth host operation: callers
    /// still interact with the resulting immutable handle exclusively through
    /// `outputs`, `render`, and `finalize`. Native-template Publisher builds
    /// reconstruct their private renderer state; callback-free Cycle builds
    /// restore the same external-builder handle and authenticated closure.
    pub fn restore(
        &mut self,
        build: site_build::ClosedSiteBuild,
        store: &dyn content_store::ContentStore,
    ) -> Result<String, String> {
        match build.site_build().render_target().mode {
            site_build::RenderMode::NativeTemplate => self.restore_publisher_runtime(build, store),
            site_build::RenderMode::ExternalBuilder => self.restore_cycle_runtime(build, store),
        }
    }

    fn restore_cycle_runtime(
        &mut self,
        build: site_build::ClosedSiteBuild,
        store: &dyn content_store::ContentStore,
    ) -> Result<String, String> {
        let operation = "restore(cycle)";
        build
            .site_build()
            .verify()
            .map_err(|error| format!("{operation}: verify SiteBuild: {error}"))?;
        let objects = all_closed_objects(&build, store, operation)?;
        validate_closed_cycle(&build, &objects, operation)?;
        verify_closed_package_materials(&build, &objects, operation)?;
        Ok(self.install_cycle(build, objects))
    }

    fn restore_publisher_runtime(
        &mut self,
        build: site_build::ClosedSiteBuild,
        store: &dyn content_store::ContentStore,
    ) -> Result<String, String> {
        let operation = "restore(publisher)";
        build
            .site_build()
            .verify()
            .map_err(|error| format!("{operation}: verify SiteBuild: {error}"))?;
        let mut objects = all_closed_objects(&build, store, operation)?;
        let target = build.site_build().render_target();
        let expected_renderer =
            site_build::ProducerRef::new("publisher-template-rust", env!("CARGO_PKG_VERSION"));
        if target.mode != site_build::RenderMode::NativeTemplate
            || target.renderer != expected_renderer
            || target.parameters.get("contract").map(String::as_str) != Some("publisher-site/v1")
            || target.parameters.get("executorRecipe").map(String::as_str)
                != Some(PUBLISHER_RUNTIME_PREPARATION_RECIPE)
        {
            return Err(format!(
                "{operation}: build targets an incompatible Publisher executor"
            ));
        }
        let template = target
            .template
            .clone()
            .ok_or_else(|| format!("{operation}: Publisher build has no template coordinate"))?;
        let active_tables = target
            .parameters
            .get("activeTables")
            .ok_or_else(|| format!("{operation}: missing activeTables"))?
            .parse::<bool>()
            .map_err(|error| format!("{operation}: invalid activeTables: {error}"))?;
        let run_uuid = target.parameters.get("runUuid").cloned();
        let package_order = target
            .parameters
            .get("compilePackageOrder")
            .ok_or_else(|| format!("{operation}: missing compilePackageOrder"))
            .and_then(|value| {
                serde_json::from_str::<Vec<String>>(value)
                    .map_err(|error| format!("{operation}: invalid compilePackageOrder: {error}"))
            })?;
        let expected_tree = target
            .parameters
            .get("mountedTreeSha256")
            .ok_or_else(|| format!("{operation}: missing mountedTreeSha256"))
            .and_then(|value| {
                site_build::Sha256Digest::parse(value.clone())
                    .map_err(|error| format!("{operation}: invalid mountedTreeSha256: {error}"))
            })?;
        let expected_runtime_recipe = target
            .parameters
            .get("publisherRuntimeRecipeSha256")
            .ok_or_else(|| format!("{operation}: missing publisherRuntimeRecipeSha256"))
            .and_then(|value| {
                site_build::Sha256Digest::parse(value.clone()).map_err(|error| {
                    format!("{operation}: invalid publisherRuntimeRecipeSha256: {error}")
                })
            })?;

        validate_closed_publisher_artifact_inventory(&build, &package_order, operation)?;
        verify_closed_package_materials(&build, &objects, operation)?;

        let prepared = prepared_from_closed_publisher(&build, &objects, operation)?;
        if prepared.guide.package_id != build.site_build().project().project_id
            || prepared.guide.fhir_version != target.fhir_version
        {
            return Err(format!(
                "{operation}: PreparedGuide identity disagrees with SiteBuild"
            ));
        }
        let template_artifacts = closed_artifact_files(
            &build,
            &objects,
            &site_build::AssetNamespace::Template,
            "package-store.template-loader",
            "template.materialize/v1",
            operation,
        )?;
        let template_files = Rc::new(
            template_artifacts
                .into_iter()
                .map(|(path, (_, _, bytes))| (path, bytes))
                .collect::<BTreeMap<_, _>>(),
        );
        let runtime_artifacts = closed_artifact_files(
            &build,
            &objects,
            &site_build::AssetNamespace::PublisherRuntime,
            "publisher-runtime",
            "publisher-runtime.assemble/v1",
            operation,
        )?;
        let runtime_files = runtime_artifacts
            .into_iter()
            .map(|(path, (provenance, media_type, bytes))| {
                let source = provenance
                    .attributes
                    .get("source")
                    .cloned()
                    .ok_or_else(|| {
                        format!("{operation}: runtime {path} has no source provenance")
                    })?;
                let license = provenance
                    .attributes
                    .get("license")
                    .cloned()
                    .ok_or_else(|| {
                        format!("{operation}: runtime {path} has no license provenance")
                    })?;
                let source_path = provenance
                    .attributes
                    .get("sourcePath")
                    .cloned()
                    .ok_or_else(|| {
                        format!("{operation}: runtime {path} has no sourcePath provenance")
                    })?;
                Ok(site_producer::publisher_runtime::PublisherRuntimeFile {
                    path,
                    bytes,
                    media_type,
                    provenance: site_producer::publisher_runtime::PublisherRuntimeProvenance {
                        source,
                        license,
                        source_path,
                        transformation: provenance.attributes.get("transformation").cloned(),
                    },
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let core = target_core_from_package_lock(
            build.site_build().package_lock(),
            &target.fhir_version,
            operation,
        )?;
        if !package_order.iter().any(|label| label == core.as_str()) {
            return Err(format!(
                "{operation}: target core {core} is absent from compilePackageOrder"
            ));
        }
        let publisher = Rc::new(
            site_producer::publisher_runtime::PublisherRuntime::from_closed_files(
                runtime_files,
                &core,
                &expected_runtime_recipe,
            )
            .map_err(|error| format!("{operation}: reconstruct Publisher runtime: {error:#}"))?,
        );
        let mut runtime_objects = ObjectMap::new();
        let runtime_ready = publisher_runtime_outputs(publisher.as_ref(), &mut runtime_objects)?;
        merge_authenticated_objects(&mut objects, &runtime_objects)?;
        let recipe_assets = Rc::new(PublisherRecipeAssets {
            key: None,
            template_files,
            publisher,
            ready: runtime_ready,
            objects: runtime_objects,
            artifact_records: Rc::new(Vec::new()),
            artifact_roots: Rc::new(BTreeSet::new()),
        });
        let mut ready = recipe_assets.ready.clone();
        let model = publisher_model(
            &prepared,
            recipe_assets.template_files.as_ref(),
            &mut ready,
            &mut objects,
            &prepared.guide.package_id,
            operation,
        )?;
        let options = SiteOptions {
            active_tables,
            run_uuid,
            ..Default::default()
        };
        let packages = package_view_from_closed_build(&build, &objects, &package_order, operation)?;
        let semantics = publisher_render_semantics(&prepared, packages, &model, &options)?;
        let state = publisher_render_state(&semantics, &model, &options)?;
        let (catalog, actual_tree) = publisher_catalog(&state, &mut ready, &model, operation)?;
        if actual_tree != expected_tree {
            return Err(format!(
                "{operation}: mounted tree mismatch: expected {expected_tree}, reconstructed {actual_tree}"
            ));
        }
        let parameters = target.parameters.clone();
        let renderer = site_build::RendererImplementation {
            id: target.renderer.id.clone(),
            version: target.renderer.version.clone(),
            recipe_sha256: site_build::sha256_canonical(&(template, &parameters))
                .map_err(|error| format!("{operation}: renderer recipe: {error}"))?,
        };
        let handle = build.site_build().build_id().to_string();
        #[cfg(feature = "dependency-observation")]
        let dependency_observation =
            crate::dependency_observation::BuildDependencyObservation::restored(&build, &catalog);
        let installed = self.install_publisher(PublisherRuntime {
            preparation_key: None,
            recipe_assets,
            state,
            build,
            catalog,
            ready,
            objects,
            renderer,
            output_options: parameters,
            #[cfg(feature = "dependency-observation")]
            dependency_observation,
        });
        debug_assert_eq!(installed, handle);
        Ok(handle)
    }
}

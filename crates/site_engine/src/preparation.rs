use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;

use package_store::PackageSource;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::render_surface::{
    build_render_semantics_reusing_package_catalog, RenderPackageCatalog, RenderSemantics,
};
use crate::runtime::{
    is_static_output, AuthenticatedObject, MaterializedPublisherCatalog, ObjectMap,
    OutputDescriptor, OutputResourceSubject, OutputSubjectPage, PreparedOutput,
    PublisherRecipeAssets, PublisherRenderCore, PublisherRuntime,
};
use crate::{
    build_render_semantics, build_render_state_from_semantics, PackageView, RenderState,
    SiteEngine, SiteOptions,
};

const ENGINE_API: u32 = 1;
const PREPARED_GUIDE_IDENTITY_SCHEMA: &str = "prepared-guide-identity/v1";
const PREPARED_GUIDE_RECIPE: &str = "sushi.snapshot+site-semantics/v1";
const SNAPSHOT_DERIVATION_MAX_RESOURCES: usize = 4_096;
const SNAPSHOT_DERIVATION_MAX_FACTS: usize = 100_000;
const SNAPSHOT_DERIVATION_MAX_MANIFEST_APPROX_BYTES: usize = 32 * 1024 * 1024;
const SNAPSHOT_DERIVATION_MAX_SNAPSHOT_JSON_BYTES: usize = 128 * 1024 * 1024;
const CYCLE_PROJECTION_RECIPE: &str = "site-build.cycle-projection/v2";
const CYCLE_RUNTIME_KEY_SCHEMA: &str = "cycle-runtime-key/v1";
const PUBLISHER_RUNTIME_PREPARATION_SCHEMA: &str = "publisher-runtime-preparation-key/v1";
const PUBLISHER_RUNTIME_PREPARATION_RECIPE: &str =
    "publisher-template-rust.runtime+model+structural+render+catalog/v3";
const PUBLISHER_RENDER_SEMANTICS_CACHE_SCHEMA: &str = "publisher-render-semantics-cache/v2";
const PUBLISHER_RENDER_PACKAGE_CATALOG_CACHE_SCHEMA: &str =
    "publisher-render-package-catalog-cache/v2";
const PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_PACKAGES: usize = 1_024;
const PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_RESOURCE_ENTRIES: usize = 100_000;
const PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_APPROX_BYTES: usize = 32 * 1024 * 1024;
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
    key: package_store::PreparedPackageKey,
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
            key: mount.key,
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
            .field("key", &self.key)
            .field("content", self.content())
            .field("declared_dependencies", &self.declared_dependencies)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct PackageEnvironment {
    source: Rc<package_store::BundleSource>,
    packages: PackageView,
    mounted_labels: Rc<Vec<String>>,
    materials: Rc<BTreeMap<String, PackageMaterial>>,
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
        Self::from_parts(source, mounted_labels, materials)
    }

    fn from_parts(
        source: package_store::BundleSource,
        mounted_labels: Vec<String>,
        materials: BTreeMap<String, PackageMaterial>,
    ) -> Result<Self, String> {
        let source = Rc::new(source);
        let root = source.cache_root().to_path_buf();
        let carrier_identities = materials
            .iter()
            .map(|(label, material)| (label.clone(), material.content().clone()))
            .collect();
        Ok(Self {
            packages: PackageView::new(source.clone(), root, None)
                .with_carrier_identities(carrier_identities),
            source,
            mounted_labels: Rc::new(mounted_labels),
            materials: Rc::new(materials),
        })
    }

    /// Return a successor environment by mounting one complete package batch.
    /// Validation finishes before the shallow source clone; all remaining
    /// fallible construction stays off-side, so callers can atomically replace
    /// the prior environment only after the successor is complete. The
    /// execution view and package-lock carriers are created from each same
    /// typed `PreparedPackage`; they cannot drift into parallel authorities.
    pub fn extended(
        &self,
        prepared: Vec<package_store::PreparedPackage>,
        operation: &str,
    ) -> Result<(Self, u32), String> {
        let mut transaction = BTreeSet::new();
        for package in &prepared {
            if !transaction.insert(package.label.clone()) {
                return Err(format!(
                    "{operation}: duplicate package label in one transaction: {}",
                    package.label
                ));
            }
            if let Some(existing) = self.materials.get(&package.label) {
                let incoming = site_build::Sha256Digest::parse(package.artifact_sha256())
                    .map_err(|error| format!("{operation}: prepared carrier digest: {error}"))?;
                if existing.key != package.key || existing.content().sha256 != incoming {
                    return Err(format!(
                        "{operation}: package label {} is already mounted with different content",
                        package.label
                    ));
                }
            }
        }

        let mut source = (*self.source).clone();
        let mut mounted_labels = (*self.mounted_labels).clone();
        let mut materials = (*self.materials).clone();
        let mut added = 0u32;
        for package in prepared {
            if materials.contains_key(&package.label) {
                continue;
            }
            let label = package.label.clone();
            let material = PackageMaterial::from_prepared(package.mount_into(&mut source))?;
            mounted_labels.push(label.clone());
            materials.insert(label, material);
            added += 1;
        }
        if added == 0 {
            return Ok((self.clone(), 0));
        }
        Ok((Self::from_parts(source, mounted_labels, materials)?, added))
    }

    pub fn mounted_len(&self) -> usize {
        self.mounted_labels.len()
    }

    pub fn mounted_labels(&self) -> &[String] {
        self.mounted_labels.as_slice()
    }

    pub fn package_key(&self, label: &str) -> Option<&package_store::PreparedPackageKey> {
        self.materials.get(label).map(|material| &material.key)
    }

    pub fn all_packages(&self) -> PackageView {
        self.packages.clone()
    }

    pub fn compression_metrics(&self) -> package_store::BundleCompressionMetrics {
        self.source.compression_metrics()
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
pub(crate) struct PrepareMeasurements {
    total_ms: f64,
    snapshot_resource_cache_hits: u64,
    snapshot_resource_cache_misses: u64,
    snapshot_derivation_history_generations: u64,
    snapshot_derivation_retained_facts: u64,
    snapshot_derivation_retained_manifest_approx_bytes: u64,
    snapshot_derivation_retained_snapshot_json_bytes: u64,
    snapshot_derivation_admitted: bool,
    site_build_cache_hit: bool,
    publisher_recipe_assets_cache_hit: bool,
    render_semantics_cache_hit: bool,
    render_package_catalog_cache_hit: bool,
    render_package_catalog_built: bool,
    render_own_context_built: bool,
    render_own_resources_preparsed: u64,
    render_package_catalog_packages: u64,
    render_package_catalog_entries: u64,
    render_package_catalog_approx_bytes: u64,
    render_package_catalog_admitted: bool,
    render_package_catalog_retained_generations: u64,
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
            (
                "snapshotResourceCacheHits".into(),
                self.snapshot_resource_cache_hits as f64,
            ),
            (
                "snapshotResourceCacheMisses".into(),
                self.snapshot_resource_cache_misses as f64,
            ),
            (
                "snapshotDerivationHistoryGenerations".into(),
                self.snapshot_derivation_history_generations as f64,
            ),
            (
                "snapshotDerivationRetainedFacts".into(),
                self.snapshot_derivation_retained_facts as f64,
            ),
            (
                "snapshotDerivationRetainedManifestApproxBytes".into(),
                self.snapshot_derivation_retained_manifest_approx_bytes as f64,
            ),
            (
                "snapshotDerivationRetainedSnapshotJsonBytes".into(),
                self.snapshot_derivation_retained_snapshot_json_bytes as f64,
            ),
            (
                "snapshotDerivationAdmitted".into(),
                f64::from(self.snapshot_derivation_admitted),
            ),
            (
                "siteBuildCacheHit".into(),
                f64::from(self.site_build_cache_hit),
            ),
            (
                "publisherRecipeAssetsCacheHit".into(),
                f64::from(self.publisher_recipe_assets_cache_hit),
            ),
            (
                "renderSemanticsCacheHit".into(),
                f64::from(self.render_semantics_cache_hit),
            ),
            (
                "renderPackageCatalogCacheHit".into(),
                f64::from(self.render_package_catalog_cache_hit),
            ),
            (
                "renderPackageCatalogBuilt".into(),
                f64::from(self.render_package_catalog_built),
            ),
            (
                "renderOwnContextBuilt".into(),
                f64::from(self.render_own_context_built),
            ),
            (
                "renderOwnResourcesPreparsed".into(),
                self.render_own_resources_preparsed as f64,
            ),
            (
                "renderPackageCatalogPackages".into(),
                self.render_package_catalog_packages as f64,
            ),
            (
                "renderPackageCatalogEntries".into(),
                self.render_package_catalog_entries as f64,
            ),
            (
                "renderPackageCatalogApproxBytes".into(),
                self.render_package_catalog_approx_bytes as f64,
            ),
            (
                "renderPackageCatalogAdmitted".into(),
                f64::from(self.render_package_catalog_admitted),
            ),
            (
                "renderPackageCatalogRetainedGenerations".into(),
                self.render_package_catalog_retained_generations as f64,
            ),
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
    history: crate::History2<PreparationGeneration>,
    #[cfg(test)]
    cache_hits: DerivedCacheHits,
}

#[derive(Default)]
struct PreparationGeneration {
    snapshots: SnapshotDerivationGeneration,
    render_semantics: Option<PublisherRenderSemanticsCacheEntry>,
    render_package_catalog: Option<PublisherRenderPackageCatalogCacheEntry>,
}

#[derive(Clone)]
struct PublisherRenderSemanticsCacheEntry {
    key: site_build::Sha256Digest,
    semantics: Rc<RenderSemantics>,
}

#[derive(Clone)]
struct PublisherRenderPackageCatalogCacheEntry {
    key: site_build::Sha256Digest,
    catalog: Option<Rc<RenderPackageCatalog>>,
}

#[derive(Clone)]
struct SnapshotResourceDerivation {
    derivation: Rc<snapshot_gen::SnapshotDerivation>,
    retention: snapshot_gen::SnapshotDerivationRetention,
}

#[derive(Default)]
struct SnapshotDerivationGeneration {
    resources: BTreeMap<String, SnapshotResourceDerivation>,
    fact_count: usize,
    manifest_approx_bytes: usize,
    snapshot_json_bytes: usize,
}

struct SnapshotDerivationCandidate {
    generation: SnapshotDerivationGeneration,
    admitted: bool,
}

struct PreparedGuideStage {
    guide: Rc<site_build::PreparedGuide>,
    generation: PreparationGeneration,
    snapshot_admitted: bool,
}

struct PublisherClosureInputs {
    preparation_key: site_build::Sha256Digest,
    project: site_build::ProjectIdentity,
    package_lock: site_build::PackageLock,
    prepared: Rc<site_build::PreparedGuide>,
    recipe_assets: Rc<PublisherRecipeAssets>,
    template: site_build::PackageCoordinate,
    fhir_version: String,
    renderer: site_build::ProducerRef,
    parameters: BTreeMap<String, String>,
    diagnostics: BTreeSet<site_build::BuildDiagnostic>,
}

struct NewPublisherTarget {
    pub(crate) core: PublisherRenderCore,
    closure: PublisherClosureInputs,
    preparation: PreparedGuideStage,
    metrics: PrepareMeasurements,
}

enum PublisherTargetCandidate {
    Reuse {
        handle: String,
        build: site_build::ClosedSiteBuild,
        metrics: PrepareMeasurements,
    },
    New(NewPublisherTarget),
}

struct TargetCandidate {
    compilation: crate::compilation::CompilationCandidate,
    compile_ms: f64,
    generation: PublisherTargetCandidate,
}

struct PreparationCacheKeys {
    prepared_guide: site_build::Sha256Digest,
    render_semantics_source: site_build::Sha256Digest,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RenderSemanticsSourceKeyPayload<'a> {
    schema: &'static str,
    engine_api: u32,
    compiled_sha256: &'a site_build::Sha256Digest,
    package_lock: &'a site_build::PackageLock,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublisherRenderSemanticsCachePayload<'a> {
    schema: &'static str,
    prepared_guide_key: &'a site_build::Sha256Digest,
    active_tables: bool,
    run_uuid: Option<&'a str>,
    build_epoch_secs: i64,
    txcache: BTreeMap<String, site_build::Sha256Digest>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublisherRenderPackageCatalogCachePayload<'a> {
    schema: &'static str,
    engine_api: u32,
    primary_implementation_guide: &'a site_build::SemanticResourceKey,
    fhir_version: Option<&'a str>,
    dependencies: Vec<PublisherRenderPackageSelection<'a>>,
    package_lock: &'a site_build::PackageLock,
    resolved_labels: &'a [String],
    package_root: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublisherRenderPackageSelection<'a> {
    package_id: &'a str,
    version: &'a str,
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

#[cfg(test)]
#[derive(Default)]
struct DerivedCacheHits {
    snapshot_resources: u64,
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
struct PreparedGuideIdentityPayload<'a> {
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
struct CycleRuntimeKeyPayload<'a> {
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

    /// Preparation generations are keyed or self-validating and deliberately
    /// survive semantic transitions so current/previous reuse remains possible.
    pub(crate) fn invalidate_exact_preparation(&mut self) {}

    #[cfg(test)]
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
                None,
            ),
            GeneratorSpec::Publisher {
                template_coordinate,
                build_epoch_secs,
                active_tables,
                run_uuid,
            } => {
                let generation = self.prepare_publisher_generation(
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
                    None,
                )?;
                self.close_publisher_generation(generation)
            }
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
        if matches!(spec, GeneratorSpec::Publisher { .. }) {
            let pending = self.prepare_publisher_values(inputs, resolved, spec, environment)?;
            return self.close_publisher_value(pending);
        }
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
        let candidate = self
            .compile_candidate(inputs, packages, resolved)
            .map_err(|message| {
                crate::BuildError::new(
                    crate::BuildOperation::Prepare,
                    crate::BuildErrorPhase::Compilation,
                    crate::BuildErrorCode::CompileFailed,
                    message,
                )
            })?;
        let compilation = candidate.outcome().clone();
        let compile_ms = elapsed_ms(compile_started);
        let GeneratorSpec::Cycle {
            build_epoch_secs,
            liquid_asset_dirs,
            branch,
            revision,
        } = spec
        else {
            unreachable!("Publisher prepare_values returns through its pending transaction")
        };
        let site = match self.prepare_cycle(
            &PrepareGuideOptions {
                build_epoch_secs,
                liquid_asset_dirs,
                branch,
                revision,
            },
            &environment,
            Some(&candidate),
        ) {
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
        Ok(self.commit_success(candidate, site, compile_ms))
    }

    /// The sole successful ProjectRevision transaction commit. Target closure,
    /// content verification, and runtime installation have no remaining
    /// fallible work when this is called; semantic recency and the authored
    /// revision therefore advance together with the already-closed target.
    fn commit_success(
        &mut self,
        compilation: crate::compilation::CompilationCandidate,
        site: PrepareResult,
        compile_ms: f64,
    ) -> PreparedProjectResult {
        let outcome = compilation.outcome().clone();
        let compilation_measurements = self.commit_compilation(compilation);
        let event = site
            .measurements
            .event(&site.build_id, compile_ms, &compilation_measurements);
        PreparedProjectResult {
            compilation: outcome,
            site,
            events: vec![event],
        }
    }

    fn prepare_publisher_values(
        &mut self,
        inputs: crate::ProjectRevision,
        resolved: crate::ResolvedPackageClosure,
        spec: GeneratorSpec,
        environment: PackageEnvironment,
    ) -> Result<TargetCandidate, crate::BuildError<crate::CompilationOutcome>> {
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
        let compilation =
            self.compile_candidate(inputs, packages, resolved)
                .map_err(|message| {
                    crate::BuildError::new(
                        crate::BuildOperation::Prepare,
                        crate::BuildErrorPhase::Compilation,
                        crate::BuildErrorCode::CompileFailed,
                        message,
                    )
                })?;
        let compile_ms = elapsed_ms(compile_started);
        let GeneratorSpec::Publisher {
            template_coordinate,
            build_epoch_secs,
            active_tables,
            run_uuid,
        } = spec
        else {
            unreachable!("prepare_publisher_values requires Publisher")
        };
        let generation = match self
            .prepare_publisher_generation(
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
                Some(&compilation),
            )
            .map_err(|message| {
                crate::BuildError::new(
                    crate::BuildOperation::Prepare,
                    crate::BuildErrorPhase::Preparation,
                    crate::BuildErrorCode::RendererFailed,
                    message,
                )
                .with_successful_compilation(compilation.outcome().clone())
            }) {
            Ok(generation) => generation,
            Err(error) => return Err(error),
        };
        Ok(TargetCandidate {
            compilation,
            compile_ms,
            generation,
        })
    }

    fn close_publisher_value(
        &mut self,
        pending: TargetCandidate,
    ) -> Result<PreparedProjectResult, crate::BuildError<crate::CompilationOutcome>> {
        let TargetCandidate {
            compilation,
            compile_ms,
            generation,
        } = pending;
        let outcome = compilation.outcome().clone();
        let completion = match self.close_publisher_generation(generation) {
            Ok(completion) => completion,
            Err(message) => {
                return Err(crate::BuildError::new(
                    crate::BuildOperation::Prepare,
                    crate::BuildErrorPhase::Preparation,
                    crate::BuildErrorCode::RendererFailed,
                    message,
                )
                .with_successful_compilation(outcome));
            }
        };
        Ok(self.commit_success(compilation, completion, compile_ms))
    }

    fn prepare_cycle(
        &mut self,
        options: &PrepareGuideOptions,
        environment: &PackageEnvironment,
        compilation: Option<&crate::compilation::CompilationCandidate>,
    ) -> Result<PrepareResult, String> {
        let total_started = self.timer();
        let operation = "prepare(cycle)";
        let mut metrics = PrepareMeasurements::default();
        let project = compilation
            .map(crate::compilation::CompilationCandidate::project)
            .or_else(|| self.project_revision())
            .cloned()
            .ok_or_else(|| {
                format!(
                    "{operation}: compileProject has not established a complete source revision"
                )
            })?;
        let compiled = compilation
            .map(crate::compilation::CompilationCandidate::compiled_resources)
            .unwrap_or_else(|| self.compiled_resources());
        let (project_id, fhir_version) = compiled_ig_identity(compiled)?;
        let (project_revision, project_objects) =
            site_build_project_revision(&project, &project_id)?;
        let package_lock = site_build_package_lock(&project, environment)?;
        let keys = self.preparation_cache_keys(
            options,
            &project_revision,
            &package_lock,
            compiled,
            operation,
        )?;
        let diagnostics = site_build_diagnostics(
            compilation
                .map(crate::compilation::CompilationCandidate::diagnostics)
                .unwrap_or_else(|| self.compile_diagnostics()),
        );
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
        let closed_key = cycle_runtime_key(&keys.prepared_guide, &render_target, &diagnostics)
            .map_err(|error| format!("{operation}: cache key: {error}"))?;
        if let Some((handle, build)) = self.find_cycle(&closed_key) {
            metrics.site_build_cache_hit = true;
            self.commit_cycle_reuse(&handle);
            metrics.total_ms = elapsed_ms(total_started);
            return Ok(PrepareResult {
                build_id: handle,
                generator: GeneratorKind::Cycle,
                site_build: build,
                measurements: metrics,
            });
        }
        let prepared_stage = self.site_model_from_compile_candidate(
            options,
            environment,
            operation,
            &mut metrics,
            compilation,
        )?;
        let prepared = prepared_stage.guide.clone();
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
        let handle = projection.site_build.site_build().build_id().to_string();
        let mut objects = environment_objects(
            &project_objects,
            projection.site_build.site_build().package_lock(),
            environment,
            operation,
        )?;
        merge_objects(&mut objects, projection.objects.clone())?;
        #[cfg(test)]
        if self.fail_during_cycle_close {
            return Err("prepareProject: injected failure during Cycle close".into());
        }
        self.install_cycle(projection.site_build.clone(), objects, Some(closed_key));
        let _ = self.commit_prepared_guide_stage(prepared_stage, &mut metrics);
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
const PAGE_XML_SHIM: &str = "---\r\n---\r\n{% include template-page.html %}";
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
    // `ContentRef::of_bytes` authenticated this exact owned allocation. Do not
    // hash it a second time through the trust-boundary `insert_object` helper.
    insert_authenticated_object(objects, &content, Rc::new(bytes))?;
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

#[allow(clippy::too_many_arguments)]
fn insert_prepared_output_content(
    outputs: &mut BTreeMap<site_build::OutputPath, PreparedOutput>,
    objects: &ObjectMap,
    path: &str,
    content: site_build::ContentRef,
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
    objects
        .get(&content.sha256)
        .ok_or_else(|| format!("authenticated object {} is absent", content.sha256))?
        .authenticates(&content)?;
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
            } else if let Some(name) = path.strip_suffix(".xml") {
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

/// Reuse the project's already authenticated source object when an authored
/// Publisher input is the exact captured source bytes. The prepared-guide
/// augmentation contract normally gives every authored file one source read;
/// hand-built/derived inputs fall back to ordinary local admission.
fn authenticated_authored_content(
    project: &site_build::ProjectIdentity,
    objects: &ObjectMap,
    file: &site_build::AuthoredFile,
    operation: &str,
) -> Result<Option<site_build::ContentRef>, String> {
    let mut reads = file.source_reads.iter();
    let Some(read) = reads.next() else {
        return Ok(None);
    };
    if reads.next().is_some() {
        return Ok(None);
    }
    let path = site_build::SourcePath::parse(read.as_str().to_string())
        .map_err(|error| format!("{operation}: authored source {read}: {error}"))?;
    let Some(source) = project.sources.get(&path) else {
        return Ok(None);
    };
    let Some(object) = objects.get(&source.content.sha256) else {
        return Ok(None);
    };
    object.authenticates(&source.content)?;
    let bytes = object.materialize()?;
    if bytes.as_slice() != file.content.as_slice() {
        return Ok(None);
    }
    Ok(Some(site_build::ContentRef {
        sha256: source.content.sha256.clone(),
        byte_length: source.content.byte_length,
        media_type: Some(file.mime.clone()),
    }))
}

fn stage_prepared_authored_files(
    prepared: &site_build::PreparedGuide,
    project: &site_build::ProjectIdentity,
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
            site_build::AuthoredFileRole::Image => {
                let producer = site_build::OutputProducer {
                    id: "publisher-authored-asset".into(),
                    version: project_id.into(),
                };
                if let Some(content) =
                    authenticated_authored_content(project, objects, file, "prepare(publisher)")?
                {
                    insert_prepared_output_content(
                        outputs,
                        objects,
                        path,
                        content,
                        producer,
                        Some(source),
                        PreparedOutputCollision::AuthoredOverridesRenderer,
                    )?;
                } else {
                    insert_prepared_output(
                        outputs,
                        objects,
                        path,
                        file.content.clone(),
                        &file.mime,
                        producer,
                        Some(source),
                        PreparedOutputCollision::AuthoredOverridesRenderer,
                    )?;
                }
            }
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
                let (name, shell) = if let Some(name) = path.strip_suffix(".md") {
                    (name, PAGE_MD_SHIM)
                } else if let Some(name) = path.strip_suffix(".xml") {
                    (name, PAGE_XML_SHIM)
                } else {
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
                        shell.as_bytes().to_vec(),
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

fn publisher_output_plan(
    ready: &BTreeMap<site_build::OutputPath, PreparedOutput>,
    pages: Vec<String>,
    resource_pages: &BTreeMap<String, site_producer::ResourcePageMetadata>,
    operation: &str,
) -> Result<MaterializedPublisherCatalog, String> {
    let pages = pages
        .into_iter()
        .map(|page| {
            let metadata = resource_pages.get(&page);
            let path = site_build::OutputPath::parse(page.clone())
                .map_err(|error| format!("{operation}: invalid page {page}: {error}"))?;
            Ok(OutputDescriptor {
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
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let static_originals = ready
        .keys()
        .filter(|path| is_static_output(path.as_str()))
        .cloned()
        .collect();
    MaterializedPublisherCatalog::new(ready, pages, static_originals, operation)
}

struct PublisherModel {
    site_files: Rc<HashMap<PathBuf, Vec<u8>>>,
    resource_pages: BTreeMap<String, site_producer::ResourcePageMetadata>,
}

fn insert_site_file(
    files: &mut HashMap<PathBuf, Vec<u8>>,
    path: PathBuf,
    bytes: Vec<u8>,
    owner: &str,
) -> Result<(), String> {
    match files.get(&path) {
        None => {
            files.insert(path, bytes);
            Ok(())
        }
        Some(existing) if existing == &bytes => Ok(()),
        Some(_) => Err(format!(
            "prepare(publisher): {owner} collides with an existing mounted file at {}",
            path.display()
        )),
    }
}

fn expand_sql_sources(
    prepared: &site_build::PreparedGuide,
    site_files: &mut HashMap<PathBuf, Vec<u8>>,
) -> Result<(), String> {
    let sources = site_files
        .iter()
        .filter(|(path, _)| {
            matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("html" | "md" | "xml")
            )
        })
        .map(|(path, bytes)| (path.to_string_lossy().into_owned(), bytes.clone()))
        .collect::<BTreeMap<_, _>>();
    let has_directive = sources.values().any(|bytes| {
        bytes
            .windows(b"{% sql".len())
            .any(|window| window == b"{% sql")
            || bytes
                .windows(b"{%! sql".len())
                .any(|window| window == b"{%! sql")
    });
    if !has_directive {
        return Ok(());
    }
    let runtime = publisher_sql::SqlRuntime::from_resources(
        prepared.resources.iter().map(|resource| &resource.resource),
    );
    let expansion = publisher_sql::expand_publisher_sql(&runtime, sources);
    for (path, bytes) in expansion.rewritten_sources {
        let path = PathBuf::from(path);
        let Some(current) = site_files.get_mut(&path) else {
            return Err(format!(
                "prepare(publisher): SQL expansion rewrote absent source {}",
                path.display()
            ));
        };
        *current = bytes;
    }
    for (name, bytes) in expansion.generated_includes {
        insert_site_file(
            site_files,
            PathBuf::from(format!("/site/_includes/{name}")),
            bytes,
            "generated SQL include",
        )?;
    }
    for (name, bytes) in expansion.generated_data {
        insert_site_file(
            site_files,
            PathBuf::from(format!("/site/_data/{name}.json")),
            bytes,
            "generated sqlToData file",
        )?;
    }
    Ok(())
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

impl SiteEngine {
    fn publisher_model(
        &self,
        prepared: &site_build::PreparedGuide,
        project: &site_build::ProjectIdentity,
        template_files: &BTreeMap<String, Vec<u8>>,
        ready: &mut BTreeMap<site_build::OutputPath, PreparedOutput>,
        objects: &mut ObjectMap,
        project_id: &str,
        operation: &str,
    ) -> Result<PublisherModel, String> {
        let config_json: Value =
            serde_json::from_slice(template_files.get("config.json").ok_or_else(|| {
                format!("{operation}: template artifacts contain no config.json")
            })?)
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

        let site_producer::SiteProducerOutput {
            files: produced_files,
            public_outputs,
            resource_pages,
        } = produced;
        for (path, output) in public_outputs {
            insert_prepared_output(
                ready,
                objects,
                &path,
                output.bytes,
                &output.media_type,
                site_build::OutputProducer {
                    id: "publisher-resource-format".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
                Some(output.source),
                PreparedOutputCollision::Reject,
            )?;
        }
        let mut site_files = HashMap::new();
        for (relative, bytes) in template_files {
            let mounted = match relative.strip_prefix("includes/") {
                Some(name) => format!("_includes/{name}"),
                None => format!("template/{relative}"),
            };
            insert_site_file(
                &mut site_files,
                PathBuf::from(format!("/site/{mounted}")),
                bytes.clone(),
                "template file",
            )?;
        }
        for (relative, bytes) in produced_files {
            insert_site_file(
                &mut site_files,
                PathBuf::from(format!("/site/{relative}")),
                bytes,
                "site producer output",
            )?;
        }
        stage_prepared_authored_files(
            prepared,
            project,
            &mut site_files,
            ready,
            objects,
            project_id,
        )?;

        expand_sql_sources(prepared, &mut site_files)?;
        Ok(PublisherModel {
            site_files: Rc::new(site_files),
            resource_pages,
        })
    }
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

fn publisher_render_semantics_reusing_package_catalog(
    render_set: Vec<(PathBuf, serde_json::Value)>,
    packages: PackageView,
    model: &PublisherModel,
    options: &SiteOptions,
    package_catalog_key: site_build::Sha256Digest,
    reusable: Option<&RenderPackageCatalog>,
) -> Result<(RenderSemantics, Rc<RenderPackageCatalog>, bool), String> {
    build_render_semantics_reusing_package_catalog(
        render_set,
        Some(packages),
        &model.site_files,
        options,
        package_catalog_key,
        reusable,
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
    prepared_guide_key: &site_build::Sha256Digest,
    model: &PublisherModel,
    options: &SiteOptions,
    build_epoch_secs: i64,
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
        prepared_guide_key,
        active_tables: options.active_tables,
        run_uuid: options.run_uuid.as_deref(),
        build_epoch_secs,
        txcache,
    })
    .map_err(|error| format!("{operation}: hash Publisher render semantics key: {error}"))
}

fn publisher_render_package_catalog_cache_key(
    prepared: &site_build::PreparedGuide,
    package_lock: &site_build::PackageLock,
    resolved_labels: &[String],
    package_root: &str,
    operation: &str,
) -> Result<site_build::Sha256Digest, String> {
    let implementation_guide = prepared
        .resources
        .iter()
        .find(|resource| resource.key == prepared.guide.implementation_guide)
        .ok_or_else(|| format!("{operation}: PreparedGuide has no primary ImplementationGuide"))?;
    let fhir_version = implementation_guide
        .resource
        .get("fhirVersion")
        .and_then(Value::as_array)
        .and_then(|versions| versions.first())
        .and_then(Value::as_str);
    let dependencies = implementation_guide
        .resource
        .get("dependsOn")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|dependency| {
            Some(PublisherRenderPackageSelection {
                package_id: dependency.get("packageId")?.as_str()?,
                version: dependency.get("version")?.as_str()?,
            })
        })
        .collect();
    site_build::sha256_canonical(&PublisherRenderPackageCatalogCachePayload {
        schema: PUBLISHER_RENDER_PACKAGE_CATALOG_CACHE_SCHEMA,
        engine_api: ENGINE_API,
        primary_implementation_guide: &implementation_guide.key,
        fhir_version,
        dependencies,
        package_lock,
        resolved_labels,
        package_root,
    })
    .map_err(|error| format!("{operation}: hash Publisher render package catalog key: {error}"))
}

#[cfg(test)]
fn publisher_render_package_catalog_admitted(
    packages: usize,
    resource_entries: usize,
    retained_approx_bytes: usize,
) -> bool {
    publisher_render_package_catalog_admitted_with_limits(
        packages,
        resource_entries,
        retained_approx_bytes,
        (
            PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_PACKAGES,
            PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_RESOURCE_ENTRIES,
            PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_APPROX_BYTES,
        ),
    )
}

fn publisher_render_package_catalog_admitted_with_limits(
    packages: usize,
    resource_entries: usize,
    retained_approx_bytes: usize,
    limits: (usize, usize, usize),
) -> bool {
    packages <= limits.0 && resource_entries <= limits.1 && retained_approx_bytes <= limits.2
}

fn publisher_mounted_tree_digest(
    model: &PublisherModel,
    operation: &str,
) -> Result<site_build::Sha256Digest, String> {
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
    site_build::sha256_canonical(&tree_manifest)
        .map_err(|error| format!("{operation}: hash mounted tree: {error}"))
}

fn publisher_artifacts(
    prepared: &site_build::PreparedGuide,
    project: &site_build::ProjectIdentity,
    package_lock: &site_build::PackageLock,
    recipe_assets: &PublisherRecipeAssets,
    authenticated_objects: &ObjectMap,
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
        if let Some(content) = authenticated_authored_content(
            project,
            authenticated_objects,
            file,
            "prepare(publisher)",
        )? {
            push_artifact_content(
                &mut records,
                &mut roots,
                key,
                content,
                "prepared-guide",
                "publisher.authored-file/v1",
                BTreeMap::from([("role".into(), authored_role_name(file.role.clone()).into())]),
                reads,
            );
        } else {
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
    // The digest was just computed from this exact owned allocation. Retain
    // that proof directly instead of routing it through the external-object
    // verifier and hashing the bytes twice.
    insert_authenticated_object(objects, &content, Rc::new(bytes))?;
    push_artifact_content(
        records, roots, key, content, producer, recipe, attributes, reads,
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn push_artifact_content(
    records: &mut Vec<site_build::ArtifactRecord>,
    roots: &mut BTreeSet<site_build::ArtifactKey>,
    key: site_build::ArtifactKey,
    content: site_build::ContentRef,
    producer: &str,
    recipe: &str,
    attributes: BTreeMap<String, String>,
    reads: BTreeSet<site_build::ReadDependency>,
) {
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
    let mut carrier_identities = BTreeMap::new();
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
        carrier_identities.insert(label.clone(), locked.content.clone());
    }
    let root = source.cache_root().to_path_buf();
    let packages = PackageView::new(Rc::new(source), root, Some(expected))
        .with_carrier_identities(carrier_identities);
    publisher_render_package_view(packages, labels, operation)
}

fn publisher_render_package_view(
    packages: PackageView,
    labels: &[String],
    operation: &str,
) -> Result<PackageView, String> {
    let mut direct_file_directories = BTreeSet::new();
    let mut additional_files = BTreeSet::new();
    for label in labels {
        let package_root = packages.root().join(label);
        let semantic_root = package_root.join("package");
        if !packages.is_dir(&semantic_root) {
            return Err(format!(
                "{operation}: list semantic package {label}: no such directory"
            ));
        }
        direct_file_directories.insert(semantic_root);
        let normalized_path = package_root.join("package/other/spec.internals");
        let native_path = package_root.join("other/spec.internals");
        if packages.exists(&normalized_path) {
            additional_files.insert(normalized_path);
        } else if packages.exists(&native_path) {
            additional_files.insert(native_path);
        }
    }
    // Both live and restored execution receive the same declarative projection
    // of the rooted carrier: files directly under each selected package plus
    // the one selected spec.internals. Do not materialize and then revalidate a
    // duplicate set of every package member merely to express this file shape.
    packages.restricted_to_direct_files(direct_file_directories, additional_files)
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
    fn authored_xml_page_projects_to_include_shell_and_public_output() {
        let file = site_build::AuthoredFile {
            role: site_build::AuthoredFileRole::PageContent,
            path: site_build::PreparedPath::parse("project.xml").unwrap(),
            mime: "application/xml".into(),
            content: br#"<div xmlns="http://www.w3.org/1999/xhtml">Project</div>"#.to_vec(),
            source_reads: BTreeSet::from([site_build::PreparedPath::parse(
                "input/pagecontent/project.xml",
            )
            .unwrap()]),
        };

        assert_eq!(
            authored_projection_targets(&file).unwrap(),
            vec![
                "tree:/site/_includes/en/project.xml",
                "tree:/site/_includes/project.xml",
                "tree:/site/en/project.html",
                "output:en/project.html",
            ]
        );
    }

    #[test]
    fn authored_content_reuses_only_the_exact_authenticated_project_source() {
        let bytes = b"guide image".to_vec();
        let source_path = site_build::SourcePath::parse("input/images/guide.png").unwrap();
        let source_content =
            site_build::ContentRef::of_bytes(&bytes, Some("image/png".to_string()));
        let project = site_build::ProjectIdentity {
            project_id: "demo.ig".into(),
            revision: "sources-sha256:test".into(),
            sources: site_build::SourceManifest::from_entries([(
                source_path,
                site_build::SourceEntry {
                    kind: site_build::SourceKind::Asset,
                    content: source_content.clone(),
                },
            )])
            .unwrap(),
        };
        let mut objects = ObjectMap::new();
        insert_object(&mut objects, &source_content, bytes.clone()).unwrap();
        let mut file = site_build::AuthoredFile {
            role: site_build::AuthoredFileRole::Image,
            path: site_build::PreparedPath::parse("guide.png").unwrap(),
            mime: "image/png".into(),
            content: bytes,
            source_reads: BTreeSet::from([site_build::PreparedPath::parse(
                "input/images/guide.png",
            )
            .unwrap()]),
        };

        let reused = authenticated_authored_content(&project, &objects, &file, "test")
            .unwrap()
            .expect("exact source should be reused");
        assert_eq!(reused.sha256, source_content.sha256);
        assert_eq!(reused.byte_length, source_content.byte_length);
        assert_eq!(reused.media_type.as_deref(), Some("image/png"));

        file.content[0] ^= 1;
        assert!(
            authenticated_authored_content(&project, &objects, &file, "test")
                .unwrap()
                .is_none()
        );
    }

    fn snapshot_derivation_fixture(
        child_min: u64,
        local_base_min: u64,
        broken: bool,
    ) -> (
        crate::compilation::CompiledProjectRevision,
        Vec<(PathBuf, Value)>,
        PackageEnvironment,
    ) {
        let core_label = "hl7.fhir.r5.core#5.0.0";
        let core = package_store::PreparedPackage::prepare(
            core_label,
            BTreeMap::from([(
                "package.json".into(),
                br#"{"name":"hl7.fhir.r5.core","version":"5.0.0","fhirVersions":["5.0.0"]}"#
                    .to_vec(),
            )]),
        )
        .unwrap();
        let environment = PackageEnvironment::new([core]).unwrap();
        let config = "id: demo.incremental\ncanonical: https://example.org/incremental\nname: IncrementalDemo\nstatus: draft\nversion: 1.0.0\nfhirVersion: 5.0.0\n";
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
                site_files: BTreeMap::new(),
            },
            resolved,
        )
        .unwrap();
        let ig = serde_json::json!({
            "resourceType":"ImplementationGuide",
            "id":"demo-incremental",
            "packageId":"demo.incremental",
            "url":"https://example.org/incremental/ImplementationGuide/demo-incremental",
            "name":"IncrementalDemo",
            "status":"draft",
            "version":"1.0.0",
            "fhirVersion":["5.0.0"],
            "definition":{}
        });
        let profile = |id: &str, base: &str, min: u64| {
            serde_json::json!({
                "resourceType":"StructureDefinition",
                "id":id,
                "url":format!("https://example.org/StructureDefinition/{id}"),
                "version":"1.0.0",
                "name":id,
                "status":"draft",
                "fhirVersion":"5.0.0",
                "kind":"complex-type",
                "abstract":false,
                "type":"Base",
                "baseDefinition":base,
                "derivation":"constraint",
                "differential":{"element":[{
                    "id":"Base",
                    "path":"Base",
                    "min":min,
                    "max":"*"
                }]}
            })
        };
        let mut compiled = vec![
            (
                PathBuf::from("/__ig__/ImplementationGuide-demo-incremental.json"),
                ig,
            ),
            (
                PathBuf::from("/out/StructureDefinition-local-base.json"),
                profile(
                    "LocalBase",
                    "http://hl7.org/fhir/StructureDefinition/Base",
                    local_base_min,
                ),
            ),
            (
                PathBuf::from("/out/StructureDefinition-child.json"),
                profile(
                    "Child",
                    "https://example.org/StructureDefinition/LocalBase",
                    child_min,
                ),
            ),
            (
                PathBuf::from("/out/StructureDefinition-sibling.json"),
                profile("Sibling", "http://hl7.org/fhir/StructureDefinition/Base", 0),
            ),
        ];
        if broken {
            compiled.push((
                PathBuf::from("/out/StructureDefinition-z-broken.json"),
                profile(
                    "Broken",
                    "https://example.org/StructureDefinition/DoesNotExist",
                    0,
                ),
            ));
        }
        (project, compiled, environment)
    }

    fn cycle_spec() -> GeneratorSpec {
        GeneratorSpec::Cycle {
            build_epoch_secs: 1,
            liquid_asset_dirs: vec![],
            branch: None,
            revision: None,
        }
    }

    fn snapshot_derivation_state_fingerprint(engine: &SiteEngine) -> String {
        let generations = engine
            .preparation
            .history
            .iter()
            .map(|generation| {
                let resources = generation
                    .snapshots
                    .resources
                    .iter()
                    .map(|(path, derivation)| {
                        (
                            path.clone(),
                            Rc::as_ptr(&derivation.derivation) as usize,
                            derivation.retention,
                        )
                    })
                    .collect::<Vec<_>>();
                (
                    resources,
                    generation.snapshots.fact_count,
                    generation.snapshots.manifest_approx_bytes,
                    generation.snapshots.snapshot_json_bytes,
                )
            })
            .collect::<Vec<_>>();
        format!("{generations:?}")
    }

    fn current_snapshot_generation(engine: &SiteEngine) -> &SnapshotDerivationGeneration {
        &engine
            .preparation
            .history
            .current
            .as_ref()
            .expect("current preparation generation")
            .snapshots
    }

    fn promote_snapshot_candidate_for_test(
        engine: &mut SiteEngine,
        candidate: SnapshotDerivationCandidate,
        metrics: &mut PrepareMeasurements,
    ) {
        metrics.snapshot_derivation_admitted = candidate.admitted;
        engine.preparation.history.promote(PreparationGeneration {
            snapshots: candidate.generation,
            render_semantics: None,
            render_package_catalog: None,
        });
        record_snapshot_derivation_metrics(&engine.preparation, metrics);
    }

    #[test]
    fn compiled_revision_cache_identity_preserves_resource_and_json_order() {
        let one: Value = serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
        let reordered: Value = serde_json::from_str(r#"{"b":2,"a":1}"#).unwrap();
        let first = vec![
            (PathBuf::from("a.json"), one.clone()),
            (PathBuf::from("b.json"), serde_json::json!({"c":3})),
        ];
        let body_reordered = vec![
            (PathBuf::from("a.json"), reordered),
            (PathBuf::from("b.json"), serde_json::json!({"c":3})),
        ];
        let resources_reordered = vec![first[1].clone(), first[0].clone()];
        assert_ne!(
            compiled_revision_sha256(&first, "test").unwrap(),
            compiled_revision_sha256(&body_reordered, "test").unwrap()
        );
        assert_ne!(
            compiled_revision_sha256(&first, "test").unwrap(),
            compiled_revision_sha256(&resources_reordered, "test").unwrap()
        );
        assert!(compiled_revision_sha256(
            &[
                (PathBuf::from("a.json"), one.clone()),
                (PathBuf::from("a.json"), one),
            ],
            "test",
        )
        .unwrap_err()
        .contains("duplicate path"));
    }

    #[test]
    fn snapshot_derivations_reuse_independent_profiles_and_invalidate_descendants() {
        let (project_a, compiled_a, environment) = snapshot_derivation_fixture(0, 0, false);
        let mut retained = SiteEngine::default();
        retained.install_compilation_for_test(project_a.clone(), compiled_a.clone());
        let a = retained.prepare(cycle_spec(), environment.clone()).unwrap();
        assert_eq!(a.measurements.snapshot_resource_cache_hits, 0);
        assert_eq!(a.measurements.snapshot_resource_cache_misses, 3);
        assert!(a.measurements.snapshot_derivation_admitted);
        let sibling_a = current_snapshot_generation(&retained)
            .resources
            .iter()
            .find(|(path, _)| path.ends_with("StructureDefinition-sibling.json"))
            .map(|(_, derivation)| derivation.derivation.clone())
            .unwrap();

        let (project_b, compiled_b, _) = snapshot_derivation_fixture(1, 0, false);
        retained.install_compilation_for_test(project_b.clone(), compiled_b.clone());
        let b = retained.prepare(cycle_spec(), environment.clone()).unwrap();
        assert_eq!(b.measurements.snapshot_resource_cache_hits, 2);
        assert_eq!(b.measurements.snapshot_resource_cache_misses, 1);
        assert_eq!(b.measurements.snapshot_derivation_history_generations, 2);
        let sibling_b = current_snapshot_generation(&retained)
            .resources
            .iter()
            .find(|(path, _)| path.ends_with("StructureDefinition-sibling.json"))
            .map(|(_, derivation)| &derivation.derivation)
            .unwrap();
        assert!(Rc::ptr_eq(&sibling_a, sibling_b));

        let mut fresh_b = SiteEngine::default();
        fresh_b.install_compilation_for_test(project_b, compiled_b);
        let fresh_b = fresh_b.prepare(cycle_spec(), environment.clone()).unwrap();
        assert_eq!(
            site_build::canonical_json_bytes(&b.site_build).unwrap(),
            site_build::canonical_json_bytes(&fresh_b.site_build).unwrap()
        );

        // A and B are the bounded runtime history. Returning directly to A
        // should reuse that closed build instead of reopening snapshot work.
        retained.install_compilation_for_test(project_a, compiled_a);
        let return_a = retained.prepare(cycle_spec(), environment.clone()).unwrap();
        assert!(return_a.measurements.site_build_cache_hit);
        assert_eq!(return_a.measurements.snapshot_resource_cache_hits, 0);
        assert_eq!(return_a.measurements.snapshot_resource_cache_misses, 0);

        let (project_base, mut compiled_base, _) = snapshot_derivation_fixture(1, 1, false);
        compiled_base.reverse();
        retained.install_compilation_for_test(project_base.clone(), compiled_base.clone());
        let base_changed = retained.prepare(cycle_spec(), environment.clone()).unwrap();
        assert_eq!(base_changed.measurements.snapshot_resource_cache_hits, 1);
        assert_eq!(base_changed.measurements.snapshot_resource_cache_misses, 2);
        let mut fresh_base = SiteEngine::default();
        fresh_base.install_compilation_for_test(project_base, compiled_base);
        let fresh_base = fresh_base
            .prepare(cycle_spec(), environment.clone())
            .unwrap();
        assert_eq!(
            site_build::canonical_json_bytes(&base_changed.site_build).unwrap(),
            site_build::canonical_json_bytes(&fresh_base.site_build).unwrap()
        );
    }

    #[test]
    fn released_superseded_runtime_preserves_published_predecessor() {
        let (project_a, compiled_a, environment) = snapshot_derivation_fixture(0, 0, false);
        let mut engine = SiteEngine::default();
        engine.install_compilation_for_test(project_a, compiled_a);
        let a = engine.prepare(cycle_spec(), environment.clone()).unwrap();

        let (project_b, compiled_b, _) = snapshot_derivation_fixture(1, 0, false);
        engine.install_compilation_for_test(project_b, compiled_b);
        let b = engine.prepare(cycle_spec(), environment.clone()).unwrap();
        assert_eq!(
            engine.retained_generation_handles(),
            vec![b.build_id.clone(), a.build_id.clone()]
        );

        assert!(engine.release(&b.build_id));
        assert!(!engine.release(&b.build_id));
        assert_eq!(
            engine.retained_generation_handles(),
            vec![a.build_id.clone()]
        );

        let (project_c, mut compiled_c, _) = snapshot_derivation_fixture(1, 1, false);
        compiled_c.reverse();
        engine.install_compilation_for_test(project_c, compiled_c);
        let c = engine.prepare(cycle_spec(), environment).unwrap();
        assert_eq!(
            engine.retained_generation_handles(),
            vec![c.build_id, a.build_id]
        );
    }

    #[test]
    fn snapshot_derivation_reuses_structure_and_recomposes_current_metadata() {
        let (project_a, compiled_a, environment) = snapshot_derivation_fixture(0, 0, false);
        let mut retained = SiteEngine::default();
        retained.install_compilation_for_test(project_a, compiled_a);
        retained.prepare(cycle_spec(), environment.clone()).unwrap();
        let child_snapshot_a = current_snapshot_generation(&retained)
            .resources
            .iter()
            .find(|(path, _)| path.ends_with("StructureDefinition-child.json"))
            .map(|(_, derivation)| derivation.derivation.clone())
            .unwrap();

        let (project_b, mut compiled_b, _) = snapshot_derivation_fixture(0, 0, false);
        let child_b = compiled_b
            .iter_mut()
            .find(|(_, resource)| resource.get("id").and_then(Value::as_str) == Some("Child"))
            .map(|(_, resource)| resource)
            .unwrap();
        child_b["title"] = Value::String("Current child title".into());
        child_b["status"] = Value::String("active".into());
        child_b["description"] = Value::String("Current child description".into());
        child_b["publisher"] = Value::String("Current publisher".into());

        retained.install_compilation_for_test(project_b.clone(), compiled_b.clone());
        let current = retained.prepare(cycle_spec(), environment.clone()).unwrap();
        assert_eq!(current.measurements.snapshot_resource_cache_hits, 3);
        assert_eq!(current.measurements.snapshot_resource_cache_misses, 0);
        let child_snapshot_b = current_snapshot_generation(&retained)
            .resources
            .iter()
            .find(|(path, _)| path.ends_with("StructureDefinition-child.json"))
            .map(|(_, derivation)| derivation.derivation.clone())
            .unwrap();
        assert!(Rc::ptr_eq(&child_snapshot_a, &child_snapshot_b));

        let prepared_child = |engine: &mut SiteEngine, environment: &PackageEnvironment| {
            let mut metrics = PrepareMeasurements::default();
            engine
                .site_model_from_compile_candidate(
                    &PrepareGuideOptions {
                        build_epoch_secs: 1,
                        liquid_asset_dirs: Vec::new(),
                        branch: None,
                        revision: None,
                    },
                    environment,
                    "test",
                    &mut metrics,
                    None,
                )
                .unwrap()
                .guide
                .resources
                .iter()
                .find(|resource| {
                    resource.resource.get("id").and_then(Value::as_str) == Some("Child")
                })
                .map(|resource| resource.resource.clone())
                .unwrap()
        };
        let current_child = prepared_child(&mut retained, &environment);
        assert_eq!(
            current_child.get("title").and_then(Value::as_str),
            Some("Current child title")
        );
        assert_eq!(
            current_child.get("description").and_then(Value::as_str),
            Some("Current child description")
        );

        let mut fresh = SiteEngine::default();
        fresh.install_compilation_for_test(project_b, compiled_b);
        fresh.prepare(cycle_spec(), environment.clone()).unwrap();
        let fresh_child = prepared_child(&mut fresh, &environment);
        assert_eq!(
            serde_json::to_vec(&current_child).unwrap(),
            serde_json::to_vec(&fresh_child).unwrap(),
            "retained and fresh resources must match in values and key order"
        );
    }

    #[test]
    fn empty_and_over_budget_successors_age_out_older_snapshot_derivations() {
        let (project, compiled, environment) = snapshot_derivation_fixture(0, 0, false);
        let mut engine = SiteEngine::default();
        engine.install_compilation_for_test(project, compiled);
        engine.prepare(cycle_spec(), environment).unwrap();
        let retained_resources = current_snapshot_generation(&engine).resources.clone();
        assert!(!retained_resources.is_empty());

        let overflow = snapshot_derivation_candidate(
            retained_resources,
            SNAPSHOT_DERIVATION_MAX_FACTS + 1,
            0,
            0,
            true,
        );
        assert!(!overflow.admitted);
        assert!(overflow.generation.resources.is_empty());
        assert_eq!(overflow.generation.fact_count, 0);
        let mut overflow_metrics = PrepareMeasurements::default();
        promote_snapshot_candidate_for_test(&mut engine, overflow, &mut overflow_metrics);
        assert_eq!(engine.preparation.history.len(), 2);
        assert!(!overflow_metrics.snapshot_derivation_admitted);

        let empty = snapshot_derivation_candidate(BTreeMap::new(), 0, 0, 0, true);
        assert!(!empty.admitted);
        let mut empty_metrics = PrepareMeasurements::default();
        promote_snapshot_candidate_for_test(&mut engine, empty, &mut empty_metrics);
        assert_eq!(engine.preparation.history.len(), 2);
        assert!(engine
            .preparation
            .history
            .iter()
            .all(|generation| generation.snapshots.resources.is_empty()));
        assert_eq!(empty_metrics.snapshot_derivation_history_generations, 2);
        assert_eq!(empty_metrics.snapshot_derivation_retained_facts, 0);
        assert!(!empty_metrics.snapshot_derivation_admitted);
    }

    #[test]
    fn render_package_catalog_admission_is_bounded_and_tombstones_age_history() {
        assert!(publisher_render_package_catalog_admitted(
            PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_PACKAGES,
            PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_RESOURCE_ENTRIES,
            PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_APPROX_BYTES,
        ));
        assert!(!publisher_render_package_catalog_admitted(
            PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_PACKAGES + 1,
            0,
            0,
        ));
        assert!(!publisher_render_package_catalog_admitted(
            0,
            PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_RESOURCE_ENTRIES + 1,
            0,
        ));
        assert!(!publisher_render_package_catalog_admitted(
            0,
            0,
            PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_APPROX_BYTES + 1,
        ));

        let tombstone = |body: &[u8]| PublisherRenderPackageCatalogCacheEntry {
            key: site_build::Sha256Digest::of_bytes(body),
            catalog: None,
        };
        let mut history = crate::History2::default();
        let a = site_build::Sha256Digest::of_bytes(b"a");
        for entry in [tombstone(b"a"), tombstone(b"b"), tombstone(b"c")] {
            history.promote(PreparationGeneration {
                snapshots: Default::default(),
                render_semantics: None,
                render_package_catalog: Some(entry),
            });
        }
        assert_eq!(history.len(), 2);
        assert!(history.iter().all(|generation| generation
            .render_package_catalog
            .as_ref()
            .is_some_and(|entry| entry.catalog.is_none())));
        assert!(history.iter().all(|generation| generation
            .render_package_catalog
            .as_ref()
            .is_some_and(|entry| entry.key != a)));
    }

    #[test]
    fn public_prepare_project_preserves_snapshot_history_across_semantic_compile() {
        #[derive(Clone)]
        struct Source(crate::ProjectRevision);
        impl crate::ProjectSource for Source {
            fn config(&mut self) -> Result<String, String> {
                Ok(self.0.config.clone())
            }

            fn capture(
                &mut self,
                _packages: &PackageEnvironment,
                _resolved: &crate::ResolvedPackageClosure,
            ) -> Result<crate::ProjectRevision, String> {
                Ok(self.0.clone())
            }
        }

        #[derive(Clone)]
        struct Packages {
            resolved: crate::ResolvedPackageClosure,
            environment: PackageEnvironment,
        }
        impl crate::PackageProvider for Packages {
            fn resolve(
                &mut self,
                config: &str,
                _generator: &GeneratorSpec,
            ) -> Result<crate::ResolvedPackageClosure, String> {
                if site_build::Sha256Digest::of_bytes(config.as_bytes())
                    != self.resolved.config_sha256
                {
                    return Err("test package closure belongs to different config".into());
                }
                Ok(self.resolved.clone())
            }

            fn environment(
                &mut self,
                resolved: &crate::ResolvedPackageClosure,
            ) -> Result<PackageEnvironment, String> {
                if resolved != &self.resolved {
                    return Err("test package provider received a different closure".into());
                }
                Ok(self.environment.clone())
            }
        }

        fn captured_revision(
            compiled_project: &crate::compilation::CompiledProjectRevision,
            compiled: &[(PathBuf, Value)],
        ) -> crate::ProjectRevision {
            let mut predefined = BTreeMap::new();
            let mut site_files = BTreeMap::new();
            for (_, body) in compiled {
                if body.get("resourceType").and_then(Value::as_str) != Some("StructureDefinition") {
                    continue;
                }
                let id = body.get("id").and_then(Value::as_str).unwrap();
                let path = format!("input/resources/StructureDefinition-{id}.json");
                let bytes = serde_json::to_vec(body).unwrap();
                predefined.insert(path.clone(), body.clone());
                site_files.insert(path, bytes);
            }
            crate::ProjectRevision {
                config: compiled_project.config().to_string(),
                fsh: BTreeMap::new(),
                predefined,
                site_files,
            }
        }

        fn prepare_metrics(result: &crate::PreparedProjectResult) -> &BTreeMap<String, f64> {
            result
                .events
                .iter()
                .find_map(|event| event.metrics.as_ref())
                .expect("prepare event metrics")
        }

        let (project_a, compiled_a, environment) = snapshot_derivation_fixture(0, 0, false);
        let resolved = project_a.resolved_packages().clone();
        let revision_a = captured_revision(&project_a, &compiled_a);
        let mut engine = SiteEngine::default();
        let mut source_a = Source(revision_a);
        let mut packages = Packages {
            resolved: resolved.clone(),
            environment: environment.clone(),
        };
        let a = engine
            .prepare_project(&mut source_a, &mut packages, cycle_spec())
            .unwrap();
        assert_eq!(prepare_metrics(&a)["snapshotResourceCacheHits"], 0.0);
        assert_eq!(prepare_metrics(&a)["snapshotResourceCacheMisses"], 3.0);

        let (project_b, compiled_b, _) = snapshot_derivation_fixture(1, 0, false);
        let revision_b = captured_revision(&project_b, &compiled_b);
        let mut source_b = Source(revision_b);
        let b = engine
            .prepare_project(&mut source_b, &mut packages, cycle_spec())
            .unwrap();
        assert_eq!(prepare_metrics(&b)["semanticCompilationCacheHit"], 0.0);
        assert_eq!(prepare_metrics(&b)["snapshotResourceCacheHits"], 2.0);
        assert_eq!(prepare_metrics(&b)["snapshotResourceCacheMisses"], 1.0);
        assert_eq!(
            prepare_metrics(&b)["snapshotDerivationHistoryGenerations"],
            2.0
        );

        let compilation_before = engine.compilation_state_fingerprint();
        let preparation_before = (
            snapshot_derivation_state_fingerprint(&engine),
            engine.retained_generation_handles(),
        );
        let (project_c, compiled_c, _) = snapshot_derivation_fixture(1, 1, false);
        let revision_c = captured_revision(&project_c, &compiled_c);
        engine.fail_during_cycle_close = true;
        let error = engine
            .prepare_project(&mut Source(revision_c.clone()), &mut packages, cycle_spec())
            .unwrap_err();
        assert!(
            error
                .message
                .contains("injected failure during Cycle close"),
            "{}",
            error.message
        );
        engine.fail_during_cycle_close = false;
        assert_eq!(engine.compilation_state_fingerprint(), compilation_before);
        assert_eq!(
            (
                snapshot_derivation_state_fingerprint(&engine),
                engine.retained_generation_handles(),
            ),
            preparation_before
        );

        let retry = engine
            .prepare_project(&mut Source(revision_c.clone()), &mut packages, cycle_spec())
            .unwrap();
        let mut fresh = SiteEngine::default();
        let mut fresh_packages = packages.clone();
        let fresh = fresh
            .prepare_project(&mut Source(revision_c), &mut fresh_packages, cycle_spec())
            .unwrap();
        assert_eq!(
            site_build::canonical_json_bytes(&retry.site.site_build).unwrap(),
            site_build::canonical_json_bytes(&fresh.site.site_build).unwrap()
        );
    }

    #[test]
    fn failed_snapshot_successor_promotes_no_incremental_state() {
        let (project_a, compiled_a, environment) = snapshot_derivation_fixture(0, 0, false);
        let mut engine = SiteEngine::default();
        engine.install_compilation_for_test(project_a, compiled_a);
        engine.prepare(cycle_spec(), environment.clone()).unwrap();
        let history_before = engine.preparation.history.len();
        let retained_before = snapshot_derivation_state_fingerprint(&engine);

        let (project_b, compiled_b, _) = snapshot_derivation_fixture(1, 0, true);
        engine.install_compilation_for_test(project_b, compiled_b);
        let error = engine.prepare(cycle_spec(), environment).unwrap_err();
        assert!(error.contains("DoesNotExist"), "{error}");
        assert_eq!(engine.preparation.history.len(), history_before);
        let retained_after = snapshot_derivation_state_fingerprint(&engine);
        assert_eq!(retained_after, retained_before);
    }

    #[test]
    fn late_cycle_failure_promotes_no_preparation_state_and_retry_matches_fresh() {
        let (project_a, compiled_a, environment) = snapshot_derivation_fixture(0, 0, false);
        let mut engine = SiteEngine::default();
        engine.install_compilation_for_test(project_a, compiled_a);
        engine.prepare(cycle_spec(), environment.clone()).unwrap();
        let history_before = snapshot_derivation_state_fingerprint(&engine);
        let runtimes_before = engine.retained_generation_handles();

        let (project_b, compiled_b, _) = snapshot_derivation_fixture(1, 0, false);
        engine.install_compilation_for_test(project_b.clone(), compiled_b.clone());
        engine.fail_during_cycle_close = true;
        let error = engine
            .prepare(cycle_spec(), environment.clone())
            .unwrap_err();
        assert!(
            error.contains("injected failure during Cycle close"),
            "{error}"
        );
        engine.fail_during_cycle_close = false;
        assert_eq!(
            snapshot_derivation_state_fingerprint(&engine),
            history_before
        );
        assert_eq!(engine.retained_generation_handles(), runtimes_before);

        let retry = engine.prepare(cycle_spec(), environment.clone()).unwrap();
        let mut fresh = SiteEngine::default();
        fresh.install_compilation_for_test(project_b, compiled_b);
        let fresh = fresh.prepare(cycle_spec(), environment).unwrap();
        assert_eq!(
            site_build::canonical_json_bytes(&retry.site_build).unwrap(),
            site_build::canonical_json_bytes(&fresh.site_build).unwrap()
        );
    }

    #[test]
    fn prepare_event_reports_compiler_package_store_reuse_metrics() {
        let compilation = crate::compilation::CompilationMeasurements {
            semantic_compilation_cache_hit: false,
            package_store_cache_hit: true,
            package_store_used: true,
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
    fn package_environment_successor_is_ordered_idempotent_and_atomic() {
        let package = |label: &str, marker: &[u8]| {
            let (name, version) = label.split_once('#').unwrap();
            package_store::PreparedPackage::prepare(
                label,
                BTreeMap::from([
                    (
                        "package.json".into(),
                        format!(r#"{{"name":"{name}","version":"{version}"}}"#).into_bytes(),
                    ),
                    ("private/marker.txt".into(), marker.to_vec()),
                ]),
            )
            .unwrap()
        };
        let a_label = "demo.a#1.0.0";
        let b_label = "demo.b#1.0.0";
        let c_label = "demo.c#1.0.0";
        let a = package(a_label, b"a");
        let b = package(b_label, b"b");
        let environment = PackageEnvironment::new([a]).unwrap();
        let before = environment.compression_metrics();

        let (successor, added) = environment.extended(vec![b.clone()], "test").unwrap();
        assert_eq!(added, 1);
        assert_eq!(successor.mounted_labels(), [a_label, b_label]);
        assert_eq!(
            successor.compression_metrics().chunks_inflated,
            before.chunks_inflated
        );
        assert_eq!(
            successor
                .packages
                .read(
                    &successor
                        .packages
                        .root()
                        .join(b_label)
                        .join("package/private/marker.txt")
                )
                .unwrap(),
            b"b"
        );
        assert!(!environment.packages.is_file(
            &environment
                .packages
                .root()
                .join(b_label)
                .join("package/private/marker.txt")
        ));

        let (idempotent, added) = successor.extended(vec![b], "test").unwrap();
        assert_eq!(added, 0);
        assert_eq!(idempotent.mounted_labels(), successor.mounted_labels());

        let conflict = package(a_label, b"conflict");
        assert!(successor
            .extended(vec![conflict.clone()], "test")
            .err()
            .unwrap()
            .contains("already mounted with different content"));
        assert_eq!(successor.mounted_labels(), [a_label, b_label]);

        let newcomer = package(c_label, b"c");
        assert!(successor
            .extended(vec![newcomer, conflict], "test")
            .err()
            .unwrap()
            .contains("already mounted with different content"));
        assert_eq!(successor.mounted_labels(), [a_label, b_label]);
        assert!(successor.package_key(c_label).is_none());
        assert_eq!(
            environment
                .packages
                .read(
                    &environment
                        .packages
                        .root()
                        .join(a_label)
                        .join("package/private/marker.txt")
                )
                .unwrap(),
            b"a"
        );
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
        let package_root = root.join(label).join("package");
        let mut package_entries = view
            .read_dir(&package_root)
            .unwrap()
            .into_iter()
            .map(|entry| (entry.file_name, entry.is_file))
            .collect::<Vec<_>>();
        package_entries.sort();
        assert_eq!(
            package_entries,
            vec![
                (".derived-index.json".into(), true),
                ("StructureDefinition-demo.json".into(), true),
                ("other".into(), false),
                ("package.json".into(), true),
            ]
        );
        assert_eq!(
            view.read_dir(&package_root.join("other"))
                .unwrap()
                .into_iter()
                .map(|entry| (entry.file_name, entry.is_file))
                .collect::<Vec<_>>(),
            vec![("spec.internals".into(), true)]
        );
        assert!(view.is_dir(&package_root.join("other")));
        assert!(!view.is_dir(&package_root.join("template")));
        assert!(!view.exists(&root.join(label).join("package/template/private.txt")));
        assert!(view
            .read(&root.join(label).join("package/template/private.txt"))
            .is_err());
    }

    #[test]
    fn publisher_package_view_prefers_normalized_spec_and_falls_back_to_native() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("cache");
        let label = "demo.package#1.0.0";
        let package_root = root.join(label);
        std::fs::create_dir_all(package_root.join("package")).unwrap();
        std::fs::create_dir_all(package_root.join("other")).unwrap();
        std::fs::write(package_root.join("other/spec.internals"), b"native-spec").unwrap();

        let scoped = || {
            PackageView::new(
                Rc::new(package_store::DiskSource::new()),
                root.clone(),
                Some(BTreeSet::from([label.into()])),
            )
        };
        let native = publisher_render_package_view(scoped(), &[label.into()], "test").unwrap();
        assert_eq!(
            native
                .read(&package_root.join("other/spec.internals"))
                .unwrap(),
            b"native-spec"
        );

        std::fs::create_dir_all(package_root.join("package/other")).unwrap();
        std::fs::write(
            package_root.join("package/other/spec.internals"),
            b"normalized-spec",
        )
        .unwrap();
        let normalized = publisher_render_package_view(scoped(), &[label.into()], "test").unwrap();
        assert_eq!(
            normalized
                .read(&package_root.join("package/other/spec.internals"))
                .unwrap(),
            b"normalized-spec"
        );
        assert!(!normalized.exists(&package_root.join("other/spec.internals")));
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

        let (revision, _) = site_build_project_revision(&project, "overlap.ig").unwrap();
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
    fn declared_resource_formats_close_as_pages_and_current_json_output() {
        let mut prepared = prepared_with_all_roles();
        prepared.authored_files.clear();
        prepared.resources.push(site_build::SemanticResource {
            key: site_build::SemanticResourceKey {
                resource_type: "Observation".into(),
                id: "format-test".into(),
            },
            resource: serde_json::json!({
                "resourceType": "Observation",
                "id": "format-test",
                "status": "final",
                "valueString": "current <value>"
            }),
            publication: None,
        });
        let project = site_build::ProjectIdentity {
            project_id: "demo.ig".into(),
            revision: "formats".into(),
            sources: site_build::SourceManifest::from_entries([]).unwrap(),
        };
        let template_files = BTreeMap::from([
            (
                "config.json".into(),
                br#"{
                    "formats":["xml","json","ttl"],
                    "defaults":{"Any":{
                        "template-format":"template/layouts/format.html",
                        "format":"{{[type]}}-{{[id]}}.{{[fmt]}}.html"
                    }}
                }"#
                .to_vec(),
            ),
            (
                "layouts/format.html".into(),
                b"---\n---\n{% include {{[type]}}-{{[name]}}.xhtml %}".to_vec(),
            ),
        ]);
        let engine = SiteEngine::default();
        let mut ready = BTreeMap::new();
        let mut objects = ObjectMap::new();
        let model = engine
            .publisher_model(
                &prepared,
                &project,
                &template_files,
                &mut ready,
                &mut objects,
                "demo.ig",
                "test",
            )
            .unwrap();
        let options = SiteOptions::default();
        let semantics = build_render_semantics(
            prepared_render_set(&prepared).unwrap(),
            None,
            model.site_files.as_ref(),
            &options,
        )
        .unwrap();
        let state = publisher_render_state(&semantics, &model, &options).unwrap();
        let catalog =
            publisher_output_plan(&ready, state.list_pages(), &model.resource_pages, "test")
                .unwrap();

        for format in ["xml", "json", "ttl"] {
            let path = format!("en/Observation-format-test.{format}.html");
            let descriptor = catalog
                .descriptors
                .iter()
                .find(|descriptor| descriptor.path.as_str() == path)
                .unwrap_or_else(|| panic!("missing format page {path}"));
            assert_eq!(descriptor.kind, crate::OutputKind::Page);
            assert!(descriptor.content.is_none());
        }
        let raw = catalog
            .descriptors
            .iter()
            .find(|descriptor| descriptor.path.as_str() == "en/Observation-format-test.json")
            .expect("current JSON output is in the closed catalog");
        assert_eq!(raw.kind, crate::OutputKind::Auxiliary);
        assert_eq!(raw.media_type, "application/fhir+json");
        let content = raw.content.as_ref().expect("raw JSON is materialized");
        let bytes = objects[&content.sha256].materialize().unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(bytes.as_slice()).unwrap(),
            prepared.resources.last().unwrap().resource
        );
        assert!(bytes.ends_with(b"\n"));
        assert!(!catalog
            .descriptors
            .iter()
            .any(|descriptor| descriptor.path.as_str() == "en/Observation-format-test.xml"));
        assert!(!catalog
            .descriptors
            .iter()
            .any(|descriptor| descriptor.path.as_str() == "en/Observation-format-test.ttl"));

        let json_page = state
            .render_page_by_name("en/Observation-format-test.json.html")
            .unwrap();
        assert!(json_page.contains("<pre class=\"json\">"), "{json_page}");
        assert!(json_page.contains("current &lt;value&gt;"), "{json_page}");
        let xml_page = state
            .render_page_by_name("en/Observation-format-test.xml.html")
            .unwrap();
        assert!(
            xml_page.contains("XML representation unavailable"),
            "{xml_page}"
        );
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
        let authenticated_objects = ObjectMap::new();
        let (artifacts, plan, mut objects) = publisher_artifacts(
            &prepared,
            &project,
            &lock,
            &recipe_assets,
            &authenticated_objects,
        )
        .unwrap();
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

    fn publisher_prepare_fixture(
        narrative: &str,
    ) -> (
        crate::ProjectRevision,
        crate::ResolvedPackageClosure,
        GeneratorSpec,
        PackageEnvironment,
    ) {
        let core_label = "hl7.fhir.r4.core#4.0.1";
        let template_label = "demo.template#1.0.0";
        let core = package_store::PreparedPackage::prepare(
            core_label,
            BTreeMap::from([
                (
                    "package.json".into(),
                    br#"{"name":"hl7.fhir.r4.core","version":"4.0.1","url":"http://hl7.org/fhir/R4","fhirVersions":["4.0.1"]}"#
                        .to_vec(),
                ),
                (".index.json".into(), br#"{"files":[]}"#.to_vec()),
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
                    br#"{"name":"demo.template","version":"1.0.0","type":"fhir.template"}"#
                        .to_vec(),
                ),
                (
                    "config.json".into(),
                    br#"{"defaults":{"Any":{"template-base":"template/layouts/default.html","base":"{{[type]}}-{{[id]}}.html"}}}"#
                        .to_vec(),
                ),
                (
                    "layouts/default.html".into(),
                    b"---\n---\n{{content}}".to_vec(),
                ),
            ]),
        )
        .unwrap();
        let environment = PackageEnvironment::new([core, template]).unwrap();
        let config = "id: demo.stage\ncanonical: https://example.org/stage\nname: StageDemo\nstatus: draft\nversion: 1.0.0\nfhirVersion: 4.0.1\n";
        let revision = crate::ProjectRevision {
            config: config.into(),
            fsh: BTreeMap::new(),
            predefined: BTreeMap::new(),
            site_files: BTreeMap::from([(
                "input/pagecontent/index.md".into(),
                narrative.as_bytes().to_vec(),
            )]),
        };
        let resolved = crate::ResolvedPackageClosure {
            config_sha256: site_build::Sha256Digest::of_bytes(config.as_bytes()),
            resolution_support: BTreeSet::new(),
            labels: vec![core_label.into()],
        };
        let spec = GeneratorSpec::Publisher {
            template_coordinate: template_label.into(),
            build_epoch_secs: 1,
            active_tables: true,
            run_uuid: None,
        };
        (revision, resolved, spec, environment)
    }

    #[test]
    fn canonical_publisher_prepare_is_atomic_with_one_eager_catalog() {
        #[derive(Clone)]
        struct Source(crate::ProjectRevision);
        impl crate::ProjectSource for Source {
            fn config(&mut self) -> Result<String, String> {
                Ok(self.0.config.clone())
            }

            fn capture(
                &mut self,
                _packages: &PackageEnvironment,
                _resolved: &crate::ResolvedPackageClosure,
            ) -> Result<crate::ProjectRevision, String> {
                Ok(self.0.clone())
            }
        }

        #[derive(Clone)]
        struct Packages {
            resolved: crate::ResolvedPackageClosure,
            environment: PackageEnvironment,
        }
        impl crate::PackageProvider for Packages {
            fn resolve(
                &mut self,
                config: &str,
                _generator: &GeneratorSpec,
            ) -> Result<crate::ResolvedPackageClosure, String> {
                if site_build::Sha256Digest::of_bytes(config.as_bytes())
                    != self.resolved.config_sha256
                {
                    return Err("fixture config changed".into());
                }
                Ok(self.resolved.clone())
            }

            fn environment(
                &mut self,
                resolved: &crate::ResolvedPackageClosure,
            ) -> Result<PackageEnvironment, String> {
                if resolved != &self.resolved {
                    return Err("fixture closure changed".into());
                }
                Ok(self.environment.clone())
            }
        }

        fn state_fingerprint(engine: &SiteEngine) -> String {
            let derivations = snapshot_derivation_state_fingerprint(engine);
            let semantics = engine
                .preparation
                .history
                .iter()
                .filter_map(|generation| generation.render_semantics.as_ref())
                .map(|entry| (entry.key.clone(), Rc::as_ptr(&entry.semantics) as usize));
            let semantics = semantics.collect::<Vec<_>>();
            let catalogs = engine
                .preparation
                .history
                .iter()
                .filter_map(|generation| generation.render_package_catalog.as_ref())
                .map(|entry| {
                    (
                        entry.key.clone(),
                        entry
                            .catalog
                            .as_ref()
                            .map(|catalog| Rc::as_ptr(catalog) as usize),
                    )
                })
                .collect::<Vec<_>>();
            format!(
                "{:?}",
                (
                    engine.compilation_state_fingerprint(),
                    derivations,
                    semantics,
                    catalogs,
                    engine.retained_generation_handles(),
                )
            )
        }

        let (revision_a, resolved, spec, environment) = publisher_prepare_fixture("# A");
        let mut packages = Packages {
            resolved: resolved.clone(),
            environment: environment.clone(),
        };
        let mut engine = SiteEngine::default();
        let installed_a = engine
            .prepare_project(&mut Source(revision_a), &mut packages, spec.clone())
            .unwrap();
        let a_fingerprint = state_fingerprint(&engine);

        let (mut revision_b, _, _, _) = publisher_prepare_fixture("# B");
        revision_b
            .fsh
            .insert("input/fsh/change.fsh".into(), "// semantic B".into());
        engine.fail_during_publisher_close = true;
        let error = engine
            .prepare_project(&mut Source(revision_b.clone()), &mut packages, spec.clone())
            .unwrap_err();
        engine.fail_during_publisher_close = false;
        assert!(error.message.contains("injected failure"));
        assert!(
            error.successful_compilation.is_some(),
            "the failed canonical prepare still reports its successful compilation"
        );
        assert_eq!(state_fingerprint(&engine), a_fingerprint);
        assert!(engine.outputs(&installed_a.site.build_id).is_ok());

        let completed = engine
            .prepare_project(&mut Source(revision_b.clone()), &mut packages, spec.clone())
            .unwrap();
        assert_eq!(completed.events[0].phase.as_deref(), Some("site.prepare"));
        assert_ne!(completed.site.build_id, installed_a.site.build_id);
        let rendered = engine
            .render(&completed.site.build_id, "en/index.html")
            .unwrap();
        let rendered_bytes = engine
            .read_content(&completed.site.build_id, rendered.sha256.as_str())
            .unwrap();
        rendered.verify(&rendered_bytes).unwrap();
        let completed_catalog = engine.outputs(&completed.site.build_id).unwrap();
        assert_eq!(
            completed_catalog
                .outputs
                .iter()
                .find(|output| output.path.as_str() == "en/index.html")
                .and_then(|output| output.content.as_ref()),
            Some(&rendered)
        );
        let repeated_catalog = engine.outputs(&completed.site.build_id).unwrap();
        assert_eq!(repeated_catalog, completed_catalog);

        // Exact reuse closes synchronously and may advance recency only after
        // the retained handle and compilation candidate both validate.
        let completed_fingerprint = state_fingerprint(&engine);
        let reused = engine
            .prepare_project(&mut Source(revision_b), &mut packages, spec.clone())
            .unwrap();
        assert_eq!(reused.site.build_id, completed.site.build_id);
        assert_eq!(state_fingerprint(&engine), completed_fingerprint);

        let (mut revision_d, _, _, _) = publisher_prepare_fixture("# D");
        revision_d
            .fsh
            .insert("input/fsh/change.fsh".into(), "// semantic D".into());
        let native = engine
            .prepare_project(&mut Source(revision_d), &mut packages, spec)
            .unwrap();
        assert_eq!(native.events[0].phase.as_deref(), Some("site.prepare"));
        let metrics = native.events[0].metrics.as_ref().unwrap();
        assert_eq!(
            native.events[0].duration_ms,
            Some(metrics["compileProjectMs"] + metrics["rustPrepareTotalMs"]),
            "native composed prepare must still report the combined measured Rust work"
        );
        assert!(native.events[0].duration_ms.unwrap() < 1_000.0);
    }

    #[test]
    fn typed_prepare_constructs_and_retains_complete_publisher_build() {
        let core_label = "hl7.fhir.r4.core#4.0.1";
        let template_label = "demo.template#1.0.0";
        let make_core = |marker: &[u8]| {
            package_store::PreparedPackage::prepare(
                core_label,
                BTreeMap::from([
                    (
                        "package.json".into(),
                        br#"{"name":"hl7.fhir.r4.core","version":"4.0.1","url":"http://hl7.org/fhir/R4","fhirVersions":["4.0.1"]}"#
                            .to_vec(),
                    ),
                    (".index.json".into(), br#"{"files":[]}"#.to_vec()),
                    ("carrier-marker.txt".into(), marker.to_vec()),
                    ("other/spec.internals".into(), b"spec".to_vec()),
                    ("other/fhir.css".into(), b"body{}".to_vec()),
                    ("other/icon_element.gif".into(), b"icon".to_vec()),
                    ("other/tbl_spacer.png".into(), b"spacer".to_vec()),
                ]),
            )
            .unwrap()
        };
        let make_template = || {
            package_store::PreparedPackage::prepare(
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
                    (
                        "includes/template-page.html".into(),
                        b"<main><h1>{{site.data.pages[page.path].title}}</h1>{% assign path = page.path | split: '.html' %}{% include {{path}}.xml %}</main>".to_vec(),
                    ),
                ]),
            )
            .unwrap()
        };
        let environment =
            PackageEnvironment::new([make_core(b"carrier-a"), make_template()]).unwrap();
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
            "definition":{
                "grouping":[{"id":"profiles","name":"Profiles"}],
                "page":{
                    "nameUrl":"index.html",
                    "title":"Home",
                    "generation":"markdown"
                }
            }
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
        let mut closed_package_objects = ObjectMap::new();
        for (_, locked) in closed.site_build().package_lock().iter() {
            let bytes = Rc::new(
                engine
                    .read_content(&prepared.build_id, locked.content.sha256.as_str())
                    .unwrap(),
            );
            insert_authenticated_object(&mut closed_package_objects, &locked.content, bytes)
                .unwrap();
        }
        let live_packages = publisher_render_package_view(
            environment.scoped(&resolved.labels, "test").unwrap(),
            &resolved.labels,
            "test",
        )
        .unwrap();
        let restored_packages = package_view_from_closed_build(
            &closed,
            &closed_package_objects,
            &resolved.labels,
            "test",
        )
        .unwrap();
        let package_root = live_packages.root().join(core_label).join("package");
        let marker = package_root.join("carrier-marker.txt");
        let spec = package_root.join("other/spec.internals");
        let hidden = package_root.join("other/fhir.css");
        let listing = |view: &PackageView, path: &std::path::Path| {
            view.read_dir(path)
                .unwrap()
                .into_iter()
                .map(|entry| (entry.file_name, entry.is_file))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            live_packages.read(&marker).unwrap(),
            restored_packages.read(&marker).unwrap()
        );
        assert_eq!(
            live_packages.read(&spec).unwrap(),
            restored_packages.read(&spec).unwrap()
        );
        assert_eq!(
            live_packages.exists(&marker),
            restored_packages.exists(&marker)
        );
        assert_eq!(
            live_packages.is_file(&marker),
            restored_packages.is_file(&marker)
        );
        assert_eq!(
            live_packages.is_dir(&package_root),
            restored_packages.is_dir(&package_root)
        );
        assert_eq!(
            listing(&live_packages, &package_root),
            listing(&restored_packages, &package_root)
        );
        assert_eq!(
            live_packages.immutable_content_identity(&marker),
            restored_packages.immutable_content_identity(&marker)
        );
        assert!(live_packages.immutable_content_identity(&marker).is_some());
        assert!(!live_packages.exists(&hidden));
        assert!(!restored_packages.exists(&hidden));
        let live_fork = live_packages.fork_for_compile().unwrap();
        let restored_fork = restored_packages.fork_for_compile().unwrap();
        assert_eq!(
            live_fork.immutable_content_identity(&marker),
            restored_fork.immutable_content_identity(&marker)
        );
        let leading_path = "en/index.html";
        let leading_content = engine.render(&prepared.build_id, leading_path).unwrap();
        let leading_bytes = engine
            .read_content(&prepared.build_id, leading_content.sha256.as_str())
            .unwrap();
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
        let restored_leading = restored.render(&restored_handle, leading_path).unwrap();
        let restored_leading_bytes = restored
            .read_content(&restored_handle, restored_leading.sha256.as_str())
            .unwrap();
        assert_eq!(restored_leading, leading_content);
        assert_eq!(restored_leading_bytes, leading_bytes);
        assert_eq!(restored.outputs(&restored_handle).unwrap(), live_catalog);

        let page_paths = live_catalog
            .outputs
            .iter()
            .filter(|output| output.content.is_none())
            .map(|output| output.path.to_string())
            .collect::<Vec<_>>();
        assert!(page_paths.iter().any(|path| path == "en/toc.html"));
        assert!(page_paths.iter().any(|path| path == "en/artifacts.html"));
        assert!(page_paths.iter().all(|path| !path.starts_with("template/")));
        let mut live_pages = BTreeMap::new();
        for path in &page_paths {
            let output = engine.render(&prepared.build_id, path).unwrap();
            let bytes = engine
                .read_content(&prepared.build_id, output.sha256.as_str())
                .unwrap();
            live_pages.insert(path.clone(), (output, bytes));
        }
        let toc = &live_pages["en/toc.html"].1;
        assert!(std::str::from_utf8(toc)
            .unwrap()
            .contains("<h1>Table of Contents</h1>"));
        assert!(std::str::from_utf8(toc)
            .unwrap()
            .contains("href=\"index.html\""));
        assert!(std::str::from_utf8(toc)
            .unwrap()
            .contains("href=\"artifacts.html\""));
        let artifacts = &live_pages["en/artifacts.html"].1;
        assert!(std::str::from_utf8(artifacts)
            .unwrap()
            .contains("<h1>Artifacts Summary</h1>"));
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

        // A semantic successor cannot reuse the whole RenderSemantics, but its
        // exact package carriers and primary-IG selection can reuse only the
        // immutable package catalog. Current own resources/context are rebuilt.
        let patient = serde_json::json!({
            "resourceType":"Patient",
            "id":"semantic-successor",
            "name":[{"family":"Current"}]
        });
        let install_semantic = |engine: &mut SiteEngine, primary_ig: Value| {
            let project = crate::compilation::CompiledProjectRevision::new(
                crate::ProjectRevision {
                    config: config.into(),
                    fsh: BTreeMap::new(),
                    predefined: BTreeMap::new(),
                    site_files: BTreeMap::from([(
                        "input/pagecontent/index.md".into(),
                        b"# Fourth narrative".to_vec(),
                    )]),
                },
                resolved.clone(),
            )
            .unwrap();
            engine.install_compilation_for_test(
                project,
                vec![
                    (
                        PathBuf::from("/__ig__/ImplementationGuide-demo.json"),
                        primary_ig,
                    ),
                    (
                        PathBuf::from("/out/Patient-semantic-successor.json"),
                        patient.clone(),
                    ),
                ],
            );
        };
        install_semantic(&mut engine, ig.clone());
        let semantic = engine
            .prepare(publisher_spec.clone(), environment.clone())
            .unwrap();
        assert!(!semantic.measurements.render_semantics_cache_hit);
        assert!(semantic.measurements.render_package_catalog_cache_hit);
        assert!(!semantic.measurements.render_package_catalog_built);
        assert!(semantic.measurements.render_own_context_built);
        assert!(semantic.measurements.render_package_catalog_packages > 0);
        assert!(semantic.measurements.render_package_catalog_admitted);
        assert!(
            semantic
                .measurements
                .render_package_catalog_retained_generations
                <= 2
        );

        // Ordered primary-IG package selection is catalog identity. Adding an
        // explicit core dependency builds a second generation even though the
        // resulting mounted package happens to be the same fallback core.
        let mut ig_b = ig.clone();
        ig_b["dependsOn"] = serde_json::json!([{
            "packageId": core_label,
            "version": "4.0.1"
        }]);
        install_semantic(&mut engine, ig_b);
        let generation_b = engine
            .prepare(publisher_spec.clone(), environment.clone())
            .unwrap();
        assert!(!generation_b.measurements.render_semantics_cache_hit);
        assert!(!generation_b.measurements.render_package_catalog_cache_hit);
        assert!(generation_b.measurements.render_package_catalog_built);
        assert!(generation_b.measurements.render_own_context_built);
        assert_eq!(
            generation_b
                .measurements
                .render_package_catalog_retained_generations,
            2
        );

        // A -> B -> A retains exactly two catalog generations. Changing a
        // renderer option prevents exact runtime/RenderSemantics reuse, so this
        // return exercises catalog A rather than an older whole-build hit.
        install_semantic(&mut engine, ig.clone());
        let option_miss = engine
            .prepare(
                GeneratorSpec::Publisher {
                    template_coordinate: template_label.into(),
                    build_epoch_secs: 1,
                    active_tables: false,
                    run_uuid: None,
                },
                environment.clone(),
            )
            .unwrap();
        assert!(option_miss.measurements.publisher_recipe_assets_cache_hit);
        assert!(!option_miss.measurements.render_semantics_cache_hit);
        assert!(option_miss.measurements.render_package_catalog_cache_hit);
        assert!(!option_miss.measurements.render_package_catalog_built);
        assert!(option_miss.measurements.render_own_context_built);
        assert!(!option_miss.measurements.site_build_cache_hit);
        assert_eq!(
            engine
                .preparation
                .cache_hits
                .publisher_recipe_artifact_builds,
            1,
            "prose/options successors must reuse exact template/runtime artifact records"
        );

        // A catalog staged under a new selection key is not visible until the
        // complete Publisher runtime installs. A retry after a deliberately
        // later failure must rebuild that catalog rather than observe the
        // uncommitted candidate.
        let mut late_failure_ig = ig.clone();
        late_failure_ig["title"] = Value::String("late failure candidate".into());
        late_failure_ig["dependsOn"] = serde_json::json!([
            {"packageId":core_label,"version":"4.0.1"},
            {"packageId":core_label,"version":"4.0.1"}
        ]);
        install_semantic(&mut engine, late_failure_ig.clone());
        engine.fail_after_render_package_catalog_stage = true;
        let failure = engine
            .prepare(publisher_spec.clone(), environment.clone())
            .unwrap_err();
        assert!(failure.contains("after render package catalog staging"));
        engine.fail_after_render_package_catalog_stage = false;
        let recovered = engine
            .prepare(publisher_spec.clone(), environment.clone())
            .unwrap();
        assert!(!recovered.measurements.render_package_catalog_cache_hit);
        assert!(recovered.measurements.render_package_catalog_built);

        // Exercise the production admission wiring through a complete prepare:
        // an over-budget success installs a same-key tombstone. The next
        // semantic successor rebuilds instead of hitting an older retained
        // catalog; restoring the real bounds admits a later fresh generation.
        engine.render_package_catalog_limits = Some((0, 0, 0));
        let mut over_budget_1 = ig.clone();
        over_budget_1["title"] = Value::String("over budget one".into());
        install_semantic(&mut engine, over_budget_1);
        let over_budget_1 = engine
            .prepare(publisher_spec.clone(), environment.clone())
            .unwrap();
        assert!(!over_budget_1.measurements.render_package_catalog_admitted);

        let mut over_budget_2 = ig.clone();
        over_budget_2["title"] = Value::String("over budget two".into());
        install_semantic(&mut engine, over_budget_2);
        let over_budget_2 = engine
            .prepare(publisher_spec.clone(), environment.clone())
            .unwrap();
        assert!(!over_budget_2.measurements.render_package_catalog_cache_hit);
        assert!(over_budget_2.measurements.render_package_catalog_built);
        assert!(!over_budget_2.measurements.render_package_catalog_admitted);

        engine.render_package_catalog_limits = None;
        let mut admitted_again = ig;
        admitted_again["title"] = Value::String("admitted again".into());
        install_semantic(&mut engine, admitted_again);
        let admitted_again = engine.prepare(publisher_spec, environment).unwrap();
        assert!(!admitted_again.measurements.render_package_catalog_cache_hit);
        assert!(admitted_again.measurements.render_package_catalog_built);
        assert!(admitted_again.measurements.render_package_catalog_admitted);
    }

    #[test]
    fn render_package_catalog_key_binds_authenticated_carriers_and_selection() {
        fn package_lock(bytes: &[u8]) -> site_build::PackageLock {
            site_build::PackageLock::from_packages([site_build::LockedPackage {
                coordinate: site_build::PackageCoordinate::parse("hl7.fhir.r4.core#4.0.1").unwrap(),
                content: site_build::ContentRef::of_bytes(
                    bytes,
                    Some(package_store::PREPARED_PACKAGE_MEDIA_TYPE),
                ),
                dependencies: BTreeSet::new(),
            }])
            .unwrap()
        }

        let prepared = prepared_with_all_roles();
        let labels = vec!["hl7.fhir.r4.core#4.0.1".to_string()];
        let lock_a = package_lock(b"carrier-a");
        let key_a = publisher_render_package_catalog_cache_key(
            &prepared,
            &lock_a,
            &labels,
            "/packages",
            "test",
        )
        .unwrap();
        assert_eq!(
            key_a,
            publisher_render_package_catalog_cache_key(
                &prepared,
                &lock_a,
                &labels,
                "/packages",
                "test",
            )
            .unwrap()
        );
        let mut metadata_only = prepared.clone();
        metadata_only.resources[0].resource["title"] = Value::String("Irrelevant".into());
        assert_eq!(
            key_a,
            publisher_render_package_catalog_cache_key(
                &metadata_only,
                &lock_a,
                &labels,
                "/packages",
                "test",
            )
            .unwrap(),
            "primary-IG fields not read by package selection must not fragment the cache"
        );
        let mut changed_selection = prepared.clone();
        changed_selection.resources[0].resource["dependsOn"] = serde_json::json!([{
            "packageId":"hl7.fhir.r4.core",
            "version":"4.0.1"
        }]);
        assert_ne!(
            key_a,
            publisher_render_package_catalog_cache_key(
                &changed_selection,
                &lock_a,
                &labels,
                "/packages",
                "test",
            )
            .unwrap()
        );
        assert_ne!(
            key_a,
            publisher_render_package_catalog_cache_key(
                &prepared,
                &package_lock(b"carrier-b"),
                &labels,
                "/packages",
                "test",
            )
            .unwrap()
        );
        assert_ne!(
            key_a,
            publisher_render_package_catalog_cache_key(
                &prepared,
                &lock_a,
                &labels,
                "/other-root",
                "test",
            )
            .unwrap()
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
    fn generate_snapshot_derivation(
        &self,
        input: snapshot_gen::SnapshotDerivationInput,
        context: &snapshot_gen::PackageContext,
        path_key: &str,
        label: &str,
        operation: &str,
        _metrics: &mut PrepareMeasurements,
    ) -> Result<(Value, SnapshotResourceDerivation), String> {
        let (generated, derivation) =
            snapshot_gen::generate_prepared_snapshot_derivation(input, context)
                .map_err(|error| format!("{operation}: snapshot {label}: {error:#}"))?;
        let retention = derivation
            .retention()
            .map_err(|error| format!("{operation}: measure snapshot {path_key}: {error:#}"))?;
        let derivation = Rc::new(derivation);
        Ok((
            generated,
            SnapshotResourceDerivation {
                derivation,
                retention,
            },
        ))
    }

    fn preparation_cache_keys(
        &self,
        input: &PrepareGuideOptions,
        project: &site_build::ProjectIdentity,
        package_lock: &site_build::PackageLock,
        compiled: &[(PathBuf, Value)],
        operation: &str,
    ) -> Result<PreparationCacheKeys, String> {
        let compiled_sha256 = compiled_revision_sha256(compiled, operation)?;
        let prepared_guide = site_build::sha256_canonical(&PreparedGuideIdentityPayload {
            schema: PREPARED_GUIDE_IDENTITY_SCHEMA,
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
        .map_err(|error| format!("{operation}: hash PreparedGuide identity: {error}"))?;
        let render_semantics_source =
            site_build::sha256_canonical(&RenderSemanticsSourceKeyPayload {
                schema: "publisher-render-semantics-source/v1",
                engine_api: ENGINE_API,
                compiled_sha256: &compiled_sha256,
                package_lock,
            })
            .map_err(|error| format!("{operation}: hash render semantics source: {error}"))?;
        Ok(PreparationCacheKeys {
            prepared_guide,
            render_semantics_source,
        })
    }

    fn site_model_from_compile_candidate(
        &mut self,
        input: &PrepareGuideOptions,
        environment: &PackageEnvironment,
        operation: &str,
        metrics: &mut PrepareMeasurements,
        compilation: Option<&crate::compilation::CompilationCandidate>,
    ) -> Result<PreparedGuideStage, String> {
        record_snapshot_derivation_metrics(&self.preparation, metrics);
        let compiled = compilation
            .map(crate::compilation::CompilationCandidate::compiled_resources)
            .unwrap_or_else(|| self.compiled_resources());
        if compiled.is_empty() {
            return Err(format!(
                "{operation}: no compiled revision; call compileProject first"
            ));
        }
        let project = compilation
            .map(crate::compilation::CompilationCandidate::project)
            .or_else(|| self.project_revision())
            .cloned()
            .ok_or_else(|| {
                format!(
                    "{operation}: compileProject has not established a complete source revision"
                )
            })?;
        let resolved = project.resolved_packages();
        let (_, fhir_version) = compiled_ig_identity(compiled)?;
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
            prepared_local_resource_set(compiled, operation)?;
        let mut snapshot_locals = local_resources
            .iter()
            .filter(|(_, body)| {
                body.get("resourceType").and_then(Value::as_str) == Some("StructureDefinition")
            })
            .cloned()
            .collect::<Vec<_>>();
        snapshot_locals.sort_by(|left, right| left.0.cmp(&right.0));
        context.load_local_resources(snapshot_locals);
        let mut derivations = BTreeMap::new();
        let mut derivation_fact_count = 0usize;
        let mut derivation_manifest_approx_bytes = 0usize;
        let mut derivation_snapshot_json_bytes = 0usize;
        let mut derivation_admissible = true;
        for (path, body) in &mut local_resources {
            if body.get("resourceType").and_then(Value::as_str) != Some("StructureDefinition") {
                continue;
            }
            let path_key = path
                .to_str()
                .ok_or_else(|| {
                    format!("{operation}: snapshot resource path is not UTF-8: {path:?}")
                })?
                .to_string();
            let mut input =
                snapshot_gen::prepare_snapshot_derivation(body.clone(), Default::default())
                    .map_err(|error| {
                        format!("{operation}: prepare snapshot input {path_key}: {error:#}")
                    })?;
            let label = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("StructureDefinition")
                .to_string();

            let mut reusable = None;
            for candidate in self
                .preparation
                .history
                .iter()
                .filter_map(|generation| generation.snapshots.resources.get(&path_key))
            {
                let recomposed = candidate
                    .derivation
                    .try_recompose(&mut input, &context)
                    .map_err(|error| {
                        format!("{operation}: recompose snapshot {path_key}: {error:#}")
                    })?;
                if let Some(recomposed) = recomposed {
                    reusable = Some((candidate.clone(), recomposed));
                    break;
                }
            }
            let derivation = if let Some((candidate, recomposed)) = reusable {
                metrics.snapshot_resource_cache_hits += 1;
                #[cfg(test)]
                {
                    self.preparation.cache_hits.snapshot_resources += 1;
                }
                *body = recomposed;
                candidate
            } else {
                metrics.snapshot_resource_cache_misses += 1;
                let (generated, derivation) = self.generate_snapshot_derivation(
                    input, &context, &path_key, &label, operation, metrics,
                )?;
                *body = generated;
                derivation
            };

            if derivation.retention.complete {
                derivation_fact_count =
                    derivation_fact_count.saturating_add(derivation.retention.dependency_facts);
                derivation_manifest_approx_bytes = derivation_manifest_approx_bytes
                    .saturating_add(derivation.retention.manifest_approx_bytes);
                derivation_snapshot_json_bytes = derivation_snapshot_json_bytes
                    .saturating_add(derivation.retention.snapshot_json_bytes);
                if derivations.len() >= SNAPSHOT_DERIVATION_MAX_RESOURCES
                    || derivation_fact_count > SNAPSHOT_DERIVATION_MAX_FACTS
                    || derivation_manifest_approx_bytes
                        > SNAPSHOT_DERIVATION_MAX_MANIFEST_APPROX_BYTES
                    || derivation_snapshot_json_bytes > SNAPSHOT_DERIVATION_MAX_SNAPSHOT_JSON_BYTES
                {
                    derivation_admissible = false;
                } else {
                    derivations.insert(path_key, derivation);
                }
            }
        }
        let generated = local_resources
            .into_iter()
            .map(|(_, body)| body)
            .collect::<Vec<_>>();
        let snapshots = snapshot_derivation_candidate(
            derivations,
            derivation_fact_count,
            derivation_manifest_approx_bytes,
            derivation_snapshot_json_bytes,
            derivation_admissible,
        );
        let guide = Rc::new(assemble_prepared_model(
            operation,
            &generated,
            &primary_implementation_guide,
            project.config(),
            project.site_files(),
            input.build_epoch_secs,
            &input.liquid_asset_dirs,
            input.branch.clone(),
            input.revision.clone(),
        )?);
        Ok(PreparedGuideStage {
            guide,
            generation: PreparationGeneration {
                snapshots: snapshots.generation,
                render_semantics: None,
                render_package_catalog: None,
            },
            snapshot_admitted: snapshots.admitted,
        })
    }

    fn commit_prepared_guide_stage(
        &mut self,
        stage: PreparedGuideStage,
        metrics: &mut PrepareMeasurements,
    ) -> Rc<site_build::PreparedGuide> {
        metrics.snapshot_derivation_admitted = stage.snapshot_admitted;
        let guide = stage.guide;
        self.preparation.history.promote(stage.generation);
        record_snapshot_derivation_metrics(&self.preparation, metrics);
        guide
    }
}

fn snapshot_derivation_candidate(
    resources: BTreeMap<String, SnapshotResourceDerivation>,
    fact_count: usize,
    manifest_approx_bytes: usize,
    snapshot_json_bytes: usize,
    observation_complete: bool,
) -> SnapshotDerivationCandidate {
    let within_bounds = observation_complete
        && resources.len() <= SNAPSHOT_DERIVATION_MAX_RESOURCES
        && fact_count <= SNAPSHOT_DERIVATION_MAX_FACTS
        && manifest_approx_bytes <= SNAPSHOT_DERIVATION_MAX_MANIFEST_APPROX_BYTES
        && snapshot_json_bytes <= SNAPSHOT_DERIVATION_MAX_SNAPSHOT_JSON_BYTES;
    let admitted = within_bounds && !resources.is_empty();
    let generation = if within_bounds {
        SnapshotDerivationGeneration {
            resources,
            fact_count,
            manifest_approx_bytes,
            snapshot_json_bytes,
        }
    } else {
        SnapshotDerivationGeneration {
            resources: BTreeMap::new(),
            fact_count: 0,
            manifest_approx_bytes: 0,
            snapshot_json_bytes: 0,
        }
    };
    SnapshotDerivationCandidate {
        generation,
        admitted,
    }
}

fn record_snapshot_derivation_metrics(
    preparation: &PreparationState,
    metrics: &mut PrepareMeasurements,
) {
    metrics.snapshot_derivation_history_generations = preparation.history.len() as u64;
    metrics.snapshot_derivation_retained_facts = preparation
        .history
        .iter()
        .map(|generation| generation.snapshots.fact_count as u64)
        .sum();
    metrics.snapshot_derivation_retained_manifest_approx_bytes = preparation
        .history
        .iter()
        .map(|generation| generation.snapshots.manifest_approx_bytes as u64)
        .sum();
    metrics.snapshot_derivation_retained_snapshot_json_bytes = preparation
        .history
        .iter()
        .map(|generation| generation.snapshots.snapshot_json_bytes as u64)
        .sum();
}

fn compiled_revision_sha256(
    compiled: &[(PathBuf, Value)],
    operation: &str,
) -> Result<site_build::Sha256Digest, String> {
    let mut seen = BTreeSet::new();
    let values = compiled
        .iter()
        .map(|(path, body)| {
            let path = path
                .to_str()
                .ok_or_else(|| format!("{operation}: compiled path is not UTF-8: {path:?}"))?;
            if !seen.insert(path) {
                return Err(format!(
                    "{operation}: compiled revision contains duplicate path {path}"
                ));
            }
            Ok((path, body))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let bytes = serde_json::to_vec(&values)
        .map_err(|error| format!("{operation}: encode compiled revision: {error}"))?;
    Ok(site_build::Sha256Digest::of_bytes(&bytes))
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
) -> Result<(site_build::ProjectIdentity, ObjectMap), String> {
    let mut entries = BTreeMap::new();
    let mut objects = ObjectMap::new();
    let mut insert = |path: &str,
                      kind: site_build::SourceKind,
                      bytes: Vec<u8>,
                      media_type: &str|
     -> Result<(), String> {
        let path = site_build::SourcePath::parse(path.to_string())
            .map_err(|error| format!("prepare: source path {path}: {error}"))?;
        let bytes = Rc::new(bytes);
        let content = site_build::ContentRef::of_bytes(bytes.as_ref(), Some(media_type));
        insert_authenticated_object(&mut objects, &content, bytes)?;
        if entries
            .insert(path.clone(), site_build::SourceEntry { kind, content })
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
        project.config().as_bytes().to_vec(),
        "application/yaml",
    )?;
    for (path, body) in project.fsh() {
        insert(
            path,
            site_build::SourceKind::Fsh,
            body.as_bytes().to_vec(),
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
            bytes,
            "application/fhir+json",
        )?;
    }
    for (path, bytes) in project.site_files() {
        let (kind, media_type) = source_kind_and_media_type(path);
        insert(path, kind, bytes.clone(), media_type)?;
    }
    let sources = site_build::SourceManifest::from_entries(entries)
        .map_err(|error| format!("prepare: source manifest: {error}"))?;
    let revision = format!(
        "sources-sha256:{}",
        site_build::sha256_canonical(&sources)
            .map_err(|error| format!("prepare: source revision: {error}"))?
    );
    Ok((
        site_build::ProjectIdentity {
            project_id: project_id.into(),
            revision,
            sources,
        },
        objects,
    ))
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
    project_objects: &ObjectMap,
    package_lock: &site_build::PackageLock,
    environment: &PackageEnvironment,
    operation: &str,
) -> Result<ObjectMap, String> {
    let mut objects = project_objects.clone();
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

fn cycle_runtime_key(
    prepared_guide_key: &site_build::Sha256Digest,
    render_target: &site_build::RenderTarget,
    diagnostics: &BTreeSet<site_build::BuildDiagnostic>,
) -> Result<site_build::Sha256Digest, site_build::CanonicalError> {
    site_build::sha256_canonical(&CycleRuntimeKeyPayload {
        schema: CYCLE_RUNTIME_KEY_SCHEMA,
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
    fn prepare_publisher_generation(
        &mut self,
        template_coordinate: &str,
        guide_options: PrepareGuideOptions,
        active_tables: bool,
        run_uuid: Option<String>,
        environment: &PackageEnvironment,
        compilation: Option<&crate::compilation::CompilationCandidate>,
    ) -> Result<PublisherTargetCandidate, String> {
        let total_started = self.timer();
        let operation = "prepare(publisher)";
        let mut metrics = PrepareMeasurements::default();
        if template_coordinate.trim().is_empty() {
            return Err(format!("{operation}: template coordinate is empty"));
        }
        let template = site_build::PackageCoordinate::parse(template_coordinate)
            .map_err(|error| format!("{operation}: template coordinate must be exact: {error}"))?;
        let project = compilation
            .map(crate::compilation::CompilationCandidate::project)
            .or_else(|| self.project_revision())
            .cloned()
            .ok_or_else(|| {
                format!(
                    "{operation}: compileProject has not established a complete source revision"
                )
            })?;
        let compiled = compilation
            .map(crate::compilation::CompilationCandidate::compiled_resources)
            .unwrap_or_else(|| self.compiled_resources());
        let (project_id, fhir_version) =
            compiled_ig_identity(compiled).map_err(|error| format!("{operation}: {error}"))?;
        let (project_revision, project_objects) =
            site_build_project_revision(&project, &project_id)?;
        let compile_lock = site_build_package_lock(&project, environment)?;
        let keys = self.preparation_cache_keys(
            &guide_options,
            &project_revision,
            &compile_lock,
            compiled,
            operation,
        )?;

        let resolution = environment.resolve_template(template_coordinate)?;
        if let Some(missing) = &resolution.missing {
            return Err(format!(
                "{operation}: template chain is incomplete; missing {missing}"
            ));
        }
        let package_lock = with_template_chain(
            &compile_lock,
            &resolution.chain,
            &environment.materials,
            operation,
        )?;
        let diagnostics = site_build_diagnostics(
            compilation
                .map(crate::compilation::CompilationCandidate::diagnostics)
                .unwrap_or_else(|| self.compile_diagnostics()),
        );
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
        if let Some((handle, build)) = self.find_publisher(&preparation_key) {
            metrics.site_build_cache_hit = true;
            metrics.total_ms = elapsed_ms(total_started);
            return Ok(PublisherTargetCandidate::Reuse {
                handle,
                build,
                metrics,
            });
        }

        let mut prepared_stage = self.site_model_from_compile_candidate(
            &guide_options,
            environment,
            operation,
            &mut metrics,
            compilation,
        )?;
        let prepared = prepared_stage.guide.clone();
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
                let tree = package_store::template_loader::materialize(
                    &environment.packages,
                    &package_store::TemplatePaths::new(environment.packages.root()),
                    template_coordinate,
                )
                .map_err(|error| {
                    format!("{operation}: materialize {template_coordinate}: {error}")
                })?;
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
        let mut objects =
            environment_objects(&project_objects, &package_lock, environment, operation)?;
        merge_authenticated_objects(&mut objects, &recipe_assets.objects)?;
        let mut ready = recipe_assets.ready.clone();

        let model = self.publisher_model(
            &prepared,
            &project_revision,
            recipe_assets.template_files.as_ref(),
            &mut ready,
            &mut objects,
            &project_id,
            operation,
        )?;

        let render_semantics_key = publisher_render_semantics_cache_key(
            &keys.render_semantics_source,
            &model,
            &options,
            guide_options.build_epoch_secs,
            operation,
        )?;
        let render_packages = publisher_render_package_view(
            environment.scoped(&project.resolved_packages().labels, operation)?,
            &project.resolved_packages().labels,
            operation,
        )?;
        let package_root = render_packages.root().to_string_lossy().into_owned();
        let package_catalog_key = publisher_render_package_catalog_cache_key(
            &prepared,
            &compile_lock,
            &project.resolved_packages().labels,
            &package_root,
            operation,
        )?;
        let retained_catalog = self
            .preparation
            .history
            .iter()
            .filter_map(|generation| generation.render_package_catalog.as_ref())
            .find(|entry| entry.key == package_catalog_key)
            .cloned();
        let retained_semantics = self
            .preparation
            .history
            .iter()
            .filter_map(|generation| generation.render_semantics.as_ref())
            .find(|entry| entry.key == render_semantics_key)
            .cloned();
        let render_semantics = match retained_semantics {
            Some(entry) => {
                metrics.render_semantics_cache_hit = true;
                let semantics = entry.semantics.clone();
                prepared_stage.generation.render_semantics = Some(entry);
                prepared_stage.generation.render_package_catalog = retained_catalog;
                semantics
            }
            None => {
                let reusable_catalog = retained_catalog
                    .as_ref()
                    .and_then(|entry| entry.catalog.clone());
                let render_set = prepared_render_set(&prepared)?;
                let (semantics, package_catalog, package_catalog_hit) =
                    publisher_render_semantics_reusing_package_catalog(
                        render_set,
                        render_packages,
                        &model,
                        &options,
                        package_catalog_key.clone(),
                        reusable_catalog.as_deref(),
                    )?;
                metrics.render_package_catalog_cache_hit = package_catalog_hit;
                metrics.render_package_catalog_built = !package_catalog_hit;
                metrics.render_own_context_built = true;
                metrics.render_own_resources_preparsed = prepared.resources.len() as u64;
                metrics.render_package_catalog_packages = package_catalog.package_count() as u64;
                metrics.render_package_catalog_entries =
                    package_catalog.resource_entry_count() as u64;
                metrics.render_package_catalog_approx_bytes =
                    package_catalog.retained_approx_bytes() as u64;
                #[cfg(test)]
                let admission_limits = self.render_package_catalog_limits.unwrap_or((
                    PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_PACKAGES,
                    PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_RESOURCE_ENTRIES,
                    PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_APPROX_BYTES,
                ));
                #[cfg(not(test))]
                let admission_limits = (
                    PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_PACKAGES,
                    PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_RESOURCE_ENTRIES,
                    PUBLISHER_RENDER_PACKAGE_CATALOG_MAX_APPROX_BYTES,
                );
                metrics.render_package_catalog_admitted =
                    publisher_render_package_catalog_admitted_with_limits(
                        package_catalog.package_count(),
                        package_catalog.resource_entry_count(),
                        package_catalog.retained_approx_bytes(),
                        admission_limits,
                    );
                prepared_stage.generation.render_package_catalog =
                    Some(PublisherRenderPackageCatalogCacheEntry {
                        key: package_catalog_key,
                        catalog: metrics
                            .render_package_catalog_admitted
                            .then_some(package_catalog),
                    });
                let semantics = Rc::new(semantics);
                prepared_stage.generation.render_semantics =
                    Some(PublisherRenderSemanticsCacheEntry {
                        key: render_semantics_key,
                        semantics: semantics.clone(),
                    });
                semantics
            }
        };
        #[cfg(test)]
        if self.fail_after_render_package_catalog_stage && metrics.render_package_catalog_built {
            return Err(
                "prepareProject: injected failure after render package catalog staging".into(),
            );
        }
        let state = publisher_render_state(&render_semantics, &model, &options)?;

        let catalog =
            publisher_output_plan(&ready, state.list_pages(), &model.resource_pages, operation)?;
        let tree_digest = publisher_mounted_tree_digest(&model, operation)?;
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
        let renderer_implementation = site_build::RendererImplementation {
            id: "publisher-template-rust".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            recipe_sha256: site_build::sha256_canonical(&(&template, &parameters))
                .map_err(|error| format!("{operation}: renderer recipe: {error}"))?,
        };
        metrics.total_ms = elapsed_ms(total_started);
        Ok(PublisherTargetCandidate::New(NewPublisherTarget {
            core: PublisherRenderCore {
                recipe_assets: recipe_assets.clone(),
                state,
                catalog,
                ready,
                objects,
                renderer: renderer_implementation,
                output_options: parameters.clone(),
            },
            closure: PublisherClosureInputs {
                preparation_key,
                project: project_revision,
                package_lock,
                prepared: prepared.clone(),
                recipe_assets,
                template,
                fhir_version,
                renderer,
                parameters,
                diagnostics,
            },
            preparation: prepared_stage,
            metrics,
        }))
    }

    fn close_publisher_generation(
        &mut self,
        generation: PublisherTargetCandidate,
    ) -> Result<PrepareResult, String> {
        let operation = "prepare(publisher)";
        match generation {
            PublisherTargetCandidate::Reuse {
                handle,
                build,
                mut metrics,
            } => {
                let completion_started = self.timer();
                self.commit_publisher_reuse(&handle);
                metrics.total_ms += elapsed_ms(completion_started);
                #[cfg(test)]
                {
                    self.preparation.cache_hits.retained_publisher_runtime += 1;
                }
                Ok(PrepareResult {
                    build_id: handle,
                    generator: GeneratorKind::Publisher,
                    site_build: build,
                    measurements: metrics,
                })
            }
            PublisherTargetCandidate::New(mut stage) => {
                let completion_started = self.timer();
                let (artifacts, plan, artifact_objects) = publisher_artifacts(
                    &stage.closure.prepared,
                    &stage.closure.project,
                    &stage.closure.package_lock,
                    stage.closure.recipe_assets.as_ref(),
                    &stage.core.objects,
                )?;
                merge_authenticated_objects(&mut stage.core.objects, &artifact_objects)?;
                #[cfg(test)]
                if self.fail_during_publisher_close {
                    return Err("prepareProject: injected failure during Publisher close".into());
                }
                let build = site_build::SiteBuild::new(
                    stage.closure.project,
                    stage.closure.package_lock,
                    site_build::RenderTarget {
                        renderer: stage.closure.renderer,
                        mode: site_build::RenderMode::NativeTemplate,
                        fhir_version: stage.closure.fhir_version,
                        template: Some(stage.closure.template),
                        parameters: stage.closure.parameters.clone(),
                    },
                    plan,
                    artifacts,
                    stage.closure.diagnostics,
                )
                .map_err(|error| format!("{operation}: SiteBuild: {error}"))?
                .close()
                .map_err(|error| format!("{operation}: close SiteBuild: {error}"))?;
                verify_ready_artifacts(&build, &stage.core.objects, operation)?;
                let handle = build.site_build().build_id().to_string();
                let installed = self.install_publisher(
                    PublisherRuntime {
                        build: build.clone(),
                        core: stage.core,
                    },
                    Some(stage.closure.preparation_key),
                );
                debug_assert_eq!(installed, handle);

                // Every fallible closure/verification operation is complete.
                // Promote all staged preparation facts only now.
                let _ = self.commit_prepared_guide_stage(stage.preparation, &mut stage.metrics);
                stage.metrics.render_package_catalog_retained_generations =
                    self.preparation
                        .history
                        .iter()
                        .filter(|generation| generation.render_package_catalog.is_some())
                        .count() as u64;
                stage.metrics.total_ms += elapsed_ms(completion_started);
                Ok(PrepareResult {
                    build_id: handle,
                    generator: GeneratorKind::Publisher,
                    site_build: build,
                    measurements: stage.metrics,
                })
            }
        }
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
        Ok(self.install_cycle(build, objects, None))
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
        let model = self.publisher_model(
            &prepared,
            build.site_build().project(),
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
        let catalog =
            publisher_output_plan(&ready, state.list_pages(), &model.resource_pages, operation)?;
        let actual_tree = publisher_mounted_tree_digest(&model, operation)?;
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
        let installed = self.install_publisher(
            PublisherRuntime {
                build,
                core: PublisherRenderCore {
                    recipe_assets,
                    state,
                    catalog,
                    ready,
                    objects,
                    renderer,
                    output_options: parameters,
                },
            },
            None,
        );
        // A restored runtime now owns the live RenderState (and its optional
        // SQL database). Drop the independent fresh-prepare semantics slot so a
        // restore cannot retain a third distinct query snapshot.
        for generation in self.preparation.history.iter_mut() {
            generation.render_semantics = None;
        }
        debug_assert_eq!(installed, handle);
        Ok(handle)
    }
}

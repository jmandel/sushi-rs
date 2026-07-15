use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{PackageView, SiteEngine};

const SEMANTIC_COMPILATION_KEY_SCHEMA: &str = "semantic-compilation-key/v1";
const SEMANTIC_COMPILATION_RECIPE: &str = "sushi.compile-project/v1";
const COMPILER_PACKAGE_STORE_KEY_SCHEMA: &str = "compiler-package-store-key/v1";
const COMPILER_PACKAGE_STORE_RECIPE: &str = "package-store.project-index+lazy-json/v1";
const COMPILER_PACKAGE_STORE_RETAINED_CACHE_LIMITS: package_store::PackageStoreCacheLimits =
    package_store::PackageStoreCacheLimits {
        max_entries: 1024,
        max_approximate_source_bytes: 16 * 1024 * 1024,
    };
// This is the semantic recipe version, not a transport-envelope dependency.
// It deliberately retains the value used by the former WASM-owned key.
const SEMANTIC_ENGINE_API: u32 = 1;

/// Exact resolver result whose package bytes are visible to one compilation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPackageClosure {
    pub config_sha256: site_build::Sha256Digest,
    /// Manifests inspected while proving the closure, including packages that
    /// were deliberately excluded from the executable R4 context.
    pub resolution_support: BTreeSet<String>,
    /// Root-first executable PackageContext order.
    pub labels: Vec<String>,
}

impl ResolvedPackageClosure {
    /// Convert the package resolver's satisfied fixpoint into the exact
    /// execution certificate shared by native and WASM hosts.
    pub fn from_resolution_step(
        config: &str,
        step: &package_store::ResolutionStep,
    ) -> Result<Self, String> {
        if !step.satisfied {
            return Err("package resolver step is not satisfied".into());
        }
        let mut labels = Vec::new();
        for request in step.compile_set.iter().chain(&step.context_closure) {
            let label = format!("{}#{}", request.package_id, request.version);
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        if labels.is_empty() {
            return Err("package resolver produced an empty satisfied closure".into());
        }
        let resolution_support = step
            .resolution_support
            .iter()
            .map(|request| format!("{}#{}", request.package_id, request.version))
            .collect();
        Ok(Self {
            config_sha256: site_build::Sha256Digest::of_bytes(config.as_bytes()),
            resolution_support,
            labels,
        })
    }
}

/// One complete immutable project input captured by a `prepare` request. Site
/// bytes are carried here even though only their normalized page listing
/// affects semantic compilation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectRevision {
    pub config: String,
    pub fsh: BTreeMap<String, String>,
    #[cfg_attr(feature = "wire-contract", ts(type = "{ [key in string]: unknown }"))]
    pub predefined: BTreeMap<String, Value>,
    #[serde(with = "base64_file_map")]
    #[cfg_attr(feature = "wire-contract", ts(type = "{ [key in string]: string }"))]
    #[cfg_attr(feature = "wire-contract", schemars(with = "BTreeMap<String, String>"))]
    pub site_files: BTreeMap<String, Vec<u8>>,
}

mod base64_file_map {
    use std::collections::BTreeMap;

    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(files: &BTreeMap<String, Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        files
            .iter()
            .map(|(path, bytes)| (path, STANDARD.encode(bytes)))
            .collect::<BTreeMap<_, _>>()
            .serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<BTreeMap<String, Vec<u8>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        BTreeMap::<String, String>::deserialize(deserializer)?
            .into_iter()
            .map(|(path, encoded)| {
                STANDARD
                    .decode(encoded)
                    .map(|bytes| (path.clone(), bytes))
                    .map_err(|error| {
                        serde::de::Error::custom(format!("invalid base64 in {path}: {error}"))
                    })
            })
            .collect()
    }
}

/// Immutable authored revision installed by the last successful compile call.
#[derive(Clone, Debug)]
pub(crate) struct CompiledProjectRevision {
    config: String,
    fsh: BTreeMap<String, String>,
    predefined: BTreeMap<String, Value>,
    site_files: BTreeMap<String, Vec<u8>>,
    resolved_packages: ResolvedPackageClosure,
}

impl CompiledProjectRevision {
    #[cfg(test)]
    pub(crate) fn new(
        inputs: ProjectRevision,
        resolved_packages: ResolvedPackageClosure,
    ) -> Result<Self, String> {
        Self::capture(inputs, resolved_packages, "project revision")
    }

    fn capture(
        inputs: ProjectRevision,
        resolved_packages: ResolvedPackageClosure,
        operation: &str,
    ) -> Result<Self, String> {
        if resolved_packages.config_sha256
            != site_build::Sha256Digest::of_bytes(inputs.config.as_bytes())
        {
            return Err(format!(
                "{operation}: resolved package closure belongs to different config bytes"
            ));
        }
        validate_local_resource_inputs(&inputs.predefined, &inputs.site_files, operation)?;
        Ok(Self {
            config: inputs.config,
            fsh: inputs.fsh,
            predefined: inputs.predefined,
            site_files: inputs.site_files,
            resolved_packages,
        })
    }

    pub(crate) fn config(&self) -> &str {
        &self.config
    }

    pub(crate) fn fsh(&self) -> &BTreeMap<String, String> {
        &self.fsh
    }

    pub(crate) fn predefined(&self) -> &BTreeMap<String, Value> {
        &self.predefined
    }

    pub(crate) fn site_files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.site_files
    }

    pub(crate) fn resolved_packages(&self) -> &ResolvedPackageClosure {
        &self.resolved_packages
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum CompilationDefinitionKind {
    FshDeclaration,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompilationDefinition {
    pub kind: CompilationDefinitionKind,
    pub path: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum CompilationDiagnosticSeverity {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "wire-contract", ts(optional_fields))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompilationResource {
    pub filename: String,
    pub text: String,
    #[serde(skip)]
    #[cfg_attr(feature = "wire-contract", ts(skip))]
    #[cfg_attr(feature = "wire-contract", schemars(skip))]
    pub body: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "String"))]
    pub resource_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "String"))]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "String"))]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "CompilationDefinition"))]
    pub definition: Option<CompilationDefinition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "wire-contract", ts(optional_fields))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompilationDiagnostic {
    pub severity: CompilationDiagnosticSeverity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "String"))]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "u32"))]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "CompilationDefinition"))]
    pub owner_definition: Option<CompilationDefinition>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompilationOutcome {
    pub resources: Vec<CompilationResource>,
    pub diagnostics: Vec<CompilationDiagnostic>,
}

/// Result plus the private transition facts a composing host needs to
/// invalidate downstream preparation caches. These facts are not wire fields.
#[derive(Debug)]
pub(crate) struct CompilationTransition {
    pub(crate) outcome: CompilationOutcome,
    pub(crate) measurements: CompilationMeasurements,
}

#[derive(Clone, Debug, PartialEq)]
struct SemanticCompilationKey {
    semantic_inputs_sha256: site_build::Sha256Digest,
    resolved_packages: ResolvedPackageClosure,
    package_store: CompilerPackageStoreKey,
}

struct SemanticCompilation {
    key: SemanticCompilationKey,
    compiled: Vec<(PathBuf, Value)>,
    outcome: CompilationOutcome,
    // Authenticated immutable package sources may attach one retained store
    // only after compilation succeeds. Non-retainable sources (notably native
    // disk caches) still compile canonically, but deliberately attach none.
    package_store: Option<Rc<RetainedPackageStore>>,
    #[cfg(feature = "dependency-observation")]
    package_lookups: dependency_observation::PackageLookupTrace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CompilerPackageStoreKey(site_build::Sha256Digest);

struct RetainedPackageStore {
    key: CompilerPackageStoreKey,
    store: package_store::PackageStore,
}

struct WorkingPackageStore {
    key: CompilerPackageStoreKey,
    store: package_store::PackageStore,
    retain_after_success: bool,
}

struct PackageStorePromotion {
    retained: Option<Rc<RetainedPackageStore>>,
    activity: package_store::PackageStoreCacheStats,
    active: package_store::PackageStoreCacheStats,
}

struct CompiledSemanticCandidate {
    semantic: SemanticCompilation,
    package_store_activity: package_store::PackageStoreCacheStats,
    active_package_store: package_store::PackageStoreCacheStats,
}

impl RetainedPackageStore {
    fn fork_for_compile(&self) -> Result<WorkingPackageStore, String> {
        Ok(WorkingPackageStore {
            key: self.key.clone(),
            store: self
                .store
                .fork_for_compile()
                .map_err(|error| format!("compile failed: reuse package store: {error:#}"))?,
            retain_after_success: true,
        })
    }
}

impl WorkingPackageStore {
    fn promote(self) -> Result<PackageStorePromotion, String> {
        if !self.retain_after_success {
            return Ok(PackageStorePromotion {
                retained: None,
                activity: self.store.cache_stats(),
                active: package_store::PackageStoreCacheStats::default(),
            });
        }
        let retained = Rc::new(RetainedPackageStore {
            key: self.key,
            store: self
                .store
                .into_retained(COMPILER_PACKAGE_STORE_RETAINED_CACHE_LIMITS)
                .map_err(|error| format!("compile failed: retain package store: {error:#}"))?,
        });
        let stats = retained.store.cache_stats();
        Ok(PackageStorePromotion {
            retained: Some(retained),
            activity: stats,
            active: stats,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CompilationMeasurements {
    pub(crate) semantic_compilation_cache_hit: bool,
    pub(crate) package_store_cache_hit: bool,
    pub(crate) package_store_used: bool,
    pub(crate) package_store_key_ms: f64,
    pub(crate) package_store_build_ms: f64,
    pub(crate) retained_package_store_generations: usize,
    pub(crate) package_body_cache_hits: u64,
    pub(crate) package_body_cache_misses: u64,
    pub(crate) package_body_cache_inserts: u64,
    pub(crate) package_body_cache_evictions: u64,
    pub(crate) active_package_body_entries: usize,
    pub(crate) active_package_body_approximate_source_bytes: usize,
    pub(crate) retained_package_catalog_generations: usize,
    pub(crate) retained_package_catalog_entries: usize,
    pub(crate) retained_package_body_logical_entries: usize,
    pub(crate) retained_package_body_logical_approximate_source_bytes: usize,
    pub(crate) retained_package_body_unique_entries: usize,
    pub(crate) retained_package_body_unique_approximate_source_bytes: usize,
}

impl CompilationMeasurements {
    fn record_retained(&mut self, retained: package_store::PackageStoreRetainedStats) {
        self.retained_package_store_generations = retained.store_generations;
        self.retained_package_catalog_generations = retained.catalog_generations;
        self.retained_package_catalog_entries = retained.catalog_entries;
        self.retained_package_body_logical_entries = retained.parsed_logical_entries;
        self.retained_package_body_logical_approximate_source_bytes =
            retained.parsed_logical_approximate_source_bytes;
        self.retained_package_body_unique_entries = retained.parsed_unique_entries;
        self.retained_package_body_unique_approximate_source_bytes =
            retained.parsed_unique_approximate_source_bytes;
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct RenderSemanticInputs {
    config: String,
    fsh: BTreeMap<String, String>,
    predefined: BTreeMap<String, Value>,
    page_listing: BTreeMap<String, Vec<String>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SemanticCompilationKeyPayload<'a> {
    schema: &'static str,
    recipe: &'static str,
    engine_api: u32,
    inputs: &'a RenderSemanticInputs,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompilerPackageStoreKeyPayload<'a> {
    schema: &'static str,
    recipe: &'static str,
    engine_api: u32,
    config_sha256: &'a site_build::Sha256Digest,
    resolved_labels: Vec<CompilerPackageCarrier>,
    resolution_support: Vec<CompilerPackageCarrier>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompilerPackageCarrier {
    label: String,
    content: site_build::ContentRef,
}

#[derive(Default)]
pub(crate) struct CompilationState {
    active: Option<SemanticCompilation>,
    previous: Option<SemanticCompilation>,
    project: Option<CompiledProjectRevision>,
    #[cfg(test)]
    cache_hits: u64,
}

fn semantic_compilation_key(
    inputs: &RenderSemanticInputs,
    resolved_packages: &ResolvedPackageClosure,
    package_store: &CompilerPackageStoreKey,
    operation: &str,
) -> Result<SemanticCompilationKey, String> {
    let semantic_inputs_sha256 = site_build::sha256_canonical(&SemanticCompilationKeyPayload {
        schema: SEMANTIC_COMPILATION_KEY_SCHEMA,
        recipe: SEMANTIC_COMPILATION_RECIPE,
        engine_api: SEMANTIC_ENGINE_API,
        inputs,
    })
    .map_err(|error| format!("{operation}: hash semantic compilation key: {error}"))?;
    Ok(SemanticCompilationKey {
        semantic_inputs_sha256,
        resolved_packages: resolved_packages.clone(),
        package_store: package_store.clone(),
    })
}

fn compiler_package_store_key(
    config: &str,
    resolved: &ResolvedPackageClosure,
    packages: &PackageView,
    operation: &str,
) -> Result<CompilerPackageStoreKey, String> {
    let carrier = |label: &str| -> Result<CompilerPackageCarrier, String> {
        let content = packages.carrier_identity(label).ok_or_else(|| {
            format!("{operation}: exact carrier identity is missing for resolved package {label}")
        })?;
        Ok(CompilerPackageCarrier {
            label: label.into(),
            content: content.clone(),
        })
    };
    let resolved_labels = resolved
        .labels
        .iter()
        .map(|label| carrier(label))
        .collect::<Result<Vec<_>, _>>()?;
    // `resolution_support` is a BTreeSet in the resolver certificate, so its
    // iteration is the canonical stable order bound into this key.
    let resolution_support = resolved
        .resolution_support
        .iter()
        .map(|label| carrier(label))
        .collect::<Result<Vec<_>, _>>()?;
    let config_sha256 = site_build::Sha256Digest::of_bytes(config.as_bytes());
    let digest = site_build::sha256_canonical(&CompilerPackageStoreKeyPayload {
        schema: COMPILER_PACKAGE_STORE_KEY_SCHEMA,
        recipe: COMPILER_PACKAGE_STORE_RECIPE,
        engine_api: SEMANTIC_ENGINE_API,
        config_sha256: &config_sha256,
        resolved_labels,
        resolution_support,
    })
    .map_err(|error| format!("{operation}: hash compiler package-store key: {error}"))?;
    Ok(CompilerPackageStoreKey(digest))
}

impl CompilationState {
    fn replace_active(&mut self, next: SemanticCompilation) -> bool {
        let same_semantics = self
            .active
            .as_ref()
            .is_some_and(|active| active.key == next.key && active.compiled == next.compiled);
        self.previous = self.active.replace(next);
        !same_semantics
    }

    fn restore(&mut self, key: &SemanticCompilationKey) -> Option<CompilationOutcome> {
        if self.active.as_ref().map(|entry| &entry.key) == Some(key) {
            #[cfg(test)]
            {
                self.cache_hits += 1;
            }
            return self.active.as_ref().map(|entry| entry.outcome.clone());
        }
        if self.previous.as_ref().map(|entry| &entry.key) != Some(key) {
            return None;
        }
        let previous = self.previous.take().expect("previous key matched");
        let outcome = previous.outcome.clone();
        self.replace_active(previous);
        #[cfg(test)]
        {
            self.cache_hits += 1;
        }
        Some(outcome)
    }

    fn retained_package_store(
        &self,
        key: &CompilerPackageStoreKey,
    ) -> Option<Rc<RetainedPackageStore>> {
        [&self.active, &self.previous]
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.package_store.as_ref())
            .find(|store| store.key == *key)
            .cloned()
    }

    #[cfg(test)]
    fn retained_package_store_generations(&self) -> usize {
        self.retained_package_store_stats().store_generations
    }

    fn retained_package_store_stats(&self) -> package_store::PackageStoreRetainedStats {
        package_store::aggregate_retained_stats(
            [&self.active, &self.previous]
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.package_store.as_ref())
                .map(|store| &store.store),
        )
    }
}

impl SiteEngine {
    /// Compile typed project inputs against one explicit resolver-scoped package
    /// view and closure. The successful authored revision and semantic result
    /// are committed together; failures leave both retained generations intact.
    pub(crate) fn compile_project(
        &mut self,
        inputs: ProjectRevision,
        packages: PackageView,
        resolved: ResolvedPackageClosure,
    ) -> Result<CompilationTransition, String> {
        let operation = "compileProject";
        let config_digest = site_build::Sha256Digest::of_bytes(inputs.config.as_bytes());
        if resolved.config_sha256 != config_digest {
            return Err(
                "compileProject: no satisfied package resolver fixpoint for these config bytes; call resolveProject after the latest mount"
                    .into(),
            );
        }
        if !packages.is_scoped_to(&resolved.labels) {
            return Err(
                "compileProject: package view does not match the exact resolved closure".into(),
            );
        }
        let mut measurements = CompilationMeasurements::default();
        let package_store_key_started = self.timer();
        let package_store_key =
            compiler_package_store_key(&inputs.config, &resolved, &packages, operation)?;
        measurements.package_store_key_ms = package_store_key_started.elapsed_ms();
        let page_listing = page_listing_from_site_files(&inputs.site_files);
        let semantic_inputs = RenderSemanticInputs {
            config: inputs.config.clone(),
            fsh: inputs.fsh.clone(),
            predefined: inputs.predefined.clone(),
            page_listing: page_listing
                .iter()
                .map(|(directory, names)| (directory.clone(), names.clone()))
                .collect(),
        };
        let key =
            semantic_compilation_key(&semantic_inputs, &resolved, &package_store_key, operation)?;
        let project =
            CompiledProjectRevision::capture(inputs.clone(), resolved.clone(), operation)?;
        if let Some(outcome) = self.compilation.restore(&key) {
            self.compilation.project = Some(project);
            measurements.semantic_compilation_cache_hit = true;
            let stats = self
                .compilation
                .active
                .as_ref()
                .expect("restored semantic compilation is active")
                .package_store
                .as_ref()
                .map(|store| store.store.cache_stats())
                .unwrap_or_default();
            measurements.record_retained(self.compilation.retained_package_store_stats());
            measurements.active_package_body_entries = stats.entries;
            measurements.active_package_body_approximate_source_bytes =
                stats.approximate_source_bytes;
            return Ok(CompilationTransition {
                outcome,
                measurements,
            });
        }

        measurements.package_store_used = true;
        let package_store = if let Some(store) =
            self.compilation.retained_package_store(&package_store_key)
        {
            measurements.package_store_cache_hit = true;
            store.fork_for_compile()?
        } else {
            let cache_dir = packages.root().to_string_lossy().into_owned();
            let package_store_started = self.timer();
            let (compile_packages, retain_after_success) = match packages.fork_for_compile() {
                Ok(packages) => (packages, true),
                Err(error) if error.kind() == std::io::ErrorKind::Unsupported => (packages, false),
                Err(error) => {
                    return Err(format!("compile failed: isolate package source: {error}"))
                }
            };
            let store = package_store::PackageStore::for_project_with_config(
                compile_packages,
                &inputs.config,
                &cache_dir,
            )
            .map_err(|error| format!("compile failed: package store: {error:#}"))?;
            measurements.package_store_build_ms = package_store_started.elapsed_ms();
            WorkingPackageStore {
                key: package_store_key,
                store,
                retain_after_success,
            }
        };
        let candidate = compile(inputs, package_store, &page_listing, key)?;
        measurements.package_body_cache_hits = candidate.package_store_activity.hits;
        measurements.package_body_cache_misses = candidate.package_store_activity.misses;
        measurements.package_body_cache_inserts = candidate.package_store_activity.inserts;
        measurements.package_body_cache_evictions = candidate.package_store_activity.evictions;
        measurements.active_package_body_entries = candidate.active_package_store.entries;
        measurements.active_package_body_approximate_source_bytes =
            candidate.active_package_store.approximate_source_bytes;
        let outcome = candidate.semantic.outcome.clone();
        let semantic_changed = self.compilation.replace_active(candidate.semantic);
        self.compilation.project = Some(project);
        if semantic_changed {
            self.invalidate_exact_preparation();
        }
        measurements.record_retained(self.compilation.retained_package_store_stats());
        Ok(CompilationTransition {
            outcome,
            measurements,
        })
    }

    pub fn clear_compilation(&mut self) {
        self.compilation = CompilationState::default();
        self.clear_preparation();
    }

    pub fn compiled_resources(&self) -> &[(PathBuf, Value)] {
        self.compilation
            .active
            .as_ref()
            .map(|active| active.compiled.as_slice())
            .unwrap_or_default()
    }

    pub(crate) fn compile_diagnostics(&self) -> &[CompilationDiagnostic] {
        self.compilation
            .active
            .as_ref()
            .map(|active| active.outcome.diagnostics.as_slice())
            .unwrap_or_default()
    }

    #[cfg(feature = "dependency-observation")]
    pub(crate) fn dependency_compilation(
        &self,
    ) -> Option<(
        &CompilationOutcome,
        &dependency_observation::PackageLookupTrace,
    )> {
        self.compilation
            .active
            .as_ref()
            .map(|active| (&active.outcome, &active.package_lookups))
    }

    pub(crate) fn project_revision(&self) -> Option<&CompiledProjectRevision> {
        self.compilation.project.as_ref()
    }

    /// Exact resolver closure bound to the current successful project. Hosts
    /// may use this to construct a snapshot PackageContext without exposing the
    /// executor's private compiled-revision state.
    pub fn resolved_packages(&self) -> Option<&ResolvedPackageClosure> {
        self.project_revision()
            .map(CompiledProjectRevision::resolved_packages)
    }

    #[cfg(test)]
    pub(crate) fn install_compilation_for_test(
        &mut self,
        project: CompiledProjectRevision,
        compiled: Vec<(PathBuf, Value)>,
    ) {
        let semantic_inputs = RenderSemanticInputs {
            config: project.config.clone(),
            fsh: project.fsh.clone(),
            predefined: project.predefined.clone(),
            page_listing: page_listing_from_site_files(&project.site_files)
                .into_iter()
                .collect(),
        };
        let package_store =
            retained_package_store_for_test(&project.config, &project.resolved_packages);
        let key = semantic_compilation_key(
            &semantic_inputs,
            &project.resolved_packages,
            &package_store.key,
            "test fixture",
        )
        .expect("test project semantic key");
        self.compilation.active = Some(SemanticCompilation {
            key,
            compiled,
            outcome: CompilationOutcome {
                resources: Vec::new(),
                diagnostics: Vec::new(),
            },
            package_store: Some(package_store),
            #[cfg(feature = "dependency-observation")]
            package_lookups: Default::default(),
        });
        self.compilation.previous = None;
        self.compilation.project = Some(project);
    }
}

fn compile(
    inputs: ProjectRevision,
    package_store: WorkingPackageStore,
    page_listing: &HashMap<String, Vec<String>>,
    key: SemanticCompilationKey,
) -> Result<CompiledSemanticCandidate, String> {
    let fsh_files = inputs
        .fsh
        .iter()
        .map(|(path, source)| (path.clone(), source.clone()))
        .collect::<Vec<_>>();
    let predefined = ordered_predefined_resources(&inputs.predefined);
    let predefined_for_render = predefined.clone();
    let (compiled, ig_resource, diagnostics) =
        compiler::build_project_in_memory_with_ig_from_store(
            &inputs.config,
            &fsh_files,
            predefined,
            &package_store.store,
            page_listing.clone(),
        )
        .map_err(|error| format!("compile failed: {error:#}"))?;
    #[cfg(feature = "dependency-observation")]
    let dependency_package_lookups = package_store.store.take_dependency_observations();

    let mut render_set = compiled
        .iter()
        .map(|resource| {
            (
                PathBuf::from(format!("/__compiled__/{}", resource.filename)),
                resource.body.clone(),
            )
        })
        .collect::<Vec<_>>();
    for (path, body) in &predefined_for_render {
        render_set.push((predefined_render_path(path)?, body.clone()));
    }
    if let Some(ig) = ig_resource {
        render_set.push((PathBuf::from(format!("/__ig__/{}", ig.filename)), ig.body));
    }
    let resources = compiled.into_iter().map(compilation_resource).collect();
    let diagnostics = diagnostics
        .into_iter()
        .map(|diagnostic| {
            let severity = match diagnostic.severity {
                "error" => CompilationDiagnosticSeverity::Error,
                "warning" => CompilationDiagnosticSeverity::Warning,
                "info" => CompilationDiagnosticSeverity::Info,
                other => {
                    return Err(format!(
                        "unsupported compiler diagnostic severity {other:?}"
                    ))
                }
            };
            Ok(CompilationDiagnostic {
                severity,
                message: diagnostic.message,
                file: diagnostic.file,
                line: diagnostic.line,
                owner_definition: diagnostic.owner_definition.map(compilation_definition),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let package_store = package_store.promote()?;
    Ok(CompiledSemanticCandidate {
        semantic: SemanticCompilation {
            key,
            compiled: render_set,
            outcome: CompilationOutcome {
                resources,
                diagnostics,
            },
            package_store: package_store.retained,
            #[cfg(feature = "dependency-observation")]
            package_lookups: dependency_package_lookups,
        },
        package_store_activity: package_store.activity,
        active_package_store: package_store.active,
    })
}

#[cfg(test)]
fn retained_package_store_for_test(
    config: &str,
    resolved: &ResolvedPackageClosure,
) -> Rc<RetainedPackageStore> {
    let source = Rc::new(package_store::BundleSource::new());
    let root = source.cache_root().to_path_buf();
    let carrier_identities = resolved
        .labels
        .iter()
        .chain(resolved.resolution_support.iter())
        .map(|label| {
            (
                label.clone(),
                site_build::ContentRef::of_bytes(
                    label.as_bytes(),
                    Some(package_store::PREPARED_PACKAGE_MEDIA_TYPE),
                ),
            )
        })
        .collect();
    let view = PackageView::new(
        source,
        root.clone(),
        Some(resolved.labels.iter().cloned().collect()),
    )
    .with_carrier_identities(carrier_identities);
    let key = compiler_package_store_key(config, resolved, &view, "test fixture")
        .expect("test package-store key");
    let cache_dir = root.to_string_lossy().into_owned();
    let store = package_store::PackageStore::for_project_with_config(view, config, &cache_dir)
        .expect("test package store");
    Rc::new(RetainedPackageStore {
        key,
        store: store
            .into_retained(COMPILER_PACKAGE_STORE_RETAINED_CACHE_LIMITS)
            .expect("retain test package store"),
    })
}

fn compilation_resource(resource: compiler::CompiledResource) -> CompilationResource {
    let compiler::CompiledResource {
        filename,
        text,
        body,
        definition,
    } = resource;
    let definition = definition.map(compilation_definition);
    let resource_type = body
        .get("resourceType")
        .and_then(Value::as_str)
        .map(str::to_string);
    let id = body.get("id").and_then(Value::as_str).map(str::to_string);
    let url = body.get("url").and_then(Value::as_str).map(str::to_string);
    CompilationResource {
        filename,
        text,
        body,
        resource_type,
        id,
        url,
        definition,
    }
}

fn compilation_definition(definition: compiler::DefinitionLocation) -> CompilationDefinition {
    CompilationDefinition {
        kind: match definition.kind {
            compiler::DefinitionKind::FshDeclaration => CompilationDefinitionKind::FshDeclaration,
        },
        path: definition.path,
        line: definition.line,
        column: definition.column,
    }
}

fn page_listing_from_site_files(
    site_files: &BTreeMap<String, Vec<u8>>,
) -> HashMap<String, Vec<String>> {
    let mut listing = HashMap::new();
    for path in site_files.keys() {
        for folder in ["pagecontent", "pages", "resource-docs"] {
            let prefix = format!("input/{folder}/");
            if let Some(rest) = path.strip_prefix(&prefix) {
                if !rest.is_empty() && !rest.contains('/') {
                    listing
                        .entry(folder.to_string())
                        .or_insert_with(Vec::new)
                        .push(rest.to_string());
                }
            }
        }
    }
    for names in listing.values_mut() {
        names.sort();
        names.dedup();
    }
    listing
}

fn is_local_resource_json(path: &str) -> bool {
    path.ends_with(".json")
        && (path.starts_with("input/resources/") || path.starts_with("input/examples/"))
}

fn validate_local_resource_inputs(
    predefined: &BTreeMap<String, Value>,
    site_files: &BTreeMap<String, Vec<u8>>,
    operation: &str,
) -> Result<(), String> {
    let expected_browser_paths = predefined
        .keys()
        .filter(|path| is_local_resource_json(path))
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let raw_paths = site_files
        .keys()
        .filter(|path| is_local_resource_json(path))
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if expected_browser_paths != raw_paths {
        let only_predefined = expected_browser_paths
            .difference(&raw_paths)
            .copied()
            .collect::<Vec<_>>();
        let only_raw = raw_paths
            .difference(&expected_browser_paths)
            .copied()
            .collect::<Vec<_>>();
        return Err(format!(
            "{operation}: parsed and raw local-resource JSON paths differ (only parsed: {only_predefined:?}; only raw: {only_raw:?})"
        ));
    }
    for (path, compiled_value) in predefined {
        let bytes = site_files.get(path).ok_or_else(|| {
            format!(
                "{operation}: parsed predefined resource {path} has no exact raw authored bytes"
            )
        })?;
        if path.to_ascii_lowercase().ends_with(".json") {
            let authored_value = serde_json::from_slice::<Value>(bytes)
                .map_err(|error| format!("{operation}: invalid JSON in {path}: {error}"))?;
            if &authored_value != compiled_value {
                return Err(format!(
                    "{operation}: predefined resource {path} differs from the raw authored site file at the same path"
                ));
            }
        } else if !path.to_ascii_lowercase().ends_with(".xml") {
            return Err(format!(
                "{operation}: predefined resource {path} is neither JSON nor XML"
            ));
        }
    }
    Ok(())
}

fn ordered_predefined_resources(resources: &BTreeMap<String, Value>) -> Vec<(PathBuf, Value)> {
    let mut ordered = resources
        .iter()
        .map(|(path, body)| (PathBuf::from(path), body.clone()))
        .collect::<Vec<_>>();
    ordered.sort_by(|(left, _), (right, _)| {
        compiler::predefined::input_path_rank(left)
            .cmp(&compiler::predefined::input_path_rank(right))
            .then_with(|| left.cmp(right))
    });
    ordered
}

fn predefined_render_path(source: &Path) -> Result<PathBuf, String> {
    let source_text = source
        .to_str()
        .ok_or_else(|| format!("compile: local resource path is not UTF-8: {source:?}"))?;
    let source_path = site_build::SourcePath::parse(source_text.to_string())
        .map_err(|error| format!("compile: invalid local resource path {source:?}: {error}"))?;
    Ok(PathBuf::from(format!(
        "/__predefined__/{}",
        source_path.as_str()
    )))
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use package_store::BundleSource;

    use super::*;

    const CONFIG: &str = "id: test\ncanonical: https://example.test\nfhirVersion: 4.0.1\n";
    const FSH_ONLY_CONFIG: &str =
        "id: store-test\ncanonical: https://example.test\nfhirVersion: 4.0.1\nFSHOnly: true\n";

    fn resolved() -> ResolvedPackageClosure {
        ResolvedPackageClosure {
            config_sha256: site_build::Sha256Digest::of_bytes(CONFIG.as_bytes()),
            resolution_support: BTreeSet::from(["hl7.fhir.r4.core#4.0.1".into()]),
            labels: vec!["hl7.fhir.r4.core#4.0.1".into()],
        }
    }

    fn package_view() -> PackageView {
        let resolution = resolved();
        package_view_for(&resolution, "")
    }

    fn package_view_for(resolution: &ResolvedPackageClosure, carrier_tag: &str) -> PackageView {
        let source = Rc::new(BundleSource::new());
        let root = source.cache_root().to_path_buf();
        let carrier_identities = resolution
            .labels
            .iter()
            .chain(resolution.resolution_support.iter())
            .map(|label| {
                let identity = if carrier_tag.is_empty() {
                    label.clone()
                } else {
                    format!("{label}@{carrier_tag}")
                };
                (
                    label.clone(),
                    site_build::ContentRef::of_bytes(
                        identity.as_bytes(),
                        Some(package_store::PREPARED_PACKAGE_MEDIA_TYPE),
                    ),
                )
            })
            .collect();
        PackageView::new(
            source,
            root,
            Some(resolution.labels.iter().cloned().collect()),
        )
        .with_carrier_identities(carrier_identities)
    }

    fn fsh_only_inputs(config: &str, page: &str) -> ProjectRevision {
        ProjectRevision {
            config: config.into(),
            fsh: BTreeMap::new(),
            predefined: BTreeMap::new(),
            site_files: BTreeMap::from([(format!("input/pagecontent/{page}.md"), Vec::new())]),
        }
    }

    fn resolved_for(config: &str, labels: &[&str], support: &[&str]) -> ResolvedPackageClosure {
        ResolvedPackageClosure {
            config_sha256: site_build::Sha256Digest::of_bytes(config.as_bytes()),
            resolution_support: support.iter().map(|label| (*label).into()).collect(),
            labels: labels.iter().map(|label| (*label).into()).collect(),
        }
    }

    fn two_resource_core() -> package_store::PreparedPackage {
        let definition = |id: &str| {
            serde_json::to_vec(&serde_json::json!({
                "resourceType": "StructureDefinition",
                "id": id,
                "url": format!("http://hl7.org/fhir/StructureDefinition/{id}"),
                "version": "4.0.1",
                "name": id,
                "status": "active",
                "kind": "resource",
                "abstract": false,
                "type": id,
                "derivation": "specialization",
                "snapshot": { "element": [{ "id": id, "path": id }] },
                "differential": { "element": [{ "id": id, "path": id }] }
            }))
            .unwrap()
        };
        let index_files = ["Patient", "Observation"].map(|id| {
            serde_json::json!({
                "filename": format!("StructureDefinition-{id}.json"),
                "resourceType": "StructureDefinition",
                "id": id,
                "url": format!("http://hl7.org/fhir/StructureDefinition/{id}"),
                "version": "4.0.1",
                "kind": "resource",
                "type": id
            })
        });
        let index = serde_json::to_vec(&serde_json::json!({
            "index-version": 2,
            "files": index_files
        }))
        .unwrap();
        package_store::PreparedPackage::prepare(
            "hl7.fhir.r4.core#4.0.1",
            BTreeMap::from([
                (
                    "package.json".into(),
                    br#"{"name":"hl7.fhir.r4.core","version":"4.0.1"}"#.to_vec(),
                ),
                (".index.json".into(), index),
                (
                    "StructureDefinition-Patient.json".into(),
                    definition("Patient"),
                ),
                (
                    "StructureDefinition-Observation.json".into(),
                    definition("Observation"),
                ),
            ]),
        )
        .unwrap()
    }

    fn compressed_two_resource_core() -> package_store::PreparedPackage {
        let prepared = two_resource_core();
        package_store::PreparedPackage::decode_expected(&prepared.encode(), &prepared.key).unwrap()
    }

    struct TemporaryPackageCache(PathBuf);

    impl Drop for TemporaryPackageCache {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn disk_package_view(
        resolution: &ResolvedPackageClosure,
    ) -> (PackageView, TemporaryPackageCache) {
        static NEXT_CACHE: AtomicU64 = AtomicU64::new(0);

        let prepared = two_resource_core();
        let artifact = prepared.artifact_bytes();
        let package_dir = std::env::temp_dir()
            .join(format!(
                "site-engine-package-store-{}-{}",
                std::process::id(),
                NEXT_CACHE.fetch_add(1, Ordering::Relaxed)
            ))
            .join(&prepared.label)
            .join("package");
        std::fs::create_dir_all(&package_dir).unwrap();
        for (name, bytes) in prepared.files.materialize_all().unwrap() {
            let path = package_dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, bytes).unwrap();
        }
        let root = package_dir
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let cache = TemporaryPackageCache(root.clone());
        let view = PackageView::new(
            Rc::new(package_store::DiskSource),
            root,
            Some(resolution.labels.iter().cloned().collect()),
        )
        .with_carrier_identities(BTreeMap::from([(
            prepared.label,
            site_build::ContentRef::of_bytes(
                artifact.as_ref(),
                Some(package_store::PREPARED_PACKAGE_MEDIA_TYPE),
            ),
        )]));
        (view, cache)
    }

    fn profile_inputs(config: &str, parent: &str, page: &str) -> ProjectRevision {
        ProjectRevision {
            config: config.into(),
            fsh: BTreeMap::from([(
                "input/fsh/profile.fsh".into(),
                format!("Profile: CacheProfile\nParent: {parent}\nId: cache-profile\n"),
            )]),
            predefined: BTreeMap::new(),
            site_files: BTreeMap::from([(format!("input/pagecontent/{page}.md"), Vec::new())]),
        }
    }

    fn assert_compilation_outcome_eq(warm: &CompilationOutcome, fresh: &CompilationOutcome) {
        let resources = |outcome: &CompilationOutcome| {
            outcome
                .resources
                .iter()
                .map(|resource| {
                    (
                        resource.filename.clone(),
                        resource.text.clone(),
                        resource.body.clone(),
                        resource.resource_type.clone(),
                        resource.id.clone(),
                        resource.url.clone(),
                        resource.definition.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(resources(warm), resources(fresh));
        assert_eq!(warm.diagnostics, fresh.diagnostics);
    }

    fn inputs_for(parent: &str, site_body: &[u8]) -> ProjectRevision {
        ProjectRevision {
            config: CONFIG.into(),
            fsh: BTreeMap::from([(
                "input/fsh/test.fsh".into(),
                format!("Profile: Test\nParent: {parent}\n"),
            )]),
            predefined: BTreeMap::new(),
            site_files: BTreeMap::from([("input/pagecontent/index.md".into(), site_body.to_vec())]),
        }
    }

    fn compilation(name: &str, parent: &str) -> SemanticCompilation {
        let inputs = inputs_for(parent, b"old prose");
        let page_listing = page_listing_from_site_files(&inputs.site_files);
        let semantic_inputs = RenderSemanticInputs {
            config: inputs.config,
            fsh: inputs.fsh,
            predefined: inputs.predefined,
            page_listing: page_listing.into_iter().collect(),
        };
        let body = serde_json::json!({
            "resourceType": "StructureDefinition",
            "id": name
        });
        let package_store = retained_package_store_for_test(CONFIG, &resolved());
        SemanticCompilation {
            key: semantic_compilation_key(
                &semantic_inputs,
                &resolved(),
                &package_store.key,
                "test",
            )
            .unwrap(),
            compiled: vec![(
                PathBuf::from(format!("/__compiled__/StructureDefinition-{name}.json")),
                body.clone(),
            )],
            outcome: CompilationOutcome {
                resources: vec![CompilationResource {
                    filename: format!("StructureDefinition-{name}.json"),
                    text: serde_json::to_string(&body).unwrap(),
                    body,
                    resource_type: Some("StructureDefinition".into()),
                    id: Some(name.into()),
                    url: None,
                    definition: None,
                }],
                diagnostics: vec![CompilationDiagnostic {
                    severity: CompilationDiagnosticSeverity::Info,
                    message: format!("compiled {name}"),
                    file: None,
                    line: None,
                    owner_definition: None,
                }],
            },
            package_store: Some(package_store),
            #[cfg(feature = "dependency-observation")]
            package_lookups: Default::default(),
        }
    }

    fn reuse_engine() -> SiteEngine {
        let inputs = inputs_for("Patient", b"old prose");
        let mut engine = SiteEngine::default();
        engine
            .compilation
            .replace_active(compilation("Test", "Patient"));
        engine.compilation.previous = None;
        engine.compilation.project = Some(CompiledProjectRevision {
            config: inputs.config,
            fsh: inputs.fsh,
            predefined: inputs.predefined,
            site_files: inputs.site_files,
            resolved_packages: resolved(),
        });
        engine
    }

    #[test]
    fn exact_semantic_result_reuses_but_replaces_authored_site_revision() {
        let mut engine = reuse_engine();
        let transition = engine
            .compile_project(
                inputs_for("Patient", b"new prose"),
                package_view(),
                resolved(),
            )
            .unwrap();

        assert_eq!(engine.compilation.cache_hits, 1);
        assert_eq!(
            transition.outcome.resources[0].filename,
            "StructureDefinition-Test.json"
        );
        assert_eq!(
            engine
                .project_revision()
                .unwrap()
                .site_files()
                .get("input/pagecontent/index.md")
                .map(Vec::as_slice),
            Some(b"new prose".as_slice())
        );
    }

    #[test]
    fn local_resources_preserve_stock_resources_before_examples_order() {
        let ordered = ordered_predefined_resources(&BTreeMap::from([
            (
                "input/examples/Patient-shared.json".into(),
                serde_json::json!({"resourceType":"Patient","id":"shared"}),
            ),
            (
                "input/resources/Patient-shared.json".into(),
                serde_json::json!({"resourceType":"Patient","id":"shared"}),
            ),
        ]));
        assert_eq!(
            ordered
                .iter()
                .map(|(path, _)| path.as_path())
                .collect::<Vec<_>>(),
            vec![
                Path::new("input/resources/Patient-shared.json"),
                Path::new("input/examples/Patient-shared.json"),
            ]
        );
        assert_eq!(
            predefined_render_path(&ordered[1].0).unwrap(),
            PathBuf::from("/__predefined__/input/examples/Patient-shared.json")
        );
    }

    #[test]
    fn project_capture_rejects_split_or_divergent_local_resource_channels() {
        let path = "input/resources/Patient-p.json";
        let parsed = serde_json::json!({"resourceType":"Patient","id":"p"});
        let base = ProjectRevision {
            config: CONFIG.into(),
            fsh: BTreeMap::new(),
            predefined: BTreeMap::from([(path.into(), parsed)]),
            site_files: BTreeMap::new(),
        };
        assert!(CompiledProjectRevision::new(base.clone(), resolved())
            .unwrap_err()
            .contains("only parsed"));

        let mut divergent = base;
        divergent.site_files.insert(
            path.into(),
            br#"{"resourceType":"Patient","id":"other"}"#.to_vec(),
        );
        assert!(CompiledProjectRevision::new(divergent, resolved())
            .unwrap_err()
            .contains("differs from the raw authored site file"));

        let wrong_closure = ResolvedPackageClosure {
            config_sha256: site_build::Sha256Digest::of_bytes(b"different config"),
            ..resolved()
        };
        assert!(
            CompiledProjectRevision::new(inputs_for("Patient", b"prose"), wrong_closure)
                .unwrap_err()
                .contains("different config bytes")
        );
    }

    #[test]
    fn reuse_invalidates_for_every_compiler_visible_identity_change() {
        let baseline = inputs_for("Patient", b"new prose");
        let mut cases = Vec::new();

        let mut config = baseline.clone();
        config.config.push_str("status: active\n");
        let mut config_resolved = resolved();
        config_resolved.config_sha256 =
            site_build::Sha256Digest::of_bytes(config.config.as_bytes());
        cases.push(("config", config, config_resolved));

        cases.push(("FSH", inputs_for("Observation", b"new prose"), resolved()));

        let mut predefined = baseline.clone();
        predefined.predefined.insert(
            "input/resources/Patient-p.json".into(),
            serde_json::json!({"resourceType":"Patient","id":"p"}),
        );
        predefined.site_files.insert(
            "input/resources/Patient-p.json".into(),
            br#"{"resourceType":"Patient","id":"p"}"#.to_vec(),
        );
        cases.push(("predefined", predefined, resolved()));

        let mut listing = baseline.clone();
        listing
            .site_files
            .insert("input/pagecontent/next.md".into(), b"next".to_vec());
        cases.push(("page listing", listing, resolved()));

        let mut closure = resolved();
        closure.labels.push("hl7.terminology.r4#7.2.0".into());
        cases.push(("closure", baseline, closure));

        for (label, inputs, closure) in cases {
            let mut engine = reuse_engine();
            let _ = engine.compile_project(inputs, package_view(), closure);
            assert_eq!(engine.compilation.cache_hits, 0, "{label}");
        }
    }

    #[test]
    fn previous_exact_compilation_reopens_across_a_b_a() {
        let mut engine = reuse_engine();
        engine
            .compilation
            .replace_active(compilation("Other", "Observation"));

        let transition = engine
            .compile_project(
                inputs_for("Patient", b"current A"),
                package_view(),
                resolved(),
            )
            .unwrap();

        assert_eq!(engine.compilation.cache_hits, 1);
        assert_eq!(
            transition.outcome.resources[0].filename,
            "StructureDefinition-Test.json"
        );
        assert_eq!(engine.compiled_resources()[0].1["id"], "Test");
        assert_eq!(engine.compile_diagnostics()[0].message, "compiled Test");
        assert_eq!(
            engine
                .compilation
                .previous
                .as_ref()
                .unwrap()
                .outcome
                .resources[0]
                .filename,
            "StructureDefinition-Other.json"
        );
    }

    #[test]
    fn failed_compile_preserves_generations_and_authored_revision() {
        let mut engine = reuse_engine();
        engine
            .compilation
            .replace_active(compilation("Other", "Observation"));
        let active_key = engine.compilation.active.as_ref().unwrap().key.clone();
        let previous_key = engine.compilation.previous.as_ref().unwrap().key.clone();
        let prior_site = engine.project_revision().unwrap().site_files().clone();

        let mut invalid = inputs_for("Encounter", b"failed authored");
        invalid.config = "id: [".into();
        let mut invalid_closure = resolved();
        invalid_closure.config_sha256 =
            site_build::Sha256Digest::of_bytes(invalid.config.as_bytes());
        let error = engine
            .compile_project(invalid, package_view(), invalid_closure)
            .unwrap_err();
        assert!(!error.is_empty());
        assert_eq!(engine.compilation.active.as_ref().unwrap().key, active_key);
        assert_eq!(
            engine.compilation.previous.as_ref().unwrap().key,
            previous_key
        );
        assert_eq!(engine.project_revision().unwrap().site_files(), &prior_site);
        assert_eq!(engine.compilation.cache_hits, 0);
    }

    #[test]
    fn compiler_package_store_key_binds_config_order_support_and_carriers() {
        let core = "hl7.fhir.r4.core#4.0.1";
        let terminology = "hl7.terminology.r4#6.2.0";
        let base = resolved_for(FSH_ONLY_CONFIG, &[core, terminology], &[core]);
        let base_view = package_view_for(&base, "carrier-a");
        let base_key =
            compiler_package_store_key(FSH_ONLY_CONFIG, &base, &base_view, "test").unwrap();
        let source = Rc::new(BundleSource::new());
        let root = source.cache_root().to_path_buf();
        let identityless =
            PackageView::new(source, root, Some(base.labels.iter().cloned().collect()));
        assert!(
            compiler_package_store_key(FSH_ONLY_CONFIG, &base, &identityless, "test",)
                .unwrap_err()
                .contains("carrier identity")
        );

        let reordered = resolved_for(FSH_ONLY_CONFIG, &[terminology, core], &[core]);
        assert_ne!(
            base_key,
            compiler_package_store_key(
                FSH_ONLY_CONFIG,
                &reordered,
                &package_view_for(&reordered, "carrier-a"),
                "test",
            )
            .unwrap()
        );

        let support_changed =
            resolved_for(FSH_ONLY_CONFIG, &[core, terminology], &[core, terminology]);
        assert_ne!(
            base_key,
            compiler_package_store_key(
                FSH_ONLY_CONFIG,
                &support_changed,
                &package_view_for(&support_changed, "carrier-a"),
                "test",
            )
            .unwrap()
        );

        assert_ne!(
            base_key,
            compiler_package_store_key(
                FSH_ONLY_CONFIG,
                &base,
                &package_view_for(&base, "carrier-b"),
                "test",
            )
            .unwrap()
        );

        let changed_config = format!("{FSH_ONLY_CONFIG}status: active\n");
        let config_resolution = resolved_for(&changed_config, &[core, terminology], &[core]);
        assert_ne!(
            base_key,
            compiler_package_store_key(
                &changed_config,
                &config_resolution,
                &package_view_for(&config_resolution, "carrier-a"),
                "test",
            )
            .unwrap()
        );
    }

    #[test]
    fn ordinary_revision_reuses_one_prebuilt_compiler_package_store() {
        let core = "hl7.fhir.r4.core#4.0.1";
        let resolution = resolved_for(FSH_ONLY_CONFIG, &[core], &[core]);
        let mut engine = SiteEngine::default();

        let first = engine
            .compile_project(
                fsh_only_inputs(FSH_ONLY_CONFIG, "first"),
                package_view_for(&resolution, "same-carrier"),
                resolution.clone(),
            )
            .unwrap();
        assert!(first.measurements.package_store_used);
        assert!(!first.measurements.package_store_cache_hit);
        assert_eq!(first.measurements.retained_package_store_generations, 1);
        let first_store = engine
            .compilation
            .active
            .as_ref()
            .unwrap()
            .package_store
            .as_ref()
            .unwrap()
            .clone();
        let first_cache = first_store.store.cache_stats();

        let second = engine
            .compile_project(
                fsh_only_inputs(FSH_ONLY_CONFIG, "second"),
                package_view_for(&resolution, "same-carrier"),
                resolution,
            )
            .unwrap();
        assert!(second.measurements.package_store_used);
        assert!(second.measurements.package_store_cache_hit);
        assert_eq!(second.measurements.package_store_build_ms, 0.0);
        assert_eq!(second.measurements.retained_package_store_generations, 2);
        assert_eq!(second.measurements.retained_package_catalog_generations, 1);
        assert!(second.measurements.retained_package_catalog_entries > 0);
        assert!(!Rc::ptr_eq(
            &first_store,
            engine
                .compilation
                .active
                .as_ref()
                .unwrap()
                .package_store
                .as_ref()
                .unwrap()
        ));
        assert_eq!(first_store.store.cache_stats(), first_cache);
    }

    #[test]
    fn fresh_disk_source_compiles_without_retaining_package_store() {
        let core = "hl7.fhir.r4.core#4.0.1";
        let resolution = resolved_for(FSH_ONLY_CONFIG, &[core], &[core]);
        let (package_view, _cache) = disk_package_view(&resolution);
        let mut engine = SiteEngine::default();

        for page in ["first", "second"] {
            let transition = engine
                .compile_project(
                    profile_inputs(FSH_ONLY_CONFIG, "Patient", page),
                    package_view.clone(),
                    resolution.clone(),
                )
                .unwrap();
            assert!(transition.measurements.package_store_used);
            assert!(!transition.measurements.package_store_cache_hit);
            assert!(transition.measurements.package_body_cache_misses > 0);
            assert!(transition.measurements.package_body_cache_inserts > 0);
            assert_eq!(transition.measurements.active_package_body_entries, 0);
            assert_eq!(
                transition
                    .measurements
                    .active_package_body_approximate_source_bytes,
                0
            );
            assert_eq!(
                transition.measurements.retained_package_store_generations,
                0
            );
            assert_eq!(
                transition.measurements.retained_package_catalog_generations,
                0
            );
            assert_eq!(
                transition
                    .measurements
                    .retained_package_body_logical_entries,
                0
            );
            assert_eq!(
                transition.measurements.retained_package_body_unique_entries,
                0
            );
            assert!(engine
                .compilation
                .active
                .as_ref()
                .unwrap()
                .package_store
                .is_none());
            assert_eq!(engine.compilation.retained_package_store_generations(), 0);
        }
    }

    #[test]
    fn same_key_failed_compile_cannot_mutate_retained_parsed_bodies() {
        assert_eq!(
            COMPILER_PACKAGE_STORE_RETAINED_CACHE_LIMITS.max_entries,
            1024
        );
        assert_eq!(
            COMPILER_PACKAGE_STORE_RETAINED_CACHE_LIMITS.max_approximate_source_bytes,
            16 * 1024 * 1024
        );

        let core = "hl7.fhir.r4.core#4.0.1";
        let resolution = resolved_for(FSH_ONLY_CONFIG, &[core], &[core]);
        let environment = crate::PackageEnvironment::new([compressed_two_resource_core()]).unwrap();
        let package_view = environment.scoped(&resolution.labels, "test").unwrap();
        let mut engine = SiteEngine::default();

        let first = engine
            .compile_project(
                profile_inputs(FSH_ONLY_CONFIG, "Patient", "first"),
                package_view.clone(),
                resolution.clone(),
            )
            .unwrap();
        assert!(first.measurements.package_body_cache_misses > 0);
        assert!(first.measurements.package_body_cache_inserts > 0);
        let retained = engine
            .compilation
            .active
            .as_ref()
            .unwrap()
            .package_store
            .as_ref()
            .unwrap()
            .clone();
        let retained_stats = retained.store.cache_stats();
        let retained_generations = engine.compilation.retained_package_store_generations();

        // Compilation reads the previously unseen Observation parent before
        // `compile` projects predefined resources into the render set. The
        // unsafe path therefore creates a deliberately late failure after the
        // working cache has changed, exercising the real success boundary.
        let mut failed = profile_inputs(FSH_ONLY_CONFIG, "Observation", "failed");
        let unsafe_path = "input/resources/../late-failure.json".to_string();
        let unsafe_value = serde_json::json!({
            "resourceType": "Patient",
            "id": "late-failure"
        });
        failed
            .predefined
            .insert(unsafe_path.clone(), unsafe_value.clone());
        failed
            .site_files
            .insert(unsafe_path, serde_json::to_vec(&unsafe_value).unwrap());
        let error = engine
            .compile_project(failed, package_view.clone(), resolution.clone())
            .unwrap_err();
        assert!(error.contains("invalid local resource path"), "{error}");
        assert_eq!(
            engine.compilation.retained_package_store_generations(),
            retained_generations
        );
        assert!(Rc::ptr_eq(
            &retained,
            &engine
                .compilation
                .active
                .as_ref()
                .unwrap()
                .package_store
                .as_ref()
                .unwrap()
        ));
        assert!(engine.compilation.previous.is_none());
        assert_eq!(retained.store.cache_stats(), retained_stats);

        // A later successful same-key miss promotes a distinct fork. It reads
        // the new Observation body while also hitting the retained Patient,
        // and the untouched prior store becomes the previous generation.
        let mut successor_inputs = profile_inputs(FSH_ONLY_CONFIG, "Observation", "successor");
        successor_inputs.fsh.insert(
            "input/fsh/patient-cache.fsh".into(),
            "Profile: PatientCacheProfile\nParent: Patient\nId: patient-cache-profile\n".into(),
        );
        let successor = engine
            .compile_project(successor_inputs.clone(), package_view, resolution.clone())
            .unwrap();
        assert!(successor.measurements.package_store_cache_hit);
        assert!(successor.measurements.package_body_cache_hits > 0);
        assert!(successor.measurements.package_body_cache_misses > 0);
        assert!(successor.measurements.package_body_cache_inserts > 0);
        assert_eq!(successor.measurements.retained_package_store_generations, 2);
        assert_eq!(
            successor.measurements.retained_package_catalog_generations,
            1
        );
        assert!(successor.measurements.active_package_body_entries > retained_stats.entries);
        assert_eq!(
            successor.measurements.retained_package_body_logical_entries,
            retained_stats.entries + successor.measurements.active_package_body_entries
        );
        assert_eq!(
            successor.measurements.retained_package_body_unique_entries,
            successor.measurements.active_package_body_entries
        );
        assert!(
            successor
                .measurements
                .retained_package_body_unique_approximate_source_bytes
                < successor
                    .measurements
                    .retained_package_body_logical_approximate_source_bytes
        );
        let active = engine.compilation.active.as_ref().unwrap();
        let previous = engine.compilation.previous.as_ref().unwrap();
        assert!(!Rc::ptr_eq(
            &retained,
            active.package_store.as_ref().unwrap()
        ));
        assert!(Rc::ptr_eq(
            &retained,
            previous.package_store.as_ref().unwrap()
        ));
        assert_eq!(retained.store.cache_stats(), retained_stats);

        // A clean compiler over the same exact carrier must produce the full
        // semantic outcome and render set byte-for-byte. This exercises the
        // warm mixed-hit/miss path rather than merely comparing helper APIs on
        // an empty cache.
        let fresh_environment =
            crate::PackageEnvironment::new([compressed_two_resource_core()]).unwrap();
        let fresh_view = fresh_environment
            .scoped(&resolution.labels, "fresh parity")
            .unwrap();
        let mut fresh_engine = SiteEngine::default();
        let fresh = fresh_engine
            .compile_project(successor_inputs, fresh_view, resolution)
            .unwrap();
        assert_compilation_outcome_eq(&successor.outcome, &fresh.outcome);
        assert_eq!(
            engine.compiled_resources(),
            fresh_engine.compiled_resources()
        );
    }

    #[test]
    fn failed_store_candidate_preserves_two_successes_and_third_success_evicts_oldest() {
        let core = "hl7.fhir.r4.core#4.0.1";
        let resolution = resolved_for(FSH_ONLY_CONFIG, &[core], &[core]);
        let mut engine = SiteEngine::default();
        engine
            .compile_project(
                fsh_only_inputs(FSH_ONLY_CONFIG, "a"),
                package_view_for(&resolution, "carrier-a"),
                resolution.clone(),
            )
            .unwrap();
        let a_key = engine
            .compilation
            .active
            .as_ref()
            .unwrap()
            .package_store
            .as_ref()
            .unwrap()
            .key
            .clone();
        engine
            .compile_project(
                fsh_only_inputs(FSH_ONLY_CONFIG, "b"),
                package_view_for(&resolution, "carrier-b"),
                resolution.clone(),
            )
            .unwrap();
        let b_key = engine
            .compilation
            .active
            .as_ref()
            .unwrap()
            .package_store
            .as_ref()
            .unwrap()
            .key
            .clone();
        assert_eq!(engine.compilation.retained_package_store_generations(), 2);

        let invalid_config = "id: failed\ncanonical: []\nfhirVersion: 4.0.1\nFSHOnly: true\n";
        let invalid_resolution = resolved_for(invalid_config, &[core], &[core]);
        assert!(engine
            .compile_project(
                fsh_only_inputs(invalid_config, "failed"),
                package_view_for(&invalid_resolution, "carrier-c"),
                invalid_resolution,
            )
            .is_err());
        assert_eq!(engine.compilation.retained_package_store_generations(), 2);
        assert_eq!(
            engine
                .compilation
                .active
                .as_ref()
                .unwrap()
                .package_store
                .as_ref()
                .unwrap()
                .key,
            b_key
        );
        assert_eq!(
            engine
                .compilation
                .previous
                .as_ref()
                .unwrap()
                .package_store
                .as_ref()
                .unwrap()
                .key,
            a_key
        );

        let third = engine
            .compile_project(
                fsh_only_inputs(FSH_ONLY_CONFIG, "c"),
                package_view_for(&resolution, "carrier-c"),
                resolution,
            )
            .unwrap();
        assert_eq!(third.measurements.retained_package_store_generations, 2);
        assert_eq!(
            engine
                .compilation
                .previous
                .as_ref()
                .unwrap()
                .package_store
                .as_ref()
                .unwrap()
                .key,
            b_key
        );
        assert!(engine.compilation.retained_package_store(&a_key).is_none());
    }

    #[test]
    fn atomic_prepare_preserves_typed_compilation_on_generator_failure() {
        let mut engine = reuse_engine();
        let core_label = "hl7.fhir.r4.core#4.0.1";
        let package = package_store::PreparedPackage::prepare(
            core_label,
            BTreeMap::from([(
                "package.json".into(),
                br#"{"name":"hl7.fhir.r4.core","version":"4.0.1"}"#.to_vec(),
            )]),
        )
        .unwrap();
        let environment = crate::PackageEnvironment::new([package]).unwrap();
        let exact_view = environment.scoped(&resolved().labels, "test").unwrap();
        engine
            .compilation
            .active
            .as_mut()
            .unwrap()
            .key
            .package_store =
            compiler_package_store_key(CONFIG, &resolved(), &exact_view, "test").unwrap();

        let error = engine
            .prepare_values(
                inputs_for("Patient", b"new prose"),
                resolved(),
                crate::GeneratorSpec::Cycle {
                    build_epoch_secs: 1,
                    liquid_asset_dirs: Vec::new(),
                    branch: None,
                    revision: None,
                },
                environment,
            )
            .unwrap_err();
        assert_eq!(error.phase, crate::BuildErrorPhase::Preparation);
        assert!(
            error.message.contains("ImplementationGuide"),
            "{}",
            error.message
        );
        let compilation = error
            .successful_compilation
            .expect("site failure retains successful compilation");
        assert_eq!(
            compilation.resources[0].filename,
            "StructureDefinition-Test.json"
        );
    }

    #[test]
    fn adapter_cancellation_checkpoint_stops_after_config_before_resolution() {
        struct CancelAfterConfig(std::cell::Cell<bool>);
        impl crate::ProjectSource for CancelAfterConfig {
            fn cancelled(&self) -> bool {
                self.0.get()
            }
            fn config(&mut self) -> Result<String, String> {
                self.0.set(true);
                Ok("fhirVersion: 4.0.1".into())
            }
            fn capture(
                &mut self,
                _packages: &crate::PackageEnvironment,
                _resolved: &crate::ResolvedPackageClosure,
            ) -> Result<crate::ProjectRevision, String> {
                panic!("cancelled source must not be captured")
            }
        }
        struct UnusedPackages;
        impl crate::PackageProvider for UnusedPackages {
            fn resolve(
                &mut self,
                _config: &str,
                _generator: &crate::GeneratorSpec,
            ) -> Result<crate::ResolvedPackageClosure, String> {
                panic!("cancelled prepare must not resolve packages")
            }
            fn environment(
                &mut self,
                _resolved: &crate::ResolvedPackageClosure,
            ) -> Result<crate::PackageEnvironment, String> {
                panic!("cancelled prepare must not transport packages")
            }
        }
        let error = SiteEngine::default()
            .prepare_project(
                &mut CancelAfterConfig(std::cell::Cell::new(false)),
                &mut UnusedPackages,
                crate::GeneratorSpec::Cycle {
                    build_epoch_secs: 1,
                    liquid_asset_dirs: Vec::new(),
                    branch: None,
                    revision: None,
                },
            )
            .unwrap_err();
        assert_eq!(error.code, crate::BuildErrorCode::Cancelled);
        assert_eq!(error.operation, crate::BuildOperation::Prepare);
    }

    #[test]
    fn project_source_cancellation_is_observed_after_package_resolution() {
        use std::cell::Cell;
        use std::rc::Rc;

        struct Source(Rc<Cell<bool>>);
        impl crate::ProjectSource for Source {
            fn cancelled(&self) -> bool {
                self.0.get()
            }
            fn config(&mut self) -> Result<String, String> {
                Ok(CONFIG.into())
            }
            fn capture(
                &mut self,
                _packages: &crate::PackageEnvironment,
                _resolved: &crate::ResolvedPackageClosure,
            ) -> Result<crate::ProjectRevision, String> {
                panic!("cancelled source must not be captured")
            }
        }

        struct CancelDuringResolution(Rc<Cell<bool>>);
        impl crate::PackageProvider for CancelDuringResolution {
            fn resolve(
                &mut self,
                _config: &str,
                _generator: &crate::GeneratorSpec,
            ) -> Result<crate::ResolvedPackageClosure, String> {
                self.0.set(true);
                Ok(resolved())
            }
            fn environment(
                &mut self,
                _resolved: &crate::ResolvedPackageClosure,
            ) -> Result<crate::PackageEnvironment, String> {
                panic!("cancelled prepare must not transport packages")
            }
        }

        let cancelled = Rc::new(Cell::new(false));
        let error = SiteEngine::default()
            .prepare_project(
                &mut Source(Rc::clone(&cancelled)),
                &mut CancelDuringResolution(cancelled),
                crate::GeneratorSpec::Cycle {
                    build_epoch_secs: 1,
                    liquid_asset_dirs: Vec::new(),
                    branch: None,
                    revision: None,
                },
            )
            .unwrap_err();
        assert_eq!(error.code, crate::BuildErrorCode::Cancelled);
        assert_eq!(error.phase, crate::BuildErrorPhase::Lifecycle);
    }

    #[test]
    fn third_distinct_success_evicts_only_oldest_compilation() {
        let mut engine = reuse_engine();
        engine
            .compilation
            .replace_active(compilation("Other", "Observation"));
        engine
            .compilation
            .replace_active(compilation("Third", "Encounter"));

        let a_inputs = inputs_for("Patient", b"A");
        let a_semantics = RenderSemanticInputs {
            config: a_inputs.config,
            fsh: a_inputs.fsh,
            predefined: a_inputs.predefined,
            page_listing: page_listing_from_site_files(&a_inputs.site_files)
                .into_iter()
                .collect(),
        };
        let package_store = retained_package_store_for_test(CONFIG, &resolved());
        let a_key = semantic_compilation_key(&a_semantics, &resolved(), &package_store.key, "test")
            .unwrap();
        assert!(engine.compilation.restore(&a_key).is_none());
        assert_eq!(engine.compilation.cache_hits, 0);

        let b_inputs = inputs_for("Observation", b"B");
        let b_semantics = RenderSemanticInputs {
            config: b_inputs.config,
            fsh: b_inputs.fsh,
            predefined: b_inputs.predefined,
            page_listing: page_listing_from_site_files(&b_inputs.site_files)
                .into_iter()
                .collect(),
        };
        let b_key = semantic_compilation_key(&b_semantics, &resolved(), &package_store.key, "test")
            .unwrap();
        let outcome = engine.compilation.restore(&b_key).unwrap();
        assert_eq!(
            outcome.resources[0].filename,
            "StructureDefinition-Other.json"
        );
        assert_eq!(engine.compilation.cache_hits, 1);
    }
}

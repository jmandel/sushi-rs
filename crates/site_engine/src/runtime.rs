use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

use serde::Serialize;

use crate::RenderState;

const RETAINED_SITE_BUILD_LIMIT: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "wire-contract", ts(optional_fields))]
#[serde(rename_all = "camelCase")]
pub struct OutputDescriptor {
    #[cfg_attr(feature = "wire-contract", ts(type = "string"))]
    #[cfg_attr(feature = "wire-contract", schemars(with = "String"))]
    pub path: site_build::OutputPath,
    pub kind: OutputKind,
    pub media_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "site_build::ContentRef"))]
    pub content: Option<site_build::ContentRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "String"))]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "OutputResourceSubject"))]
    pub subject: Option<OutputResourceSubject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "OutputSubjectPage"))]
    pub subject_page: Option<OutputSubjectPage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "OutputPageKind"))]
    pub page_kind: Option<OutputPageKind>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct OutputResourceSubject {
    pub resource_type: String,
    pub id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum OutputSubjectPage {
    Primary,
    Companion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum OutputKind {
    Page,
    Asset,
    Auxiliary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum OutputPageKind {
    Narrative,
    Artifacts,
    Profile,
    ProfileCompanion,
    #[serde(rename = "valueset")]
    ValueSet,
    #[serde(rename = "codesystem")]
    CodeSystem,
    Example,
    Toc,
    Validation,
    Generic,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct OutputCatalog {
    pub build_id: String,
    pub outputs: Vec<OutputDescriptor>,
}

#[derive(Clone)]
pub(crate) struct PreparedOutput {
    pub content: site_build::ContentRef,
    pub producer: site_build::OutputProducer,
    pub source: Option<String>,
    pub owner: Option<site_build::OutputPath>,
}

/// Recipe-identical immutable Publisher implementation assets shared only
/// through the already bounded current/previous runtimes. This is not a build
/// handoff or an independent cache: every successor still constructs its own
/// current model, render state, SiteBuild, catalog, and object closure.
pub(crate) struct PublisherRecipeAssets {
    pub key: Option<site_build::Sha256Digest>,
    pub template_files: Rc<BTreeMap<String, Vec<u8>>>,
    pub publisher: Rc<site_producer::publisher_runtime::PublisherRuntime>,
    pub ready: BTreeMap<site_build::OutputPath, PreparedOutput>,
    pub objects: ObjectMap,
    pub artifact_records: Rc<Vec<site_build::ArtifactRecord>>,
    pub artifact_roots: Rc<BTreeSet<site_build::ArtifactKey>>,
}

/// Complete immutable Publisher runtime installed only after preparation has
/// succeeded. Path rendering may memoize addressed output bytes, but can never
/// change the SiteBuild named by the handle.
pub(crate) struct PublisherRuntime {
    pub preparation_key: Option<site_build::Sha256Digest>,
    pub recipe_assets: Rc<PublisherRecipeAssets>,
    pub state: Rc<RenderState>,
    pub build: site_build::ClosedSiteBuild,
    pub catalog: Vec<OutputDescriptor>,
    pub ready: BTreeMap<site_build::OutputPath, PreparedOutput>,
    pub objects: ObjectMap,
    pub renderer: site_build::RendererImplementation,
    pub output_options: BTreeMap<String, String>,
    #[cfg(feature = "dependency-observation")]
    pub dependency_observation: crate::dependency_observation::BuildDependencyObservation,
}

struct CycleRuntime {
    build: site_build::ClosedSiteBuild,
    objects: ObjectMap,
    renderer: Option<CycleRendererState>,
}

struct CycleRendererState {
    renderer: site_build::RendererImplementation,
    output_schema: String,
    options: BTreeMap<String, String>,
    expected: BTreeSet<site_build::OutputPath>,
    ready: BTreeMap<site_build::OutputPath, site_build::SiteOutputFile>,
}

#[derive(Clone)]
pub(crate) struct AuthenticatedObject {
    content: site_build::ContentRef,
    bytes: Rc<Vec<u8>>,
}

impl AuthenticatedObject {
    pub(crate) fn eager_authenticated(
        content: site_build::ContentRef,
        bytes: Rc<Vec<u8>>,
    ) -> Result<Self, String> {
        if content.byte_length != bytes.len() as u64 {
            return Err(format!(
                "content {} length {} differs from authenticated bytes {}",
                content.sha256,
                content.byte_length,
                bytes.len()
            ));
        }
        Ok(Self { content, bytes })
    }

    pub(crate) fn content(&self) -> &site_build::ContentRef {
        &self.content
    }

    /// Confirm that this already-authenticated allocation is the object named
    /// by `expected`. Bytes are hashed exactly once, at admission; closing a
    /// build checks the retained proof instead of re-hashing the same object.
    pub(crate) fn authenticates(&self, expected: &site_build::ContentRef) -> Result<(), String> {
        if self.content.sha256 != expected.sha256
            || self.content.byte_length != expected.byte_length
        {
            return Err(format!(
                "authenticated object {} does not match requested content {}",
                self.content.sha256, expected.sha256
            ));
        }
        Ok(())
    }

    pub(crate) fn materialize(&self) -> Result<Rc<Vec<u8>>, String> {
        Ok(self.bytes.clone())
    }
}

pub(crate) type ObjectMap = BTreeMap<site_build::Sha256Digest, AuthenticatedObject>;

enum Runtime {
    Publisher(PublisherRuntime),
    Cycle(CycleRuntime),
}

fn build_failure(
    operation: crate::BuildOperation,
    phase: crate::BuildErrorPhase,
    code: crate::BuildErrorCode,
    message: impl Into<String>,
) -> crate::BuildError<()> {
    crate::BuildError::new(operation, phase, code, message)
}

/// Canonical in-process executor and bounded immutable handle owner.
///
/// Native and WASM transports use this same runtime. They may choose different
/// renderer implementations (Rust Liquid for Publisher, LiquidJS for Cycle),
/// but handle lifetime, output inventory, content verification, and canonical
/// finalization are shared here.
pub struct SiteEngine {
    pub(crate) compilation: crate::compilation::CompilationState,
    pub(crate) preparation: crate::preparation::PreparationState,
    runtimes: BTreeMap<String, Runtime>,
    generations: VecDeque<String>,
    clock_ms: fn() -> f64,
    #[cfg(test)]
    pub(crate) render_package_catalog_limits: Option<(usize, usize, usize)>,
    #[cfg(test)]
    pub(crate) fail_after_render_package_catalog_stage: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct PhaseTimer {
    clock_ms: fn() -> f64,
    started_ms: f64,
}

impl PhaseTimer {
    pub(crate) fn elapsed_ms(self) -> f64 {
        ((self.clock_ms)() - self.started_ms).max(0.0)
    }
}

fn native_clock_ms() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

impl Default for SiteEngine {
    fn default() -> Self {
        Self {
            compilation: Default::default(),
            preparation: Default::default(),
            runtimes: Default::default(),
            generations: Default::default(),
            clock_ms: native_clock_ms,
            #[cfg(test)]
            render_package_catalog_limits: None,
            #[cfg(test)]
            fail_after_render_package_catalog_stage: false,
        }
    }
}

impl SiteEngine {
    /// Install the target host's monotonic millisecond clock. This affects only
    /// observational phase metrics and never build identity or output bytes.
    pub fn set_clock(&mut self, clock_ms: fn() -> f64) {
        self.clock_ms = clock_ms;
    }

    pub(crate) fn timer(&self) -> PhaseTimer {
        PhaseTimer {
            clock_ms: self.clock_ms,
            started_ms: (self.clock_ms)(),
        }
    }

    #[cfg(all(test, feature = "dependency-observation"))]
    pub(crate) fn dependency_decision_for_page(
        &self,
        handle: &str,
        path: &site_build::OutputPath,
    ) -> Result<::dependency_observation::RebuildDecision, String> {
        let runtime = self
            .runtimes
            .get(handle)
            .ok_or_else(|| format!("unknown dependency-observation handle {handle}"))?;
        let Runtime::Publisher(runtime) = runtime else {
            return Err("Cycle dependency observation is conservatively whole-build".into());
        };
        runtime.dependency_observation.decision_for_page(path)
    }

    pub(crate) fn install_publisher(&mut self, runtime: PublisherRuntime) -> String {
        let handle = runtime.build.site_build().build_id().to_string();
        self.retain(handle.clone(), Runtime::Publisher(runtime));
        handle
    }

    pub(crate) fn install_cycle(
        &mut self,
        build: site_build::ClosedSiteBuild,
        objects: ObjectMap,
    ) -> String {
        let handle = build.site_build().build_id().to_string();
        self.retain(
            handle.clone(),
            Runtime::Cycle(CycleRuntime {
                build,
                objects,
                renderer: None,
            }),
        );
        handle
    }

    fn retain(&mut self, handle: String, runtime: Runtime) {
        self.generations.retain(|existing| existing != &handle);
        self.runtimes.insert(handle.clone(), runtime);
        self.generations.push_back(handle);
        while self.generations.len() > RETAINED_SITE_BUILD_LIMIT {
            let retired = self
                .generations
                .pop_front()
                .expect("retained generation limit exceeded");
            self.runtimes.remove(&retired);
        }
        debug_assert_eq!(self.runtimes.len(), self.generations.len());
        debug_assert!(self.runtimes.len() <= RETAINED_SITE_BUILD_LIMIT);
    }

    /// Reuse an exact Publisher runtime only while it is already retained. A
    /// hit refreshes preparation recency and preserves rendered-page memoization.
    pub(crate) fn reuse_publisher(
        &mut self,
        preparation_key: &site_build::Sha256Digest,
    ) -> Option<(String, site_build::ClosedSiteBuild)> {
        let (handle, build) =
            self.generations
                .iter()
                .rev()
                .find_map(|handle| match self.runtimes.get(handle) {
                    Some(Runtime::Publisher(runtime))
                        if runtime.preparation_key.as_ref() == Some(preparation_key) =>
                    {
                        Some((handle.clone(), runtime.build.clone()))
                    }
                    _ => None,
                })?;
        self.generations.retain(|existing| existing != &handle);
        self.generations.push_back(handle.clone());
        Some((handle, build))
    }

    /// Borrow immutable template/runtime assets from a retained Publisher
    /// runtime without refreshing that build's recency. Installing the new
    /// complete successor is the only operation that changes retention order.
    pub(crate) fn reuse_publisher_recipe_assets(
        &self,
        key: &site_build::Sha256Digest,
    ) -> Option<Rc<PublisherRecipeAssets>> {
        self.generations
            .iter()
            .rev()
            .find_map(|handle| match self.runtimes.get(handle) {
                Some(Runtime::Publisher(runtime))
                    if runtime.recipe_assets.key.as_ref() == Some(key) =>
                {
                    Some(runtime.recipe_assets.clone())
                }
                _ => None,
            })
    }

    pub fn outputs(&self, handle: &str) -> Result<OutputCatalog, crate::BuildError<()>> {
        let runtime = self.runtimes.get(handle).ok_or_else(|| {
            build_failure(
                crate::BuildOperation::Outputs,
                crate::BuildErrorPhase::Lifecycle,
                crate::BuildErrorCode::UnknownBuild,
                format!("outputs: unknown build handle {handle}"),
            )
        })?;
        let Runtime::Publisher(runtime) = runtime else {
            return Err(build_failure(
                crate::BuildOperation::Outputs,
                crate::BuildErrorPhase::Renderer,
                crate::BuildErrorCode::RendererFailed,
                "outputs: Cycle output catalog belongs to the external LiquidJS host",
            ));
        };
        let mut outputs = runtime.catalog.clone();
        for output in &mut outputs {
            if let Some(ready) = runtime.ready.get(&output.path) {
                output.content = Some(ready.content.clone());
            }
        }
        Ok(OutputCatalog {
            build_id: runtime.build.site_build().build_id().to_string(),
            outputs,
        })
    }

    pub fn render(
        &mut self,
        handle: &str,
        path: &str,
    ) -> Result<site_build::ContentRef, crate::BuildError<()>> {
        let path = site_build::OutputPath::parse(path.to_string()).map_err(|error| {
            build_failure(
                crate::BuildOperation::Render,
                crate::BuildErrorPhase::Input,
                crate::BuildErrorCode::InvalidInput,
                format!("render: invalid output path {path}: {error}"),
            )
        })?;
        let runtime = self.runtimes.get_mut(handle).ok_or_else(|| {
            build_failure(
                crate::BuildOperation::Render,
                crate::BuildErrorPhase::Lifecycle,
                crate::BuildErrorCode::UnknownBuild,
                format!("render: unknown build handle {handle}"),
            )
        })?;
        let Runtime::Publisher(runtime) = runtime else {
            return Err(build_failure(
                crate::BuildOperation::Render,
                crate::BuildErrorPhase::Renderer,
                crate::BuildErrorCode::RendererFailed,
                "render: Cycle outputs are rendered by the external LiquidJS host",
            ));
        };
        if let Some(output) = runtime.ready.get(&path) {
            return Ok(output.content.clone());
        }
        if !runtime.catalog.iter().any(|output| output.path == path) {
            return Err(build_failure(
                crate::BuildOperation::Render,
                crate::BuildErrorPhase::Input,
                crate::BuildErrorCode::InvalidInput,
                format!("render: path {path} is not declared by outputs"),
            ));
        }
        #[cfg(feature = "dependency-observation")]
        let (html, dependency_reads) = runtime
            .state
            .render_page_tracked_by_name(path.as_str())
            .map_err(|error| {
                build_failure(
                    crate::BuildOperation::Render,
                    crate::BuildErrorPhase::Renderer,
                    crate::BuildErrorCode::RendererFailed,
                    format!("render {path}: {error}"),
                )
            })?;
        #[cfg(not(feature = "dependency-observation"))]
        let html = runtime
            .state
            .render_page_by_name(path.as_str())
            .map_err(|error| {
                build_failure(
                    crate::BuildOperation::Render,
                    crate::BuildErrorPhase::Renderer,
                    crate::BuildErrorCode::RendererFailed,
                    format!("render {path}: {error}"),
                )
            })?;
        #[cfg(feature = "dependency-observation")]
        runtime
            .dependency_observation
            .record_page(&path, dependency_reads)
            .map_err(|message| {
                build_failure(
                    crate::BuildOperation::Render,
                    crate::BuildErrorPhase::Renderer,
                    crate::BuildErrorCode::Integrity,
                    format!("render {path}: dependency observation: {message}"),
                )
            })?;
        #[cfg(feature = "dependency-observation")]
        let html = {
            let (html, post_pass) = runtime.recipe_assets.publisher.finish_html_observed(&html);
            runtime
                .dependency_observation
                .record_html_post_pass(&path, &post_pass)
                .map_err(|message| {
                    build_failure(
                        crate::BuildOperation::Render,
                        crate::BuildErrorPhase::Renderer,
                        crate::BuildErrorCode::Integrity,
                        format!("render {path}: HTML dependency observation: {message}"),
                    )
                })?;
            html
        };
        #[cfg(not(feature = "dependency-observation"))]
        let html = runtime.recipe_assets.publisher.finish_html(&html);
        let bytes = html.into_bytes();
        let content = site_build::ContentRef::of_bytes(&bytes, Some("text/html"));
        #[cfg(feature = "dependency-observation")]
        runtime
            .dependency_observation
            .record_page_output(&path, &content)
            .map_err(|message| {
                build_failure(
                    crate::BuildOperation::Render,
                    crate::BuildErrorPhase::Renderer,
                    crate::BuildErrorCode::Integrity,
                    format!("render {path}: output dependency observation: {message}"),
                )
            })?;
        let object = AuthenticatedObject::eager_authenticated(content.clone(), Rc::new(bytes))
            .map_err(|message| {
                build_failure(
                    crate::BuildOperation::Render,
                    crate::BuildErrorPhase::ContentStore,
                    crate::BuildErrorCode::Integrity,
                    format!("render {path}: {message}"),
                )
            })?;
        runtime
            .objects
            .entry(content.sha256.clone())
            .or_insert(object);
        runtime.ready.insert(
            path.clone(),
            PreparedOutput {
                content: content.clone(),
                producer: site_build::OutputProducer {
                    id: "publisher-page".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
                source: Some(path.to_string()),
                owner: None,
            },
        );
        Ok(content)
    }

    pub fn read_content(&self, handle: &str, digest: &str) -> Result<Vec<u8>, String> {
        let digest = site_build::Sha256Digest::parse(digest.to_string())
            .map_err(|error| format!("readContent: invalid digest: {error}"))?;
        let runtime = self
            .runtimes
            .get(handle)
            .ok_or_else(|| format!("readContent: unknown build handle {handle}"))?;
        let object = match runtime {
            Runtime::Publisher(runtime) => runtime.objects.get(&digest).cloned(),
            Runtime::Cycle(runtime) => runtime.objects.get(&digest).cloned(),
        }
        .ok_or_else(|| format!("readContent: object {digest} is absent from build {handle}"))?;
        let bytes = object.materialize()?;
        Ok(bytes.as_ref().clone())
    }

    /// Bind the callback-free renderer opened over this exact closed build.
    /// This is private renderer transport used by native/WASM adapters; it is
    /// not a host build operation or a serialized build plan.
    #[doc(hidden)]
    pub fn open_renderer(
        &mut self,
        handle: &str,
        renderer: site_build::RendererImplementation,
        output_schema: String,
        options: BTreeMap<String, String>,
        paths: Vec<site_build::OutputPath>,
    ) -> Result<(), String> {
        let path_count = paths.len();
        let expected = paths.into_iter().collect::<BTreeSet<_>>();
        let runtime = self
            .runtimes
            .get_mut(handle)
            .ok_or_else(|| format!("openRenderer: unknown build handle {handle}"))?;
        let Runtime::Cycle(runtime) = runtime else {
            return Err("openRenderer: Publisher renderer is owned by Rust".into());
        };
        if runtime.renderer.is_some() {
            return Err("openRenderer: Cycle renderer is already open".into());
        }
        if expected.is_empty() {
            return Err("openRenderer: Cycle renderer declared no outputs".into());
        }
        if expected.len() != path_count {
            return Err("openRenderer: Cycle renderer declared duplicate output paths".into());
        }
        runtime.renderer = Some(CycleRendererState {
            renderer,
            output_schema,
            options,
            expected,
            ready: BTreeMap::new(),
        });
        Ok(())
    }

    /// Admit one completed renderer file and its authenticated bytes into the
    /// renderer session bound by `open_renderer`.
    #[doc(hidden)]
    pub fn admit_output(
        &mut self,
        handle: &str,
        file: site_build::SiteOutputFile,
        bytes: Vec<u8>,
    ) -> Result<(), String> {
        file.content
            .verify(&bytes)
            .map_err(|error| format!("admitOutput: verify {}: {error}", file.path))?;
        let runtime = self
            .runtimes
            .get_mut(handle)
            .ok_or_else(|| format!("admitOutput: unknown build handle {handle}"))?;
        let Runtime::Cycle(runtime) = runtime else {
            return Err("admitOutput: Publisher outputs are owned by Rust".into());
        };
        let renderer = runtime
            .renderer
            .as_mut()
            .ok_or_else(|| "admitOutput: Cycle renderer is not open".to_string())?;
        if !renderer.expected.contains(&file.path) {
            return Err(format!(
                "admitOutput: renderer did not declare output {}",
                file.path
            ));
        }
        if let Some(existing) = renderer.ready.get(&file.path) {
            if existing == &file {
                return Ok(());
            }
            return Err(format!(
                "admitOutput: renderer changed completed output {}",
                file.path
            ));
        }
        let object =
            AuthenticatedObject::eager_authenticated(file.content.clone(), Rc::new(bytes))?;
        if let Some(existing) = runtime.objects.get(&file.content.sha256) {
            existing.authenticates(&file.content)?;
        } else {
            runtime.objects.insert(file.content.sha256.clone(), object);
        }
        renderer.ready.insert(file.path.clone(), file);
        Ok(())
    }

    pub fn finalize(&self, handle: &str) -> Result<site_build::SiteOutput, crate::BuildError<()>> {
        let runtime = self.runtimes.get(handle).ok_or_else(|| {
            build_failure(
                crate::BuildOperation::Finalize,
                crate::BuildErrorPhase::Lifecycle,
                crate::BuildErrorCode::UnknownBuild,
                format!("finalize: unknown build handle {handle}"),
            )
        })?;
        match runtime {
            Runtime::Publisher(runtime) => Self::finalize_publisher(runtime).map_err(|message| {
                build_failure(
                    crate::BuildOperation::Finalize,
                    crate::BuildErrorPhase::Finalization,
                    crate::BuildErrorCode::RendererFailed,
                    message,
                )
            }),
            Runtime::Cycle(runtime) => Self::finalize_cycle(runtime).map_err(|message| {
                build_failure(
                    crate::BuildOperation::Finalize,
                    crate::BuildErrorPhase::Finalization,
                    crate::BuildErrorCode::RendererFailed,
                    message,
                )
            }),
        }
    }

    fn finalize_publisher(runtime: &PublisherRuntime) -> Result<site_build::SiteOutput, String> {
        let missing = runtime
            .catalog
            .iter()
            .filter(|output| !runtime.ready.contains_key(&output.path))
            .map(|output| output.path.to_string())
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(format!(
                "finalize: {} declared outputs are not rendered: {}",
                missing.len(),
                missing.join(", ")
            ));
        }
        let files = runtime
            .ready
            .iter()
            .map(|(path, output)| site_build::SiteOutputFile {
                path: path.clone(),
                content: output.content.clone(),
                producer: output.producer.clone(),
                source: output.source.clone(),
                owner: output.owner.clone(),
            });
        let output = site_build::SiteOutput::new(
            &runtime.build,
            runtime.renderer.clone(),
            "publisher-site/v1",
            runtime.output_options.clone(),
            files,
        )
        .map_err(|error| format!("finalize: {error}"))?;
        output
            .verify_for(&runtime.build)
            .map_err(|error| format!("finalize: verify: {error}"))?;
        for file in output.files() {
            let object = runtime
                .objects
                .get(&file.content.sha256)
                .ok_or_else(|| format!("finalize: content object for {} is absent", file.path))?;
            let bytes = object.materialize()?;
            file.content
                .verify(bytes.as_ref())
                .map_err(|error| format!("finalize: verify {} bytes: {error}", file.path))?;
        }
        Ok(output)
    }

    fn finalize_cycle(runtime: &CycleRuntime) -> Result<site_build::SiteOutput, String> {
        let renderer = runtime
            .renderer
            .as_ref()
            .ok_or_else(|| "finalize: Cycle renderer is not open".to_string())?;
        let ready = renderer.ready.keys().cloned().collect::<BTreeSet<_>>();
        if ready != renderer.expected {
            let missing = renderer
                .expected
                .difference(&ready)
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            return Err(format!(
                "finalize: {} Cycle outputs are not rendered: {}",
                missing.len(),
                missing.join(", ")
            ));
        }
        for file in renderer.ready.values() {
            let object = runtime
                .objects
                .get(&file.content.sha256)
                .ok_or_else(|| format!("finalize: content object for {} is absent", file.path))?;
            object.authenticates(&file.content)?;
        }
        let output = site_build::SiteOutput::new(
            &runtime.build,
            renderer.renderer.clone(),
            renderer.output_schema.clone(),
            renderer.options.clone(),
            renderer.ready.values().cloned(),
        )
        .map_err(|error| format!("finalize: {error}"))?;
        output
            .verify_for(&runtime.build)
            .map_err(|error| format!("finalize: verify: {error}"))?;
        Ok(output)
    }
}

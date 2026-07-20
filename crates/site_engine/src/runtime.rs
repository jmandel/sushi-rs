use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use serde::Serialize;

use crate::RenderState;

pub(crate) fn is_static_output(path: &str) -> bool {
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

pub(crate) struct MaterializedPublisherCatalog {
    pub(crate) descriptors: Vec<OutputDescriptor>,
    pub(crate) ready: BTreeMap<site_build::OutputPath, PreparedOutput>,
}

impl MaterializedPublisherCatalog {
    pub(crate) fn new(
        ready: &BTreeMap<site_build::OutputPath, PreparedOutput>,
        pages: Vec<OutputDescriptor>,
        static_originals: BTreeSet<site_build::OutputPath>,
        operation: &str,
    ) -> Result<Self, String> {
        if static_originals
            .iter()
            .any(|path| !ready.contains_key(path))
        {
            return Err(format!(
                "{operation}: Publisher output plan names an absent static output"
            ));
        }
        if let Some((path, _)) = ready
            .iter()
            .find(|(_, output)| output.content.media_type.is_none())
        {
            return Err(format!(
                "{operation}: prepared output {path} has no media type"
            ));
        }
        let mut page_paths = BTreeSet::new();
        for page in &pages {
            if page.kind != OutputKind::Page
                || page.content.is_some()
                || page.media_type != "text/html"
            {
                return Err(format!(
                    "{operation}: Publisher page descriptor {} is not an unresolved HTML page",
                    page.path
                ));
            }
            if !page_paths.insert(page.path.clone()) {
                return Err(format!("{operation}: duplicate output path {}", page.path));
            }
            if ready.contains_key(&page.path) {
                return Err(format!("{operation}: duplicate output path {}", page.path));
            }
        }

        let page_directories = pages
            .iter()
            .filter_map(|page| {
                page.path
                    .as_str()
                    .rsplit_once('/')
                    .map(|(directory, _)| directory.to_string())
            })
            .filter(|directory| !directory.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

        // Validate the complete alias namespace while failure can still abort
        // preparation without installing or promoting a generation.
        for directory in &page_directories {
            for path in &static_originals {
                if path.as_str().starts_with(&format!("{directory}/")) {
                    continue;
                }
                let alias = site_build::OutputPath::parse(format!("{directory}/{path}"))
                    .map_err(|error| format!("{operation}: invalid asset alias: {error}"))?;
                if !ready.contains_key(&alias) && page_paths.contains(&alias) {
                    return Err(format!("{operation}: duplicate output path {alias}"));
                }
            }
        }

        // Close the complete ordinary output namespace during preparation.
        let mut expanded = ready
            .iter()
            .filter(|(path, _)| !page_paths.contains(*path))
            .map(|(path, output)| (path.clone(), output.clone()))
            .collect::<BTreeMap<_, _>>();
        let originals = static_originals
            .iter()
            .filter_map(|path| ready.get(path).map(|output| (path, output)))
            .collect::<Vec<_>>();
        for directory in &page_directories {
            for (path, output) in &originals {
                if path.as_str().starts_with(&format!("{directory}/")) {
                    continue;
                }
                let alias = site_build::OutputPath::parse(format!("{directory}/{path}"))
                    .expect("validated Publisher asset alias");
                let mut alias_output = (*output).clone();
                alias_output.source = alias_output
                    .source
                    .as_ref()
                    .map(|source| format!("{source}; alias={alias}"));
                expanded.entry(alias).or_insert(alias_output);
            }
        }

        let mut descriptors = expanded
            .iter()
            .map(|(path, output)| OutputDescriptor {
                path: path.clone(),
                kind: if is_static_output(path.as_str()) {
                    OutputKind::Asset
                } else {
                    OutputKind::Auxiliary
                },
                media_type: output
                    .content
                    .media_type
                    .clone()
                    .expect("validated prepared output media type"),
                content: Some(output.content.clone()),
                title: None,
                subject: None,
                subject_page: None,
                page_kind: None,
            })
            .collect::<Vec<_>>();
        descriptors.extend(pages);
        descriptors.sort_by(|left, right| left.path.cmp(&right.path));
        debug_assert!(descriptors
            .windows(2)
            .all(|pair| pair[0].path != pair[1].path));
        Ok(MaterializedPublisherCatalog {
            descriptors,
            ready: expanded,
        })
    }

    pub(crate) fn contains_page(&self, path: &site_build::OutputPath) -> bool {
        self.descriptors
            .iter()
            .any(|descriptor| descriptor.path == *path && descriptor.kind == OutputKind::Page)
    }
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

/// The exact render-capable half of one Publisher generation. Canonical
/// preparation moves this value into the installed runtime without
/// reconstructing its RenderState, fragment cache, or optional SQL runtime.
pub(crate) struct PublisherRenderCore {
    pub recipe_assets: Rc<PublisherRecipeAssets>,
    pub state: Rc<RenderState>,
    pub catalog: MaterializedPublisherCatalog,
    pub ready: BTreeMap<site_build::OutputPath, PreparedOutput>,
    pub objects: ObjectMap,
    pub renderer: site_build::RendererImplementation,
    pub output_options: BTreeMap<String, String>,
}

/// Complete immutable Publisher runtime installed only after preparation has
/// succeeded. Path rendering may memoize addressed output bytes, but can never
/// change the SiteBuild named by the handle.
pub(crate) struct PublisherRuntime {
    pub build: site_build::ClosedSiteBuild,
    pub core: PublisherRenderCore,
}

impl std::ops::Deref for PublisherRuntime {
    type Target = PublisherRenderCore;

    fn deref(&self) -> &Self::Target {
        &self.core
    }
}

impl std::ops::DerefMut for PublisherRuntime {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.core
    }
}

impl PublisherRenderCore {
    fn output_catalog(&self, build_id: String) -> OutputCatalog {
        let mut outputs = self.catalog.descriptors.clone();
        for output in &mut outputs {
            if let Some(ready) = self.ready.get(&output.path) {
                output.content = Some(ready.content.clone());
            }
        }
        OutputCatalog { build_id, outputs }
    }
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum RuntimeKind {
    Publisher,
    Cycle,
}

struct RuntimeEntry {
    handle: String,
    preparation_key: Option<site_build::Sha256Digest>,
    runtime: Runtime,
}

impl RuntimeEntry {
    fn kind(&self) -> RuntimeKind {
        match self.runtime {
            Runtime::Publisher(_) => RuntimeKind::Publisher,
            Runtime::Cycle(_) => RuntimeKind::Cycle,
        }
    }

    fn build(&self) -> &site_build::ClosedSiteBuild {
        match &self.runtime {
            Runtime::Publisher(runtime) => &runtime.build,
            Runtime::Cycle(runtime) => &runtime.build,
        }
    }
}

fn build_failure(
    operation: crate::BuildOperation,
    phase: crate::BuildErrorPhase,
    code: crate::BuildErrorCode,
    message: impl Into<String>,
) -> crate::BuildError<()> {
    crate::BuildError::new(operation, phase, code, message)
}

fn render_publisher_core(
    core: &mut PublisherRenderCore,
    path: site_build::OutputPath,
) -> Result<site_build::ContentRef, crate::BuildError<()>> {
    if let Some(output) = core
        .ready
        .get(&path)
        .or_else(|| core.catalog.ready.get(&path))
        .cloned()
    {
        let content = output.content.clone();
        core.ready.entry(path).or_insert(output);
        return Ok(content);
    }
    if !core.catalog.contains_page(&path) {
        return Err(build_failure(
            crate::BuildOperation::Render,
            crate::BuildErrorPhase::Input,
            crate::BuildErrorCode::InvalidInput,
            format!("render: path {path} is not declared by outputs"),
        ));
    }
    let html = core
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
    let html = core.recipe_assets.publisher.finish_html(&html);
    let bytes = html.into_bytes();
    let content = site_build::ContentRef::of_bytes(&bytes, Some("text/html"));
    let object = AuthenticatedObject::eager_authenticated(content.clone(), Rc::new(bytes))
        .map_err(|message| {
            build_failure(
                crate::BuildOperation::Render,
                crate::BuildErrorPhase::ContentStore,
                crate::BuildErrorCode::Integrity,
                format!("render {path}: {message}"),
            )
        })?;
    core.objects.entry(content.sha256.clone()).or_insert(object);
    core.ready.insert(
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

/// Canonical in-process executor and bounded immutable handle owner.
///
/// Native and WASM transports use this same runtime. They may choose different
/// renderer implementations (Rust Liquid for Publisher, LiquidJS for Cycle),
/// but handle lifetime, output inventory, content verification, and canonical
/// finalization are shared here.
pub struct SiteEngine {
    pub(crate) compilation: crate::compilation::CompilationState,
    pub(crate) preparation: crate::preparation::PreparationState,
    runtimes: crate::History2<RuntimeEntry>,
    clock_ms: fn() -> f64,
    #[cfg(test)]
    pub(crate) render_package_catalog_limits: Option<(usize, usize, usize)>,
    #[cfg(test)]
    pub(crate) fail_after_render_package_catalog_stage: bool,
    #[cfg(test)]
    pub(crate) fail_during_publisher_close: bool,
    #[cfg(test)]
    pub(crate) fail_during_cycle_close: bool,
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
            clock_ms: native_clock_ms,
            #[cfg(test)]
            render_package_catalog_limits: None,
            #[cfg(test)]
            fail_after_render_package_catalog_stage: false,
            #[cfg(test)]
            fail_during_publisher_close: false,
            #[cfg(test)]
            fail_during_cycle_close: false,
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

    pub(crate) fn install_publisher(
        &mut self,
        runtime: PublisherRuntime,
        preparation_key: Option<site_build::Sha256Digest>,
    ) -> String {
        let handle = runtime.build.site_build().build_id().to_string();
        self.retain(RuntimeEntry {
            handle: handle.clone(),
            preparation_key,
            runtime: Runtime::Publisher(runtime),
        });
        handle
    }

    pub(crate) fn install_cycle(
        &mut self,
        build: site_build::ClosedSiteBuild,
        objects: ObjectMap,
        preparation_key: Option<site_build::Sha256Digest>,
    ) -> String {
        let handle = build.site_build().build_id().to_string();
        self.retain(RuntimeEntry {
            handle: handle.clone(),
            preparation_key,
            runtime: Runtime::Cycle(CycleRuntime {
                build,
                objects,
                renderer: None,
            }),
        });
        handle
    }

    fn retain(&mut self, entry: RuntimeEntry) {
        if self
            .runtimes
            .current
            .as_ref()
            .is_some_and(|current| current.handle == entry.handle)
        {
            self.runtimes.current = Some(entry);
            return;
        }
        if self
            .runtimes
            .previous
            .as_ref()
            .is_some_and(|previous| previous.handle == entry.handle)
        {
            let prior_current = self.runtimes.current.take();
            self.runtimes.current = Some(entry);
            self.runtimes.previous = prior_current;
            return;
        }
        self.runtimes.promote(entry);
    }

    /// Release one private execution handle without disturbing the retained
    /// predecessor. Hosts use this only when a successfully prepared candidate
    /// loses its publication lease before it becomes externally visible.
    #[doc(hidden)]
    pub fn release(&mut self, handle: &str) -> bool {
        if self
            .runtimes
            .current
            .as_ref()
            .is_some_and(|entry| entry.handle == handle)
        {
            self.runtimes.current = self.runtimes.previous.take();
            return true;
        }
        if self
            .runtimes
            .previous
            .as_ref()
            .is_some_and(|entry| entry.handle == handle)
        {
            self.runtimes.previous = None;
            return true;
        }
        false
    }

    fn runtime(&self, handle: &str) -> Option<&Runtime> {
        self.runtimes
            .iter()
            .find(|entry| entry.handle == handle)
            .map(|entry| &entry.runtime)
    }

    fn runtime_mut(&mut self, handle: &str) -> Option<&mut Runtime> {
        self.runtimes
            .iter_mut()
            .find(|entry| entry.handle == handle)
            .map(|entry| &mut entry.runtime)
    }

    fn touch_runtime(&mut self, handle: &str, kind: RuntimeKind) {
        if self
            .runtimes
            .current
            .as_ref()
            .is_some_and(|entry| entry.handle == handle && entry.kind() == kind)
        {
            return;
        }
        let matched = self
            .runtimes
            .previous
            .as_ref()
            .is_some_and(|entry| entry.handle == handle && entry.kind() == kind);
        debug_assert!(
            matched,
            "validated retained runtime disappeared before commit"
        );
        if matched {
            std::mem::swap(&mut self.runtimes.current, &mut self.runtimes.previous);
        }
    }

    /// Find an exact retained Publisher without changing its recency. Recency
    /// advances only after the canonical prepare transaction closes cleanly.
    pub(crate) fn find_publisher(
        &self,
        preparation_key: &site_build::Sha256Digest,
    ) -> Option<(String, site_build::ClosedSiteBuild)> {
        self.runtimes.iter().find_map(|entry| {
            (entry.kind() == RuntimeKind::Publisher
                && entry.preparation_key.as_ref() == Some(preparation_key))
            .then(|| (entry.handle.clone(), entry.build().clone()))
        })
    }

    pub(crate) fn commit_publisher_reuse(&mut self, handle: &str) {
        debug_assert!(matches!(self.runtime(handle), Some(Runtime::Publisher(_))));
        self.touch_runtime(handle, RuntimeKind::Publisher);
    }

    pub(crate) fn find_cycle(
        &self,
        preparation_key: &site_build::Sha256Digest,
    ) -> Option<(String, site_build::ClosedSiteBuild)> {
        self.runtimes.iter().find_map(|entry| {
            (entry.kind() == RuntimeKind::Cycle
                && entry.preparation_key.as_ref() == Some(preparation_key))
            .then(|| (entry.handle.clone(), entry.build().clone()))
        })
    }

    pub(crate) fn commit_cycle_reuse(&mut self, handle: &str) {
        debug_assert!(matches!(self.runtime(handle), Some(Runtime::Cycle(_))));
        self.touch_runtime(handle, RuntimeKind::Cycle);
    }

    /// Borrow immutable template/runtime assets from a retained Publisher
    /// runtime without refreshing that build's recency. Installing the new
    /// complete successor is the only operation that changes retention order.
    pub(crate) fn reuse_publisher_recipe_assets(
        &self,
        key: &site_build::Sha256Digest,
    ) -> Option<Rc<PublisherRecipeAssets>> {
        self.runtimes.iter().find_map(|entry| match &entry.runtime {
            Runtime::Publisher(runtime) if runtime.recipe_assets.key.as_ref() == Some(key) => {
                Some(runtime.recipe_assets.clone())
            }
            _ => None,
        })
    }

    #[cfg(test)]
    pub(crate) fn retained_generation_handles(&self) -> Vec<String> {
        self.runtimes
            .iter()
            .map(|entry| entry.handle.clone())
            .collect()
    }

    pub fn outputs(&self, handle: &str) -> Result<OutputCatalog, crate::BuildError<()>> {
        let runtime = self.runtime(handle).ok_or_else(|| {
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
        Ok(runtime.output_catalog(runtime.build.site_build().build_id().to_string()))
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
        let runtime = self.runtime_mut(handle).ok_or_else(|| {
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
        render_publisher_core(&mut runtime.core, path)
    }

    pub fn read_content(&self, handle: &str, digest: &str) -> Result<Vec<u8>, String> {
        let digest = site_build::Sha256Digest::parse(digest.to_string())
            .map_err(|error| format!("readContent: invalid digest: {error}"))?;
        let runtime = self
            .runtime(handle)
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
            .runtime_mut(handle)
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
            .runtime_mut(handle)
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
        let runtime = self.runtime(handle).ok_or_else(|| {
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
        let catalog = &runtime.catalog;
        let missing = runtime
            .catalog
            .descriptors
            .iter()
            .filter(|output| {
                !runtime.ready.contains_key(&output.path)
                    && !catalog.ready.contains_key(&output.path)
            })
            .map(|output| output.path.to_string())
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(format!(
                "finalize: {} declared outputs are not rendered: {}",
                missing.len(),
                missing.join(", ")
            ));
        }
        let files = catalog.descriptors.iter().map(|descriptor| {
            let output = runtime
                .ready
                .get(&descriptor.path)
                .or_else(|| catalog.ready.get(&descriptor.path))
                .expect("complete Publisher output checked above");
            site_build::SiteOutputFile {
                path: descriptor.path.clone(),
                content: output.content.clone(),
                producer: output.producer.clone(),
                source: output.source.clone(),
                owner: output.owner.clone(),
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared(bytes: &[u8], media_type: &str, source: &str) -> PreparedOutput {
        PreparedOutput {
            content: site_build::ContentRef::of_bytes(bytes, Some(media_type)),
            producer: site_build::OutputProducer {
                id: "fixture".into(),
                version: "1".into(),
            },
            source: Some(source.into()),
            owner: None,
        }
    }

    fn page(path: &str) -> OutputDescriptor {
        OutputDescriptor {
            path: site_build::OutputPath::parse(path).unwrap(),
            kind: OutputKind::Page,
            media_type: "text/html".into(),
            content: None,
            title: Some("Fixture".into()),
            subject: None,
            subject_page: None,
            page_kind: None,
        }
    }

    #[test]
    fn publisher_catalog_closes_aliases_eagerly() {
        let css = site_build::OutputPath::parse("assets/site.css").unwrap();
        let auxiliary = site_build::OutputPath::parse("package.json").unwrap();
        let ready = BTreeMap::from([
            (css.clone(), prepared(b"css", "text/css", "runtime")),
            (
                auxiliary.clone(),
                prepared(b"{}", "application/json", "runtime"),
            ),
        ]);
        let catalog = MaterializedPublisherCatalog::new(
            &ready,
            vec![page("en/index.html")],
            BTreeSet::from([css.clone()]),
            "test",
        )
        .unwrap();
        let alias = site_build::OutputPath::parse("en/assets/site.css").unwrap();
        let resolved = catalog.ready.get(&alias).unwrap();
        assert_eq!(resolved.content, ready[&css].content);
        assert_eq!(
            resolved.source.as_deref(),
            Some("runtime; alias=en/assets/site.css")
        );
        assert!(!catalog
            .ready
            .contains_key(&site_build::OutputPath::parse("en/package.json").unwrap()));
        assert_eq!(
            catalog
                .descriptors
                .iter()
                .map(|output| (output.path.to_string(), output.kind))
                .collect::<Vec<_>>(),
            vec![
                ("assets/site.css".into(), OutputKind::Asset),
                ("en/assets/site.css".into(), OutputKind::Asset),
                ("en/index.html".into(), OutputKind::Page),
                ("package.json".into(), OutputKind::Auxiliary),
            ]
        );
    }

    #[test]
    fn publisher_catalog_rejects_page_collision_before_runtime_install() {
        let path = site_build::OutputPath::parse("en/index.html").unwrap();
        let ready = BTreeMap::from([(
            path.clone(),
            prepared(b"already ready", "text/html", "fixture"),
        )]);
        let error = MaterializedPublisherCatalog::new(
            &ready,
            vec![page(path.as_str())],
            BTreeSet::new(),
            "prepare(publisher)",
        )
        .err()
        .expect("colliding plan must fail");
        assert!(error.contains("duplicate output path en/index.html"));
    }
}

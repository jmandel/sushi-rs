use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

use render_page::ArtifactObservation;
use serde::{Deserialize, Serialize};

use crate::RenderState;

const RETAINED_SITE_BUILD_LIMIT: usize = 2;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputDescriptor {
    pub path: site_build::OutputPath,
    pub kind: &'static str,
    pub media_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<site_build::ContentRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<OutputResourceSubject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_page: Option<OutputSubjectPage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputResourceSubject {
    pub resource_type: String,
    pub id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputSubjectPage {
    Primary,
    Companion,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputCatalog {
    pub build_id: String,
    pub outputs: Vec<OutputDescriptor>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedOutput {
    pub path: site_build::OutputPath,
    pub media_type: String,
    pub content: site_build::ContentRef,
    pub non_ready_fragments: usize,
}

#[derive(Clone)]
pub struct PreparedOutput {
    pub content: site_build::ContentRef,
    pub producer: site_build::OutputProducer,
    pub source: Option<String>,
    pub owner: Option<site_build::OutputPath>,
}

/// Complete immutable Publisher runtime installed only after preparation has
/// succeeded. Path rendering may memoize addressed output bytes, but can never
/// change the SiteBuild named by the handle.
pub(crate) struct PublisherRuntime {
    pub preparation_key: site_build::Sha256Digest,
    pub state: Rc<RenderState>,
    pub publisher: Option<site_producer::publisher_runtime::PublisherRuntime>,
    pub build: site_build::ClosedSiteBuild,
    pub catalog: Vec<OutputDescriptor>,
    pub ready: BTreeMap<site_build::OutputPath, PreparedOutput>,
    pub objects: ObjectMap,
    pub renderer: site_build::RendererImplementation,
    pub output_options: BTreeMap<String, String>,
}

struct CycleRuntime {
    build: site_build::ClosedSiteBuild,
    objects: ObjectMap,
}

pub(crate) type ObjectMap = BTreeMap<site_build::Sha256Digest, Rc<Vec<u8>>>;

enum Runtime {
    Publisher(PublisherRuntime),
    Cycle(CycleRuntime),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalFinalizeInput {
    pub renderer: site_build::RendererImplementation,
    pub output_schema: String,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
    pub catalog: Vec<site_build::OutputPath>,
    pub files: Vec<site_build::SiteOutputFile>,
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
            Runtime::Cycle(CycleRuntime { build, objects }),
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
                        if &runtime.preparation_key == preparation_key =>
                    {
                        Some((handle.clone(), runtime.build.clone()))
                    }
                    _ => None,
                })?;
        self.generations.retain(|existing| existing != &handle);
        self.generations.push_back(handle.clone());
        Some((handle, build))
    }

    pub fn outputs(&self, handle: &str) -> Result<OutputCatalog, String> {
        let runtime = self
            .runtimes
            .get(handle)
            .ok_or_else(|| format!("outputs: unknown build handle {handle}"))?;
        let Runtime::Publisher(runtime) = runtime else {
            return Err(
                "outputs: Cycle output catalog belongs to the external LiquidJS host".into(),
            );
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

    pub fn render(&mut self, handle: &str, path: &str) -> Result<RenderedOutput, String> {
        let path = site_build::OutputPath::parse(path.to_string())
            .map_err(|error| format!("render: invalid output path {path}: {error}"))?;
        let runtime = self
            .runtimes
            .get_mut(handle)
            .ok_or_else(|| format!("render: unknown build handle {handle}"))?;
        let Runtime::Publisher(runtime) = runtime else {
            return Err("render: Cycle outputs are rendered by the external LiquidJS host".into());
        };
        if let Some(output) = runtime.ready.get(&path) {
            return Ok(RenderedOutput {
                path,
                media_type: output
                    .content
                    .media_type
                    .clone()
                    .ok_or_else(|| "render: prepared output has no media type".to_string())?,
                content: output.content.clone(),
                non_ready_fragments: 0,
            });
        }
        if !runtime.catalog.iter().any(|output| output.path == path) {
            return Err(format!("render: path {path} is not declared by outputs"));
        }
        let (html, reads) = runtime
            .state
            .render_page_tracked_by_name(path.as_str())
            .map_err(|error| format!("render {path}: {error}"))?;
        let html = runtime
            .publisher
            .as_ref()
            .map(|publisher| publisher.finish_html(&html))
            .unwrap_or(html);
        let non_ready_fragments = reads
            .observations()
            .values()
            .filter(|observation| matches!(observation, ArtifactObservation::NotReady { .. }))
            .count();
        let bytes = html.into_bytes();
        let content = site_build::ContentRef::of_bytes(&bytes, Some("text/html"));
        runtime
            .objects
            .entry(content.sha256.clone())
            .or_insert_with(|| Rc::new(bytes));
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
        Ok(RenderedOutput {
            path,
            media_type: "text/html".into(),
            content,
            non_ready_fragments,
        })
    }

    pub fn read_content(&self, handle: &str, digest: &str) -> Result<Vec<u8>, String> {
        let digest = site_build::Sha256Digest::parse(digest.to_string())
            .map_err(|error| format!("readContent: invalid digest: {error}"))?;
        let runtime = self
            .runtimes
            .get(handle)
            .ok_or_else(|| format!("readContent: unknown build handle {handle}"))?;
        let bytes = match runtime {
            Runtime::Publisher(runtime) => runtime.objects.get(&digest).cloned(),
            Runtime::Cycle(runtime) => runtime.objects.get(&digest).cloned(),
        }
        .ok_or_else(|| format!("readContent: object {digest} is absent from build {handle}"))?;
        if site_build::Sha256Digest::of_bytes(&bytes) != digest {
            return Err(format!(
                "readContent: object {digest} failed digest verification"
            ));
        }
        Ok(bytes.as_ref().clone())
    }

    pub fn finalize(&self, handle: &str) -> Result<site_build::SiteOutput, String> {
        let runtime = self
            .runtimes
            .get(handle)
            .ok_or_else(|| format!("finalize: unknown build handle {handle}"))?;
        let Runtime::Publisher(runtime) = runtime else {
            return Err("finalize: Cycle finalization uses the external-renderer binding".into());
        };
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
            let bytes = runtime
                .objects
                .get(&file.content.sha256)
                .ok_or_else(|| format!("finalize: content object for {} is absent", file.path))?;
            file.content
                .verify(bytes)
                .map_err(|error| format!("finalize: verify {} bytes: {error}", file.path))?;
        }
        Ok(output)
    }

    pub fn finalize_external(
        &self,
        handle: &str,
        input: ExternalFinalizeInput,
    ) -> Result<site_build::SiteOutput, String> {
        let runtime = self
            .runtimes
            .get(handle)
            .ok_or_else(|| format!("finalizeExternal: unknown build handle {handle}"))?;
        let Runtime::Cycle(runtime) = runtime else {
            return Err("finalizeExternal: handle does not name an external Cycle build".into());
        };
        let catalog = input.catalog.into_iter().collect::<BTreeSet<_>>();
        if catalog.len() != input.files.len() {
            return Err(
                "finalizeExternal: catalog/file cardinality differs or catalog contains duplicates"
                    .into(),
            );
        }
        let file_paths = input
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<BTreeSet<_>>();
        if file_paths.len() != input.files.len() {
            return Err("finalizeExternal: output files contain duplicate paths".into());
        }
        let missing = catalog.difference(&file_paths).cloned().collect::<Vec<_>>();
        let undeclared = file_paths.difference(&catalog).cloned().collect::<Vec<_>>();
        if !missing.is_empty() || !undeclared.is_empty() {
            return Err(format!(
                "finalizeExternal: incomplete catalog (missing: {}; undeclared: {})",
                missing
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                undeclared
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let output = site_build::SiteOutput::new(
            &runtime.build,
            input.renderer,
            input.output_schema,
            input.options,
            input.files,
        )
        .map_err(|error| format!("finalizeExternal: {error}"))?;
        output
            .verify_for(&runtime.build)
            .map_err(|error| format!("finalizeExternal: verify: {error}"))?;
        Ok(output)
    }
}

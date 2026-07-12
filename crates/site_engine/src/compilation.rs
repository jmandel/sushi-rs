use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::{PackageView, SiteEngine};

const SEMANTIC_COMPILATION_KEY_SCHEMA: &str = "semantic-compilation-key/v1";
const SEMANTIC_COMPILATION_RECIPE: &str = "sushi.compile-project/v1";
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

/// One complete authored project revision. Site bytes are captured here even
/// though only their normalized page listing affects semantic compilation.
#[derive(Clone, Debug)]
pub struct ProjectInputs {
    pub config: String,
    pub fsh: BTreeMap<String, String>,
    pub predefined: BTreeMap<String, Value>,
    pub site_files: BTreeMap<String, Vec<u8>>,
}

/// Immutable authored revision installed by the last successful compile call.
#[derive(Clone, Debug)]
pub struct ProjectRevision {
    config: String,
    fsh: BTreeMap<String, String>,
    predefined: BTreeMap<String, Value>,
    site_files: BTreeMap<String, Vec<u8>>,
    resolved_packages: ResolvedPackageClosure,
}

impl ProjectRevision {
    pub fn new(
        inputs: ProjectInputs,
        resolved_packages: ResolvedPackageClosure,
    ) -> Result<Self, String> {
        Self::capture(inputs, resolved_packages, "project revision")
    }

    fn capture(
        inputs: ProjectInputs,
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

    pub fn config(&self) -> &str {
        &self.config
    }

    pub fn fsh(&self) -> &BTreeMap<String, String> {
        &self.fsh
    }

    pub fn predefined(&self) -> &BTreeMap<String, Value> {
        &self.predefined
    }

    pub fn site_files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.site_files
    }

    pub fn resolved_packages(&self) -> &ResolvedPackageClosure {
        &self.resolved_packages
    }

    #[cfg(feature = "test-support")]
    pub fn config_mut_for_test(&mut self) -> &mut String {
        &mut self.config
    }

    #[cfg(feature = "test-support")]
    pub fn fsh_mut_for_test(&mut self) -> &mut BTreeMap<String, String> {
        &mut self.fsh
    }

    #[cfg(feature = "test-support")]
    pub fn predefined_mut_for_test(&mut self) -> &mut BTreeMap<String, Value> {
        &mut self.predefined
    }

    #[cfg(feature = "test-support")]
    pub fn site_files_mut_for_test(&mut self) -> &mut BTreeMap<String, Vec<u8>> {
        &mut self.site_files
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompilationDefinitionKind {
    FshDeclaration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompilationDefinition {
    pub kind: CompilationDefinitionKind,
    pub path: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Debug)]
pub struct CompilationResource {
    pub filename: String,
    pub text: String,
    pub body: Value,
    pub definition: Option<CompilationDefinition>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompilationDiagnostic {
    pub severity: String,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct CompilationOutcome {
    pub resources: Vec<CompilationResource>,
    pub diagnostics: Vec<CompilationDiagnostic>,
}

/// Result plus the private transition facts a composing host needs to
/// invalidate downstream preparation caches. These facts are not wire fields.
#[derive(Debug)]
pub struct CompilationTransition {
    pub outcome: CompilationOutcome,
    pub semantic_changed: bool,
    pub cache_hit: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct SemanticCompilationKey {
    semantic_inputs_sha256: site_build::Sha256Digest,
    resolved_packages: ResolvedPackageClosure,
}

struct SemanticCompilation {
    key: SemanticCompilationKey,
    compiled: Vec<(PathBuf, Value)>,
    outcome: CompilationOutcome,
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

#[derive(Default)]
pub(crate) struct CompilationState {
    active: Option<SemanticCompilation>,
    previous: Option<SemanticCompilation>,
    project: Option<ProjectRevision>,
    #[cfg(test)]
    cache_hits: u64,
}

fn semantic_compilation_key(
    inputs: &RenderSemanticInputs,
    resolved_packages: &ResolvedPackageClosure,
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
    })
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
}

impl SiteEngine {
    /// Compile typed project inputs against one explicit resolver-scoped package
    /// view and closure. The successful authored revision and semantic result
    /// are committed together; failures leave both retained generations intact.
    pub fn compile_project(
        &mut self,
        inputs: ProjectInputs,
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
        let key = semantic_compilation_key(&semantic_inputs, &resolved, operation)?;
        let project = ProjectRevision::capture(inputs.clone(), resolved.clone(), operation)?;
        if let Some(outcome) = self.compilation.restore(&key) {
            self.compilation.project = Some(project);
            return Ok(CompilationTransition {
                outcome,
                semantic_changed: false,
                cache_hit: true,
            });
        }

        let next = compile(inputs, packages, &page_listing, key)?;
        let outcome = next.outcome.clone();
        let semantic_changed = self.compilation.replace_active(next);
        self.compilation.project = Some(project);
        if semantic_changed {
            self.clear_preparation();
        }
        Ok(CompilationTransition {
            outcome,
            semantic_changed,
            cache_hit: false,
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

    pub fn compile_diagnostics(&self) -> &[CompilationDiagnostic] {
        self.compilation
            .active
            .as_ref()
            .map(|active| active.outcome.diagnostics.as_slice())
            .unwrap_or_default()
    }

    pub fn project_revision(&self) -> Option<&ProjectRevision> {
        self.compilation.project.as_ref()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn install_compilation_for_test(
        &mut self,
        project: ProjectRevision,
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
        let key =
            semantic_compilation_key(&semantic_inputs, &project.resolved_packages, "test-support")
                .expect("test project semantic key");
        self.compilation.active = Some(SemanticCompilation {
            key,
            compiled,
            outcome: CompilationOutcome {
                resources: Vec::new(),
                diagnostics: Vec::new(),
            },
        });
        self.compilation.previous = None;
        self.compilation.project = Some(project);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn project_revision_mut_for_test(&mut self) -> Option<&mut ProjectRevision> {
        self.compilation.project.as_mut()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn compiled_resources_mut_for_test(&mut self) -> &mut Vec<(PathBuf, Value)> {
        &mut self
            .compilation
            .active
            .as_mut()
            .expect("test compilation is installed")
            .compiled
    }
}

fn compile(
    inputs: ProjectInputs,
    packages: PackageView,
    page_listing: &HashMap<String, Vec<String>>,
    key: SemanticCompilationKey,
) -> Result<SemanticCompilation, String> {
    let fsh_files = inputs
        .fsh
        .iter()
        .map(|(path, source)| (path.clone(), source.clone()))
        .collect::<Vec<_>>();
    let predefined = ordered_predefined_resources(&inputs.predefined);
    let predefined_for_render = predefined.clone();
    let cache = packages.root().to_string_lossy().into_owned();
    let (compiled, ig_resource, diagnostics) = compiler::build_project_in_memory_with_ig(
        &inputs.config,
        &fsh_files,
        predefined,
        packages,
        &cache,
        page_listing.clone(),
    )
    .map_err(|error| format!("compile failed: {error:#}"))?;

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
        .map(|diagnostic| CompilationDiagnostic {
            severity: diagnostic.severity.to_string(),
            message: diagnostic.message,
            file: diagnostic.file,
            line: diagnostic.line,
        })
        .collect::<Vec<_>>();
    Ok(SemanticCompilation {
        key,
        compiled: render_set,
        outcome: CompilationOutcome {
            resources,
            diagnostics,
        },
    })
}

fn compilation_resource(resource: compiler::CompiledResource) -> CompilationResource {
    let compiler::CompiledResource {
        filename,
        text,
        body,
        definition,
    } = resource;
    let definition = definition.map(|definition| CompilationDefinition {
        kind: match definition.kind {
            compiler::DefinitionKind::FshDeclaration => CompilationDefinitionKind::FshDeclaration,
        },
        path: definition.path,
        line: definition.line,
        column: definition.column,
    });
    CompilationResource {
        filename,
        text,
        body,
        definition,
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

    use package_store::BundleSource;

    use super::*;

    const CONFIG: &str = "id: test\ncanonical: https://example.test\nfhirVersion: 4.0.1\n";

    fn resolved() -> ResolvedPackageClosure {
        ResolvedPackageClosure {
            config_sha256: site_build::Sha256Digest::of_bytes(CONFIG.as_bytes()),
            resolution_support: BTreeSet::from(["hl7.fhir.r4.core#4.0.1".into()]),
            labels: vec!["hl7.fhir.r4.core#4.0.1".into()],
        }
    }

    fn package_view() -> PackageView {
        let source = Rc::new(BundleSource::new());
        let root = source.cache_root().to_path_buf();
        PackageView::new(source, root, Some(resolved().labels.into_iter().collect()))
    }

    fn inputs_for(parent: &str, site_body: &[u8]) -> ProjectInputs {
        ProjectInputs {
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
        SemanticCompilation {
            key: semantic_compilation_key(&semantic_inputs, &resolved(), "test").unwrap(),
            compiled: vec![(
                PathBuf::from(format!("/__compiled__/StructureDefinition-{name}.json")),
                body.clone(),
            )],
            outcome: CompilationOutcome {
                resources: vec![CompilationResource {
                    filename: format!("StructureDefinition-{name}.json"),
                    text: serde_json::to_string(&body).unwrap(),
                    body,
                    definition: None,
                }],
                diagnostics: vec![CompilationDiagnostic {
                    severity: "information".into(),
                    message: format!("compiled {name}"),
                    file: None,
                    line: None,
                }],
            },
        }
    }

    fn reuse_engine() -> SiteEngine {
        let inputs = inputs_for("Patient", b"old prose");
        let mut engine = SiteEngine::default();
        engine
            .compilation
            .replace_active(compilation("Test", "Patient"));
        engine.compilation.previous = None;
        engine.compilation.project = Some(ProjectRevision {
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

        assert!(transition.cache_hit);
        assert!(!transition.semantic_changed);
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
        let base = ProjectInputs {
            config: CONFIG.into(),
            fsh: BTreeMap::new(),
            predefined: BTreeMap::from([(path.into(), parsed)]),
            site_files: BTreeMap::new(),
        };
        assert!(ProjectRevision::new(base.clone(), resolved())
            .unwrap_err()
            .contains("only parsed"));

        let mut divergent = base;
        divergent.site_files.insert(
            path.into(),
            br#"{"resourceType":"Patient","id":"other"}"#.to_vec(),
        );
        assert!(ProjectRevision::new(divergent, resolved())
            .unwrap_err()
            .contains("differs from the raw authored site file"));

        let wrong_closure = ResolvedPackageClosure {
            config_sha256: site_build::Sha256Digest::of_bytes(b"different config"),
            ..resolved()
        };
        assert!(
            ProjectRevision::new(inputs_for("Patient", b"prose"), wrong_closure)
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
            if let Ok(transition) = engine.compile_project(inputs, package_view(), closure) {
                assert!(!transition.cache_hit, "{label} unexpectedly reused");
            }
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

        assert!(transition.cache_hit);
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
    fn atomic_prepare_preserves_typed_compilation_on_generator_failure() {
        let mut engine = reuse_engine();
        let package_bytes = Rc::new(b"authenticated core fixture".to_vec());
        let core_label = "hl7.fhir.r4.core#4.0.1";
        let environment = crate::PackageEnvironment::new(
            package_view(),
            vec![core_label.into()],
            BTreeMap::from([(
                core_label.into(),
                crate::PackageMaterial::new(
                    site_build::ContentRef::of_bytes(package_bytes.as_ref(), None::<String>),
                    BTreeMap::new(),
                    package_bytes,
                )
                .unwrap(),
            )]),
        )
        .unwrap();

        let error = engine
            .prepare_project(
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
        let crate::PrepareProjectError::Site {
            message,
            compilation,
            ..
        } = error
        else {
            panic!("successful exact compilation must make this a site failure")
        };
        assert!(message.contains("ImplementationGuide"), "{message}");
        assert_eq!(
            compilation.resources[0].filename,
            "StructureDefinition-Test.json"
        );
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
        let a_key = semantic_compilation_key(&a_semantics, &resolved(), "test").unwrap();
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
        let b_key = semantic_compilation_key(&b_semantics, &resolved(), "test").unwrap();
        let outcome = engine.compilation.restore(&b_key).unwrap();
        assert_eq!(
            outcome.resources[0].filename,
            "StructureDefinition-Other.json"
        );
        assert_eq!(engine.compilation.cache_hits, 1);
    }
}

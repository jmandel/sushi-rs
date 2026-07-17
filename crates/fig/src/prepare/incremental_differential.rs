//! Exhaustive fresh-versus-retained differential gate for real catalog guides.
//!
//! This module is test-only. It deliberately composes the same private native
//! ProjectSource/PackageProvider adapters with the public canonical SiteEngine
//! operations; it does not add a Fig command, transport, or build value.

use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const CASE_SCHEMA: &str = "fig-incremental-differential-case/v1";
const RECEIPT_SCHEMA: &str = "fig-incremental-differential-receipt/v1";
const CASE_ENV: &str = "FIG_DIFFERENTIAL_CASE_JSON";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Case {
    schema_version: String,
    case_id: String,
    source_a: PathBuf,
    source_b: PathBuf,
    package_cache: PathBuf,
    package_carrier_store: PathBuf,
    report: PathBuf,
    expected_changed_output: String,
    expect_compilation_change: bool,
    expected_snapshot_resource_misses: Option<u64>,
    template_coordinate: String,
    build_epoch_secs: i64,
    fixture: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    schema_version: &'static str,
    status: &'static str,
    case_id: String,
    fixture: Value,
    comparisons: Vec<&'static str>,
    package_corpus: PackageCorpusSummary,
    executions: BTreeMap<String, ExecutionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failed_successor: Option<FailedSuccessorSummary>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FailedSuccessorSummary {
    operation: site_engine::BuildOperation,
    phase: site_engine::BuildErrorPhase,
    code: site_engine::BuildErrorCode,
    retryable: bool,
    successful_compilation: bool,
    injected_body_sha256: site_build::Sha256Digest,
    recovery: ExecutionSummary,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PackageCorpusSummary {
    capture_attempts: usize,
    config_sha256: String,
    resolved_labels: Vec<String>,
    resolution_support: Vec<String>,
    carriers: Vec<PackageCarrierSummary>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PackageCarrierSummary {
    label: String,
    content: site_build::ContentRef,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionSummary {
    build_id: String,
    output_id: Option<String>,
    compiled_resources: usize,
    compilation_diagnostics: usize,
    closure_references: usize,
    closure_objects: usize,
    output_paths: usize,
    output_bytes: Option<u64>,
    render_order: &'static str,
    metrics: BTreeMap<String, f64>,
}

#[derive(Clone)]
struct FixedSource {
    revision: site_engine::ProjectRevision,
}

impl site_engine::ProjectSource for FixedSource {
    fn config(&mut self) -> std::result::Result<String, String> {
        Ok(self.revision.config.clone())
    }

    fn capture(
        &mut self,
        _packages: &site_engine::PackageEnvironment,
        _resolved: &site_engine::ResolvedPackageClosure,
    ) -> std::result::Result<site_engine::ProjectRevision, String> {
        Ok(self.revision.clone())
    }
}

struct FixedPackages {
    resolved: site_engine::ResolvedPackageClosure,
    environment: site_engine::PackageEnvironment,
}

impl site_engine::PackageProvider for FixedPackages {
    fn resolve(
        &mut self,
        config: &str,
        _generator: &site_engine::GeneratorSpec,
    ) -> std::result::Result<site_engine::ResolvedPackageClosure, String> {
        if site_build::Sha256Digest::of_bytes(config.as_bytes()) != self.resolved.config_sha256 {
            return Err(
                "fixed differential package closure belongs to different config bytes".into(),
            );
        }
        Ok(self.resolved.clone())
    }

    fn environment(
        &mut self,
        resolved: &site_engine::ResolvedPackageClosure,
    ) -> std::result::Result<site_engine::PackageEnvironment, String> {
        if resolved != &self.resolved {
            return Err("fixed differential package provider received a different closure".into());
        }
        Ok(self.environment.clone())
    }
}

struct PackageCorpus {
    resolved: site_engine::ResolvedPackageClosure,
    carriers: Vec<Vec<u8>>,
    summary: PackageCorpusSummary,
}

impl PackageCorpus {
    fn environment(&self) -> Result<site_engine::PackageEnvironment> {
        let packages = self
            .carriers
            .iter()
            .map(|carrier| package_store::PreparedPackage::decode(carrier))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        site_engine::PackageEnvironment::new(packages).map_err(anyhow::Error::msg)
    }
}

#[derive(Clone, Copy, Debug)]
enum RenderOrder {
    None,
    Forward,
    Reverse,
}

impl RenderOrder {
    fn name(self) -> &'static str {
        match self {
            Self::None => "not-rendered",
            Self::Forward => "forward",
            Self::Reverse => "reverse",
        }
    }
}

#[derive(Debug)]
struct RenderedOutput {
    content: site_build::ContentRef,
}

#[derive(Debug)]
struct Evidence {
    build_id: String,
    compilation: Value,
    compiled_resources: usize,
    compilation_diagnostics: usize,
    closed_bytes: Vec<u8>,
    closure_references: Vec<site_build::ContentRef>,
    closure_objects: BTreeSet<String>,
    initial_catalog: Value,
    final_catalog: Option<Value>,
    rendered: BTreeMap<String, RenderedOutput>,
    site_output_bytes: Option<Vec<u8>>,
    output_id: Option<String>,
    output_bytes: Option<u64>,
    metrics: BTreeMap<String, f64>,
    render_order: RenderOrder,
}

struct VerifiedByteStore {
    directory: tempfile::TempDir,
}

impl VerifiedByteStore {
    fn create(parent: &Path) -> Result<Self> {
        fs::create_dir_all(parent)?;
        let directory = tempfile::Builder::new()
            .prefix(".fig-differential-bytes-")
            .tempdir_in(parent)
            .with_context(|| format!("create verified byte store below {}", parent.display()))?;
        Ok(Self { directory })
    }

    fn admit(&self, content: &site_build::ContentRef, bytes: &[u8]) -> Result<()> {
        content.verify(bytes)?;
        let path = self.directory.path().join(content.sha256.as_str());
        match fs::read(&path) {
            Ok(existing) => {
                if existing != bytes {
                    bail!(
                        "ContentRef {} resolved to different direct bytes",
                        content.sha256
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::write(&path, bytes)
                    .with_context(|| format!("write verified content object {}", content.sha256))?;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read verified content object {}", content.sha256));
            }
        }
        Ok(())
    }

    fn read(&self, content: &site_build::ContentRef) -> Result<Vec<u8>> {
        let bytes = fs::read(self.directory.path().join(content.sha256.as_str()))
            .with_context(|| format!("read verified content object {}", content.sha256))?;
        content.verify(&bytes)?;
        Ok(bytes)
    }
}

impl Evidence {
    fn summary(&self) -> ExecutionSummary {
        ExecutionSummary {
            build_id: self.build_id.clone(),
            output_id: self.output_id.clone(),
            compiled_resources: self.compiled_resources,
            compilation_diagnostics: self.compilation_diagnostics,
            closure_references: self.closure_references.len(),
            closure_objects: self.closure_objects.len(),
            output_paths: self.rendered.len().max(catalog_len(&self.initial_catalog)),
            output_bytes: self.output_bytes,
            render_order: self.render_order.name(),
            metrics: self.metrics.clone(),
        }
    }
}

fn catalog_len(value: &Value) -> usize {
    value
        .get("outputs")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default()
}

fn read_case() -> Result<Case> {
    let path = env::var_os(CASE_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("{CASE_ENV} is required for the ignored differential test"))?;
    let bytes = fs::read(&path).with_context(|| format!("read case file {}", path.display()))?;
    let case: Case = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse case file {}", path.display()))?;
    if case.schema_version != CASE_SCHEMA {
        bail!(
            "unsupported differential case schema {:?}",
            case.schema_version
        );
    }
    if case.case_id.trim().is_empty()
        || case.template_coordinate.trim().is_empty()
        || case.expected_changed_output.trim().is_empty()
    {
        bail!("differential case identities must be non-empty");
    }
    Ok(case)
}

fn generator(case: &Case) -> site_engine::GeneratorSpec {
    site_engine::GeneratorSpec::Publisher {
        template_coordinate: case.template_coordinate.clone(),
        build_epoch_secs: case.build_epoch_secs,
        active_tables: false,
        run_uuid: None,
    }
}

fn canonical_directory(path: &Path, description: &str) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("canonicalize {description} {}", path.display()))?;
    if !canonical.is_dir() {
        bail!("{description} is not a directory: {}", canonical.display());
    }
    Ok(canonical)
}

fn reject_user_fhir_cache(cache: &Path) -> Result<()> {
    if let Some(home) = env::var_os("HOME") {
        let user_fhir = PathBuf::from(home).join(".fhir");
        if let Ok(user_fhir) = user_fhir.canonicalize() {
            if cache == user_fhir || cache.starts_with(&user_fhir) {
                bail!("differential harness refuses the user's ~/.fhir tree");
            }
        }
    }
    Ok(())
}

fn capture_revision(
    root: &Path,
    environment: &site_engine::PackageEnvironment,
    resolved: &site_engine::ResolvedPackageClosure,
) -> Result<(String, site_engine::ProjectRevision)> {
    let mut source = FilesystemProjectSource::open(root)?;
    let config = site_engine::ProjectSource::config(&mut source).map_err(anyhow::Error::msg)?;
    let revision = site_engine::ProjectSource::capture(&mut source, environment, resolved)
        .map_err(anyhow::Error::msg)?;
    source.verify_unchanged()?;
    Ok((config, revision))
}

fn package_corpus(case: &Case, config: &str) -> Result<PackageCorpus> {
    let cache = canonical_directory(&case.package_cache, "package cache")?;
    reject_user_fhir_cache(&cache)?;
    let mut accepted = None;
    for attempt in 1..=12 {
        let first_closure = resolve_prepare_closure(config, &cache, &generator(case))?;
        let first = collect_package_snapshot(&cache, &first_closure)?;
        let second_closure = resolve_prepare_closure(config, &cache, &generator(case))?;
        let second = collect_package_snapshot(&cache, &second_closure)?;
        if first_closure == second_closure
            && first.identities == second.identities
            && first.objects == second.objects
        {
            accepted = Some((attempt, second_closure, second));
            break;
        }
    }
    let (capture_attempts, closure, snapshot) = accepted.ok_or_else(|| {
        anyhow!("package inputs did not remain byte-identical across 12 capture attempts")
    })?;
    preserve_package_carriers(case, &snapshot)?;
    let carriers = snapshot
        .identities
        .iter()
        .map(|(label, digest)| {
            let digest = site_build::Sha256Digest::parse(digest.clone())
                .with_context(|| format!("parse carrier digest for {label}"))?;
            snapshot
                .objects
                .get(&digest)
                .cloned()
                .ok_or_else(|| anyhow!("captured carrier object for {label} is absent"))
        })
        .collect::<Result<Vec<_>>>()?;
    if carriers.is_empty() {
        bail!("captured package corpus is empty");
    }
    let carrier_summaries = snapshot
        .identities
        .iter()
        .map(|(label, digest)| {
            let digest = site_build::Sha256Digest::parse(digest.clone())?;
            let bytes = snapshot
                .objects
                .get(&digest)
                .ok_or_else(|| anyhow!("captured carrier object for {label} is absent"))?;
            Ok(PackageCarrierSummary {
                label: label.clone(),
                content: site_build::ContentRef {
                    sha256: digest,
                    byte_length: bytes.len() as u64,
                    media_type: Some(package_store::PREPARED_PACKAGE_MEDIA_TYPE.into()),
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let summary = PackageCorpusSummary {
        capture_attempts,
        config_sha256: closure.resolved.config_sha256.to_string(),
        resolved_labels: closure.resolved.labels.clone(),
        resolution_support: closure
            .resolved
            .resolution_support
            .iter()
            .cloned()
            .collect(),
        carriers: carrier_summaries,
    };
    Ok(PackageCorpus {
        resolved: closure.resolved,
        carriers,
        summary,
    })
}

fn preserve_package_carriers(case: &Case, snapshot: &CapturedPackages) -> Result<()> {
    let root = canonical_directory(&case.package_carrier_store, "package carrier store")?;
    for (label, digest) in &snapshot.identities {
        let digest = site_build::Sha256Digest::parse(digest.clone())?;
        let bytes = snapshot
            .objects
            .get(&digest)
            .ok_or_else(|| anyhow!("captured carrier object for {label} is absent"))?;
        let content = site_build::ContentRef {
            sha256: digest.clone(),
            byte_length: bytes.len() as u64,
            media_type: Some(package_store::PREPARED_PACKAGE_MEDIA_TYPE.into()),
        };
        content.verify(bytes)?;
        let destination = root.join(digest.as_str());
        match fs::read(&destination) {
            Ok(existing) => {
                content.verify(&existing)?;
                if existing != *bytes {
                    bail!("retained package carrier {digest} has different direct bytes");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let temporary = root.join(format!(
                    ".{}.{}.tmp",
                    digest.as_str(),
                    safe_component(&case.case_id)
                ));
                fs::write(&temporary, bytes).with_context(|| {
                    format!("write retained package carrier for {label} at {temporary:?}")
                })?;
                fs::rename(&temporary, &destination).with_context(|| {
                    format!("publish retained package carrier for {label} at {destination:?}")
                })?;
                let retained = fs::read(&destination)?;
                content.verify(&retained)?;
                if retained != *bytes {
                    bail!("published package carrier {digest} differs from captured bytes");
                }
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("read retained package carrier for {label} at {destination:?}")
                });
            }
        }
    }
    Ok(())
}

fn prepare_on(
    engine: &mut site_engine::SiteEngine,
    revision: &site_engine::ProjectRevision,
    corpus: &PackageCorpus,
    environment: &site_engine::PackageEnvironment,
    spec: &site_engine::GeneratorSpec,
    order: RenderOrder,
    byte_store: &VerifiedByteStore,
) -> Result<Evidence> {
    let mut source = FixedSource {
        revision: revision.clone(),
    };
    let mut packages = FixedPackages {
        resolved: corpus.resolved.clone(),
        environment: environment.clone(),
    };
    let prepared = engine
        .prepare_project(&mut source, &mut packages, spec.clone())
        .map_err(|error| anyhow!(error.to_string()))?;
    let handle = prepared.site.build_id.clone();
    let closed = prepared.site.site_build.clone();
    let compilation = serde_json::to_value(&prepared.compilation)?;
    let compiled_resources = prepared.compilation.resources.len();
    let compilation_diagnostics = prepared.compilation.diagnostics.len();
    let metrics = prepared
        .events
        .iter()
        .filter_map(|event| event.metrics.as_ref())
        .flat_map(|metrics| metrics.iter().map(|(key, value)| (key.clone(), *value)))
        .collect::<BTreeMap<_, _>>();

    let closed_bytes = closed.site_build().canonical_bytes()?;
    let mut closure_references = bundle_content_refs(&closed);
    closure_references.sort_by(|left, right| {
        (
            left.sha256.as_str(),
            left.byte_length,
            left.media_type.as_deref(),
        )
            .cmp(&(
                right.sha256.as_str(),
                right.byte_length,
                right.media_type.as_deref(),
            ))
    });
    closure_references.dedup();
    let mut closure_objects = BTreeSet::new();
    for content in &closure_references {
        let bytes = engine
            .read_content(&handle, content.sha256.as_str())
            .map_err(anyhow::Error::msg)?;
        byte_store.admit(content, &bytes)?;
        closure_objects.insert(content.sha256.to_string());
    }

    let initial = engine
        .outputs(&handle)
        .map_err(|error| anyhow!(error.to_string()))?;
    if initial.build_id != handle || initial.outputs.is_empty() {
        bail!("output catalog is empty or belongs to a different build");
    }
    let initial_catalog = serde_json::to_value(&initial)?;
    if matches!(order, RenderOrder::None) {
        return Ok(Evidence {
            build_id: handle,
            compilation,
            compiled_resources,
            compilation_diagnostics,
            closed_bytes,
            closure_references,
            closure_objects,
            initial_catalog,
            final_catalog: None,
            rendered: BTreeMap::new(),
            site_output_bytes: None,
            output_id: None,
            output_bytes: None,
            metrics,
            render_order: order,
        });
    }

    let mut paths = initial
        .outputs
        .iter()
        .map(|output| output.path.to_string())
        .collect::<Vec<_>>();
    if matches!(order, RenderOrder::Reverse) {
        paths.reverse();
    }
    let descriptors = initial
        .outputs
        .iter()
        .map(|output| (output.path.to_string(), output.content.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut rendered = BTreeMap::new();
    for path in &paths {
        let content = engine
            .render(&handle, path)
            .map_err(|error| anyhow!(error.to_string()))?;
        if let Some(Some(declared)) = descriptors.get(path) {
            if declared != &content {
                bail!("render changed predeclared ContentRef for {path}");
            }
        }
        let bytes = engine
            .read_content(&handle, content.sha256.as_str())
            .map_err(anyhow::Error::msg)?;
        byte_store.admit(&content, &bytes)?;
        if rendered
            .insert(path.clone(), RenderedOutput { content })
            .is_some()
        {
            bail!("catalog repeats output path {path}");
        }
    }
    if rendered.len() != initial.outputs.len() {
        bail!("not every catalog output was rendered");
    }

    let final_catalog = engine
        .outputs(&handle)
        .map_err(|error| anyhow!(error.to_string()))?;
    if final_catalog
        .outputs
        .iter()
        .any(|output| output.content.is_none())
    {
        bail!("post-render catalog still contains unresolved output ContentRefs");
    }
    for output in &final_catalog.outputs {
        if rendered
            .get(output.path.as_str())
            .map(|rendered| &rendered.content)
            != output.content.as_ref()
        {
            bail!(
                "post-render catalog disagrees with render for {}",
                output.path
            );
        }
    }
    let final_catalog = serde_json::to_value(&final_catalog)?;
    let site_output = engine
        .finalize(&handle)
        .map_err(|error| anyhow!(error.to_string()))?;
    site_output.verify_for(&closed)?;
    if site_output.files().len() != rendered.len() {
        bail!("SiteOutput file count differs from the rendered catalog");
    }
    let mut output_bytes = 0u64;
    for file in site_output.files() {
        let Some(rendered) = rendered.get(file.path.as_str()) else {
            bail!("SiteOutput contains undeclared path {}", file.path);
        };
        if file.content != rendered.content {
            bail!("SiteOutput ContentRef differs for {}", file.path);
        }
        let bytes = engine
            .read_content(&handle, file.content.sha256.as_str())
            .map_err(anyhow::Error::msg)?;
        byte_store.admit(&file.content, &bytes)?;
        output_bytes += bytes.len() as u64;
    }
    let output_id = site_output.output_id().to_string();
    let site_output_bytes = site_output.canonical_bytes()?;

    Ok(Evidence {
        build_id: handle,
        compilation,
        compiled_resources,
        compilation_diagnostics,
        closed_bytes,
        closure_references,
        closure_objects,
        initial_catalog,
        final_catalog: Some(final_catalog),
        rendered,
        site_output_bytes: Some(site_output_bytes),
        output_id: Some(output_id),
        output_bytes: Some(output_bytes),
        metrics,
        render_order: order,
    })
}

fn mismatch_root(case: &Case) -> PathBuf {
    case.report
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{}-mismatch", safe_component(&case.case_id)))
}

fn safe_component(value: &str) -> String {
    let mut result = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .take(96)
        .collect::<String>();
    if result.is_empty() {
        result.push_str("mismatch");
    }
    result
}

fn write_pair(case: &Case, name: &str, suffix: &str, left: &[u8], right: &[u8]) -> Result<()> {
    let root = mismatch_root(case);
    fs::create_dir_all(&root)?;
    let name = safe_component(name);
    fs::write(root.join(format!("{name}.left.{suffix}")), left)?;
    fs::write(root.join(format!("{name}.right.{suffix}")), right)?;
    Ok(())
}

fn compare_value(case: &Case, label: &str, left: &Value, right: &Value) -> Result<()> {
    if left != right {
        write_pair(
            case,
            label,
            "json",
            &serde_json::to_vec_pretty(left)?,
            &serde_json::to_vec_pretty(right)?,
        )?;
        bail!(
            "{label} differs; evidence written below {}",
            mismatch_root(case).display()
        );
    }
    Ok(())
}

fn compare_bytes(case: &Case, label: &str, suffix: &str, left: &[u8], right: &[u8]) -> Result<()> {
    if left != right {
        write_pair(case, label, suffix, left, right)?;
        bail!(
            "{label} bytes differ; evidence written below {}",
            mismatch_root(case).display()
        );
    }
    Ok(())
}

fn compare_rendered(
    case: &Case,
    label: &str,
    left: &Evidence,
    right: &Evidence,
    byte_store: &VerifiedByteStore,
) -> Result<()> {
    if left.rendered.keys().collect::<Vec<_>>() != right.rendered.keys().collect::<Vec<_>>() {
        let left_paths = serde_json::to_vec_pretty(&left.rendered.keys().collect::<Vec<_>>())?;
        let right_paths = serde_json::to_vec_pretty(&right.rendered.keys().collect::<Vec<_>>())?;
        write_pair(case, label, "json", &left_paths, &right_paths)?;
        bail!("{label} rendered path sets differ");
    }
    for (path, left_output) in &left.rendered {
        let right_output = right.rendered.get(path).expect("path sets compared");
        if left_output.content != right_output.content {
            let stem = format!("{label}-{}", safe_component(path));
            write_pair(
                case,
                &format!("{stem}-ref"),
                "json",
                &serde_json::to_vec_pretty(&left_output.content)?,
                &serde_json::to_vec_pretty(&right_output.content)?,
            )?;
            write_pair(
                case,
                &format!("{stem}-bytes"),
                "bin",
                &byte_store.read(&left_output.content)?,
                &byte_store.read(&right_output.content)?,
            )?;
            bail!("{label} differs at rendered path {path}");
        }
    }
    Ok(())
}

fn compare_prepared(case: &Case, label: &str, left: &Evidence, right: &Evidence) -> Result<()> {
    compare_value(
        case,
        &format!("{label}-compilation"),
        &left.compilation,
        &right.compilation,
    )?;
    compare_bytes(
        case,
        &format!("{label}-closed-site-build"),
        "json",
        &left.closed_bytes,
        &right.closed_bytes,
    )?;
    if left.closure_references != right.closure_references {
        write_pair(
            case,
            &format!("{label}-closure-refs"),
            "json",
            &serde_json::to_vec_pretty(&left.closure_references)?,
            &serde_json::to_vec_pretty(&right.closure_references)?,
        )?;
        bail!("{label} closure ContentRefs differ");
    }
    if left.closure_objects != right.closure_objects {
        write_pair(
            case,
            &format!("{label}-closure-objects"),
            "json",
            &serde_json::to_vec_pretty(&left.closure_objects)?,
            &serde_json::to_vec_pretty(&right.closure_objects)?,
        )?;
        bail!("{label} closure object sets differ");
    }
    compare_value(
        case,
        &format!("{label}-initial-catalog"),
        &left.initial_catalog,
        &right.initial_catalog,
    )?;
    Ok(())
}

fn compare_complete(
    case: &Case,
    label: &str,
    left: &Evidence,
    right: &Evidence,
    byte_store: &VerifiedByteStore,
) -> Result<()> {
    compare_prepared(case, label, left, right)?;
    let left_catalog = left
        .final_catalog
        .as_ref()
        .ok_or_else(|| anyhow!("{label} left execution was not rendered"))?;
    let right_catalog = right
        .final_catalog
        .as_ref()
        .ok_or_else(|| anyhow!("{label} right execution was not rendered"))?;
    compare_value(
        case,
        &format!("{label}-final-catalog"),
        left_catalog,
        right_catalog,
    )?;
    compare_rendered(case, label, left, right, byte_store)?;
    compare_bytes(
        case,
        &format!("{label}-site-output"),
        "json",
        left.site_output_bytes.as_deref().unwrap_or_default(),
        right.site_output_bytes.as_deref().unwrap_or_default(),
    )?;
    Ok(())
}

fn require_metric(evidence: &Evidence, key: &str) -> Result<()> {
    if evidence.metrics.get(key).copied() != Some(1.0) {
        bail!(
            "returned A did not exercise {key}=1: {:?}",
            evidence.metrics.get(key)
        );
    }
    Ok(())
}

fn metric(evidence: &Evidence, key: &str) -> Result<f64> {
    evidence
        .metrics
        .get(key)
        .copied()
        .ok_or_else(|| anyhow!("execution did not report {key}"))
}

fn count_metric(evidence: &Evidence, key: &str) -> Result<f64> {
    let value = metric(evidence, key)?;
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        bail!("execution reported invalid count metric {key}={value}");
    }
    Ok(value)
}

fn require_snapshot_reuse(case: &Case, retained_b: &Evidence, fresh_b: &Evidence) -> Result<()> {
    let retained_hits = count_metric(retained_b, "snapshotResourceCacheHits")?;
    let retained_misses = count_metric(retained_b, "snapshotResourceCacheMisses")?;
    let fresh_hits = count_metric(fresh_b, "snapshotResourceCacheHits")?;
    let fresh_misses = count_metric(fresh_b, "snapshotResourceCacheMisses")?;
    if let Some(expected_misses) = case.expected_snapshot_resource_misses {
        let expected_misses = expected_misses as f64;
        if retained_misses != expected_misses
            || fresh_hits != 0.0
            || fresh_misses <= 0.0
            || retained_hits + retained_misses != fresh_misses
            || count_metric(retained_b, "snapshotDerivationAdmitted")? != 1.0
            || count_metric(fresh_b, "snapshotDerivationAdmitted")? != 1.0
        {
            bail!(
                "retained B did not prove exact bounded per-resource snapshot reuse: retained hits/misses={retained_hits}/{retained_misses}, expected misses={expected_misses}, fresh hits/misses={fresh_hits}/{fresh_misses}, metrics={:?}",
                retained_b.metrics
            );
        }
    } else {
        if retained_hits != fresh_misses
            || retained_misses != 0.0
            || fresh_hits != 0.0
            || fresh_misses <= 0.0
            || count_metric(fresh_b, "snapshotDerivationAdmitted")? != 1.0
        {
            bail!(
                "site-only retained B did not exercise complete per-resource snapshot reuse against a canonical fresh miss: {:?}",
                retained_b.metrics
            );
        }
    }
    Ok(())
}

fn require_render_package_catalog_reuse(
    case: &Case,
    retained_b: &Evidence,
    fresh_b: &Evidence,
) -> Result<()> {
    let fresh_entries = count_metric(fresh_b, "renderPackageCatalogEntries")?;
    let fresh_packages = count_metric(fresh_b, "renderPackageCatalogPackages")?;
    let fresh_catalog_bytes = count_metric(fresh_b, "renderPackageCatalogApproxBytes")?;
    let fresh_own_resources = count_metric(fresh_b, "renderOwnResourcesPreparsed")?;
    if count_metric(fresh_b, "renderSemanticsCacheHit")? != 0.0
        || count_metric(fresh_b, "renderPackageCatalogCacheHit")? != 0.0
        || count_metric(fresh_b, "renderPackageCatalogBuilt")? != 1.0
        || count_metric(fresh_b, "renderOwnContextBuilt")? != 1.0
        || count_metric(fresh_b, "renderPackageCatalogAdmitted")? != 1.0
        || fresh_packages <= 0.0
        || fresh_entries <= 0.0
        || fresh_catalog_bytes <= 0.0
        || fresh_own_resources <= 0.0
    {
        bail!(
            "fresh B did not prove canonical render package-catalog construction: {:?}",
            fresh_b.metrics
        );
    }

    if case.expected_snapshot_resource_misses.is_some() {
        let retained_generations =
            count_metric(retained_b, "renderPackageCatalogRetainedGenerations")?;
        if count_metric(retained_b, "renderSemanticsCacheHit")? != 0.0
            || count_metric(retained_b, "renderPackageCatalogCacheHit")? != 1.0
            || count_metric(retained_b, "renderPackageCatalogBuilt")? != 0.0
            || count_metric(retained_b, "renderOwnContextBuilt")? != 1.0
            || count_metric(retained_b, "renderOwnResourcesPreparsed")? != fresh_own_resources
            || count_metric(retained_b, "renderPackageCatalogAdmitted")? != 1.0
            || count_metric(retained_b, "renderPackageCatalogPackages")? != fresh_packages
            || count_metric(retained_b, "renderPackageCatalogEntries")? != fresh_entries
            || count_metric(retained_b, "renderPackageCatalogApproxBytes")? != fresh_catalog_bytes
            || !(1.0..=2.0).contains(&retained_generations)
        {
            bail!(
                "retained B did not prove bounded package-catalog-only render-context reuse: {:?}",
                retained_b.metrics
            );
        }
    } else if count_metric(retained_b, "renderSemanticsCacheHit")? != 1.0
        || count_metric(retained_b, "renderPackageCatalogCacheHit")? != 0.0
        || count_metric(retained_b, "renderPackageCatalogBuilt")? != 0.0
        || count_metric(retained_b, "renderOwnContextBuilt")? != 0.0
        || count_metric(retained_b, "renderOwnResourcesPreparsed")? != 0.0
        || count_metric(retained_b, "renderPackageCatalogAdmitted")? != 0.0
    {
        bail!(
            "site-only retained B did not short-circuit through exact RenderSemantics: {:?}",
            retained_b.metrics
        );
    }
    Ok(())
}

fn failed_snapshot_successor_probe(
    engine: &mut site_engine::SiteEngine,
    revision_a: &site_engine::ProjectRevision,
    revision_b: &site_engine::ProjectRevision,
    corpus: &PackageCorpus,
    environment: &site_engine::PackageEnvironment,
    spec: &site_engine::GeneratorSpec,
    byte_store: &VerifiedByteStore,
) -> Result<(FailedSuccessorSummary, Evidence)> {
    let path = "input/resources/StructureDefinition-incremental-failed-successor-probe.json";
    let body = serde_json::json!({
        "resourceType": "StructureDefinition",
        "id": "incremental-failed-successor-probe",
        "url": "http://example.org/fhir/StructureDefinition/incremental-failed-successor-probe",
        "name": "IncrementalFailedSuccessorProbe",
        "status": "draft",
        "fhirVersion": "4.0.1",
        "kind": "resource",
        "abstract": false,
        "type": "Patient",
        "baseDefinition": "http://example.org/fhir/StructureDefinition/incremental-definitely-missing-base",
        "derivation": "constraint",
        "differential": {
            "element": [{"id": "Patient", "path": "Patient"}]
        }
    });
    let body_bytes = serde_json::to_vec(&body)?;
    let mut failed_revision = revision_a.clone();
    failed_revision.predefined.insert(path.into(), body);
    failed_revision
        .site_files
        .insert(path.into(), body_bytes.clone());
    let mut source = FixedSource {
        revision: failed_revision,
    };
    let mut packages = FixedPackages {
        resolved: corpus.resolved.clone(),
        environment: environment.clone(),
    };
    let error = engine
        .prepare_project(&mut source, &mut packages, spec.clone())
        .expect_err("missing snapshot base must fail after semantic compilation");
    if error.operation != site_engine::BuildOperation::Prepare
        || error.phase != site_engine::BuildErrorPhase::Preparation
        || error.code != site_engine::BuildErrorCode::RendererFailed
        || error.retryable
        || error.successful_compilation.is_none()
        || !error
            .message
            .contains("incremental-definitely-missing-base")
    {
        bail!("failed successor returned the wrong typed error: {error:?}");
    }
    let operation = error.operation;
    let phase = error.phase;
    let code = error.code;
    let retryable = error.retryable;
    let successful_compilation = error.successful_compilation.is_some();
    let recovery = prepare_on(
        engine,
        revision_b,
        corpus,
        environment,
        spec,
        RenderOrder::Forward,
        byte_store,
    )?;
    let summary = FailedSuccessorSummary {
        operation,
        phase,
        code,
        retryable,
        successful_compilation,
        injected_body_sha256: site_build::Sha256Digest::of_bytes(&body_bytes),
        recovery: recovery.summary(),
    };
    Ok((summary, recovery))
}

fn run_case(case: &Case) -> Result<Receipt> {
    let source_a = canonical_directory(&case.source_a, "source A")?;
    let source_b = canonical_directory(&case.source_b, "source B")?;
    if source_a == source_b {
        bail!("source A and source B must be distinct immutable directories");
    }

    let mut source = FilesystemProjectSource::open(&source_a)?;
    let config_a = site_engine::ProjectSource::config(&mut source).map_err(anyhow::Error::msg)?;
    source.verify_unchanged()?;
    let corpus = package_corpus(case, &config_a)?;
    let capture_environment = corpus.environment()?;
    let (captured_config_a, revision_a) =
        capture_revision(&source_a, &capture_environment, &corpus.resolved)?;
    let (config_b, revision_b) =
        capture_revision(&source_b, &capture_environment, &corpus.resolved)?;
    if captured_config_a != config_a || config_b != config_a {
        bail!("A and B must retain byte-identical configuration");
    }
    if serde_json::to_value(&revision_a)? == serde_json::to_value(&revision_b)? {
        bail!("A and B ProjectRevision values are identical; mutation is a no-op");
    }
    let spec = generator(case);
    let byte_store = VerifiedByteStore::create(
        case.report
            .parent()
            .ok_or_else(|| anyhow!("report path has no parent"))?,
    )?;

    let mut fresh_a_engine = site_engine::SiteEngine::default();
    let fresh_a_environment = corpus.environment()?;
    let fresh_a = prepare_on(
        &mut fresh_a_engine,
        &revision_a,
        &corpus,
        &fresh_a_environment,
        &spec,
        RenderOrder::Forward,
        &byte_store,
    )?;
    let failed_successor = if case.case_id == "tiny" {
        Some(failed_snapshot_successor_probe(
            &mut fresh_a_engine,
            &revision_a,
            &revision_b,
            &corpus,
            &fresh_a_environment,
            &spec,
            &byte_store,
        )?)
    } else {
        None
    };
    drop(fresh_a_engine);

    let mut retained_engine = site_engine::SiteEngine::default();
    let retained_environment = corpus.environment()?;
    let retained_seed_a = prepare_on(
        &mut retained_engine,
        &revision_a,
        &corpus,
        &retained_environment,
        &spec,
        RenderOrder::None,
        &byte_store,
    )?;
    let retained_b = prepare_on(
        &mut retained_engine,
        &revision_b,
        &corpus,
        &retained_environment,
        &spec,
        RenderOrder::Forward,
        &byte_store,
    )?;
    let retained_return_a = prepare_on(
        &mut retained_engine,
        &revision_a,
        &corpus,
        &retained_environment,
        &spec,
        RenderOrder::Reverse,
        &byte_store,
    )?;
    drop(retained_engine);

    let mut fresh_b_engine = site_engine::SiteEngine::default();
    let fresh_b_environment = corpus.environment()?;
    let fresh_b = prepare_on(
        &mut fresh_b_engine,
        &revision_b,
        &corpus,
        &fresh_b_environment,
        &spec,
        RenderOrder::Reverse,
        &byte_store,
    )?;
    drop(fresh_b_engine);

    if let Some((_, recovery_b)) = &failed_successor {
        compare_complete(
            case,
            "failed-successor-recovery-b-vs-fresh-b",
            recovery_b,
            &fresh_b,
            &byte_store,
        )?;
        let recovery_hits = count_metric(recovery_b, "snapshotResourceCacheHits")?;
        let recovery_misses = count_metric(recovery_b, "snapshotResourceCacheMisses")?;
        let fresh_hits = count_metric(&fresh_b, "snapshotResourceCacheHits")?;
        let fresh_misses = count_metric(&fresh_b, "snapshotResourceCacheMisses")?;
        if recovery_hits <= 0.0
            || recovery_misses <= 0.0
            || recovery_hits + recovery_misses != fresh_misses
            || fresh_hits != 0.0
            || count_metric(recovery_b, "snapshotDerivationAdmitted")? != 1.0
        {
            bail!("failed-successor recovery B did not retain safe classified snapshot reuse");
        }
        require_render_package_catalog_reuse(case, recovery_b, &fresh_b).context(
            "failed-successor recovery B did not retain the last successful render package catalog",
        )?;
    }

    compare_prepared(
        case,
        "fresh-a-vs-retained-seed-a",
        &fresh_a,
        &retained_seed_a,
    )?;
    compare_complete(
        case,
        "fresh-a-vs-retained-return-a",
        &fresh_a,
        &retained_return_a,
        &byte_store,
    )?;
    compare_complete(
        case,
        "retained-b-vs-fresh-b",
        &retained_b,
        &fresh_b,
        &byte_store,
    )?;
    require_metric(&retained_return_a, "semanticCompilationCacheHit")?;
    require_metric(&retained_return_a, "siteBuildCacheHit")?;
    if metric(&retained_return_a, "snapshotResourceCacheHits")? != 0.0
        || metric(&retained_return_a, "snapshotResourceCacheMisses")? != 0.0
        || metric(&retained_return_a, "renderPackageCatalogCacheHit")? != 0.0
        || metric(&retained_return_a, "renderPackageCatalogBuilt")? != 0.0
        || metric(&retained_return_a, "renderOwnContextBuilt")? != 0.0
        || metric(&retained_return_a, "renderOwnResourcesPreparsed")? != 0.0
        || metric(&retained_return_a, "renderPackageCatalogAdmitted")? != 0.0
    {
        bail!("returned A exact build reuse must not masquerade as incremental derivation proof");
    }
    require_snapshot_reuse(case, &retained_b, &fresh_b)?;
    require_render_package_catalog_reuse(case, &retained_b, &fresh_b)?;

    if fresh_a.build_id == fresh_b.build_id {
        bail!("A and B produced the same ClosedSiteBuild id");
    }
    if fresh_a.site_output_bytes == fresh_b.site_output_bytes {
        bail!("A and B produced the same canonical SiteOutput");
    }
    let changed_path = &case.expected_changed_output;
    let a_changed = fresh_a
        .rendered
        .get(changed_path)
        .ok_or_else(|| anyhow!("A catalog lacks expected changed output {changed_path}"))?;
    let b_changed = fresh_b
        .rendered
        .get(changed_path)
        .ok_or_else(|| anyhow!("B catalog lacks expected changed output {changed_path}"))?;
    if a_changed.content == b_changed.content
        || byte_store.read(&a_changed.content)? == byte_store.read(&b_changed.content)?
    {
        bail!("expected output {changed_path} did not change from A to B");
    }
    let compilation_changed = fresh_a.compilation != fresh_b.compilation;
    if compilation_changed != case.expect_compilation_change {
        bail!(
            "compilation change expectation was {}, observed {}",
            case.expect_compilation_change,
            compilation_changed
        );
    }

    let mut executions = BTreeMap::new();
    executions.insert("freshAForward".into(), fresh_a.summary());
    executions.insert("retainedSeedA".into(), retained_seed_a.summary());
    executions.insert("retainedBForward".into(), retained_b.summary());
    executions.insert("retainedReturnAReverse".into(), retained_return_a.summary());
    executions.insert("freshBReverse".into(), fresh_b.summary());
    Ok(Receipt {
        schema_version: RECEIPT_SCHEMA,
        status: "pass",
        case_id: case.case_id.clone(),
        fixture: case.fixture.clone(),
        comparisons: vec![
            "fresh A equals retained seed A before rendering",
            "fresh A forward equals retained return A reverse",
            "retained B forward equals fresh B reverse",
            "compilation and ordered diagnostics",
            "ClosedSiteBuild canonical closure and every addressed byte",
            "initial and fully materialized output catalogs",
            "every output ContentRef and byte body",
            "canonical final SiteOutput",
            "retained B classified snapshot reuse accounting against fresh B",
            "retained B classified render package-catalog reuse against fresh B",
        ],
        package_corpus: corpus.summary.clone(),
        executions,
        failed_successor: failed_successor.map(|(summary, _)| summary),
    })
}

fn write_report(path: &Path, value: &Value) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("report path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("json.tmp");
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(&temporary, bytes)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

#[test]
#[ignore = "exhaustive real-guide differential corpus; run through scripts/verify-incremental-differential.py"]
fn catalog_case() {
    let case = match read_case() {
        Ok(case) => case,
        Err(error) => panic!("read differential case: {error:#}"),
    };
    match run_case(&case) {
        Ok(receipt) => {
            let value = serde_json::to_value(receipt).expect("serialize pass receipt");
            write_report(&case.report, &value).expect("write pass receipt");
        }
        Err(error) => {
            let failure = serde_json::json!({
                "schemaVersion": RECEIPT_SCHEMA,
                "status": "fail",
                "caseId": case.case_id,
                "fixture": case.fixture,
                "error": format!("{error:#}"),
            });
            let _ = write_report(&case.report, &failure);
            panic!("incremental differential failed: {error:#}");
        }
    }
}

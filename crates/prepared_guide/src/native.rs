//! Native compile + snapshot + renderer-neutral guide preparation.
//!
//! This is the filesystem adapter for [`crate::semantics`]. It deliberately
//! returns the domain handoff itself, [`crate::PreparedGuide`], rather than a
//! pipeline-specific wrapper. Relational rows and renderer output are downstream
//! projections and do not belong in this preparation path.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::{semantics, AugmentInputs, DiskFiles, PreparedGuide};

/// Explicit native inputs for preparing one guide.
#[derive(Clone, Debug)]
pub struct PrepareInputs {
    pub ig_dir: PathBuf,
    pub sushi_out: PathBuf,
    pub cache_dir: PathBuf,
    pub build_epoch_secs: i64,
    pub branch: Option<String>,
    pub revision: Option<String>,
    pub run_sushi: bool,
    pub core_package: String,
    pub layer_b: snapshot_gen::LayerBOptions,
    pub liquid_asset_dirs: Vec<PathBuf>,
}

/// Compile and snapshot authored inputs, then prepare the renderer-neutral
/// handoff. Generated resources remain available at
/// `<sushi_out>/fsh-generated/resources` for compatibility consumers.
pub fn prepare(input: &PrepareInputs) -> Result<PreparedGuide> {
    let resources_dir = input.sushi_out.join("fsh-generated").join("resources");

    if input.run_sushi {
        compiler::build_project_with_cache(
            &input.ig_dir.to_string_lossy(),
            &input.sushi_out.to_string_lossy(),
            &input.cache_dir.to_string_lossy(),
        )
        .context("rust_sushi build (compiler::build_project_with_cache)")?;
    }
    if !resources_dir.is_dir() {
        bail!(
            "no fsh-generated/resources at {} (run with run_sushi=true or point --sushi-out at a build)",
            resources_dir.display()
        );
    }

    let config_path = input.ig_dir.join("sushi-config.yaml");
    let config_text = std::fs::read_to_string(&config_path)
        .with_context(|| format!("read {}", config_path.display()))?;
    let package_context =
        resolve_snapshot_package_context(&config_text, &input.cache_dir, &input.core_package)?;

    let mut context = snapshot_gen::PackageContext::new(&input.cache_dir, &package_context)
        .with_context(|| {
            format!(
                "open package cache {} with {}",
                input.cache_dir.display(),
                input.core_package
            )
        })?;
    context
        .load_local_dir(&resources_dir)
        .context("load local SD dir for snapshot base resolution")?;

    for (path, resource) in read_json_files(&resources_dir)? {
        if resource_type(&resource) != "StructureDefinition" {
            continue;
        }
        let output = snapshot_gen::generate_snapshot_layer_b(
            resource,
            &context,
            snapshot_gen::SnapshotOptions::default(),
            effective_layer_b(input),
        )
        .with_context(|| format!("generate_snapshot for {}", path.display()))?;
        let bytes = json_emit::to_fhir_json_string(&output).into_bytes();
        if std::fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
            std::fs::write(&path, bytes)?;
        }
    }

    let generated_with_paths = read_json_files(&resources_dir)?;
    let primary_filename = primary_implementation_guide_filename(&config_text)?;
    let primary_implementation_guide = generated_with_paths
        .iter()
        .find(|(path, _)| {
            path.file_name().and_then(|name| name.to_str()) == Some(primary_filename.as_str())
        })
        .map(|(_, resource)| resource.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "generated primary ImplementationGuide file {primary_filename} is absent"
            )
        })?;
    let generated: Vec<Value> = generated_with_paths
        .into_iter()
        .map(|(_, resource)| resource)
        .collect();
    let examples: Vec<Value> = read_json_files(&input.ig_dir.join("input/resources"))?
        .into_iter()
        .map(|(_, resource)| resource)
        .collect();

    semantics::prepare(&semantics::PrepareInputs {
        generated: &generated,
        primary_implementation_guide: &primary_implementation_guide,
        examples: &examples,
        sushi_config_yaml: &config_text,
        build_epoch_secs: input.build_epoch_secs,
        branch: input.branch.clone(),
        revision: input.revision.clone(),
        augmentation: AugmentInputs {
            ig: &primary_implementation_guide,
            sushi_config_yaml: &config_text,
            project_root: input.ig_dir.clone(),
            pagecontent_dir: input.ig_dir.join("input/pagecontent"),
            image_dir: input.ig_dir.join("input/images"),
            liquid_asset_dirs: input.liquid_asset_dirs.clone(),
            files: &DiskFiles,
        },
    })
}

fn resource_type(resource: &Value) -> &str {
    resource
        .get("resourceType")
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn read_json_files(directory: &Path) -> Result<Vec<(PathBuf, Value)>> {
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(directory)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            let resource = serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(&text))
                .with_context(|| format!("parse {}", path.display()))?;
            Ok((path, resource))
        })
        .collect()
}

fn primary_implementation_guide_filename(config_text: &str) -> Result<String> {
    let config: serde_yaml::Value = serde_yaml::from_str(config_text)?;
    let id = config
        .get("id")
        .and_then(serde_yaml::Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("sushi-config.yaml has no primary IG id"))?;
    if id.contains('/') || id.contains('\\') || id.contains('\0') {
        bail!("sushi-config.yaml has an unsafe primary IG id {id:?}");
    }
    Ok(format!("ImplementationGuide-{id}.json"))
}

fn resolve_snapshot_package_context(
    config_text: &str,
    cache_dir: &Path,
    expected_core: &str,
) -> Result<Vec<String>> {
    let source = package_store::DiskSource;
    let index = package_store::version_index_from_cache(&source, cache_dir);
    let step = package_store::resolve_project(config_text, &source, cache_dir, Some(&index))
        .context("resolve exact package closure for snapshot completion")?;
    if !step.satisfied {
        bail!(
            "package cache does not satisfy snapshot closure: {}",
            serde_json::to_string(&step.missing)?
        );
    }
    let mut labels = Vec::new();
    for request in step.compile_set.iter().chain(&step.context_closure) {
        let label = format!("{}#{}", request.package_id, request.version);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    if !labels.iter().any(|label| label == expected_core) {
        bail!(
            "resolved snapshot closure does not contain distinguished core package {expected_core}"
        );
    }
    Ok(labels)
}

fn effective_layer_b(input: &PrepareInputs) -> snapshot_gen::LayerBOptions {
    snapshot_gen::LayerBOptions {
        pin: input.layer_b.pin,
        project_r4: input.layer_b.project_r4
            && input.core_package.starts_with("hl7.fhir.r4.core#4"),
    }
}

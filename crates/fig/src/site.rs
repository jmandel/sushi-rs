//! Native transport for the canonical SiteEngine operations.
//!
//! Every command starts from the same sealed `ClosedSiteBuild + ContentStore`
//! handoff emitted by [`crate::prepare`]. It restores an ordinary SiteEngine
//! handle and delegates to `outputs`, `render`, or `finalize`; there is no
//! native staged-tree renderer or second site model.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use content_store::{ContentStore, FileContentStore};
use serde::{Deserialize, Serialize};
use site_build::{SiteOutputCache, SITE_OUTPUT_MANIFEST_PATH};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderOutcome {
    pub build_id: String,
    pub path: site_build::OutputPath,
    pub media_type: String,
    pub content: site_build::ContentRef,
    #[serde(skip)]
    pub bytes: Vec<u8>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalizeOutcome {
    pub build_id: String,
    pub cache_key: String,
    pub output_id: String,
    pub out: String,
    pub files: usize,
    pub bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalFinalizePlan {
    /// Exact closed SiteBuild opened by the external renderer. Finalization
    /// must restore this same identity before it authenticates any output.
    pub input_build_id: site_build::OutputInputId,
    pub renderer: site_build::RendererImplementation,
    pub output_schema: String,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
    pub files: Vec<ExternalFileDeclaration>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalFileDeclaration {
    pub path: site_build::OutputPath,
    pub media_type: String,
    pub producer: site_build::OutputProducer,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub owner: Option<site_build::OutputPath>,
}

/// Native transport choices behind the one public `finalize` operation.
pub enum FinalizeRequest<'a> {
    Publisher {
        destination: &'a Path,
    },
    External {
        site: &'a Path,
        plan: ExternalFinalizePlan,
        cache_root: Option<&'a Path>,
    },
}

pub fn outputs(bundle: &Path) -> Result<site_engine::OutputCatalog> {
    let (engine, handle) = restore_publisher(bundle)?;
    engine.outputs(&handle).map_err(anyhow::Error::msg)
}

/// Admit a closed bundle through the same target-specific SiteEngine restore
/// used by every execution operation. Cache lookup is not a weaker lifecycle:
/// malformed-but-closed input must be rejected before its key can address a
/// previously published output.
pub(crate) fn admit(bundle: &Path) -> Result<site_build::ClosedSiteBuild> {
    let (closed, _, _) = restore_bundle(bundle)?;
    Ok(closed)
}

pub fn render(bundle: &Path, path: &str) -> Result<RenderOutcome> {
    let (mut engine, handle) = restore_publisher(bundle)?;
    let rendered = engine.render(&handle, path).map_err(anyhow::Error::msg)?;
    let bytes = engine
        .read_content(&handle, rendered.content.sha256.as_str())
        .map_err(anyhow::Error::msg)?;
    rendered
        .content
        .verify(&bytes)
        .context("verify rendered output bytes")?;
    Ok(RenderOutcome {
        build_id: handle,
        path: rendered.path,
        media_type: rendered.media_type,
        content: rendered.content,
        bytes,
    })
}

/// Render every declared Publisher output, finalize its canonical SiteOutput,
/// and atomically publish the complete site into a new directory.
pub fn finalize(bundle: &Path, request: FinalizeRequest<'_>) -> Result<FinalizeOutcome> {
    match request {
        FinalizeRequest::Publisher { destination } => finalize_publisher(bundle, destination),
        FinalizeRequest::External {
            site,
            plan,
            cache_root,
        } => finalize_external(bundle, site, plan, cache_root),
    }
}

fn finalize_publisher(bundle: &Path, destination: &Path) -> Result<FinalizeOutcome> {
    let output = crate::publication::new_directory_destination(destination)?;
    let (mut engine, handle) = restore_publisher(bundle)?;
    let catalog = engine.outputs(&handle).map_err(anyhow::Error::msg)?;
    for declared in &catalog.outputs {
        if declared.content.is_none() {
            engine
                .render(&handle, declared.path.as_str())
                .map_err(anyhow::Error::msg)?;
        }
    }
    let receipt = engine.finalize(&handle, None).map_err(anyhow::Error::msg)?;
    let parent = output
        .parent()
        .expect("publication destination has a parent");
    let temp = tempfile::Builder::new()
        .prefix(".fig-finalize-")
        .tempdir_in(parent)
        .with_context(|| format!("create temporary site beside {}", output.display()))?;
    let mut byte_length = 0u64;
    for file in receipt.files() {
        let bytes = engine
            .read_content(&handle, file.content.sha256.as_str())
            .map_err(anyhow::Error::msg)?;
        file.content
            .verify(&bytes)
            .with_context(|| format!("verify finalized output {}", file.path))?;
        let path = temp.path().join(file.path.as_str());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create site directory {}", parent.display()))?;
        }
        fs::write(&path, &bytes)
            .with_context(|| format!("write finalized output {}", file.path))?;
        byte_length += bytes.len() as u64;
    }
    let receipt_bytes = receipt.canonical_bytes()?;
    fs::write(temp.path().join(SITE_OUTPUT_MANIFEST_PATH), &receipt_bytes)
        .context("write canonical SiteOutput receipt")?;
    verify_site_tree(temp.path(), &receipt)?;
    if fs::symlink_metadata(&output).is_ok() {
        bail!(
            "output appeared while site was being finalized: {}",
            output.display()
        );
    }
    crate::publication::rename_no_replace(temp.path(), &output)
        .with_context(|| format!("publish site atomically at {}", output.display()))?;
    Ok(FinalizeOutcome {
        build_id: handle,
        cache_key: receipt.cache_key().to_string(),
        output_id: receipt.output_id().to_string(),
        out: output.display().to_string(),
        files: receipt.files().len(),
        bytes: byte_length,
    })
}

/// Authenticate an external renderer's complete private staging tree and make
/// Rust the sole constructor of its canonical SiteOutput receipt.
fn finalize_external(
    bundle: &Path,
    site: &Path,
    plan: ExternalFinalizePlan,
    cache_root: Option<&Path>,
) -> Result<FinalizeOutcome> {
    let mut protected = vec![bundle, site];
    if let Some(cache_root) = cache_root {
        protected.push(cache_root);
    }
    crate::output_cache::require_disjoint_paths(&protected)?;
    let (closed, engine, handle) = restore_bundle(bundle)?;
    require_expected_input_build(&plan.input_build_id, &handle)?;
    let files = authenticate_external_tree(site, &plan.files, None)?;
    let catalog = files.iter().map(|file| file.path.clone()).collect();
    let receipt = engine
        .finalize(
            &handle,
            Some(site_engine::ExternalFinalizeInput {
                renderer: plan.renderer,
                output_schema: plan.output_schema,
                options: plan.options,
                catalog,
                files: files.clone(),
            }),
        )
        .map_err(anyhow::Error::msg)?;
    let receipt_bytes = receipt.canonical_bytes()?;
    let receipt_path = site.join(SITE_OUTPUT_MANIFEST_PATH);
    let mut receipt_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&receipt_path)
        .with_context(|| format!("create canonical receipt {}", receipt_path.display()))?;
    receipt_file.write_all(&receipt_bytes)?;
    receipt_file.sync_all()?;
    let verified = authenticate_external_tree(site, &plan.files, Some(&receipt_bytes))?;
    if verified != files {
        bail!("external site changed while its canonical receipt was being written");
    }
    if let Some(cache_root) = cache_root {
        let objects = FileContentStore::create(cache_root.join("objects/sha256"))
            .context("open SiteOutput cache object store")?;
        for file in receipt.files() {
            let bytes = read_stable_regular(
                &site.join(file.path.as_str()),
                &format!("external output {}", file.path),
            )?;
            objects.put(&file.content, &bytes)?;
        }
        let manifests = site_build::FileSiteOutputCache::create(cache_root.join("manifests"))
            .context("open SiteOutput manifest cache")?;
        manifests
            .publish(&receipt, &closed, &objects)
            .context("publish canonical SiteOutput cache entry")?;
    }
    let final_verified = authenticate_external_tree(site, &plan.files, Some(&receipt_bytes))?;
    if final_verified != files {
        bail!("external site changed before finalization completed");
    }
    Ok(FinalizeOutcome {
        build_id: handle,
        cache_key: receipt.cache_key().to_string(),
        output_id: receipt.output_id().to_string(),
        out: site.display().to_string(),
        files: receipt.files().len(),
        bytes: receipt
            .files()
            .iter()
            .map(|file| file.content.byte_length)
            .sum(),
    })
}

fn require_expected_input_build(
    expected: &site_build::OutputInputId,
    restored_handle: &str,
) -> Result<()> {
    if expected.as_str() != restored_handle {
        bail!(
            "external finalize input build changed: renderer opened {}, but Fig restored {}",
            expected,
            restored_handle
        );
    }
    Ok(())
}

fn restore_publisher(bundle: &Path) -> Result<(site_engine::SiteEngine, String)> {
    let (_, engine, handle) = restore_bundle(bundle)?;
    Ok((engine, handle))
}

fn restore_bundle(
    bundle: &Path,
) -> Result<(site_build::ClosedSiteBuild, site_engine::SiteEngine, String)> {
    let closed = crate::output_cache::open_closed_bundle_manifest(bundle)?;
    let store = FileContentStore::create(bundle.join("objects/sha256"))
        .context("open closed-build content store")?;
    let mut engine = site_engine::SiteEngine::default();
    let handle = engine
        .restore(closed.clone(), &store)
        .map_err(anyhow::Error::msg)?;
    Ok((closed, engine, handle))
}

fn authenticate_external_tree(
    root: &Path,
    declarations: &[ExternalFileDeclaration],
    expected_receipt: Option<&[u8]>,
) -> Result<Vec<site_build::SiteOutputFile>> {
    let root_metadata = fs::symlink_metadata(root)
        .with_context(|| format!("inspect external site {}", root.display()))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        bail!("external site is not a real directory: {}", root.display());
    }
    let mut declared = BTreeSet::new();
    for declaration in declarations {
        if declaration.media_type.trim().is_empty() {
            bail!(
                "external output {} has an empty media type",
                declaration.path
            );
        }
        if !declared.insert(declaration.path.clone()) {
            bail!("external output declaration repeats {}", declaration.path);
        }
    }
    let mut actual = BTreeSet::new();
    let mut receipt_verified = false;
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_symlink() {
            bail!(
                "external site contains a symlink: {}",
                entry.path().display()
            );
        }
        if entry.file_type().is_dir() {
            continue;
        }
        if !entry.file_type().is_file() {
            bail!(
                "external site member is not a regular file: {}",
                entry.path().display()
            );
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .expect("walk entry is below external site")
            .to_string_lossy()
            .replace('\\', "/");
        if relative == SITE_OUTPUT_MANIFEST_PATH {
            if let Some(expected) = expected_receipt {
                let bytes = read_stable_regular(entry.path(), "external SiteOutput receipt")?;
                if bytes != expected {
                    bail!("external SiteOutput receipt differs from the canonical receipt");
                }
                receipt_verified = true;
                continue;
            }
            bail!("external staging tree already contains {SITE_OUTPUT_MANIFEST_PATH}");
        }
        actual.insert(site_build::OutputPath::parse(relative)?);
    }
    if expected_receipt.is_some() && !receipt_verified {
        bail!("external site is missing its canonical {SITE_OUTPUT_MANIFEST_PATH}");
    }
    if actual != declared {
        let missing = declared
            .difference(&actual)
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let extra = actual
            .difference(&declared)
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        bail!(
            "external site inventory differs from declarations (missing: {}; extra: {})",
            missing.join(", "),
            extra.join(", ")
        );
    }
    declarations
        .iter()
        .map(|declaration| {
            let bytes = read_stable_regular(
                &root.join(declaration.path.as_str()),
                &format!("external output {}", declaration.path),
            )?;
            Ok(site_build::SiteOutputFile {
                path: declaration.path.clone(),
                content: site_build::ContentRef::of_bytes(
                    &bytes,
                    Some(declaration.media_type.clone()),
                ),
                producer: declaration.producer.clone(),
                source: declaration.source.clone(),
                owner: declaration.owner.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()
        .and_then(|files| {
            let current = fs::symlink_metadata(root)
                .with_context(|| format!("reinspect external site {}", root.display()))?;
            if current.file_type().is_symlink()
                || !current.is_dir()
                || !same_fs_object(&root_metadata, &current)
            {
                bail!("external site root changed during authentication");
            }
            Ok(files)
        })
}

fn read_stable_regular(path: &Path, label: &str) -> Result<Vec<u8>> {
    let before = fs::symlink_metadata(path)
        .with_context(|| format!("inspect {label} {}", path.display()))?;
    if before.file_type().is_symlink() || !before.is_file() {
        bail!("{label} is not a regular file: {}", path.display());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("open {label} {}", path.display()))?;
    let opened = file
        .metadata()
        .with_context(|| format!("inspect opened {label} {}", path.display()))?;
    if !opened.is_file() || !same_fs_object(&before, &opened) {
        bail!(
            "{label} changed while it was being opened: {}",
            path.display()
        );
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("read {label} {}", path.display()))?;
    let opened_after = file
        .metadata()
        .with_context(|| format!("reinspect opened {label} {}", path.display()))?;
    let path_after = fs::symlink_metadata(path)
        .with_context(|| format!("reinspect {label} {}", path.display()))?;
    if path_after.file_type().is_symlink()
        || !path_after.is_file()
        || !same_fs_object(&opened, &opened_after)
        || !same_fs_object(&opened, &path_after)
        || opened.len() != opened_after.len()
        || opened_after.len() != bytes.len() as u64
        || opened.modified().ok() != opened_after.modified().ok()
    {
        bail!(
            "{label} changed while it was being read: {}",
            path.display()
        );
    }
    Ok(bytes)
}

#[cfg(unix)]
fn same_fs_object(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_fs_object(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.len() == right.len()
        && left.created().ok() == right.created().ok()
        && left.modified().ok() == right.modified().ok()
}

fn verify_site_tree(root: &Path, output: &site_build::SiteOutput) -> Result<()> {
    let mut actual = Vec::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_symlink() {
            bail!(
                "finalized site contains a symlink: {}",
                entry.path().display()
            );
        }
        if entry.file_type().is_file() {
            let relative = entry
                .path()
                .strip_prefix(root)
                .expect("walk entry is below root")
                .to_string_lossy()
                .replace('\\', "/");
            actual.push(relative);
        }
    }
    actual.sort();
    let mut expected = output
        .files()
        .iter()
        .map(|file| file.path.as_str().to_string())
        .collect::<Vec<_>>();
    expected.push(SITE_OUTPUT_MANIFEST_PATH.into());
    expected.sort();
    if actual != expected {
        bail!("finalized site inventory differs from its SiteOutput receipt");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILD_A: &str =
        "sb1-sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BUILD_B: &str =
        "sb1-sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn external_finalize_plan_requires_a_typed_input_build_identity() {
        let missing = serde_json::json!({
            "renderer": {
                "id": "cycle-site",
                "version": "1",
                "recipeSha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            },
            "outputSchema": "cycle-static-site/v1",
            "files": []
        });
        assert!(serde_json::from_value::<ExternalFinalizePlan>(missing)
            .unwrap_err()
            .to_string()
            .contains("inputBuildId"));

        let malformed = serde_json::json!({
            "inputBuildId": "not-a-build-id",
            "renderer": {
                "id": "cycle-site",
                "version": "1",
                "recipeSha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            },
            "outputSchema": "cycle-static-site/v1",
            "files": []
        });
        assert!(serde_json::from_value::<ExternalFinalizePlan>(malformed).is_err());
    }

    #[test]
    fn external_finalize_rejects_a_restored_build_other_than_the_rendered_input() {
        let expected = site_build::OutputInputId::parse(BUILD_A).unwrap();
        require_expected_input_build(&expected, BUILD_A).unwrap();
        let error = require_expected_input_build(&expected, BUILD_B).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "external finalize input build changed: renderer opened {BUILD_A}, but Fig restored {BUILD_B}"
            )
        );
    }

    fn declaration(path: &str) -> ExternalFileDeclaration {
        ExternalFileDeclaration {
            path: site_build::OutputPath::parse(path).unwrap(),
            media_type: "text/html".into(),
            producer: site_build::OutputProducer {
                id: "cycle-site".into(),
                version: "1".into(),
            },
            source: Some("page".into()),
            owner: None,
        }
    }

    #[test]
    fn external_tree_requires_the_exact_regular_receipt_after_sealing() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("en")).unwrap();
        fs::write(temp.path().join("en/index.html"), b"<h1>Demo</h1>").unwrap();
        let declarations = [declaration("en/index.html")];
        let files = authenticate_external_tree(temp.path(), &declarations, None).unwrap();
        assert_eq!(files.len(), 1);

        let receipt = b"canonical receipt";
        let missing = authenticate_external_tree(temp.path(), &declarations, Some(receipt))
            .unwrap_err()
            .to_string();
        assert!(missing.contains("missing its canonical"));

        fs::write(temp.path().join(SITE_OUTPUT_MANIFEST_PATH), receipt).unwrap();
        assert_eq!(
            authenticate_external_tree(temp.path(), &declarations, Some(receipt)).unwrap(),
            files
        );
        fs::write(temp.path().join(SITE_OUTPUT_MANIFEST_PATH), b"changed").unwrap();
        assert!(
            authenticate_external_tree(temp.path(), &declarations, Some(receipt))
                .unwrap_err()
                .to_string()
                .contains("differs from the canonical receipt")
        );
    }

    #[cfg(unix)]
    #[test]
    fn external_tree_rejects_symlink_members() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("page.html"), b"page").unwrap();
        symlink(
            temp.path().join("page.html"),
            temp.path().join("alias.html"),
        )
        .unwrap();
        let error = authenticate_external_tree(temp.path(), &[declaration("page.html")], None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("contains a symlink"));
    }
}

//! Native transport for the canonical SiteEngine operations.
//!
//! Every command starts from the same sealed `ClosedSiteBuild + ContentStore`
//! handoff emitted by [`crate::prepare`]. It restores an ordinary SiteEngine
//! handle and delegates to `outputs`, `render`, or `finalize`; there is no
//! native staged-tree renderer or second site model.

use std::collections::BTreeMap;
#[cfg(unix)]
use std::fs::File;
use std::fs::{self};
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use content_store::{ContentStore, FileContentStore};
use site_build::SITE_OUTPUT_MANIFEST_PATH;

/// Immutable native facade over one restored SiteBuild. Opening is lifecycle;
/// persisted authority remains the closed manifest plus its ContentStore.
pub struct Build {
    bundle: PathBuf,
    closed: site_build::ClosedSiteBuild,
    engine: site_engine::SiteEngine,
    id: String,
}

impl Build {
    pub fn open(bundle: &Path) -> Result<Self> {
        let closed = crate::output_cache::open_closed_bundle_manifest(bundle)?;
        let store = FileContentStore::open(bundle.join("objects/sha256"))
            .context("open closed-build content store")?;
        let mut engine = site_engine::SiteEngine::default();
        let id = engine
            .restore(closed.clone(), &store)
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            bundle: bundle.to_path_buf(),
            closed,
            engine,
            id,
        })
    }

    pub fn outputs(
        &self,
    ) -> std::result::Result<site_engine::OutputCatalog, site_engine::BuildError<()>> {
        self.engine.outputs(&self.id)
    }

    pub fn render(
        &mut self,
        path: &str,
    ) -> std::result::Result<site_build::ContentRef, site_engine::BuildError<()>> {
        self.engine.render(&self.id, path)
    }

    pub fn finalize(
        &mut self,
    ) -> std::result::Result<site_build::SiteOutput, site_engine::BuildError<()>> {
        let catalog = self.outputs()?;
        for declared in &catalog.outputs {
            if declared.content.is_none() {
                self.render(declared.path.as_str())?;
            }
        }
        self.engine.finalize(&self.id)
    }

    /// ContentStore read used by native stdout/file/publication adapters after
    /// `render` or `finalize` returns an authenticated reference.
    #[doc(hidden)]
    pub fn read(&self, content: &site_build::ContentRef) -> Result<Vec<u8>> {
        let bytes = self
            .engine
            .read_content(&self.id, content.sha256.as_str())
            .map_err(anyhow::Error::msg)?;
        content
            .verify(&bytes)
            .context("verify rendered output bytes")?;
        Ok(bytes)
    }
}

pub fn publish_publisher(mut build: Build, destination: &Path) -> Result<site_build::SiteOutput> {
    let output = crate::publication::new_directory_destination(destination)?;
    let receipt = build.finalize()?;
    let parent = output
        .parent()
        .expect("publication destination has a parent");
    let temp = tempfile::Builder::new()
        .prefix(".fig-finalize-")
        .tempdir_in(parent)
        .with_context(|| format!("create temporary site beside {}", output.display()))?;
    let mut byte_length = 0u64;
    for file in receipt.files() {
        let bytes = build
            .engine
            .read_content(&build.id, file.content.sha256.as_str())
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
    debug_assert_eq!(
        byte_length,
        receipt
            .files()
            .iter()
            .map(|file| file.content.byte_length)
            .sum::<u64>()
    );
    Ok(receipt)
}

/// Verify renderer ContentRefs through its ContentStore, make Rust the
/// sole SiteOutput constructor, and write only the canonical receipt requested
/// by the final publication adapter.
#[doc(hidden)]
pub fn complete_renderer_ipc(
    mut build: Build,
    content_store: &Path,
    input_build_id: String,
    renderer: site_build::RendererImplementation,
    output_schema: String,
    options: BTreeMap<String, String>,
    files: Vec<site_build::SiteOutputFile>,
    receipt_path: &Path,
    cache_root: Option<&Path>,
) -> Result<site_build::SiteOutput> {
    if input_build_id != build.id {
        bail!(
            "external renderer opened {}, but Fig restored {}",
            input_build_id,
            build.id
        );
    }
    let mut protected = vec![build.bundle.as_path(), receipt_path];
    if let Some(cache_root) = cache_root {
        protected.push(cache_root);
    }
    crate::output_cache::require_disjoint_paths(&protected)?;
    crate::output_cache::require_disjoint_paths(&[
        build.bundle.as_path(),
        receipt_path,
        content_store,
    ])?;
    let external_objects =
        FileContentStore::open(content_store).context("open external renderer ContentStore")?;
    let cache_objects = cache_root
        .map(|cache_root| {
            FileContentStore::create(cache_root.join("objects/sha256"))
                .context("open SiteOutput cache ContentStore")
        })
        .transpose()?;
    if let (Some(cache_root), Some(cache_objects)) = (cache_root, &cache_objects) {
        if cache_objects.root() != external_objects.root() {
            crate::output_cache::require_disjoint_paths(&[cache_root, external_objects.root()])?;
        }
    }
    build
        .engine
        .open_renderer(
            &build.id,
            renderer,
            output_schema,
            options,
            files.iter().map(|file| file.path.clone()).collect(),
        )
        .map_err(anyhow::Error::msg)?;
    for file in &files {
        let bytes = external_objects
            .read(&file.content)
            .with_context(|| format!("read external output {}", file.path))?
            .into_bytes();
        if let Some(cache_objects) = &cache_objects {
            cache_objects
                .put(&file.content, &bytes)
                .with_context(|| format!("cache external output {}", file.path))?;
        }
        build
            .engine
            .admit_output(&build.id, file.clone(), bytes)
            .map_err(anyhow::Error::msg)?;
    }
    let receipt = build
        .engine
        .finalize(&build.id)
        .map_err(anyhow::Error::new)?;
    if let (Some(cache_root), Some(cache_objects)) = (cache_root, cache_objects) {
        crate::output_cache::publish_site_output_cache(
            cache_root,
            &receipt,
            &build.closed,
            &cache_objects,
        )
        .context("publish canonical SiteOutput cache entry")?;
    }
    let receipt_bytes = receipt.canonical_bytes()?;
    let parent = receipt_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("receipt has no parent: {}", receipt_path.display()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary receipt beside {}", receipt_path.display()))?;
    temporary.write_all(&receipt_bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(receipt_path)
        .map_err(|error| error.error)
        .with_context(|| format!("publish canonical receipt {}", receipt_path.display()))?;
    #[cfg(unix)]
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("sync receipt directory {}", parent.display()))?;
    Ok(receipt)
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
